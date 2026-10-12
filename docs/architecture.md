# q-pid: Architecture

Project name: q-pid (renamed before v1.0.0; every shipped artifact uses q-pid).

This document is a set of orders for a coding agent. Follow it in sequence. When a rule and a convenience conflict, the rule wins. When a crate name, feature name or API in this document does not match the current docs on docs.rs, trust the docs and keep the intent.

## 1. Goal

A Windows 11 audio player. One portable exe. The user points to a folder or a file and it plays.

Controls: previous, back 15 s, play/pause, forward 15 s, next, and speed (0.5x, 1x, 1.25x, 1.5x, 2x). The player resumes where the last session stopped.

Main content is audiobooks, lectures and podcasts. Music works too.

Priority order for every decision:

1. Background CPU and RAM cost.
2. Playback correctness: no clicks, exact seeks, pitch preserved at every speed.
3. Simple, clean UI.
4. Feature count. Fewer is better.

## 2. Budgets

Measured on Windows 11 x64, release build, one 44.1 kHz stereo MP3 at 128 kbps, after 60 s of playback.

1. Exe size: 11,031,024 B (10.52 MiB) or less. (Raised from 10 MB for PULSAR: the Slint software renderer links a one-time ~34 KB path rasterizer the first time a `Path` element is used — measured 2026-10-11, 10,493,440 B with Path icons vs 10,459,648 without. Old 10 MB cap = 10,485,760 B.)
2. Startup to first frame: 300 ms or less.
3. Private working set, window visible, playing at 1x: 25 MB or less.
4. Private working set, window minimized, playing at 1x: 15 MB or less. Measure without the working set trim from section 11. The trim must not be used to pass this budget.
5. CPU, playing at 1x, minimized: 0.5% of one core or less.
6. CPU, playing at 2x, minimized: 2% of one core or less.
7. Paused for more than 10 s: 0.0% CPU, no timers, no audio stream open, no decoder open.
8. Threads owned by the app: 12 or fewer. (Revised from 4 — the thread isolation measurement in `BENCH.md` shows Slint's floor alone is 9 and cpal's is 7; the app-owned design target stays 3 threads.)
9. Wakeups from app code during background playback, excluding the audio callback: 2 per second or fewer.
10. CPU, visualizer on, window visible, playing at 1x: 3% of one core or less.

Budgets 4 to 9 are measured with the visualizer off and again with it on and the window minimized (must match off).

If a budget fails, fix it before adding features. These are targets. The agent must measure them, not assume them.

## 3. Decisions

1. Rust only at runtime. One process, one exe. No Python in the shipped app. A Python runtime would consume a large share of the RAM budget. Python is used only for tooling in `tools/`.
2. No browser engine. No WebView2, Electron or Tauri. They start extra processes and cost far above the budget.
3. UI: Slint with the software renderer and the winit backend. Event driven. No GPU context. No continuous redraw. If Phase 0 measurement fails the budget, replace only the UI layer with raw Win32 plus Direct2D. The UI is isolated behind `Command`, `Event` and `Shared` (section 5) so this swap touches one module.
4. Decoding: `symphonia`. Pure Rust. No ffmpeg, libmpv or VLC DLLs.
5. Output: `cpal` on WASAPI shared mode.
6. Speed: pitch preserving time stretch. `signalsmith-stretch` was the first choice; its build failed on the target machine (bindgen needs libclang), so the section 4 note 4 fallback — a hand-written WSOLA stretcher — is implemented instead. Fully bypassed at 1x.
7. Decoding runs in bursts into a 3 s ring buffer. The engine thread sleeps between bursts. The real time audio callback only copies memory.
8. Persistence: one small JSON file.
9. No Python, tokio, async runtime, logging framework, tag library, database or tray library.

## 4. Dependencies

Versions are omitted on purpose. Resolve the latest stable with `cargo add`. Verify feature names on docs.rs.

