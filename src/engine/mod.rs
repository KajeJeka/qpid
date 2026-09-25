//! Engine thread: owns the decoder, DSP chain, playlist, persistence and the
//! cpal output stream. Communicates with the UI thread only through
//! `Command` (in), `Event` (out) and `Shared` (lock-free position/state
//! atomics polled by the UI timer).
//!
//! Scope: OpenPath, Play, Pause, TogglePlay, SeekRelative, SeekAbsolute,
//! SetSpeed (Phase 2: WSOLA stretcher through the flush path), Shutdown,
//! and the flush/pause/release protocol. Next/Prev are accepted but are
//! no-ops beyond what single-file playback needs until Phase 3 lands.

pub mod decode;
pub mod dsp;
pub mod output;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use self::decode::Decoder;
use self::dsp::{Resampler, Stretcher};
use self::output::OutputStream;

// ---------------------------------------------------------------------
// Shared types (architecture.md section 5)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    X0_5,
    X1,
    X1_25,
    X1_5,
    X2,
}

impl Speed {
    pub fn as_milli(self) -> u32 {
        match self {
            Speed::X0_5 => 500,
            Speed::X1 => 1000,
            Speed::X1_25 => 1250,
            Speed::X1_5 => 1500,
            Speed::X2 => 2000,
        }
    }

    pub fn as_f64(self) -> f64 {
        self.as_milli() as f64 / 1000.0
    }

    pub fn from_index(i: u32) -> Speed {
        match i {
            0 => Speed::X0_5,
            1 => Speed::X1,
            2 => Speed::X1_25,
            3 => Speed::X1_5,
            _ => Speed::X2,
        }
    }

