# Phase 4: UI — design spec

Date: 2026-10-03
Status: approved design (Approach A, confirmed by human partner 2026-10-03)
Authority: binding over the implementation plan. Architecture (`architecture.md`)
remains binding over this spec. Where both name a rule, the spec cites it.

## 1. Context

Phases 0–3 are complete: the skeleton window, the headless engine, speed
control, playlist and persistence are built, committed, and gate-verified
(latest: `e818485`, 16/16 tests, exe 10,443,264 B of 10,485,760 allowed).
The UI layer is a Phase 0 stub: buttons send commands, two engine events
render, and there is no timer, no real slider behavior, no keyboard, and no
minimize/occlusion awareness.

Phase 4 wires the real UI per section 15 Phase 4 exit: *every control works.
Timer stops while minimized and while paused. Budgets 3 and 4 pass.*

## 2. Decisions (settled before this spec)

1. **Approach A — Slint-native plus one minimal hook.** Everything Slint can
   do with stable APIs (timer, slider, keyboard, property writes) is done with
   stable APIs. The `unstable-winit-030` feature is enabled for one capability
   Slint does not expose: minimize/occlusion detection (section 10 rule 3
   prefers the hook; the 1 s polling fallback is sanctioned only when the hook
   is unavailable — it was verified available, so no fallback is built).
2. **Exact slint pin in `Cargo.toml`** — `version = "=1.18.1"`, not a semver
   range. `cargo update` must not be able to silently move the
   `unstable-winit-030` surface. Verified working against slint 1.18.1 /
   winit 0.30.13 sources in the local registry:
   - `slint::winit_030::{CustomApplicationHandler, EventLoopBuilder, EventResult}`
     exists behind the feature (slint `lib.rs:663-726`);
   - `CustomApplicationHandler::window_event()` intercepts winit
     `WindowEvent`s *before* Slint, with both the winit and Slint windows
     passed in, and an `EventResult` to propagate or swallow
     (`i-slint-backend-winit lib.rs:210-234`);
   - registration path: `BackendSelector::with_winit_event_loop_builder`
     (`i-slint-backend-selector api.rs:192`) taking
     `EventLoopBuilder::with_custom_application_handler` (backend
     `lib.rs:360-367`);
   - `Window::is_minimized()` exists publicly as a cross-check
     (`i-slint-core api.rs:593`).
3. **Theme switching deferred to Phase 5.** Section 15 Phase 4 exit criteria
   do not mention it; the `main.slint` comment claiming "Phase 4/5" is
   corrected to Phase 5.
4. **Drag-and-drop stays Phase 5** (section 15) even though the hook lands
   now. The hook only observes visibility in Phase 4.
5. **The Ended-stream release is in scope**, reclassified from the Phase 4
   "deferred" list: it is a Phase 1 budget violation (budgets 4/7/8 territory,
   section 8: resources release after 10 s paused) that Phase 3 surfaced —
   last file → `Ended` currently keeps the stream open indefinitely. It does
   not block Phase 4's start; it lands in the first measurement pass.

## 3. Scope

### In

- 500 ms position timer with stop/restart on pause, minimize, occlusion
  (section 10 rule 1).
- Seek slider: drag-vs-commit (rule 2) and duration-unknown disable
  (section 7 rule 7).
- Keyboard shortcuts (section 10 rule 6).
- Speed control: already live since Phase 2 — verification only, plus the
  `[` / `]` step bindings.
- Minimize/occlusion detection via `CustomApplicationHandler` (rule 3 hook
  path).
- Command-line path argument (rule 5): already wired in `main.rs:145`;
  acceptance-verified as Explorer "Open with".
- Ended-stream release (decision 2.5).
- Exact slint pin (decision 2.2).
- Measurement passes: budgets 3, 4, 9, timer-stop verification, budget 1
  re-check.

### Out

- Drag-and-drop (Phase 5). EcoQoS, working-set trim (Phase 5, section 11
  rules 7–8). Light/dark theme (decision 2.3). Single instance, media keys,
  device change (Phase 5). Tray icon (never — rule 10.7). Any new dependency,
  thread, or timer beyond the single `slint::Timer` and the existing wake
  path.

## 4. Components

### 4.1 `Cargo.toml`

