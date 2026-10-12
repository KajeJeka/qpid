//! Slint glue. Per architecture.md section 13's dependency rule, this module
//! may import only `Command`, `Event`, and `Shared` from `engine` — never
//! `decode`, `dsp`, or `output` directly.
//!
//! Phase 0/1 scope: enough wiring to open a file/folder and toggle play so
//! the skeleton is visually operable, plus displaying events as they arrive.
//! Phase 2 added the speed row (the engine's SetSpeed is live). Phase 4
//! wires the 500 ms position timer (spec 4.3: runs only while playing &&
//! window visible, stopped by pause/minimize), the seek slider drag guard,
//! and the visibility sink from src/winit_hook.rs.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::engine::{Command, Event, Shared};
use crate::MainWindow;
use slint::ComponentHandle;

/// Section 10 rule 4: `M:SS` under an hour, `H:MM:SS` at or above.
fn format_time(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Spec 4.3: commit = fraction × duration, rounded. Duration 0 (unknown)
/// yields 0 and the slider is disabled anyway (section 7 rule 7); the
/// engine re-clamps (section 7 rule 3).
fn fraction_to_ms(fraction: f32, duration_ms: u64) -> u64 {
    if duration_ms == 0 {
        return 0;
    }
    (fraction.clamp(0.0, 1.0) * duration_ms as f32).round() as u64
}

/// Spec 4.4: `[`/`]` step the segment index, clamped to the five speeds.
fn clamp_step(current: i32, delta: i32) -> i32 {
    (current + delta).clamp(0, 4)
}

/// Design §5.2: folder row shows the last path component only; a path
/// with no file name (drive root) falls back to the full string.
fn folder_display(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Design §8.1: cycle Off → Line → Pulsar → Off. Out-of-range input is
/// clamped first, so 5 wraps to 0 like any other value above 2.
fn next_viz_mode(mode: u32) -> u32 {
    (mode.min(2) + 1) % 3
}

// -----------------------------------------------------------------------
// PULSAR visualizer (design §8.2/§8.3). Pure helpers below; the draw path
// and the peak-ring tap live with the timer (design §8.4).
// -----------------------------------------------------------------------

/// Bell window centered on a 48-column row: exp(-((x - w/2) / (w * 0.19))^2),
/// peak 1.0 at the center, ~0 at the edges (design §8.3).
fn bell_weight(x: f32, w: f32) -> f32 {
    let c = (x - w / 2.0) / (w * 0.19);
    (-c * c).exp()
}

/// Newest 48 blocks of the peak ring ending at `head`, each scaled by the
/// row's bell window. Values come out in [0, ~1]; the draw path multiplies
/// by 24 px (design §8.3 "scales to at most 24 px"). `row`/`rows` are kept
/// for call-site symmetry; row-dependent work (color, baseline) is done at
/// draw time from the history buffer.
fn pulsar_row(peaks: &[AtomicU32; 128], head: u32, _row: usize, _rows: usize) -> [f32; 48] {
    let w = 48.0f32;
    let mut out = [0.0f32; 48];
    for x in 0..48 {
        let idx = (head.wrapping_sub(48) + x as u32) % 128;
        let v = f32::from_bits(peaks[idx as usize].load(Ordering::Relaxed));
        out[x] = v * bell_weight(x as f32, w);
    }
    out
}

/// Newest 128 blocks of the peak ring as a polyline: point `i` sits at
/// `mid ± peak * half`, the sign alternating per point so the line reads as
/// a waveform (design §8.3 Line mode).
fn line_points(peaks: &[AtomicU32; 128], head: u32, mid: f32, half: f32) -> [(f32, f32); 128] {
    let mut out = [(0.0f32, 0.0f32); 128];
    for i in 0..128 {
        let idx = (head.wrapping_sub(128) + i as u32) % 128;
        let v = f32::from_bits(peaks[idx as usize].load(Ordering::Relaxed));
        let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
        out[i] = (i as f32, mid + sign * v * half);
    }
    out
}

/// Pulsar history: the new front row enters at index 0, older rows move
/// back one slot (design §8.3).
fn push_history(history: &mut [[f32; 48]; 15], front: [f32; 48]) {
    history.rotate_right(1);
    history[0] = front;
}

// Design tokens used by the renderer (§4): bg, divider, accent, pulsar
// back-row tint #1c5a10. RGBA8, opaque.
const VIZ_BG: [u8; 4] = [0x0b, 0x0d, 0x0a, 0xff];
const VIZ_DIVIDER: [u8; 4] = [0x23, 0x28, 0x1c, 0xff];
const VIZ_ACCENT: [u8; 4] = [0x3a, 0xb8, 0x14, 0xff];
const VIZ_PULSAR_BACK: [u8; 4] = [0x1c, 0x5a, 0x10, 0xff];

/// Design §8.3.1: strip width × 72 logical, in physical pixels, capped
/// 1280 × 240.
fn viz_buffer_size(window: &slint::Window) -> (u32, u32) {
    let scale = window.scale_factor();
    let logical = window.size().to_logical(scale);
    let w = (logical.width as f32 * scale).round() as u32;
    let h = (72.0 * scale).round() as u32;
    (w.clamp(2, 1280), h.clamp(2, 240))
}

#[inline]
fn blend_px(dst: &mut [u8], src: [u8; 4], cov: f32) {
    let a = cov * src[3] as f32 / 255.0;
    let inv = 1.0 - a;
    dst[0] = (src[0] as f32 * a + dst[0] as f32 * inv + 0.5) as u8;
    dst[1] = (src[1] as f32 * a + dst[1] as f32 * inv + 0.5) as u8;
    dst[2] = (src[2] as f32 * a + dst[2] as f32 * inv + 0.5) as u8;
    // buffer is fully opaque (design §8.3.1: bg fill); alpha channel untouched
}

/// Hand-written AA segment (design §8.3.3): coverage = clamp(half + 0.5 −
/// distance-to-segment, 0, 1), blended over the bg. Zero-length segments
/// stamp a disc, which covers the polyline's round joins.
fn draw_line_aa(
    buf: &mut slint::SharedPixelBuffer<slint::Rgba8Pixel>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    half: f32,
    color: [u8; 4],
) {
    let bw = buf.width() as i32;
    let bh = buf.height() as i32;
    let min_x = ((x0.min(x1) - half - 1.0).floor() as i32).max(0);
    let max_x = ((x0.max(x1) + half + 1.0).ceil() as i32).min(bw - 1);
    let min_y = ((y0.min(y1) - half - 1.0).floor() as i32).max(0);
    let max_y = ((y0.max(y1) + half + 1.0).ceil() as i32).min(bh - 1);
    if min_x > max_x || min_y > max_y {
        return;
    }
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;
    let data = buf.make_mut_bytes();
    let stride = bw as usize * 4;
    for py in min_y..=max_y {
        let row = py as usize * stride;
        for px in min_x..=max_x {
            let cx = px as f32 + 0.5;
            let cy = py as f32 + 0.5;
            let t = if len_sq > 0.0 {
                (((cx - x0) * dx + (cy - y0) * dy) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let qx = x0 + t * dx;
            let qy = y0 + t * dy;
            let dist = ((cx - qx).powi(2) + (cy - qy).powi(2)).sqrt();
            let cov = (half + 0.5 - dist).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend_px(&mut data[row + px as usize * 4..row + px as usize * 4 + 4], color, cov);
            }
        }
    }
}

fn lerp_color(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t + 0.5) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t + 0.5) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t + 0.5) as u8,
        0xff,
    ]
}

