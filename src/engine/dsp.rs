//! DSP chain stages between the decoder and the ring buffer: time-stretch
//! (pitch-preserving speed change) and sample-rate conversion.
//!
//! Phase 1 scope: both stages are pass-through. At 1x speed, section 6.6
//! requires skipping the stretcher entirely — that is the default and only
//! path implemented here. Phase 2 fills in the real stretch engine
//! (signalsmith-stretch, pending crate verification — see Cargo.toml) and
//! the rubato resampler for when the file rate differs from the device
//! rate.

use crate::engine::Speed;

/// Time-stretch stage. Phase 1: bypass only. Phase 2 wraps
/// signalsmith-stretch here, fed in ~4096 frame chunks (section 6.5), with
/// `reset()` called on every flush and `flush()` called at end-of-file to
/// drain the stretcher's internal tail.
pub struct Stretcher {
    speed: Speed,
}

impl Stretcher {
    pub fn new() -> Self {
        Stretcher { speed: Speed::X1 }
    }

    pub fn set_speed(&mut self, speed: Speed) {
        self.speed = speed;
    }

    pub fn is_bypassed(&self) -> bool {
        self.speed == Speed::X1
    }

    /// Phase 1: identity. At speeds other than 1x, until Phase 2 lands, this
    /// still passes audio through unchanged rather than silently mis-timing
    /// it — the engine only claims support for 1x in this phase.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        input.to_vec()
    }

    /// Called on every flush (seek, speed change, next/prev). Phase 2: reset
    /// internal stretcher state. Phase 1: no-op.
    pub fn reset(&mut self) {}

    /// Called at end-of-file to drain any buffered tail. Phase 2 only.
    pub fn flush_tail(&mut self) -> Vec<f32> {
        Vec::new()
    }
}

/// Sample-rate conversion stage. Phase 1: bypass only, since Phase 1's
/// acceptance tests (architecture.md section 16) do not require mixed
/// source/device rates to resample correctly yet — the output device is
/// opened at its default config and files are expected to match or be close
/// enough that cpal's own conversion (if any) is not relied upon here.
/// Phase 2 replaces this with a real `rubato` fixed-input FFT resampler
/// whenever `source_rate != device_rate`.
pub struct Resampler {
    source_rate: u32,
    device_rate: u32,
}

impl Resampler {
    pub fn new(source_rate: u32, device_rate: u32) -> Self {
        Resampler {
            source_rate,
            device_rate,
        }
    }

    pub fn is_bypassed(&self) -> bool {
        self.source_rate == self.device_rate
    }

    /// Phase 1: identity when rates match (the expected case for the CLI
    /// smoke test). When rates differ, still passes through unchanged and
    /// relies on cpal's device config negotiation — Phase 2 is required
    /// before mismatched rates are considered correctly handled.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        input.to_vec()
    }
}
