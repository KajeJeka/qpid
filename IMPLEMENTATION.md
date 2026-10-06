# Q-pid: Implementation status

Spec: `architecture.md`. This file tracks what is actually built, where it
deviates from the spec, and what still needs verification (this skeleton was
authored in a Linux container with no Windows/MSVC build environment; every
build-time API guess made then was settled by the Phases 1–5 builds — see
"Unverified" below. Phase 6 (final measurement) is complete — results in
`BENCH.md` "Phase 6 final table — v1.0.0 (2026-10-05)". What is left open
is the human listening checks, delivered as the Phase 6 close checklist in
`BENCH.md`).

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
  | 1 | Pause (incl. TogglePlay→pause) | `src/engine/mod.rs:1212` | `pause` |
  | 2 | Next | `mod.rs:540` (Next arm, no direct save) → `mod.rs:1062` inside `advance_to` (`mod.rs:1045`); EOF handoff entry `mod.rs:376` (ramp=false) also saves here | `next/prev` |
  | 3 | Prev | `mod.rs:561` (position > 5 s restart) + `mod.rs:563` (index step) → `mod.rs:1062` inside `advance_to` | `prev restart` / `next/prev` |
  | 4 | Open of another path | `mod.rs:476` (pre-open, old file's position) | `open of another path` |
  | 5 | Speed change | `mod.rs:591` — after `speed_milli` store (`mod.rs:584`), so position and new speed both land | `speed change` |
  | 6 | Exit | engine: `mod.rs:461` (Shutdown) + `mod.rs:324` (channel dropped); main: `src/main.rs:156-157` (CLI `q`/EOF: Shutdown then `join()`) and `src/main.rs:196-197` (UI: Shutdown then `join()`) | `app exit` |
  | 7 | Error stop (2 sites) | `mod.rs:788` (file-open failure) + `mod.rs:1124` (`advance_to` no-playable-file arm) | `error stop` |
  | 8 | 30 s while playing | `mod.rs:357` (checkpoint in the wake path, only when position changed) | `30s checkpoint` |
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
- Phase 5 (Windows integration): **build-verified, probes PASS; all five
  human gates passed 2026-10-05** (unplug, sleep/wake, drag-drop, physical
  media keys, visual window raise — reported passing by the user, recorded
  in the BENCH.md §15.6 exit-criteria table; the sixth row there,
  second-launch handoff, is automated PASS).
  - *EcoQoS + trim* (now `src/platform/windows.rs`: `set_ecoqos` via
    `SetProcessInformation`/`ProcessPowerThrottling`, `trim_working_set`
    via `SetProcessWorkingSetSize`) are driven from the existing
    visibility sink (`src/ui.rs:320-322`): `set_ecoqos(!visible)` on every
    transition, trim only when hidden and `QPID_NO_TRIM` is unset (the
    measurement escape hatch for budget 4). **Why no timer (§18):** §18
    forbids background threads/timers/periodic tasks not in the document,
    and the visibility event already fires on exactly the transitions that
    matter — so the wiring is inline on the event-loop thread, nothing
    polls, and there is no wake when hidden beyond the engine's own.
    `lower_thread_priority` moved into the platform file (same behavior;
    now `src/platform/windows.rs`).
    Soak: **129.6 min minimized playing** (debug, trim armed),
    `underruns=0` on all 581 wake-log lines, `[eco] EcoQoS on` after
    `visible=false` (BENCH.md "Phase 5 probes"). Budget 4: **4.97 MB**
    no-trim official / 2.10 MB trim-on info.
  - *Device/sleep handling* (§12.3): cpal's `err_fn` stores
    `Shared.device_error` (one relaxed store, no logging, no allocation,
    `src/engine/output.rs`); the engine checks it once per existing wake —
    one relaxed atomic load (`src/engine/mod.rs:338`) — and runs
    `device_lost()` (`mod.rs:1176`): while Playing it calls `pause()`
    (save + fold + `StateChanged(Paused)`), then drops the context into
    `released_path` so resume rebuilds on the current default device, and
    sends the status message "Output device changed — press Play to
    resume". No new thread/timer/channel — the flag rides the wake that
    already exists. Probe: `StateChanged(Paused)` + the Message in 3/3
    runs, resume position within ±1.5 s of the pre-error position (one
    run spent its blind `p` during the ≤2 s detection window and never
    rebuilt — a probe race, not a product path; see task-2 report).
  - *Pause-position fold* (`mod.rs:1219-1221`): `pause()` now folds the
    live position into `base_ms` and zeroes `played_frames`. Rationale:
    `resume()`'s released branch reads `base_ms` only, so without the fold
    a pause → 10 s release → resume restarted at the **last seek point**
    instead of the pause point (the store was already correct — only the
    in-memory base was stale). Probe before/after: P1 38292 ms → resume
    P2 39264 ms; pre-fix would be ≈32372 ms (the last seek), off by the
    ~6 s the fold preserves. The fold sits immediately after `save_now`,
    so the store and `base_ms` record the same value.
  - *Drag-and-drop* (§15 Phase 5 item 2 / §10.3 window events):
    `src/winit_hook.rs` gained a second
    thread-local sink (`set_drop_sink`, :30) and dispatches
    `WindowEvent::DroppedFile` inline on the event-loop thread (:99) to
    the closure registered in `wire()` (`src/ui.rs:194`), which sends the
    existing `Command::OpenPath`. No winit type appears in `ui.rs`, events
    still propagate (nothing swallowed), no new thread/timer. Gate 4 holds:
    `winit_030` only in `src/winit_hook.rs`. End-to-end drop was
    human-confirmed 2026-10-05; prior verification was compile-level only.
  - *Single instance* (§12.5 rule 4): `claim_instance()`
    (`src/platform/windows.rs:79`) creates `Global\qpid-instance` and tests
    `ERROR_ALREADY_EXISTS`; it is the **first statement of `run_ui`**
    (`src/main.rs:163`), i.e. before the winit hook and the engine — a
    second instance creates no window, no engine thread and never writes a
    state file (its path is only serialized into the payload). On loss it
    writes UTF-16 (empty = raise-only) to the machine-global
    `\\.\pipe\qpid-open` with 10 × 50 ms retries (`send_to_first_instance`,
    `src/platform/windows.rs:111`) and returns. The first instance runs the
    **sanctioned 4th thread** (`qpid-pipe`, `thread::Builder`) looping
    `ConnectNamedPipe` →
    `OpenPath`/raise → `DisconnectNamedPipe` (`start_instance_listener`,
    `src/platform/windows.rs:140`), then `raise_window()` (`src/platform/windows.rs:202`:
    `EnumWindows` on own PID,
    `SW_RESTORE` + `SetForegroundWindow` — best-effort, the foreground lock
    can refuse). `--cli` is exempt. Probe: **inst2 exit 51 ms**,
    `[store] save (open of another path)` in inst1 stderr, exactly one
    qpid left. `Global\` = one player per machine by design. The three
    features the compiler demanded (`Win32_Security`,
    `Win32_Storage_FileSystem`, `Win32_System_IO`) cost **0 B** after LTO.
  - *Media keys* (§12.5 rule 5, optional): `RegisterHotKey` for
    `VK_MEDIA_PLAY_PAUSE` / `NEXT_TRACK` / `PREV_TRACK` (ids 1–3) on a
    message-only window `qpid-media-keys` created on the main thread
    (`src/platform/windows.rs:285`), `WM_HOTKEY` → the existing
    `TogglePlay`/`Next`/`Prev` commands; started after `spawn_engine`
    (`main.rs:169`), `stop_media_keys()` (`src/platform/windows.rs:331`) after
    `window.run()` unregisters all three and destroys the window
    (`main.rs:194`). Every failure path returns silently (the feature is
    optional); no new thread, no timer. Size: **+512 B** including the
    `Win32_UI_Input_KeyboardAndMouse` feature (10,451,456 → 10,451,968 B
    at Task 5's build, ≤ 10,485,760 PASS). Probe: window
    present, all
    three keys held (conflict probe → 1409, bracketed by baseline/post-exit
    successes), `WM_HOTKEY` ids accepted, released after exit.
  - **Documented limitation:** the `underruns > 0` path is asserted by
    code review + counter wiring but **not exercised end-to-end** — there
    is no deterministic underrun generator (ring is 2.9 s, max engine
    sleep 2 s; only a stalled engine thread could starve it). Soak and
    normal playback observed `underruns=0`.
  - Gates at HEAD (branch-review cycle, 2026-10-05): `cargo test` 31/31,
    release warnings 7 (baseline), exe **10,452,992 B** (margin **32,768 B**
    ≤ 10,485,760), threads 11 idle / 12 playing (**AT CAP 12/12**), budgets
    5–7 not re-run (Phase 5 adds no steady-state CPU — one INFO
    `playing-2x-minimized` spot-check at 1.65%; see BENCH.md).
- Phase 6 (final measurement): **complete — all nine budgets re-measured on
  the final build, results recorded** (BENCH.md "Phase 6 final table —
  v1.0.0 (2026-10-05)"; evidence in
  `.superpowers/sdd/2026-10-05-phase6-final-measurement/` task reports).
  Budgets 1/2/3/4/6/7/9 **PASS**; budget 5 = **permanent ruling, 0.62%
  median** (UNSTABLE — scheduler/heterogeneous-core variance, not carried to
  1.1); budget 8 = **AT CAP (12/12, zero headroom)**. Acceptance §16.1–16.12
  all PASS (§16.6 rides the Phase 3 evidence, §16.12 = the budget 7
  scenario); security baseline + rule-10/gate-4 audits PASS. **One src fix
  landed in Phase 6:** the §7.6 timing rule (commit `18aaf39` — Coarse
  fallback for relative seeks after a >300 ms seek; 3 new unit tests,
  `cargo test` 31 → 34; warm-up stalls >300 ms 4 → 0); every Lane B number
  above is from the post-fix build, size unchanged 10,452,992 B, warnings 7.
  Outstanding: the human listening checklist (BENCH.md, "Phase 6 close").
- Phase 7 (platform abstraction): **done** (`6382c1b`). `src/winapi.rs`
  moved to `src/platform/windows.rs`; `src/platform/mod.rs` re-exports a
  single `crate::platform` path per OS behind `#[cfg]` (no trait, no dyn —
  one small binary). `set_ecoqos` renamed `set_background_power_mode`
  (neutral naming); `winresource` target-gated so it never enters the
  Linux dependency graph (verified with `cargo metadata
  --filter-platform`, ruling R-L5 — the literal `cargo tree` still shows
  host-side build-dep chains on Windows, and the Linux CI job is the
  real gate).