```
cargo add slint --no-default-features --features std,compat-1-2,backend-winit,renderer-software
cargo add slint-build --build
cargo add cpal
cargo add symphonia --no-default-features --features mp3,aac,isomp4,flac,vorbis,ogg,wav,pcm,alac
cargo add rubato
cargo add rtrb
cargo add rfd
cargo add serde --features derive
cargo add serde_json
cargo add natord
cargo add windows
cargo add winresource --build
```

Notes:

1. Add the Slint `accessibility` feature only if screen reader support is required. It costs memory.
2. Slint needs the winit hook feature (unstable in some versions) to receive raw window events. It is used for minimize detection, occlusion and dropped files. If unavailable, use the fallbacks in sections 10 and 11.
3. `windows`: enable only the features the compiler asks for. Expected areas are threading, process power throttling, working set, named mutex, named pipe and window messages.
4. `signalsmith-stretch` builds C++ through `cc`. It needs the MSVC build tools, which a Rust MSVC setup already requires. If it fails to build, use SoundTouch bindings or write a WSOLA stretcher (about 150 lines). Do not use Rubber Band unless GPL is acceptable.
5. Symphonia does not decode Opus or WMA. They are out of scope for v1.

## 5. Threads and communication

The app owns three threads.

1. UI thread. Slint event loop. It blocks in the OS message loop when idle. It sends `Command` values to the engine and receives `Event` values.
2. Engine thread. Owns the decoder, stretcher, resampler, playlist, persistence and the cpal `Stream`. `Stream` is not `Send` on every platform, so it is created and dropped on this thread. Thread priority is `BELOW_NORMAL`. It sleeps in `recv_timeout`.
3. Audio callback thread. Created by cpal. Real time. It reads the ring buffer, applies the gain ramp and counts frames. Rules for this thread: no allocation, no locks, no file I/O, no logging, no channel sends. Atomics only.

A fourth thread is allowed only for the single instance pipe listener (Phase 5). It blocks on a read and costs nothing while idle.

Shared types:

```rust
pub enum Speed { X0_5, X1, X1_25, X1_5, X2 }

pub enum Command {
    OpenPath(std::path::PathBuf),   // file or folder
    Play,
    Pause,
    TogglePlay,
    SeekRelative(i64),              // milliseconds, negative allowed
    SeekAbsolute(u64),              // milliseconds
    Next,
    Prev,
    SetSpeed(Speed),
    SetViz(u8),                      // 0 off, 1 line, 2 pulsar; saved like a speed change
    Shutdown,
}

pub enum Event {                    // sent only when something changes
    TrackChanged { folder: String, title: String, index: u32, count: u32, duration_ms: u64 },
    StateChanged(PlayState),        // Idle, Playing, Paused, Ended, Error
    Message(String),                // short user visible error text
    Settings { speed_index: u32, viz: u8 }, // sent once after the store loads; sets the speed row and the viz mode (also fixes the stale speed-row quirk)
}

pub struct Shared {                 // all atomics, read by the UI timer
    pub base_ms: AtomicU64,         // source position of first frame written after last flush
    pub played_frames: AtomicU64,   // output frames consumed by the callback since last flush
    pub speed_milli: AtomicU32,     // 500, 1000, 1250, 1500, 2000
    pub out_rate: AtomicU32,
    pub duration_ms: AtomicU64,     // 0 when unknown
    pub target_gain: AtomicU32,     // f32 bits
    pub flush_req: AtomicBool,
    pub flush_ack: AtomicBool,
    pub eof: AtomicBool,
    pub drained: AtomicBool,
    // T5 adds three viz atomics: viz_on: AtomicBool, viz_head: AtomicU32,
    // viz_peaks: [AtomicU32; 128] (peak as f32 bits)
}
```

Events reach the UI through `slint::invoke_from_event_loop`. The UI reads `Shared` atomics from its own timer. The UI never touches the engine internals.

## 6. Audio pipeline

```
file -> symphonia decode -> stereo f32 interleaved -> stretch (skip at 1x)
     -> resample (skip when rates match) -> rtrb ring -> callback -> gain ramp -> device
```

