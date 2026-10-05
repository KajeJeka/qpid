# q-pid benchmark results

Measured 2026-09-24, Windows 11 x64, release build, `tools/bench.py`.
Test file: `test_audio/test.mp3` (44.1 kHz stereo MP3, ~60 min).
Phase 3 budget re-checks (exe size, budget 6 spot-check) measured
2026-09-26 — see rows 1/6 and the Phase 3 probes section.
Phase 4 measurements (UI wiring) taken 2026-10-03; budget
re-measurements (rows 1/3/4/5/7/9) taken 2026-10-04 — see the
"Phase 4" rows and the Phase 4 probes section.
Phase 5 measurements (Windows integration) taken 2026-10-05 — rows
4/6/8, the EcoQoS soak, and the §15.6 exit-criteria table in the
Phase 5 sections at the bottom of this file.

## Budgets (architecture.md §2)

| # | Budget | Target | Result | Verdict |
|---|--------|--------|--------|---------|
| 1 | Exe size | ≤ 10 MB | **10,418,176 B (9.94 MB)** — Phase 4 final build (after the rule-10 debug guard; Task 6 measured 10,419,200 B before it); Phase 3 was 10,443,264 B, Phase 2 was 10,379,776 B | **PASS** (cap 10,485,760) |
| 2 | Startup to first frame | ≤ 300 ms | 141 ms median (5 runs; first cold run 329, warm 134–248) — **not re-run for Phase 3 or 4** | **PASS** |
| 3 | USS, visible, 1x | ≤ 25 MB | 5.48 MB — Phase 4 re-measure with the 500 ms timer active (2026-10-04, `bench_results/playing-1x-visible.csv`); Phase 0 was 3.35 MB | **PASS** |
| 4 | USS, minimized, 1x | ≤ 15 MB | **Phase 5 (2026-10-05): 4.97 MB without trim — official, 300 s, `QPID_NO_TRIM=1` (`bench_results/p5-t1-notrim`); 2.10 MB with trim armed — info only, 60 s (`bench_results/p5-t1-trim`), −58% USS**; Phase 4: 5.86 MB fresh UI re-run with the 500 ms timer active (2026-10-04, `bench_results/playing-1x-minimized.csv`; earlier 5.34/5.24 report-only); Phase 0 was 5.39 MB | **PASS** |
| 5 | CPU, minimized, 1x | ≤ 0.5% | 0.44–0.98% interleaved A/B spread, n=4 per build (2026-10-04) — **see budget-5 note below** | UNSTABLE (0.44–0.98%, see note) |
| 6 | CPU, playing at 2x | ≤ 2% | 1.57–1.88% (5 runs; see scenario below); **Phase 3 spot-check: 1.63% avg (1 run)**; **Phase 4 control: 1.82% (2026-10-04)**; **Phase 5 spot-check: 1.65% (1 run, 2026-10-05, INFO — see Phase 5 CPU note)** | **PASS** |
| 7 | Paused > 10 s | 0.0% CPU | **Phase 4 ended >10 s (probe, 2026-10-03):** 0 CPU-s over the 15 s fully-released window, handles 202 → 185, wake rate 0/s; **Phase 4 paused, 60 s re-run (2026-10-04):** 0.00%, USS 1.75 MB, 9 threads (`bench_results/paused-over-10s.csv`; controller's 30 s run same day also 0.00%); history (Phase 2, 2026-09-24): 0.00% over 30 s, USS 1.84 MB, handles −17 | **PASS** |
| 8 | Threads | ≤ 12 (revised from 4, see isolation below) | **Phase 5 Task 4 (2026-10-05): 11 idle / 12 playing** (`bench_results/p5-t4-threads/*.csv`, 60 s runs; +1 thread over the 10/11 history = `qpid-pipe`, the single-instance listener); history: 10 idle / 11 playing / 9 paused-released (Phase 4 minimized runs also showed 11), not re-run for Phase 3 or 4 | **AT CAP (12/12, zero headroom) — any future thread-adding change is a budget failure** |
| 9 | Wakeups/s, background | ≤ 2/s | 0.529/s primary (debug UI binary minimized, 38 wakes / 71.8 s) + 0.533/s cross-check (release `--cli`, 32 wakes / 60 s) — engine counter method, see Phase 4 probes | **PASS** |

Budgets 2/7/8 were not re-run for Phase 3: no design change since
Phase 2 (no new threads; the 30 s checkpoint rides the existing wake,
no new timer). Phase 4 re-measured budgets 1/3/4/5/7/9 (rows above);
budgets 2/6/8 were not re-run as full scenarios for Phase 4 (budget 6
has a single-run control, row 6).

**Budget 5 note (Phase 4 human ruling, measured 2026-10-04):**
interleaved A/B, n=4 each, run order base/head/base/head/…:

| build | min | median | max | mean |
|---|---|---|---|---|
| Phase 4 base `1466e19` | 0.44% | 0.67% | 0.82% | 0.650% |
| Phase 4 head `46923c5` | 0.58% | 0.70% | 0.98% | 0.741% |

Δ of medians 0.03 pp; Δ of means 0.09 pp; base itself fails the ≤0.5%
gate 3/4 today. Root cause: heterogeneous-CPU scheduler variance on a
bursty workload (i5-12450HX 4P+4E); historical same-config spread in
this file was already 0.11–0.89 incl. a prior 0.59% FAIL noted as
variance. Controls on 2026-10-04: budget 6 2x = 1.82% PASS, budget 7
paused = 0.00% PASS. Attribution: NOT a Phase 4 contributor (controller
interleaved A/B above).

**Phase 5 CPU% note (2026-10-05):** budgets 5–7 were **not re-run** as
suites for Phase 5. Phase 5 adds no steady-state CPU — no new timers, no
new threads on the hot path (the device-error flag check is one relaxed
atomic load riding the existing engine wake), and EcoQoS / working-set
trim only engage while the window is hidden. One 60 s
`playing-2x-minimized` spot-check was run anyway (`--cli`, release,
`bench_results/p5-t6-2x`): avg CPU **1.65%**, max USS 4.24 MB, max
threads 9 — inside the Phase 2 1.57–1.88% band, PASS vs ≤ 2%. Recorded
as **INFO only**; the budget-5 UNSTABLE ruling above is untouched. Caveats:
`--cli` means no window to minimize (the harness's "could not find/
minimize the window" warning is expected for this mode, as in the Phase 2
runs) and the store's saved position advanced the file from track 3 to 4
mid-run.

## Scenarios

### idle-visible (30 s, window visible, no file)

- avg CPU: 0.00%
- max USS: 3.35 MB
- max threads: 10
- max handles: 187
- Raw: `bench_results/idle-visible.csv`

### playing-1x-minimized (60 s, minimized, 1x)

Multiple runs (CPU is bursty ~16 ms every ~2 s; 1 s sampling):

| Run | avg CPU | max USS | max threads |
|-----|---------|---------|-------------|
| 1 | 0.41% | 4.91 MB | 11 |
| 2 | 0.47% | 4.92 MB | 11 |
| 3 | 0.40% | 4.93 MB | 11 |
| 4 | 0.35% | 4.91 MB | 11 |
| 5 | 0.39% | 4.95 MB | 11 |
| 6 | 0.45% | 4.89 MB | 11 |
| 7 | 0.32% | 4.96 MB | 11 |

Final recorded run: avg CPU **0.315%**, max USS **4.96 MB**, max threads **11**.
All runs pass USS and CPU gates; one earlier run (0.59%) failed CPU due to measurement variance.

Post-resampler run (rubato engaged, 44.1 → 48 kHz, deps at `opt-level = "s"`,
rustfft at `-O3`): avg CPU **0.42%**, max USS **5.39 MB**, max threads **11** —
PASS. Building rustfft at `-z` for size had pushed CPU to 0.80% (FAIL) while
saving only ~8 KB, so FFT stays at -O3 (see architecture.md §14).

Thread count drops 11 → 8/9 after ~28 s (transient UI helper threads exit when minimized).

### playing-2x-minimized (60 s, 2x, 5 runs, budget 6)

| Run | avg CPU | max USS | max threads |
|-----|---------|---------|-------------|
| 1 | 1.88% | 3.34 MB | 7 |
| 2 | 1.65% | 3.35 MB | 7 |
| 3 | 1.87% | 3.57 MB | 7 |
| 4 | 1.57% | — | 7 |
| 5 | 1.77% | — | 7 |

PASS vs ≤ 2% in all five runs. Spread verdict: the runs are identical
config (same speed, file, build), so the 1.57–1.88 range (0.31 pp) is
run-to-run measurement noise, not a speed-dependent effect. Run order
(1.88 → 1.65 → 1.87 → 1.57 → 1.77) alternates with no monotonic trend,
which argues against a thermal ramp; for reference, budget 5's
same-config spread was larger (0.11–0.89). Plan against the worst
observed number, **1.88%**, which leaves 0.12 pp (6%) of headroom.

**Method / caveats:** `--cli` mode — the harness presses `s` three times
on stdin (1 → 1.25 → 1.5 → 2) before sampling, and the process is
headless (`windows_subsystem = "windows"`, no window), so "minimized" is
moot. **This measurement excludes all UI thread cost** (no Slint window,
no 500 ms position timer — the latter does not exist yet either). When
Phase 4 wires the timer and the UI drives speed directly, this budget
**must be re-verified in UI mode**: the thin 0.12 pp margin means even
small added CPU can flip the verdict. UI mode cannot be automated for
this scenario today (the harness has no way to click the speed row), so
re-verification will need a UI-capable harness or a manual run. Speed
engagement was verified separately (CLI smoke below): position advanced
at exactly 2.00x wall clock over 2 s, counted at the device callback, so
the stretcher, resampler and ring all flowed at 2x.

### paused-over-10s (11 s release wait, then 30 s sampling)

- avg CPU: 0.00% (every sample after the release window is exactly 0)
- max USS: 1.84 MB — down from 5.39 MB playing: the §8.2 release drops
  stream, ring, decoder and resampler (file + device handles: 193 → 176)
- max threads: 9 — the `cpal_wasapi_out` thread is gone
- Release behavior probe: seek-while-released updates position only;
  resume reopens the file at the stored position (`TrackChanged` re-emitted)
- Raw: `bench_results/paused-over-10s.csv`

## Thread isolation (budget 8 investigation)

Measured 2026-09-24 with a temporary `--slint-only` probe (window, no engine, no cpal) and the CLI path (engine+cpal, no window). Named-thread attribution via `GetThreadDescription`; start-address → module map failed (snapshot returns empty under this sandbox). Counts are stable across repeated runs.

| Configuration | Threads | Composition |
|---|---|---|
| A: Slint window only | **9** | main + softbuffer + ~7 Windows/winit/COM helpers (EventPairLow) |
| B: CLI engine+cpal (no window) | **7** | main + `qpid-engine` + `cpal_wasapi_out` + ~4 COM/WASAPI helpers |
| C: Full UI, no file | **10** | A + engine = 9 + 1 |
| D: Full UI, playing (bench) | **11** | C + `cpal_wasapi_out` = 10 + 1 |

**Subtraction:**

- C − A = **1** → engine thread only (cpal stream not open until a file plays).
- D − C = **1** → `cpal_wasapi_out` appears when playback starts.
- App-owned threads we create: main, `qpid-engine`, `softbuffer_*` (Slint), `cpal_wasapi_out`, one UI event-drain (`ui.rs`). rfd dialog threads are transient (only while a file picker is open).
- Remaining **EventPairLow** threads are Windows COM/RPC infrastructure spun up by winit and WASAPI — not application threads, not reducible without replacing the libraries.

**Floor without our code:** Slint-only already sits at 9 (> 4). CLI (no Slint window) sits at 7 (> 4). Both library stacks independently exceed budget 8 before counting our three designed threads.

**Verdict:** The budget of 4 assumed a minimal stack that does not match Slint+winit+cpal+Windows COM reality. Replacing the UI (§3.3) or cpal (Phase 6.3) would not reach 4 while still using either library's Windows backend. Budget 8 revised to **12** (approved); architecture.md §2.8 updated with this measurement as the reason. The three-thread design for app-owned threads (§5) stays the soft target.

## Known failures / caveats

1. **Thread budget:** revised from 4 to **12** in architecture.md §2.8, on the
   strength of the isolation table above (Slint floor 9, cpal floor 7). The
   app-owned design target stays 3 threads.
2. **`test.m4a` generation fails** (ffmpeg AAC experimental encoder). mp3/wav/flac/ogg OK.
3. **Clicks on pause/seek:** not verifiable programmatically — human listening required (Phase 1 exit criteria §15).
4. **Rate mismatch pitch bug — fixed:** device is 48 kHz, test file 44.1 kHz;
   the rubato `FftFixedIn` resampler now engages (unit tests in
   `src/engine/dsp.rs`; debug builds print `resampler=engaged`). Human ear
   check of the resulting pitch still pending.
5. **Ring overflow drops — fixed:** `HIGH_WATER` equalled the 3 s ring
   capacity, so the last packet of every refill burst was dropped (~11 ms of
   audio per cycle; measured 6 drop events / 6,528 samples in 12 s).
   `HIGH_WATER` reduced to 2.9 s; re-measured 0 drops in 12 s.
6. **End-of-track position** can read ~140 ms past the duration (the
   resampler's drained tail silence is counted as played). Cosmetic; not fixed.
7. **Pause release was incomplete — fixed:** `release()` used to drop only
   the cpal stream; the decoder stayed open and the engine loop kept a 10 s
   re-check timer, violating budget 7 and §8.2. The whole `PlaybackContext`
   is now dropped at 10 s (keeping only path + position), the engine parks
   for 1 h until the next command, and resume reopens the file at the stored
   position. Measured: 0.00% CPU, handles −17, USS 1.84 MB (scenario above).

## CLI smoke (Phase 1)

`qpid.exe --cli <file>` + stdin keys: play/pause, ±15 s seek, resume-from-seek all verified exit 0.

## CLI smoke (Phase 2, speed)

`--cli` + `s` key: cycle 1 → 1.25 → 1.5 → 2 → 0.5 → 1 verified (printed
per press). Position rate matched each speed over wall clock (2x: +4020 ms
in 2000 ms; 0.5x: +295 ms in 600 ms), no jumps at the speed-change flushes,
and pressing `n` at 1.25x drained to `StateChanged(Ended)` (EOF flush runs
through the engaged stretcher). Release build exit 0. Human listening
(pitch at each speed, clicks on speed change) still pending.

## Phase 3 probes (playlist + persistence)

All automated, release build, scripts in `$env:TEMP\opencode\` (not
committed). Detail in `.superpowers/sdd/2026-09-26-phase3-playlist-persistence/`
task reports.

- **Mixed-rate advance (Task 3): PASS both directions.** 44.1k → 48k via
  `n`: wall TrackChanged→Ended 30050 ms vs duration 30024 (ratio 1.0009;
  wrong-resampler-config prediction would be +8.8%), final position 30024
  exact. 48k → 44.1k via `P`: near-EOF segment 12360 ms wall vs correct
  prediction 12582 (wrong config 14021) — wrong config excluded by
  1661 ms. `TrackChanged` durations match each file.
- **Restore probes A/B/C (Task 5): PASS.** A (fresh restore): saved
  8022 ms → relaunch restored `position_ms = 8022`, `StateChanged(Paused)`,
  no `Playing`, `state.json` schema as §9. B (no history): clean start,
  `position_ms = 0`, no events, `state.json` recreated. C (corrupt
  `state.json`): clean start, no events, exit 0.
- **Acceptance 6 — kill while playing → relaunch (Task 6): PASS.**
  Play 65 s (checkpoints at 30 s and 60 s) → `Stop-Process -Force` →
  relaunch `--cli` with no path arg:
  ```
  [event] TrackChanged { folder: "...\\test_audio", title: "test", index: 3, count: 7, duration_ms: 600032 }
  [event] StateChanged(Paused)
  position_ms = 60012
  ```
  position 60012 ∈ [35000, 70000] (killed at ~65 s, last checkpoint 60 s,
  ≤ 30 s loss per §9 rule 7), `StateChanged(Paused)` present, **no**
  `StateChanged(Playing)`, same file (`test.mp3`) restored.
- **Budget 1 re-check:** 10,443,264 B ≤ 10,485,760 PASS (see row 1).
- **Budget 6 spot-check (Task 6):** one run, `playing-2x-minimized`,
  60 s: avg CPU **1.63%**, max USS 3.42 MB, max threads 7 — CPU budget
  (2.0%) PASS, memory PASS, thread PASS. Inside the Phase 2 1.57–1.88%
  band; no regression.

## Phase 4 probes (UI wiring)

All automated (except the items marked human). Build per probe:
**debug** for visibility (Task 1), timer-stop (Tasks 2/4), keyboard
(Task 4), ended-release (Task 5) and the budget-9 primary (Task 6) —
those probes assert cfg-gated output (`[vis]`/`[tick]`/`[release]`) that
a release build does not print; **release** for the path-arg acceptance
and the budget-9 cross-check (Task 6). Scripts in `$env:TEMP\opencode\`
(not committed). Probes from Tasks 1–5 taken 2026-10-03; Task 6 probes
and the budget re-measurements 2026-10-04. Detail in
`.superpowers/sdd/2026-10-03-phase4-ui/` task reports.

- **Visibility probe (Task 1):** minimize/restore ×2 produced 4 `[vis]`
  transitions (5 `[vis]` lines in total including the startup seed); the
  transition trio showed
  `[vis] min=false occ=false size=600x400 -> visible=true` →
  `[vis] min=true occ=false size=0x0 -> visible=false` →
  `[vis] min=false occ=false size=600x400 -> visible=true`
  — Windows emitted **zero-size** on minimize (`is_minimized=true` and
  `size=0x0`; `occluded` stayed false and no `WindowEvent::Occluded` was
  ever emitted — zero `[vis] occluded-event` lines).
- **Timer stop (Tasks 2 + 4):** 0 `[tick]` lines over 30 s minimized while
  playing (15 ticks counted while playing before the minimize; 8 ticks
  in the 4 s after restore); 0 over 30 s paused (`save (pause)` confirms
  the pause; focus assert `True`); ticks resumed after restore (8 in 4 s
  ≥ 4 expected). Debug build; spec §5 must-hold.
- **Ended release (Task 5):** `[release] Ended stream+decoder released`
  printed (True); handles 202 → 185; CPU 0 s over the 15 s
  fully-released window (t=47..62, gate ≤ 0.05); post-release wake rate
  0/s (wakes 28 → 28 over 15.1 s); restart via TogglePlay reopened the
  file (TrackChanged ×2) and positioned at 1950 ms (< 10 s expected).
- **Budget 9 method (0.529/s primary, 0.533/s cross-check):**
  Self-instrumented engine-loop wake counter (`Shared.wakes`, one
  increment per loop iteration); primary reading from the actual debug UI
  binary minimized ≥70 s (measured 71.8 s) via the env-gated file sink;
  cross-check from the release `--cli` build over 60 s; excludes the
  audio callback by design; UI timer contributes 0 while minimized/paused
  (Task 2/4 probes: 0 `[tick]` over 30 s each); Limitation: counts
  engine-loop iterations, not OS-level wakeups, and does not directly
  count winit event-loop/COM helper threads — bounded by the measured
  CPU of the same minimized release run (budgets 3/4/5); a wakeup
  arriving while a thread is already awake is not distinguished.
- **Path-arg acceptance (Task 6):** `qpid.exe "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\test_audio\test.mp3"`
  (absolute path) — window
  opened (True), CPU 0.06 s over 5 s (autoplay), closed cleanly via
  WM_CLOSE (True); all three expectations met. Caveat: the 0.05 s CPU
  gate clears by only 0.01 s — bench's visible steady-state (~0.028
  CPU-s per 5 s) sits below it, so the check leans on early decode
  activity and could flake on a quieter machine.
- **Keyboard probe (Task 4):** focus failures 0; Space → pause save 1/1;
  Right → `[seek]` line 1/1; Ctrl+O → dialog opened (True); N → next
  save 1/1 (input file `test.mp3` — `tone48k_60m.mp3` sorts last, where
  Next is a designed no-op). `[`/`]` speed step and Ctrl+Shift+O: human
  checklist.

## Phase 5 probes (Windows integration)

Automated unless marked human. Detail in
`.superpowers/sdd/2026-10-04-phase5-windows-integration/` task reports;
scripts in `%TEMP%\opencode\` (not committed). Taken 2026-10-05.

- **EcoQoS soak (Task 1 Step 8, read in Task 6 Step 1): PASS.** "EcoQoS
  soak: 30 min minimized playing, debug build, trim armed,
  underruns=0" — the soak ran **129.6 min** (gate ≥ 30 min), `wake.log`
  has 581 lines with **`underruns=0` on every line** and a uniform line
  format (single writer); `stderr.txt` contains `[eco] EcoQoS on` **after**
  `[vis] ... visible=false` (both asserts true).
  Sleep-resume note: one wake gap jumped `uptime_s` 3610 → 7210 — the
  machine slept during the soak; wakes resumed afterwards and underruns
  stayed 0. Supporting evidence for the sleep/wake human gate only, not a
  replacement for it.
- **Budget 4:** row 4 — 4.97 MB no-trim (official) / 2.10 MB trim-on (info).
- **Threads:** row 8 — 11 idle / 12 playing, **AT CAP (12/12)**.
- **Single-instance probe (Task 4 Step 6):** inst2 exit **51 ms**,
  `[store] save (open of another path)` in inst1's stderr, exactly one
  qpid from the probe path remained.
- **Media-key probe (Task 5):** message-only window `qpid-media-keys`
  present; all three `VK_MEDIA_*` registrations held (probe's
  `RegisterHotKey` → error 1409, bracketed by baseline/post-exit
  successes); `WM_HOTKEY` ids 1/2/3 accepted and routed to the engine;
  all three released after exit.
- **Limitation:** the `underruns > 0` path is asserted by code review and
  counter wiring but never exercised end-to-end — no deterministic
  underrun generator exists (ring 2.9 s vs max engine sleep 2 s; only a
  stalled engine thread could starve it). Soak and normal playback
  observed `underruns=0` (see IMPLEMENTATION.md, Phase 5).

## Phase 5 exit criteria (architecture.md §15.6)

Automated rows are final; the five human rows are **PENDING-HUMAN** and
must not be claimed as passing until the user reports results.

| Gate | Evidence |
|---|---|
| Unplug headphones while playing → pauses cleanly | **PENDING-HUMAN** (Task 2 Step 8). Automated support: device-error path probe — `StateChanged(Paused)` + `Message("Output device changed — press Play to resume")` 3/3 runs |
| Sleep and wake works | **PENDING-HUMAN** (Task 2 Step 8). Supporting evidence only: soak sleep-resume gap `uptime_s` 3610 → 7210, wakes resumed, `underruns=0` |
| Drag-drop a file onto the window opens it | **PENDING-HUMAN** (Task 3 Step 4) — compile/type-level verification only (`WindowEvent::DroppedFile` sink) |
| Second launch with a file path opens it in the first instance | **PASS (automated)** — Task 4 Step 6: inst2 exit **51 ms** (≤ 1000 ms), `[store] save (open of another path)` in inst1 stderr, exactly one qpid from the probe path |
| Visual: second launch raises/restores the first window | **PENDING-HUMAN** (Task 4 Step 8) — probe proves the code path ran; `SetForegroundWindow` can be refused by the OS foreground lock |
| Physical media keys control playback | **PENDING-HUMAN** (Task 5 Step 5). Automated support: 3/3 `VK_MEDIA_*` held (1409), `WM_HOTKEY` 1/2/3 accepted, released on exit |
| Exe ≤ 10,485,760 | **PASS** — **10,451,968 B** (Task 5 release build at `741ce43`), margin **33,792 B** (verified from `target\release\qpid.exe`, code unchanged since) |
| Threads ≤ 12 | **AT CAP (12/12, zero headroom)** — Task 4 Step 7: 11 idle / 12 playing (row 8) |
| Budget 4 ≤ 15 MB without trim | **PASS** — Task 1 Step 7: **4.97 MB** (300 s, `QPID_NO_TRIM=1`); trim-on 2.10 MB (info) (row 4) |
| Tests 31/31, release warnings = 7 | **PASS** — `cargo test` **31/31** re-run at HEAD code for this table (also Task 5's run); release warnings **7** = baseline (Task 5 build, no code change since) |
| Rule 10 audit | **1 pre-existing exception, no Phase 5 violation** — equivalent of `grep -n -B1 "eprintln!" src/*.rs src/**/*.rs`: 13 hits, 12 have `debug_assertions` on the previous line; the one exception is `src/main.rs:147` `eprintln!("unknown command: ...")` in the dev-only `--cli` stdin loop, from Phase 1 (`dca6282`, 2026-09-24), untouched by Phase 5 |
| Gate 4 | **PASS** — `grep -rn "winit_030" src` → only `src/winit_hook.rs:8` |
