//! DSP chain stages between the decoder and the ring buffer: time-stretch
//! (pitch-preserving speed change) and sample-rate conversion.
//!
//! Both stages are live: a hand-written WSOLA stretcher (architecture.md
//! §4 note 4 fallback — signalsmith-stretch needs libclang to build) and a
//! real `rubato` FFT resampler whenever the file rate differs from the
//! device rate (the device only offers 48 kHz, so 44.1 kHz files are the
//! common case). Order is decode -> stretch -> resample (section 6.6).

use crate::engine::Speed;
use rubato::Resampler as _;

/// Time-stretch stage: WSOLA (waveform similarity overlap-add).
///
/// Chosen over signalsmith-stretch (spec §4.6 first choice) when its build
/// failed for missing libclang; §4 note 4 sanctions "a WSOLA stretcher
/// (about 150 lines)" as the fallback. Speech is the main content, which
/// is exactly WSOLA's strong case.
///
/// Per synthesis step: pick the next `STRETCH_FRAME` of source starting
/// near the nominal analysis position, shifted by up to `±STRETCH_SEARCH`
/// so it correlates best with where the previous frame said the signal
/// continues (that search is what keeps the pitch — plain overlap-add of
/// unaligned frames would smear it), window it with a Hann, overlap-add
/// into the output, advance analysis by `STRETCH_HOP` and synthesis by
/// `STRETCH_HOP / speed`.
///
/// 1x is a pure copy (section 6.6 rule 6: no stretcher work at all).
pub struct Stretcher {
    speed: Speed,
    /// Synthesis hop: STRETCH_HOP / speed (integer, >= 1).
    hts: usize,
    /// Deinterleaved source awaiting analysis, absolute positions
    /// `[in_base, in_base + len)`. The consumed prefix is drained each
    /// `process` call, so this stays bounded (a few frames of window).
    in_buf: [Vec<f32>; 2],
    in_base: u64,
    /// Nominal analysis position, absolute source frames.
    ana: u64,
    /// Mono reference for the correlation search (previous frame's
    /// continuation); empty until the first frame is selected.
    ref_seg: Vec<f32>,
    /// Mono scratch covering the search window of one step.
    mono: Vec<f32>,
    /// Overlap-add accumulator, exactly one window per channel.
    ola: [Vec<f32>; 2],
    window: Vec<f32>,
    /// Set once `flush_tail` drained; cleared by `process`/`reset` so the
    /// EOF drain can never run twice (same contract as `Resampler`).
    flushed: bool,
}

/// Window length. 1024 frames ≈ 23 ms at 44.1 kHz: long enough for the
/// correlation to see a pitch period, short enough to keep EOF latency low.
const STRETCH_FRAME: usize = 1024;
/// Analysis hop: N/4 (75% analysis overlap). The N/4 (not N/2) keeps at
/// least 50% overlap between synthesis windows even at 0.5x, where the
/// synthesis hop doubles to 1024.
const STRETCH_HOP: usize = 256;
/// Candidate offsets around the nominal position, in frames (±96 ≈ ±2.2 ms
/// at 44.1 kHz — covers a jittered pitch period at speech fundamentals).
const STRETCH_SEARCH: i64 = 96;
/// Correlation length. 512 frames ≈ 11.6 ms covers ~1 period of a low
/// male voice (85 Hz ≈ 11.7 ms); beyond that, extra length mostly costs CPU.
const STRETCH_CORR: usize = 512;
/// Subsample the correlation by 2: halves the search cost (the dominant
/// CPU at 2x) for a <= 1-sample alignment difference — inaudible on speech.
const STRETCH_CORR_STEP: usize = 2;

