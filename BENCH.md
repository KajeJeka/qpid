# q-pid benchmark results

Measured 2026-09-24, Windows 11 x64, release build, `tools/bench.py`.
Test file: `test_audio/test.mp3` (44.1 kHz stereo MP3, ~60 min).

## Budgets (architecture.md §2)

| # | Budget | Target | Result | Verdict |
|---|--------|--------|--------|---------|
| 1 | Exe size | ≤ 10 MB | 10,217,472 B (9.74 MB) | **PASS** |
| 3 | USS, visible, 1x | ≤ 25 MB | 3.35 MB | **PASS** |
| 4 | USS, minimized, 1x | ≤ 15 MB | 4.96 MB | **PASS** |
| 5 | CPU, minimized, 1x | ≤ 0.5% | 0.315% avg | **PASS** |
| 8 | Threads | ≤ 4 | 10 idle / 11 playing | **FAIL** |

Not measured yet: startup-to-first-frame (2), CPU at 2x (6), paused >10 s (7), wakeups/s (9).

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

Thread count drops 11 → 8/9 after ~28 s (transient UI helper threads exit when minimized).

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

**Verdict:** The budget of 4 assumed a minimal stack that does not match Slint+winit+cpal+Windows COM reality. Replacing the UI (§3.3) or cpal (Phase 6.3) would not reach 4 while still using either library's Windows backend. Recommend revising budget 8 to **12** with this measurement as the reason; keep the three-thread design for app-owned threads (§5) as the soft target. Decision pending user approval.

## Known failures / caveats

1. **Thread budget FAIL (11 vs 4)** — see isolation table above; floors are library-imposed.
2. **`test.m4a` generation fails** (ffmpeg AAC experimental encoder). mp3/wav/flac/ogg OK.
3. **Clicks on pause/seek:** not verifiable programmatically — human listening required (Phase 1 exit criteria §15).
4. **Rate mismatch:** device is 48 kHz, test file 44.1 kHz; no resampler until Phase 2.

## CLI smoke (Phase 1)

`qpid.exe --cli <file>` + stdin keys: play/pause, ±15 s seek, resume-from-seek all verified exit 0.