- Phase 8/9 (Linux compile via CI + Linux implementation): **done**.
  `ci.yml`'s `linux` job (`671686e`) needs
  `libasound2-dev libfontconfig1-dev pkg-config` (fontique/fontdb link
  fontconfig — run 1 failed exactly there, fixed in `5540e10`) and runs
  `cargo test` + a release build that **reports** size, never gates it
  (linux-port.md §15). `src/platform/linux.rs` (`9d4a77b`): XDG state
  path `~/.local/state/qpid/state.json` (`$XDG_STATE_HOME` when set),
  single instance over `$XDG_RUNTIME_DIR/qpid.sock`, dotfile
  `hidden_flags`, and documented no-ops for EcoQoS/trim/thread
  priority/media keys/window raise (raise is a no-op because Wayland
  refuses force-focus; `OpenPath` is still delivered). State-key
  lowercasing is **unconditional on Linux too** — two names differing
  only by case share an entry (rare on Linux, recorded not "fixed").
  Test counts: Windows `cargo test` **35**, Linux **38** (35 shared + 3
  `platform::linux`). CI run 2: Windows GREEN (warning locations ≤ 7,
  exe ≤ 10,485,760 B) and Linux 37/38 — the one red was the `rule2`
  fixture's hardcoded `C:\Music\*.mp3` (backslash is not a separator on
  unix), fixed to native paths in `44048e1`; run 3 is the green-gate run
  (38/38 expected).