Orders:

1. Read files with plain buffered reads. Keep the symphonia `MediaSourceStream` buffer at 64 KiB. Never memory map files. Never load a whole file. Audiobook files can exceed 100 MB.
2. Convert every source to stereo f32. Mono is duplicated. More than 2 channels are downmixed to L and R with a simple fold.
3. Output uses the default device config from cpal. Require f32. Callback channel map: 1 device channel gets (L+R)/2. 2 channels get L and R. More than 2 get L and R in the first two and zeros elsewhere.
4. Resample with `rubato` only when the file rate differs from the device rate. Use a fixed input FFT resampler. Skip it when the rates match.
5. Stretch runs before the resampler. Feed input chunks of about 4096 frames. Output length is input length divided by speed. Keep the stretch parameters modest (frame/search sizes in `dsp.rs` are the tuning knob) — speech is the main content. Use `reset()` on every flush. Call flush on the stretcher at end of file to drain its tail. Re-measure budget 6 (CPU at 2x) after any parameter change.
6. Speed 1x: no stretcher call at all. Copy decoded frames straight on.
7. The ring holds 3 s of stereo f32 at the device rate. Preallocate it once per stream. At 48 kHz that is about 1.1 MB.
8. Water marks: `HIGH_WATER` 2.9 s, `LOW_WATER` 1.0 s, `PREROLL` 0.25 s. HIGH_WATER must leave headroom under the 3 s ring: refilling to exactly capacity makes the last packet of every burst overflow and drop (measured: ~11 ms of audio lost per refill cycle before this was corrected).
9. Decode errors on a single packet: skip the packet and continue. IO errors or unsupported files: emit `Message`, skip to the next file. If no file is playable, go to `Error` state.
10. Preallocate all working `Vec` buffers once. No allocation inside the decode loop.

### Engine loop

```
loop {
    let wait = match state {
        Playing => secs until ring fill falls to LOW_WATER, clamped to 100 ms .. 2000 ms,
        Paused  => 10 s,            // then release everything (section 8)
        _       => no timeout,      // Idle, Ended, Error
    };
    match rx.recv_timeout(wait) {
        Ok(cmd)                  => handle(cmd),
        Err(Timeout)             => on_timeout(),
        Err(Disconnected)        => break,
    }
    if state == Playing && fill < LOW_WATER {
        refill_until(HIGH_WATER);   // burst, in chunks
    }
    if checkpoint_due() { save_state(); }
}
```

Rules:

1. Fill is `capacity - producer.slots()` (rtrb).
2. `refill_until` works in chunks of about 4096 source frames. After each chunk it calls `try_recv`. A pending command must never wait longer than one chunk.
3. Expected pattern while playing: one burst about every 2 s, then sleep.

## 7. Position, seek and speed

Position formula:

```
position_ms = base_ms + played_frames * 1000 * speed / out_rate
```

1. `base_ms` is the source position of the first frame written after the last flush.
2. `played_frames` counts only real audio frames the callback consumed. Silence is not counted.
3. Accuracy is within the stretcher latency, under about 100 ms. This is enough for UI and resume.
4. When paused, the same formula gives the frozen position.
5. The UI tolerates one stale read after a flush. Do not add locking for this.

### Flush while playing

Used by seek, speed change, next, prev and open.

1. Engine sets `target_gain` to 0. The callback ramps down over 15 ms. Engine waits 20 ms.
2. Engine sets `flush_req = true`.
3. The callback sees `flush_req`. It pops everything from the ring, sets `played_frames` to 0, sets `flush_ack = true`. While `flush_req` stays true it outputs silence and reads nothing.
4. Engine waits for `flush_ack` with short parks of 2 ms. Timeout at 200 ms means the device stalled. Treat it as a device error (section 12).
5. Engine seeks the decoder, resets stretcher and resampler, writes `base_ms` and `speed_milli`, clears `eof` and `drained`.
6. Engine refills to `PREROLL`, clears `flush_ack`, then clears `flush_req`.
7. Engine sets `target_gain` to 1. The callback ramps up over 15 ms.

