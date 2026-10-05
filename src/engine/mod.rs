//! Engine thread: owns the decoder, DSP chain, playlist, persistence and the
//! cpal output stream. Communicates with the UI thread only through
//! `Command` (in), `Event` (out) and `Shared` (lock-free position/state
//! atomics polled by the UI timer).
//!
//! Scope: OpenPath, Play, Pause, TogglePlay, SeekRelative, SeekAbsolute,
//! SetSpeed (Phase 2: WSOLA stretcher through the flush path), Next/Prev
//! (real navigation: natural-sorted playlist, advance_to handoff),
//! RestoreSession (launch without args: last folder/file/position, paused,
//! no autoplay), Shutdown, and the flush/pause/release protocol. Playback
//! state is persisted through save_now (section 9 rule 5).

#[path = "../playlist.rs"]
pub mod playlist;
#[path = "../store.rs"]
pub mod store;
pub mod decode;
pub mod dsp;
pub mod output;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use self::decode::Decoder;
use self::dsp::{Resampler, Stretcher};
use self::output::OutputStream;
use self::store::Store;

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
    RestoreSession,
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
    // Budget 9 instrumentation (spec 5): engine-loop wake count + engine
    // start instant. `w` on the CLI reads them; no OS-level tooling.
    pub wakes: AtomicU64,
    // Phase 5 soak (section 11.7 rule 7): callback silence-pads observed
    // while audio should be flowing. Excluded: flush path, EOF drain,
    // pre-first-frame startup (guarded in the callback).
    pub underruns: AtomicU64,
    pub started: std::time::Instant,
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
            wakes: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            started: std::time::Instant::now(),
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
    crate::winapi::lower_thread_priority();

    // Loaded here (section 5.2: engine owns persistence). Every write goes
    // through save_now (section 9 rule 5), which is the only place that
    // calls store.save().
    let mut store = store::load();

    let mut state = PlayState::Idle;
    let mut ctx: Option<PlaybackContext> = None;
    let mut paused_since: Option<std::time::Instant> = None;
    // Set by the 10 s pause release (section 8.2): the only thing kept of
    // the context besides the resume position already in `shared.base_ms`.
    let mut released_path: Option<PathBuf> = None;
    let mut playlist: Vec<PathBuf> = Vec::new();
    let mut idx: usize = 0;

    // Section 9 rule 5: checkpoint every 30 s while playing, and only if
    // the position changed. Rides the existing recv_timeout wake — no new
    // thread or timer (section 18).
    const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);
    let mut next_checkpoint = std::time::Instant::now() + CHECKPOINT_INTERVAL;
    let mut last_saved_pos: u64 = 0;

    // Budget 9 primary (Ruling 2b): debug-only wake log. Read the env var
    // once, before the loop; release builds have neither local. Snapshots
    // ride the existing wakes (>= 5 s apart) — no new thread or timer.
    #[cfg(debug_assertions)]
    let wake_log: Option<PathBuf> = std::env::var_os("QPID_WAKE_LOG").map(PathBuf::from);
    #[cfg(debug_assertions)]
    let mut last_write: Option<std::time::Instant> = None;

    loop {
        shared.wakes.fetch_add(1, Ordering::Relaxed);
        #[cfg(debug_assertions)]
        if let Some(path) = &wake_log {
            if last_write.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let n = shared.wakes.load(Ordering::Relaxed);
                    let t = shared.started.elapsed().as_secs_f64();
                    let u = shared.underruns.load(Ordering::Relaxed);
                    let _ = writeln!(f, "wakes={n} uptime_s={t:.1} underruns={u}");
                    last_write = Some(std::time::Instant::now());
                }
            }
        }
        let wait = match state {
            PlayState::Playing => {
                let fill_ms = ctx
                    .as_ref()
                    .map(|c| c.output.fill_ms())
                    .unwrap_or(HIGH_WATER_MS);
                let wait = if fill_ms <= LOW_WATER_MS {
                    // Architecture §6's clamp floor (100 ms .. 2000 ms) — a
                    // 0 here spins: at EOF the decoder is exhausted, refill
                    // raises nothing and fill stays <= LOW_WATER until the
                    // callback empties the ring, so recv_timeout(0) would
                    // return every iteration (measured: ~80k wakes/s drain-
                    // poll spin per track transition). The next refill still
                    // runs immediately after this wait.
                    Duration::from_millis(100)
                } else {
                    let until_low = fill_ms - LOW_WATER_MS;
                    Duration::from_millis(until_low.clamp(100, 2000))
                };
                // Shrink the fill-based wait so the checkpoint can fire on
                // time even when the ring stays full for minutes.
                let until_ckpt = next_checkpoint.saturating_duration_since(std::time::Instant::now());
                wait.min(until_ckpt)
            }
            // Released (ctx taken): no timer — only a command wakes us,
            // which is what budget 7 ("no timers") asks for after 10 s.
            PlayState::Paused if ctx.is_some() => PAUSE_RELEASE,
            PlayState::Ended if ctx.is_some() => PAUSE_RELEASE,
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
                    &mut store,
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
            Err(RecvTimeoutError::Disconnected) => {
                // Sender dropped without a Shutdown (UI window closed the
                // channel): still an app exit — persist before the thread
                // dies.
                save_now(
                    &mut store,
                    &shared,
                    ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref()),
                    "app exit",
                );
                break;
            }
        }

        // Section 9 rule 5: checkpoint every 30 s while playing, and only
        // if the position changed. Folded into the existing wake — no new
        // thread or timer (section 18).
        if state == PlayState::Playing && std::time::Instant::now() >= next_checkpoint {
            let pos = shared.position_ms();
            if pos != last_saved_pos {
                let current = ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref());
                save_now(&mut store, &shared, current, "30s checkpoint");
                last_saved_pos = pos;
            }
            next_checkpoint = std::time::Instant::now() + CHECKPOINT_INTERVAL;
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
                    // open. save_now inside advance_to writes the rule 4
                    // `done` flag for the finished file.
                    advance_to(
                        next,
                        &playlist,
                        &mut idx,
                        &mut store,
                        &mut state,
                        &mut ctx,
                        &mut released_path,
                        &shared,
                        &tx_events,
                        false,
                    );
                } else {
                    // Section 7.4.4: last file -> Ended. Start the same
                    // 10 s release clock pause uses (spec 4.6, budget 7):
                    // on_timeout releases the stream/decoder when it
                    // elapses with no intervening command.
                    state = PlayState::Ended;
                    paused_since = Some(std::time::Instant::now());
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
                    &mut store,
                    &shared,
                    &tx_events,
                ) {
                    break; // Shutdown
                }
            }
        }
    }
}