impl Stretcher {
    pub fn new() -> Self {
        let mut window = Vec::with_capacity(STRETCH_FRAME);
        for i in 0..STRETCH_FRAME {
            // Hann, periodic form: sums to a constant across hops (COLA).
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / STRETCH_FRAME as f32).cos();
            window.push(w as f32);
        }
        Stretcher {
            speed: Speed::X1,
            hts: STRETCH_HOP,
            in_buf: [Vec::new(), Vec::new()],
            in_base: 0,
            ana: 0,
            ref_seg: Vec::new(),
            mono: Vec::new(),
            ola: [vec![0.0; STRETCH_FRAME], vec![0.0; STRETCH_FRAME]],
            window,
            flushed: false,
        }
    }

    pub fn set_speed(&mut self, speed: Speed) {
        self.speed = speed;
        self.hts = ((STRETCH_HOP as f64) / speed.as_f64()).round().max(1.0) as usize;
    }

    pub fn is_bypassed(&self) -> bool {
        self.speed == Speed::X1
    }

    /// Feeds interleaved stereo source frames, returns interleaved
    /// stretched frames as whole synthesis steps become ready (may return
    /// fewer frames than the ratio implies — that is the streaming delay;
    /// `flush_tail` drains it at EOF). Engine thread only: allocations
    /// here are allowed (section 5.3 covers the audio callback only).
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.is_bypassed() {
            self.flushed = false;
            return input.to_vec(); // section 6.6 rule 6: straight copy
        }
        self.flushed = false;
        for frame in input.chunks_exact(2) {
            self.in_buf[0].push(frame[0]);
            self.in_buf[1].push(frame[1]);
        }
        let mut out = Vec::new();
        while self.step_ready() {
            self.step(&mut out);
            self.drain_consumed();
        }
        out
    }

    /// Called on every flush (seek, speed change, replay): drops all
    /// stream state so the next audio starts from a clean slate.
    pub fn reset(&mut self) {
        self.in_buf[0].clear();
        self.in_buf[1].clear();
        self.in_base = 0;
        self.ana = 0;
        self.ref_seg.clear();
        self.ola[0].fill(0.0);
        self.ola[1].fill(0.0);
        self.flushed = false;
    }

    /// End-of-file drain: pads the source with enough silence for the last
    /// real window to be selected, runs the remaining steps, then shifts
    /// the final window's tail out of the overlap-add accumulator with a
    /// bounded number of silent steps. Subsequent calls return empty.
    pub fn flush_tail(&mut self) -> Vec<f32> {
        if self.flushed {
            return Vec::new();
        }
        self.flushed = true;
        if self.is_bypassed() {
            return Vec::new();
        }
        let real_end = self.in_base + self.in_buf[0].len() as u64;
        let pad = STRETCH_FRAME + STRETCH_SEARCH as usize + self.hts + STRETCH_CORR;
        let target = self.in_buf[0].len() + pad;
        self.in_buf[0].resize(target, 0.0);
        self.in_buf[1].resize(target, 0.0);

        let mut out = Vec::new();
        while self.ana < real_end && self.step_ready() {
            self.step(&mut out);
            self.drain_consumed();
        }
        // The last window's tail still sits in the accumulator; content is
        // gone once it has fully shifted out (ceil(N/hts) steps), +2 slack.
        // Deliberate fixed bound — an epsilon scan could loop on hot tails.
        let drain_steps = STRETCH_FRAME.div_ceil(self.hts) + 2;
        for _ in 0..drain_steps {
            self.emit_shift(&mut out);
        }
        out
    }

    /// One step needs every byte any candidate or the reference copy can
    /// read: `ana ± SEARCH` frame starts, `+ FRAME` for the candidate,
    /// `+ hts + CORR` for taking the next reference.
    fn step_ready(&self) -> bool {
        let end = self.ana
            + STRETCH_SEARCH as u64
            + (STRETCH_FRAME.max(self.hts + STRETCH_CORR)) as u64;
        end <= self.in_base + self.in_buf[0].len() as u64
    }

    fn step(&mut self, out: &mut Vec<f32>) {
        let ana = self.ana;
        let start = ana.saturating_sub(STRETCH_SEARCH as u64);
        let end = ana + STRETCH_SEARCH as u64 + (STRETCH_FRAME.max(self.hts + STRETCH_CORR)) as u64;
        let s = (start - self.in_base) as usize;
        let e = (end - self.in_base) as usize;

        // Mono window for the search (one deinterleave pass per step).
        self.mono.resize(e - s, 0.0);
        for i in 0..(e - s) {
            self.mono[i] = (self.in_buf[0][s + i] + self.in_buf[1][s + i]) * 0.5;
        }
        // ana - start, i.e. where the nominal position sits in `mono`.
        let off_base = (ana - start) as i64;

        let sel = if self.ref_seg.is_empty() {
            ana // first frame: nothing to align with yet
        } else {
            // Unnormalized correlation (energy varies little across the
            // small candidate range, so normalization buys little).
            let mut best = f32::MIN;
            let mut best_off = 0i64;
            for off in -STRETCH_SEARCH..=STRETCH_SEARCH {
                let pos = (off_base + off) as usize;
                let mut c = 0.0f32;
                for j in (0..STRETCH_CORR).step_by(STRETCH_CORR_STEP) {
                    c += self.mono[pos + j] * self.ref_seg[j];
                }
                if c > best {
                    best = c;
                    best_off = off;
                }
            }
            (ana as i64 + best_off) as u64
        };

        // Reference for the NEXT step: where this frame says the signal
        // continues (its source at the synthesis hop offset).
        let rel = (sel - start) as usize;
        self.ref_seg.clear();
        self.ref_seg
            .extend_from_slice(&self.mono[rel + self.hts..rel + self.hts + STRETCH_CORR]);

        // Window and overlap-add the selected frame, emit the finished
        // head of the accumulator, shift the rest down.
        let sel_idx = (sel - self.in_base) as usize;
        for ch in 0..2 {
            for i in 0..STRETCH_FRAME {
                self.ola[ch][i] += self.window[i] * self.in_buf[ch][sel_idx + i];
            }
        }
        self.emit_shift(out);
        self.ana += STRETCH_HOP as u64;
    }

    /// Push the accumulator's finished head to `out` (interleaved) and
    /// shift the tail down, zero-padding back to one window.
    fn emit_shift(&mut self, out: &mut Vec<f32>) {
        let hts = self.hts;
        let base = out.len();
        out.resize(base + 2 * hts, 0.0);
        for ch in 0..2 {
            for i in 0..hts {
                out[base + 2 * i + ch] = self.ola[ch][i];
            }
            self.ola[ch].drain(..hts);
            self.ola[ch].resize(STRETCH_FRAME, 0.0);
        }
    }

    /// Drop source the future can no longer read (everything before the
    /// next step's earliest candidate). Keeps `in_buf` bounded.
    fn drain_consumed(&mut self) {
        let keep_from = self.ana.saturating_sub(STRETCH_SEARCH as u64);
        let n = (keep_from.saturating_sub(self.in_base)) as usize;
        let n = n.min(self.in_buf[0].len());
        if n > 0 {
            self.in_buf[0].drain(..n);
            self.in_buf[1].drain(..n);
            self.in_base += n as u64;
        }
    }
}

