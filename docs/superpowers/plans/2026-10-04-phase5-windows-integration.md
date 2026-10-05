# Phase 5: Windows Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Phase 5 of architecture.md — EcoQoS + working-set trim, device/sleep handling, drag-and-drop, single instance, optional media keys — and pass every §15.6 exit gate.

**Architecture:** All Windows API surface goes in the new `src/winapi.rs` (spec §13). Power/visibility changes ride the existing winit visibility sink; device errors ride one new `Shared` atomic polled on the engine's existing wakes; single instance is a named mutex plus a named pipe whose listener is the one sanctioned 4th thread (spec §5). No new timers, no new threads beyond that listener (spec §18).

**Tech Stack:** Rust 2021, windows 0.58 (feature additions only as the compiler asks), cpal 0.15, slint 1.18.1 + winit 0.30 (via `winit_hook.rs` only), PowerShell probes.

**Spec:** `architecture.md` — §2 budgets 1/3/4/8, §11.7 (EcoQoS), §11.8 (trim), §12.3 (device/sleep), §12.5 rules 4–5 (single instance, media keys), §13 (layout), §15 Phase 5 order + exit, §18 (no new threads/timers), §5 (thread 4 allowance), §9 rule 10 (eprintln discipline).

## Global Constraints

- Exe ≤ 10,485,760 B (baseline 52b4280: 10,443,776 B — ~41 KB margin). **Check `(Get-Item target\release\qpid.exe).Length` after EVERY task's release build, not just Task 5** — Task 4 can eat margin with new `windows` features before Task 5 measures it.
- Threads ≤ 12 (revised budget 8). Task 4 projects 11→12 playing: when that lands, BENCH.md must record **"AT CAP — zero headroom"**, never a clean PASS, so the next thread-adding change fails loudly.
- Tests 31/31 after every task; release warnings baseline 7 (identical list).
- Budget 4 (minimized playing ≤ 15 MB USS) is measured **without** the trim (`QPID_NO_TRIM=1`). The trim must never be what passes budget 4.
- Rule 10: every `eprintln!` sits behind `cfg!(debug_assertions)` or inside a `#[cfg(debug_assertions)]` block — audit the line ABOVE each call, not after.
- Gate 4: `winit_030` / `slint::winit_030` may appear ONLY in `src/winit_hook.rs`.
- No new timers or threads except the single-instance pipe listener (spec §18/§5).
- Add `windows` crate features ONLY when the compiler errors on a missing module (spec §4.7).

### Speed rules (from review; binding on every task)

1. **Debug builds for all correctness/logic verification.** Exactly **one release build per task, at the very end**, for exe-size/warnings/budget gates. Never release-build mid-task (fat LTO + codegen-units=1 is minutes per build).
2. **Tasks 2 and 3 dispatch in parallel** (disjoint files: `engine/*` vs `winit_hook.rs`/`ui.rs`), same for their reviews.
3. **The Task 1 soak blocks only CPU%-sensitive benches.** Implementation of Tasks 2–5 proceeds during the 30-minute soak. Thread/handle-count snapshot runs (count is noise-immune) may run during it; CPU% budget runs may not.
4. **Probe windows:** 5–10 s for binary state checks (timer stopped? position held?); long windows (300 s) only for averaged percentages (CPU%).
5. **Task 4 splits into two dispatches if it runs long:** writer side (mutex + pipe write + second-instance exit) and reader side (listener thread + OpenPath forwarding + raise_window), reviewed separately.
6. **Re-review cycles point at what changed since the last review** — do not hand the reviewer architecture.md/handoff.md and the full diff again; only the fix diff plus the prior review's finding list.

### Environment / probe conventions (Phase 3–4 lessons)

- Probe scripts: `.ps1` files in `$env:TEMP\opencode\`, run via `powershell -NoProfile -ExecutionPolicy Bypass -File ...`. Never `Start-Sleep <big int>` without unit awareness (`Start-Sleep 11` = seconds; use `-Milliseconds` when unsure).
- CLI probes: `ProcessStartInfo` with `FileName='cmd.exe'`, `Arguments='/c ""exe" --cli "file" > "out" 2>&1"'`, `RedirectStandardInput=$true`, drive with `$p.StandardInput.WriteLine(...)`.
- UI probes: redirect `$env:LOCALAPPDATA` to a temp dir and delete `state.json` before every run (state leaks between runs). `FindWindow` fails on this box — use `$p.MainWindowHandle` + `ShowWindow`.
- Bash for the SDD scripts: `"C:\Program Files\Git\bin\bash.exe"`.

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/winapi.rs` | Create | EcoQoS, working-set trim, thread priority (moved), single instance (mutex/pipe/raise), media keys (Task 5) |
| `src/ui.rs` | Modify (sink at :305, wire at :162) | Visibility sink → EcoQoS+trim; drop sink registration |
| `src/winit_hook.rs` | Modify | Forward `DroppedFile` to a second sink (only file allowed to name winit_030) |
| `src/engine/mod.rs` | Modify (`Shared` :123, loop :283, `pause` :1145, remove `lower_thread_priority` :1223) | `underruns`/`device_error` atomics, device-error wake check, `device_lost()`, pause fold, CLI `d` |
| `src/engine/output.rs` | Modify (`err_fn` :101, callback `_` arm :234) | Real device-error flag, underrun counting |
| `src/main.rs` | Modify (`run_ui` :152) | Single-instance claim, pipe listener wiring, media keys start |
| `Cargo.toml` | Modify (Task 5 only, maybe Task 4) | New `windows` features as compiler asks |

---

### Task 1: EcoQoS + working-set trim + underrun counter (+ soak launch)

**Files:**
- Create: `src/winapi.rs`
- Modify: `src/main.rs` (add `mod winapi;`), `src/ui.rs:305-311`, `src/engine/mod.rs` (`Shared` :123-156, wake-log :243-245, remove :1223-1237, call site :201), `src/engine/output.rs` (callback :231-241, `w` output in `src/main.rs` :127-137)

