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

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
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

    /// Spec 4.3: start iff playing && visible; stop otherwise. Starting a
    /// running timer (or stopping a stopped one) is a no-op via `running`.
    fn recompute(&mut self) {
        if self.playing && self.visible {
            if !self.running {
                self.running = true;
                self.timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), || {
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
        #[cfg(debug_assertions)]
        eprintln!("[tick]");
        self.refresh();
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
    // Open file / open folder: the two entry points required by Phase 0/1.
    {
        let cmd_tx = cmd_tx.clone();
        window.on_open_file(move || {
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
        window.on_open_folder(move || {
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
        crate::winit_hook::set_drop_sink(move |path| {
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
    // Spec 4.4: one source of truth for the segment index; both the
    // segment buttons and the [ ] step binding go through it.
    let speed_index = Rc::new(Cell::new(1i32)); // window default is 1 (1x)
    // Shared sync sequence (send -> step source -> UI property). Clamping
    // stays at each call site: set clamps, step uses clamp_step.
    fn apply_speed(
        cmd_tx: &Sender<Command>,
        weak: &slint::Weak<MainWindow>,
        speed_index: &Cell<i32>,
        index: i32,
    ) {
        let _ = cmd_tx.send(Command::SetSpeed(crate::engine::Speed::from_index(
            index as u32,
        )));
        speed_index.set(index);
        if let Some(window) = weak.upgrade() {
            window.set_speed_index(index);
        }
    }
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        let speed_index = Rc::clone(&speed_index);
        window.on_set_speed(move |index| {
            apply_speed(&cmd_tx, &weak, &speed_index, index.clamp(0, 4));
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        let speed_index = Rc::clone(&speed_index);
        window.on_step_speed(move |delta| {
            apply_speed(
                &cmd_tx,
                &weak,
                &speed_index,
                clamp_step(speed_index.get(), delta),
            );
        });
    }

    // Spec 4.3: the timer's state lives in a thread_local so both the
    // visibility sink (event-loop thread) and the drain (via
    // invoke_from_event_loop) can drive it.
    with_tc(|slot| {
        *slot = Some(TimerControl::new(window.as_weak(), Arc::clone(&shared)));
    });
    crate::winit_hook::set_sink(|visible| {
        // Section 11.7 rule 7: EcoQoS follows visibility (minimize or
        // occlusion). Trim rides the same transition (Phase 5 item 1,
        // optional); QPID_NO_TRIM is the measurement escape hatch budget 4
        // requires. Runs inline on the event-loop thread: both calls are
        // cheap and §18 forbids timers not in the document.
        crate::winapi::set_ecoqos(!visible);
        if !visible && std::env::var_os("QPID_NO_TRIM").is_none() {
            crate::winapi::trim_working_set();
        }
        with_tc(|tc| {
            if let Some(t) = tc {
                t.set_visible(visible);
            }
        });
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
                        window.set_folder_name(folder.into());
                        window.set_track_title(title.into());
                        window.set_track_index(index as i32);
                        window.set_track_count(count as i32);
                        window.set_has_track(true);
                    }
                    Event::StateChanged(state) => {
                        let playing = matches!(state, crate::engine::PlayState::Playing);
                        window.set_is_playing(playing);
                        with_tc(|tc| {
                            if let Some(t) = tc {
                                t.set_playing(playing);
                            }
                        });
                    }
                    Event::Message(msg) => {
                        window.set_status_text(msg.into());
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
}
