# handoff.md — q-pid session handoff (2026-10-04)

For the next agent working on this repo. Read this first, then
`architecture.md` (binding), `IMPLEMENTATION.md` (status), `BENCH.md`
(budget evidence), and `docs/USER_GUIDE.md` (user-facing behavior).

## What the project is

**q-pid** — a small Windows audio player (Rust + Slint 1.18.1, software
renderer, no GPU deps). Folder/file playlist player with seek bar, ±15 s
skips, five playback speeds (0.5–2x, WSOLA time-stretch), session restore
(opens paused at last position), 30 s autosave checkpoints, and strict
resource budgets (exe ≤ 10 MB, minimized CPU/RAM, ≤ 2 background wakeups/s).
Two faces: the **GUI** (single window, dark theme) and a **`--cli` headless
mode** (stdin commands) used for testing/benchmarks. Full design history in
`docs/superpowers/specs/` + `docs/superpowers/plans/`; per-phase reports in
`.superpowers/sdd/`.

## What was done this session

1. **Phase 4 executed end-to-end** (UI wiring) via subagent-driven
   development: 7 tasks, 12 commits (`f168086..fc21910` on `master`):
   - T1 slint pin + winit visibility hook (minimize/occlusion detection)
   - T2 500 ms position timer + `format_time` (m:ss / h:mm:ss)
   - T3 seek slider (drag vs commit, duration-0 disable, fraction→ms)
   - T4 keyboard shortcuts (one FocusScope) + `step-speed`
   - T5 Ended stream releases after 10 s like pause; Play restarts from 0;
     wake counter (`Shared.wakes` + CLI `w`); **EOF spin fix** (0 ms →
     100 ms floor in the engine wait)
   - T6 budget measurements (1/3/4/7/9) + path-arg acceptance + budget-9
     file sink (debug-only `QPID_WAKE_LOG`)
   - T7 BENCH/IMPLEMENTATION docs, final gate greps, rule-10 sweep
     (fixed 1 genuine release `eprintln` in `engine/dsp.rs`), human
     checklist written into `task-7-report.md`
2. **Budget 5 investigation** (CPU minimized ≤ 0.5% was failing): ran a
   controlled interleaved A/B of the Phase-4 build vs its own base — proved
   **no regression** (both builds straddle the gate); root cause is
   heterogeneous-CPU scheduler variance (i5-12450HX, 4P+4E). Human ruling:
   BENCH row records `UNSTABLE (0.44-0.98%, see note)` with both builds'
   min/median/max — **do not relitigate; bench.py robustification is a
   separate future task** (pin to P-cores or median-of-N).
3. **`docs/USER_GUIDE.md`** written: every button, shortcut, CLI command,
   stateful behavior, quirk, with `file:line` references.
