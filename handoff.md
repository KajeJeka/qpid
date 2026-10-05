# handoff.md — q-pid session handoff (Phase 5, 2026-10-05)

For the next agent working on this repo. Read this first, then
`architecture.md` (binding), `IMPLEMENTATION.md` (status), `BENCH.md`
(§15.6 exit table at the end), and `docs/USER_GUIDE.md`. This handoff
covers **Phase 5 only**; earlier handoffs and SDD ledgers (Phases 0–4)
are in git history and `.superpowers/sdd/`.

## What the project is

**q-pid** — a small Windows audio player (Rust + Slint 1.18.1, software
renderer, no GPU deps). Folder/file playlist player with seek bar, ±15 s
skips, five playback speeds (WSOLA), session restore, autosave
checkpoints, strict resource budgets (exe ≤ 10 MB, minimized CPU/RAM,
≤ 2 background wakeups/s), plus a `--cli` headless mode for tests/benches.

## What was done this session (Phase 5: Windows integration)

1. **6 tasks + whole-branch review on `master`, clean tree, 11 commits:**
   `dbf8f1e` plan → `207f378` T1 → `3b53215` T3 → `ba025bc` T2 →
   `317c323` T4 → `284f6e0` T4-fix → `741ce43` T5 → `e91c5bc` T6-docs →
   `40b0719` branch-review-fixes → `9e1ae52` human-gate results.
2. **T1** EcoQoS + working-set trim on hide (no timer — rides the existing
   visibility sink), underrun counter, soak launched. Soak later read:
   **129.6 min ≥ 30, `underruns=0` on all 581 wake.log lines**, `[eco]
   EcoQoS on` after `visible=false`; one machine sleep mid-soak (uptime
   3610→7210 s), resumed clean.
3. **T2** cpal device error → `Paused` + Message (flag on existing wake,
   no new thread/timer) + **pause-position fold** (`base_ms` fold after
   `save_now`; resume reads `base_ms` only). Probe: Paused + Message 3/3,
   resume within ±1.5 s.
4. **T3** drag-and-drop: `WindowEvent::DroppedFile` thread-local sink in
   `winit_hook.rs` → existing `Command::OpenPath` (one event per file).
5. **T4** single instance: `Global\qpid-instance` mutex (first statement
   of `run_ui`, before window/engine) + machine-global `\\.\pipe\qpid-open`
   handoff, sanctioned 4th thread `qpid-pipe`, `raise_window`. Probe:
   inst2 exit **51 ms** (post-fix re-run 48 ms), `[store]` handoff line.
6. **T5** media keys: `RegisterHotKey` (ids 1–3) on a message-only window
   with own wndproc, `WM_HOTKEY` → TogglePlay/Next/Prev. Size **+512 B**.
   Probe: all 3 `VK_MEDIA_*` held (1409), routed, released on exit.
7. **T6 + branch review**: BENCH/IMPLEMENTATION/USER_GUIDE + §15.6 table;
   review found 2 Important → fixed in `40b0719` (`SetLastError` pre-seed,
   no-op `mem::forget` removal + doc refresh).
8. **All §15.6 exit gates PASS, including all 5 human gates** (user
   reported 2026-10-05). Final numbers below.

## Final gate numbers (final build = `40b0719`, docs commits after it don't rebuild)

- `cargo test` **31/31**; release warnings **7** (baseline dead-code set).
- Exe **10,452,992 B** ≤ 10,485,760 (margin **32,768 B** — tightest gate).
- Threads **11 idle / 12 playing = AT CAP (zero headroom)** — any new
  thread is a budget failure; BENCH wording stays "AT CAP", never bare PASS.
- Budget 4: **4.97 MB** no-trim official (`QPID_NO_TRIM=1`) / 2.10 MB
  trim-on (info). Budgets 5–7 not re-run (no steady-state CPU added);
  one INFO spot-check: 2x-minimized 1.65%.
- Rule 10: 13 `eprintln!` hits, 12 guarded, documented pre-existing
  exception `src/main.rs:147` (Phase 1 dev-only CLI). Gate 4: PASS
  (`winit_030` only `src/winit_hook.rs:8`).
- Size trail: T1 10,444,288 → T2/T3 10,445,824 → T4 10,451,456 →
  T5 10,451,968 → HEAD 10,452,992 (+1,024 = slint help text; slint
  compiles INTO the exe).

## How it was done (process to follow)

- **SDD loop per task**: brief → implementer subagent → review package →
  independent reviewer → fix round → re-verify. Full trail in
  `.superpowers/sdd/2026-10-04-phase5-windows-integration/` —
  `progress.md` is the **authoritative ledger** (rulings, probe races,
  deferred minors); trust it over recollection or task reports.
- **Rulings live in the ledger**; append any new human ruling there.
- T2+T3 ran in **parallel by user order** (disjoint files); implementers
  did debug verification only — controller ran **one** release build +
  size/warning gate after both landed.