/// Section 9 rule 5's single write path. `current` is the file being
/// left/updated (caller derives it from ctx.path or released_path);
/// None writes top-level fields only (speed) plus folder-less state.
fn save_now(store: &mut Store, shared: &Arc<Shared>, current: Option<&Path>, reason: &'static str) {
    store.speed = store::milli_to_speed(shared.speed_milli.load(Ordering::Relaxed));
    if let Some(p) = current {
        if let (Some(folder), Some(name)) = (p.parent(), p.file_name()) {
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            let pos = shared.position_ms();
            let dur = shared.duration_ms.load(Ordering::Relaxed);
            // Rule 4: done at EOS or within the last 3 s. Duration 0
            // (unknown) can only be done at EOS (the saturating compare
            // would otherwise be trivially true).
            let done = if dur == 0 {
                shared.eof.load(Ordering::Relaxed)
            } else {
                pos >= dur.saturating_sub(3000)
            };
            store.record(&folder.to_string_lossy(), &name.to_string_lossy(), size, pos, done);
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("[store] save ({reason})");
    }
    store.save();
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
    store: &mut Store,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
) -> bool {
    match cmd {
        Command::Shutdown => {
            // Section 9 rule 5: app exit persists before the thread dies.
            save_now(
                store,
                shared,
                ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref()),
                "app exit",
            );
            return false;
        }

        Command::OpenPath(path) => {
            // Trigger 4: save the file being left, before open_path resets
            // the position/duration bookkeeping.
            let old = ctx.as_ref().map(|c| c.path.clone()).or_else(|| released_path.clone());
            if let Some(old) = old {
                if old != path {
                    save_now(store, shared, Some(&old), "open of another path");
                    // ^^^ position of the old file is captured BEFORE the
                    // open resets bookkeeping; save_now reads shared state,
                    // so the recorded position is correct as long as it is
                    // read while base/played still describe the old file.
                }
            }
            open_path(path, state, ctx, paused_since, shared, tx_events, 0, store, released_path, playlist, idx);
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
                    store,
                    playlist,
                    idx,
                );
            } else if *state == PlayState::Ended {
                restart_from_ended(
                    state, ctx, paused_since, released_path, shared, tx_events,
                    store, playlist, idx,
                );
            }
        }

        Command::Pause => {
            if *state == PlayState::Playing {
                pause(ctx, state, paused_since, shared, tx_events, store);
            }
        }

        Command::TogglePlay => {
            match *state {
                PlayState::Playing => pause(ctx, state, paused_since, shared, tx_events, store),
                PlayState::Paused => {
                    resume(ctx, state, paused_since, released_path, shared, tx_events, store, playlist, idx)
                }
                // Spec 4.6: after Ended (released or not) Play restarts
                // from 0 — without this arm the UI's Space/space button
                // was a no-op once Ended (shipped gap).
                PlayState::Ended => restart_from_ended(
                    state, ctx, paused_since, released_path, shared, tx_events,
                    store, playlist, idx,
                ),
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
                advance_to(*idx + 1, playlist, idx, store, state, ctx, released_path, shared, tx_events, true);
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
                // Trigger 3: restart recorded after the fact so pos 0 is
                // what lands (rule 3 drops the entry — the user restarted).
                let current = ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref());
                save_now(store, shared, current, "prev restart");
            } else if *idx > 0 {
                advance_to(*idx - 1, playlist, idx, store, state, ctx, released_path, shared, tx_events, true);
            } else if *state == PlayState::Playing {
                flush_playing(ctx.as_mut().unwrap(), 0, shared); // restart first file
            } else {
                shared.base_ms.store(0, Ordering::Relaxed);
                shared.played_frames.store(0, Ordering::Relaxed);
                if let Some(c) = ctx.as_mut() { let _ = c.decoder.seek(0); c.stretcher.reset(); c.resampler.reset(); }
            }
        }

        Command::RestoreSession => {
            restore_session(store, state, ctx, released_path, playlist, idx, shared, tx_events, paused_since);
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
            // Trigger 5: placed AFTER the rewrite — position_ms() is still
            // cur (base holds it, played_frames is 0, so speed no longer
            // enters) while speed_milli already carries the NEW speed, so
            // state.json gets both the position and the new speed.
            save_now(
                store,
                shared,
                ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref()),
                "speed change",
            );
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
    if cfg!(debug_assertions) {
        eprintln!("[seek] -> {target_ms} (state {state:?})");
    }
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

