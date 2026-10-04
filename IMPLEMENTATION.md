# Q-pid: Implementation status

Spec: `architecture.md`. This file tracks what is actually built, where it
deviates from the spec, and what still needs verification on a real Windows
machine (this skeleton was authored without access to a Windows/MSVC build
environment — see "Unverified" below before trusting anything here).

## Status

- Phase 0 (skeleton and measurement tooling): **build-verified**, release
  build succeeds, exe size 9.89 MB (budget 10 MB; was 9.74 MB before the
  resampler linked rustfft — see BENCH.md).
- Phase 1 (headless engine): **build-verified**. CLI smoke test passes
  (play, pause, ±15 s seek, resume, end-of-track). The 44.1/48 kHz
  rate-mismatch pitch bug is fixed: real `rubato::FftFixedIn` resampler in
  `src/engine/dsp.rs` with unit tests (TDD), wired through decode → resample
  → ring, reset on every flush, EOF tail drained. Ring headroom drop fixed
  (`HIGH_WATER` 3.0 → 2.9 s; was losing ~11 ms of audio per refill cycle).
  Benches — see `BENCH.md`: exe 9.89 MB PASS, startup 141 ms PASS,
  minimized USS 5.39 MB PASS, CPU 0.42% PASS, paused >10 s 0.00% PASS,
  threads 11 ≤ revised budget 12 PASS. The §8.2 pause release now drops
  stream, ring, decoder and resampler (whole context) and resume reopens
  the file at the stored position — budget 7's "no decoder open, no
  timers" clauses were failing before this. Clicks on pause/seek
  and the resulting pitch need human listening. m4a test generation fails
  (ffmpeg encoder).
- Phase 2 (speed): **build-verified**. `signalsmith-stretch` resolved on
  crates.io (v0.1.3) but failed to build (its `build.rs` runs bindgen
  unconditionally and libclang is not installed), so the §4 note 4
  fallback is implemented: a hand-written WSOLA stretcher in
  `src/engine/dsp.rs` (TDD — 8 unit tests: bypass identity, output-length
  ratio at 0.5/1.25/1.5/2x, Goertzel pitch preservation, reset
  determinism). Wired decode → stretch → resample → ring; 1x is a pure
  copy (§6.6 rule 6); `SetSpeed` is a flush-to-position (§7.1) capturing
  the position under the old speed first; stretcher flushes before the
  resampler at EOF; resets on every flush/seek. CLI `s` cycles the five
  speeds; the Slint speed row is wired. Budget 6: 1.65–1.88% ≤ 2% (3 runs,
  BENCH.md). Exe 9.90 MB (budget 10). Human checks pending: pitch by ear
  at each speed, clicks on speed change (§16 test 4). Thread budget
  revised to 12 in architecture.md §2.8 with the isolation evidence in
  BENCH.md; Slint and cpal stay as-is.
