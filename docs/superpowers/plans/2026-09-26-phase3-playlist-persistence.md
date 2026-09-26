# Phase 3: Playlist and Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Real playlist navigation (scan/sort/next/prev/end-of-track handoff) and JSON state persistence with atomic saves and section 9 restore rules.

**Architecture:** Approach A (approved): two new modules — `src/playlist.rs` (scan/filter/natural sort) and `src/store.rs` (schema/atomic save/prune) — plus engine-owned state (`playlist`, `idx`, `store`, checkpoint deadline) in `engine::run()`. A two-path `advance_to()` reuses the section 7 flush protocol while Playing and the released-path reopen otherwise. `serde`/`serde_json` get linked for the first time, so exe size is measured before anything else builds on it.

**Tech Stack:** Rust, serde/serde_json (already declared), natord (already declared), std::fs only. No new dependencies (section 18 rule 1).

**Spec:** `docs/superpowers/specs/2026-09-26-phase3-playlist-persistence-design.md` (approved). Argue every decision from it; when this plan and the spec disagree, the spec wins.

## Global Constraints

- Exe size <= 10,485,760 bytes (budget 1). Headroom at start: 10,380,288 B (~105 KB). **Task 1 measures the size gate first; over budget -> stop and report options (section 18 rule 1), do not proceed.**
- No new dependencies, no new threads/timers/periodic tasks (section 18 rules 1-2). The 30 s checkpoint folds into the existing `recv_timeout` loop.
- State file: `%LOCALAPPDATA%` + `/qpid/state.json` (partner decision; Task 1 updates architecture.md section 9 and the section 13 `hush/` repo root).
- `PREV_RESTART_THRESHOLD_MS = 5000` (named constant, partner decision).
- Next/EOF-advance always starts the new file at position 0 (partner decision, spec-literal section 7.4).
- All 8 existing DSP tests must stay green after every task. `cargo test` and `cargo build --release` must pass before each commit.
- Every task ends with a commit; never commit until its verification steps pass.
- Probe scripts go in `$env:TEMP` (a pre-approved temp dir) as `.ps1` files, run via `powershell -NoProfile -ExecutionPolicy Bypass -File`. Never `Start-Sleep <large int>` (means seconds; use `-Milliseconds`). Launch the CLI with `cmd /c "exe --cli file > out 2>&1"` and `RedirectStandardInput = $true`.
- Dependency rule (section 13): `ui.rs` imports only `Command`/`Event`/`Shared` from engine. `playlist.rs`/`store.rs` live at `src/` top level; `engine/mod.rs` declares `mod` for both.

---

### Task 1: `store.rs` + exe-size gate

**Files:**
- Create: `src/store.rs`
- Modify: `src/engine/mod.rs` (add `mod store;` next to `mod decode;`, and one `let mut store = store::load();` in `run()` so serde links)
- Modify: `architecture.md` line 230 (state path `hush` -> `qpid`) and line 334 (repo root `hush/` -> `q-pid/`)

**Interfaces (produced; relied on by Tasks 3-6):**
- `store::Store { version: u32, speed: f64, last_folder: String, folders: HashMap<String, Folder> }`
- `store::Folder { touched: u64, last_file: String, files: HashMap<String, FileEntry> }`
- `store::FileEntry { pos_ms: u64, size: u64, done: bool }`
- `store::load() -> Store` (never panics), `Store::save(&mut self)` (prunes, atomic tmp+rename, swallows errors)
- `Store::record(&mut self, folder, file, size, pos_ms, done)` (lowercase keys, strips long-path prefix, updates `last_folder`/`last_file`/`touched`, stores entry only if `pos_ms > 0 || done`, else removes it)
- `Store::position_for(&self, folder, file, size) -> u64` (0 on missing/size mismatch), `Store::is_done(&self, folder, file) -> bool`
- `store::speed_to_milli(f64) -> u32`, `store::milli_to_speed(u32) -> f64`, `store::key(&str) -> String`

- [ ] **Step 1: Create `src/store.rs` with tests (RED first — functions stubbed)**