/// Section 9 rule 2 pick: last_file if present and not done; else the
/// first not-done file (a missing `files` entry counts as not done); else 0.
fn rule2_pick(playlist: &[PathBuf], f: &store::Folder) -> usize {
    let last = &f.last_file;
    playlist.iter().position(|p| {
        p.file_name().map(|n| store::key(&n.to_string_lossy()) == *last).unwrap_or(false)
    }).filter(|_| !f.files.get(last).map(|e| e.done).unwrap_or(false))
        .or_else(|| playlist.iter().position(|p| {
            f.files.get(&store::key(&p.file_name().unwrap_or_default().to_string_lossy()))
                .map(|e| !e.done).unwrap_or(true)
        })).unwrap_or(0)
}

fn open_path(
    path: PathBuf,
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    paused_since: &mut Option<std::time::Instant>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    resume_ms: u64, // 0 on a fresh open; a resume-after-release position
    store: &mut Store,
    released_path: &mut Option<PathBuf>,
    playlist: &mut Vec<PathBuf>,
    idx: &mut usize,
) {
    *paused_since = None;

    let was_dir = path.is_dir();
    let mut file_path = if was_dir {
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

    // Section 9 restore rules 2/3: the playlist is the folder's sorted
    // audio files; selection and start position come from the store.
    let folder = file_path.parent().map(Path::to_path_buf).unwrap_or_default();
    *playlist = playlist::scan(&folder);
    if playlist.is_empty() {
        // Opened file is outside the scan (hidden/system attribute or an
        // extension the filter skips): keep a one-file playlist so
        // track_event and advance_to never index an empty vec.
        *playlist = vec![file_path.clone()];
    }
    *idx = playlist::index_of(playlist, &file_path).unwrap_or(0);
    let folder_name = folder.to_string_lossy().to_string();
    let fk = store::key(&folder_name);
    if was_dir && resume_ms == 0 {
        // Rule 2 pick: last_file if present and not done; else first
        // not-done; else first. Single-file opens (rule 3) and
        // resume-after-release (resume_ms > 0) keep the file as opened.
        let pick = store.folders.get(&fk).map(|f| rule2_pick(playlist, f)).unwrap_or(0);
        *idx = pick;
        file_path = playlist[*idx].clone();
    }
    let file_name = file_path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let size_on_disk = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
    let saved_pos = store.position_for(&folder_name, &file_name, size_on_disk)
        .saturating_sub(store::RESUME_REWIND_MS);
    let start_pos = if resume_ms > 0 { resume_ms } else { saved_pos };

    match Decoder::open(&file_path) {
        Ok(decoder) => {
            let source_rate = decoder.sample_rate();
            shared
                .duration_ms
                .store(decoder.duration_ms().unwrap_or(0), Ordering::Relaxed);
            shared.base_ms.store(start_pos, Ordering::Relaxed);
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
                    *ctx = Some(PlaybackContext {
                        decoder,
                        output,
                        stretcher,
                        resampler,
                        path: file_path,
                    });

                    if let Some(c) = ctx.as_mut() {
                        if start_pos > 0 {
                            let _ = c.decoder.seek(start_pos);
                        }
                        prefill(c, shared, PREROLL_MS);
                        c.output.play();
                    }

                    *state = PlayState::Playing;
                    let _ = tx_events.send(track_event(playlist, *idx, shared));
                    let _ = tx_events.send(Event::StateChanged(*state));
                    // Success supersedes any earlier release: a stale
                    // released_path would make the next trigger 4 / error
                    // save attribute the position to the wrong file.
                    *released_path = None;
                }
                Err(e) => {
                    // Deliberately NO save here: open_path already rewrote
                    // shared (duration/base/played describe this failed
                    // attempt) while ctx still names the previous file, so
                    // save_now would record the old file at ~0 and clobber
                    // trigger 4's correct pre-open save.
                    *state = PlayState::Error;
                    let _ = tx_events.send(Event::Message(format!("Output device error: {e}")));
                    let _ = tx_events.send(Event::StateChanged(*state));
                }
            }
        }
        Err(e) => {
            // Trigger 7 (error stop), same current derivation as above.
            let current = ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref());
            *state = PlayState::Error;
            save_now(store, shared, current, "error stop");
            let _ = tx_events.send(Event::Message(format!("Could not open file: {e}")));
            let _ = tx_events.send(Event::StateChanged(*state));
        }
    }
}