`slint = { version = "=1.18.1", default-features = false, features = ["std", "compat-1-2", "backend-winit", "renderer-software", "unstable-winit-030"] }`.
No other dependency changes.

### 4.2 `src/winit_hook.rs` — the only file allowed to name the unstable API

- Implements `CustomApplicationHandler` and is registered through the
  backend selector before the event loop runs (section 4.1 of this spec's
  decision 2 shows the exact API chain).
- **Always returns `EventResult::Propagate`** — observation only; Slint's
  own handling is never suppressed.
- Computes a boolean `visible` per window event:
  `visible = !winit_window.is_minimized() && last_occluded == false && size != (0,0)`
  where `last_occluded` tracks the most recent
  `WindowEvent::Occluded(bool)`. (Windows may not emit `Occluded` on
  minimize — the slint adapter works around exactly this by also checking
  zero size — so all three signals are honored; which actually fire is an
  empirical question for the plan's probe, not an assumption.)
- On each `visible` **transition**, invokes a registered visibility sink
  (below). Evaluation piggybacks on event arrival — it is not a polling
  loop (section 11 rule 1).
- **Visibility sink:** a registration function called once from `ui.rs`
  before `window.run()`, delivering `fn(bool /* visible */)` on the
  event-loop thread (both hook and `run()` live on the main thread — no
  channel, no new thread; `slint::Timer` is not `Send`, so the callback runs
  where the timer lives). Unregistered sink → hook no-ops.
- Drop-file events are ignored in Phase 4 (decision 2.4).

### 4.3 `src/ui.rs` — the 500 ms timer

- One `slint::Timer`, repeating 500 ms (section 10 rule 1).
- **Runs only when `is_playing == true && window visible`.** Each condition
  change re-evaluates: stop the timer when either fails, start when both
  hold. Starting an already-running timer is a no-op; same for stop.
- Per tick: read `shared` once, compute, then write `elapsed-text`,
  `duration-text`, `seek-fraction` **only if the displayed value changed**
  (section 11 rule 5). Position from `Shared::position_ms()` (already
  speed-correct — Phase 2); duration from `Shared::duration_ms`, 0 =
  unknown.
- Time format: `H:MM:SS`, or `M:SS` under one hour (section 10 rule 4).
- **Drag guard:** while the user is dragging the slider the timer must not
  write `seek-fraction` (rule 2: ignore engine position during drag).
- `SeekAbsolute` commit: fraction × `duration_ms`, rounded, sent only on
  slider release — replacing today's hardcoded `SeekAbsolute(0)`
  (`ui.rs:88`). Clamp happens engine-side (section 7 rule 3).
- **Duration unknown (`duration_ms == 0`):** slider disabled (`enabled:
  false`), duration text blank, elapsed still shown (section 7 rule 7).
- The engine-event drain thread keeps its current role (TrackChanged /
  StateChanged / Message). `StateChanged` continues to drive `is_playing`,
  which the timer conditions read.

### 4.4 `ui/main.slint` — keyboard and slider behavior

Keyboard handled with Slint's own key events (stable API, a `FocusArea` or
window-level key handling; no text inputs exist, so focus is simple):

| Key | Action |
|---|---|
| Space | toggle play |
| Left / Right | seek −15 s / +15 s |
| N / P | next / previous track |
| `[` / `]` | step speed down / up |
| Ctrl+O | open file dialog |
| Ctrl+Shift+O | open folder dialog |

- Step speed maps to a new `step-speed(int delta)` callback: `ui.rs` clamps
  `speed_index + delta` to 0..=4 and sends `SetSpeed(Speed::from_index(_))`,
  updating `speed_index` (same path the segment buttons already use).
- Modifier chords (Ctrl+O / Ctrl+Shift+O) must not fire on plain O.
- Slider: if the std `Slider` cannot expose drag state (pressed/changed vs
  released), replace it with a minimal `TouchArea`-based slider inside this
  file. **The behavior — no engine-position writes during drag, seek commit
  on release, disabled at duration 0 — is the requirement; the widget is the
  plan's choice.**
- Correct the stale comment (`main.slint:6`) to defer theme to Phase 5.

### 4.5 `src/main.rs`

- No construction for the path argument — `run_ui(args.first())` already
  sends `OpenPath` (`main.rs:145-146`). Acceptance-verified only.