/// Sample-rate conversion stage. Wraps `rubato::FftFixedIn`, a fixed
/// input-chunk FFT resampler: it consumes exactly `input_frames_next()`
/// source frames per call and emits the matching device-rate frames.
///
/// The engine's decoder hands over interleaved L,R,L,R stereo, rubato wants
/// deinterleaved per-channel buffers, so this wrapper owns the conversion
/// plus an input `pending` buffer that accumulates decoded packets until a
/// full rubato chunk is available (decoder packets are ~1152 frames for
/// mp3, not our chunk size).
///
/// Bypass path (`source_rate == device_rate`, or construction failure):
/// zero-cost pass-through, `input_frames_needed()` reports `None` so the
/// engine keeps its normal decode chunk size.
pub struct Resampler {
    inner: Option<rubato::FftFixedIn<f32>>,
    /// Deinterleaved decoded frames waiting for a full rubato chunk.
    pending: [Vec<f32>; 2],
    /// Preallocated deinterleave/reinterleave scratch, sized by rubato.
    in_buf: Vec<Vec<f32>>,
    out_buf: Vec<Vec<f32>>,
    /// Set once `flush_tail` has drained; cleared by `process`/`reset` so
    /// the EOF drain can never run twice and push silence into the ring.
    flushed: bool,
}

