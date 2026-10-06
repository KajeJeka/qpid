# handoff.md — q-pid session handoff (Phase 6 CLOSED / v1.0.0 tagged, 2026-10-06)

For the next agent working on this repo. Read this first, then
`architecture.md` (binding), `IMPLEMENTATION.md` (status), `BENCH.md`
(final table at the end), and `docs/USER_GUIDE.md`. The block below is
the Phase 6 close record (work + struggles), then the Phase 5 body
(integration details still accurate). Earlier handoffs and SDD ledgers
(Phases 0–5) are in git history and `.superpowers/sdd/`.

**Next phase (NOT started): Linux port.** The repo is on GitHub
(`origin` = https://github.com/KajeJeka/qpid, pushed 2026-10-06); the
`v1.0.0` release is published at
https://github.com/KajeJeka/qpid/releases/tag/v1.0.0 with the portable
exe `qpid-v1.0.0.exe` attached (rebuilt from the tag, 10,452,480 B).
The Linux port will need the Windows-only
surface (`winit_hook`, `winapi.rs`, EcoQoS/trim, `Global\` mutex,
named pipe, `RegisterHotKey`, cpal/WASAPI specifics) behind `cfg` gates —
`cfg(not(windows))` dead-code lints are already an accepted open item
(see below).

## State at Phase 6 (measurement complete → v1.0.0)

- **Phase 6 is fully closed** — all 20 tasks done, whole-branch review
  APPROVE-WITH-FIXES (0 must-fix) + M1–M5 fix dispatch + re-review
  APPROVE, `Cargo.toml` `1.0.0`, **tagged `v1.0.0` = `a5bede4`**, tree
  clean. Evidence: `.superpowers/sdd/2026-10-05-phase6-final-measurement/`
  (`progress.md` ledger, `review-final.md`, task reports — all gitignored
  per ruling R2).
- **All nine budgets fresh in `BENCH.md` "Phase 6 final table —
  v1.0.0 (2026-10-05/06)"** — every row traced to a task report by the
  Task 18 review (7/7 APPROVE).
- **Budget 5 is settled (do not relitigate): permanent UNSTABLE ruling** —
  0.62% median (5 × 300 s, P-core pinned, `bench.py --affinity/--repeat`),
  attributed to scheduler/heterogeneous-core variance, **not carried to
  1.1**. This closes lesson 9's "robustification is a future task" — the
  harness fix is done and the ruling is recorded in BENCH.md.
- **Budget 8: AT CAP (12/12, zero headroom)** — never write it as a bare
  PASS; any new thread is a budget failure (§18.2). Task 17 re-confirmed:
  spawn set byte-identical to `40b0719`.
- **§7.6 was implemented in Phase 6** (the phase's only src change): commit
  `18aaf39`, Coarse fallback for relative seeks after a >300 ms seek,
  3 unit tests → `cargo test` **34**, size unchanged **10,452,992 B**,
  warnings **7**; §16.5 warm-up stalls 4 → 0. Every Lane B number is
  post-fix.
- Outstanding for the user: the **human listening checklist**
  (`BENCH.md`, "Human listening checklist (Phase 6 close)") — the only
  gate no script can run.

## Phase 6 session: what was done

1. **20 tasks, two lanes, all on `master`, 11 commits
   (`19d4fc6` → `a5bede4`), every task reviewed** (6 APPROVE + 2 parked
   minors in Lane A; every Lane B re-run APPROVE; Task 18 APPROVE 7/7;
   Task 19 APPROVE-WITH-FIXES → fix → APPROVE).
2. **Lane A (tasks 1–10, parallel):** acceptance tests §16.1–16.9
   (30-file order, m4a siblings, corrupt-file skip, unicode paths, 500-file
   open 63.5 ms, corrupt message, ASCII+unicode 7/7) → all PASS;
   security audit → BENCH baseline ("zero network surface in 1.0" with the
   `webbrowser` transitive-crate caveat stated, not buried); doc sweeps of
   IMPLEMENTATION / USER_GUIDE / BENCH (37 citation fixes); rule 10 +
   gate 4 audit PASS. Controller committed doc waves (R3/R4).
3. **Lane B (tasks 11–17, strictly serial — one exclusive subagent at a
   time, `Get-Process qpid` empty before each):** build + full budget
   sweep (all PASS), budget 5 harness (`--affinity` P-core pin +
   `--repeat` median-of-5 → permanent ruling), timing tests (§16.3/16.4),
   3-hour seek soak (§16.5), device change (§16.10), 30-min minimized 2×
   debug soak (§16.11), thread re-confirm (§budget 8).
4. **§7.6 fix — the phase's one src change:** the 3-hour seek run's
   warm-up fired the spec's timing condition (4 stalls >300 ms, max
   1.220 s). Root cause: `decode.rs` only had the *error*-based Coarse
   fallback; the spec's *timing* rule ("seek >300 ms → Coarse for
   relative seeks") was never implemented, and `seek_to()` erased the
   relative/absolute distinction. TDD red→green: `SeekPolicy`
   (mode_for/note_cost, sticky), `seek(target, relative)`, flag threaded
   through 12 call sites. Gates unchanged (34 tests, 7 warnings, size
   identical). **Re-run chain:** tasks 11/12/13/14 all re-measured on the
   new exe; stalls 4 → 0, budget 5 numbers refreshed (0.80 → 0.62).
5. **Close (tasks 18–20):** final nine-budget table + §16 probe rows +
   human listening checklist in BENCH.md; IMPLEMENTATION/handoff refresh;
   whole-branch review over `9207694..3c7d652` (every number
   spot-checked, all 12 call sites re-verified, `cargo test` run at HEAD);
   M1–M5 minors fixed in one dispatch + scoped re-review; version `1.0.0`
   + annotated tag `v1.0.0`.
6. **Ledger rulings (R1–R5):** R1 = every subagent must keep its todo
   list updated (user order); R2 = `.superpowers/sdd/**` gitignored
   (reports are workspace artifacts — the plan's "committed" wording was
   itself fixed as M5); R3 = Lane A parallel sanctioned, controller
   commits; R4 = subagents edit only, never commit; R5 = Lane B strictly
   serial exclusive.

## Phase 6 main struggles and how they were fixed

1. **Spec rule implemented in tests but not in code (§7.6).** The
   acceptance run *caught* a spec-vs-implementation gap that 5 prior
   phases missed: the >300 ms timing fallback existed only as prose.
   Fix pattern that worked: systematic-debugging root cause → TDD
   (RED test for the missing `SeekPolicy`) → implement → **re-run every
   measurement task whose number could have been tainted** (11–14).
   Lesson: an acceptance soak that fires a never-implemented rule is a
   *find*, not a flaky harness — check the spec text before blaming the
   probe.
2. **Budget 5 kept failing across heterogeneous P/E cores.** 0.59% →
   0.69% → 0.80%, machine-dependent, not reproducible run-to-run.
   Fix: `bench.py --affinity` (pin to P-cores; detection probe showed
   0.69 vs 0.87 unpinned) + `--repeat 5` median-of-5 → 0.62%. When still
   >0.5%: the plan's sanctioned fallback — permanent **UNSTABLE ruling**
   in BENCH with all four required citations, explicitly "not carried to
   1.1". Never re-run it into the ground.
3. **Task 15's own probe asserted the wrong bound.** Run 1 "failed" on
   symmetric ±1500 ms across a *deliberately accepted* ≤2 s
   device-detection wake (observed +1972 ms). The spec (§16.10) only
   says "pauses without a crash", and the Phase 5 fold bound is
   *asymmetric* (resume must not lose position). Fix: controller ruling —
   corrected asserts (Paused+Message 3/3, fold ±1500, detection latency
   ≤2000 recorded separately) → re-run 3/3 PASS. Lesson: before fixing
   product code on a probe FAIL, re-read the spec sentence the probe is
   supposed to encode.
4. **Measurement hygiene kept the numbers trustworthy:** per-second bench
   logs redirected to files (never controller context) + generous bash
   timeouts; no `Start-Sleep 500ms` (PS 5.1); Python subprocess with
   list args; `done.marker` detached launches; kill only paths ending
   `target\...\qpid.exe`; `Get-Process qpid` empty before every task.
5. **Doc drift found at the last gate, not before.** Task 19 found the
   canonical top budget table still showing Phase 4/5 numbers with no
   pointer to the Phase 6 final table (M1), a causal "4 → 0" claim the
   task-14 report itself hedged (M2 — the trigger never fired on the
   re-run; unit tests cover the branch), mixed 10-05/10-06 dates (M3),
   two `bench.py` harness traps (M4: pin status not shown in repeat
   summary, zero-sample run counted as 0.0%), and the plan's own stale
   R2/test-count wording (M5). All fixed in **one** dispatch + scoped
   re-review — findings went to a single fix batch, not N micro-rounds.
6. **Parked is a real state.** 5 items parked with adjudications
   (3 PARK-FOREVER, 1 folded into M2, 1 absorbed) instead of being
   re-litigated at close. The ledger `progress.md` is authoritative —
   trust it over task-report prose.


## What the project is

**q-pid** — a small Windows audio player (Rust + Slint 1.18.1, software
renderer, no GPU deps). Folder/file playlist player with seek bar, ±15 s
skips, five playback speeds (WSOLA), session restore, autosave
checkpoints, strict resource budgets (exe ≤ 10 MB, minimized CPU/RAM,
≤ 2 background wakeups/s), plus a `--cli` headless mode for tests/benches.

## Previous session (Phase 5: Windows integration)

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

## Final gate numbers (Phase 5 snapshot; final build then = `40b0719` — superseded by the Phase 6 block above)

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
9. **Do not relitigate budget-5 CPU UNSTABLE** — **settled in Phase 6**
   (permanent ruling, BENCH.md; `bench.py` gained `--affinity` P-core pin +
   `--repeat` median-of-N, pinned median 0.62% > 0.5% gate). Closed.
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
cargo test                                   # expect 34 passed (31 + 3 from the §7.6 fix)
cargo build --release 2>&1 | Select-String ': warning'  # expect 7 headers
(Get-Item target\release\qpid.exe).Length    # cap 10,485,760; now 10,452,992 (margin 32,768)
# rule 10 (previous line must show debug_assertions; 13 hits, 12 guarded,
# documented exception src/main.rs:147):
& "C:\Program Files\Git\bin\bash.exe" -c 'grep -n -B1 "eprintln!" src/*.rs src/**/*.rs'
# gate 4 — only src/winit_hook.rs:8:
& "C:\Program Files\Git\bin\bash.exe" -c 'grep -rn "winit_030" src'
# threads: 11 idle / 12 playing — ≤ 12, AT CAP; any new thread = budget failure
```

State at handoff: `master`, **Phase 6 CLOSED**, tagged **`v1.0.0` =
`a5bede4`**, pushed to GitHub, release `v1.0.0` published with the exe
asset, tree clean. Gates:
`cargo test` **34 passed** (31 + 3 from the §7.6 fix), release warnings
**7**, release asset **10,452,480 B** (cap 10,485,760; measured gate
build was 10,452,992 B, delta = version metadata only, see BENCH release
note), rule 10 =
13/12+1 (`main.rs:147`), gate 4 isolated, all §15.6 gates PASS (5 human
gates 2026-10-05), all nine Phase 6 budgets recorded in BENCH.md (5 =
permanent UNSTABLE ruling, 8 = AT CAP). Next phase: Linux port — start a
new session/plan for it; begin with a `cfg` audit of the Windows-only
surface.