/// Launch without arguments: last folder, last file, saved position,
/// stay Paused, do not autoplay (section 9 rule 1; acceptance test 6).
fn restore_session(
    store: &Store,
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    released_path: &mut Option<PathBuf>,
    playlist: &mut Vec<PathBuf>,
    idx: &mut usize,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    paused_since: &mut Option<std::time::Instant>,
) {
    if store.last_folder.is_empty() {
        return; // no history: stay Idle with defaults
    }
    let folder = std::path::PathBuf::from(&store.last_folder);
    *playlist = playlist::scan(&folder);
    if playlist.is_empty() {
        return;
    }
    // Find the stored last_file (lowercase key) among the real names.
    let wanted = store
        .folders
        .get(&store::key(&store.last_folder))
        .map(|f| f.last_file.clone())
        .unwrap_or_default();
    let found = wanted
        .is_empty()
        .then_some(0)
        .unwrap_or_else(|| {
            playlist
                .iter()
                .position(|p| {
                    p.file_name()
                        .map(|n| store::key(&n.to_string_lossy()) == wanted)
                        .unwrap_or(false)
                })
                .unwrap_or(0)
        });
    *idx = found;
    let path = playlist[*idx].clone();
    // Size check (rule 2): mismatch -> position 0.
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut pos = store.position_for(&store.last_folder, &path.file_name().unwrap_or_default().to_string_lossy(), size);
    pos = pos.saturating_sub(store::RESUME_REWIND_MS); // rule 8
    // Metadata-only duration probe: Decoder::open reads codec_params
    // (duration comes free at open; no packets decoded) so the UI gets a
    // real duration without touching audio (budget 2).
    if let Ok(probe) = Decoder::open(&path) {
        shared.duration_ms.store(probe.duration_ms().unwrap_or(0), Ordering::Relaxed);
    }
    *paused_since = Some(std::time::Instant::now()); // treat as paused now
    shared.base_ms.store(pos, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
    shared.speed_milli.store(store::speed_to_milli(store.speed), Ordering::Relaxed);
    *released_path = Some(path); // Play -> resume() reopens at base_ms
    *ctx = None;
    *state = PlayState::Paused;
    let _ = tx_events.send(track_event(playlist, *idx, shared));
    let _ = tx_events.send(Event::StateChanged(*state));
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
                // File-level skip (section 6.9) lives in advance_to: an
                // unreadable file at open forwards to the next candidate and
                // Error is only reported when none is playable. Per-packet
                // skipping remains deferred — a decode error just stops this
                // burst and the next refill retries (Phase 1 behavior).
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
    store: &mut Store,
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
    // Triggers 2 and 3 (next/prev), and the EOF handoff when ramp == false:
    // the file being left is recorded while base/played still describe it.
    save_now(
        store,
        shared,
        ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref()),
        "next/prev",
    );
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
                    // Trigger 7 (error stop), run-level: no file playable.
                    // The three former caller-side StateChanged(Error)
                    // sends were dropped (double-emission fix), so this arm
                    // is the single StateChanged(Error) source — open_path's
                    // file-open failure is the other error-stop save.
                    save_now(
                        store,
                        shared,
                        ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref()),
                        "error stop",
                    );
                    let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                    let _ = tx_events.send(Event::Message("No playable files left".into()));
                    return false;
                }
            }
        }
    }
}