/// One full visualizer frame (design §8.3): clears to bg, then draws the
/// mode's content. Called only from the timer tick with new peak blocks
/// (design §8.4).
fn draw_viz(
    buf: &mut slint::SharedPixelBuffer<slint::Rgba8Pixel>,
    mode: u32,
    peaks: &[AtomicU32; 128],
    head: u32,
    history: &[[f32; 48]; 15],
) {
    {
        let data = buf.make_mut_bytes();
        for px in data.chunks_exact_mut(4) {
            px.copy_from_slice(&VIZ_BG);
        }
    }
    match mode {
        1 => draw_line_mode(buf, peaks, head),
        2 => draw_pulsar_mode(buf, history),
        _ => {}
    }
}

fn draw_line_mode(buf: &mut slint::SharedPixelBuffer<slint::Rgba8Pixel>, peaks: &[AtomicU32; 128], head: u32) {
    let h = buf.height() as f32;
    let w = buf.width() as f32;
    let mid = h / 2.0;
    let amp = h * (30.0 / 72.0); // 30 logical px scaled to the physical buffer
    // center line (divider, 1 px)
    draw_line_aa(buf, 0.0, mid, w - 1.0, mid, 0.5, VIZ_DIVIDER);
    // polyline over the last 128 blocks; the AA caps at each segment end
    // double as round joins
    let pts = line_points(peaks, head, mid, amp);
    let sx = (w - 1.0) / 127.0;
    for i in 0..127 {
        draw_line_aa(buf, pts[i].0 * sx, pts[i].1, pts[i + 1].0 * sx, pts[i + 1].1, 0.75, VIZ_ACCENT);
    }
}

