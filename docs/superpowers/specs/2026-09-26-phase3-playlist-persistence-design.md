# Phase 3: Playlist and Persistence — Design

Date: 2026-09-26
Status: approved (sections 1–5, brainstorming session 2026-09-26)
Spec: `architecture.md` §7 (end of track), §9 (persistence), §12.6–12.7
(scan/sort), §13 (project layout), §15.3 (exit criteria), §16 tests 1/6,
§18 (rules).

## Goal

Real playlist navigation and JSON state persistence. Exit per §15.3:
kill while playing → relaunch → position within 30 s; folder plays
through all files in order.

## Decisions made with the human partner

1. **Approach A** — two plain modules (`src/playlist.rs`,
   `src/store.rs`), engine-owned state in `run()`. Chosen because it
   matches §13's file layout and does not touch the command handling
   verified in Phases 1–2.
2. **Prev semantics** (spec gap): restart current file if
   `position_ms > PREV_RESTART_THRESHOLD_MS` (named constant, 5000 ms),
   else previous file; at file 0 restart file 0.
3. **Next / EOF-advance start position**: always 0 (spec-literal §7.4).
   Saved positions are used only by the §9 restore rules.
4. **State path**: `%LOCALAPPDATA%\qpid\state.json` (partner decision;
   architecture.md §9 and §13's `hush/` root updated to match).
5. **Save-trigger verification is a gate**: report must contain the
   trigger → call-site table below with exact `mod.rs` line numbers; a
   missing row is a bug, not a follow-up.
6. **`advance_to` gets its own coverage**, including the mixed-sample-rate
   case (see Risks).

## Section 1 — Modules

### `src/playlist.rs` (pure)

- `scan(dir) -> Vec<PathBuf>` — extensions
  `mp3 m4a m4b aac flac ogg wav` (case-insensitive), skip hidden/system
  files (§12.6; Windows `file_attributes()` & (HIDDEN|SYSTEM)), natural
  sort by file name with `natord` (§12.7).
- Split: pure `is_playable(name, attributes)` (unit-testable without
  faking fs attributes) + thin `read_dir` wrapper.
- `index_of(files, path) -> Option<usize>` — case-insensitive (store
  keys are lowercase, disk names may not be).

### `src/store.rs`

- `Store` struct, serde derive, schema exactly §9:
  `version, speed (f64), last_folder, folders{touched,
  last_file, files{pos_ms, size, done}}`.
- `RESUME_REWIND_MS: u64 = 0` (§9 rule 8), applied once on restore.
- `path()` → `%LOCALAPPDATA%\qpid\state.json`; `LOCALAPPDATA` unset →
  persistence disabled silently, never crash.
- `load()` → missing / corrupt / wrong version → `Store::default()`
  (rule 4).
- `save()` → write `state.json.tmp` in same dir, `fs::rename` over
  (rule 6); write errors swallowed.
- `position_for(name, size)` → 0 on size mismatch (rule 2);
  `record(...)` stores only `pos_ms > 0 || done` (rule 3);
  `prune()` ≤ 100 folders evicting oldest `touched` (rule 3);
  key normalization: lowercase, strip `\\?\` (rule 1).
- Speed: schema f64 ↔ `speed_milli` u32 conversion helper (same
  milli-table as `Speed`, fall back to 1x on unknown).
- No new dependencies (serde/serde_json/natord already declared).

## Section 2 — Engine integration

`run()` locals added: `playlist: Vec<PathBuf>`, `idx: usize`,
`store: Store` (loaded once at startup; stored speed applied to
`shared.speed_milli`), `next_checkpoint: Instant`,
`last_saved_pos: u64`.

### `advance_to(target)` — one helper, two paths

**Path 1 — Playing (stream live):**

1. `save_now` (trigger: next/prev).
2. §7 flush protocol (gain down 20 ms, `flush_req`, wait `flush_ack`).
3. Instead of `decoder.seek`: `Decoder::open(new_path)`.
   - Success:
     a. `shared.duration_ms.store(dec.duration_ms().unwrap_or(0), ...)`
        — mirrors `open_path` mod.rs:437-439; without this the Phase 4
        slider would show the previous track's duration (§7 rule 7
        tolerates one stale *position* read, not a permanently wrong
        duration).
     b. `ctx.resampler = Resampler::new(dec.sample_rate(),
        ctx.output.device_rate())` — **unconditional rebuild**; the
        resampler was keyed to the previous file's rate at
        `open_path` (mod.rs:449) and must not be carried over. Also
        clears filter history. (`Stretcher` is speed-keyed →
        `reset()` only, keeps speed.)
     c. replace `ctx.decoder`, `ctx.path`; `base_ms = 0`,
        `played_frames = 0`, clear `eof`/`drained`.
   - Failure: `Message` event, try next index; if none playable →
     `PlayState::Error` (§6.9 IO errors).
4. `prefill(PREROLL)`, clear flush flags, gain up, emit `TrackChanged`
   with real `index`/`count`.
5. Stream object untouched → §7.4.3 "stream stays open", gap < 100 ms.

**Path 2 — not Playing (Paused / Ended / no live ctx):**

- Record outgoing file's position in store; set `released_path =
  target`, `base_ms = 0`, `idx = target`, emit `TrackChanged`, state →
  `Paused` (from `Ended`, Next/Prev land in `Paused`; Play reuses the
  existing tested `resume()` reopen path). Reuse the 10-second-release
  drop logic rather than duplicating it.

### Next / Prev (replace placeholders mod.rs:338-344)

- Next: `advance_to(idx+1)` if it exists, else no-op.
- Prev: restart current (existing `flush_playing(target=0)` while
  Playing) if `position_ms > PREV_RESTART_THRESHOLD_MS` (5000);
  else `advance_to(idx-1)` if `idx > 0`; else restart file 0.

### EOF handoff (replaces mod.rs:248-252 `state = Ended`)

- Mark done + `save_now` → next file exists → `advance_to(idx+1)`,
  stay Playing (stream stays open, §7.4.3); else → `Ended` exactly as
  today. Single-file playlist behaves identically to Phase 1/2.

### 30 s checkpoint (§9 rule 5)

- Folded into the existing `recv_timeout`: Playing wait =
  `min(fill-based, time until next_checkpoint)`. On wake, save only if
  `position_ms != last_saved_pos`; reset deadline. No new thread or
  timer (§18).

## Section 3 — Save-trigger gate (§9 rule 5)

Every trigger is one call site of `save_now(reason)`; Phase 3 report
must list these with exact line numbers (grep `save_now` cross-check);
missing row = bug before done:

| Trigger | Call site |
|---|---|
| pause | `pause()` |
| next | `advance_to()` (manual Next + EOF handoff) |
| prev | `advance_to()` (manual Prev, incl. restart-current) |
| open of another path | `open_path()` entry, only when old path ≠ new path (resume-after-release must not rewrite) |
| speed change | `SetSpeed` handler |
| app exit | `Shutdown` + both loop-break paths in `run()` |
| error stop | every `*state = PlayState::Error` site |
| 30 s while playing | checkpoint in `run()` timeout branch |

`save_now` no-ops when no file is current (speed/exit still persist
top-level fields). `done` computed at save time as
`position ≥ duration.saturating_sub(3000)` (§9 rule 4) or EOS.

## Section 4 — Restore flows (§9 restore rules)

1. **Launch, no args** (rule 1; acceptance test 6): new
   `Command::RestoreSession` sent by `main.rs` when no path argument
   (CLI and UI — CLI makes the test automatable). Engine: load store →
   scan `last_folder` (real casing recovered from scan, matched
   case-insensitively to the stored lowercase key) → size check
   (mismatch → pos 0) → **metadata-only duration probe** (`Decoder::open`
   → `duration_ms()` → drop; duration comes from `codec_params.n_frames`
   at open time, decode.rs:120 — no packets decoded, negligible cost vs
   budget 2) → emit `TrackChanged`, `base_ms = pos − RESUME_REWIND_MS`
   (≥ 0), `released_path = Some(file)`, state = **Paused**, no autoplay.
   Play uses existing `resume()`.
2. **Open folder** (rule 2): save old file if any → `scan` → pick
   `last_file` if present & not done, else first not-done, else first →
   open from saved position (size-checked) → autoplay (current
   behavior).
3. **Open single file** (rule 3): playlist = sorted siblings, `idx` =
   that file, saved position, autoplay.

### Deferred (not in this phase)

- Per-packet decode-error skip (§6.9 "skip the packet and continue";
  mod.rs:554 comment). `advance_to` includes the *file-level* failure
  path (open failure → skip to next) because it needs it anyway;
  mid-file packet skipping stays a pre-existing TODO. Comment at
  mod.rs:554 must be left accurate or reworded to say so.

## Section 5 — Testing and gates

- **Unit**: playlist natural sort `1.mp3…30.mp3` (acceptance 1),
  extension filter, `is_playable` attribute rules; store roundtrip,
  corrupt→default, size mismatch, pruning, key normalization,
  pos>0/done rule, atomic rename; speed f64↔milli.
- **Existing**: all 8 DSP tests stay green.
- **Mixed-rate advance probe** (resampler-rebuild gate): folder with
  `test.mp3` (44.1 kHz) + `tone48k.mp3` (48 kHz); advance both
  directions (engaged→bypassed and bypassed→engaged), verify position
  rate against wall clock and clean EOF each leg. This is where the
  Phase 1 pitch bug would reappear if the rebuild line were missing.
- **Acceptance 6 probe**: play 65 s → `taskkill /F` → relaunch
  `--cli` with no args → assert same file, position within 30 s,
  state Paused, no autoplay.
- **Trigger table**: report artifact; missing row = bug.
- **Budgets**: re-run budget 1 (exe size — see risk) and spot-check
  budget 6; budgets 2–5, 7–8 unaffected in design (no new threads,
  checkpoint folded into existing wake).
- **Docs**: IMPLEMENTATION.md Phase 3, BENCH.md if anything is
  remeasured, architecture.md §9/§13 `hush`→`qpid` rename.
- **Commit** after gates pass.

## Risks

1. **Exe size (step-1 sequencing, approved)**: serde/serde_json are
   declared but currently unused → likely not linked. Headroom is
   ~103 KB (10,380,288 vs 10,485,760 cap). First implementation step:
   wire the smallest possible `Store`, build, measure. Over budget →
   stop and report numbers with options (§18 rule 1), do not ship over.
2. **Mixed-rate track advance** (covered by probe above).
3. **Checkpoint vs kill timing**: acceptance 6 probe uses 65 s so at
   least two checkpoints (30/60 s) have fired; rule 7's "≤ 30 s loss"
   is the bound being tested.

## Exit criteria (§15.3)

- Kill while playing, relaunch: position within 30 s (probe).
- Folder plays through all files in order (probe + unit sort test).
- Acceptance test 6: same file, same position, paused, no autoplay.