/// Section 7: Play after Ended restarts the current file from position 0.
/// ctx Some: flush in place (Phase 3 behavior). ctx None: released after
/// the 10 s window — reopen the saved path (open_path auto-plays; its
/// start position comes from the store, so flush to 0 right after), then
/// flush to 0. No save: the trigger table has no play/restart trigger.
fn restart_from_ended(
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    paused_since: &mut Option<std::time::Instant>,
    released_path: &mut Option<PathBuf>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    store: &mut Store,
    playlist: &mut Vec<PathBuf>,
    idx: &mut usize,
) {
    *paused_since = None;
    if let Some(c) = ctx.as_mut() {
        flush_playing(c, 0, shared);
        *state = PlayState::Playing;
        let _ = tx_events.send(Event::StateChanged(*state));
    } else if let Some(path) = released_path.take() {
        open_path(
            path, state, ctx, paused_since, shared, tx_events, 0, store,
            released_path, playlist, idx,
        );
        if let Some(c) = ctx.as_mut() {
            flush_playing(c, 0, shared);
        }
    }
}

fn pause(
    ctx: &mut Option<PlaybackContext>,
    state: &mut PlayState,
    paused_since: &mut Option<std::time::Instant>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    store: &mut Store,
) {
    // Trigger 1 (pause): fires for Command::Pause and TogglePlay->pause.
    // ctx is None after a 10 s release — current is None then, so nothing
    // is recorded but the top-level speed still lands. Placed before the
    // gain-down so the recorded position is pre-ramp.
    save_now(store, shared, ctx.as_ref().map(|c| c.path.as_path()), "pause");
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
    store: &mut Store,
    playlist: &mut Vec<PathBuf>,
    idx: &mut usize,
) {
    *paused_since = None;

    // Released after the 10 s pause window (section 8.2): reopen the file
    // at the stored position — open_path rebuilds decoder, stream, ring and
    // resampler from scratch.
    if ctx.is_none() {
        if let Some(path) = released_path.take() {
            let resume_ms = shared.base_ms.load(Ordering::Relaxed);
            open_path(path, state, ctx, paused_since, shared, tx_events, resume_ms, store, released_path, playlist, idx);
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
    // Paused and Ended share one release mechanism (spec 4.6): after
    // PAUSE_RELEASE with the context still open, keep only the path.
    if matches!(*state, PlayState::Paused | PlayState::Ended) && ctx.is_some() {
        if let Some(since) = *paused_since {
            if since.elapsed() >= PAUSE_RELEASE {
                *released_path = ctx.take().map(|c| c.path);
                *paused_since = None;
                if cfg!(debug_assertions) {
                    eprintln!("[release] {state:?} stream+decoder released");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn rule2_pick_treats_missing_entries_as_not_done() {
        let pl = vec![
            PathBuf::from("C:\\Music\\a.mp3"),
            PathBuf::from("C:\\Music\\b.mp3"),
            PathBuf::from("C:\\Music\\c.mp3"),
        ];
        let entry = |done: bool| store::FileEntry { pos_ms: 0, size: 0, done };

        // last_file done: fall through to the first not-done file (b).
        let mut files = HashMap::new();
        files.insert(store::key("a.mp3"), entry(true));
        files.insert(store::key("b.mp3"), entry(false));
        files.insert(store::key("c.mp3"), entry(true));
        let f = store::Folder { touched: 0, last_file: store::key("a.mp3"), files };
        assert_eq!(rule2_pick(&pl, &f), 1);

        // last_file with no entry at all: treated as not done, keep it.
        let f2 = store::Folder {
            touched: 0,
            last_file: store::key("b.mp3"),
            files: HashMap::new(),
        };
        assert_eq!(rule2_pick(&pl, &f2), 1);

        // last_file done, later files missing entries: pick the later file,
        // never the done file at index 0.
        let mut files3 = HashMap::new();
        files3.insert(store::key("a.mp3"), entry(true));
        let f3 = store::Folder { touched: 0, last_file: store::key("a.mp3"), files: files3 };
        assert_eq!(rule2_pick(&pl, &f3), 1);
    }
}