fn draw_pulsar_mode(buf: &mut slint::SharedPixelBuffer<slint::Rgba8Pixel>, history: &[[f32; 48]; 15]) {
    let w = buf.width() as f32;
    let h = buf.height() as f32;
    let scale = h / 72.0;
    let spacing = ((72.0 - 28.0) / 14.0) * scale; // baselines (design §8.3)
    let amp = 24.0 * scale; // at most 24 logical px
    let front_baseline = h - 1.0;
    let sx = (w - 1.0) / 47.0;
    // back to front: row 14 (oldest) first, row 0 (front/newest) last
    for r in (0..15).rev() {
        let baseline = front_baseline - r as f32 * spacing;
        let color = lerp_color(VIZ_PULSAR_BACK, VIZ_ACCENT, (14 - r) as f32 / 14.0);
        let row = &history[r];
        // occlusion fill: from each column's curve down to the baseline plus
        // one spacing, so the next (front) row hides this one behind it
        {
            let bh = buf.height() as usize;
            let bw = buf.width() as usize;
            let data = buf.make_mut_bytes();
            for x in 0..48 {
                let px = (x as f32 * sx) as usize;
                let curve_y = (baseline - row[x] * amp).floor().max(0.0) as usize;
                let fill_end = ((baseline + spacing).ceil() as usize).min(bh - 1);
                for py in curve_y..=fill_end {
                    let idx = py * bw * 4 + px * 4;
                    data[idx..idx + 4].copy_from_slice(&VIZ_BG);
                }
            }
        }
        // stroke the row
        for x in 0..47 {
            let y0 = baseline - row[x] * amp;
            let y1 = baseline - row[x + 1] * amp;
            draw_line_aa(buf, x as f32 * sx, y0, (x + 1) as f32 * sx, y1, 0.65, color);
        }
    }
}

thread_local! {
    static TC: RefCell<Option<TimerControl>> = RefCell::new(None);
}

fn with_tc<R>(f: impl FnOnce(&mut Option<TimerControl>) -> R) -> R {
    TC.with(|c| f(&mut c.borrow_mut()))
}

/// Owns the single sanctioned slint::Timer (spec 4.3) and both of its
/// start/stop conditions. Lives in a thread_local so it outlives wire()
/// and stays on the event-loop thread (Timer is !Send).
struct TimerControl {
    timer: slint::Timer,
    weak: slint::Weak<MainWindow>,
    shared: Arc<Shared>,
    playing: bool,
    visible: bool,
    dragging: bool, // Task 3 sets it from the slider; tick skips seek-fraction
    running: bool,
    last_elapsed: String,
    last_duration: String,
    last_frac: f32,
    viz_mode: u32, // 0 off, 1 line, 2 pulsar (design §8.4: drives the interval)
    // Design §8.4.2: redraw only when new peak blocks arrived; on mode
    // change the history clears and the sentinel forces the next redraw.
    last_viz_head: u32,
    viz_history: [[f32; 48]; 15],
}