/// Input frames per rubato call. 1024 in / 1025-ish out at 44.1→48 kHz
/// keeps refill granularity well under one decoder packet.
const RESAMPLE_CHUNK_IN: usize = 1024;
const RESAMPLE_SUB_CHUNKS: usize = 1;
const RESAMPLE_CHANNELS: usize = 2;
/// Bound on the EOF drain: each `process_partial(None)` call pops the
/// remaining saved input (worst case one full chunk) plus the FFT overlap
/// tail (`fft_size_out/2` frames of delay), so 3 calls always covers it
/// with ~70 ms of trailing silence. Deliberate ceiling — if a future
/// rubato upgrade changes the delay math, revisit this count.
const FLUSH_CALLS: usize = 3;

impl Resampler {
    pub fn new(source_rate: u32, device_rate: u32) -> Self {
        let inner = if source_rate == 0 || device_rate == 0 || source_rate == device_rate {
            None
        } else {
            match rubato::FftFixedIn::<f32>::new(
                source_rate as usize,
                device_rate as usize,
                RESAMPLE_CHUNK_IN,
                RESAMPLE_SUB_CHUNKS,
                RESAMPLE_CHANNELS,
            ) {
                Ok(r) => Some(r),
                Err(e) => {
                    // Engine thread, not the audio callback, so a log is
                    // fine — but debug-only (global rule 10: no un-gated
                    // logging reachable in release UI mode).
                    if cfg!(debug_assertions) {
                        eprintln!(
                            "[qpid] resampler {source_rate}->{device_rate} failed: {e}; playing uncorrected"
                        );
                    }
                    None
                }
            }
        };
        let (in_buf, out_buf) = match &inner {
            Some(r) => (r.input_buffer_allocate(true), r.output_buffer_allocate(true)),
            None => (Vec::new(), Vec::new()),
        };
        Resampler {
            inner,
            pending: [Vec::new(), Vec::new()],
            in_buf,
            out_buf,
            flushed: false,
        }
    }

    pub fn is_bypassed(&self) -> bool {
        self.inner.is_none()
    }

    /// How many source frames rubato wants next, or `None` when bypassed
    /// (caller keeps its own default chunk size).
    pub fn input_frames_needed(&self) -> Option<usize> {
        self.inner.as_ref().map(|r| r.input_frames_next())
    }

    /// Resamples an interleaved stereo chunk, buffering internally until a
    /// full rubato chunk is available. Returns interleaved device-rate
    /// frames; empty when not enough input has accumulated yet. Engine
    /// thread only — allocations here are allowed (section 5.3 applies to
    /// the audio callback, not this thread).
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let Some(inner) = self.inner.as_mut() else {
            return input.to_vec();
        };
        self.flushed = false;
        for frame in input.chunks_exact(2) {
            self.pending[0].push(frame[0]);
            self.pending[1].push(frame[1]);
        }

        let chunk = inner.input_frames_next();
        let mut out = Vec::new();
        while self.pending[0].len() >= chunk {
            for ch in 0..RESAMPLE_CHANNELS {
                self.in_buf[ch][..chunk].copy_from_slice(&self.pending[ch][..chunk]);
                self.pending[ch].drain(..chunk);
            }
            let Ok((consumed, out_len)) =
                inner.process_into_buffer(&self.in_buf, &mut self.out_buf, None)
            else {
                return out; // validation error: impossible with fixed buffers, bail
            };
            debug_assert_eq!(consumed, chunk);
            interleave_into(&mut out, &self.out_buf, out_len);
        }
        out
    }

    /// Called on every flush (seek, speed change, replay) to drop stale
    /// filter history — a stateful stream processor keeps the old signal
    /// otherwise, which clicks and wobbles pitch across the discontinuity.
    pub fn reset(&mut self) {
        self.pending[0].clear();
        self.pending[1].clear();
        self.flushed = false;
        if let Some(r) = self.inner.as_mut() {
            r.reset();
        }
    }

    /// End-of-file drain: pushes the final partial input through (rubato
    /// zero-pads it), then calls `process_partial(None)` a bounded number
    /// of times to flush the delayed tail out of rubato's internal buffers.
    /// Subsequent calls return empty until new `process` input arrives.
    pub fn flush_tail(&mut self) -> Vec<f32> {
        if self.flushed {
            return Vec::new();
        }
        self.flushed = true;
        let Some(inner) = self.inner.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if !self.pending[0].is_empty() {
            let pending: &[Vec<f32>] = &self.pending;
            if let Ok((_, out_len)) =
                inner.process_partial_into_buffer(Some(pending), &mut self.out_buf, None)
            {
                interleave_into(&mut out, &self.out_buf, out_len);
            }
            self.pending[0].clear();
            self.pending[1].clear();
        }
        let none: Option<&[Vec<f32>]> = None;
        for _ in 0..FLUSH_CALLS {
            match inner.process_partial_into_buffer(none, &mut self.out_buf, None) {
                Ok((_, 0)) => break, // nothing saved anymore; rest is zeros
                Ok((_, out_len)) => interleave_into(&mut out, &self.out_buf, out_len),
                Err(_) => break,
            }
        }
        out
    }
}

