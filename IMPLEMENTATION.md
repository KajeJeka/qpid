# Q-pid Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Build a Windows 11 audio player. Phase 0 delivers the project skeleton and measurement tooling. Phase 1 delivers the headless engine (decode, ring buffer, cpal callback, flush protocol) with a temporary CLI for validation.

**Architecture:** Rust-only runtime. Slint with winit backend and software renderer for UI. Symphonia for decoding, cpal for WASAPI output, signalsmith-stretch for pitch-preserving speed change. Three threads: UI, engine, audio callback.

**Tech Stack:** Rust, Slint, cpal, symphonia, signalsmith-stretch, rubato, rtrb, serde/serde_json, rfd, windows crate, winresource.

**Spec:** architecture.md � every requirement, budget, thread rule, and acceptance test lives there. This plan delegates from that spec.

## Global Constraints

- Rust only at runtime. No Python, tokio, async runtime, logging framework, tag library, database, or tray library.
- Exe size <= 10 MB. Startup <= 300 ms. Private working set <= 25 MB (visible), <= 15 MB (minimized). CPU <= 0.5% at 1x, <= 2% at 2x. Paused > 10 s: 0% CPU, no timers, no stream, no decoder. Threads <= 4. Wakeups <= 2/s excluding audio callback.
- No memory mapping of audio files. No logging in release builds (eprintln! behind debug_assertions only).
- Audio callback: no allocation, no locks, no file I/O, no logging, no channel sends. Atomics only.
- ui.rs may import only Command, Event, Shared from engine/mod.rs.
- opt-level = s, lto = fat, codegen-units = 1, panic = abort, strip = true in release profile.

---

## Task 1: Project Skeleton and Build Configuration

### Steps

- [ ] **Step 1: Initialize the Rust project**
  Run cargo init --name qpid in the project root. Replace src/main.rs with a stub that compiles.

- [ ] **Step 2: Write Cargo.toml**
  Verify feature names on docs.rs for slint and symphonia before writing the file. Architecture.md section 4 specifies slint features std,compat-1-2,backend-winit,renderer-software and symphonia features mp3,aac,isomp4,flac,vorbis,ogg,wav,pcm,alac, but docs.rs may have changed. Include all dependencies with correct feature flags: slint, slint-build, cpal, symphonia, rubato, signalsmith-stretch, rtrb, rfd, serde (derive), serde_json, natord, windows, winresource. Configure [profile.release] with opt-level=s, lto=fat, codegen-units=1, panic=abort, strip=true, opt-level=3 for package glob.

- [ ] **Step 3: Write .cargo/config.toml**
  Add target.x86_64-pc-windows-msvc rustflags with -C target-feature=+crt-static.

- [ ] **Step 4: Write build.rs**
  Call slint_build::compile_with_config with style fluent and EmbedResourcesKind::EmbedForSoftwareRenderer. Embed manifest and icon with winresource.

- [ ] **Step 5: Write app.manifest**
  PerMonitorV2 DPI awareness, UTF-8 active code page, long path awareness, Windows 10/11 supportedOS, icon reference.

- [ ] **Step 6: Create assets/icon.ico**
  Place a 256x256 ICO file.

- [ ] **Step 7: Write ui/main.slint**
  Empty Slint window with theme tokens and fluent style. Static layout placeholders only.

- [ ] **Step 8: Create src/main.rs stub**
  #![cfg_attr(not(debug_assertions), windows_subsystem =  windows)]. Set Slint backend selector to winit/software renderer before any Slint call.

- [ ] **Step 9: Verify build**
  cargo build --release. Confirm exe <= 10 MB.

**Exit criteria:** cargo build --release succeeds, window opens, exe size <= 10 MB.

---

## Task 2: Measurement Tooling

### Steps

- [ ] **Step 1: Write tools/gen_test_audio.py**
  Generate a 10-minute stereo WAV using wave module (sweep plus spoken-rhythm pulses). If ffmpeg is on PATH, also create mp3, m4a, flac, and ogg copies. Output to test_audio/.

