//! Slint glue. Per architecture.md section 13's dependency rule, this module
//! may import only `Command`, `Event`, and `Shared` from `engine` — never
//! `decode`, `dsp`, or `output` directly.
//!
//! Phase 0/1 scope: enough wiring to open a file/folder and toggle play so
//! the skeleton is visually operable, plus displaying events as they arrive.
//! The 500ms position timer, full transport (seek, next/prev, speed),
//! keyboard shortcuts, and minimize/occlusion timer suspension are all
//! Phase 4 scope (section 15) and are NOT implemented here yet — wiring
//! them now would be building ahead of the engine features (speed, playlist)
//! that Phase 2/3 have not landed.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::engine::{Command, Event, Shared};
use crate::MainWindow;
use slint::ComponentHandle;

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
    window.on_set_speed(move |_index| {
        // Phase 2 scope: signalsmith-stretch is not wired yet. Intentionally
        // not sending SetSpeed here so the UI does not claim a feature the
        // engine cannot yet perform correctly (section 3.6, 6.6).
    });

    let _ = shared; // Phase 4 wires the 500ms position timer against this.

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
                        window.set_is_playing(matches!(state, crate::engine::PlayState::Playing));
                    }
                    Event::Message(msg) => {
                        window.set_status_text(msg.into());
                    }
                }
            });
        }
    });
}