- Phase 10 (budget measurement on a physical Linux box): **deferred —
  pending the friend's machine.** `tools/bench.py` gained X11/xdotool
  minimize support (`2114877`; Wayland stays a documented gap), but no
  Linux number exists yet — see BENCH.md "Linux budgets — NOT YET
  MEASURED". Never inherit the Windows numbers.
- Phase 11 (license, desktop assets, three package formats): **done**
  (`2114877`, `76110a7`). MIT `LICENSE` + `license = "MIT"` in
  `Cargo.toml`; `assets/icon.png` (256 px, extracted from
  `assets/icon.ico` with Pillow); `assets/qpid.desktop`;
  `packaging/aur` (PKGBUILD template, checksum rendered by CI, artifact
  name **`aur`**); `packaging/flatpak` (app-id
  `io.github.KajeJeka.qpid`, self-hosted bundle attached to GitHub
  Releases, **not Flathub**); `packaging/appimage/build.sh` (linuxdeploy
  in an `ubuntu:22.04` container, glibc baseline).
- Phase 12 (CI + release workflows): **done** (`671686e`, `44048e1`).
  `ci.yml` = windows (test + warning/size gates) · linux (test + build) ·
  aur (PKGBUILD syntax + ref-aware render validation). `release.yml` =
  windows, appimage-x86_64, appimage-aarch64, flatpak, aur, and a
  tag-gated `publish` that attaches all five artifacts; the release
  notes state Linux sizes are reported per job with no Linux size
  budget.