Write the file with the struct/const definitions below plus these failing tests; stub every function body so tests fail (`unimplemented!()` is fine for `record`/`position_for`/`is_done`/`save_to`/`load_from` where the test needs real behavior — `load_from` may return `Store::default()` to fail the roundtrip test):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("qpid_store_test_{tag}_{}.json", std::process::id()))
    }

    #[test]
    fn roundtrip_and_corrupt_default() {
        let p = tmp_path("rt");
        let mut s = Store::default();
        s.record("D:\\Books", "Ch 1.mp3", 100, 5000, false);
        s.save_to(&p);
        let loaded = load_from(&p);
        assert_eq!(loaded.last_folder, "D:\\Books");
        assert!(loaded.folders[&key("d:\\books")].files[&key("ch 1.mp3")].pos_ms == 5000);
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(load_from(&p).version, SCHEMA_VERSION); // rule 4: corrupt -> defaults
        assert!(load_from(Path::new("Z:\\no\\such\\file.json")).version == SCHEMA_VERSION);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn size_mismatch_and_entry_rules() {
        let mut s = Store::default();
        s.record("F", "a.mp3", 100, 0, false);   // pos 0, not done -> no entry (rule 3)
        assert!(s.position_for("F", "a.mp3", 100) == 0);
        s.record("F", "b.mp3", 100, 50, false);
        assert_eq!(s.position_for("F", "b.mp3", 999), 0);  // size differs -> ignore pos (rule 2)
        assert_eq!(s.position_for("F", "b.mp3", 100), 50);
        s.record("F", "c.mp3", 100, 99, true);   // done with pos -> stored
        assert!(s.is_done("F", "c.mp3"));
        assert_eq!(s.last_folder, "F");
    }

    #[test]
    fn key_and_speed_helpers() {
        assert_eq!(key(r"\\?\C:\A"), key("c:\\a"));
        assert_eq!(speed_to_milli(1.25), 1250);
        assert_eq!(speed_to_milli(f64::NAN), 1000);
        assert!((milli_to_speed(1500) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn prune_keeps_100_newest() {
        let mut s = Store::default();
        for i in 0..105u64 {
            let f = format!("F{i}");
            s.folders.insert(f.clone(), Folder { touched: i, last_file: String::new(), files: HashMap::new() });
        }
        s.prune();
        assert_eq!(s.folders.len(), 100);
        assert!(!s.folders.contains_key("F0") && s.folders.contains_key("F104"));
    }
}
```

- [ ] **Step 2: Run tests, confirm RED**

Run: `cargo test store`
Expected: FAIL (stubbed functions / not-yet-existing tests).

- [ ] **Step 3: Implement `src/store.rs` (everything outside `mod tests`)**

The non-test portion:

```rust
//! JSON state persistence (architecture.md section 9): schema, atomic
//! save, pruning, key normalization. Never crashes on bad state: a
//! missing, corrupt or wrong-version file loads as defaults.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const RESUME_REWIND_MS: u64 = 0; // section 9 rule 8
const MAX_FOLDERS: usize = 100;       // section 9 rule 3

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Store {
    pub version: u32,
    pub speed: f64,
    pub last_folder: String,
    pub folders: HashMap<String, Folder>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Folder {
    pub touched: u64,
    pub last_file: String,
    pub files: HashMap<String, FileEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileEntry {
    pub pos_ms: u64,
    pub size: u64,
    pub done: bool,
}

impl Default for Store {
    fn default() -> Self {
        Store { version: SCHEMA_VERSION, speed: 1.0, last_folder: String::new(), folders: HashMap::new() }
    }
}

/// Lowercase + strip the Windows long-path prefix (section 9 rule 1).
pub fn key(s: &str) -> String {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    s.to_lowercase()
}

pub fn speed_to_milli(s: f64) -> u32 {
    if !s.is_finite() || s <= 0.0 { return 1000; }
    (s * 1000.0).round() as u32
}

pub fn milli_to_speed(m: u32) -> f64 {
    m as f64 / 1000.0
}

/// `%LOCALAPPDATA%/qpid/state.json`; None when LOCALAPPDATA is unset
/// (persistence silently disabled, never crash).
pub fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("qpid").join("state.json"))
}

pub fn load() -> Store {
    match state_path() {
        Some(p) => load_from(&p),
        None => Store::default(),
    }
}

fn load_from(p: &Path) -> Store {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(_) => return Store::default(),
    };
    match serde_json::from_str::<Store>(&text) {
        Ok(s) if s.version == SCHEMA_VERSION => s,
        _ => Store::default(), // corrupt / wrong version: defaults (rule 4)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Store {
    pub fn save(&mut self) {
        match state_path() {
            Some(p) => self.save_to(&p),
            None => {}
        }
    }

    fn save_to(&mut self, p: &Path) {
        self.prune();
        let text = match serde_json::to_string_pretty(self) {
            Ok(t) => t,
            Err(_) => return,
        };
        let dir = match p.parent() { Some(d) => d, None => return };
        if std::fs::create_dir_all(dir).is_err() { return; }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_err() { return; }
        // std::fs::rename replaces an existing destination on Windows
        // (MoveFileExW + REPLACE_EXISTING): the rule 6 atomic write.
        let _ = std::fs::rename(&tmp, p);
    }

    fn prune(&mut self) {
        while self.folders.len() > MAX_FOLDERS {
            let oldest = self.folders.iter().min_by_key(|(_, f)| f.touched).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => { self.folders.remove(&k); }
                None => break,
            }
        }
    }

    pub fn record(&mut self, folder: &str, file: &str, size: u64, pos_ms: u64, done: bool) {
        self.last_folder = folder.to_string();
        let fk = key(folder);
        let file_key = key(file);
        let touched = now_unix();
        let entry = self.folders.entry(fk.clone()).or_insert_with(|| Folder {
            touched,
            last_file: String::new(),
            files: HashMap::new(),
        });
        entry.touched = touched;
        entry.last_file = file_key.clone();
        if pos_ms > 0 || done {
            entry.files.insert(file_key, FileEntry { pos_ms, size, done });
        } else {
            entry.files.remove(&file_key); // rule 3: entries only if pos > 0 or done
        }
    }

    pub fn position_for(&self, folder: &str, file: &str, size: u64) -> u64 {
        self.folders
            .get(&key(folder))
            .and_then(|f| f.files.get(&key(file)))
            .filter(|e| e.size == size) // rule 2: size differs -> ignore position
            .map(|e| e.pos_ms)
            .unwrap_or(0)
    }

    pub fn is_done(&self, folder: &str, file: &str) -> bool {
        self.folders
            .get(&key(folder))
            .and_then(|f| f.files.get(&key(file)))
            .map(|e| e.done)
            .unwrap_or(false)
    }
}
```

- [ ] **Step 4: Run tests, confirm GREEN**

Run: `cargo test store`
Expected: 4 passed (plus the existing 8 DSP tests in the full run).

- [ ] **Step 5: Wire into engine so serde links, then MEASURE THE SIZE GATE**

In `src/engine/mod.rs`: add `pub mod store;` next to `pub mod decode;`. In `run()`, add as the first line after `lower_thread_priority();`:

```rust
    // Loaded here (section 5.2: engine owns persistence). Wire-up of the
    // triggers lands in a later task; loading now links serde and lets the
    // exe-size gate be measured before anything builds on it.
    let mut store = store::load();
    store.speed = store.speed; // deliberately trivial use; keep binding live
```

Then: `cargo build --release` and `Get-Item target\release\qpid.exe | ForEach-Object { $_.Length }`.

**GATE:** size <= 10,485,760 B. If OVER: STOP. Report the number, the delta, and options (e.g. `serde_json` without `preserve_order`, dropping `to_string_pretty` for compact output does not affect linked size — real options are profile tweaks or spec budget discussion) — do not continue to Task 2.

- [ ] **Step 6: Update architecture.md**

- Line 230: state location text `hush` -> `qpid` (keep `%LOCALAPPDATA%` wording).
- Line 334: tree root `hush/` -> `q-pid/`.

- [ ] **Step 7: Full test run + commit**

Run: `cargo test` (expect 12 passed: 8 DSP + 4 store), then:

```bash
git add src/store.rs src/engine/mod.rs architecture.md
git commit -m "Phase 3: store.rs (section 9 schema, atomic save) + exe size gate"
```

### Task 2: `playlist.rs` + folder-open uses it

**Files:**
- Create: `src/playlist.rs`
- Modify: `src/engine/mod.rs` (add `pub mod playlist;`; replace `first_audio_file_in` call + delete that function)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces (relied on by Tasks 3-6):
  - `playlist::is_playable(name: &str, attributes: u32) -> bool` (attributes = Windows `FILE_ATTRIBUTE` bits; tests pass 0)
  - `playlist::scan(dir: &Path) -> Vec<PathBuf>` (playable files only, natural-sorted by file name)
  - `playlist::index_of(files: &[PathBuf], path: &Path) -> Option<usize>` (unicode-lowercase full-path compare)

- [ ] **Step 1: Write failing tests in `src/playlist.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_playable_filters_exts_and_hidden_system() {
        assert!(is_playable("a.mp3", 0));
        assert!(is_playable("B.M4A", 0));           // case-insensitive ext
        assert!(!is_playable("a.txt", 0));
        assert!(!is_playable("a.mp3", 0x2));         // FILE_ATTRIBUTE_HIDDEN
        assert!(!is_playable("a.mp3", 0x4));         // FILE_ATTRIBUTE_SYSTEM
        assert!(is_playable("a.mp3", 0x20));         // ARCHIVE is fine
    }

    #[test]
    fn natural_sort_orders_1_to_30_numerically() {   // acceptance test 1
        let names: Vec<String> = (1..=30).map(|i| format!("{i}.mp3")).collect();
        let mut paths: Vec<std::path::PathBuf> =
            names.iter().map(|n| std::path::PathBuf::from(n)).collect();
        paths.sort_by(|a, b| {
            natord::compare(
                &a.file_name().unwrap_or_default().to_string_lossy(),
                &b.file_name().unwrap_or_default().to_string_lossy(),
            )
        });
        let sorted: Vec<String> = paths.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(sorted.first().unwrap(), "1.mp3");
        assert_eq!(sorted.get(9).unwrap(), "10.mp3");
        assert_eq!(sorted.last().unwrap(), "30.mp3");
        // "2" must sort before "10" (section 12.7)
        let pos2 = sorted.iter().position(|n| n == "2.mp3").unwrap();
        let pos10 = sorted.iter().position(|n| n == "10.mp3").unwrap();
        assert!(pos2 < pos10);
    }

    #[test]
    fn index_of_is_case_insensitive() {
        let files = vec![
            std::path::PathBuf::from("C:\\Music\\a.mp3"),
            std::path::PathBuf::from("C:\\Music\\b.mp3"),
        ];
        assert_eq!(index_of(&files, std::path::Path::new("c:\\MUSIC\\B.MP3")), Some(1));
        assert_eq!(index_of(&files, std::path::Path::new("C:\\Music\\zz.mp3")), None);
    }
}
```

- [ ] **Step 2: Run, confirm RED**

Run: `cargo test playlist`
Expected: FAIL (module not found).

- [ ] **Step 3: Implement `src/playlist.rs`**

```rust
//! Folder scan for the playlist (architecture.md sections 12.6, 12.7,
//! 13): playable extension filter, hidden/system skip, natural sort.

use std::path::{Path, PathBuf};

const EXTS: [&str; 7] = ["mp3", "m4a", "m4b", "aac", "flac", "ogg", "wav"];
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;

/// Pure predicate so tests do not have to fake file attributes.
pub fn is_playable(name: &str, attributes: u32) -> bool {
    let ext_ok = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false);
    let hidden = attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0;
    ext_ok && !hidden
}

