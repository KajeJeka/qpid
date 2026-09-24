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

## Known failures / caveats

1. **Thread budget FAIL (11 vs 4).** Named threads: main, `qpid-engine`, `softbuffer_*` (Slint renderer), `cpal_wasapi_out`, plus a UI event-drain thread and unnamed winit/Windows helpers. Architecture §5 allows 3 (+1 for Phase 5 pipe). Needs investigation before Phase 2 features (rule 18: fix budget failures before adding features).
2. **`test.m4a` generation fails** (ffmpeg AAC experimental encoder). mp3/wav/flac/ogg OK.
3. **Clicks on pause/seek:** not verifiable programmatically — human listening required (Phase 1 exit criteria §15).
4. **Rate mismatch:** device is 48 kHz, test file 44.1 kHz; no resampler until Phase 2.

## CLI smoke (Phase 1)

`qpid.exe --cli <file>` + stdin keys: play/pause, ±15 s seek, resume-from-seek all verified exit 0.