## Unverified — all resolved by the Phases 1–5 builds

This project was scaffolded in a Linux container with no MSVC toolchain, no
Windows, and no access to docs.rs/crates.io to confirm exact API shapes, so
each item below was a best-effort guess that had to be checked against real
compiler errors. Every one has since been settled by a real build:

1. ~~**`signalsmith-stretch` crate name/availability.~~ RESOLVED:** the
   crate resolves (v0.1.3) but does not build here — its `build.rs` runs
   bindgen unconditionally and libclang is absent (only MSVC/cc
   available), with no feature flag to skip it. Dependency removed from
   Cargo.toml; the §4 note 4 fallback (hand-written WSOLA) is implemented
   instead. Do not re-add it without libclang.
2. **`rtrb` API surface** (`src/engine/output.rs`) — **RESOLVED (Phase 1):**
   `RingBuffer::new` returning `(Producer, Consumer)` and the
   `push`/`pop`/`slots`/`is_empty` names all matched the real crate;
   `rtrb = "0.3"` builds at HEAD (`Cargo.toml:16`) and the ring is driven
   live by every playback probe since Phase 1 (`cargo test` 31/31 at HEAD —
   BENCH.md §15.6).
3. **Slint API surface** (`slint_build::CompilerConfiguration`,
   `EmbedResourcesKind::EmbedForSoftwareRenderer`, `.with_style(...)`,
   `slint::include_modules!()`, callback and property-setter naming) —
   **RESOLVED (Phase 4):** all of it compiled and ran against the pinned
   `slint = "=1.18.1"` (`Cargo.toml:12`); the Phase 4 UI probes (500 ms
   timer stop conditions, seek slider, keyboard shortcuts) passed.
4. **`winresource` crate name** — **RESOLVED:** it is the correct crate;
   `winresource = "0.1"` (`Cargo.toml:41`) has resolved and linked through
   every release build since Phase 0 (exe-size rows in BENCH.md).
5. **`windows` crate feature names** — **RESOLVED:** the `windows = 0.58`
   feature list (`Cargo.toml:23-37`) compiles at HEAD, and Phase 5's
   additions (`Win32_Security`, `Win32_Storage_FileSystem`,
   `Win32_System_IO`, `Win32_UI_Input_KeyboardAndMouse`) all bound without
   a rename.
6. **cpal `StreamConfig`/`SupportedStreamConfig` API** (`.config()`,
   `.sample_format()`, `build_output_stream` closure signature) —
   **RESOLVED (Phase 1):** `cpal = "0.15"` (`Cargo.toml:13`) builds against
   `src/engine/output.rs`, and output is probe-verified end to end
   (playback since Phase 1; device-error path 3/3 in Phase 5).

All items above are settled against the crate versions resolved at HEAD;
no re-verification is needed before 1.0.

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
  it is a deviation from the letter of section 6.10. Phase 6 ruling
  (2026-10-05): **intentionally not addressed** — no budget fails because
  of it, so it stays a documented deviation (recorded, not fixed).
- **UI wiring: ahead of spec in Phase 0, complete as of Phase 4.** Open
  file/folder and toggle-play were stubbed in Phase 0 (they cost nothing
  to stub and make the skeleton runnable end to end), the speed row is
  wired as of Phase 2 (the engine's SetSpeed is real), and the 500 ms
  timer, seek slider commit and keyboard shortcuts were wired in Phase 4
  (see the Phase 4 Status bullet).
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
  packet skip was landing in Phase 3). Phase 6 ruling (2026-10-05):
  **intentionally not addressed in Phase 6** — no budget fails because of
  it; recorded as a permanent deviation, not fixed.