/// All playable files in `dir` (no recursion, section 17), natural
/// sorted by file name so "2.mp3" precedes "10.mp3".
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    use std::os::windows::fs::MetadataExt;
    let mut out: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                if !p.is_file() { return false; }
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let attrs = p.metadata().map(|m| m.file_attributes()).unwrap_or(0);
                is_playable(name, attrs)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by(|a, b| {
        natord::compare(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    out
}

pub fn index_of(files: &[PathBuf], path: &Path) -> Option<usize> {
    let target = path.to_string_lossy().to_lowercase();
    files.iter().position(|p| p.to_string_lossy().to_lowercase() == target)
}
```

- [ ] **Step 4: Run, confirm GREEN**

Run: `cargo test playlist` -> 3 passed.

- [ ] **Step 5: Switch folder-open to `playlist::scan` and delete the old helper**

In `src/engine/mod.rs`:
1. Add `pub mod playlist;` beside `pub mod store;`.
2. In `open_path`, replace the directory branch body:

```rust
        match first_audio_file_in(&path) {
            Some(p) => p,
            None => { ... }
        }
```
with:
```rust
        match playlist::scan(&path).into_iter().next() {
            Some(p) => p,
            None => {
                let _ = tx_events.send(Event::Message("No audio files in folder".into()));
                return;
            }
        }
```
3. Delete the whole `fn first_audio_file_in` (mod.rs, ~20 lines). Its natural-sort logic now lives in `playlist::scan` — behavior for a folder open is identical (first naturally-sorted file).

- [ ] **Step 6: Full test + build + commit**

Run: `cargo test` (15 passed) and `cargo build --release`.

```bash
git add src/playlist.rs src/engine/mod.rs
git commit -m "Phase 3: playlist.rs scan/sort/index, folder open uses it"
```

---

### Task 3: `advance_to` navigation (no persistence yet)

**Files:**
- Modify: `src/engine/mod.rs` (flush helpers extracted, `swap_in`, `advance_to`, Next/Prev, EOF handoff, `handle_command`/`run` signatures, `Command` unchanged)

**Interfaces:**
- Consumes: `playlist::scan/index_of` (Task 2), `Decoder::open/sample_rate/duration_ms`, `Resampler::new`, `Stretcher::reset`, `Shared` fields.
- Produces (relied on by Tasks 4-6):
  - `const PREV_RESTART_THRESHOLD_MS: u64 = 5000;`
  - `fn advance_to(target: usize, playlist: &[PathBuf], idx: &mut usize, state: &mut PlayState, ctx: &mut Option<PlaybackContext>, released_path: &mut Option<PathBuf>, shared: &Arc<Shared>, tx_events: &Sender<Event>, ramp: bool) -> bool` — returns false only if it set `PlayState::Error` (caller then emits nothing extra); `ramp=true` for manual Next/Prev while Playing, `ramp=false` for the EOF handoff.
  - `fn swap_in(ctx: &mut PlaybackContext, new_dec: Decoder, new_path: PathBuf, shared: &Arc<Shared>)` — duration store + **unconditional resampler rebuild** + resets.
  - `handle_command` and both call sites gain `playlist: &mut Vec<PathBuf>` and `idx: &mut usize` parameters.
  - `run()` locals `playlist: Vec<PathBuf>` (empty at start) and `idx: usize` (0).

- [ ] **Step 1: Extract flush helpers (behavior-preserving refactor of `flush_playing`)**

In `src/engine/mod.rs`, add these two functions and rewrite `flush_playing` (currently mod.rs:642-680) to call them — the resulting sequence must remain byte-for-byte the same order as today (gain 0 -> sleep 20 -> flush_req -> wait ack (200 ms cap) -> clear_ring -> seek -> stretcher.reset -> resampler.reset -> base/played/eof/drained stores -> prefill -> clear req -> clear ack -> gain 1):

```rust
/// Section 7 flush steps 1-4 + ring clear, shared by flush_playing and
/// advance_to's manual (ramp) path.
fn ramp_and_flush_ack(ctx: &mut PlaybackContext, shared: &Arc<Shared>) {
    shared.set_gain(0.0);
    std::thread::sleep(Duration::from_millis(20));
    shared.flush_req.store(true, Ordering::SeqCst);
    let start = std::time::Instant::now();
    while !shared.flush_ack.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_millis(200) {
            break; // device stall; proceed rather than hang
        }
        std::thread::park_timeout(Duration::from_millis(2));
    }
    ctx.output.clear_ring();
}

/// Section 7 flush steps 5 (stores)-7: position bookkeeping, refill to
/// PREROLL, release the flush gate, ramp back up. The caller has already
/// seeked or swapped the decoder and reset the DSP.
fn refill_and_release_flush(ctx: &mut PlaybackContext, shared: &Arc<Shared>, base_ms: u64) {
    shared.base_ms.store(base_ms, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
    shared.eof.store(false, Ordering::Relaxed);
    shared.drained.store(false, Ordering::Relaxed);
    prefill(ctx, shared, PREROLL_MS);
    shared.flush_req.store(false, Ordering::SeqCst);
    shared.flush_ack.store(false, Ordering::SeqCst);
    shared.set_gain(1.0);
}
```

`flush_playing` becomes:

```rust
fn flush_playing(ctx: &mut PlaybackContext, target_ms: u64, shared: &Arc<Shared>) {
    ramp_and_flush_ack(ctx, shared);
    let _ = ctx.decoder.seek(target_ms);
    ctx.stretcher.reset();
    ctx.resampler.reset();
    refill_and_release_flush(ctx, shared, target_ms);
}
```

**Verify the refactor before continuing:** `cargo build --release`, then re-run the Phase 2 speed-change smoke (start `qpid.exe --cli test_audio\test.mp3`, send `s`, `f`, `q`): clean exit, `speed ->` printed, position advancing. This is the tested code the partner flagged — proof it still behaves comes now, not later.

- [ ] **Step 2: Add `swap_in` (the resampler-rebuild gate)**

```rust
/// Section 7.4.3 file handoff body: swap in a freshly opened decoder.
/// The resampler is rebuilt unconditionally — it was keyed to the
/// PREVIOUS file's sample rate at open_path, and a 44.1 -> 48 kHz advance
/// would otherwise reuse the wrong config (Phase 1 pitch bug in a new
/// disguise). Also stores the new duration so the UI can never show the
/// old track's length against the new track's position.
fn swap_in(ctx: &mut PlaybackContext, new_dec: Decoder, new_path: PathBuf, shared: &Arc<Shared>) {
    shared.duration_ms.store(new_dec.duration_ms().unwrap_or(0), Ordering::Relaxed);
    ctx.resampler = Resampler::new(new_dec.sample_rate(), ctx.output.device_rate());
    ctx.stretcher.reset(); // keeps speed (speed-keyed, not rate-keyed)
    ctx.decoder = new_dec;
    ctx.path = new_path;
    shared.base_ms.store(0, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
    shared.eof.store(false, Ordering::Relaxed);
    shared.drained.store(false, Ordering::Relaxed);
}
```

- [ ] **Step 3: Add `advance_to` and the TrackChanged helper**

```rust
fn track_event(playlist: &[PathBuf], idx: usize, shared: &Arc<Shared>) -> Event {
    let p = &playlist[idx];
    let title = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let folder = p.parent().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    Event::TrackChanged {
        folder,
        title,
        index: idx as u32 + 1,
        count: playlist.len() as u32,
        duration_ms: shared.duration_ms.load(Ordering::Relaxed),
    }
}

/// Navigate to `playlist[target]`. Three shapes (spec section 2):
/// - Playing + ramp: section 7 flush protocol, stream stays open.
/// - Playing + !ramp (EOF handoff): ring already empty — swap and refill
///   directly, no ramp (section 7.4.3, gap under 100 ms).
/// - Not Playing: record selection via released_path; Play reopens.
/// Forward-skip on a failed Decoder::open (section 6.9); Error only when
/// no file from `target` onward is playable. Returns false iff Error.
fn advance_to(
    target: usize,
    playlist: &[PathBuf],
    idx: &mut usize,
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    released_path: &mut Option<PathBuf>,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    ramp: bool,
) -> bool {
    if playlist.is_empty() || target >= playlist.len() {
        return true; // nothing to advance to (e.g. Next on last file: no-op)
    }
    // Try target, then forward (section 6.9: skip to the next file).
    let mut cand = target;
    loop {
        let path = playlist[cand].clone();
        match Decoder::open(&path) {
            Ok(new_dec) => {
                *idx = cand;
                match state {
                    PlayState::Playing if ctx.is_some() => {
                        let c = ctx.as_mut().unwrap();
                        if ramp {
                            ramp_and_flush_ack(c, shared);
                            swap_in(c, new_dec, path, shared);
                            refill_and_release_flush(c, shared, 0);
                        } else {
                            // EOF handoff: ring is empty, no flush needed.
                            shared.base_ms.store(0, Ordering::Relaxed);
                            swap_in(c, new_dec, path, shared);
                            prefill(c, shared, PREROLL_MS);
                        }
                    }
                    _ => {
                        // Paused / Ended / released: no live stream to
                        // flush. Metadata-only probe for the new duration
                        // (Decoder::open already computed it from
                        // codec_params; no packets decoded). If the probe
                        // decoder just opened IS the one we keep for the
                        // reopen path we only need its duration; drop it —
                        // resume() reopens via open_path.
                        shared.duration_ms.store(new_dec.duration_ms().unwrap_or(0), Ordering::Relaxed);
                        if let Some(c) = ctx.as_mut() {
                            // Paused < 10 s: drop the live context now
                            // (same work the 10 s release does).
                            *c = *match None { _ => break }; // placeholder-guard: see Step 3a
                        }
                        *released_path = Some(path);
                        shared.base_ms.store(0, Ordering::Relaxed);
                        shared.played_frames.store(0, Ordering::Relaxed);
                        *state = PlayState::Paused;
                        let _ = tx_events.send(Event::StateChanged(*state));
                    }
                }
                let _ = tx_events.send(track_event(playlist, *idx, shared));
                return true;
            }
            Err(_) => {
                let _ = tx_events.send(Event::Message(format!(
                    "Skipping unreadable file: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )));
                cand += 1;
                if cand >= playlist.len() {
                    *state = PlayState::Error;
                    let _ = tx_events.send(Event::StateError_marker());
                    return false;
                }
            }
        }
    }
}
```

**Step 3a (required fix before compiling):** the `_ =>` arm above is written defensively but the `break` placeholder is invalid Rust — replace that inner block with the correct drop of a live paused context:

```rust
                        if let Some(mut old) = ctx.take() {
                            old.output.pause(); // stream was already stopped while paused
                            // drop old: ring, decoder, DSP go with it (section 8.2)
                        }
```

and the `Event::StateError_marker()` call with the codebase's existing error emission pattern used in `open_path`:

```rust
                    let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                    let _ = tx_events.send(Event::Message("No playable files left".into()));
```

- [ ] **Step 4: Thread `playlist`/`idx` through `run` and `handle_command`**

1. `run()` (mod.rs:185): add locals after `released_path`:
```rust
    let mut playlist: Vec<PathBuf> = Vec::new();
    let mut idx: usize = 0;
```
2. `handle_command` signature gains `playlist: &mut Vec<PathBuf>, idx: &mut usize` (after `released_path`); update **both** call sites in `run()` (the `recv_timeout` arm and the refill pending-command arm) to pass `&mut playlist, &mut idx`.
3. `Command::Next` / `Command::Prev` replace the placeholders (mod.rs:338-344):

```rust
        Command::Next => {
            let cur = ctx.as_ref().map(|c| shared.position_ms()).unwrap_or(0);
            let _ = cur;
            if *idx + 1 < playlist.len() {
                if !advance_to(*idx + 1, playlist, idx, state, ctx, released_path, shared, tx_events, true) {
                    let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                }
            } // else: last file -> no-op (section 7.4.4: Ended comes from EOF)
        }
        Command::Prev => {
            let threshold = PREV_RESTART_THRESHOLD_MS;
            if shared.position_ms() > threshold {
                // Restart current file (named constant, partner decision).
                let dur_target_zero = 0;
                match *state {
                    PlayState::Playing => flush_playing(ctx.as_mut().unwrap(), dur_target_zero, shared),
                    _ => {
                        shared.base_ms.store(0, Ordering::Relaxed);
                        shared.played_frames.store(0, Ordering::Relaxed);
                        if let Some(c) = ctx.as_mut() { let _ = c.decoder.seek(0); c.stretcher.reset(); c.resampler.reset(); }
                        if let Some(p) = released_path.as_ref() { let _ = p; } // base already 0; reopen seeks there
                    }
                }
            } else if *idx > 0 {
                if !advance_to(*idx - 1, playlist, idx, state, ctx, released_path, shared, tx_events, true) {
                    let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                }
            } else if *state == PlayState::Playing {
                flush_playing(ctx.as_mut().unwrap(), 0, shared); // restart first file
            } else {
                shared.base_ms.store(0, Ordering::Relaxed);
                shared.played_frames.store(0, Ordering::Relaxed);
                if let Some(c) = ctx.as_mut() { let _ = c.decoder.seek(0); c.stretcher.reset(); c.resampler.reset(); }
            }
        }
```

Add `const PREV_RESTART_THRESHOLD_MS: u64 = 5000;` near the other consts (`PREROLL_MS` etc.).

- [ ] **Step 5: EOF handoff replaces `state = Ended` (run loop, mod.rs:244-256)**

Replace the block inside `if let Some(c) = ctx.as_mut()`:

```rust
                if c.output.is_drained_eof(&shared) {
                    let next = *idx + 1;
                    if next < playlist.len() {
                        // Section 7.4.3: mark done + advance, stream stays
                        // open (done-marking lands with save_now in Task 4).
                        if !advance_to(next, playlist, idx, state, ctx, released_path, shared, tx_events, false) {
                            let _ = tx_events.send(Event::StateChanged(PlayState::Error));
                        }
                    } else {
                        // Section 7.4.4: last file -> Ended.
                        *state = PlayState::Ended;
                        let _ = tx_events.send(Event::StateChanged(*state));
                    }
                }
```

Note: the borrow of `c` above ends before `advance_to` takes `ctx` — restructure to end the `c` borrow first if the compiler objects: compute `let drained = ctx.as_ref().map(|c| c.output.is_drained_eof(&shared)).unwrap_or(false);` **before** the `if let Some(c)` refill block and branch on `drained` after it. (The engine loop already tolerates this shape — follow whatever split `cargo` requires while keeping refill-then-check ordering.)

- [ ] **Step 6: Build + full tests + mixed-rate advance probe**

1. `cargo test` -> 15 passed. `cargo build --release`.
2. Probe: `New-Item -ItemType Directory -Force test_audio\mixed; Copy-Item test_audio\test.mp3, test_audio\tone48k.mp3 test_audio\mixed\` (test_audio is gitignored).
3. Write `$env:TEMP\opencode\probe_advance.ps1`: launch `qpid.exe --cli test_audio\mixed\test.mp3`, sleep 3 s, send `n` (advance to tone48k: 44.1 -> 48 kHz, resampler engaged -> bypassed), sleep 2 s, print blank line (position), send `n` again (past last file: no-op), send `b`-style check? No — then `q`. Relaunch the other direction: start with `tone48k.mp3`, `n` would be no-op (it is last? natural sort: test < tone48k so tone is last) — for 48 -> 44.1 use Prev: start `tone48k.mp3`, sleep 3 s, send `P` (position > 5 s -> restarts tone48k), sleep 1 s, send `P` again (within 5 s -> previous file test.mp3: 48 -> 44.1, bypassed -> engaged), sleep 2 s, print position, `q`.
4. Verify in output: `TrackChanged` with correct `index`/`count`/`duration_ms` per file (600032 for test.mp3, tone48k's duration for the other), position advancing at ~1x wall clock after each advance (no pitch/speed anomaly; 6 s at 1x ~ 6000 ms), clean exit 0 both directions. **This is the resampler-rebuild gate: a wrong rate here shows as position drift or a wrong-duration event.**
5. Also re-run the Phase 2 2x smoke on `test.mp3` alone (3x `s`, then `n` to EOF at 1.25x): `StateChanged(Ended)` still appears when the playlist has one file (EOF -> idx+1 >= len -> Ended).

- [ ] **Step 7: Commit**

```bash
git add src/engine/mod.rs
git commit -m "Phase 3: advance_to two-path navigation, real Next/Prev, EOF handoff"
```

### Task 4: persistence wiring — `save_now`, all 8 triggers, 30 s checkpoint, trigger-table gate

**Files:**
- Modify: `src/engine/mod.rs` (`save_now`, `handle_command` gains `store: &mut Store`, `pause`/`open_path` gain it, checkpoint in `run()`)

**Interfaces:**
- Consumes: `store::Store` + methods (Task 1), `advance_to` (Task 3).
- Produces:
  - `fn save_now(store: &mut Store, shared: &Arc<Shared>, current: Option<&Path>, reason: &'static str)`
  - `handle_command(..., playlist, idx, store: &mut Store, ...)`; `pause(..., store)`, `open_path(..., store, released_path, ...)`.
  - `run()` locals `next_checkpoint: Instant` (init `Instant::now() + CHECKPOINT_INTERVAL`), `last_saved_pos: u64` (init 0), `const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);`.

- [ ] **Step 1: Implement `save_now`**

```rust
/// Section 9 rule 5's single write path. `current` is the file being
/// left/updated (caller derives it from ctx.path or released_path);
/// None writes top-level fields only (speed) plus folder-less state.
fn save_now(store: &mut Store, shared: &Arc<Shared>, current: Option<&Path>, reason: &'static str) {
    store.speed = store::milli_to_speed(shared.speed_milli.load(Ordering::Relaxed));
    if let Some(p) = current {
        if let (Some(folder), Some(name)) = (p.parent(), p.file_name()) {
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            let pos = shared.position_ms();
            let dur = shared.duration_ms.load(Ordering::Relaxed);
            // Rule 4: done at EOS or within the last 3 s. Duration 0
            // (unknown) can only be done at EOS (the saturating compare
            // would otherwise be trivially true).
            let done = if dur == 0 {
                shared.eof.load(Ordering::Relaxed)
            } else {
                pos >= dur.saturating_sub(3000)
            };
            store.record(&folder.to_string_lossy(), &name.to_string_lossy(), size, pos, done);
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("[store] save ({reason})");
    }
    store.save();
}
```

- [ ] **Step 2: Thread `store` through signatures**

1. `handle_command`: add `store: &mut Store` (after `idx`); update both call sites in `run()`.
2. `pause(...)`: add `store: &mut Store` (it is called from `Command::Pause`, `TogglePlay`, and the device paths — update every caller).
3. `open_path(...)`: add `store: &mut Store` and `released_path: &mut Option<PathBuf>` (needed for trigger 4 and to clear a stale released path on success: at the end of the successful-branch, `*released_path = None;` — this also fixes the pre-existing staleness where OpenPath-after-release left `released_path` pointing at the old file).
4. `Command::OpenPath` arm: derive old current first:

```rust
        Command::OpenPath(path) => {
            let old = ctx.as_ref().map(|c| c.path.clone()).or_else(|| released_path.clone());
            if let Some(old) = old {
                if old != path {
                    let cur = shared.position_ms();
                    let _ = cur;
                    save_now(store, shared, Some(&old), "open of another path");
                    // ^^^ position of the old file is captured BEFORE the
                    // open resets bookkeeping; save_now reads shared state,
                    // so store the position snapshot by recording right
                    // here while base/played still describe the old file.
                }
            }
            open_path(path, state, ctx, paused_since, shared, tx_events, 0, store, released_path);
        }
```

Note: `save_now` reads `shared.position_ms()` — at this point `base_ms`/`played_frames`/`speed_milli`/`duration_ms` still describe the OLD file, so the recorded position is correct without any snapshot variable. (If the old file was paused-released, `duration_ms` may already describe it — it is only overwritten inside `open_path`/`swap_in`, both of which run after this call.)

- [ ] **Step 3: Place the 8 triggers**

| # | Trigger | Exact placement (grep `save_now` to verify) |
|---|---|---|
| 1 | pause | inside `pause()`, first line of the `if let Some(c) = ctx.as_mut()` body's function (before gain-down), current = `Some(&c.path.clone())`; also fires when TogglePlay->pause. If `ctx` is None, `pause()` still runs — pass `current = None` (nothing recorded, speed persisted). |
| 2 | next | `advance_to` at entry, **only when `ramp == true` or state was `Playing`** — simplest correct form: first line of `advance_to` after the bounds check: `save_now(store, shared, ctx.as_ref().map(\|c\| c.path.as_path()).or(released_path.as_deref()), "next/prev");` (requires `advance_to` to gain `store: &mut Store` — update Task 3's signature and its three call sites). The EOF call (`ramp == false`) hits the same line: that is rule 4's `done` write for the finished file. |
| 3 | prev | same `advance_to` line; restart-current in `Command::Prev` gets its own `save_now(store, shared, current, "prev restart")` **after** the restart so pos 0 is recorded (removes the entry per rule 3 — correct: user restarted). |
| 4 | open of another path | `Command::OpenPath` arm (Step 2 above), only when `old != path` |
| 5 | speed change | `Command::SetSpeed` arm, first line (position under old speed is still intact — same ordering insight as Phase 2's `cur` capture) |
| 6 | app exit | (a) `Command::Shutdown` arm, before `return false`; (b) `run()`'s `Disconnected` arm, before `break` |
| 7 | error stop | each `*state = PlayState::Error` site: two in `open_path` (derive current from the path being opened — it failed, so record the previous current if any: use `ctx`/`released_path` like trigger 4), one in `advance_to` (no file playable: current = old released_path/ctx path), any others found by grepping `PlayState::Error` |
| 8 | 30 s while playing | Step 4 below |

- [ ] **Step 4: Checkpoint in `run()`**

Add locals + const (as in Interfaces). After the match on `rx.recv_timeout(wait)` **and** before the refill block, add:

```rust
        // Section 9 rule 5: checkpoint every 30 s while playing, and only
        // if the position changed. Folded into the existing wake — no new
        // thread or timer (section 18).
        if *(&state) == PlayState::Playing && Instant::now() >= next_checkpoint {
            let pos = shared.position_ms();
            if pos != last_saved_pos {
                let current = ctx.as_ref().map(|c| c.path.as_path()).or(released_path.as_deref());
                save_now(&mut store, shared, current, "30s checkpoint");
                last_saved_pos = pos;
            }
            next_checkpoint = Instant::now() + CHECKPOINT_INTERVAL;
        }
```

(`Instant` is `std::time::Instant`; import or fully qualify. `state` is a local `PlayState`, compare with `==` directly: `if state == PlayState::Playing && ...`.)

Also shrink the Playing wait arm so the checkpoint can fire on time:

```rust
            PlayState::Playing => {
                // ... existing fill-based wait ...
                let until_ckpt = next_checkpoint.saturating_duration_since(std::time::Instant::now());
                wait.min(until_ckpt)
            }
```

- [ ] **Step 5: GATE — trigger table verification (partner requirement, not optional)**

Run: `Select-String -Path src\engine\mod.rs -Pattern 'save_now' | ForEach-Object { "$($_.LineNumber): $($_.Line.Trim())" }`

Produce the table from the actual output: each of the 8 triggers from section 9 rule 5 mapped to its line number. **Any trigger without a matching line = bug: fix before proceeding.** The table goes into the final report and IMPLEMENTATION.md.

- [ ] **Step 6: Tests + checkpoint probe + commit**

1. `cargo test` -> 15 passed; `cargo build --release`.
2. Probe: launch CLI with `test.mp3`, sleep 40 s (one checkpoint must fire at 30 s; debug build prints `[store] save` — use the **debug** build for this probe, `cargo build` then `target\debug\qpid.exe`), confirm the save line appears, `q` (exit trigger) produces another save, exit 0.
3. Confirm no save during a paused gap: send `p`, wait 5 s (no checkpoint line — only the pause-triggered save), `q`.

```bash
git add src/engine/mod.rs
git commit -m "Phase 3: save_now + all section 9 rule 5 triggers + 30s checkpoint"
```

---

### Task 5: restore flows + playlist population

**Files:**
- Modify: `src/engine/mod.rs` (`Command::RestoreSession` variant, restore logic, `open_path` sets playlist/idx/real TrackChanged)
- Modify: `src/main.rs` (no-args -> `RestoreSession` for both CLI and UI)

**Interfaces:**
- Consumes: `store::load/position_for/is_done/key/RESUME_REWIND_MS`, `playlist::scan/index_of`, `save_now` (Task 4).
- Produces: `Command::RestoreSession` (unit variant); `open_path` leaves `playlist` = sorted files of the opened folder (dir open) or the file's parent (single file), `idx` set, `TrackChanged` carries real `index/count`.

- [ ] **Step 1: `Command::RestoreSession` + main.rs wiring**

1. Add to the `Command` enum (beside `Prev`): `RestoreSession,`.
2. `handle_command` arm:

```rust
        Command::RestoreSession => {
            restore_session(store, state, ctx, released_path, playlist, idx, shared, tx_events, paused_since);
        }
```

3. `src/main.rs`: in `run_cli`, replace the `else { eprintln!("usage..."); return; }` with `cmd_tx.send(Command::RestoreSession)` (keep stdin loop); in `run_ui`, when `initial_path` is None send `RestoreSession` instead of doing nothing.

- [ ] **Step 2: `restore_session` (section 9 restore rule 1)**

```rust
/// Launch without arguments: last folder, last file, saved position,
/// stay Paused, do not autoplay (section 9 rule 1; acceptance test 6).
fn restore_session(
    store: &Store,
    state: &mut PlayState,
    ctx: &mut Option<PlaybackContext>,
    released_path: &mut Option<PathBuf>,
    playlist: &mut Vec<PathBuf>,
    idx: &mut usize,
    shared: &Arc<Shared>,
    tx_events: &Sender<Event>,
    paused_since: &mut Option<std::time::Instant>,
) {
    if store.last_folder.is_empty() {
        return; // no history: stay Idle with defaults
    }
    let folder = std::path::PathBuf::from(&store.last_folder);
    *playlist = playlist::scan(&folder);
    if playlist.is_empty() {
        return;
    }
    // Find the stored last_file (lowercase key) among the real names.
    let wanted = store
        .folders
        .get(&store::key(&store.last_folder))
        .map(|f| f.last_file.clone())
        .unwrap_or_default();
    let found = wanted
        .is_empty()
        .then_some(0)
        .unwrap_or_else(|| {
            playlist
                .iter()
                .position(|p| {
                    p.file_name()
                        .map(|n| store::key(&n.to_string_lossy()) == wanted)
                        .unwrap_or(false)
                })
                .unwrap_or(0)
        });
    *idx = found;
    let path = playlist[*idx].clone();
    // Size check (rule 2): mismatch -> position 0.
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut pos = store.position_for(&store.last_folder, &path.file_name().unwrap_or_default().to_string_lossy(), size);
    pos = pos.saturating_sub(store::RESUME_REWIND_MS); // rule 8
    // Metadata-only duration probe: Decoder::open reads codec_params
    // (duration comes free at open; no packets decoded) so the UI gets a
    // real duration without touching audio (budget 2).
    if let Ok(probe) = Decoder::open(&path) {
        shared.duration_ms.store(probe.duration_ms().unwrap_or(0), Ordering::Relaxed);
    }
    *paused_since = Some(std::time::Instant::now()); // treat as paused now
    shared.base_ms.store(pos, Ordering::Relaxed);
    shared.played_frames.store(0, Ordering::Relaxed);
    shared.speed_milli.store(store::speed_to_milli(store.speed), Ordering::Relaxed);
    *released_path = Some(path); // Play -> resume() reopens at base_ms
    *ctx = None;
    *state = PlayState::Paused;
    let _ = tx_events.send(track_event(playlist, *idx, shared));
    let _ = tx_events.send(Event::StateChanged(*state));
}
```

(`store.speed` for a fresh store is 1.0 -> 1000 milli ✓. `paused_since` set so PAUSE_RELEASE math cannot misbehave if a ctx-less pause path reads it.)

- [ ] **Step 3: `open_path` populates playlist/idx + saved positions (restore rules 2 and 3)**

In `open_path` (after `file_path` resolution, before `Decoder::open`):

```rust
    // Section 9 restore rules 2/3: the playlist is the folder's sorted
    // audio files; selection and start position come from the store.
    let folder = file_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let scanned = playlist::scan(&folder);
    *playlist = scanned;
    *idx = playlist::index_of(&playlist, &file_path).unwrap_or(0);
    let file_name = file_path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let folder_name = folder.to_string_lossy().to_string();
    let size_on_disk = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
    let saved_pos = store.position_for(&folder_name, &file_name, size_on_disk)
        .saturating_sub(store::RESUME_REWIND_MS);
    let start_pos = if resume_ms > 0 { resume_ms } else { saved_pos };
```

Then: use `start_pos` where the body currently uses `resume_ms` (`if start_pos > 0 { c.decoder.seek(start_pos) }`), and replace the hardcoded `index: 1, count: 1` in the `TrackChanged` emit with the `track_event(playlist, *idx, shared)` helper (or inline the real values).

`open_path` signature (final form): `fn open_path(path, state, ctx, paused_since, shared, tx_events, resume_ms: u64, store: &mut Store, released_path: &mut Option<PathBuf>, playlist: &mut Vec<PathBuf>, idx: &mut usize)` — update all callers (`OpenPath` arm, `resume()`, and any probe-only call sites). `resume()` passes the same `store`/`released_path` it already holds plus run-level playlist/idx — thread them through `resume` too (its signature gains playlist/idx/store; update `Play`/`TogglePlay` arms).

**Rule 2/3 pick order for a folder open:** after scanning, if `resume_ms == 0` (fresh `OpenPath`, not a resume):

```rust
    let fk = store::key(&folder_name);
    if resume_ms == 0 {
        let pick = store.folders.get(&fk).map(|f| {
            // last_file if present and not done; else first not-done; else first
            let last = &f.last_file;
            let last_ok = playlist.iter().position(|p| {
                p.file_name().map(|n| store::key(&n.to_string_lossy()) == *last).unwrap_or(false)
            }).filter(|&i| !f.files.get(last).map(|e| e.done).unwrap_or(false));
            last_ok.or_else(|| playlist.iter().position(|p| {
                f.files.get(&store::key(&p.file_name().unwrap_or_default().to_string_lossy()))
                    .map(|e| !e.done).unwrap_or(false)
            })).unwrap_or(0)
        }).unwrap_or(0);
        *idx = pick;
        // reopen at the picked file with its saved position (rule 2)
        let picked_path = playlist[*idx].clone();
        if picked_path != file_path {
            // recurse-free: re-point file_path via a single re-entry
            // (implementation: set file_path = picked_path and let the
            // body continue; all Decoder::open work happens after this
            // block anyway)
        }
    }
```

Implementation note: do this selection **before** `Decoder::open(&file_path)` and simply overwrite `file_path` (make it `let mut file_path`) — one pass, no recursion. Single-file opens (rule 3) hit the same code: their parent scan naturally yields `index_of` = the opened file unless rule 2's `last_file` says another file of that folder was current — which is exactly the spec's intent when opening a folder, but for a **single-file** open the spec says "Start at that file". So gate the rule-2 re-pick on "the OpenPath target was a directory": pass `was_dir: bool` (the original `path.is_dir()` result) through and apply the re-pick only when `was_dir`.

- [ ] **Step 4: Build + restore probe (acceptance test 6, dry run)**

1. `cargo test` (15), `cargo build --release`.
2. Probe A (fresh restore with history): run CLI with `test.mp3`, sleep 8 s, `p` (pause -> save), `q` (exit -> save). Relaunch CLI **without** a file: expect `TrackChanged { title: "test", ... }` + `StateChanged(Paused)` + NO `StateChanged(Playing)`; send blank line -> `position_ms` within a few seconds of 8000 (pause-time position, size-matched).
3. Probe B (no history): delete `%LOCALAPPDATA%\qpid\state.json`, relaunch no-args -> no events, no crash, exit 0 on `q`.
4. Probe C (corrupt history): write `garbage` into state.json, relaunch no-args -> no events, no crash.

```bash
git add src/engine/mod.rs src/main.rs
git commit -m "Phase 3: RestoreSession + rule 1-3 restore, playlist populated on open"
```

### Task 6: acceptance probes, budget re-checks, docs, final gates

**Files:**
- Modify: `BENCH.md` (budget 1 number, Phase 3 probe results), `IMPLEMENTATION.md` (Phase 3 status + trigger table), `src/engine/mod.rs` (reword the mod.rs:554 decode-error comment to say packet-skip is deferred, file-level skip landed in `advance_to`)
- Probe scripts in `$env:TEMP\opencode\` (not committed; `test_audio/` is gitignored)

- [ ] **Step 1: Full test sweep**

Run: `cargo test` (15 passed) and `cargo build --release`. Any failure = fix before continuing.

- [ ] **Step 2: Acceptance 6 probe (kill while playing -> relaunch -> within 30 s, paused)**

Write `$env:TEMP\opencode\probe_accept6.ps1`:

```powershell
$exe = "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\target\release\qpid.exe"
$mp3 = "C:\Users\Luiz\Desktop\gabriel\projetos_python\q-pid\test_audio\test.mp3"
$out1 = "$env:TEMP\opencode\acc6_killed.txt"
$out2 = "$env:TEMP\opencode\acc6_restore.txt"

$psi = New-Object Diagnostics.ProcessStartInfo
$psi.FileName = 'cmd.exe'
$psi.Arguments = "/c `"`"$exe`" --cli `"$mp3`" > `"$out1`" 2>&1`""
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$p = [Diagnostics.Process]::Start($psi)
Start-Sleep 65   # checkpoints fire at 30s and 60s
Get-Process qpid -ErrorAction SilentlyContinue | Stop-Process -Force   # rule 7: <= 30s loss
Start-Sleep 1

$psi2 = New-Object Diagnostics.ProcessStartInfo
$psi2.FileName = 'cmd.exe'
$psi2.Arguments = "/c `"`"$exe`" --cli > `"$out2`" 2>&1`""
$psi2.UseShellExecute = $false
$psi2.RedirectStandardInput = $true
$p2 = [Diagnostics.Process]::Start($psi2)
Start-Sleep 3
$p2.StandardInput.WriteLine('')   # print position
Start-Sleep 1
$p2.StandardInput.WriteLine('q')
$null = $p2.WaitForExit(8000)
"--- restore output ---"
Get-Content $out2
```

**Asserts:** `TrackChanged` title `test`; **no** `StateChanged(Playing)` (restore rule 1: paused, no autoplay); `position_ms` in [35000, 70000] (killed at ~65 s, last checkpoint 60 s, bound = 30 s loss per rule 7). Run with `powershell -NoProfile -ExecutionPolicy Bypass -File ...`.

- [ ] **Step 3: Budget re-checks (gate: budgets 1 and 6)**

1. Budget 1: `Get-Item target\release\qpid.exe | ForEach-Object { $_.Length }` — must be <= 10,485,760. Record the number (it will have grown with serde).
2. Budget 6 spot-check: `python tools/bench.py --exe target/release/qpid.exe --scenario playing-2x-minimized --file test_audio/test.mp3 --duration 60 --cli` — PASS if <= 2.0%. One run is a regression spot-check (Phase 3 adds no DSP work; expect the same 1.57–1.88 band).
3. Budgets 2/7/8: no design change (no new threads; checkpoint rides the existing wake). Note in BENCH that they were not re-run and why (one line).

- [ ] **Step 4: Trigger table from the REAL grep output (final form for the report)**

Run the `Select-String -Pattern 'save_now'` command from Task 4 Step 5; build the 8-row table with actual line numbers; paste into `IMPLEMENTATION.md` under Phase 3. Missing row = stop and fix.

- [ ] **Step 5: Documentation updates**

`IMPLEMENTATION.md`:
- Status bullet: Phase 3 build-verified — playlist (scan/natural sort/next/prev/EOF handoff with stream-reuse + resampler rebuild), store (section 9 schema, atomic save, triggers, 30 s checkpoint), restore rules 1–3, probes (mixed-rate advance, restore, acceptance 6 kill/relaunch), budgets 1/6 re-checked with numbers, trigger table.
- Measurement status: acceptance 6 result; remaining human-only checks (none new — Phase 3 has no listening criteria in §15.3).
- Deviations: packet-level decode-error skip (§6.9 "skip the packet") remains deferred — file-level skip (unreadable file -> next) landed in `advance_to`; mod.rs:554 comment reworded to say exactly that.

`BENCH.md`:
- Budget 1 row: new measured size.
- Budget 6 spot-check row/note if the number moved outside the old band.
- One-line note on budgets 2/7/8 not re-run (no design change).
- Phase 3 probe results section: mixed-rate advance, restore, acceptance 6 (with numbers).

`src/engine/mod.rs`: reword the `decode_burst` Err-branch comment (currently says "Phase 3 adds the skip bad packet / skip to next file behavior") to: file-level skip now lives in `advance_to` (unreadable file at open); per-packet skipping remains deferred — the burst just stops for this refill and the next refill retries (unchanged Phase 1 behavior).

- [ ] **Step 6: Final sweep + commit**

```bash
cargo test
cargo build --release
git add -A
git commit -m "Phase 3: acceptance probes, budget re-checks, docs; trigger table verified"
```

- [ ] **Step 7: Final report (to the human partner)**

Must contain: (a) the 8-row trigger table with line numbers, (b) exe size vs budget, (c) budget 6 spot number, (d) acceptance 6 probe result (position delta), (e) mixed-rate advance result, (f) any deviations from spec.

---

## Self-review (written by the planner, run before handoff)

1. **Spec coverage:** spec sections 1–5 map to Tasks 1–5; testing/gates to Task 6; exe-size risk = Task 1 Step 5 gate; trigger table = Tasks 4/6; resampler-rebuild = Task 3 Step 2 + mixed-rate probe; duration-store in Path 1 = `swap_in`; metadata-only restore probe = Task 5 Step 2. Deferred packet-skip explicitly documented (spec "Deferred" section) = Task 6 Step 5.
2. **Placeholder scan:** Task 3 Step 3 contains a deliberately-marked invalid placeholder guarded by Step 3a (the fix is spelled out) — an executor must apply Step 3a before building; Task 5 Step 3 contains one empty `if` body whose instruction is the following implementation note (set `file_path = picked_path` before `Decoder::open`) — both are instructions with complete code, not TBDs.
3. **Type consistency:** `advance_to(target: usize, playlist: &[PathBuf], idx: &mut usize, state, ctx, released_path, shared, tx_events, ramp: bool) -> bool` used at all three call sites (Next/Prev/EOF) plus `store: &mut Store` added by Task 4 at all three; `save_now(store, shared, current: Option<&Path>, reason: &'static str)` consistent; `open_path` final signature listed once in Task 5 Step 3 and its callers enumerated.