**Interfaces:**
- Produces (used by Tasks 2–6): `winapi::set_ecoqos(bool)`, `winapi::trim_working_set()`, `winapi::lower_thread_priority()`, `Shared.underruns: AtomicU64`.
- Produces (soak): background soak process running a debug-exe copy with `QPID_WAKE_LOG`, checked in Task 6.

- [ ] **Step 1: Create `src/winapi.rs`**

```rust
//! Windows process/power integration (architecture.md section 11.7 and 13).
//! Everything here is best-effort: a failed call must never break the UI.

/// Section 11.7 rule 7: EcoQoS (execution-speed throttling) follows window
/// visibility. ControlMask and StateMask both carry EXECUTION_SPEED when
/// enabled; StateMask 0 restores normal scheduling.
pub fn set_ecoqos(enabled: bool) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, ProcessPowerThrottling,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_STATE, SetProcessInformation,
        };
        let mut state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: if enabled {
                PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            } else {
                0
            },
        };
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const core::ffi::c_void,
            core::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
    #[cfg(not(windows))]
    let _ = enabled;
    if cfg!(debug_assertions) {
        eprintln!("[eco] EcoQoS {}", if enabled { "on" } else { "off" });
    }
}

/// Section 11.8 / Phase 5 item 1: optional working-set trim on hide.
/// QPID_NO_TRIM disables it so budget 4's official number is measured
/// without the trim (section 2 budget 4).
pub fn trim_working_set() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, SetProcessWorkingSetSize,
        };
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(windows)]
pub fn lower_thread_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
pub fn lower_thread_priority() {
    // No-op on non-Windows dev hosts (project targets Windows 11; cargo
    // check elsewhere should still compile).
}
```

Note: the `eprintln!` at the end of `set_ecoqos` is already `cfg!(debug_assertions)`-guarded (rule 10).

- [ ] **Step 2: Wire the module and move the priority call**

`src/main.rs` after `mod winit_hook;`:
```rust
mod winapi;
```

`src/engine/mod.rs`: delete both `lower_thread_priority` definitions (:1223-1237) and replace the call at :201 with `crate::winapi::lower_thread_priority();`.

- [ ] **Step 3: Add `underruns` to `Shared`**

`src/engine/mod.rs` — new field after `wakes` (:136):
```rust
    // Phase 5 soak (section 11.7 rule 7): callback silence-pads observed
    // while audio should be flowing. Excluded: flush path, EOF drain,
    // pre-first-frame startup (guarded in the callback).
    pub underruns: AtomicU64,
```
Init after `wakes: AtomicU64::new(0),` (:153): `underruns: AtomicU64::new(0),`

`src/engine/output.rs` — in `audio_callback`'s underrun arm (:234-240), before `break`:
```rust
            _ => {
                if !shared.eof.load(Ordering::Relaxed)
                    && shared.played_frames.load(Ordering::Relaxed) > 0
                {
                    shared.underruns.fetch_add(1, Ordering::Relaxed);
                }
                break;
            }
```
(Flush returns early above; EOF drain is excluded by `eof`; startup before the first frame by `played_frames > 0`; pauses stop the stream so the callback does not run.)

- [ ] **Step 4: Surface the counter (debug log + CLI `w`)**

`src/engine/mod.rs` wake-log block (:243-245):
```rust
                    let n = shared.wakes.load(Ordering::Relaxed);
                    let t = shared.started.elapsed().as_secs_f64();
                    let u = shared.underruns.load(Ordering::Relaxed);
                    let _ = writeln!(f, "wakes={n} uptime_s={t:.1} underruns={u}");
```

`src/main.rs` `w` arm (:131-136): add `underruns={}` to the println:
```rust
                let underruns = shared.underruns.load(Ordering::Relaxed);
                println!(
                    "wakes={wakes} uptime_s={up:.1} wakes_per_s={:.2} underruns={underruns}",
                    wakes as f64 / up.max(0.001)
                );
```

- [ ] **Step 5: EcoQoS + trim in the visibility sink**

`src/ui.rs:305`:
```rust
    crate::winit_hook::set_sink(|visible| {
        // Section 11.7 rule 7: EcoQoS follows visibility (minimize or
        // occlusion). Trim rides the same transition (Phase 5 item 1,
        // optional); QPID_NO_TRIM is the measurement escape hatch budget 4
        // requires. Runs inline on the event-loop thread: both calls are
        // cheap and §18 forbids timers not in the document.
        crate::winapi::set_ecoqos(!visible);
        if !visible && std::env::var_os("QPID_NO_TRIM").is_none() {
            crate::winapi::trim_working_set();
        }
        with_tc(|tc| {
            if let Some(t) = tc {
                t.set_visible(visible);
            }
        });
    });
```

- [ ] **Step 6: Verify (debug only)**

Run: `cargo test` — expect 31/31.
Run: `cargo build` — expect 0 errors, warnings ≤ baseline (new dead-code warnings get `#[allow(dead_code)]` only if justified).
Manual probe (debug exe): launch UI, minimize → stderr shows `[vis] ... visible=false` then `[eco] EcoQoS on`; restore → `[eco] EcoQoS off`. Restore with `QPID_NO_TRIM=1` → no crash, no trim (trim is silent; the visible check is the EcoQoS pair). 5–10 s window is enough — this is a binary state check.

- [ ] **Step 7: One release build + budget gates + exe size**