## Deviations from the Linux port spec (linux-port.md)

- **`src/playlist.rs` scan was Windows-only (port spec §2 wrong).** The
  folder scan read the hidden bit through
  `std::os::windows::fs::MetadataExt`, which does not exist on unix —
  the spec assumed a cross-platform scan. **Fixed:** the check moved
  behind `platform::hidden_flags(path) -> u32` (`windows.rs` returns the
  file attributes, `linux.rs` returns the dotfile flag with the same
  `FILE_ATTRIBUTE_HIDDEN` value), so the shared `is_playable()` predicate
  in `playlist.rs` is byte-for-byte the same on both platforms.
- **`assets/icon.png` did not exist (port spec §10 wrong).** The spec
  listed it as already present. **Fixed** by extracting the 256 px image
  out of `assets/icon.ico` with Pillow (extraction, not a new
  commission); `assets/qpid.desktop` and the AUR/Flatpak/AppImage
  packaging all consume it.
- **Test fixture, not product code:** `rule2_pick_treats_missing_entries_as_not_done`
  built keys from hardcoded `C:\Music\*.mp3` strings; converted to native
  `PathBuf` joins (production `playlist::scan` always yielded native
  paths — the bug was in the test only). Landed inside `44048e1`.

## Measurement status

Numbers for the measured budgets are in `BENCH.md`. Measurement items
(phases 1–4, status; Phase 6 close status follows):

1. Human listen for clicks on pause/seek (cannot be automated), plus
   Phase 2 listening: pitch at each speed and clicks on speed change
   (§16 test 4) — **delivered as the "Human listening checklist (Phase 6
   close)" section in `BENCH.md`**; results still to be reported.
2. Budget 9 measured for Phase 4: 0.53/s ≤ 2 PASS (0.529/s primary,
   0.533/s cross-check), method + limitations in BENCH.md.
3. Budgets 1–8 PASS except budget 5, recorded **UNSTABLE
   (0.44–0.98%)** by human ruling — scheduler variance on this machine,
   not a Phase 4 contributor (interleaved A/B, see the BENCH.md
   budget-5 note). Thread budget (8) was measured FAIL at 11 vs 4;
   isolation recorded in `BENCH.md` (Slint floor 9, CLI floor 7) and the
   budget was revised to **12** in architecture.md §2.8.

**Phase 6 close status (2026-10-05/06, final build):**

- All nine budgets re-measured — `BENCH.md` "Phase 6 final table —
  v1.0.0 (2026-10-05)". Budgets 1/2/3/4/6/7/9 PASS (1: 10,452,992 B;
  2: 20.5 ms warm median; 3: 5.43 MB; 4: 5.99 MB official / 3.20 MB
  trim info; 6: 1.68%; 7: 0.00%; 9: 0.55/s + soak 0.281/s).
- **Budget 5 final verdict = permanent ruling: 0.62% median** of 5 ×
  300 s P-core-pinned runs (`--affinity --repeat`, harness fix done) >
  0.5% gate → **UNSTABLE, permanently attributed to
  scheduler/heterogeneous-core variance; not carried forward to 1.1 as an
  open question** (ruling + citations in BENCH.md).
- **Budget 8 = AT CAP (12/12, zero headroom)** — 11 idle / 12 playing /
  12 soak max; spawn inventory identical to `40b0719`, no new thread.
- **§7.6 was implemented in Phase 6** (this is the only src change of the
  phase): commit `18aaf39`, Coarse fallback for relative seeks after a
  >300 ms seek; 3 unit tests added (`cargo test` 31 → 34); §16.5 warm-up
  stalls >300 ms went 4 → 0. All Lane B numbers above are post-fix.

Phase 3 verification status: all three probes (mixed-rate advance,
restore A/B/C, acceptance 6 kill/relaunch) are automated and PASS — see
`BENCH.md` "Phase 3 probes" and the task reports. **Acceptance 6:**
killed at ~65 s while playing → relaunch with no path arg restored
`test.mp3` at `position_ms = 60012` (bound [35000, 70000]; last
checkpoint at 60 s, 30 s max loss per §9 rule 7), `StateChanged(Paused)`
emitted, no `StateChanged(Playing)`. No new human-only checks for
Phase 3: §15.3 has no listening criteria for playlist/persistence; the
Phase 1/2 listening items above are unchanged.