- Phase 3 (playlist and persistence): **build-verified**. Playlist
  (`src/playlist.rs`: extension filter, hidden/system skip, natural sort,
  `index_of`) with real Next/Prev and EOF handoff through `advance_to`
  (stream reuse when the next file's rate matches, unconditional resampler
  rebuild when it does not — both directions probe-verified). Store
  (`src/store.rs`: §9 schema, atomic temp+rename save) with all 8 §9
  rule-5 triggers wired through `save_now` plus the 30 s while-playing
  checkpoint (rides the existing wake — no new thread/timer), and
  `RestoreSession` restoring last folder/file/position Paused with no
  autoplay (rules 1–3; probes A/B/C pass: fresh restore 8022 ms, no
  history → clean start, corrupt `state.json` → clean start). Probes:
  mixed-rate advance PASS both directions (wrong-config excluded by
  1.2–3.0 s timing margins), acceptance 6 kill/relaunch PASS (killed at
  ~65 s, relaunch restored `test.mp3` at `position_ms = 60012` ∈
  [35000, 70000], `StateChanged(Paused)` only — no `Playing`). Budgets
  re-checked: exe size **10,443,264 B** ≤ 10,485,760 PASS (Phase 3 start
  was 10,421,248 B; +22,016 B for serde/store); budget 6 spot-check
  **1.63%** ≤ 2% PASS (within the 1.57–1.88% Phase 2 band). 15/15 tests,
  7 release warnings (baseline). Trigger table:

  | # | §9 rule-5 trigger | Site(s) — real `grep 'save_now'` line numbers | reason |
  |---|---|---|---|
  | 1 | Pause (incl. TogglePlay→pause) | `src/engine/mod.rs:1069` | `pause` |
  | 2 | Next | `mod.rs:467` (Next arm) → `mod.rs:980` inside `advance_to` (`mod.rs:963`); EOF handoff entry `mod.rs:311` (ramp=false) also saves here | `next/prev` |
  | 3 | Prev | `mod.rs:486` (position > 5 s restart) + `mod.rs:488` (index step) → `mod.rs:980` inside `advance_to` | `prev restart` / `next/prev` |
  | 4 | Open of another path | `mod.rs:407` (pre-open, old file's position) | `open of another path` |
  | 5 | Speed change | `mod.rs:516` — after `speed_milli` store (`mod.rs:509`), so position and new speed both land | `speed change` |
  | 6 | Exit | engine: `mod.rs:392` (Shutdown) + `mod.rs:275` (channel dropped); main: `src/main.rs:135-136` (CLI `q`/EOF: Shutdown then `join()`) and `src/main.rs:154-155` (UI: Shutdown then `join()`) | `app exit` |
  | 7 | Error stop (2 sites) | `mod.rs:706` (file-open failure) + `mod.rs:1042` (`advance_to` no-playable-file arm) | `error stop` |
  | 8 | 30 s while playing | `mod.rs:292` (checkpoint in the wake path, only when position changed) | `30s checkpoint` |
- Phase 4 (UI): **build-verified**. The 500 ms position timer runs only
  while playing && window visible (winit `CustomApplicationHandler` hook in
  `src/winit_hook.rs` — the only unstable-API file; slint pinned `=1.18.1`),
  stopped by pause/minimize/occlusion (probe: 0 ticks over 30 s in each
  condition). Seek slider with drag-vs-commit and disabled-at-duration-0
  (custom `TouchArea` slider — the std Slider cannot expose drag state).
  Keyboard: Space/←/→/N/P/[/]/Ctrl+O/Ctrl+Shift+O via one FocusScope (probe:
  Space, arrows, Ctrl+O, N observed; `[`/`]` and Ctrl+Shift+O human-checked).
  Ended now releases stream+decoder after 10 s like pause (probe: release
  line, handles drop, 0 CPU, ~0 wakes/s) and Play/TogglePlay restarts the
  file from 0 after release. Budgets: 1 10,418,176 B PASS, 3 5.48 MB PASS,
  4 5.86 MB PASS, 5 UNSTABLE (0.44–0.98%, scheduler variance, NOT a Phase 4
  contributor — see BENCH.md note), 7 paused + ended PASS, 9 0.53/s PASS
  (self-instrumented engine counter — method in BENCH.md). Tests 31/31,
  warnings ≤ baseline. Known gap (pre-existing, spec-silent): after
  RestoreSession the engine may play at the saved speed while the UI speed
  row still shows 1x — there is no speed event; fixed by a segment click.
  Human checklist pending (spec §6/§7).
- Phase 5 (Windows integration): not started.
- Phase 6 (measurement and tuning): not started.

## Unverified — do these first

This project was scaffolded in a Linux container with no MSVC toolchain, no
Windows, and no access to docs.rs/crates.io to confirm exact API shapes. The
following are best-effort from written knowledge of each crate and MUST be
checked against the actual compiler errors on first build:

1. ~~**`signalsmith-stretch` crate name/availability.~~ RESOLVED:** the
   crate resolves (v0.1.3) but does not build here — its `build.rs` runs
   bindgen unconditionally and libclang is absent (only MSVC/cc
   available), with no feature flag to skip it. Dependency removed from
   Cargo.toml; the §4 note 4 fallback (hand-written WSOLA) is implemented
   instead. Do not re-add it without libclang.
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
  The speed row is wired as of Phase 2 (the engine's SetSpeed is real). The
  500ms timer, seek slider commit, and keyboard shortcuts are explicitly
  NOT wired, since they depend on features that don't exist yet — wiring
  them now would create UI that lies about what the engine can do.
- **Stretcher is a hand-written WSOLA, not `signalsmith-stretch` (§4.6
  first choice).** The crate's build requires libclang (bindgen in its
  build.rs, unconditional), which is not installed. §4 note 4 explicitly
  sanctions "write a WSOLA stretcher (about 150 lines)" as the fallback;
  that is what `src/engine/dsp.rs` implements.
- **§6.9 packet-level decode-error skip remains deferred (Phase 3).** An
  unreadable file at open forwards to the next playable file inside
  `advance_to` (Error only when none is left), but a decode error on an
  already-playing file still just stops the current `decode_burst` refill
  and the next refill retries (Phase 1 behavior) — "skip the packet" is
  not implemented. The `decode_burst` Err-branch comment in
  `src/engine/mod.rs` now says exactly that (was stale, claimed the
  packet skip was landing in Phase 3).

## Measurement status

Numbers for the measured budgets are in `BENCH.md`. Remaining before
Phase 1 exit is fully closed:

1. Human listen for clicks on pause/seek (cannot be automated), plus
   Phase 2 listening: pitch at each speed and clicks on speed change
   (§16 test 4).
2. Budget 9 measured for Phase 4: 0.53/s ≤ 2 PASS (0.529/s primary,
   0.533/s cross-check), method + limitations in BENCH.md.
3. Budgets 1–8 PASS except budget 5, recorded **UNSTABLE
   (0.44–0.98%)** by human ruling — scheduler variance on this machine,
   not a Phase 4 contributor (interleaved A/B, see the BENCH.md
   budget-5 note). Thread budget (8) was measured FAIL at 11 vs 4;
   isolation recorded in `BENCH.md` (Slint floor 9, CLI floor 7) and the
   budget was revised to **12** in architecture.md §2.8.

Phase 3 verification status: all three probes (mixed-rate advance,
restore A/B/C, acceptance 6 kill/relaunch) are automated and PASS — see
`BENCH.md` "Phase 3 probes" and the task reports. **Acceptance 6:**
killed at ~65 s while playing → relaunch with no path arg restored
`test.mp3` at `position_ms = 60012` (bound [35000, 70000]; last
checkpoint at 60 s, 30 s max loss per §9 rule 7), `StateChanged(Paused)`
emitted, no `StateChanged(Playing)`. No new human-only checks for
Phase 3: §15.3 has no listening criteria for playlist/persistence; the
Phase 1/2 listening items above are unchanged.