impl TimerControl {
    fn new(weak: slint::Weak<MainWindow>, shared: Arc<Shared>) -> Self {
        Self {
            timer: slint::Timer::default(),
            weak,
            shared,
            playing: false,
            visible: true,
            dragging: false,
            running: false,
            last_elapsed: String::new(),
            // Mirrors the .slint default duration-text ("0:00") so the first
            // dur==0 refresh sees a change and blanks it (section 7 rule 7).
            last_duration: "0:00".into(),
            last_frac: 0.0,
            viz_mode: 0,
            last_viz_head: u32::MAX,
            viz_history: [[0.0; 48]; 15],
        }
    }

    fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.recompute();
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        self.recompute();
    }

    /// Design §8.4.1: the one sanctioned timer is restarted at the new
    /// mode-dependent interval when the mode changes while running.
    fn set_viz_mode(&mut self, mode: u32) {
        if self.viz_mode == mode {
            return;
        }
        self.viz_mode = mode;
        // Design §8.4.2: on mode change the buffer clears — zero the
        // history and arm the sentinel so the next tick redraws fresh.
        self.viz_history = [[0.0; 48]; 15];
        self.last_viz_head = u32::MAX;
        if self.running {
            self.running = false;
            self.timer.stop();
        }
        self.recompute();
    }

    /// Spec 4.3: start iff playing && visible; stop otherwise. Starting a
    /// running timer (or stopping a stopped one) is a no-op via `running`.
    /// Design §8.2.4: also gates the audio-side peak tap — true only when
    /// the mode is not Off and the window is visible (a minimized window
    /// means zero tap work; a paused stream means the callback never runs).
    fn recompute(&mut self) {
        self.shared
            .viz_on
            .store(self.viz_mode != 0 && self.visible, Ordering::Relaxed);
        if self.playing && self.visible {
            if !self.running {
                self.running = true;
                // Design §3 / §8.4: 500 ms off, 66 ms line, 100 ms pulsar.
                let interval = match self.viz_mode {
                    1 => 66,
                    2 => 100,
                    _ => 500,
                };
                self.timer.start(slint::TimerMode::Repeated, Duration::from_millis(interval), || {
                    with_tc(|tc| {
                        if let Some(t) = tc {
                            t.tick();
                        }
                    });
                });
            }
        } else if self.running {
            self.running = false;
            self.timer.stop();
        }
    }

    fn tick(&mut self) {
        self.refresh();
        self.draw_viz_if_needed();
    }

    /// Design §8.4.2: draw only when the mode is on, the window is visible
    /// and we are playing (belt and braces — the timer already stops
    /// otherwise), and new peak blocks arrived since the last redraw. On
    /// pause the timer stops, so the strip freezes on the last frame with
    /// zero CPU.
    fn draw_viz_if_needed(&mut self) {
        if self.viz_mode == 0 || !self.visible || !self.playing {
            return;
        }
        let head = self.shared.viz_head.load(Ordering::Relaxed);
        if head == self.last_viz_head {
            return;
        }
        self.last_viz_head = head;
        let Some(window) = self.weak.upgrade() else { return };
        if self.viz_mode == 2 {
            push_history(&mut self.viz_history, pulsar_row(&self.shared.viz_peaks, head, 0, 15));
        }
        let (w, h) = viz_buffer_size(&window.window());
        let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
        draw_viz(
            &mut buf,
            self.viz_mode,
            &self.shared.viz_peaks,
            head,
            &self.viz_history,
        );
        window.set_viz_image(slint::Image::from_rgba8(buf));
    }

    /// One shared read, three display writes, each only when the displayed
    /// value changed (section 11 rule 5). seek-fraction is skipped while
    /// the user drags (spec 4.3 / section 10 rule 2).
    fn refresh(&mut self) {
        let pos = self.shared.position_ms();
        let dur = self.shared.duration_ms.load(Ordering::Relaxed);
        let elapsed = format_time(pos);
        let duration = if dur == 0 { String::new() } else { format_time(dur) };
        let frac = if dur == 0 {
            0.0
        } else {
            (pos as f64 / dur as f64).clamp(0.0, 1.0)
        } as f32;

        let Some(window) = self.weak.upgrade() else { return };

        if elapsed != self.last_elapsed {
            window.set_elapsed_text(elapsed.clone().into());
            self.last_elapsed = elapsed;
        }
        if duration != self.last_duration {
            window.set_duration_text(duration.clone().into());
            // Section 7 rule 7: duration 0 = slider disabled, text blank;
            // elapsed keeps showing (it is written by the branch above).
            window.set_seek_enabled(dur > 0);
            self.last_duration = duration;
        }
        if !self.dragging && frac != self.last_frac {
            window.set_seek_fraction(frac);
            self.last_frac = frac;
        }
    }
}