### Flush while paused

The stream is stopped, so the callback cannot drain the ring. Do not use the protocol above. Drop the stream, ring, decoder and DSP. Keep only the resume position. Update `base_ms` and `played_frames` directly so the UI shows the new position. Everything is rebuilt on the next Play.

### Rules

1. Speed change is a flush to the current position with the new speed. This makes the change audible within about 100 ms. Never wait for the old 3 s of audio to play out.
2. Pause: target gain 0, wait 20 ms, then `stream.pause()`. Resume: `stream.play()`, target gain 1.
3. Seek target is clamped to 0 and to duration minus 1 s. If duration is unknown, forward seeks are allowed and end of file triggers next.
4. Back 15 s at less than 15 s into a file clamps to 0. It does not go to the previous file.
5. Forward 15 s past the end of a file goes to the next file at position 0.
6. Decoder seek: symphonia `SeekMode::Accurate` with `SeekTo::Time`. After the seek, decode and discard frames until the required timestamp. If a seek in a VBR MP3 without a table of contents takes more than 300 ms, fall back to `Coarse` for the relative seeks only.
7. Duration comes from codec params (frame count and time base). If unknown, store 0. The UI then shows elapsed time only and disables the slider drag.

### End of track

1. Decoder returns end of stream. Engine drains the stretcher tail into the ring, sets `eof = true`, and stops refilling.
2. The callback sets `drained = true` when the ring is empty and `eof` is true.
3. Engine polls `drained` every 100 ms. On true: mark the file done, open the next file, refill to `PREROLL`, clear the flags, continue. The stream stays open. Expected gap: under 100 ms. Gapless playback is out of scope.
4. After the last file: state `Ended`, pause path, stream released after 10 s. Play after `Ended` restarts the last file from 0.

## 8. Paused and idle resources

1. On pause, stop the stream immediately. A stopped stream removes the audio callback wakeups.
2. After 10 s paused, release the cpal stream, ring, decoder, stretcher and resampler. Keep the file path and resume position.
3. On Play, rebuild everything, seek to the resume position, refill to `PREROLL`, start. Expected start latency: under 200 ms.
4. Idle with no file loaded: nothing allocated beyond the UI and the state file contents.

## 9. Persistence

Location: `%LOCALAPPDATA%\qpid\state.json`. Read it with `std::env::var("LOCALAPPDATA")`. No extra crate.

Schema:

```json
{
  "version": 1,
  "speed": 1.25,
  "viz": 0,
  "last_folder": "d:\\audiobooks\\book one",
  "folders": {
    "d:\\audiobooks\\book one": {
      "touched": 1758400000,
      "last_file": "chapter 03.mp3",
      "files": {
        "chapter 03.mp3": { "pos_ms": 734000, "size": 48211930, "done": false }
      }
    }
  }
}
```

`viz`: 0 off, 1 Line, 2 Pulsar. A missing field loads as 0, so files written by older builds open as Off. `version` stays 1.

Rules:

