#!/usr/bin/env python3
"""Benchmark Q-pid against the budgets in architecture.md section 2.

Launches the exe, drives it into the requested scenario, samples resource
usage once per second for the sampling window, and writes a CSV.

Scenarios (architecture.md section 15, Phase 0 step 3):
    idle-visible            window open, nothing loaded
    playing-1x-visible      playing a test file at 1x, window visible
    playing-1x-minimized    playing at 1x, window minimized
    playing-2x-minimized    playing at 2x, window minimized (Phase 2+)
    paused-over-10s         paused, sampled past the 10s release window

Usage:
    python tools/bench.py --exe target/release/qpid.exe --scenario idle-visible
    python tools/bench.py --exe target/release/qpid.exe --scenario playing-1x-minimized \
        --file test_audio/test.mp3 --duration 60

    Pin + median-of-N (budget 5 harness):
    python tools/bench.py --exe target/release/qpid.exe --scenario playing-1x-minimized \
        --file test_audio/test.mp3 --affinity 0,1,2,3,4,5,6,7 --repeat 5

This drives the UI binary (`qpid.exe <file>` autoplays via `run_ui`, section
15/main.rs). Pass --cli to instead launch `qpid.exe --cli <file>` for
engine-only measurement without the UI thread — useful for isolating engine
cost from UI cost when a budget fails (architecture.md section 15, Phase 6
step 2 tuning order). Note: playing-2x-minimized must run with --cli —
the harness presses `s` three times on stdin to reach 2x, which only the
CLI binary reads; in UI mode there is no automated way to set speed and
the run would silently measure 1x.

Requires: psutil (pip install psutil --break-system-packages)
On Windows only for minimize/foreground control (uses ctypes/user32); the
process-sampling portion runs on any OS psutil supports, so `idle-visible`
can be sanity-checked cross-platform, but minimize-dependent scenarios need
Windows.
"""
import argparse
import csv
import ctypes
import platform
import statistics
import subprocess
import sys
import time
from pathlib import Path

try:
    import psutil
except ImportError:
    print("psutil is required: pip install psutil --break-system-packages", file=sys.stderr)
    sys.exit(1)

SCENARIOS = [
    "idle-visible",
    "playing-1x-visible",
    "playing-1x-minimized",
    "playing-2x-minimized",
    "paused-over-10s",
]

# Budgets from architecture.md section 2, for the printed pass/fail summary.
BUDGETS_MB = {
    "idle-visible": 25.0,
    "playing-1x-visible": 25.0,
    "playing-1x-minimized": 15.0,
    "playing-2x-minimized": 15.0,
}
BUDGETS_CPU_PCT = {
    "playing-1x-minimized": 0.5,
    "playing-2x-minimized": 2.0,
    "paused-over-10s": 0.0,
}


def minimize_window(pid: int) -> bool:
    """Best-effort minimize via user32. Windows only. Returns True if a
    window was found and minimized."""
    if platform.system() != "Windows":
        print("minimize is only supported on Windows; skipping", file=sys.stderr)
        return False

    user32 = ctypes.windll.user32
    found_hwnd = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_int, ctypes.c_int)
    def enum_handler(hwnd, _lparam):
        found_pid = ctypes.c_ulong()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(found_pid))
        if found_pid.value == pid and user32.IsWindowVisible(hwnd):
            found_hwnd.append(hwnd)
            return False
        return True

    user32.EnumWindows(enum_handler, 0)
    if not found_hwnd:
        return False

    SW_MINIMIZE = 6
    user32.ShowWindow(found_hwnd[0], SW_MINIMIZE)
    return True


def sample_process(proc: psutil.Process) -> dict:
    with proc.oneshot():
        cpu_pct = proc.cpu_percent(interval=None)
        mem = proc.memory_full_info() if hasattr(proc, "memory_full_info") else proc.memory_info()
        # USS (Unique Set Size) is the closest cross-platform equivalent to
        # "private working set" in the budgets. Falls back to RSS if the
        # platform/permissions don't expose USS.
        uss_bytes = getattr(mem, "uss", None) or mem.rss
        threads = proc.num_threads()
        try:
            handles = proc.num_handles() if hasattr(proc, "num_handles") else 0
        except psutil.AccessDenied:
            handles = 0
    return {
        "cpu_pct": cpu_pct,
        "uss_mb": uss_bytes / (1024 * 1024),
        "threads": threads,
        "handles": handles,
    }


def run_scenario(args) -> None:
    avg_cpus = []
    for i in range(args.repeat):
        suffix = f"_run{i + 1}" if args.repeat > 1 else ""
        if args.repeat > 1:
            print(f"\n=== run {i + 1}/{args.repeat} ===")
        avg_cpus.append(run_once(args, suffix))

    if args.repeat > 1:
        print("\n--- repeat summary ---")
        for i, a in enumerate(avg_cpus, 1):
            print(f"run {i} avg CPU: {a:.2f}%")
        print(f"median avg CPU: {statistics.median(avg_cpus):.2f}%  ({args.repeat} runs)")