- Backend selector gains the event-loop builder carrying the hook
  (section 4.2), before `window.run()`.

### 4.6 `src/engine/mod.rs` — Ended-stream release

- Entering `Ended` starts the same 10 s release path pause uses (section 8;
  budget 7). After 10 s in `Ended` with no intervening command, the engine
  reaches the identical idle state pause reaches: stream closed, decoder
  closed, no timers firing beyond the existing blocking wake.
- Implementation reuses the existing paused-release mechanism and its
  timestamp — no new timer, no new thread. The exact integration point (how
  `Ended` participates in `paused_since`-style bookkeeping) is the plan's
  call, read from the current code.
- Play after `Ended` still restarts the file from position 0 (Phase 3
  behavior, section 7) — the release must not break it; the plan verifies.
- No change to the 8-trigger save table; position is already saved when the
  handoff marks the last file done (Phase 3 `advance_to`).

## 5. Measurement plan (own pass, before Phase 4 is called done)

Method honesty is a gate: every number below is measured, never assumed
(section 2, final paragraph).

| Check | Method | Gate |
|---|---|---|
| Budget 3: visible, playing 1x | `bench.py` visible-playing scenario | ≤ 25 MB — **pass required** |
| Budget 4: minimized, playing 1x | `bench.py` minimized-playing scenario | ≤ 15 MB — **pass required** |
| Budget 9: wakeups/s, background playback, excluding audio callback | method chosen by the plan; bench.py does not count wakeups today. Plan must state the method and its limitations (self-instrumented counters vs OS-level) and record it in `BENCH.md`. | ≤ 2/s — **recorded; failure reported with numbers** |
| Timer stops minimized | probe: while minimized and playing, no timer-driven property writes / timer wakeups over a ≥ 30 s window | must hold |
| Timer stops paused | same probe while paused | must hold |
| Budget 1: exe size | release build measurement | ≤ 10,485,760 B — **pass required** |
| Budget 7: paused >10 s and **ended >10 s** | existing paused probe + new ended probe | 0 % CPU, no stream, no decoder — **pass required** |
| Existing tests | `cargo test` | 16/16 + new tests green, warnings ≤ baseline |

Any budget failure: report the number, fix before further features.

## 6. Testing

- **TDD, unit level:** time formatting (`H:MM:SS` / `M:SS` boundaries),
  step-speed clamp (0 and 4 edges), fraction→ms conversion (rounding, 0
  duration), drag-guard logic if extracted to a pure function.
- **Engine:** the Ended release gets an automated check if the existing
  engine test harness can host it; otherwise a probe with recorded output.
- **Probes (scripts in `$env:TEMP\opencode\`, never the repo):** timer-stop
  probes and the ended-release probe above; keyboard input via `SendInput`
  in a probe script where practical.
- **Control checklist (necessarily partly manual — human partner):** every
  §10 table row + slider drag/commit + disabled slider on a duration-0 file +
  Ctrl chords not firing as plain letters. Listed explicitly in the final
  report as human-only checks.

## 7. Gates for "Phase 4 done"

1. Section 15 Phase 4 exit: every control works; timer stops minimized and
   paused; budgets 3 and 4 pass.
2. Budget 9 measured and recorded per section 5.
3. Budget 1 within cap; `cargo test` green; no new threads/timers/deps
   (one `slint::Timer` is the single added timer, sanctioned by rule 10.1).
4. The unstable API is imported by `src/winit_hook.rs` and nowhere else
   (enforceable by grep; recorded in the final report).
5. `Cargo.toml` pins slint to `=1.18.1`.
6. Ended-stream release landed and probe-verified.
7. Human-only control checklist signed off.

## 8. Risks and open points for the plan

- **Std `Slider` drag-state exposure** — the one known discovery point; a
  minimal custom slider is pre-approved (section 4.4).
- **Which winit events actually fire on Windows minimize/restore** — probe
  first, then finalize the `visible` formula (section 4.2 already defines the
  belt-and-braces form; probe confirms or simplifies it).
- **Wakeup counting method** — the plan must pick and justify it before
  claiming budget 9 (section 5).
- **Unstable-feature evolution** — blast radius is `winit_hook.rs` only, the
  version is pinned exactly; a future slint upgrade must re-verify this one
  file, nothing else.