1. Key folders by lowercase path with any `\\?\` prefix stripped. Key files by lowercase file name.
2. Store `size`. If the size differs on load, ignore the saved position for that file.
3. Store entries only for files with a position above 0 or marked done. Keep at most 100 folders, evicting by oldest `touched`.
4. A file is `done` when it reaches end of stream or the position is within the last 3 s.
5. Write triggers: pause, next, prev, open of another path, speed change, visualizer change, app exit, error stop, and every 30 s while playing. Also write when a checkpoint is due only if the position changed.
6. Write atomically: write `state.json.tmp` in the same folder, then `std::fs::rename` over the target. Never write per second.
7. Expected loss after a crash or power cut: 30 s or less.
8. Optional constant `RESUME_REWIND_MS`, default 0. Applied once when resuming a saved position.

Restore rules:

1. Launch without arguments: load the last folder, select `last_file`, show the saved position, stay paused. Do not autoplay.
2. Open a folder: build the playlist, pick `last_file` if it exists and is not done. Otherwise pick the first file that is not done. If all are done, pick the first file. Then start playing.
3. Open a single file: playlist is the sorted audio files in its parent folder. Start at that file with its saved position. Start playing.
4. If the state file is missing or corrupt, start with defaults and overwrite it on the next write. Never crash.

## 10. UI specification

Window: default 480 x 320 px, minimum 400 x 320 px (was 280 high: the new rows need 314), resizable. With the visualizer on, the preferred and the minimum height become 400. Extra height goes to a flexible spacer above the seek row. Native title bar. Follow the system light or dark theme. Font: Segoe UI Variable (system font, nothing embedded). Use the Slint `fluent` style for slider and buttons. Icons: drawn with Slint `Path`, literal `commands` strings only, 24 x 24 viewbox, `fill` set, no SVG assets.

Layout, top to bottom, 20 px outer padding, 8 px spacing grid, heights in px (no visualizer, total 314 of 320):

| # | Row | Height | Gap above |
|---|---|---|---|
| 1 | Top row | 36 | 16 (padding) |
| 2 | Folder row | 16 | 8 |
| 3 | Title row | 28 | 8 |
| 3b | Visualizer strip (only when on) | 72 | 8 |
| 4 | Seek control (24) + times row (14) | 42 | 8 (+4 inside) |
| 5 | Transport row | 64 | 8 |
| 6 | Speed control | 34 | 8 |
| 7 | Status line (always reserved, no layout jump) | 14 | 8 |
|   | Bottom padding | 16 |  |

1. Top row: "Open file" and "Open folder" buttons left, 36 px tall, surface fill, 1 px divider border, radius 8, 12 px text, 14 px side padding. Right: two round 36 px icon buttons, divider border, no fill: Visualizer (wave glyph, cycles the mode) and Help (`?`, 14 px, semibold, text-muted). Hit area of each control is 40 px tall (TouchArea 2 px larger on top and bottom, no clipping).
2. Folder row: 14 px folder icon, then the folder name, 12 px, text-muted, elide right. The name is the last path component of the `folder` string, computed in `ui.rs` when `TrackChanged` arrives. No engine change. If the path has no file name (a drive root), show the full string.
3. Title row: title 20 px, semibold, elide right. The `N / M` counter sits right after the title, aligned to the title baseline, in a pill: 11 px text-muted, surface fill, 1 px divider border, radius 999, padding 2 px by 9 px.
3b. Visualizer strip: present only when the visualizer mode is not Off. Turning it on or off changes the window height by 80 px (72 px strip plus 8 px gap); Line to Pulsar does not resize.
4. Seek control: touch height 24 px, track 6 px high, centered, radius 3, divider color, accent fill. Thumb: 14 px circle, text-primary, centered on the fill end, shown only when the control is enabled. The 24 px touch height is the deliberate exception to the 40 px rule, the control spans the full width. All pointer logic in `SeekSlider` stays as it is; only the drawing changes. Times row below: elapsed on the left in text-primary, duration on the right in text-muted, both 11 px, so digit width changes move nothing. Format `H:MM:SS`, or `M:SS` under one hour.
5. Transport row: centered, 10 px between items. Previous and Next: 44 px round, no fill, 20 px `Path` icon, hover surface-hover. Back and forward 15: pill buttons 40 px tall, min width 48, surface fill, 1 px divider border, 13 px, semibold, text `-15` and `+15`. Play and pause: 64 px circle, accent fill, 26 px `Path` icon in accent text. The icon swaps between play and pause.
6. Speed control: one pill container, surface fill, 1 px divider border, radius 17, 3 px padding. Five equal cells, 56 x 26, radius 13. Active cell: accent fill, semibold. Inactive: text-muted. Container width is 288 px. Labels: 0.5x, 1x, 1.25x, 1.5x, 2x.
7. Status line: always reserved, no layout jump. Empty by default. Shows `Message` text such as an unreadable file. Clear it on the next `TrackChanged`, `Play` or open command.

Empty state: when there is no track, rows 2 and 3 are replaced by one card of the same total height (52 px): surface-sunken fill, 1 px divider border, radius 8, a folder icon and "Drop a folder or file here, or press Open" in 13 px text-muted, centered. Seek, transport and speed stay in place with the dim tokens, so nothing moves when a track loads.

Design tokens (define once in the .slint file, light and dark variants):

1. Background, surface, text primary, text muted, accent, accent text, divider.
2. Corner radius 8 px. Buttons 36 px tall. Hit targets at least 40 px.
3. One accent color. Blue is fine.
4. No shadows, gradients or animations.

Behavior:

1. Timer: the one sanctioned timer, started while the window is visible and playing, stopped on pause, minimize and occlusion, restarted on the opposite event (same start and stop rules as before). Interval: 500 ms with the visualizer off, 66 ms in Line, 100 ms in Pulsar; restarted when the mode changes. Still one timer. It reads `Shared`, computes position, and sets properties only when the displayed value changes.
2. Slider drag: while the user drags, ignore engine position. On release, send `SeekAbsolute`.
3. Window events: use the winit hook to receive minimize, occlusion and dropped file events. Fallback if the hook is unavailable: a 1 s timer that only checks `Window::is_minimized()` and stops the sanctioned timer. Skip drag and drop in v1 if the hook is unavailable.
4. File dialogs: `rfd` in a short lived thread. Return the path with `invoke_from_event_loop`. Send `OpenPath`.
5. Command line: a path argument behaves like `OpenPath`. This makes Explorer "Open with" work.
6. Keyboard: Space toggles play. Left and Right seek 15 s. `N` next. `P` previous. `[` and `]` step speed down and up. `V` cycles the visualizer. `?` and `F1` toggle Help. Ctrl+O opens a file. Ctrl+Shift+O opens a folder.
7. Close button: save state and exit. No tray icon.
8. The current speed is global and persisted.

## 11. Background and power rules

Every rule is mandatory.

1. No polling loops. Every wait is a blocking wait with a timeout or an event.
2. Never call `timeBeginPeriod`. It raises timer resolution system wide.
3. No `sleep(1)` loops, no `yield_now` loops, no spin waits longer than the 2 ms flush parks.
4. Animations off. No spinners, no hover transitions, no progress animations. The visualizer is a data display, not an animation. It runs only while the window is visible, playing, and the mode is not Off.
5. Set UI properties only when the value changes. The visualizer redraws only when new peak blocks arrived since the last tick.
6. Producer thread priority `BELOW_NORMAL`. The 3 s ring absorbs scheduling delay.
7. On minimize or occlusion: enable EcoQoS for the process with `SetProcessInformation` and `ProcessPowerThrottling`. Set `ControlMask` and `StateMask` both to `PROCESS_POWER_THROTTLING_EXECUTION_SPEED`. On restore, set `StateMask` to 0. Test 30 minutes of playback with EcoQoS on and confirm zero underruns. If underruns appear, keep EcoQoS off and raise the ring to 5 s.
8. On minimize, after 2 s, call `SetProcessWorkingSetSize(process, usize::MAX, usize::MAX)` once. This lowers the number shown in Task Manager. Committed memory does not change. Budgets are measured without it.
9. No cover art decoding. No tag scanning. The title is the file name without extension.
10. No logging in release builds. Use `eprintln!` behind `debug_assertions` only.
11. Do not create the audio stream at startup. Create it on the first Play.
12. Do not read the whole folder tree. One directory listing, non recursive.
13. If loading system fonts raises startup time or memory above the budget, embed a single font and restrict the font database.

## 12. Windows integration

1. Manifest (embed with `winresource`): PerMonitorV2 DPI awareness, UTF-8 active code page, long path awareness, Windows 10 and 11 in `supportedOS`, and an icon.
2. Console: hidden in release with `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`.
3. Device changes and sleep: the cpal error callback sets an atomic flag. The engine handles it on its next wake. It saves the position, moves to `Paused`, drops the stream, and shows a `Message`. The next Play rebuilds on the current default device. Do not auto resume.
4. Single instance (Phase 5): a named mutex. A second instance sends its path argument through a named pipe to the first, then exits. The first instance runs `OpenPath` and raises its window.
5. Media keys and headset buttons (Phase 5, optional): System Media Transport Controls through `windows`. Handle play, pause, next, previous. Accept it only if it adds 4 MB or less of memory. If it adds more, register the media virtual keys with `RegisterHotKey` instead.
6. File extensions accepted: `mp3 m4a m4b aac flac ogg wav`. Compare case insensitively. Skip hidden and system files.
7. Playlist order: natural sort of file names with `natord`, so "2" sorts before "10".

## 13. Project layout

```
q-pid/
  Cargo.toml
  build.rs
  app.manifest
  assets/icon.ico
  .cargo/config.toml
  ui/main.slint
  src/main.rs            startup, wiring, winit hook
  src/ui.rs              Slint glue, timer, property updates
  src/engine/mod.rs      engine thread, state machine, command handling
  src/engine/decode.rs   symphonia wrapper, seek, duration
  src/engine/dsp.rs      stereo convert, stretch, resample
  src/engine/output.rs   cpal stream, callback, ring, gain ramp
  src/playlist.rs        folder scan, filter, natural sort
  src/store.rs           JSON state, atomic save, pruning
  src/platform/          OS boundary: windows.rs / linux.rs behind cfg
                         (EcoQoS, trim, thread priority, single instance, state path)
  tools/gen_test_audio.py
  tools/bench.py
