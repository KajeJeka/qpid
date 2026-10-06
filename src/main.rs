#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod ui;
mod winit_hook;
mod platform;

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;

use engine::{Command, Event, Shared, Speed};

slint::include_modules!();

fn main() {
    // Select the software renderer / winit backend explicitly before any
    // Slint call, per architecture.md section 14.
    std::env::set_var("SLINT_BACKEND", "winit-software");

    let args: Vec<String> = std::env::args().skip(1).collect();

    // Phase 1 temporary CLI: `qpid.exe --cli <file>` plays headlessly with
    // stdin controls. This exists only to validate the engine before the UI
    // is wired in Phase 4; it is not the shipping interface (section 15,
    // Phase 1 step 2).
    if args.first().map(|s| s.as_str()) == Some("--cli") {
        let path = args.get(1).cloned();
        run_cli(path);
        return;
    }

    run_ui(args.first().cloned());
}

/// Spawns the engine thread and returns the command sender, event receiver,
/// the shared atomics block, and the engine's JoinHandle. Shared by both the
/// CLI and UI entry points so there is exactly one place that wires the
/// engine up. The handle is joined after a Shutdown send on every exit path
/// (section 9 rule 5: the teardown save must run before the process dies).
fn spawn_engine() -> (
    mpsc::Sender<Command>,
    mpsc::Receiver<Event>,
    Arc<Shared>,
    std::thread::JoinHandle<()>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (evt_tx, evt_rx) = mpsc::channel::<Event>();
    let shared = Shared::new();

    let shared_for_engine = Arc::clone(&shared);
    let handle = std::thread::Builder::new()
        .name("qpid-engine".into())
        .spawn(move || {
            engine::run(cmd_rx, evt_tx, shared_for_engine);
        })
        .expect("failed to spawn engine thread");

    (cmd_tx, evt_rx, shared, handle)
}

fn run_cli(initial_path: Option<String>) {
    let (cmd_tx, evt_rx, shared, engine) = spawn_engine();

    // Drain events on a background thread and just print them; Phase 1's
    // CLI is a smoke test, not a UI.
    std::thread::spawn(move || {
        while let Ok(event) = evt_rx.recv() {
            println!("[event] {event:?}");
        }
    });

    if let Some(p) = initial_path {
        let _ = cmd_tx.send(Command::OpenPath(PathBuf::from(p)));
    } else {
        // No argument: restore the previous session (section 9 rule 1) —
        // last folder/file/position, Paused, no autoplay. The stdin loop
        // below still runs, so the session can be driven or quit normally.
        let _ = cmd_tx.send(Command::RestoreSession);
    }

    println!("Controls: p=pause/resume  f=+15s  b=-15s  n=next  P=prev  s=speed  w=wake-counter  d=device-error  q=quit");

    // Fresh process always starts at 1x, so the cycle position is known.
    let mut speed_idx: u32 = 1;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        match line.trim() {
            "p" => {
                let _ = cmd_tx.send(Command::TogglePlay);
            }
            "f" => {
                let _ = cmd_tx.send(Command::SeekRelative(15_000));
            }
            "b" => {
                let _ = cmd_tx.send(Command::SeekRelative(-15_000));
            }
            "n" => {
                let _ = cmd_tx.send(Command::Next);
            }
            "P" => {
                let _ = cmd_tx.send(Command::Prev);
            }
            "s" => {
                // Cycle 1 -> 1.25 -> 1.5 -> 2 -> 0.5 -> 1, so three presses
                // from a fresh start land on 2x (tools/bench.py relies on it).
                speed_idx = match speed_idx {
                    0 => 1,
                    4 => 0,
                    i => i + 1,
                };
                let speed = Speed::from_index(speed_idx);
                println!("speed -> {}x", speed.as_f64());
                let _ = cmd_tx.send(Command::SetSpeed(speed));
            }
            "q" => {
                let _ = cmd_tx.send(Command::Shutdown);
                break;
            }
            "" => {
                // Bare Enter: print current position as a quick sanity check
                // of the position formula (section 7).
                println!("position_ms = {}", shared.position_ms());
            }
            "w" => {
                // Budget 9 (spec 5): engine wakes over uptime. The stdin
                // read itself does not wake the engine, so a single
                // reading over a 60 s window IS the background rate.
                let wakes = shared.wakes.load(Ordering::Relaxed);
                let up = shared.started.elapsed().as_secs_f64();
                let underruns = shared.underruns.load(Ordering::Relaxed);
                println!(
                    "wakes={wakes} uptime_s={up:.1} wakes_per_s={:.2} underruns={underruns}",
                    wakes as f64 / up.max(0.001)
                );
            }
            "d" => {
                // Phase 5 probe: simulate cpal's device-error callback
                // (run_cli is the dev-only CLI; rule 10's no-log rule does
                // not apply to its existing stdout prints).
                shared.device_error.store(true, Ordering::Relaxed);
            }
            other => {
                eprintln!("unknown command: {other}");
            }
        }
    }

    // Exit path: "q" already sent a Shutdown (a second one is harmless —
    // the engine has either consumed it or is gone), and stdin EOF reaches
    // here without one. Sending before joining guarantees the engine sees
    // a Shutdown rather than blocking on a sender we still hold.
    let _ = cmd_tx.send(Command::Shutdown);
    let _ = engine.join(); // section 9 rule 5: let the teardown save land
}

fn run_ui(initial_path: Option<String>) {
    // Section 12.5 rule 4: a second launch hands its path to the first
    // instance and exits. The --cli path is exempt (dev tool, rule ruling).
    if !platform::claim_instance() {
        platform::send_to_first_instance(initial_path.as_deref().map(PathBuf::from).as_deref());
        return;
    }
    winit_hook::install();
    let (cmd_tx, evt_rx, shared, engine) = spawn_engine();
    platform::start_media_keys(cmd_tx.clone());

    // The first instance's reader: decode UTF-16 payload -> OpenPath,
    // empty payload = raise only. Always raise the window (rule 4).
    {
        let cmd_tx = cmd_tx.clone();
        platform::start_instance_listener(move |buf| {
            if buf.len() >= 2 && buf.len() % 2 == 0 {
                let units: Vec<u16> = buf
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                if let Ok(s) = String::from_utf16(&units) {
                    if !s.is_empty() {
                        let _ = cmd_tx.send(Command::OpenPath(PathBuf::from(s)));
                    }
                }
            }
            platform::raise_window();
        });
    }

    let window = MainWindow::new().expect("failed to create UI window");
    ui::wire(&window, cmd_tx.clone(), evt_rx, Arc::clone(&shared));

    if let Some(p) = initial_path {
        let _ = cmd_tx.send(Command::OpenPath(PathBuf::from(p)));
    } else {
        // No argument: restore the previous session (section 9 rule 1).
        let _ = cmd_tx.send(Command::RestoreSession);
    }

    window.run().expect("event loop failed");
    platform::stop_media_keys();

    let _ = cmd_tx.send(Command::Shutdown);
    let _ = engine.join(); // section 9 rule 5: let the teardown save land
}
