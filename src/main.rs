#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod ui;

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use engine::{Command, Event, Shared};

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
/// and the shared atomics block. Shared by both the CLI and UI entry points
/// so there is exactly one place that wires the engine up.
fn spawn_engine() -> (mpsc::Sender<Command>, mpsc::Receiver<Event>, Arc<Shared>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (evt_tx, evt_rx) = mpsc::channel::<Event>();
    let shared = Shared::new();

    let shared_for_engine = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("qpid-engine".into())
        .spawn(move || {
            engine::run(cmd_rx, evt_tx, shared_for_engine);
        })
        .expect("failed to spawn engine thread");

    (cmd_tx, evt_rx, shared)
}

fn run_cli(initial_path: Option<String>) {
    let (cmd_tx, evt_rx, shared) = spawn_engine();

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
        eprintln!("usage: qpid --cli <file-or-folder>");
        return;
    }

    println!("Controls: p=pause/resume  f=+15s  b=-15s  n=next  P=prev  q=quit");

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
            "q" => {
                let _ = cmd_tx.send(Command::Shutdown);
                break;
            }
            "" => {
                // Bare Enter: print current position as a quick sanity check
                // of the position formula (section 7).
                println!("position_ms = {}", shared.position_ms());
            }
            other => {
                eprintln!("unknown command: {other}");
            }
        }
    }
}

fn run_ui(initial_path: Option<String>) {
    let (cmd_tx, evt_rx, shared) = spawn_engine();

    let window = MainWindow::new().expect("failed to create UI window");
    ui::wire(&window, cmd_tx.clone(), evt_rx, Arc::clone(&shared));

    if let Some(p) = initial_path {
        let _ = cmd_tx.send(Command::OpenPath(PathBuf::from(p)));
    }

    window.run().expect("event loop failed");

    let _ = cmd_tx.send(Command::Shutdown);
}