Run: `cargo build --release 2>&1 | Select-String "warning" | Measure-Object -Line` — expect warning count 7 (same list as baseline).
Run: `(Get-Item target\release\qpid.exe).Length` — must be ≤ 10,485,760; record number (margin was ~41 KB).
Run (official budget 4, no trim, 300 s — long window justified: same scenario as Phase 4's recorded number):
```powershell
$env:QPID_NO_TRIM = "1"; $env:LOCALAPPDATA = "$env:TEMP\opencode\qpid-p5-b4"
Remove-Item "$env:LOCALAPPDATA\qpid\state.json" -ErrorAction SilentlyContinue
python tools/bench.py --exe target/release/qpid.exe --scenario playing-1x-minimized --file test_audio/tone48k_60m.mp3 --duration 300 --outdir bench_results\p5-t1-notrim
Remove-Item Env:\QPID_NO_TRIM
```
Gate: USS ≤ 15.0 MB; record number.
Run (informational with trim, 60 s — absolute MB, not a percentage, so short window):
```powershell
$env:LOCALAPPDATA = "$env:TEMP\opencode\qpid-p5-b4t"
python tools/bench.py --exe target/release/qpid.exe --scenario playing-1x-minimized --file test_audio/tone48k_60m.mp3 --duration 60 --outdir bench_results\p5-t1-trim
```
Record the trim-on number (info only).

- [ ] **Step 8: Launch the 30-minute soak (last, after benches)**

```powershell
$soak = "$env:TEMP\opencode\qpid-p5-soak"
New-Item -ItemType Directory -Force -Path $soak | Out-Null
Copy-Item target\debug\qpid.exe "$soak\qpid.exe" -Force   # debug: [vis]/[eco] stderr lines must be visible
Remove-Item "$soak\state.json" -ErrorAction SilentlyContinue
$env:LOCALAPPDATA = $soak
$env:QPID_WAKE_LOG = "$soak\wake.log"
$psi = New-Object Diagnostics.ProcessStartInfo
$psi.FileName = 'cmd.exe'
$psi.Arguments = "/c `"`"$soak\qpid.exe`" `"`"test_audio\tone48k_60m.mp3`" > `"$soak\stderr.txt`" 2>&1`""
$psi.UseShellExecute = $false
$p = [Diagnostics.Process]::Start($psi)
Start-Sleep 5
$p.Refresh()
Add-Type -Namespace W -Name U -MemberDefinition '[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);'
[W.U]::ShowWindow($p.MainWindowHandle, 6)  # SW_MINIMIZE
```
Wait ≥10 s, then assert `$soak\stderr.txt` contains `[vis] min=true` → `visible=false` AND `[eco] EcoQoS on`, and `wake.log` accumulates `underruns=0` lines. `tone48k_60m.mp3` is 60 min → no track transition inside the soak window (transition/handoff windows must not produce false underruns). Record the process id for Task 6 (`Get-Process qpid | Where-Object {$_.Path -like "*qpid-p5-soak*"}`).

**Soak rule: Tasks 2–5 implementation proceeds during the soak. Only CPU%-sensitive bench runs wait for it.** Do not kill or idle-wait on the soak.

- [ ] **Step 9: Commit**

```bash
git add src/winapi.rs src/main.rs src/ui.rs src/engine/mod.rs src/engine/output.rs
git commit -m "Phase 5: EcoQoS + working-set trim on hide, underrun counter, start soak"
```

---

### Task 2: Device/sleep handling + pause-position fold

**Files:**
- Modify: `src/engine/mod.rs` (`Shared` :123, loop after :283 match, `pause` :1145, CLI `d` in `src/main.rs` :83-141), `src/engine/output.rs` (`err_fn` :101)

**Interfaces:**
- Consumes: nothing from Task 1 (parallel-dispatchable with Task 3).
- Produces: `Shared.device_error: AtomicBool` (engine polls, `run_cli` sets for probes); `pause()` folds position (fixed semantics for release/resume); probe `d` command.

**Dispatch note:** Task 2 and Task 3 go out in ONE message as parallel subagents (disjoint files). Reviews likewise.

- [ ] **Step 1: Fold the position inside `pause()` (the pre-existing release-position bug)**

`src/engine/mod.rs:1157`, insert right after the `save_now(...)` line of `pause()`:
```rust
    // Section 8.2 position fix (Phase 5 ruling): fold the live position
    // into base_ms NOW. resume()'s released branch reads base_ms only, so
    // without this a pause -> 10 s release -> resume restarted at the last
    // seek point instead of the pause point. The fold is position-
    // preserving (base = position_ms, frames = 0) and save_now above
    // already recorded the same value.
    let pos = shared.position_ms();
    shared.base_ms.store(pos, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
```
No other call-site changes: `pause()` only runs from `Command::Pause`/`TogglePlay` while Playing (:481-492), and `device_lost` below reuses it.

- [ ] **Step 2: `device_error` flag + real `err_fn`**

`src/engine/mod.rs` `Shared`, after `wakes` (plus the Task 1 `underruns` field if already merged):
```rust
    // Section 12.3 rule 3: cpal error callback (device removed / sleep)
    // sets this; the engine swaps it false on its next wake.
    pub device_error: AtomicBool,
```
Init: `device_error: AtomicBool::new(false),`

`src/engine/output.rs:99-107` — err_fn now captures a shared clone:
```rust
        let shared_cb = Arc::clone(&shared);
        let shared_ef = Arc::clone(&shared);

        let err_fn = move |_err: cpal::StreamError| {
            // Section 12.3: device error (headphone unplug, sleep). cpal
            // calls this off the audio thread; one atomic store, no logging
            // or allocation — the engine polls it on its next wake.
            shared_ef.device_error.store(true, Ordering::Release);
        };
```

- [ ] **Step 3: Engine wakes handle the flag**

`src/engine/mod.rs` — insert between the end of the `match rx.recv_timeout(wait) {...}` block (:322) and the checkpoint block (:324):
```rust
        // Section 12.3 rule 3: device error flagged by cpal (unplug /
        // sleep) — handle on this wake: save, Paused, drop the stream,
        // Message. The next Play rebuilds on the current default device.
        // No auto-resume.
        if shared.device_error.swap(false, Ordering::Relaxed) {
            device_lost(
                &mut state,
                &mut ctx,
                &mut paused_since,
                &mut released_path,
                &shared,
                &tx_events,
                &mut store,
            );
        }
```

New function next to `pause()`:
```rust
/// Section 12.3 rule 3: cpal reported a device error. While Playing this
/// is a full pause (save + fold via pause()); otherwise just drop the
/// stream and keep the state (Ended stays Ended, Idle stays Idle). The
/// saved path goes to released_path so resume/restart rebuilds on the
/// current default device.
fn device_lost(
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    paused_since: &mut Option<std::time::Instant>,
    released_path: &mut Option<PathBuf>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    store: &mut Store,
) {
    if *state == PlayState::Playing {
        pause(ctx, state, paused_since, shared, tx_events, store);
    }
    if let Some(c) = ctx.take() {
        *released_path = Some(c.path);
        *paused_since = None; // release timer irrelevant without ctx
        if cfg!(debug_assertions) {
            eprintln!("[device] error -> stream released, state {state:?}");
        }
    }
    let _ = tx_events.send(Event::Message(
        "Output device changed — press Play to resume".into(),
    ));
}
```

- [ ] **Step 4: CLI probe command `d`**

`src/main.rs` match, new arm after `w` (:127-137) — and add `d=device-error` to the Controls println at :83:
```rust
            "d" => {
                // Phase 5 probe: simulate cpal's device-error callback
                // (run_cli is the dev-only CLI; rule 10's no-log rule does
                // not apply to its existing stdout prints).
                shared.device_error.store(true, Ordering::Relaxed);
            }
```

- [ ] **Step 5: Verify — fold probe (debug build)**

Probe script `$env:TEMP\opencode\p5-fold.ps1` (established pattern):
```powershell
$exe = "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\target\debug\qpid.exe"
$mp3 = "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\test_audio\tone48k_60m.mp3"
$out = "$env:TEMP\opencode\p5-fold-out.txt"
$psi = New-Object Diagnostics.ProcessStartInfo
$psi.FileName = 'cmd.exe'
$psi.Arguments = "/c `"`"$exe`" --cli `"$mp3`" > `"$out`" 2>&1`""
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$p = [Diagnostics.Process]::Start($psi)
Start-Sleep 3                 # autoplay into the track
$p.StandardInput.WriteLine('f')   # +15 s seek -> base=15000
$p.StandardInput.WriteLine('f')   # +15 s seek -> base=30000
Start-Sleep 6                 # play on: frames accumulate ~6 s past base
$p.StandardInput.WriteLine('')    # P1 ~= 36000
$p.StandardInput.WriteLine('p')   # pause (fold fires)
Start-Sleep 11                 # 10 s release fires (ctx dropped)
$p.StandardInput.WriteLine('p')   # resume via released branch (base_ms)
Start-Sleep 1
$p.StandardInput.WriteLine('')    # P2
Start-Sleep 1
$p.StandardInput.WriteLine('q')
$null = $p.WaitForExit(8000)
Get-Content $out
```
Asserts: last two `position_ms` lines — **P2 ≥ 33000 and P2 ≥ P1 − 1500** (buggy pre-fix behavior: P2 ≈ 30000, off by the ~6 s of frames the fold now preserves). Also `[store] save (pause)` appears; after the 11 s wait, `[release]` line.

- [ ] **Step 6: Verify — device probe (debug build)**

Same script shape, file playing, commands: blank line (P1), `d`, sleep 1, blank (P2), `p`, sleep 1, `q`.
Asserts: stdout contains `[event] StateChanged(Paused)` and `[event] Message("Output device changed — press Play to resume")`; P2 within ±1500 ms of P1; after `p`: `[event] StateChanged(Playing)` (stream rebuilt). 5–10 s window — binary checks.

- [ ] **Step 7: Regression + gates (debug, then one release build)**

Run: `cargo test` — 31/31.
Release build at task end: `cargo build --release` → warnings = 7; `(Get-Item target\release\qpid.exe).Length` ≤ 10,485,760 (record — margin check after every task).

- [ ] **Step 8: Human gates (collect now, record in Task 6)**

- Unplug headphones/headset while playing → status shows the Message, state Paused; replug; Play resumes.
- Sleep + wake while playing → no crash; either resumes cleanly or lands in Paused with the Message; Play works.

- [ ] **Step 9: Commit**

```bash
git add src/engine/mod.rs src/engine/output.rs src/main.rs
git commit -m "Phase 5: device/sleep error handling (cpal flag -> Paused), pause position fold"
```

---

### Task 3: Drag-and-drop through the winit hook

**Files:**
- Modify: `src/winit_hook.rs`, `src/ui.rs` (in `wire`, near the open-folder block :178)

**Interfaces:**
- Consumes: `Command::OpenPath(PathBuf)`; winit `WindowEvent::DroppedFile(PathBuf)` (verified: winit 0.30.13 `event.rs:180`; slint backend forwards to `custom_application_handler.window_event` before slint dispatch; Windows drag-and-drop is ON by default — `WindowsAttributes.drag_and_drop: true`, verified winit 0.30.13 `platform_impl/windows/mod.rs:49`).
- Produces: `winit_hook::set_drop_sink(impl FnMut(PathBuf) + 'static)`.

**Dispatch note:** parallel with Task 2 — one message, two subagents.

- [ ] **Step 1: Second sink in `winit_hook.rs`**

After the existing `SINK` thread_local (:13-15):
```rust
thread_local! {
    static DROP_SINK: RefCell<Option<Box<dyn FnMut(std::path::PathBuf)>>> = RefCell::new(None);
}
```
After `set_sink` (:20-22):
```rust
/// Registers the dropped-file sink. Main/event-loop thread only (same
/// constraints as set_sink). Called once from `ui::wire`.
pub fn set_drop_sink(sink: impl FnMut(std::path::PathBuf) + 'static) {
    DROP_SINK.with(|s| *s.borrow_mut() = Some(Box::new(sink)));
}
```
In `window_event` (:75-94), after the `Occluded` handling, before `evaluate`:
```rust
        if let WindowEvent::DroppedFile(path) = event {
            DROP_SINK.with(|s| {
                if let Some(f) = s.borrow_mut().as_mut() {
                    f(path.clone());
                }
            });
        }
```
Still return `EventResult::Propagate` — never swallow anything from Slint (spec 4.2).

- [ ] **Step 2: Register the sink in `ui.rs::wire`**

After the `on_open_folder` block (:178-188):
```rust
    // Phase 5 item 2: dropping a file or folder on the window goes through
    // the same Command::OpenPath as the dialogs (gate 4 intact: the event
    // type is only named inside winit_hook).
    {
        let cmd_tx = cmd_tx.clone();
        crate::winit_hook::set_drop_sink(move |path| {
            let _ = cmd_tx.send(Command::OpenPath(path));
        });
    }
```

- [ ] **Step 3: Verify**

Run: `cargo test` — 31/31.
Run: `grep -rn "winit_030" src --include=*.rs` → hits only in `src/winit_hook.rs` (gate 4).
Release build at task end: warnings = 7; exe size ≤ 10,485,760 (record).

- [ ] **Step 4: Human gate (collect now, record in Task 6)**

Drag an mp3 onto the window while it plays a different track → the dropped file plays; drag a folder → folder playlist opens. Drop while a file is playing → old file position saved (`[store] save (open of another path)` in debug stderr).

- [ ] **Step 5: Commit**

```bash
git add src/winit_hook.rs src/ui.rs
git commit -m "Phase 5: drag-and-drop open via winit DroppedFile sink"
```

---

### Task 4: Single instance (named mutex + named pipe)

**Files:**
- Modify: `src/winapi.rs` (add 4 fns), `src/main.rs` (`run_ui` :152)
- Possibly: `Cargo.toml` (only features the compiler errors for — likely `Win32_Security`, `Win32_Storage_FileSystem`, `Win32_System_IO` for the pipe signature types; zero expected size cost, verify with the size gate)

**Interfaces:**
- Consumes: `winapi.rs` shell from Task 1; `Command::OpenPath`; `[store] save ({reason})` debug line (verified `mod.rs:414`) as the probe signal.
- Produces: `winapi::claim_instance() -> bool`, `winapi::send_to_first_instance(Option<&Path>)`, `winapi::start_instance_listener(Fn(Vec<u8>) + Send + 'static)`, `winapi::raise_window()`.
- Threading: the listener is the sanctioned 4th thread (spec §5); **projected counts after this task: playing 11→12 (AT CAP), idle 10→11.**

**Split rule (speed note):** if this task runs long, split into two dispatches — 4a writer side (`claim_instance` + `send_to_first_instance` + `run_ui` claim), 4b reader side (`start_instance_listener` + decode + `raise_window`) — reviewed separately.

- [ ] **Step 1: `claim_instance` in `src/winapi.rs`**

```rust
/// Section 12.5 rule 4: one instance per session. True = we are the first
/// (caller proceeds); False = another instance owns the mutex (caller
/// hands over its path and exits).
pub fn claim_instance() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::core::PCWSTR;
        unsafe {
            let name: Vec<u16> = "Local\\qpid-instance"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let Ok(h) = CreateMutexW(None, false, PCWSTR(name.as_ptr())) else {
                return true; // cannot create a mutex -> run anyway, never block the user
            };
            let first = GetLastError() != ERROR_ALREADY_EXISTS;
            std::mem::forget(h); // keep the mutex handle alive for the process
            first
        }
    }
    #[cfg(not(windows))]
    {
        true
    }
}
```

- [ ] **Step 2: `send_to_first_instance` (writer side)**

```rust
/// Second instance: hand our path argument to the first over the named
/// pipe, then the caller exits. Empty payload = "just raise the window".
/// Best-effort: if the pipe never appears we still exit (the user asked
/// for one window, not two).
pub fn send_to_first_instance(path: Option<&std::path::Path>) {
    #[cfg(windows)]
    {
        use std::io::Write;
        let payload: Vec<u8> = match path {
            Some(p) => p
                .to_string_lossy()
                .encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect(),
            None => Vec::new(),
        };
        for _ in 0..10 {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .write(true)
                .open(r"\\.\pipe\qpid-open")
            {
                let _ = f.write_all(&payload);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    #[cfg(not(windows))]
    let _ = path;
}
```
(Client side is pure std — no new windows features. 10 × 50 ms covers the first instance's listener-startup race.)

- [ ] **Step 3: `start_instance_listener` (reader side, the 4th thread)**

```rust
/// Section 12.5 rule 4 / section 5: the first instance's reader. One
/// sanctioned extra thread (name it for thread-count diagnostics),
/// blocked on ConnectNamedPipe — costs nothing while idle. The payload
/// bytes (UTF-16 path, or empty = raise only) go to `on_path`.
pub fn start_instance_listener(on_path: impl Fn(Vec<u8>) + Send + 'static) {
    #[cfg(windows)]
    {
        use std::io::Read;
        use std::os::windows::io::FromRawHandle;
        use windows::core::PCWSTR;
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
            PIPE_ACCESS_INBOUND, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
        };
        let name: Vec<u16> = r"\\.\pipe\qpid-open"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = std::thread::Builder::new()
            .name("qpid-pipe".into())
            .spawn(move || unsafe {
                let h = CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1,
                    0,
                    0,
                    0,
                    None,
                );
                if h.is_invalid() {
                    if cfg!(debug_assertions) {
                        eprintln!("[pipe] CreateNamedPipeW failed");
                    }
                    return;
                }
                loop {
                    let _ = ConnectNamedPipe(h, None); // ERROR_PIPE_CONNECTED ok
                    // Borrow the raw handle as a std File without owning it,
                    // so the loop can keep the pipe open for the next client.
                    let file = std::mem::ManuallyDrop::new(std::fs::File::from_raw_handle(
                        h.0 as *mut core::ffi::c_void,
                    ));
                    let mut buf = Vec::new();
                    let _ = (&*file).read_to_end(&mut buf);
                    on_path(buf);
                    let _ = DisconnectNamedPipe(h);
                }
            });
        let _ = handle; // detached: it lives for the process
    }
    #[cfg(not(windows))]
    let _ = on_path;
}
```
If the compiler errors on `Security::SECURITY_ATTRIBUTES` / `Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES` / `IO::OVERLAPPED` types, add exactly the features it names (`Win32_Security`, `Win32_Storage_FileSystem`, `Win32_System_IO`) — metadata-only, expect ~0 bytes after LTO; the size gate in Step 6 proves it.

- [ ] **Step 4: `raise_window`**

```rust
/// Section 12.5 rule 4: bring this process's top-level window forward.
/// Best-effort: SetForegroundWindow can be refused by the OS foreground
/// lock; ShowWindow(SW_RESTORE) still un-minimizes.
pub fn raise_window() {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows::Win32::System::Threading::GetCurrentProcessId;
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
            SetForegroundWindow, ShowWindow, SW_RESTORE,
        };
        struct Seek {
            pid: u32,
            found: Option<HWND>,
        }
        unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let seek = &mut *(lparam.0 as *mut Seek);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == seek.pid && IsWindowVisible(hwnd).as_bool() {
                seek.found = Some(hwnd);
                return BOOL(0); // stop enumerating
            }
            BOOL(1)
        }
        let mut seek = Seek {
            pid: unsafe { GetCurrentProcessId() },
            found: None,
        };
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut seek as *mut Seek as isize));
            if let Some(hwnd) = seek.found {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}
```

- [ ] **Step 5: Wire `run_ui` (claim BEFORE window creation)**

`src/main.rs`:
```rust
fn run_ui(initial_path: Option<String>) {
    // Section 12.5 rule 4: a second launch hands its path to the first
    // instance and exits. The --cli path is exempt (dev tool, rule ruling).
    if !winapi::claim_instance() {
        winapi::send_to_first_instance(initial_path.as_deref().map(PathBuf::from).as_deref());
        return;
    }
    winit_hook::install();
    let (cmd_tx, evt_rx, shared, engine) = spawn_engine();

    // The first instance's reader: decode UTF-16 payload -> OpenPath,
    // empty payload = raise only. Always raise the window (rule 4).
    {
        let cmd_tx = cmd_tx.clone();
        winapi::start_instance_listener(move |buf| {
            if buf.len() >= 2 && buf.len() % 2 == 0 {
                let units: Vec<u16> = buf
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                if let Ok(s) = String::from_utf16(&units) {
                    if !s.is_empty() {
                        let _ = cmd_tx.send(Command::OpenPath(PathBuf::from(s)));
                    }
                }
            }
            winapi::raise_window();
        });
    }

    let window = MainWindow::new().expect("failed to create UI window");
    // ... unchanged rest
```

- [ ] **Step 6: Verify — two-instance probe (debug build)**

Script `$env:TEMP\opencode\p5-twoinst.ps1`:
```powershell
$exe = "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\target\debug\qpid.exe"
$a = "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\test_audio\tone48k_60m.mp3"
$b = "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\test_audio\test.mp3"
$dir = "$env:TEMP\opencode\qpid-p5-2inst"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$env:LOCALAPPDATA = $dir
Remove-Item "$dir\qpid\state.json" -ErrorAction SilentlyContinue
$err1 = "$dir\inst1-stderr.txt"

$psi = New-Object Diagnostics.ProcessStartInfo
$psi.FileName = 'cmd.exe'
$psi.Arguments = "/c `"`"$exe`" `"`"$a`" > `"$dir\inst1-out.txt`" 2>`"$err1`"`""
$psi.UseShellExecute = $false
$inst1 = [Diagnostics.Process]::Start($psi)
Start-Sleep 4   # window up, playing A

$psi2 = New-Object Diagnostics.ProcessStartInfo
$psi2.FileName = 'cmd.exe'
$psi2.Arguments = "/c `"`"$exe`" `"`"$b`" > `"$dir\inst2-out.txt`" 2>&1`""
$psi2.UseShellExecute = $false
$sw = [Diagnostics.Stopwatch]::StartNew()
$inst2 = [Diagnostics.Process]::Start($psi2]
$null = $inst2.WaitForExit(5000)
$sw.Stop()

"inst2 exit in $($sw.ElapsedMilliseconds) ms, still running: $(-not $inst2.HasExited)"
Get-Content $err1
Get-Process qpid | Where-Object { $_.Id -ne $inst1.Id } | Select-Object Id, Path
```
Asserts: inst2 **exits ≤ ~1000 ms** (well inside 5 s) even if its own probe window is up; inst1 stderr gains `[store] save (open of another path)` (OpenPath trigger 4 — proves the payload arrived and was routed); **exactly one qpid process remains**; inst1 window raised/restored (visible on screen). Clean up: `Stop-Process -Id $inst1.Id`.

- [ ] **Step 7: Thread + size gates (one release build at task end)**

Run: `cargo test` — 31/31.
Release build: warnings = 7; **`(Get-Item target\release\qpid.exe).Length` ≤ 10,485,760 — record (this is the task most likely to eat the margin).**
Thread counts (short 60 s runs — counts are noise-immune, may run during the soak; read only the `threads` column):
```powershell
python tools/bench.py --exe target/release/qpid.exe --scenario playing-1x-visible --file test_audio/tone48k_60m.mp3 --duration 60 --outdir bench_results\p5-t4-threads
python tools/bench.py --exe target/release/qpid.exe --scenario idle-visible --duration 60 --outdir bench_results\p5-t4-threads
```
Asserts: playing threads ≤ 12, idle ≤ 12. **BENCH.md wording when playing = 12: "AT CAP (12/12, zero headroom) — any future thread-adding change is a budget failure" — never a bare PASS.** If > 12: stop and report with the per-thread breakdown (`Get-Process qpid | Select -Expand Threads`) before proceeding.

- [ ] **Step 8: Commit**

```bash
git add src/winapi.rs src/main.rs Cargo.toml
git commit -m "Phase 5: single instance with named mutex + pipe handoff, window raise"
```
(If no Cargo.toml change was needed, drop it from the add.)

---

### Task 5: Media keys via RegisterHotKey (optional — size-gated)

**Files:**
- Modify: `src/winapi.rs`, `src/main.rs` (`run_ui`), possibly `Cargo.toml` (add `Win32_UI_Input_KeyboardAndMouse`)

**Interfaces:**
- Consumes: `Command::{TogglePlay, Next, Prev}`; Task 4's `run_ui` shape.
- Produces: `winapi::start_media_keys(Sender<Command>)` (no-op on failure — the feature is optional, spec §15.5).

- [ ] **Step 1: Size gate FIRST — skip the whole task if margin is gone**

```powershell
$size = (Get-Item target\release\qpid.exe).Length
$margin = 10485760 - $size
"size=$size margin=$margin"
if ($margin -lt 16384) { "SKIP TASK 5" } else { "PROCEED" }
```
If `SKIP TASK 5`: mark the task cancelled in progress.md with the number, do not touch code (spec-sanctioned: Phase 5 item 5 is optional; §12.5 rule 5's own fallback ordering lets RegisterHotKey land, but never at the cost of budget 1). Otherwise proceed.

- [ ] **Step 2: Cargo feature**

Add to the windows features list in `Cargo.toml`: `"Win32_UI_Input_KeyboardAndMouse",`.

- [ ] **Step 3: `start_media_keys` in `src/winapi.rs`**

```rust
use std::sync::mpsc::Sender;
use std::sync::Mutex;

// Wndproc context: hotkey messages arrive on the UI thread's pump, so the
// lock is uncontended in practice. Const-initializable (Rust >= 1.63).
static MEDIA_TX: Mutex<Option<Sender<crate::engine::Command>>> = Mutex::new(None);

/// Section 12.5 rule 5 / Phase 5 item 5 (optional): media transport keys
/// via RegisterHotKey on a message-only window with our own wndproc
/// (winit has no WM_HOTKEY path; spec's fallback when SMTC is skipped).
/// Failure at any point = feature quietly absent.
pub fn start_media_keys(tx: Sender<crate::engine::Command>) {
    #[cfg(not(windows))]
    let _ = tx;
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            RegisterHotKey, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, RegisterClassExW, WM_HOTKEY,
            WNDCLASSEXW, WNDPROC, WS_POPUP, HWND_MESSAGE,
        };

        *MEDIA_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);

        unsafe extern "system" fn wndproc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            if msg == WM_HOTKEY {
                let cmd = match wparam.0 {
                    1 => Some(crate::engine::Command::TogglePlay),
                    2 => Some(crate::engine::Command::Next),
                    3 => Some(crate::engine::Command::Prev),
                    _ => None,
                };
                if let (Some(cmd), Ok(guard)) = (cmd, MEDIA_TX.lock()) {
                    if let Some(tx) = guard.as_ref() {
                        let _ = tx.send(cmd);
                    }
                }
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        let class: Vec<u16> = "qpid-media-keys".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: WNDPROC(Some(wndproc)),
                // Message-only window, never rendered: a null instance is
                // fine (no resources to load). Contingency if
                // RegisterClassExW fails with an instance-related error:
                // add Win32_System_LibraryLoader + GetModuleHandleW and
                // convert HMODULE.0 -> HINSTANCE.0.
                hInstance: HINSTANCE::default(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                if cfg!(debug_assertions) {
                    eprintln!("[media] RegisterClassExW failed");
                }
                return;
            }
            let hwnd = match CreateWindowExW(
                Default::default(),
                PCWSTR(class.as_ptr()),
                PCWSTR(core::ptr::null()),
                WS_POPUP,
                0, 0, 0, 0,
                HWND_MESSAGE,
                Default::default(),
                HINSTANCE::default(),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            };
            // Ids 1..3 are what wndproc matches; hotkeys auto-release at
            // process exit, no cleanup needed.
            let _ = RegisterHotKey(hwnd, 1, Default::default(), VK_MEDIA_PLAY_PAUSE.0 as u32);
            let _ = RegisterHotKey(hwnd, 2, Default::default(), VK_MEDIA_NEXT_TRACK.0 as u32);
            let _ = RegisterHotKey(hwnd, 3, Default::default(), VK_MEDIA_PREV_TRACK.0 as u32);
        }
    }
}
```
If `WNDPROC(Some(wndproc))` doesn't typecheck (wndproc is already `Option`), use `Some(wndproc)` directly; if `Default::default()` for `HOT_KEY_MODIFIERS` doesn't exist, use `HOT_KEY_MODIFIERS(0)`; if `HWND_MESSAGE` import path differs, compiler will name it. Exactly one of these may need adjusting — no design change.

- [ ] **Step 4: Call site**

`src/main.rs` `run_ui`, after `spawn_engine()`:
```rust
    winapi::start_media_keys(cmd_tx.clone());
