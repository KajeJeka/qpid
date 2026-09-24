//! Engine thread: owns the decoder, DSP chain, playlist, persistence and the
//! cpal output stream. Communicates with the UI thread only through
//! `Command` (in), `Event` (out) and `Shared` (lock-free position/state
//! atomics polled by the UI timer).
//!
//! Phase 0/1 scope: OpenPath, Play, Pause, TogglePlay, SeekRelative,
//! SeekAbsolute, Shutdown, and the flush/pause/release protocol at 1x only.
//! Next/Prev/SetSpeed are accepted but are no-ops beyond what single-file
//! playback needs until Phase 2/3 land.

pub mod decode;
pub mod dsp;
pub mod output;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use self::decode::Decoder;
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

// Water marks (section 6.8), in milliseconds.
const HIGH_WATER_MS: u64 = 3000;
const LOW_WATER_MS: u64 = 1000;
const PREROLL_MS: u64 = 250;
const PAUSE_RELEASE: Duration = Duration::from_secs(10);
const CHUNK_FRAMES: usize = 4096;

struct PlaybackContext {
    decoder: Decoder,
    output: OutputStream,
    #[allow(dead_code)] // consumed by Phase 3 playlist navigation
    path: PathBuf,
}

/// Engine thread entry point. Owns everything real-time-adjacent.
/// `tx_events` sends UI-facing events; `shared` is the atomics block the UI
/// timer polls directly.
pub fn run(rx: Receiver<Command>, tx_events: Sender<Event>, shared: Arc<Shared>) {
    lower_thread_priority();

    let mut state = PlayState::Idle;
    let mut ctx: Option<PlaybackContext> = None;
    let mut paused_since: Option<std::time::Instant> = None;

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
            PlayState::Paused => PAUSE_RELEASE,
            PlayState::Idle | PlayState::Ended | PlayState::Error => Duration::from_secs(3600),
        };

        match rx.recv_timeout(wait) {
            Ok(cmd) => {
                if !handle_command(
                    cmd,
                    &mut state,
                    &mut ctx,
                    &mut paused_since,
                    &shared,
                    &tx_events,
                ) {
                    break; // Shutdown
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                on_timeout(&mut state, &mut ctx, &mut paused_since, &shared, &tx_events);
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
                if !handle_command(cmd, &mut state, &mut ctx, &mut paused_since, &shared, &tx_events) {
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
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
) -> bool {
    match cmd {
        Command::Shutdown => return false,

        Command::OpenPath(path) => {
            open_path(path, state, ctx, paused_since, shared, tx_events);
        }

        Command::Play => {
            if *state == PlayState::Paused {
                resume(ctx, state, paused_since, shared, tx_events);
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
                PlayState::Paused => resume(ctx, state, paused_since, shared, tx_events),
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
            // Phase 1: store the value so position math stays correct; the
            // actual stretcher bypass/engage is Phase 2 scope.
            shared.speed_milli.store(speed.as_milli(), Ordering::Relaxed);
            if *state == PlayState::Playing {
                let cur = shared.position_ms();
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
    let Some(c) = ctx.as_mut() else { return };
    let dur = shared.duration_ms.load(Ordering::Relaxed);
    let clamped = if dur > 0 {
        target_ms.min(dur.saturating_sub(1000))
    } else {
        target_ms
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
            let out_rate = decoder.sample_rate();
            shared.out_rate.store(out_rate, Ordering::Relaxed);
            shared
                .duration_ms
                .store(decoder.duration_ms().unwrap_or(0), Ordering::Relaxed);
            shared.base_ms.store(0, Ordering::Relaxed);
            shared.played_frames.store(0, Ordering::Relaxed);
            shared.eof.store(false, Ordering::Relaxed);
            shared.drained.store(false, Ordering::Relaxed);
            shared.set_gain(1.0);

            match OutputStream::build(out_rate, Arc::clone(shared)) {
                Ok(output) => {
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
                        path: file_path,
                    });

                    if let Some(c) = ctx.as_mut() {
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
        match ctx.decoder.decode_chunk(CHUNK_FRAMES) {
            Ok(Some(frames)) => {
                ctx.output.push_frames(&frames);
            }
            Ok(None) => {
                // End of stream reached during decode.
                shared.eof.store(true, Ordering::Relaxed);
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
        match ctx.decoder.decode_chunk(CHUNK_FRAMES) {
            Ok(Some(frames)) => {
                ctx.output.push_frames(&frames);
            }
            Ok(None) => {
                shared.eof.store(true, Ordering::Relaxed);
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

    // 5. Seek decoder, reset position bookkeeping.
    ctx.output.clear_ring();
    let _ = ctx.decoder.seek(target_ms);
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
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
) {
    *paused_since = None;

    // If resources were released after the 10s pause window, rebuild them.
    if ctx.as_ref().map(|c| c.output.is_live()).unwrap_or(false) == false {
        if let Some(c) = ctx.as_mut() {
            let resume_ms = shared.base_ms.load(Ordering::Relaxed);
            let out_rate = shared.out_rate.load(Ordering::Relaxed);
            match OutputStream::build(out_rate, Arc::clone(shared)) {
                Ok(output) => {
                    c.output = output;
                    let _ = c.decoder.seek(resume_ms);
                    shared.played_frames.store(0, Ordering::Relaxed);
                    shared.eof.store(false, Ordering::Relaxed);
                    shared.drained.store(false, Ordering::Relaxed);
                    prefill(c, shared, PREROLL_MS);
                }
                Err(e) => {
                    *state = PlayState::Error;
                    let _ = tx_events.send(Event::Message(format!("Output device error: {e}")));
                    let _ = tx_events.send(Event::StateChanged(*state));
                    return;
                }
            }
        }
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
    shared: &Arc<Shared>,
    _tx_events: &Sender<Event>,
) {
    if *state == PlayState::Paused {
        if let (Some(since), Some(c)) = (*paused_since, ctx.as_mut()) {
            if since.elapsed() >= PAUSE_RELEASE {
                // Section 8: release stream/decoder/DSP, keep only the path
                // and resume position (already in `shared.base_ms`).
                c.output.release();
            }
        }
    }
    let _ = state; // no state transition on timeout otherwise in Phase 1
    let _ = shared;
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