- [ ] **Step 2: Write tools/bench.py**
  Use psutil. Accept --scenario flag (idle-visible, playing-1x-visible, playing-1x-minimized, playing-2x-minimized, paused-over-10s). Launch exe, sample once per second for 300s. Record CPU percent, private working set (USS), thread count, handle count. Write CSV. Use ctypes/user32.ShowWindow for minimize.

- [ ] **Step 3: Verify bench.py runs**
  Run python tools/bench.py --scenario idle-visible against the release exe. Record the idle visible working set number in BENCH.md.

**Exit criteria:** Both tools work. Idle visible working set measurable and <= 25 MB.

---

## Task 3: Headless Engine � Core Infrastructure

### Steps

- [ ] **Step 1: Define shared types in src/engine/mod.rs**
  Speed enum (X0_5, X1, X1_25, X1_5, X2), Command enum (all variants from section 5), Event enum (TrackChanged, StateChanged, Message), Shared struct with atomics (base_ms, played_frames, speed_milli, out_rate, duration_ms, target_gain, flush_req, flush_ack, eof, drained). PlayState enum (Idle, Playing, Paused, Ended, Error).

- [ ] **Step 2: Implement engine thread in src/engine/mod.rs**
  engine_thread(rx: Receiver<Command>) owns decoder, stretcher, resampler, playlist, persistence, cpal Stream. Thread priority BELOW_NORMAL. Main loop: Playing = secs until ring fill < LOW_WATER (clamped 100ms-2000ms), Paused = 10s, others = no timeout. On timeout, on_timeout() releases resources after 10s. If Playing and fill < LOW_WATER, refill_until(HIGH_WATER) in chunks of ~4096 frames, try_recv after each chunk.

- [ ] **Step 3: Implement ring buffer and audio callback in src/engine/output.rs**
  Use rtrb. Preallocate 3s of stereo f32 at device rate (~1.1 MB at 48kHz). Water marks: HIGH_WATER 3.0s, LOW_WATER 1.0s, PREROLL 0.25s. cpal callback reads ring, applies gain ramp, counts frames. No allocation/locks/I/O/logging/channel sends. Atomics only. Sets drained=true when ring empty and eof=true.

- [ ] **Step 4: Implement symphonia decode wrapper in src/engine/decode.rs**
  Decoder struct wrapping symphonia. MediaSourceStream with 64KiB buffer. Plain buffered reads only. Convert to stereo f32 (mono duplicated, >2 channels folded). seek(time_ms) using SeekMode::Accurate with SeekTo::Time, decode and discard frames until timestamp. Fall back to Coarse for relative seeks if VBR MP3 without TOC takes >300ms. Duration from codec params, 0 if unknown. Handle decode errors (skip packet) and IO errors (emit Message, skip to next file).

- [ ] **Step 5: Implement stereo convert in src/engine/dsp.rs**
  Convert any channel count to stereo f32. Mono duplicated, >2 channels folded to L/R. Preallocate working Vec buffers once.

- [ ] **Step 6: Implement stretch stub**
  Test `cargo build` with signalsmith-stretch alone first. It needs MSVC build tools via `cc`. If it fails, fall back to SoundTouch bindings or a custom WSOLA stretcher (~150 lines) as architecture.md section 4 note 4 specifies.

- [ ] **Step 7: Implement resample stub**
  Resampler struct using rubato. Skip when file rate matches device rate. Fixed input FFT resampler when rates differ.

- [ ] **Step 8: Wire pipeline in src/main.rs**
  Create the Command channel (`mpsc::channel::<Command>()`), spawn the engine thread with the `Receiver<Command>`, keep the `Sender` in `main`. No UI — headless for now.

**Exit criteria:** Engine thread compiles and runs. Core types and thread loop functional.

---

## Task 4: Headless Engine � Audio Pipeline and Flush Protocol

### Steps

- [ ] **Step 1: Implement flush protocol while playing**
  flush_playing() in engine/mod.rs:
  1. Set target_gain to 0. Callback ramps down over 15ms. Engine waits 20ms.
  2. Set flush_req = true.
  3. Callback pops everything from ring, sets played_frames to 0, sets flush_ack = true. While flush_req true, outputs silence.
  4. Engine waits for flush_ack with 2ms parks, timeout 200ms = device error.
  5. Seek decoder, reset stretcher/resampler, write base_ms and speed_milli, clear eof and drained.
  6. Refill to PREROLL, clear flush_ack, then clear flush_req.
  7. Set target_gain to 1. Callback ramps up over 15ms.