```

- [ ] **Step 5: Verify (debug + gates)**

Run: `cargo test` — 31/31.
Release build: warnings = 7; **size ≤ 10,485,760 — if this task pushed it over, revert the Task 5 commit and record "Task 5 dropped: exe budget" (spec-sanctioned).**
Human gate: with qpid open (even minimized/occluded), hardware/media play-pause key toggles playback; next/prev skip. If keys don't fire and stderr shows `[media] RegisterClassExW failed`, apply the LibraryLoader contingency from Step 3 and retry once; if still failing, drop the task (optional).

- [ ] **Step 6: Commit (or revert)**

```bash
git add src/winapi.rs src/main.rs Cargo.toml
git commit -m "Phase 5: media keys via RegisterHotKey on message-only window"
```

---

### Task 6: Soak result, exit gates, documentation, whole-branch review

**Files:**
- Modify: `BENCH.md`, `IMPLEMENTATION.md`, `USER_GUIDE.md`, `ui/main.slint` (:409 help text)

**Interfaces:**
- Consumes: everything from Tasks 1–5; the running soak process (Task 1 Step 8 pid); human-gate results collected in Tasks 2–5 Step 8/4.

- [ ] **Step 1: Soak result (>= 30 minutes after Task 1 Step 8)**

```powershell
$soak = "$env:TEMP\opencode\qpid-p5-soak"
Get-Content "$soak\wake.log" -Tail 3
$eco = Select-String -Path "$soak\stderr.txt" -Pattern "\[eco\] EcoQoS on" -Quiet
$min = Select-String -Path "$soak\stderr.txt" -Pattern "visible=false" -Quiet
Get-Process qpid | Where-Object { $_.Path -like "*qpid-p5-soak*" } | Stop-Process -Force
```
Asserts: ≥30 min uptime in the last line; **`underruns=0` on every line**; `[eco] on` present after minimize. Record in BENCH.md: "EcoQoS soak: 30 min minimized playing, debug build, trim armed, underruns=0".
If underruns > 0: contingency per §11.7 rule 7 — EcoQoS stays off (visibility sink keeps trim), `RING_SECONDS` 3→5 in `output.rs`, re-run budget/soak pieces; record as a deviation with numbers.

- [ ] **Step 2: §15.6 exit criteria — assemble the evidence table**

| Gate | Evidence |
|---|---|
| Unplug headphones while playing → pauses cleanly | Human result (Task 2 Step 8) |
| Sleep and wake works | Human result (Task 2 Step 8) |
| Second launch with a file path opens it in the first instance | Task 4 Step 6 probe output |
| Exe ≤ 10,485,760 | `(Get-Item target\release\qpid.exe).Length` after final release build — record, incl. remaining margin |
| Threads ≤ 12 | Task 4 Step 7 numbers — worded **AT CAP** if 12 |
| Budget 4 ≤ 15 MB without trim | Task 1 Step 7 number |
| Tests 31/31, release warnings = 7 | Final `cargo test` + release build |
| Rule 10 audit | `grep -n -B1 "eprintln!" src/*.rs src/**/*.rs` — every hit's previous line contains `debug_assertions` (CLI stdout prints exempt) |
| Gate 4 | `grep -rn "winit_030" src` → only `winit_hook.rs` |
| Human drag-drop + media keys | Task 3 Step 4 / Task 5 Step 5 results |

Any missing human result: stop and ask the user before claiming exit.

- [ ] **Step 3: Documentation**

`BENCH.md`: budget 4 row (no-trim number + trim-on info), thread rows with the AT CAP note, soak line, note that CPU% budgets were not re-run unless a bench ran (Phase 5 adds no steady-state CPU; EcoQoS only engages when hidden — if budget 6/7 need a spot-check, one 60 s `playing-2x-minimized` run after the soak is dead, do it).
`IMPLEMENTATION.md`: Phase 5 section — EcoQoS/trim wiring + why no timer (§18), device-error path + fold fix rationale, drop sink, single instance design (mutex/pipe/4th thread), media keys or the size-based drop with numbers, probe results.
`USER_GUIDE.md` + `ui/main.slint:409` help text: add lines for drag-and-drop ("Drop a file or folder anywhere on the window to open it"), single instance ("Opening a second copy hands the file to the running window"), media keys ("Media keys: play/pause, next, previous" — only if Task 5 landed).

- [ ] **Step 4: Whole-branch review (one pass, delta-focused)**

Dispatch the requesting-code-review flow over the full Phase 5 diff (`git diff 52b4280..HEAD`) with the finding list from all per-task reviews attached so it verifies rather than re-derives. Fix what it finds, re-review only the fix hunks (speed rule 6).

- [ ] **Step 5: Final commit**

```bash
git add BENCH.md IMPLEMENTATION.md USER_GUIDE.md ui/main.slint
git commit -m "Phase 5: soak result, exit gates, docs, human checklist"
```

---

## Self-Review notes (skill step, done in-session)

- Spec coverage: §15.5 items 1–6 → Tasks 1,2,3,4,5,6 respectively; §11.7/§11.8 → Task 1; §12.3 → Task 2; §12.5 rules 4/5 → Tasks 4/5; budget 4 no-trim → Task 1 Step 7; thread budget → Task 4 Step 7 + Task 6 table; rule 10 / gate 4 → Task 6 Step 2.
- Type consistency checked against sources: `PROCESS_POWER_THROTTLING_STATE{u32×3}` + `Default`, `SetProcessInformation(*const c_void, u32)`, `SetProcessWorkingSetSize` in Threading, `CreateMutexW(Option<*const SECURITY_ATTRIBUTES>, Param<BOOL>, Param<PCWSTR>)`, `CreateNamedPipeW` → `FILE_FLAGS_AND_ATTRIBUTES` + `NAMED_PIPE_MODE`, `ConnectNamedPipe(Option<*mut OVERLAPPED>)`, `RegisterHotKey(hwnd, i32, HOT_KEY_MODIFIERS, u32)`, `VK_MEDIA_*: VIRTUAL_KEY(176/177/179)`, `GetCurrentProcessId -> u32` (Threading), `HWND/HINSTANCE/HMODULE(*mut c_void)` in Foundation, `WNDCLASSEXW: Default`, `WM_HOTKEY = 786`, `WNDPROC = Option<unsafe extern "system" fn(HWND,u32,WPARAM,LPARAM)->LRESULT>`, winit `WindowEvent::DroppedFile(PathBuf)`, winit Windows `drag_and_drop: true` default.
- Probe signals verified in source: `[store] save ({reason})` at `mod.rs:414`, `[vis]`/`[release]` debug lines, `position_ms` blank-line CLI read.
