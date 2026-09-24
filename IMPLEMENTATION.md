# Q-pid: Implementation status

Spec: `architecture.md`. This file tracks what is actually built, where it
deviates from the spec, and what still needs verification on a real Windows
machine (this skeleton was authored without access to a Windows/MSVC build
environment — see "Unverified" below before trusting anything here).

## Status

- Phase 0 (skeleton and measurement tooling): **build-verified**, release
  build succeeds, exe size 9.74 MB (budget 10 MB).
- Phase 1 (headless engine): **build-verified**. CLI smoke test passes
  (play, pause, ±15 s seek, resume). Benches run — see `BENCH.md`:
  idle USS 3.35 MB PASS, playing-minimized USS 4.96 MB PASS, CPU 0.315%
  PASS. Thread budget 11 vs 4 **FAIL**. Clicks on pause/seek need human
  listening. m4a test generation fails (ffmpeg encoder).
- Phase 2 (speed): not started. Blocked on thread-budget investigation
  (architecture rule: fix budget failures before adding features).
- Phase 3 (playlist and persistence): not started.
- Phase 4 (UI): partially pre-wired (open file/folder, toggle play, event
  display) but the 500ms timer, seek slider, speed control, keyboard
  shortcuts, and minimize-driven timer suspension are NOT implemented.
  Do not consider Phase 4 started; this is scaffolding only.
- Phase 5 (Windows integration): not started.
- Phase 6 (measurement and tuning): not started.

## Unverified — do these first

This project was scaffolded in a Linux container with no MSVC toolchain, no
Windows, and no access to docs.rs/crates.io to confirm exact API shapes. The
following are best-effort from written knowledge of each crate and MUST be
checked against the actual compiler errors on first build:

1. **`signalsmith-stretch` crate name/availability.** Commented out of
   Cargo.toml entirely for Phase 0/1 (not needed until Phase 2). Run
   `cargo add signalsmith-stretch` yourself and confirm it resolves before
   starting Phase 2. If it fails, architecture.md section 4 note 4 names the
   fallback (SoundTouch bindings or a ~150-line WSOLA stretcher).
2. **`rtrb` API surface** (`src/engine/output.rs`). The constructor name,
   whether `RingBuffer::new(capacity)` returns `(Producer<T>, Consumer<T>)`,
   and the method names `push`/`pop`/`slots`/`is_empty` are assumed from the
   crate's known shape and were not checked live. Run `cargo doc -p rtrb` on
   first build.
3. **Slint API surface**: `slint_build::CompilerConfiguration`,
   `EmbedResourcesKind::EmbedForSoftwareRenderer`, `.with_style(...)`,
   `slint::include_modules!()`, callback naming convention
   (`on_open_file` etc. generated from `callback open-file()` in .slint),
   and property setter naming (`set_track_title` from `in property
   <string> track-title`). These follow Slint's documented conventions as
   of my training data but Slint's API has changed across versions before —
   verify against the actual `slint` crate version `cargo add` resolves.
4. **`winresource` crate name.** This may be `embed-resource` or
   `winres` depending on what's current — "winresource" was used because
   it was named in architecture.md section 4, but confirm it's still the
   maintained/correct crate on crates.io.
5. **`windows` crate feature names** in Cargo.toml
   (`Win32_System_Threading`, etc.) — verify against the installed
   `windows` crate version; these move between major versions.
6. **cpal `StreamConfig`/`SupportedStreamConfig` API**: `.config()`,
   `.sample_format()`, `build_output_stream` signature (closure + error
   callback + optional timeout) — verify against the cpal version resolved.

None of these are exotic guesses; they're the documented shape of each
crate as of when this was written. But "verify feature names on docs.rs"
(architecture.md section 4) applies doubly here since no docs.rs lookup was
possible during authoring.

## Deviations from architecture.md

- **Stereo conversion lives in `decode.rs`, not `dsp.rs`.** The channel-fold
  logic (section 6.2) is applied directly on symphonia's decoded buffer
  inside `Decoder::buffer_to_stereo`, since it needs the buffer's `SignalSpec`
  which is decoder-internal. `dsp.rs` holds only the stretch and resample
  stages, both pass-through stubs in Phase 1. This does not change behavior,
  only which file owns the code section 13 assigns to "dsp.rs".
- **Per-packet allocation not fully eliminated in `decode.rs`.** See the
  code comment on `Decoder::scratch`. `decode_chunk` uses `mem::take` to
  avoid cloning the scratch buffer, but this still allocates a fresh `Vec`
  on the following call. This is on the engine thread, not the real-time
  audio callback, so it does not violate the hard rule in section 5.3, but
  it is a deviation from the letter of section 6.10. Flagged for the Phase 6
  tuning pass rather than solved now, per the "measure, then fix" priority
  order in section 1.
- **UI wiring is ahead of spec in one direction, behind in another.** Open
  file/folder and toggle-play are wired now (Phase 4 scope) because they
  cost nothing to stub in Phase 0 and make the skeleton runnable end to end.
  The 500ms timer, seek slider commit, speed control, and keyboard shortcuts
  are explicitly NOT wired, since they depend on Phase 2/3 engine features
  that don't exist yet — wiring them now would create UI that lies about
  what the engine can do.

## Measurement status

Numbers for the measured budgets are in `BENCH.md`. Remaining before
Phase 1 exit is fully closed:

1. Human listen for clicks on pause/seek (cannot be automated).
2. Budgets 2, 6, 7, 9 not yet measured.
3. Thread budget (8) fails: 10–11 observed vs 4 allowed — investigate
   before starting Phase 2.
