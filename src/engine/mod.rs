//! Engine thread: owns the decoder, DSP chain, playlist, persistence and the
//! cpal output stream. Communicates with the UI thread only through
//! `Command` (in), `Event` (out) and `Shared` (lock-free position/state
//! atomics polled by the UI timer).
//!
//! Scope: OpenPath, Play, Pause, TogglePlay, SeekRelative, SeekAbsolute,
//! SetSpeed (Phase 2: WSOLA stretcher through the flush path), Shutdown,
//! and the flush/pause/release protocol. Next/Prev are accepted but are
//! no-ops beyond what single-file playback needs until Phase 3 lands.

#[path = "../playlist.rs"]
pub mod playlist;
#[path = "../store.rs"]
pub mod store;
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
const PREV_RESTART_THRESHOLD_MS: u64 = 5000;
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

    // Loaded here (section 5.2: engine owns persistence). Wire-up of the
    // triggers lands in a later task; loading now links serde and lets the
    // exe-size gate be measured before anything builds on it.
    let mut store = store::load();
    // deliberately trivial use; keep binding live (block scopes the lint allow)
    #[allow(dead_code)]
    {
        store.speed = store.speed;
    }

    let mut state = PlayState::Idle;
    let mut ctx: Option<PlaybackContext> = None;
    let mut paused_since: Option<std::time::Instant> = None;
    // Set by the 10 s pause release (section 8.2): the only thing kept of
    // the context besides the resume position already in `shared.base_ms`.
    let mut released_path: Option<PathBuf> = None;
    let mut playlist: Vec<PathBuf> = Vec::new();
    let mut idx: usize = 0;

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
                    &mut playlist,
                    &mut idx,
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
                refill_until(c, &shared, HIGH_WATER_MS, &rx)
            } else {
                None
            };
            let drained = ctx.as_ref().map(|c| c.output.is_drained_eof(&shared)).unwrap_or(false);
            if drained {
                let next = idx + 1;
                if next < playlist.len() {
                    // Section 7.4.3: mark done + advance, stream stays
                    // open (done-marking lands with save_now in Task 4).
                    if !advance_to(
                        next,
                        &playlist,
                        &mut idx,
                        &mut state,
                        &mut ctx,
                        &mut released_path,
                        &shared,
                        &tx_events,
                        false,
                    ) {
                        let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                    }
                } else {
                    // Section 7.4.4: last file -> Ended.
                    state = PlayState::Ended;
                    let _ = tx_events.send(Event::StateChanged(state));
                }
            }
            if let Some(cmd) = pending {
                if !handle_command(
                    cmd,
                    &mut state,
                    &mut ctx,
                    &mut paused_since,
                    &mut released_path,
                    &mut playlist,
                    &mut idx,
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
    playlist: &mut Vec<PathBuf>,
    idx: &mut usize,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
) -> bool {
    match cmd {
        Command::Shutdown => return false,

        Command::OpenPath(path) => {
            open_path(path, state, ctx, paused_since, shared, tx_events, 0);
            // T3 probe wiring (controller ruling): populate the playlist
            // from the opened file's folder so Next/Prev/EOF advance has
            // something to navigate. T5 moves this inside open_path.
            if let Some(opened) = ctx.as_ref().map(|c| c.path.clone()) {
                if let Some(parent) = opened.parent() {
                    *playlist = playlist::scan(parent);
                    *idx = playlist::index_of(playlist, &opened).unwrap_or(0);
                }
            }
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

        Command::Next => {
            if *idx + 1 < playlist.len() {
                if !advance_to(*idx + 1, playlist, idx, state, ctx, released_path, shared, tx_events, true) {
                    let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                }
            } // else: last file -> no-op (section 7.4.4: Ended comes from EOF)
        }
        Command::Prev => {
            let threshold = PREV_RESTART_THRESHOLD_MS;
            if shared.position_ms() > threshold {
                // Restart current file (named constant, partner decision).
                let dur_target_zero = 0;
                match *state {
                    PlayState::Playing => flush_playing(ctx.as_mut().unwrap(), dur_target_zero, shared),
                    _ => {
                        shared.base_ms.store(0, Ordering::Relaxed);
                        shared.played_frames.store(0, Ordering::Relaxed);
                        if let Some(c) = ctx.as_mut() { let _ = c.decoder.seek(0); c.stretcher.reset(); c.resampler.reset(); }
                    }
                }
            } else if *idx > 0 {
                if !advance_to(*idx - 1, playlist, idx, state, ctx, released_path, shared, tx_events, true) {
                    let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                }
            } else if *state == PlayState::Playing {
                flush_playing(ctx.as_mut().unwrap(), 0, shared); // restart first file
            } else {
                shared.base_ms.store(0, Ordering::Relaxed);
                shared.played_frames.store(0, Ordering::Relaxed);
                if let Some(c) = ctx.as_mut() { let _ = c.decoder.seek(0); c.stretcher.reset(); c.resampler.reset(); }
            }
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
        match playlist::scan(&path).into_iter().next() {
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

/// Section 7 flush steps 1-4 + ring clear, shared by flush_playing and
/// advance_to's manual (ramp) path.
fn ramp_and_flush_ack(ctx: &mut PlaybackContext, shared: &Arc<Shared>) {
    shared.set_gain(0.0);
    std::thread::sleep(Duration::from_millis(20));
    shared.flush_req.store(true, Ordering::SeqCst);
    let start = std::time::Instant::now();
    while !shared.flush_ack.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_millis(200) {
            break; // device stall; proceed rather than hang
        }
        std::thread::park_timeout(Duration::from_millis(2));
    }
    ctx.output.clear_ring();
}

/// Section 7 flush steps 5 (stores)-7: position bookkeeping, refill to
/// PREROLL, release the flush gate, ramp back up. The caller has already
/// seeked or swapped the decoder and reset the DSP.
fn refill_and_release_flush(ctx: &mut PlaybackContext, shared: &Arc<Shared>, base_ms: u64) {
    shared.base_ms.store(base_ms, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
    shared.eof.store(false, Ordering::Relaxed);
    shared.drained.store(false, Ordering::Relaxed);
    prefill(ctx, shared, PREROLL_MS);
    shared.flush_req.store(false, Ordering::SeqCst);
    shared.flush_ack.store(false, Ordering::SeqCst);
    shared.set_gain(1.0);
}

fn flush_playing(ctx: &mut PlaybackContext, target_ms: u64, shared: &Arc<Shared>) {
    ramp_and_flush_ack(ctx, shared);
    let _ = ctx.decoder.seek(target_ms);
    ctx.stretcher.reset();
    ctx.resampler.reset();
    refill_and_release_flush(ctx, shared, target_ms);
}

/// Section 7.4.3 file handoff body: swap in a freshly opened decoder.
/// The resampler is rebuilt unconditionally — it was keyed to the
/// PREVIOUS file's sample rate at open_path, and a 44.1 -> 48 kHz advance
/// would otherwise reuse the wrong config (Phase 1 pitch bug in a new
/// disguise). Also stores the new duration so the UI can never show the
/// old track's length against the new track's position.
fn swap_in(ctx: &mut PlaybackContext, new_dec: Decoder, new_path: PathBuf, shared: &Arc<Shared>) {
    shared.duration_ms.store(new_dec.duration_ms().unwrap_or(0), Ordering::Relaxed);
    ctx.resampler = Resampler::new(new_dec.sample_rate(), ctx.output.device_rate());
    ctx.stretcher.reset(); // keeps speed (speed-keyed, not rate-keyed)
    ctx.decoder = new_dec;
    ctx.path = new_path;
    shared.base_ms.store(0, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
    shared.eof.store(false, Ordering::Relaxed);
    shared.drained.store(false, Ordering::Relaxed);
}

fn track_event(playlist: &[PathBuf], idx: usize, shared: &Arc<Shared>) -> Event {
    let p = &playlist[idx];
    let title = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let folder = p.parent().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    Event::TrackChanged {
        folder,
        title,
        index: idx as u32 + 1,
        count: playlist.len() as u32,
        duration_ms: shared.duration_ms.load(Ordering::Relaxed),
    }
}

/// Navigate to `playlist[target]`. Three shapes (spec section 2):
/// - Playing + ramp: section 7 flush protocol, stream stays open.
/// - Playing + !ramp (EOF handoff): ring already empty — swap and refill
///   directly, no ramp (section 7.4.3, gap under 100 ms).
/// - Not Playing: record selection via released_path; Play reopens.
/// Forward-skip on a failed Decoder::open (section 6.9); Error only when
/// no file from `target` onward is playable. Returns false iff Error.
fn advance_to(
    target: usize,
    playlist: &[PathBuf],
    idx: &mut usize,
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    released_path: &mut Option<PathBuf>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    ramp: bool,
) -> bool {
    if playlist.is_empty() || target >= playlist.len() {
        return true; // nothing to advance to (e.g. Next on last file: no-op)
    }
    // Try target, then forward (section 6.9: skip to the next file).
    let mut cand = target;
    loop {
        let path = playlist[cand].clone();
        match Decoder::open(&path) {
            Ok(new_dec) => {
                *idx = cand;
                match *state {
                    PlayState::Playing if ctx.is_some() => {
                        let c = ctx.as_mut().unwrap();
                        if ramp {
                            ramp_and_flush_ack(c, shared);
                            swap_in(c, new_dec, path, shared);
                            refill_and_release_flush(c, shared, 0);
                        } else {
                            // EOF handoff: ring is empty, no flush needed.
                            shared.base_ms.store(0, Ordering::Relaxed);
                            swap_in(c, new_dec, path, shared);
                            prefill(c, shared, PREROLL_MS);
                        }
                    }
                    _ => {
                        // Paused / Ended / released: no live stream to
                        // flush. Metadata-only probe for the new duration
                        // (Decoder::open already computed it from
                        // codec_params; no packets decoded). The probe
                        // decoder is dropped — resume() reopens via
                        // open_path.
                        shared.duration_ms.store(new_dec.duration_ms().unwrap_or(0), Ordering::Relaxed);
                        if let Some(mut old) = ctx.take() {
                            old.output.pause(); // stream was already stopped while paused
                            // drop old: ring, decoder, DSP go with it (section 8.2)
                        }
                        *released_path = Some(path);
                        shared.base_ms.store(0, Ordering::Relaxed);
                        shared.played_frames.store(0, Ordering::Relaxed);
                        *state = PlayState::Paused;
                        let _ = tx_events.send(Event::StateChanged(*state));
                    }
                }
                let _ = tx_events.send(track_event(playlist, *idx, shared));
                return true;
            }
            Err(_) => {
                let _ = tx_events.send(Event::Message(format!(
                    "Skipping unreadable file: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )));
                cand += 1;
                if cand >= playlist.len() {
                    *state = PlayState::Error;
                    let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                    let _ = tx_events.send(Event::Message("No playable files left".into()));
                    return false;
                }
            }
        }
    }
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
