# Phase 6: final measurement pass → v1.0.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (Lane A) and superpowers:executing-plans / controller-inline (Lane B) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax. **Every subagent MUST always keep its todo list updated (todowrite) as it works — create a todo per step at start, mark in_progress before doing it, completed after; never leave it stale.**

**Goal:** Run the Phase 6 gate (`architecture.md` §15 Phase 6): fresh measurements of all nine budgets on the final build, remaining §16 acceptance tests, security baseline, doc sweep, budget-5 ruling — then whole-branch review and `git tag v1.0.0`.

**Architecture:** Two lanes. **Lane A** = parallel subagents (pure-logic acceptance tests, read-only audits, doc sweeps) — all finish before any measurement. **Lane B** = serial, exclusive-machine measurement (bench sweep, budget 5, timing-sensitive tests, soak, thread re-confirm). **Close** = final table, docs, review, version bump, tag.

**Tech Stack:** Rust/Slint release exe, `tools/bench.py` (psutil), Python probes in `$env:TEMP\opencode\`, ffmpeg for fixtures, PowerShell 5.1.

**Spec:** `architecture.md` §2 (budgets), §15 Phase 6 (this phase), §16 (acceptance), §18 (rules); the user's Phase 6 brief; `handoff.md` lessons (probe quirks) are binding for all probe work.

**Decisions (human, 2026-10-05):** budget 5 = fix harness (P-core pin + median-of-N), permanent ruling as fallback if still >0.5%; Cargo `version` bumped to `1.0.0` before the tag; ear-only criteria automated where possible + a human listening checklist delivered at close.

## Global Constraints

- **Exe size (budget 1):** `target\release\qpid.exe` ≤ **10,485,760 B** — check after every release build (start, and after any src fix). Baseline now: 10,452,992 B.
- **Baseline gates:** `cargo test` **31 passed**; release warnings **7** headers (`Select-String ': warning'`). Any src change → re-run all three gates.
- **Threads (budget 8):** ≤ 12, currently **AT CAP (11 idle / 12 playing)** — BENCH wording stays "AT CAP", never bare PASS. No new thread/timer (§18.2). Grep `thread::spawn|thread::Builder` inventory in Task 17.
- **Lane exclusivity:** no two Lane-B measurements overlap; no Lane-A subagent (audio/CPU work) runs during Lane B. Before each Lane B task: `Get-Process qpid -ErrorAction SilentlyContinue` must be empty (Global\ mutex makes stray UI instances hand off and exit silently — kill leftovers, only paths ending `target\...\qpid.exe`).
- **Probes/fixtures** live in `$env:TEMP\opencode\p6_*` — never the repo. `bench_results/` gitignored; only BENCH.md numbers are durable.
- **State isolation:** every probe sets the child's `LOCALAPPDATA` to its own dir under `$env:TEMP\opencode\` so `state.json` never touches the real profile and probes never cross-contaminate.
- **Acceptance tests run `--cli`** (exempt from single-instance, `main.rs:163`) except the two UI-mode tasks (budget-2 probe, soak) which run alone.
- **No code changes expected.** If a test/budget fails → `superpowers:systematic-debugging` → minimal fix → rebuild → re-run exe gates → **re-run every Lane B measurement taken on the old exe** (handoff lesson 7: numbers go stale on any rebuild).
- **Rule 10** (eprintln under `debug_assertions`) and **Gate 4** (`winit_030` only in `src/winit_hook.rs`) must hold at close.
- **Probe mechanics (handoff lessons 3–5):** long probes detached (`Start-Process -WindowStyle Hidden`) + `done.marker` polling; probes kill only their own process; prefer Python `subprocess` list-args over cmd quoting; device-error probe needs **≥ 2.5 s** between `d` and `p`; no `Start-Sleep 500ms` (PS 5.1).
- **Commits:** controller commits per task with explicit file lists (no `git add -A`); **doc/test subagents edit only, never commit** (avoids `index.lock` races); Lane B implementers may commit when serial. Probe scripts never committed.
- **Rulings not up for relitigation:** budget-8 revision to 12; §6.9 packet-skip and the per-packet `decode.rs` allocation remain documented, intentionally-unfixed deviations (no budget fails because of them); budget 5 gets exactly Task 12's procedure.
- **Every subagent always updates its todo list (todowrite) while working.**

## File Structure

| File | Change | Task |
|---|---|---|
| `tools/bench.py` | add `--affinity`, `--repeat` (median) | 12 |
| `BENCH.md` | security baseline; doc-sweep fixes; **final nine-budget table dated for 1.0**; Phase 6 probe sections | 6, 9, 11–18 |
| `IMPLEMENTATION.md` | stale-claim fixes, Phase 6 status at close | 7, 18 |
| `docs/USER_GUIDE.md` | accuracy sweep vs HEAD code | 8 |
| `Cargo.toml` | `version = "1.0.0"` | 20 |
| `handoff.md` | Phase 6 state refresh | 18 |
| `.superpowers/sdd/2026-10-05-phase6-final-measurement/` | briefs/reports (committed by controller) | all |

No `src/` or `ui/` changes are planned.

---

### Task 1: Acceptance §16.1 — 30-file folder numeric order (Lane A, parallel)

**Files:** none in repo. Fixtures/probe: `$env:TEMP\opencode\p6_t1\`.

- [ ] **Fixture:** `ffmpeg -y -f lavfi -i "sine=frequency=440:duration=30" -b:a 96k base.mp3`, then copy to `folder30\1.mp3 … 30.mp3` (PowerShell `1..30 | % { Copy-Item ... }`).
- [ ] **Probe (Python):** child env `LOCALAPPDATA=$env:TEMP\opencode\p6_t1\state`; `Popen([exe, "--cli", folder], stdin/stdout pipes)`, line-reader thread. After first `TrackChanged`, send `n` ×29 at 150 ms, collect titles.
- [ ] **Assert:** `count == 30`; title sequence exactly `1,2,3,…,30` (natural: `2` before `10`); first track `1` (fresh store → first not-done, §9 rule 2); no `Message(`; exit 0 on `q`.
- [ ] **Report** PASS/FAIL with the parsed sequence + raw `[event]` lines to `task-1-report.md`. No repo edits, no commits.

### Task 2: Acceptance §16.2 — single m4a picks up siblings (Lane A, parallel)

**Files:** none. Fixtures: `$env:TEMP\opencode\p6_t2\album\`.

- [ ] **Fixture:** `a.mp3`, `b.mp3` (ffmpeg sine 60 s), `target.m4a`. AAC: `ffmpeg -i base -c:a aac target.m4a`; retry `-strict -2`; **final fallback `-c:a alac`** (still `.m4a`; symphonia has `alac`). Never use the 0-byte `test_audio\test.m4a`.
- [ ] **Probe:** `Popen([exe, "--cli", "...\\album\\target.m4a"])`, isolated `LOCALAPPDATA`.
- [ ] **Assert:** first `TrackChanged` `count == 3`, `index == 3` (natural order a, b, target); `duration_ms > 0`; `n` → no further event, no `Message(`; exit 0.
- [ ] **Report** with raw event lines to `task-2-report.md`.

### Task 3: Acceptance §16.7 — corrupt file skipped with message (Lane A, parallel)

**Files:** none. Fixtures: `$env:TEMP\opencode\p6_t3\mixed\`.

- [ ] **Fixture:** `1.mp3` (30 s sine), `2.mp3` = text `this is not audio` + zero padding (not a valid stream), `3.mp3` (30 s sine).
- [ ] **Probe:** `--cli` folder; wait `TrackChanged` (index 1, count 3), send `n`.
- [ ] **Assert:** ≥1 `[event] Message(` (exact text quoted in report); next `TrackChanged` has `index == 3` (open failure → skip forward, §6.9); no crash/hang; exit 0 on `q`.
- [ ] **Report** raw event lines to `task-3-report.md`.

### Task 4: Acceptance §16.8 — unicode paths (Lane A, parallel)

**Files:** none. Fixtures: `$env:TEMP\opencode\p6_t4\música 测试 folder\`.

- [ ] **Fixture:** `01 - café naïve.mp3`, `02 - 日本語テスト.mp3`, `03 - whitespace  name.mp3` (30 s sine copies).
- [ ] **Probe:** `--cli` on the unicode folder (Python list-args, UTF-8 argv; no cmd), isolated `LOCALAPPDATA`.
- [ ] **Assert:** first `TrackChanged` title == `01 - café naïve`, folder endswith `música 测试 folder` (exact; Rust Debug keeps printable unicode); `n` walks `02 - 日本語テスト` then `03 - whitespace  name`; one `f` seek advances position ≥ 15 s (bare-Enter read); no `Message(`; exit 0.
- [ ] **Report** raw event lines to `task-4-report.md`.

### Task 5: Acceptance §16.9 — 500-file folder opens < 200 ms (Lane A wave 2, after Tasks 1–4 finish)

**Files:** none. Fixtures: `$env:TEMP\opencode\p6_t9\big500\`.

- [ ] **Fixture:** one 1 s mp3 copied to `1.mp3 … 500.mp3`.
- [ ] **Probe (Python, timestamped stdout):** isolated `LOCALAPPDATA`; `t_ready` = `Controls:` line, `t_track` = first `TrackChanged`. Open latency = `t_track − t_ready` (excludes process startup; definition stated in report).
- [ ] **Assert:** warm-run latency **< 200 ms**; also record launch→TrackChanged total and the cold first run as info. Run when no other probe is active.
- [ ] If ≥ 200 ms: **report the number, do not fix** (§18.6) — controller decides.
- [ ] **Report** all timings to `task-5-report.md`.

### Task 6: Security/privacy baseline audit (Lane A, parallel; writes BENCH.md)

**Files:** Modify `BENCH.md` (new `## Security baseline (Phase 6, 2026-10-05)` section appended before `## Known failures / caveats`). Read: `Cargo.toml`, `Cargo.lock`, `src/**`.

- [ ] Evidence: (1) `cargo tree -e normal` full list, flag network-capable crates (reqwest, hyper, tokio, mio, socket2, ureq, isahc, curl, native-tls…); (2) grep `Cargo.lock` for the same name set; (3) grep `src` for `std::net|TcpStream|UdpSocket|reqwest|hyper|http|InternetOpen|WinHttp|WinInet|WSAStartup|Win32_Networking`; (4) confirm `Cargo.toml` `windows` features contain no `Win32_Networking_*`.
- [ ] Write the section as a **baseline fact for 1.1's "no data collection" requirement** — expected verdict *zero network surface in 1.0*; list each check and result (transitive crate count, negative greps quoted). If ANY network-capable crate is found, report it honestly (name, why linked, runtime reachability) — do not force the verdict.
- [ ] **Report** findings to `task-6-report.md`. Edit only — controller commits `BENCH.md`.

### Task 7: Doc sweep — IMPLEMENTATION.md (Lane A, parallel, edit only)

- [ ] Fix stale claims vs HEAD: Phase 5 bullet "five human gates pending" → **passed 2026-10-05** (cite BENCH §15.6); "Unverified" items 2–6 (rtrb, Slint API, winresource, windows features, cpal) → **resolved by Phases 1–5 builds** — mark resolved with one-line proof; spot-check ~5 drifted `src/...:line` citations and fix those you touch.
- [ ] Update deviations: per-packet allocation + §6.9 packet-skip **intentionally not addressed in Phase 6** (ruling: budgets pass without them; recorded, not fixed).
- [ ] Leave the Phase 6 status line as "in progress — results at close" (Task 18 owns it).
- [ ] **Report** the diff summary to `task-7-report.md`. No commits.

### Task 8: Doc sweep — docs/USER_GUIDE.md (Lane A, parallel, edit only)

- [ ] Verify every behavioral claim against HEAD code (three launch modes, keyboard list, single-instance, media keys, drag-drop, speed row, persistence triggers); fix drifted `src/...:line` citations for lines you touch; flag (don't invent) any feature documented-but-missing or existing-but-undocumented.
- [ ] **Report** the diff summary to `task-8-report.md`. No commits.

### Task 9: Doc sweep — BENCH.md (Lane A wave 2, after Task 6 lands, edit only)

- [ ] Accuracy/structure pass only: fix stale cross-references, broken tables, contradictory wording. **Do not** add Phase 6 numbers (don't exist yet); do not touch the security section Task 6 just added.
- [ ] **Report** the diff summary to `task-9-report.md`. No commits.

### Task 10: Rule audits — rule 10 + gate 4 (Lane A, parallel, read-only)

- [ ] `& "C:\Program Files\Git\bin\bash.exe" -c 'grep -n -B1 "eprintln!" src/*.rs src/**/*.rs'` → expect **13 hits, 12 guarded by the previous line, 1 documented exception `src/main.rs:147`**. Any new unguarded hit = violation → report.
- [ ] `& "C:\Program Files\Git\bin\bash.exe" -c 'grep -rn "winit_030" src'` → expect **only `src/winit_hook.rs:8`**.
- [ ] **Report** both outputs verbatim to `task-10-report.md`. Read-only, no commits.

**Lane A gate:** all reports in, no unanswered FAIL; controller commits doc edits (Tasks 6/7/8/9 files) with explicit paths; `Get-Process qpid` empty → Lane B begins, nothing else dispatched until Close.

---

### Task 11: Fresh release build + full bench sweep (Lane B, serial, controller)

- [ ] `cargo build --release`; gates: size ≤ 10,485,760 B, warnings = 7, `cargo test` = 31. Record.
- [ ] Scenarios (nothing else running; redirect per-second output to files, read only summaries):

| scenario | flags | duration | budget |
|---|---|---|---|
| `idle-visible` | UI | 60 s | baseline |
| `playing-1x-visible` | UI `--file test_audio\test.mp3` | 300 s | 3 ≤ 25 MB |
| `playing-1x-minimized` | UI, env `QPID_NO_TRIM=1` | 300 s | 4 ≤ 15 MB (official) |
| `playing-1x-minimized` | UI, no env (trim armed) | 60 s | 4 info |
| `playing-2x-minimized` | `--cli` (bench presses `s`×3) | 300 s | 6 ≤ 2% |
| `paused-over-10s` | `--cli` | 60 s | 7 = 0.0% |

- [ ] **Budget 2 probe** (Python UI, sequential, kill after each): launch → first visible window via `EnumWindows` (reuse bench.py handler pattern); 5 warm + 1 cold; **median of warm ≤ 300 ms**.
- [ ] **Budget 9 probe:** release `--cli` on `test.mp3`, 60 s warmup, send `w`, parse `wakes_per_s` ≤ **2.00** (method: release CLI cross-check; Phase 4 debug-UI 0.529/s cited as supporting).
- [ ] Extract per-scenario `max USS / avg CPU / max threads` from CSVs.
- [ ] **Report** fresh budget 1/2/3/4/6/7/9 numbers + thread counts → `task-11-report.md`; controller records into BENCH Phase 6 section (commit).

### Task 12: Budget 5 — settle it now (Lane B, serial; edits `tools/bench.py`)

Decision (human): **fix harness → one confident number → else permanent ruling.**

- [ ] **Edit `tools/bench.py`:** add `--affinity "0,1,..."` (after `Popen`: `psutil.Process(pid).cpu_affinity(list)`, warn+continue on failure) and `--repeat N` (N fresh runs, print per-run avg-CPU + **median**, CSV suffix `_runN`); defaults unchanged (backward compatible).
- [ ] **Identify P-cores empirically:** `playing-1x-minimized` 60 s UI once with `--affinity 0,1,2,3,4,5,6,7` and once with `--affinity 8,9,10,11` (i5-12450HX = 4P×2 + 4E = 12 logical). Lower avg CPU for identical work = P-cores → that mask pins. If within 0.05 pp (ambiguous) or logical count ≠ 12 → fallback `GetSystemCpuSetInformation` EfficiencyClass via ctypes; report method used.
- [ ] **Confident number:** 5 runs × 300 s `playing-1x-minimized`, pinned (`--affinity <mask> --repeat 5`).
  - **median ≤ 0.5%** → budget 5 **PASS**; BENCH row updated with number **and method** (P-core pinned, median-of-5, 300 s window, EcoQoS engaged while minimized).
  - **median > 0.5%** → **permanent ruling in BENCH.md**: verdict `UNSTABLE — permanently attributed to scheduler/heterogeneous-core variance`, citing (a) Phase 4 interleaved A/B table (base 0.44–0.82 / head 0.58–0.98, Δ medians 0.03 pp, base itself fails 3/4), (b) historical same-config spread 0.11–0.89, (c) today's pinned data, (d) EcoQoS-on-minimized (Phase 5) not present in Phase 4 data. Explicit line: **not carried forward to 1.1 as an open question.**
- [ ] Commit `tools/bench.py` + BENCH row/ruling; **Report** median table → `task-12-report.md`.

### Task 13: Timing tests §16.3 + §16.4 (Lane B, serial)

**Test 3 — rapid seek ×10** (`test_audio\test.mp3`, ~60 min):

- [ ] `--cli`, isolated state; wait `StateChanged(Playing)`; bare-Enter `pos0`; send `f` ×10 at 50 ms; 1 s settle; bare-Enter `pos1`; `W` = wall time pos0→pos1.
- [ ] **Assert:** `pos1 − pos0 − 150000 ∈ [0, W + 500]`; then two reads 3 s apart differ `3000 ± 500` (not stuck); `underruns=0` via `w`; exit 0.

**Test 4 — 20 speed switches:**

- [ ] Same launch; collect `speed -> Xx` lines while sending `s` ×20 at 400 ms.
- [ ] **Assert:** exactly 20 speed lines, cycle `[1.25, 1.5, 2, 0.5, 1] ×4`, final **1x**; pos read → 5 s → pos read delta ∈ **[4500, 5500]** (no drift at 1x after flushes); `underruns=0`; no `Message(`; exit 0.
- [ ] Clicks → **listening checklist** (Task 18), not automatable.
- [ ] **Report** both tables → `task-13-report.md`; controller records into BENCH.

### Task 14: Test §16.5 — 3-hour file, seek to middle < 300 ms (Lane B, serial)

- [ ] **Fixture:** `long3h.mp3` = concat of `test_audio\tone48k_60m.mp3` ×3: `ffmpeg -y -f concat -safe 0 -i list.txt -c copy long3h.mp3`; verify `duration_ms ≈ 10,800,000 ± 5000`.
- [ ] `--cli`; send `f` ×360 buffered (engine queues; each a real seek); poll bare-Enter until `pos ≥ 5,400,000 − 3000`. Time each warm-up seek; report max (the §7.6 300 ms Coarse-fallback trigger).
- [ ] **Measured seek:** write `f\n` then `\n`; elapsed until `position_ms` returns = response. **3×**.
- [ ] **Assert:** all 3 **< 300 ms**; each lands `+15000 ± 500`; final pos ∈ `[5,400,000, 5,400,000 + wall]`; no Message; exit 0.
- [ ] Any > 300 ms → systematic-debugging (§7.6 Coarse fallback for relative seeks is the sanctioned fix), rebuild → **re-run Tasks 11–13 numbers** → re-run this test.
- [ ] **Report** → `task-14-report.md`.

### Task 15: Test §16.10 — device change (Lane B, serial)

- [ ] **Automated (Phase 5 probe re-run on this build):** `--cli` playing; send `d`; **sleep ≥ 2.5 s** (handoff lesson 2); assert `StateChanged(Paused)` + `Message(` containing `Output device changed`; wait 1.5 s, `p`, assert resume position within **±1.5 s** and `StateChanged(Playing)`. **3/3 runs.**
- [ ] Real default-device switch: not automatable without new deps (§18.1) → Phase 5 human unplug gate (**PASS 2026-10-05**) cited + one line in the Task 18 checklist ("switch default output device while playing → pauses with message").
- [ ] **Report** → `task-15-report.md`.

### Task 16: Test §16.11 — 30-minute minimized 2× soak (Lane B, serial, detached + marker)

Method split (documented in BENCH): soak proves **underruns, budget 8, budget 9** in the exact condition (minimized, EcoQoS on, 2×); **budget 6's** official number comes from Task 11's release `playing-2x-minimized` run (debug CPU not budget-comparable) — cite both.

- [ ] **Prep (isolated `LOCALAPPDATA`):** CLI on `long3h.mp3` (Task 14 fixture — 90 min content at 2×, no EOF mid-soak): `s`×3 → 2×, `q`. Verify `state.json` `"speed": 2.0` (save trigger 5) + `last_folder`/`last_file` = soak file.
- [ ] **Fresh debug build** (`cargo build`; release untouched) — `QPID_WAKE_LOG` is cfg-gated (`mod.rs:236`).
- [ ] **Launch detached debug UI, no args** → restore Paused at 2× (`restore_session` sets `speed_milli` from store, `mod.rs:850`). Env: `QPID_WAKE_LOG=<temp>\wake.log`, probe `LOCALAPPDATA`. Confirm `state.json` speed still 2. `SendInput` Space after `SetForegroundWindow` (Space probe-proven) → Playing; `ShowWindow(SW_MINIMIZE)`; assert debug stderr `[eco] EcoQoS on` **after** `visible=false`.
- [ ] **Sample 30 min** (psutil 1/s, detached + `done.marker`): avg CPU, max USS, **max threads**.
- [ ] **Assert:** wall ≥ 1800 s; **`underruns=0` on every wake.log line**; final line `wakes/uptime ≤ 2.00/s` (budget 9); **max threads ≤ 12** (budget 8); EcoQoS evidence true. CPU avg = **info** (debug build). Cleanup kills only its own pid path.
- [ ] **Report** all fields → `task-16-report.md`. Budget 6 = Task 11 row (cross-referenced).

### Task 17: Re-confirm budget 8 — threads on final build (Lane B, serial)

- [ ] `max threads` from Task 11 CSVs: idle (expect 11), playing (expect 12), paused (expect ≤ 9), plus soak max (Task 16).
- [ ] Grep inventory `grep -rn "thread::spawn\|thread::Builder" src` → sanctioned set only (engine, `qpid-pipe`, CLI event printer, rfd transients, library cpal/softbuffer). No new spawn since `40b0719`.
- [ ] **Verdict wording: `AT CAP (12/12, zero headroom)`** — never bare PASS. Count > 12 = budget failure → fix before continuing.
- [ ] **Report** → `task-17-report.md`.

---

### Task 18: Final nine-budget table + docs (Close)

- [ ] **BENCH.md:** `## Phase 6 final table — v1.0.0 (2026-10-05)` — one row per budget (# / target / fresh result / verdict / method-scenario ref), all nine, every number from Tasks 11–17 on this build (budget 5 = Task 12 verdict; budget 6 = release 2× run + soak cross-ref; budget 8 = AT CAP). Plus Phase 6 probes subsection: acceptance tests 1,2,3,4,5,7,8,9,10,11 results with evidence pointers; note §16.6 = Phase 3 PASS, §16.12 = budget 7 paused scenario PASS.
- [ ] **IMPLEMENTATION.md:** Phase 6 status bullet; measurement-status list updated (budget 5 verdict, listening checklist outstanding).
- [ ] **handoff.md:** refresh state line to "Phase 6 complete, tagged v1.0.0" (short — first file the 1.1 agent reads).
- [ ] **Human listening checklist delivered to the user:** clicks on pause/seek (§15.1), pitch at each of the 5 speeds (§15 Phase 2), clicks on 20 speed switches (§16.4), speech intelligibility at 2×, rapid-seek audio continuity (§16.3), real default-device switch (§16.10, optional — Phase 5 unplug already PASSed).
- [ ] Controller commits docs.

### Task 19: Whole-branch review (Close)

- [ ] Review package over Phase 6 range (`9207694..HEAD`) via `scripts/review-package`; dispatch final code reviewer on the most capable model (superpowers:requesting-code-review pattern) with brief + ledger parked/minor list.
- [ ] Findings → ONE fix dispatch + one scoped re-review (SDD final-review rule); residuals adjudicated with rulings in the ledger.
- [ ] Re-run final gates after any fix: `cargo test` 31, warnings 7, size ≤ cap, rule 10, gate 4.

### Task 20: Version bump + tag (Close)

- [ ] `Cargo.toml`: `version = "1.0.0"`; controller commit `Phase 6: version 1.0.0`.
- [ ] Confirm clean tree, on `master`, all gates green.
- [ ] `git tag -a v1.0.0 -m "q-pid v1.0.0"` (annotated).
- [ ] Report tag + final gate numbers to the user.

## Execution handoff

Lane A Tasks 1–4, 6, 7, 8, 10 dispatched in parallel (disjoint files/no repo edits — sanctioned by the Phase 6 brief); Tasks 5, 9 in wave 2 (dependency: 1–4, 6). Lane B Tasks 11–17 strictly serial with machine exclusivity. Close 18–20. Ledger: `.superpowers/sdd/2026-10-05-phase6-final-measurement/progress.md`.