pub fn wire(window: &MainWindow, cmd_tx: Sender<Command>, evt_rx: Receiver<Event>, shared: Arc<Shared>) {
    // Design §7 footer strings, set once at startup. Manual sync point:
    // the Cargo.toml version feeds the label via env!("CARGO_PKG_VERSION").
    window.set_help_version(concat!("Q-pid ", env!("CARGO_PKG_VERSION")).into());
    window.set_help_media_note(if cfg!(windows) {
        "Media keys: play, next, previous"
    } else {
        ""
    }
    .into());

    // Open file / open folder: the two entry points required by Phase 0/1.
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_open_file(move || {
            if let Some(window) = weak.upgrade() {
                window.set_status_text("".into());
            }
            let tx = cmd_tx.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Audio", &["mp3", "m4a", "m4b", "aac", "flac", "ogg", "wav"])
                    .pick_file()
                {
                    let _ = tx.send(Command::OpenPath(path));
                }
            });
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_open_folder(move || {
            if let Some(window) = weak.upgrade() {
                window.set_status_text("".into());
            }
            let tx = cmd_tx.clone();
            std::thread::spawn(move || {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    let _ = tx.send(Command::OpenPath(path));
                }
            });
        });
    }
    // Phase 5 item 2: dropping a file or folder on the window goes through
    // the same Command::OpenPath as the dialogs (gate 4 intact: the event
    // type is only named inside winit_hook).
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        crate::winit_hook::set_drop_sink(move |path| {
            if let Some(window) = weak.upgrade() {
                window.set_status_text("".into());
            }
            let _ = cmd_tx.send(Command::OpenPath(path));
        });
    }

    // Transport: only toggle-play is meaningful before Phase 2/3 add speed
    // and a real playlist. Seek/next/prev/speed callbacks are left connected
    // to the engine's Phase 1 fallback behavior (see engine/mod.rs) so the
    // buttons do not crash if clicked, but they are not the final behavior.
    {
        let cmd_tx = cmd_tx.clone();
        window.on_toggle_play(move || {
            let _ = cmd_tx.send(Command::TogglePlay);
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        window.on_seek_back(move || {
            let _ = cmd_tx.send(Command::SeekRelative(-15_000));
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        window.on_seek_forward(move || {
            let _ = cmd_tx.send(Command::SeekRelative(15_000));
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        window.on_next_track(move || {
            let _ = cmd_tx.send(Command::Next);
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        window.on_prev_track(move || {
            let _ = cmd_tx.send(Command::Prev);
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        let shared_for_seek = Arc::clone(&shared);
        window.on_seek_to(move |fraction| {
            let dur = shared_for_seek.duration_ms.load(Ordering::Relaxed);
            if dur == 0 {
                return; // slider is disabled at duration 0 (belt and braces)
            }
            let ms = fraction_to_ms(fraction, dur);
            // Spec 4.3: commit on release — reflect it immediately so the
            // bar doesn't snap back to the pre-seek engine value while the
            // engine flushes; the next tick writes the engine truth.
            if let Some(window) = weak.upgrade() {
                window.set_seek_fraction(fraction);
            }
            with_tc(|tc| {
                if let Some(t) = tc {
                    t.last_frac = fraction;
                }
            });
            let _ = cmd_tx.send(Command::SeekAbsolute(ms));
        });
    }
    {
        window.on_seek_dragging(|changed| {
            // Spec 4.3 drag guard: while true, refresh() skips seek-fraction.
            with_tc(|tc| {
                if let Some(t) = tc {
                    t.dragging = changed;
                }
            });
        });
    }
    // Spec 4.4: the `speed-index` window property is the one source of
    // truth for the segment index; both the segment buttons and the [ ]
    // step binding go through it. (The former Rc<Cell<i32>> duplicate
    // could not be updated from the event-drain thread — Rc is !Send —
    // and Event::Settings needs the same sync; the property covers both.)
    fn apply_speed(cmd_tx: &Sender<Command>, weak: &slint::Weak<MainWindow>, index: i32) {
        let _ = cmd_tx.send(Command::SetSpeed(crate::engine::Speed::from_index(
            index as u32,
        )));
        if let Some(window) = weak.upgrade() {
            window.set_speed_index(index);
        }
    }
    // Shared sync sequence (send -> UI property). Clamping stays at each
    // call site: set clamps, step uses clamp_step.
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_set_speed(move |index| {
            apply_speed(&cmd_tx, &weak, index.clamp(0, 4));
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_step_speed(move |delta| {
            let Some(w) = weak.upgrade() else { return };
            let next = clamp_step(w.get_speed_index(), delta);
            apply_speed(&cmd_tx, &weak, next);
        });
    }

    // Design §8.1: V key, top-row wave button and mode-label click all
    // land here. Off <-> mode crossings resize the window by 80 logical px
    // (72 strip + 8 gap); Line <-> Pulsar does not resize. The size is
    // captured before the mode change so the min-height enforcement that
    // follows cannot double-count the strip. A refused resize (maximized,
    // tiled) is not retried: the `viz-mode != 0` strip binding hides it
    // until there is room.
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_cycle_viz(move || {
            let Some(window) = weak.upgrade() else { return };
            let old = window.get_viz_mode();
            let next = next_viz_mode(old.max(0) as u32);
            let captured = if (old == 0) != (next == 0) {
                let w = window.window();
                Some(w.size().to_logical(w.scale_factor()))
            } else {
                None
            };
            window.set_viz_mode(next as i32);
            with_tc(|tc| {
                if let Some(t) = tc {
                    t.set_viz_mode(next);
                }
            });
            if let Some(logical) = captured {
                let h = (logical.height + if next == 0 { -80.0 } else { 80.0 })
                    .max(if next == 0 { 320.0 } else { 400.0 });
                window.window().set_size(slint::LogicalSize::new(logical.width, h));
            }
            let _ = cmd_tx.send(Command::SetViz(next as u8));
        });
    }

    // Spec 4.3: the timer's state lives in a thread_local so both the
    // visibility sink (event-loop thread) and the drain (via
    // invoke_from_event_loop) can drive it.
    with_tc(|slot| {
        *slot = Some(TimerControl::new(window.as_weak(), Arc::clone(&shared)));
    });
    let weak_sink = window.as_weak();
    crate::winit_hook::set_sink(move |visible| {
        // Section 11.7 rule 7: EcoQoS follows visibility (minimize or
        // occlusion). Trim rides the same transition (Phase 5 item 1,
        // optional); QPID_NO_TRIM is the measurement escape hatch budget 4
        // requires. Runs inline on the event-loop thread: both calls are
        // cheap and §18 forbids timers not in the document.
        crate::platform::set_background_power_mode(!visible);
        if !visible && std::env::var_os("QPID_NO_TRIM").is_none() {
            crate::platform::trim_working_set();
        }
        with_tc(|tc| {
            if let Some(t) = tc {
                t.set_visible(visible);
            }
        });
        if visible {
            // Restore-from-minimize: the window surface is invalidated and
            // the restore-time draw can present before DWM finishes showing
            // the window, so its full repaint is lost and only later
            // partial updates (timer ticks) appear — leaving title/buttons
            // stale or blank. Bump the repaint nonce on the next
            // event-loop iteration to re-mark the whole window dirty.
            let weak = weak_sink.clone();
            slint::Timer::single_shot(Duration::from_millis(50), move || {
                if let Some(w) = weak.upgrade() {
                    w.set_repaint_nonce(w.get_repaint_nonce() + 1);
                }
            });
        }
    });

    // Drain engine events and reflect them onto the window: track/title/
    // status changes directly, plus one trailing refresh() so position,
    // slider and time text update on every event even while the timer is
    // stopped (spec 4.3).
    let weak = window.as_weak();
    std::thread::spawn(move || {
        while let Ok(event) = evt_rx.recv() {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(window) = weak.upgrade() else { return };
                match event {
                    Event::TrackChanged {
                        folder,
                        title,
                        index,
                        count,
                        duration_ms: _,
                    } => {
                        window.set_folder_name(folder_display(&folder).into());
                        window.set_track_title(title.into());
                        window.set_track_index(index as i32);
                        window.set_track_count(count as i32);
                        window.set_has_track(true);
                        window.set_status_text("".into());
                    }
                    Event::StateChanged(state) => {
                        let playing = matches!(state, crate::engine::PlayState::Playing);
                        window.set_is_playing(playing);
                        if playing {
                            window.set_status_text("".into());
                        }
                        with_tc(|tc| {
                            if let Some(t) = tc {
                                t.set_playing(playing);
                            }
                        });
                    }
                    Event::Message(msg) => {
                        window.set_status_text(msg.into());
                    }
                    Event::Settings { speed_index, viz } => {
                        // Design §8.5: engine -> UI only; never echo SetViz
                        // back (the store is already the source of truth).
                        window.set_speed_index(speed_index as i32);
                        window.set_viz_mode(viz as i32);
                        with_tc(|tc| {
                            if let Some(t) = tc {
                                t.set_viz_mode(viz as u32);
                            }
                        });
                    }
                }
                // Any event can change the displayed position/duration
                // (open, restore, seek, advance): refresh once even while
                // the timer is stopped, so a paused restore shows real
                // times immediately (spec 4.3).
                with_tc(|tc| {
                    if let Some(t) = tc {
                        t.refresh();
                    }
                });
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::format_time;
    use super::fraction_to_ms;
    use super::clamp_step;
    use super::folder_display;
    use super::next_viz_mode;
    use super::bell_weight;
    use super::pulsar_row;
    use super::line_points;
    use super::push_history;
    use super::draw_viz;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn bell_weight_peaks_at_center() {
        assert!((bell_weight(24.0, 48.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bell_weight_symmetric_and_decays() {
        assert!((bell_weight(12.0, 48.0) - bell_weight(36.0, 48.0)).abs() < 1e-6);
        assert!(bell_weight(0.0, 48.0) < 0.01);
    }

    #[test]
    fn pulsar_row_reads_newest_48() {
        let peaks = std::array::from_fn(|_| AtomicU32::new(0));
        // write distinct values into slots 0..128 with head=100
        for i in 0..128u32 {
            peaks[(i % 128) as usize].store((i as f32).to_bits(), Ordering::Relaxed);
        }
        let row = pulsar_row(&peaks, 100, 0, 15);
        // newest value (index 99) sits at x=47 before bell scaling (bell(47) tiny) —
        // assert row[24] corresponds to head-24 == 76 and equals 76 * bell_weight(24,48)
        assert!((row[24] - 76.0 * bell_weight(24.0, 48.0)).abs() < 1e-3);
    }

    #[test]
    fn line_points_alternate_sign_and_excursion() {
        let peaks = std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits()));
        let pts = line_points(&peaks, 128, 36.0, 30.0);
        // peak 1.0 everywhere: even i -> mid + half, odd i -> mid - half
        assert!((pts[0].1 - 66.0).abs() < 1e-4);
        assert!((pts[1].1 - 6.0).abs() < 1e-4);
        assert!((pts[2].1 - 66.0).abs() < 1e-4);
        let max_y = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        assert!((max_y - 66.0).abs() < 1e-4);
    }

    // Design §10 / T5 step 8: synthetic sine feed — the render-cost spike's
    // stand-in for live audio. Fills the ring, then drives the draw path
    // 300x (30 s at 10 fps). Completes without panicking; wall-clock cost is
    // measured outside the test.
    #[test]
    fn viz_synthetic_sine_feed_completes() {
        let peaks: [AtomicU32; 128] =
            std::array::from_fn(|i| AtomicU32::new((i as f32 * 0.2).sin().abs().to_bits()));
        let mut history = [[0.0f32; 48]; 15];
        let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(220, 72);
        for i in 0..300u32 {
            push_history(&mut history, pulsar_row(&peaks, 128 + i, 0, 15));
            draw_viz(&mut buf, 2, &peaks, 128 + i, &history);
            draw_viz(&mut buf, 1, &peaks, 128 + i, &history);
        }
    }

    #[test]
    fn clamp_step_steps_within_range() {
        assert_eq!(clamp_step(1, 1), 2);
        assert_eq!(clamp_step(3, -1), 2);
    }

    #[test]
    fn clamp_step_clamps_at_zero() {
        assert_eq!(clamp_step(0, -1), 0);
        assert_eq!(clamp_step(1, -4), 0);
    }

    #[test]
    fn clamp_step_clamps_at_four() {
        assert_eq!(clamp_step(4, 1), 4);
        assert_eq!(clamp_step(3, 4), 4);
    }

    #[test]
    fn clamp_step_identity_on_zero_delta() {
        assert_eq!(clamp_step(2, 0), 2);
    }

    #[test]
    fn fraction_zero_and_full() {
        assert_eq!(fraction_to_ms(0.0, 1000), 0);
        assert_eq!(fraction_to_ms(1.0, 999), 999);
    }

    #[test]
    fn fraction_rounds_to_nearest_ms() {
        assert_eq!(fraction_to_ms(0.5, 1000), 500);
        assert_eq!(fraction_to_ms(0.333_333_34, 1000), 333);
        assert_eq!(fraction_to_ms(0.666_5, 1000), 667);
    }

    #[test]
    fn fraction_clamps_out_of_range() {
        assert_eq!(fraction_to_ms(2.0, 1000), 1000);
        assert_eq!(fraction_to_ms(-1.0, 1000), 0);
    }

    #[test]
    fn fraction_zero_duration_is_zero() {
        assert_eq!(fraction_to_ms(0.5, 0), 0);
    }

    #[test]
    fn format_time_zero() {
        assert_eq!(format_time(0), "0:00");
    }

    #[test]
    fn format_time_under_a_minute() {
        assert_eq!(format_time(59_000), "0:59");
    }

    #[test]
    fn format_time_minute_boundary() {
        assert_eq!(format_time(60_000), "1:00");
        assert_eq!(format_time(75_000), "1:15");
    }

    #[test]
    fn format_time_just_under_an_hour() {
        assert_eq!(format_time(3_599_000), "59:59");
    }

    #[test]
    fn format_time_hour_boundary() {
        assert_eq!(format_time(3_600_000), "1:00:00");
    }

    #[test]
    fn format_time_over_an_hour() {
        assert_eq!(format_time(3_661_000), "1:01:01");
    }

    #[test]
    fn format_time_long_track() {
        assert_eq!(format_time(36_000_000), "10:00:00");
    }

    #[test]
    fn folder_display_last_component() {
        assert_eq!(folder_display(r"C:\Music\Book One"), "Book One");
        assert_eq!(folder_display("/home/u/albums"), "albums");
    }

    #[test]
    fn folder_display_root_falls_back_to_full() {
        assert_eq!(folder_display(r"C:\"), r"C:\");
        assert_eq!(folder_display("/"), "/");
    }

    #[test]
    fn next_viz_mode_cycles_three_states() {
        assert_eq!(next_viz_mode(0), 1);
        assert_eq!(next_viz_mode(1), 2);
        assert_eq!(next_viz_mode(2), 0);
    }

    #[test]
    fn next_viz_mode_clamps_out_of_range() {
        assert_eq!(next_viz_mode(5), 0); // clamp-then-wrap: 5 clamps to 2, (2 + 1) % 3 == 0
    }
}