/// Deinterleaved rubato output buffers → one interleaved Vec.
fn interleave_into(out: &mut Vec<f32>, bufs: &[Vec<f32>], len: usize) {
    for i in 0..len {
        out.push(bufs[0][i]);
        out.push(bufs[1][i]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interleaved stereo constant-valued signal.
    fn stereo(n: usize, l: f32, r: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * 2);
        for _ in 0..n {
            v.push(l);
            v.push(r);
        }
        v
    }

    /// Interleaved stereo sine (both channels identical).
    fn sine(n: usize, freq: f32, rate: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (std::f32::consts::TAU * freq * i as f32 / rate).sin();
            v.push(s);
            v.push(s);
        }
        v
    }

    #[test]
    fn bypass_when_rates_match() {
        let mut r = Resampler::new(48000, 48000);
        assert!(r.is_bypassed());
        assert!(r.input_frames_needed().is_none());
        let input = stereo(64, 0.5, -0.5);
        assert_eq!(r.process(&input), input);
        assert!(r.flush_tail().is_empty());
    }

    #[test]
    fn upsamples_44100_to_48000() {
        let mut r = Resampler::new(44100, 48000);
        assert!(!r.is_bypassed());
        let n = 44100;
        let out = r.process(&sine(n, 440.0, 44100.0));
        let ratio = out.len() as f64 / 2.0 / n as f64;
        // Bypass would give 1.0; the wrong direction (44100/48000) gives 0.919.
        assert!(ratio > 1.03 && ratio < 1.10, "stream ratio = {ratio}");
        let tail = r.flush_tail();
        assert!(!tail.is_empty(), "EOF drain must emit the delayed tail");
        assert!(r.flush_tail().is_empty(), "second flush must be empty");
        let total = (out.len() + tail.len()) as f64 / 2.0 / n as f64;
        assert!(total < 1.20, "total ratio = {total}");
    }

    #[test]
    fn channels_stay_separate() {
        let mut r = Resampler::new(44100, 48000);
        let out = r.process(&stereo(12000, 1.0, -1.0));
        // Skip the resampler startup delay (~1000 frames) before checking.
        for i in (4000..out.len().saturating_sub(4)).step_by(2) {
            assert!(out[i] > 0.5, "L at {i} = {}", out[i]);
            assert!(out[i + 1] < -0.5, "R at {i} = {}", out[i + 1]);
        }
    }

    #[test]
    fn reset_clears_state() {
        let mut a = Resampler::new(44100, 48000);
        let mut b = Resampler::new(44100, 48000);
        let first = sine(9000, 300.0, 44100.0);
        let second = sine(9000, 500.0, 44100.0);
        a.process(&first);
        a.reset();
        let oa = a.process(&second);
        let ob = b.process(&second);
        assert_eq!(oa.len(), ob.len());
        for (x, y) in oa.iter().zip(&ob) {
            assert!((x - y).abs() < 1e-4, "{x} vs {y}");
        }
    }

    // ---- Stretcher (Phase 2, WSOLA) ----

    /// Run the full pipeline shape the engine uses: feed in ~4096-frame
    /// chunks, then drain at EOF.
    fn stretch(speed: Speed, input: &[f32]) -> Vec<f32> {
        let mut s = Stretcher::new();
        s.set_speed(speed);
        let mut out = Vec::new();
        for chunk in input.chunks(2 * 4096) {
            out.extend(s.process(chunk));
        }
        out.extend(s.flush_tail());
        out
    }

    /// Goertzel power of `freq` over one real signal (interleaved ch0).
    fn goertzel_power(ch0: &[f32], rate: f32, freq: f32) -> f32 {
        let coeff = 2.0 * (2.0 * std::f32::consts::PI * freq / rate).cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &x in ch0 {
            let s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        s1 * s1 + s2 * s2 - coeff * s1 * s2
    }

    /// Strongest frequency in 150..=900 Hz on ch0 of an interleaved slice.
    fn peak_hz(ch0: &[f32], rate: f32) -> f32 {
        let mut best = (0.0f32, 0.0f32); // (power, freq)
        for f in 150..=900 {
            let p = goertzel_power(ch0, rate, f as f32);
            if p > best.0 {
                best = (p, f as f32);
            }
        }
        best.1
    }

    #[test]
    fn stretch_bypass_identity_at_1x() {
        let mut s = Stretcher::new();
        assert!(s.is_bypassed());
        let input = stereo(4097, 0.5, -0.5);
        assert_eq!(s.process(&input), input);
        assert!(s.flush_tail().is_empty());
        s.set_speed(Speed::X1_5);
        assert!(!s.is_bypassed());
        s.set_speed(Speed::X1);
        assert!(s.is_bypassed());
    }

    #[test]
    fn stretch_output_length_matches_ratio() {
        let n = 44100;
        let input = sine(n, 440.0, 44100.0);
        for speed in [Speed::X0_5, Speed::X1_25, Speed::X1_5, Speed::X2] {
            let out = stretch(speed, &input);
            let frames = out.len() / 2;
            let expected = n as f64 / speed.as_f64();
            let ratio = frames as f64 / expected;
            // The WSOLA tail (last window, ~1 frame) overshoots slightly;
            // a bypass stub would give exactly `speed`, failing every one.
            assert!(
                ratio > 0.90 && ratio < 1.10,
                "speed {speed:?}: got {frames} frames, expected {expected} (ratio {ratio:.3})"
            );
        }
    }

    #[test]
    fn stretch_preserves_pitch() {
        let n = 44100;
        let input = sine(n, 440.0, 44100.0);
        for speed in [Speed::X0_5, Speed::X1_25, Speed::X1_5, Speed::X2] {
            let out = stretch(speed, &input);
            let frames = out.len() / 2;
            // Analyze the middle, away from ramp-in/out and EOF tail.
            let start_frame = frames / 3;
            let ch0: Vec<f32> = out[start_frame * 2..(start_frame + 8192) * 2]
                .iter()
                .step_by(2)
                .copied()
                .collect();
            let peak = peak_hz(&ch0, 44100.0);
            assert!(
                (peak - 440.0).abs() <= 2.0,
                "speed {speed:?}: fundamental {peak} Hz, expected 440"
            );
            let p440 = goertzel_power(&ch0, 44100.0, 440.0);
            let p220 = goertzel_power(&ch0, 44100.0, 220.0);
            let p880 = goertzel_power(&ch0, 44100.0, 880.0);
            assert!(
                p440 > 3.0 * p220 && p440 > 3.0 * p880,
                "speed {speed:?}: sub/over-harmonic leak (440={p440:.0} 220={p220:.0} 880={p880:.0})"
            );
        }
    }

    #[test]
    fn stretcher_reset_clears_state() {
        let mut a = Stretcher::new();
        let mut b = Stretcher::new();
        for s in [&mut a, &mut b] {
            s.set_speed(Speed::X1_5);
        }
        let first = sine(9000, 300.0, 44100.0);
        let second = sine(9000, 500.0, 44100.0);
        a.process(&first);
        a.reset();
        let oa = a.process(&second);
        let ob = b.process(&second);
        assert_eq!(oa.len(), ob.len());
        for (x, y) in oa.iter().zip(&ob) {
            assert!((x - y).abs() < 1e-4, "{x} vs {y}");
        }
    }
}
