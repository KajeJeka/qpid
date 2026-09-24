//! cpal output stream and the real-time audio callback.
//!
//! The callback (architecture.md section 5.3) must never allocate, lock,
//! touch the filesystem, log, or send on a channel. It only reads from the
//! ring buffer, applies a linear gain ramp, and writes to the device buffer.
//! All coordination with the engine thread is through the `Shared` atomics.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};

// VERIFY ON BUILD: rtrb's exact API (constructor name, whether it's
// `RingBuffer::new(capacity)` returning `(Producer<T>, Consumer<T>)`, and
// the exact method names `push`/`pop`/`slots`/`is_empty`) is assumed here
// from the crate's well-known 0.2/0.3 shape and was not checked against
// docs.rs from this environment. Run `cargo doc --open -p rtrb` or check
// docs.rs/rtrb on first build and adjust names if they've drifted.

use super::Shared;

const RING_SECONDS: f64 = 3.0;
const GAIN_RAMP_MS: f32 = 15.0;

#[derive(Debug)]
pub enum OutputError {
    NoDevice,
    NoConfig,
    UnsupportedFormat,
    BuildStream(String),
    Play(String),
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputError::NoDevice => write!(f, "no output device"),
            OutputError::NoConfig => write!(f, "no supported output config"),
            OutputError::UnsupportedFormat => write!(f, "device does not support f32 samples"),
            OutputError::BuildStream(s) => write!(f, "failed to build stream: {s}"),
            OutputError::Play(s) => write!(f, "failed to start stream: {s}"),
        }
    }
}

impl std::error::Error for OutputError {}

/// Owns the cpal stream (when live) and the producer side of the ring.
/// `None` stream means resources were released after the 10s pause window
/// (section 8) or playback has not started yet; `is_live()` reflects this.
pub struct OutputStream {
    stream: Option<Stream>,
    producer: Producer<f32>,
    ring_capacity_frames: usize,
    device_channels: usize,
    device_rate: u32,
}

impl OutputStream {
    /// Builds a new cpal stream at the device's default config, requiring
    /// f32 samples, and wires the callback to `shared`. `source_rate` is
    /// recorded into `shared.out_rate` by the caller before/after this call
    /// once resampling (Phase 2) makes the two rates the same; for Phase 1
    /// the device's own default rate is what actually plays.
    pub fn build(source_rate: u32, shared: Arc<Shared>) -> Result<OutputStream, OutputError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(OutputError::NoDevice)?;

        let supported = device
            .default_output_config()
            .map_err(|_| OutputError::NoConfig)?;

        if supported.sample_format() != SampleFormat::F32 {
            // Phase 1: bail rather than silently reinterpret samples. Most
            // WASAPI shared-mode default configs are f32; if this fires in
            // practice, add an i16/u16 conversion path here.
            return Err(OutputError::UnsupportedFormat);
        }

        let config: StreamConfig = supported.config();
        let device_rate = config.sample_rate.0;
        let device_channels = config.channels as usize;

        let ring_capacity_frames = ((device_rate as f64) * RING_SECONDS) as usize;
        let ring_capacity_samples = ring_capacity_frames * 2; // stereo storage regardless of device_channels
        let (producer, mut consumer) = RingBuffer::<f32>::new(ring_capacity_samples);

        // Per-callback-instance state that must NOT be shared/atomic because
        // it is only ever touched on this one real-time thread.
        let mut current_gain: f32 = 0.0;
        let ramp_step_per_sample = {
            let ramp_samples = (GAIN_RAMP_MS / 1000.0) * device_rate as f32;
            1.0 / ramp_samples.max(1.0)
        };

        let shared_cb = Arc::clone(&shared);