- [ ] **Step 2: Implement flush protocol while paused**
  flush_paused(): drop stream, ring, decoder, DSP. Keep resume position. Update base_ms and played_frames directly. Everything rebuilt on next Play.

- [ ] **Step 3: Implement pause/resume**
  Pause: target_gain=0, wait 20ms, stream.pause(). After 10s paused, release all resources. Resume: stream.play(), target_gain=1, rebuild, seek to resume, refill to PREROLL, start.

- [ ] **Step 4: Implement end-of-track handoff**
  Decoder EOF -> drain stretcher tail, set eof=true, stop refilling. Callback sets drained=true when ring empty and eof true. Engine polls drained every 100ms. On true: mark file done, open next file, refill to PREROLL. After last file: state Ended, stream released after 10s.

- [ ] **Step 5: Implement seek clamping**
  Clamp to 0 and duration-1s. Back 15s at <15s into file clamps to 0. Forward 15s past end goes to next file at 0.

- [ ] **Step 6: Implement temporary CLI**
  Support Q-pid.exe <file> to play. Keys from stdin: p=pause, f=seek forward 15s, b=seek backward 15s, q=quit. Use engine thread and Command channel. Print position updates periodically.

**Exit criteria:** All formats play. Seek and pause produce no clicks. Position formula correct at 1x. CPU at 1x within budget 5 (0.5% of one core).

---

## Task 5: Measurement and Validation

### Steps

- [ ] **Step 1: Run all bench scenarios**
  Build release exe. Run all five scenarios. Record CSV results.

- [ ] **Step 2: Compare against all nine budgets**
  Check section 2 budgets. Fix failures before proceeding.

- [ ] **Step 3: Validate position formula**
  Verify position_ms = base_ms + played_frames * 1000 * speed / out_rate within ~100ms.

- [ ] **Step 4: Validate no clicks on seek/pause**
  Verify gain ramp timing and flush protocol ordering.

- [ ] **Step 5: Record results**
  Write findings to BENCH.md. Note that Phase 1 CPU numbers are engine-only (no UI timer). Must be re-measured after Phase 4 when the UI timer adds cost.

**Exit criteria:** All nine budgets pass or documented failures. Phase 0 gate (25 MB idle visible) passes.

---

## Task 6: Commit and Document

- [ ] **Step 1: Initialize git repository**
  git init, add all files, create initial commit.

- [ ] **Step 2: Commit after each task**
  Commit at the end of Tasks 1, 2, 3, and 4 so a failed later task does not put earlier work at risk.

- [ ] **Step 3: Finalize IMPLEMENTATION.md**
  Update with actual file paths, deviations, and measurement results.

---

## Spec Coverage Check

| Architecture Section | Covered By Task |
|---|---|
| Section 1 (Goal) | All tasks |
| Section 2 (Budgets) | Task 5 |
| Section 3 (Decisions) | All tasks |
| Section 4 (Dependencies) | Task 1 Step 2 |
| Section 5 (Threads) | Task 3 Step 1 |
| Section 6 (Audio Pipeline) | Tasks 3-4 |
| Section 7 (Position/Seek/Speed) | Task 4 |
| Section 8 (Paused/Idle) | Task 4 Steps 2-3 |
| Section 9 (Persistence) | Phase 3 |
| Section 10 (UI) | Phase 4 |
| Section 11-12 (Windows) | Phase 5 |
| Section 13 (Layout) | Task 1 |
| Section 14 (Build Config) | Task 1 Steps 2-5 |
| Section 15 (Order) | This plan follows Phase 0-1 |
| Section 16 (Acceptance) | Task 5 |
| Section 17-18 (Rules) | All tasks |

## Next Phases

Phase 2: Speed control (signalsmith-stretch, resampler, flush on speed change)
Phase 3: Playlist and persistence
Phase 4: UI
Phase 5: Windows integration
Phase 6: Full measurement
