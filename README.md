# q-pid

A small folder-based audio player for Windows, written in Rust with a Slint UI. Point it at a folder or a file, and it plays the folder as a naturally sorted playlist: seek bar, skip buttons, five playback speeds that keep pitch, session restore, and media-key support. A headless `--cli` mode runs the same engine without a window.

It is built to strict resource limits: the executable stays under 10 MB, a minimized instance keeps CPU near zero, the process uses no more than 12 threads, and the background engine wakes no more than twice per second. Every number in this README comes from the measurement tables in [BENCH.md](BENCH.md); the normative design lives in [architecture.md](architecture.md).

## Building and running

```powershell
cargo build --release        # target\release\qpid.exe (<= 10 MB by design)
cargo test                   # 34 tests
target\release\qpid.exe                      # restore last session, paused
target\release\qpid.exe <folder-or-file>     # open and play immediately
target\release\qpid.exe --cli [file]         # headless mode, stdin controls
```

Release settings favor size over speed (`opt-level = "s"`, fat LTO, LTO'd dependencies, symbols stripped): decode and resample do not come close to saturating one core at 1x, and the 10 MB budget binds harder than dependency speed. The UI uses Slint's software renderer, so there is no GPU or driver dependency.

Supported formats: mp3, m4a, m4b, aac, flac, ogg, wav (via symphonia).

## How the code works, start to finish

### 1. Process startup (`src/main.rs`)

`main()` first sets `SLINT_BACKEND=winit-software` so every later Slint call uses the software renderer, then splits on the arguments:

- `--cli` runs `run_cli`: the engine with a stdin loop instead of a window. This is the harness the tests and benchmark scripts drive.
- anything else runs `run_ui`, the shipping path.

`run_ui` does four things before showing a window:

1. **Single instance.** `winapi::claim_instance()` takes the `Global\qpid-instance` mutex. If another instance already holds it, this process sends its file argument to the first instance over the `\\.\pipe\qpid-open` named pipe and exits. The first instance decodes the payload, opens the path, and raises its window. One player per machine.
2. **Installs the winit hook** (`src/winit_hook.rs`), the only module allowed to touch Slint's unstable winit API. It observes window visibility and file drops, forwards both to sinks that `ui.rs` registers, and passes every other winit event to Slint unchanged.
3. **Spawns the engine** (see below) and the Windows extras: a media-key listener (`RegisterHotKey` for play/pause, next, previous) and the pipe listener from step 1.
4. **Creates the Slint window** (`ui/main.slint` compiled by `slint-build` in `build.rs`), wires it with `ui::wire()`, then sends either `OpenPath` (argument given) or `RestoreSession` (no argument).

`window.run()` blocks until the window closes. On every exit path the main thread sends `Command::Shutdown` and joins the engine, so the final state save always lands before the process dies.

### 2. Threading model

The process runs at most 12 threads at once (the thread budget; measured at 11 idle, 12 playing):

| Thread | Role |
|---|---|
| main | Slint event loop, window, input; polls `Shared` atomics |
| qpid-engine | decoder, DSP, playlist, persistence; blocks on `recv_timeout` |
| cpal callback(s) | real-time audio: pops the ring, ramps gain, writes to the device |
| qpid-pipe | named-pipe listener for second-instance handoffs |
| media keys | owns the `WM_HOTKEY` message window |

Everything else is deliberately absent: no timers beyond one 500 ms UI position ticker, no watchdog threads, no background scanners. Work rides existing wakes.

### 3. Talking to the engine (`Command`, `Event`, `Shared`)

The UI and the engine share three things, all defined in `src/engine/mod.rs`:

- `Command` is an `mpsc` channel into the engine: `OpenPath`, `Play`, `Pause`, `TogglePlay`, `SeekRelative`, `SeekAbsolute`, `Next`, `Prev`, `SetSpeed`, `RestoreSession`, `Shutdown`.
- `Event` is the channel back: `TrackChanged`, `StateChanged`, `Message`.
- `Shared` is a block of atomics both sides read lock-free. The UI timer never blocks and never wakes the engine to read the playhead; it just computes

```
position_ms = base_ms + played_frames * speed_milli / out_rate
```

`base_ms` is the position when the current segment started; `played_frames` counts frames consumed since. A pause or speed change folds the live position back into `base_ms`, which is why the formula never jumps.

### 4. The engine loop (`engine::run`)

The engine is one thread with one loop and no timers. Each iteration:

1. Compute how long to sleep. While playing, that is the time until the ring buffer drains to its low-water mark (1 s; high water 2.9 s inside a 3 s ring), clamped to 100..2000 ms and capped by the next 30-second checkpoint deadline. While paused or idle it sleeps until the 10-second release deadline or an hour, whichever comes first.
2. `recv_timeout(wait)`: handle a command, run timeout work (pause release, checkpoints) on expiry, or save and exit if the UI dropped the sender.
3. React to flags: a cpal device error pauses and releases the stream; the checkpoint persists the position if it moved.
4. If playing, `refill_until` decodes chunks into the ring, checking for a pending command between chunks so a pause or seek is never delayed by more than one chunk. When the decoder reports end of stream, the engine advances to the next playlist entry, or enters `Ended` on the last file.

That is the whole background-wakeup story: the engine sleeps on the ring's own fill level, so a full buffer means minutes of silence.

### 5. Opening a file

`open_path` walks the folder once (`src/playlist.rs`): keep the seven playable extensions, skip hidden and system files, sort with natural ordering so `2.mp3` precedes `10.mp3`. Which entry to start on comes from the store (last file if it is not done, else the first not-done file).

Then it builds the playback context:

1. `Decoder::open` (`src/engine/decode.rs`) wraps symphonia: one file, one 64 KiB stream buffer, one packet at a time, everything converted to interleaved stereo `f32`. Duration comes free from codec metadata at open, no packets decoded.
2. `OutputStream::build` (`src/engine/output.rs`) opens the default cpal device and a 3-second lock-free `rtrb` ring.
3. `Resampler::new` (rubato, FFT-based) converts the file's rate to the device rate; it is bypassed when they match (48 kHz files).
4. `Stretcher` (WSOLA, hand-written in `src/engine/dsp.rs`) handles speed changes without changing pitch.
5. Seek to the saved position if any, prefill 250 ms into the ring, start the stream, state becomes `Playing`.

### 6. The audio pipeline

```
file -> symphonia decode -> WSOLA stretcher -> rubato resampler -> rtrb ring -> cpal callback -> device
```

The callback in `output.rs` is the only real-time code in the project. It never allocates, locks, touches the filesystem, or logs. It pops frames from the ring, applies a short linear gain ramp, and writes samples. If the ring is empty when it should not be (and it is not a flush, drain, or startup), it bumps the `underruns` counter.

At 1x the stretcher is a pure copy, so normal playback pays for nothing beyond decode and (if needed) resample.

### 7. Seeking and speed changes

Both are **flushes**, never live parameter tweaks (`src/engine/mod.rs`, the §7 protocol):

1. Ramp gain to zero over 20 ms.
2. Set `flush_req`, wait for the callback's `flush_ack` (200 ms ceiling so a stalled device cannot hang the engine).
3. Clear the ring, seek the decoder, reset stretcher and resampler.
4. Refill 250 ms, release the gate, ramp gain back up.

The user hears a short fade instead of a click.

Seek accuracy follows §7.6 of the spec: `Accurate` by default; once a single relative seek takes more than 300 ms (VBR MP3s without an index), `SeekPolicy` sticks to `Coarse` for the rest of that file's relative seeks. Absolute seeks, like slider drags, always stay accurate.

A speed change captures the position under the old speed, stores the new speed, then flushes back to that position. The five speeds are 0.5x, 1x, 1.25x, 1.5x, 2x.

### 8. Pause, release, and resume

Pause folds the position into `base_ms`, saves, fades out, and stops the stream. After 10 seconds paused (or `Ended`), `on_timeout` releases the whole context: stream, ring, decoder, DSP are dropped, and only the path survives in `released_path`. This is how a minimized player reaches near-zero CPU: there is nothing left running. Resume reopens the file from scratch at the stored position.

If cpal reports a device error (unplug, sleep), the engine pauses the same way and shows a message telling the user to press Play to resume. Playback never auto-resumes onto the new device.

### 9. Persistence (`src/store.rs`)

State lives in `%LOCALAPPDATA%\qpid\state.json`: last folder, last file, per-file position and size, a `done` flag per file, playback speed, and up to 100 folders of history. Loading is forgiving: a missing, corrupt, or wrong-version file loads as defaults, never a crash. Saving goes through one function, `save_now`, called only at well-defined triggers: pause, opening another file, next/prev, speed change, error stops, the 30-second checkpoint while playing, and application exit (including the panic-free paths where the UI channel just closes).

A file position is only trusted if the file size still matches; a restored position rewinds a hair before resume (the constant exists even though it is currently 0).

### 10. The UI layer (`src/ui.rs`, `ui/main.slint`)

The UI is intentionally dumb. It may import exactly `Command`, `Event`, and `Shared` from the engine, never the decoder or DSP. Buttons translate to commands; events update labels and messages. One sanctioned Slint timer ticks every 500 ms while playing and the window is visible, updating the position label and slider; pause or minimize stops it.

The visibility sink from the winit hook does two jobs on minimize: it switches the process to EcoQoS (Windows power throttling) and trims the working set (`QPID_NO_TRIM=1` disables the trim for measurement). Restoring the window undoes both. Dragging a file onto the window arrives through the same hook as an `OpenPath` command.

### 11. Where the numbers come from

The nine resource budgets (executable size, launch time, RAM while playing and minimized, CPU in each state, thread count, wakeup rate) are defined in architecture.md §2 and measured in BENCH.md. Current results at the v1.0.0 tag: 10,452,992 bytes, 20.5 ms warm launch, 5.43 MB playing, 0.00% paused CPU, 12 threads (at cap), 0.55 wakes/s. Minimized CPU carries a permanent "unstable" ruling caused by Windows scheduling on hybrid CPUs, documented rather than hidden.

## Repository layout

| Path | What it holds |
|---|---|
| `src/main.rs` | entry point, CLI mode, UI startup, instance handoff |
| `src/engine/mod.rs` | engine loop, commands, position formula, flush protocol |
| `src/engine/decode.rs` | symphonia wrapper, chunked decode, seek policy |
| `src/engine/dsp.rs` | WSOLA stretcher and rubato resampler |
| `src/engine/output.rs` | cpal stream, ring buffer, real-time callback |
| `src/playlist.rs` | folder scan, extension filter, natural sort |
| `src/store.rs` | `state.json` load/save, pruning, key normalization |
| `src/ui.rs` | Slint glue: timers, buttons, events, visibility sink |
| `src/winit_hook.rs` | winit observation: visibility and file drops |
| `src/winapi.rs` | EcoQoS, working-set trim, single instance, media keys |
| `ui/main.slint` | the UI layout itself |
| `tools/bench.py` | budget measurement harness |
| `tools/gen_test_audio.py` | fixture generator for tests |

## Documentation

- [architecture.md](architecture.md) is the binding design: budgets, rules, state machine, acceptance tests.
- [BENCH.md](BENCH.md) holds every measurement, the final v1.0.0 table, and the permanent rulings.
- [docs/USER_GUIDE.md](docs/USER_GUIDE.md) is the user-facing manual with keyboard shortcuts.
- [IMPLEMENTATION.md](IMPLEMENTATION.md) tracks implementation status and recorded deviations.
- [handoff.md](handoff.md) is the development session handoff: state, struggles, next phase.

## License

No license file has been chosen yet.