    /// Inverse of `as_milli` for restoring the saved speed (shared ->
    /// stretcher on reopen); unknown values fall back to 1x.
    pub fn from_milli(m: u32) -> Speed {
        match m {
            500 => Speed::X0_5,
            1250 => Speed::X1_25,
            1500 => Speed::X1_5,
            2000 => Speed::X2,
            _ => Speed::X1,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    OpenPath(PathBuf),
    Play,
    Pause,
    TogglePlay,
    SeekRelative(i64),
    SeekAbsolute(u64),
    Next,
    Prev,
    SetSpeed(Speed),
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Idle,
    Playing,
    Paused,
    Ended,
    Error,
}

#[derive(Debug, Clone)]
pub enum Event {
    TrackChanged {
        folder: String,
        title: String,
        index: u32,
        count: u32,
        duration_ms: u64,
    },
    StateChanged(PlayState),
    Message(String),
}

/// All fields are atomics so the UI timer can read position without ever
/// touching a lock or waking the engine thread. See section 7 for the
/// position formula.
pub struct Shared {
    pub base_ms: AtomicU64,
    pub played_frames: AtomicU64,
    pub speed_milli: AtomicU32,
    pub out_rate: AtomicU32,
    pub duration_ms: AtomicU64,
    pub target_gain: AtomicU32, // f32 bits, see gain()/set_gain()
    pub flush_req: AtomicBool,
    pub flush_ack: AtomicBool,
    pub eof: AtomicBool,
    pub drained: AtomicBool,
}

impl Shared {
    pub fn new() -> Arc<Shared> {
        Arc::new(Shared {
            base_ms: AtomicU64::new(0),
            played_frames: AtomicU64::new(0),
            speed_milli: AtomicU32::new(1000),
            out_rate: AtomicU32::new(48000),
            duration_ms: AtomicU64::new(0),
            target_gain: AtomicU32::new(1.0f32.to_bits()),
            flush_req: AtomicBool::new(false),
            flush_ack: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            drained: AtomicBool::new(false),
        })
    }

    pub fn set_gain(&self, g: f32) {
        self.target_gain.store(g.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.target_gain.load(Ordering::Relaxed))
    }

    /// Position formula, section 7:
    /// position_ms = base_ms + played_frames * 1000 * speed / out_rate
    pub fn position_ms(&self) -> u64 {
        let base = self.base_ms.load(Ordering::Relaxed);
        let frames = self.played_frames.load(Ordering::Relaxed);
        let speed_milli = self.speed_milli.load(Ordering::Relaxed) as u64;
        let rate = self.out_rate.load(Ordering::Relaxed).max(1) as u64;
        // frames * 1000 * (speed_milli/1000) / rate == frames * speed_milli / rate
        base + (frames.saturating_mul(speed_milli)) / rate
    }
}

// Water marks (section 6.8), in milliseconds. HIGH_WATER must stay below
// the 3 s ring capacity: refilling to exactly capacity means the last
// packet of every burst overshoots and gets dropped (measured: ~11 ms of
// audio lost per refill cycle before the 100 ms headroom was added).
const HIGH_WATER_MS: u64 = 2900;
const LOW_WATER_MS: u64 = 1000;
const PREROLL_MS: u64 = 250;
const PAUSE_RELEASE: Duration = Duration::from_secs(10);
const CHUNK_FRAMES: usize = 4096;

struct PlaybackContext {
    decoder: Decoder,
    output: OutputStream,
    stretcher: Stretcher,
    resampler: Resampler,
    path: PathBuf, // kept across a section 8.2 release to reopen at resume
}

/// Engine thread entry point. Owns everything real-time-adjacent.
/// `tx_events` sends UI-facing events; `shared` is the atomics block the UI
/// timer polls directly.
pub fn run(rx: Receiver<Command>, tx_events: Sender<Event>, shared: Arc<Shared>) {
    lower_thread_priority();

    let mut state = PlayState::Idle;
    let mut ctx: Option<PlaybackContext> = None;
    let mut paused_since: Option<std::time::Instant> = None;
    // Set by the 10 s pause release (section 8.2): the only thing kept of
    // the context besides the resume position already in `shared.base_ms`.
    let mut released_path: Option<PathBuf> = None;

    loop {
        let wait = match state {
            PlayState::Playing => {
                let fill_ms = ctx
                    .as_ref()
                    .map(|c| c.output.fill_ms())
                    .unwrap_or(HIGH_WATER_MS);
                if fill_ms <= LOW_WATER_MS {
                    Duration::from_millis(0)
                } else {
                    let until_low = fill_ms - LOW_WATER_MS;
                    Duration::from_millis(until_low.clamp(100, 2000))
                }
            }
            // Released (ctx taken): no timer — only a command wakes us,
            // which is what budget 7 ("no timers") asks for after 10 s.
            PlayState::Paused if ctx.is_some() => PAUSE_RELEASE,
            PlayState::Idle | PlayState::Ended | PlayState::Error | PlayState::Paused => {
                Duration::from_secs(3600)
            }
        };

        match rx.recv_timeout(wait) {
            Ok(cmd) => {
                if !handle_command(
                    cmd,
                    &mut state,
                    &mut ctx,
                    &mut paused_since,
                    &mut released_path,
                    &shared,
                    &tx_events,
                ) {
                    break; // Shutdown
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                on_timeout(
                    &mut state,
                    &mut ctx,
                    &mut paused_since,
                    &mut released_path,
                    &shared,
                    &tx_events,
                );
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if state == PlayState::Playing {
            let pending = if let Some(c) = ctx.as_mut() {
                let cmd = refill_until(c, &shared, HIGH_WATER_MS, &rx);
                if c.output.is_drained_eof(&shared) {
                    // End of track. Phase 1: single file, so this becomes Ended.
                    // Phase 3 replaces this branch with playlist advance.
                    state = PlayState::Ended;
                    let _ = tx_events.send(Event::StateChanged(state));
                }
                cmd
            } else {
                None
            };
            if let Some(cmd) = pending {
                if !handle_command(
                    cmd,
                    &mut state,
                    &mut ctx,
                    &mut paused_since,
                    &mut released_path,
                    &shared,
                    &tx_events,
                ) {
                    break; // Shutdown
                }
            }
        }
    }
}

/// Returns false on Shutdown (caller breaks the loop).
fn handle_command(
    cmd: Command,
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    paused_since: &mut Option<std::time::Instant>,
    released_path: &mut Option<PathBuf>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
) -> bool {
    match cmd {
        Command::Shutdown => return false,

        Command::OpenPath(path) => {
            open_path(path, state, ctx, paused_since, shared, tx_events, 0);
        }

        Command::Play => {
            if *state == PlayState::Paused {
                resume(
                    ctx,
                    state,
                    paused_since,
                    released_path,
                    shared,
                    tx_events,
                );
            } else if *state == PlayState::Ended {
                if let Some(c) = ctx.as_mut() {
                    flush_playing(c, 0, shared);
                    *state = PlayState::Playing;
                    let _ = tx_events.send(Event::StateChanged(*state));
                }
            }
        }

        Command::Pause => {
            if *state == PlayState::Playing {
                pause(ctx, state, paused_since, shared, tx_events);
            }
        }

        Command::TogglePlay => {
            match *state {
                PlayState::Playing => pause(ctx, state, paused_since, shared, tx_events),
                PlayState::Paused => resume(ctx, state, paused_since, released_path, shared, tx_events),
                _ => {}
            }
        }

        Command::SeekRelative(delta_ms) => {
            let cur = shared.position_ms() as i64;
            let target = (cur + delta_ms).max(0) as u64;
            seek_to(target, ctx, state, shared);
        }

        Command::SeekAbsolute(ms) => {
            seek_to(ms, ctx, state, shared);
        }

        // Phase 3 implements real playlist navigation (playlist.rs). Phase 1
        // has no playlist, only the single opened file, so these fall back
        // to seeking within it: Next jumps to end-of-file (which the normal
        // EOF/Ended path then handles), Prev jumps to the start.
        Command::Next => {
            let dur = shared.duration_ms.load(Ordering::Relaxed);
            seek_to(dur, ctx, state, shared);
        }
        Command::Prev => {
            seek_to(0, ctx, state, shared);
        }

        Command::SetSpeed(speed) => {
            // Section 7.1: a speed change is a flush to the current
            // position with the new speed — never a live parameter tweak.
            // Capture the position under the OLD speed first: position_ms
            // multiplies played_frames by speed_milli, so storing first
            // would jump the position (e.g. double it going 1x -> 2x).
            let cur = shared.position_ms();
            shared.speed_milli.store(speed.as_milli(), Ordering::Relaxed);
            shared.base_ms.store(cur, Ordering::Relaxed);
            shared.played_frames.store(0, Ordering::Relaxed);
            if let Some(c) = ctx.as_mut() {
                c.stretcher.set_speed(speed);
            }
            if matches!(*state, PlayState::Playing | PlayState::Paused) {
                seek_to(cur, ctx, state, shared);
            }
        }
    }
    true
}

fn seek_to(
    target_ms: u64,
    ctx: &mut Option<PlaybackContext>,
    state: &mut PlayState,
    shared: &Arc<Shared>,
) {
    let dur = shared.duration_ms.load(Ordering::Relaxed);
    let clamped = if dur > 0 {
        target_ms.min(dur.saturating_sub(1000))
    } else {
        target_ms
    };

    let Some(c) = ctx.as_mut() else {
        // Released after the 10s pause window (section 8.2): no decoder to
        // move, just record where to resume from — `resume` reopens the
        // file and seeks there.
        if matches!(*state, PlayState::Paused | PlayState::Ended) {
            shared.base_ms.store(clamped, Ordering::Relaxed);
            shared.played_frames.store(0, Ordering::Relaxed);
        }
        return;
    };

    match *state {
        PlayState::Playing => {
            flush_playing(c, clamped, shared);
        }
        PlayState::Paused | PlayState::Ended => {
            // Not producing audio right now; just record where we'll resume.
            shared.base_ms.store(clamped, Ordering::Relaxed);
            shared.played_frames.store(0, Ordering::Relaxed);
            if let Err(e) = c.decoder.seek(clamped) {
                let _ = e; // Phase 1: swallow; Phase 3 surfaces via Event::Message
            }
            c.stretcher.reset(); // no stale samples from before the seek
            c.resampler.reset(); // no stale samples from before the seek
        }
        _ => {}
    }
}

fn open_path(
    path: PathBuf,
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    paused_since: &mut Option<std::time::Instant>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    resume_ms: u64, // 0 on a fresh open; a resume-after-release position
) {
    *paused_since = None;

    let file_path = if path.is_dir() {
        // Phase 1: no playlist yet. Phase 3 replaces this with a real scan
        // (section 6 project layout: playlist.rs).
        match first_audio_file_in(&path) {
            Some(p) => p,
            None => {
                let _ = tx_events.send(Event::Message("No audio files in folder".into()));
                return;
            }
        }
    } else {
        path
    };

    match Decoder::open(&file_path) {
        Ok(decoder) => {
            let source_rate = decoder.sample_rate();
            shared
                .duration_ms
                .store(decoder.duration_ms().unwrap_or(0), Ordering::Relaxed);
            shared.base_ms.store(resume_ms, Ordering::Relaxed);
            shared.played_frames.store(0, Ordering::Relaxed);
            shared.eof.store(false, Ordering::Relaxed);
            shared.drained.store(false, Ordering::Relaxed);
            shared.set_gain(1.0);

            match OutputStream::build(Arc::clone(shared)) {
                Ok(output) => {
                    let device_rate = output.device_rate();
                    let resampler = Resampler::new(source_rate, device_rate);
                    let mut stretcher = Stretcher::new();
                    stretcher.set_speed(Speed::from_milli(
                        shared.speed_milli.load(Ordering::Relaxed),
                    ));
                    if cfg!(debug_assertions) {
                        eprintln!(
                            "[diag] source_rate={source_rate} device_rate={device_rate} channels={} resampler={}",
                            output.device_channels(),
                            if resampler.is_bypassed() { "bypass" } else { "engaged" }
                        );
                    }
                    let title = file_path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let folder = file_path
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();

                    *ctx = Some(PlaybackContext {
                        decoder,
                        output,
                        stretcher,
                        resampler,
                        path: file_path,
                    });

                    if let Some(c) = ctx.as_mut() {
                        if resume_ms > 0 {
                            let _ = c.decoder.seek(resume_ms);
                        }
                        prefill(c, shared, PREROLL_MS);
                        c.output.play();
                    }

                    *state = PlayState::Playing;
                    let _ = tx_events.send(Event::TrackChanged {
                        folder,
                        title,
                        index: 1,
                        count: 1,
                        duration_ms: shared.duration_ms.load(Ordering::Relaxed),
                    });
                    let _ = tx_events.send(Event::StateChanged(*state));
                }
                Err(e) => {
                    *state = PlayState::Error;
                    let _ = tx_events.send(Event::Message(format!("Output device error: {e}")));
                    let _ = tx_events.send(Event::StateChanged(*state));
                }
            }
        }
        Err(e) => {
            *state = PlayState::Error;
            let _ = tx_events.send(Event::Message(format!("Could not open file: {e}")));
            let _ = tx_events.send(Event::StateChanged(*state));
        }
    }
}

fn first_audio_file_in(dir: &std::path::Path) -> Option<PathBuf> {
    const EXTS: [&str; 7] = ["mp3", "m4a", "m4b", "aac", "flac", "ogg", "wav"];
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| EXTS.contains(&e.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by(|a, b| {
        natord::compare(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    entries.into_iter().next()
}

/// Fill the ring from empty up to `target_ms` worth of audio. Used on open
/// and after a flush, where there is no pending-command check to do (the
/// caller is already inside command handling). See `refill_until` for the
/// steady-state version used from the main loop.
fn prefill(ctx: &mut PlaybackContext, shared: &Arc<Shared>, target_ms: u64) {
    decode_burst(ctx, shared, target_ms);
}

fn decode_burst(ctx: &mut PlaybackContext, shared: &Arc<Shared>, target_ms: u64) {
    loop {
        if ctx.output.fill_ms() >= target_ms {
            return;
        }
        match ctx.decoder.decode_chunk(decode_want(ctx)) {
            Ok(Some(frames)) => push_resampled(ctx, &frames.data),
            Ok(None) => {
                // End of stream reached during decode.
                end_of_stream(ctx, shared);
                return;
            }
            Err(_e) => {
                // Phase 1: stop this burst on a decode error. Phase 3 adds
                // the "skip bad packet / skip to next file" behavior from
                // section 6.9 instead of just stopping.
                return;
            }
        }
    }
}

/// Steady-state refill called from the main loop while Playing. Decodes in
/// chunks of `CHUNK_FRAMES` up to `target_ms`, checking for a pending
/// command after every chunk so a Pause/Seek is never delayed by more than
/// one chunk (section 6 engine loop). Any command found is returned to the
/// caller rather than dropped, since consuming it here would lose it.
fn refill_until(
    ctx: &mut PlaybackContext,
    shared: &Arc<Shared>,
    target_ms: u64,
    rx: &Receiver<Command>,
) -> Option<Command> {
    loop {
        if ctx.output.fill_ms() >= target_ms {
            return None;
        }
        match ctx.decoder.decode_chunk(decode_want(ctx)) {
            Ok(Some(frames)) => push_resampled(ctx, &frames.data),
            Ok(None) => {
                end_of_stream(ctx, shared);
                return None;
            }
            Err(_e) => {
                return None;
            }
        }
        if let Ok(cmd) = rx.try_recv() {
            return Some(cmd);
        }
    }
}

/// Decode granularity: the stretcher eats ~4096-frame chunks (section
/// 6.5) when engaged; otherwise whatever the resampler wants next (rubato
/// consumes fixed-size input chunks), the normal chunk when both bypass.
fn decode_want(ctx: &PlaybackContext) -> usize {
    if !ctx.stretcher.is_bypassed() {
        CHUNK_FRAMES
    } else {
        ctx.resampler.input_frames_needed().unwrap_or(CHUNK_FRAMES)
    }
}

/// Decoder output -> stretcher -> resampler -> ring, the section 6.6
/// pipeline.
fn push_resampled(ctx: &mut PlaybackContext, data: &[f32]) {
    let stretched = ctx.stretcher.process(data);
    if stretched.is_empty() {
        return; // stretcher is accumulating input; no output yet
    }
    let out = ctx.resampler.process(&stretched);
    if !out.is_empty() {
        ctx.output.push_frames(&out);
    }
}

/// Decoder returned end of stream: drain the stretcher's tail (it feeds
/// the resampler), then the resampler's delayed tail into the ring once,
/// then flag eof (section 6.10 step 1). The `eof` guard matters because
/// `refill_until` keeps seeing `Ok(None)` every loop pass while playing
/// the tail, and re-flushing would push silence over audio.
fn end_of_stream(ctx: &mut PlaybackContext, shared: &Arc<Shared>) {
    if shared.eof.load(Ordering::Relaxed) {
        return;
    }
    let stretch_tail = ctx.stretcher.flush_tail();
    if !stretch_tail.is_empty() {
        let out = ctx.resampler.process(&stretch_tail);
        if !out.is_empty() {
            ctx.output.push_frames(&out);
        }
    }
    let tail = ctx.resampler.flush_tail();
    if !tail.is_empty() {
        ctx.output.push_frames(&tail);
        shared.drained.store(false, Ordering::Relaxed);
    }
    shared.eof.store(true, Ordering::Relaxed);
}

fn flush_playing(ctx: &mut PlaybackContext, target_ms: u64, shared: &Arc<Shared>) {
    // 1. Ramp down.
    shared.set_gain(0.0);
    std::thread::sleep(Duration::from_millis(20));

    // 2/3/4. Request flush, wait for callback ack.
    shared.flush_req.store(true, Ordering::SeqCst);
    let start = std::time::Instant::now();
    while !shared.flush_ack.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_millis(200) {
            break; // device stall; proceed anyway rather than hang the engine
        }
        std::thread::park_timeout(Duration::from_millis(2));
    }

    // 5. Seek decoder, reset position bookkeeping. Stretcher and resampler
    // are stateful stream processors: stale filter/history across the seek
    // clicks and wobbles pitch, so both reset alongside the decoder seek
    // (section 7.5 requires both).
    ctx.output.clear_ring();
    let _ = ctx.decoder.seek(target_ms);
    ctx.stretcher.reset();
    ctx.resampler.reset();
    shared.base_ms.store(target_ms, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
    shared.eof.store(false, Ordering::Relaxed);
    shared.drained.store(false, Ordering::Relaxed);

    // 6. Refill, then release the flush gate. Clear flush_req first so the
    // callback starts reading again while flush_ack is still true; clearing
    // ack first would open a window where req&&!ack causes a re-drain of
    // the just-prefilled PREROLL.
    prefill(ctx, shared, PREROLL_MS);
    shared.flush_req.store(false, Ordering::SeqCst);
    shared.flush_ack.store(false, Ordering::SeqCst);

    // 7. Ramp up.
    shared.set_gain(1.0);
}

fn pause(
    ctx: &mut Option<PlaybackContext>,
    state: &mut PlayState,
    paused_since: &mut Option<std::time::Instant>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
) {
    if let Some(c) = ctx.as_mut() {
        shared.set_gain(0.0);
        std::thread::sleep(Duration::from_millis(20));
        c.output.pause();
    }
    *state = PlayState::Paused;
    *paused_since = Some(std::time::Instant::now());
    let _ = tx_events.send(Event::StateChanged(*state));
}

fn resume(
    ctx: &mut Option<PlaybackContext>,
    state: &mut PlayState,
    paused_since: &mut Option<std::time::Instant>,
    released_path: &mut Option<PathBuf>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
) {
    *paused_since = None;

    // Released after the 10 s pause window (section 8.2): reopen the file
    // at the stored position — open_path rebuilds decoder, stream, ring and
    // resampler from scratch.
    if ctx.is_none() {
        if let Some(path) = released_path.take() {
            let resume_ms = shared.base_ms.load(Ordering::Relaxed);
            open_path(path, state, ctx, paused_since, shared, tx_events, resume_ms);
        }
        return;
    }

    if let Some(c) = ctx.as_mut() {
        shared.set_gain(1.0);
        c.output.play();
    }
    *state = PlayState::Playing;
    let _ = tx_events.send(Event::StateChanged(*state));
}

fn on_timeout(
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    paused_since: &mut Option<std::time::Instant>,
    released_path: &mut Option<PathBuf>,
    _shared: &Arc<Shared>,
    _tx_events: &Sender<Event>,
) {
    if *state == PlayState::Paused && ctx.is_some() {
        if let Some(since) = *paused_since {
            if since.elapsed() >= PAUSE_RELEASE {
                // Section 8.2: release stream, ring, decoder and resampler;
                // keep only the file path and the resume position
                // (shared.base_ms).
                *released_path = ctx.take().map(|c| c.path);
                *paused_since = None;
            }
        }
    }
}

#[cfg(windows)]
fn lower_thread_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
fn lower_thread_priority() {
    // No-op on non-Windows dev hosts (this project targets Windows 11, but
    // `cargo check` on other platforms should still compile for iteration).
}