        let err_fn = |_err: cpal::StreamError| {
            // Section 12.3: device error handling is the engine thread's
            // job. The callback itself must not log or allocate; cpal calls
            // this on a separate internal path, not the audio thread, so a
            // no-op here plus the engine polling `Shared` state is enough
            // for Phase 1. If a signal is needed, add one more AtomicBool.
        };

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    audio_callback(
                        data,
                        device_channels,
                        &mut consumer,
                        &shared_cb,
                        &mut current_gain,
                        ramp_step_per_sample,
                    );
                },
                err_fn,
                None,
            )
            .map_err(|e| OutputError::BuildStream(e.to_string()))?;

        shared.out_rate.store(device_rate, Ordering::Relaxed);
        let _ = source_rate; // Phase 2 compares this against device_rate to decide resampling

        Ok(OutputStream {
            stream: Some(stream),
            producer,
            ring_capacity_frames,
            device_channels,
            device_rate,
        })
    }

    pub fn play(&mut self) {
        if let Some(s) = &self.stream {
            let _ = s.play();
        }
    }

    pub fn pause(&mut self) {
        if let Some(s) = &self.stream {
            let _ = s.pause();
        }
    }

    /// Section 8: drop the stream (and with it, all callback wakeups) after
    /// 10s paused. The ring and its producer/consumer are also dropped by
    /// replacing this whole struct on the next `build()` call from `resume`.
    pub fn release(&mut self) {
        self.stream = None;
    }

    pub fn is_live(&self) -> bool {
        self.stream.is_some()
    }

    /// Approximate ring fill in milliseconds, used by the engine loop to
    /// decide when to refill (section 6 engine loop) and to size the
    /// `recv_timeout` wait. `producer.slots()` is free writable slots
    /// (rtrb), so filled = capacity − free.
    pub fn fill_ms(&self) -> u64 {
        let capacity_samples = self.ring_capacity_frames * 2;
        let filled_samples = capacity_samples.saturating_sub(self.producer.slots());
        let filled_frames = filled_samples / 2;
        (filled_frames as u64 * 1000) / self.device_rate.max(1) as u64
    }

    #[allow(dead_code)] // exposed for future capacity checks / diagnostics
    pub fn capacity_frames(&self) -> usize {
        self.ring_capacity_frames
    }

    /// Pushes an interleaved stereo chunk into the ring. Called from the
    /// engine thread only, never from the callback. If the ring is full
    /// (should not happen given the water-mark discipline in `engine/mod.rs`),
    /// remaining samples are dropped rather than blocking the engine thread.
    pub fn push_frames(&mut self, frames: &super::decode::Frames) {
        for &sample in &frames.data {
            if self.producer.push(sample).is_err() {
                break; // ring full; drop the rest of this chunk
            }
        }
    }

    /// Engine-thread-side flush: no-op. The real-time callback drains the
    /// consumer side when it observes `flush_req` (see `audio_callback`);
    /// the producer has no `pop` in rtrb. Kept as a named step so the
    /// flush sequence in `engine/mod.rs` stays readable against section 7.
    pub fn clear_ring(&mut self) {
        // Intentionally empty — callback owns the drain during flush.
    }

    /// True once the callback has reported the ring empty and the decoder
    /// has reported end-of-file (section 6, "End of track").
    pub fn is_drained_eof(&self, shared: &Shared) -> bool {
        shared.eof.load(Ordering::Relaxed) && shared.drained.load(Ordering::Relaxed)
    }

    #[allow(dead_code)] // available for Phase 2 channel-map decisions
    pub fn device_channels(&self) -> usize {
        self.device_channels
    }
}

/// The real-time callback. No allocation, no locks, no I/O, no logging, no
/// channel sends — atomics only (section 5.3).
fn audio_callback(
    data: &mut [f32],
    device_channels: usize,
    consumer: &mut Consumer<f32>,
    shared: &Arc<Shared>,
    current_gain: &mut f32,
    ramp_step_per_sample: f32,
) {
    let target_gain = shared.gain();
    let flush_req = shared.flush_req.load(Ordering::SeqCst);
    let flush_ack = shared.flush_ack.load(Ordering::SeqCst);

    if flush_req {
        // Drain once when the flush is first observed (step 3). While
        // flush_req stays true after the ack, output silence and do NOT
        // drain again — the engine prefills PREROLL during that window.
        if !flush_ack {
            while consumer.pop().is_ok() {}
            shared.played_frames.store(0, Ordering::Relaxed);
            shared.flush_ack.store(true, Ordering::SeqCst);
        }
        data.fill(0.0);
        return;
    }

    let frames_needed = data.len() / device_channels.max(1);
    let mut frames_written = 0usize;

    for frame_idx in 0..frames_needed {
        let (l, r) = match (consumer.pop(), consumer.pop()) {
            (Ok(l), Ok(r)) => (l, r),
            _ => {
                // Ring underrun: pad with silence for the remainder of this
                // callback. Section 6.9/8 — an underrun here means the
                // engine's water-mark refill fell behind, not a bug in the
                // callback itself.
                break;
            }
        };

        // Linear gain ramp toward target_gain, section 7.2/flush steps 1/7.
        if *current_gain < target_gain {
            *current_gain = (*current_gain + ramp_step_per_sample).min(target_gain);
        } else if *current_gain > target_gain {
            *current_gain = (*current_gain - ramp_step_per_sample).max(target_gain);
        }

        let out_l = l * *current_gain;
        let out_r = r * *current_gain;

        write_frame(data, frame_idx, device_channels, out_l, out_r);
        frames_written += 1;
    }

    // Silence any remaining frames in this callback (underrun tail).
    for frame_idx in frames_written..frames_needed {
        write_frame(data, frame_idx, device_channels, 0.0, 0.0);
    }

    shared
        .played_frames
        .fetch_add(frames_written as u64, Ordering::Relaxed);

    if consumer.is_empty() {
        shared.drained.store(true, Ordering::Relaxed);
    }
}

/// Channel map per section 6.3: 1 device channel gets (L+R)/2, 2 channels
/// get L and R, more than 2 get L/R in the first two and zeros elsewhere.
#[inline(always)]
fn write_frame(data: &mut [f32], frame_idx: usize, device_channels: usize, l: f32, r: f32) {
    let base = frame_idx * device_channels;
    match device_channels {
        1 => {
            data[base] = (l + r) * 0.5;
        }
        2 => {
            data[base] = l;
            data[base + 1] = r;
        }
        n => {
            data[base] = l;
            data[base + 1] = r;
            for ch in 2..n {
                data[base + ch] = 0.0;
            }
        }
    }
}
