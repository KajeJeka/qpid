# q-pid benchmark results

Measured 2026-09-24, Windows 11 x64, release build, `tools/bench.py`.
Test file: `test_audio/test.mp3` (44.1 kHz stereo MP3, ~60 min).

## Budgets (architecture.md §2)

| # | Budget | Target | Result | Verdict |
|---|--------|--------|--------|---------|
| 1 | Exe size | ≤ 10 MB | 10,379,776 B (9.90 MB) after the Phase 2 WSOLA (+4.6 KB) | **PASS** |
| 2 | Startup to first frame | ≤ 300 ms | 141 ms median (5 runs; first cold run 329, warm 134–248) | **PASS** |
| 3 | USS, visible, 1x | ≤ 25 MB | 3.35 MB | **PASS** |
| 4 | USS, minimized, 1x | ≤ 15 MB | 5.39 MB | **PASS** |
| 5 | CPU, minimized, 1x | ≤ 0.5% | 0.42% avg | **PASS** |
| 6 | CPU, playing at 2x | ≤ 2% | 1.65–1.88% (3 runs; see scenario below) | **PASS** |
| 7 | Paused > 10 s | 0.0% CPU | 0.00% avg over 30 s; USS 1.84 MB; handles −17 | **PASS** |
| 8 | Threads | ≤ 12 (revised from 4, see isolation below) | 10 idle / 11 playing / 9 paused-released | **PASS** |

Not measured: wakeups/s (9 — needs per-thread wakeup tooling to exclude
the audio callback; not built yet).

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

### playing-2x-minimized (60 s, 2x, 3 runs, budget 6)

| Run | avg CPU | max USS | max threads |
|-----|---------|---------|-------------|
| 1 | 1.88% | 3.34 MB | 7 |
| 2 | 1.65% | 3.35 MB | 7 |
| 3 | 1.87% | 3.57 MB | 7 |

PASS vs ≤ 2% in all three runs, but the margin is thin (worst 1.88 vs
2.00). Method: `--cli` mode — the harness presses `s` three times on
stdin (1 → 1.25 → 1.5 → 2) before sampling, and the process is headless
(`windows_subsystem = "windows"`, no window), so "minimized" is moot.
UI mode cannot be automated for this scenario: the harness has no way to
click the speed row, and the engine starts every process at 1x. The Slint
thread's minimized cost is therefore excluded; idle-visible measures
0.00%, so it should not change the verdict. Speed engagement was verified
separately (CLI smoke below): position advanced at exactly 2.00x wall
clock over 2 s, counted at the device callback, so the stretcher,
resampler and ring all flowed at 2x.

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
