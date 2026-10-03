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
        window.on_seek_to(move |fraction| {
            // Phase 1: duration may be 0 (unknown), in which case this is a
            // no-op seek to 0. Phase 4 disables slider dragging when
            // duration is unknown (section 10.6/7).
            let _ = fraction;
            let _ = cmd_tx.send(Command::SeekAbsolute(0));
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        let weak = window.as_weak();
        window.on_set_speed(move |index| {
            // Phase 2: the engine flushes to position with the new speed
            // (section 7.1), so this is safe to send at any time. Indices
            // match Speed::from_index (0=0.5x .. 4=2x).
            let _ = cmd_tx.send(Command::SetSpeed(crate::engine::Speed::from_index(
                index as u32,
            )));
            if let Some(window) = weak.upgrade() {
                window.set_speed_index(index);
            }
        });
    }

    // Spec 4.3: the timer's state lives in a thread_local so both the
    // visibility sink (event-loop thread) and the drain (via
    // invoke_from_event_loop) can drive it.
    with_tc(|slot| {
        *slot = Some(TimerControl::new(window.as_weak(), Arc::clone(&shared)));
    });
    crate::winit_hook::set_sink(|visible| {
        with_tc(|tc| {
            if let Some(t) = tc {
                t.set_visible(visible);
            }
        });
    });

    // Drain engine events and reflect the minimal ones Phase 0/1 cares
    // about (title/status) onto the window. Position/slider/time-text
    // updates are Phase 4.
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