4. **Help button** (this session's last feature): third button in the top
   row (`ui/main.slint`) opening an in-app overlay listing all commands.
   Pure-SLint state (`show-help` property) — no Rust wiring. Modal: while
   open, `Esc` closes and all other keys are rejected.
5. Every task passed independent code review (brief → implement → review →
   fix → re-review); final whole-branch review **Approved** (0 Critical).

## How it was done (process you should follow)

- **SDD loop per task**: generate brief
  (`.../scripts/task-brief <plan> N`) → dispatch implementer subagent →
  generate review package (`.../scripts/review-package <plan> <base> <head>`)
  → independent reviewer subagent → fix round → re-verify. Full trail in
  `.superpowers/sdd/2026-10-03-phase4-ui/` (`progress.md` = ledger of all
  controller rulings and deferred minors — **trust it over recollection**).
- **Rulings live in the ledger.** Anything a human decided (budget-5
  wording, absorbed task steps, probe rules) is appended to `progress.md`.
- **Probes** (PowerShell scripts) stay in `$env:TEMP\opencode\` — never the
  repo. `bench_results/*.csv` is gitignored; only BENCH.md numbers are
  durable, so re-run any bench you need raw samples from (bench.py
  overwrites per-scenario CSVs with `"w"`).
- Shell notes: no `bash` on PATH — use
  `"C:\Program Files\Git\bin\bash.exe"` for bash scripts. Subagents inherit
  the session model (no model param on dispatch).

## Main struggles and how to beat them next time

1. **Flaky perf gates on this machine.** The i5-12450HX (4P+4E) migrates
   the engine thread between core types; same-binary runs of budget 5
   ranged 0.11–0.98%. *How to surpass*: never judge a perf regression from
   one run — do an **interleaved A/B against the last known-good build**
   (this is what exonerated Phase 4); keep probes ≥60 s; for a stable
   verdict later, fix bench.py with CPU-affinity pinning or median-of-N
   (human-approved separate task).
2. **`FindWindow` fails on this box.** EnumWindows-based lookup also
   exists, but the reliable pattern is **`$p.MainWindowHandle`** from
   `Start-Process -PassThru` (used by every probe since T1).
3. **State leaks between runs.** The app restores its session from
   `LOCALAPPDATA`. Redirect it (`$env:LOCALAPPDATA = "$env:TEMP\opencode\qpid-p4"`)
   and **delete `state.json` before every probe/bench launch**, or you'll
   measure a paused/offset session. Bench.py does NOT do this for you.
4. **Bench CSVs are single-slot.** `bench.py` writes one CSV per scenario
   name — a later `--cli` run destroys the UI run's samples. Re-run with
   distinct `--outdir`s (or copy immediately) when keeping evidence.
5. **Debug-only logging discipline (rule 10).** Every `eprintln` must sit
   behind `cfg(debug_assertions)` (attribute on the line above, or
   `if cfg!(debug_assertions)`) or be inside `run_cli` (unreachable in UI
   mode). Beware: grepping only the `eprintln` line yields false hits —
   check the previous line too.
6. **Unstable-API isolation (gate 4).** `winit_030` /
   `with_winit_custom_application_handler` may appear ONLY in
   `src/winit_hook.rs`; slint stays pinned `=1.18.1` (gate 5). Run the
   Step-4 greps from `task-7-brief.md` after any UI-adjacent change.
7. **EOF drain-poll spin** (fixed in `46923c5`): if fill ≤ LOW at EOF, a
   0 ms wait spins ~80k wakes/s. The engine wait floor is 100 ms — do not
   "optimize" it back to zero. Verify with the CLI `w` command (expect
   ~0.5/s background).
8. **Subagent dispatches can be cancelled** (session pause/lunch). State
   is always recoverable: `git status` + `git log --oneline -5` +
   `progress.md` tell you exactly where to re-dispatch.

## Open items (deliberately not done)

- **Human checklist** (spec §6) — gate 7, written verbatim in
  `.superpowers/sdd/2026-10-03-phase4-ui/task-7-report.md` §3: manual
  key sweep, slider drag/click/duration-0, chord non-firing, focus
  retention, click-listening on pause/seek.
- **Backlog M7/M8/M9** (in `progress.md`): SeekSlider clamp expression
  duplicated ×3; Ctrl+Alt+O chord arm; import order.
- Drag-and-drop, light/dark theme, tag/artwork display → later phases
  (spec).
- Known quirks (documented, not bugs to silently fix): speed row shows 1x
  after session restore until clicked; status line never clears; budget-6
  UI-mode re-verification open.

## Verification commands

```powershell
cargo test                      # expect 31 passed
cargo build --release 2>&1 | Select-String ': warning'   # expect 7
(Get-Item target\release\qpid.exe).Length                # cap 10,485,760
python tools/bench.py --exe target\release\qpid.exe --scenario playing-1x-minimized --file test_audio/test.mp3 --duration 60
```

Current state at handoff: `master` @ help-button commit, 31/31 tests,
7 release warnings (baseline), exe 10,443,776 B ≤ cap, whole-branch review
Approved. **Not committed**: `docs/USER_GUIDE.md`, this file, and the help
button slint change (see `git status`).