- Probes stay in `$env:TEMP\opencode\` — never the repo. `bench_results/`
  is gitignored; only BENCH.md numbers are durable.

## Main struggles and how to beat them next time

1. **Task 4 subagent answered with a PLAN, not code** (entered its own
   plan mode). Fix: resume the *same* `task_id` with "approved, execute
   now" + explicit pre-approvals so it can't stall again.
2. **Device probe race:** `d` → `p` needs **≥ 2.5 s** — the engine's
   organic Playing wake is ≤ 2 s; a shorter sleep lets `p` beat the flag
   → false failure (seen 1/3 at 1 s pacing).
3. **Probe scripts must kill their own inst1/children before exit** — a
   cmd wrapper inheriting the shell's stdout hangs the controller shell
   (happened twice). Launch probes detached (`Start-Process -WindowStyle
   Hidden`), poll a `done.marker`, kill only paths ending
   `target\debug\qpid.exe` (so the soak is never killed).
4. **cmd-quoting for exe + redirects:** build the `/c "..."` string by
   **concatenation**; the brief's escaped-quote form swallows `>`/`2>` as
   arguments (no files created, path arg mangled). Verify with a `ping`
   canary before the real run.
5. **PS 5.1 probe quirks:** `Start-Sleep 500ms` is PS7-only (floods 5.1);
   `Process.ExitCode` stays `$null` after `Start-Process` with redirected
   streams — capture `$proc.Handle` at launch, read exit code via
   `GetExitCodeProcess`.
6. **`windows` crate features are compiler-forced** — add ONLY on a
   compiler error, then re-measure exe size. Full set now in Cargo.toml:
   Foundation, **Security, Storage_FileSystem, System_IO** (T4 pipe/mutex),
   Threading, Power, ProcessStatus, Memory, Pipes, WindowsAndMessaging,
   **UI_Input_KeyboardAndMouse**, **Graphics_Gdi** (T5 media keys;
   GDI-gated `WNDCLASSEXW`), Media_Audio.
7. **Slint text compiles into the exe** — after ANY `ui/main.slint` edit
   the size/warning evidence is stale until a fresh release build (branch
   review caught this). Also: pasted brief snippets can add a warning
   (T1's `mut`) — diff warnings against a stashed baseline.
8. **Measure exe size after EVERY task** (user rule). Warning-count
   command quirk: `Select-String "warning" | Measure-Object -Line` returns
   8 (7 headers + summary) — the gate is the 7 warning headers.
9. **Do not relitigate budget-5 CPU UNSTABLE** (Phase 4 ruling stands);
   bench.py robustification (P-core pin / median-of-N) is a separate
   future task.
10. **Namespace ruling:** mutex + pipe are both machine-global
    (`Global\qpid-instance`, `\\.\pipe\qpid-open`) — one player per
    machine; **do not session-suffix** (cross-session handoff accepted).
11. **`GetLastError` after `CreateMutexW` needs a
    `SetLastError(ERROR_SUCCESS)` pre-seed** — a stale
    `ERROR_ALREADY_EXISTS` makes instance #1 exit into a dead pipe
    (fixed `40b0719`; probe re-run exercised it).
12. **windows 0.58 `HANDLE` is `Copy` with no `Drop`** — `mem::forget`
    is a no-op (removed at `40b0719`); never close the mutex/pipe handles.
13. **winit has no `WM_HOTKEY` path** → message-only window + own
    wndproc; slint's winit backend forwards ALL window events to the
    custom handler *before* slint dispatch; winit emits one `DroppedFile`
    per file (multi-drop = sequential OpenPath).
14. **Rule-10 grep must check the line ABOVE each `eprintln`**
    (`grep -B1`); `main.rs:147` is a documented pre-existing exception
    (Phase 1, dev-only CLI) — don't "fix" it silently.
15. **Parallel implementers = commit races** → retry on `index.lock`
    (5 × 10 s); controller runs ONE release build after a parallel pair.
16. **Trim/EcoQoS knobs:** `QPID_NO_TRIM=1` gates the working-set trim
    (budget-4 official run uses it); EcoQoS engages only when hidden.

## Open items (deliberately not done)

- **Optional, only if a gate regresses:** `[drop]` debug line
  (`winit_hook`), dialog-aware `raise_window` (rfd dialog can win
  Z-order), duplicate-Message ceiling on device_lost stream drop (never
  observed), `cfg(not(windows))` dead-code lints, panic-path skips
  `stop_media_keys`, stale `MEDIA_TX` clear on early-return (all accepted
  at branch-review triage).
- **`underruns > 0` path documented, not exercised** (no deterministic
  generator: ring 2.9 s vs max engine sleep 2 s) — IMPLEMENTATION.md.
- Earlier-phase leftovers: see git history / `.superpowers/sdd/`.

## Verification commands

```powershell
cargo test                                   # expect 31 passed
cargo build --release 2>&1 | Select-String ': warning'  # expect 7 headers
(Get-Item target\release\qpid.exe).Length    # cap 10,485,760; now 10,452,992 (margin 32,768)
# rule 10 (previous line must show debug_assertions; 13 hits, 12 guarded,
# documented exception src/main.rs:147):
& "C:\Program Files\Git\bin\bash.exe" -c 'grep -n -B1 "eprintln!" src/*.rs src/**/*.rs'
# gate 4 — only src/winit_hook.rs:8:
& "C:\Program Files\Git\bin\bash.exe" -c 'grep -rn "winit_030" src'
# threads: 11 idle / 12 playing — ≤ 12, AT CAP; any new thread = budget failure
```

State at handoff: `master`, clean tree, docs-only commits after `40b0719`
(no rebuild needed), 31/31 tests, 7 release warnings, all §15.6 gates
PASS (5 human gates 2026-10-05).