```

Dependency rule: `ui.rs` may import only the shared types from `engine/mod.rs` (`Command`, `Event`, `Shared`). It must not import decode, dsp or output.

## 14. Build configuration

`Cargo.toml`:

```toml
[profile.release]
opt-level = "s"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true

[profile.release.package."*"]
opt-level = "s"

[profile.release.package.rustfft]
opt-level = 3
```

The app crate is size optimized, and so are the dependencies: the 10 MB exe budget binds before dependency speed — decode plus resample stay well under one core at 1x (measured, see `BENCH.md`). rustfft is the exception at -O3 because it carries the resampler and costs only ~8 KB after LTO.

`.cargo/config.toml`:

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

This removes the Visual C++ redistributable dependency.

`build.rs`:

1. Compile `ui/main.slint` with `slint_build::compile_with_config`. Use style `fluent` and `EmbedResourcesKind::EmbedForSoftwareRenderer`.
2. Embed the manifest and icon with `winresource`.

Select the winit backend with the software renderer explicitly at startup (`slint::BackendSelector`, or set `SLINT_BACKEND` in code before any Slint call).

## 15. Implementation order

Do not start a phase before the previous exit criteria pass.

### Phase 0: Skeleton and measurement

1. `cargo new`, layout from section 13, manifest, build config.
2. Empty Slint window with the final theme tokens and static layout.
3. Write `tools/bench.py` with `psutil`. Launch the exe, sample once per second for 300 s, record CPU percent, private working set (USS), thread count and handle count, write a CSV. Support flags `--scenario` for: idle visible, playing 1x visible, playing 1x minimized, playing 2x minimized, paused over 10 s. Minimize with `ctypes` and `user32.ShowWindow`.
4. Write `tools/gen_test_audio.py`. Generate a 10 minute stereo WAV with the `wave` module (a sweep plus spoken rhythm style pulses is enough). If `ffmpeg` is on PATH, also make mp3, m4a, flac and ogg copies.
5. Exit: window opens, idle visible working set is recorded. Gate: 25 MB or less. If it fails after removing fonts and accessibility overhead, replace the UI layer with raw Win32 plus Direct2D and keep everything else.

### Phase 1: Headless engine

1. Engine thread, `Shared`, ring, cpal callback, symphonia decode, stereo convert.
2. A temporary CLI: `qpid.exe <file>` plays it. Keys on stdin for pause and seek.
3. Implement the flush protocol, gain ramp, pause with stream stop, and the 10 s release.
4. Exit: all formats play. Seek and pause produce no clicks. Position formula is correct at 1x. CPU at 1x within budget 5.

### Phase 2: Speed

1. Stretcher, resampler, bypass at 1x.
2. Speed change through the flush path.
3. Exit: all five speeds. Pitch is unchanged. Speech is intelligible at 2x. Speed change audible within 150 ms. CPU within budget 6. Position stays correct after speed changes.

### Phase 3: Playlist and persistence

1. Folder scan, filter, natural sort, next and prev, end of track handoff.
2. `store.rs`, atomic save, restore rules from section 9.
3. Exit: kill the process while playing, relaunch, and the position is within 30 s of where it stopped. Folder plays through all files in order.

### Phase 4: UI

1. Wire `Command`, `Event` and the 500 ms timer.
2. Buttons, slider, speed control, dialogs, keyboard, command line argument.
3. Exit: every control works. Timer stops while minimized and while paused. Budgets 3 and 4 pass.

### Phase 5: Windows integration

1. EcoQoS on minimize, optional working set trim.
2. Drag and drop through the winit hook.
3. Device change and sleep handling.
4. Single instance with pipe.
5. Media keys through SMTC or `RegisterHotKey`.
6. Exit: unplug headphones while playing and the app pauses cleanly. Sleep and wake works. Second launch with a file path opens it in the first instance.

### Phase 6: Measurement and tuning

1. Run every scenario in `tools/bench.py`. Compare to section 2.
2. Fix failures in this order: threads and wakeups, timers, memory, CPU.
3. If callback wakeups exceed budget, replace cpal with direct WASAPI (`windows` crate) in shared mode, timer driven, with a 200 ms period. Do this only if measurement demands it.
4. Exit: all ten budgets pass. Record the results in `BENCH.md`.

## 16. Acceptance tests

1. Open a folder with 30 mp3 files named `1.mp3` to `30.mp3`. Order is numeric.
2. Open a single m4a file. Its sibling files become the playlist.
3. Seek forward 15 s ten times fast. No crash, no stuck audio, position matches within 0.5 s.
4. Switch speed 20 times while playing. No clicks louder than the fade ramps. No drift in position.
5. Play a 3 hour file. Seek to the middle. Response under 300 ms.
6. Close and reopen the app. Same file, same position, paused.
7. Corrupt file in the middle of a folder. It is skipped with a message.
8. Unicode file names and paths with spaces, accents, and CJK characters.
9. Folder with 500 files opens in under 200 ms.
10. Change the default output device while playing. The app pauses without a crash.
11. Minimize for 30 minutes at 2x. No underruns, budgets 6, 8 and 9 hold.
12. Pause and wait 15 s. Confirm no audio stream, no decoder, no timers, CPU 0.0%.

## 17. Out of scope for v1

Playlist editing, tags and cover art, equalizer, gapless playback, crossfade, m4b chapter markers, Opus, WMA, APE, recursive folder scan, tray icon, network streams, in app volume (use the Windows mixer), sleep timer, library database, installer.

## 18. Rules for the agent

Always:

1. Read docs.rs for each crate before using it.
2. Measure after each phase with `tools/bench.py`.
3. Keep the audio callback free of allocation, locks, I/O and channel sends.
4. Keep `ui.rs` isolated from the engine internals.
5. Preallocate buffers.
6. Report any budget failure with numbers before continuing.

Never:

1. Add a dependency that is not listed here without stating its RAM and binary cost.
2. Add a background thread, timer or periodic task that is not in this document. The visualizer must reuse the sanctioned timer; no new timer or thread for it.
3. Use `timeBeginPeriod`, busy loops, or animations.
4. Memory map audio files.
5. Ship Python, ffmpeg, libmpv or a browser engine.
6. Read audio tags or cover art.
7. Add features from section 17.