def run_once(args, suffix: str) -> float:
    exe = Path(args.exe)
    if not exe.exists():
        print(f"exe not found: {exe}", file=sys.stderr)
        sys.exit(1)

    cmd = [str(exe)]
    if args.cli:
        cmd.append("--cli")
    if args.file:
        cmd.append(args.file)

    print(f"launching: {' '.join(cmd)}")
    proc_handle = subprocess.Popen(cmd, stdin=subprocess.PIPE if args.cli else None)
    time.sleep(1.0)  # let it reach the first frame before sampling

    try:
        proc = psutil.Process(proc_handle.pid)
    except psutil.NoSuchProcess:
        print("process exited immediately; check the exe path/args", file=sys.stderr)
        sys.exit(1)

    if args.affinity:
        try:
            cpus = [int(c) for c in args.affinity.split(",") if c.strip()]
            proc.cpu_affinity(cpus)
            print(f"pinned to CPUs {cpus}")
        except (ValueError, psutil.Error) as e:
            print(f"warning: could not set CPU affinity ({e}); continuing unpinned", file=sys.stderr)

    proc.cpu_percent(interval=None)  # prime the counter (first call is always 0)

    if "minimized" in args.scenario:
        time.sleep(0.5)
        if not minimize_window(proc_handle.pid):
            print("warning: could not find/minimize the window (non-Windows host?)", file=sys.stderr)

    if args.scenario == "paused-over-10s":
        if args.cli:
            print("sending pause ('p') over stdin...")
            try:
                proc_handle.stdin.write(b"p\n")
                proc_handle.stdin.flush()
            except (BrokenPipeError, AttributeError):
                print("could not write to stdin; is stdin piped? (Popen needs stdin=PIPE)", file=sys.stderr)
        else:
            print(
                "UI mode: this scenario requires the app to already be paused.\n"
                "Click pause in the window, or re-run with --cli for automatic pausing.",
                file=sys.stderr,
            )
        print("waiting past the 10s pause-release window before sampling...")
        time.sleep(11.0)
        # Re-prime so the first sample's window starts after the release,
        # not back when the process was still playing (it would attribute
        # pre-pause CPU to the paused run and fail the 0.0% budget).
        proc.cpu_percent(interval=None)

    if args.scenario == "playing-2x-minimized":
        if args.cli:
            # Fresh process starts at 1x; the CLI 's' key cycles
            # 1 -> 1.25 -> 1.5 -> 2, so three presses reach 2x.
            for _ in range(3):
                proc_handle.stdin.write(b"s\n")
                proc_handle.stdin.flush()
                time.sleep(0.5)  # each press is a flush (~120 ms)
        else:
            print(
                "UI mode: this scenario needs the app set to 2x.\n"
                "Switch speed in the window, or re-run with --cli.",
                file=sys.stderr,
            )

    rows = []
    print(f"sampling for {args.duration}s (1 sample/sec)...")
    for second in range(args.duration):
        time.sleep(1.0)
        if not proc.is_running():
            print(f"process exited after {second}s", file=sys.stderr)
            break
        try:
            sample = sample_process(proc)
        except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
            print(f"lost access to process at t={second}s: {e}", file=sys.stderr)
            break
        sample["t"] = second
        rows.append(sample)
        print(
            f"  t={second:4d}s  cpu={sample['cpu_pct']:5.1f}%  "
            f"uss={sample['uss_mb']:6.2f} MB  threads={sample['threads']}  "
            f"handles={sample['handles']}"
        )

    proc_handle.terminate()
    try:
        proc_handle.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc_handle.kill()

    out_path = Path(args.outdir) / f"{args.scenario}{suffix}.csv"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["t", "cpu_pct", "uss_mb", "threads", "handles"])
        writer.writeheader()
        writer.writerows(rows)
    print(f"wrote {out_path}")

    summarize(args.scenario, rows)
    return sum(r["cpu_pct"] for r in rows) / len(rows) if rows else 0.0


def summarize(scenario: str, rows: list) -> None:
    if not rows:
        print("no samples collected; cannot summarize")
        return

    avg_cpu = sum(r["cpu_pct"] for r in rows) / len(rows)
    max_uss = max(r["uss_mb"] for r in rows)
    max_threads = max(r["threads"] for r in rows)

    print(f"\n--- summary: {scenario} ---")
    print(f"avg CPU:    {avg_cpu:.2f}%")
    print(f"max USS:    {max_uss:.2f} MB")
    print(f"max threads:{max_threads}")

    mem_budget = BUDGETS_MB.get(scenario)
    if mem_budget is not None:
        status = "PASS" if max_uss <= mem_budget else "FAIL"
        print(f"memory budget ({mem_budget} MB): {status}")

    cpu_budget = BUDGETS_CPU_PCT.get(scenario)
    if cpu_budget is not None:
        status = "PASS" if avg_cpu <= cpu_budget else "FAIL"
        print(f"CPU budget ({cpu_budget}%): {status}")

    # Budget 8, revised from 4 to 12 (architecture.md §2; evidence: the
    # thread isolation table in BENCH.md).
    if max_threads > 12:
        print(f"thread budget (12): FAIL ({max_threads} threads)")
    else:
        print("thread budget (12): PASS")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--exe", required=True, help="path to the built qpid executable")
    parser.add_argument("--scenario", required=True, choices=SCENARIOS)
    parser.add_argument("--file", help="audio file to open (required for playing-* and paused-* scenarios)")
    parser.add_argument("--cli", action="store_true", help="launch qpid.exe --cli <file> instead of the UI binary")
    parser.add_argument("--duration", type=int, default=300, help="sampling window in seconds (default 300)")
    parser.add_argument("--outdir", default="bench_results", help="directory for CSV output")
    parser.add_argument("--affinity", help='comma-separated CPUs to pin the target to, e.g. "0,1,2,3"')
    parser.add_argument("--repeat", type=int, default=1, help="fresh runs; prints per-run avg CPU and the median (default 1)")
    args = parser.parse_args()

    if args.repeat < 1:
        print("--repeat must be >= 1", file=sys.stderr)
        sys.exit(1)

    if args.scenario != "idle-visible" and not args.file:
        print(f"--file is required for scenario {args.scenario}", file=sys.stderr)
        sys.exit(1)

    run_scenario(args)


if __name__ == "__main__":
    main()
