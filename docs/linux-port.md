# q-pid Linux Port — Flatpak / AUR / AppImage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship q-pid on Linux as the same codebase in three formats — AppImage, Flatpak, AUR — built by GitHub Actions, with zero local Linux/VM testing (a friend tests later).

**Architecture:** Create `src/platform/` (free functions behind `#[cfg]`, per linux-port.md §3), implement the Linux surface (XDG state, Unix-socket single instance, deliberate no-ops for EcoQoS/trim/MPRIS/raise), then add packaging files and two workflows: `ci.yml` (the Linux compile/test gate, since no Linux exists locally) and `release.yml` (tag/dispatch → exe + 2 AppImages + Flatpak bundle + AUR render).

**Tech Stack:** Rust (no new runtime dependencies — std `UnixListener`/`UnixStream` only), Slint 1.18.1, cpal/ALSA, GitHub Actions, linuxdeploy, flatpak-builder, PKGBUILD.

**Spec:** `linux-port.md` (binding for Linux; `architecture.md` binding elsewhere). **Decisions locked by user 2026-10-06:** GitHub Actions builds all artifacts · MIT license · Flatpak app-id `io.github.KajeJeka.qpid`, self-hosted bundle on GitHub Releases (no Flathub) · keep unconditional lowercasing of state keys (documented risk).

## Global Constraints

- Windows regression gates after every code task (Tasks 1, 3): `cargo test` passes (34 baseline; 35 after Task 3's new `key()` test), release warnings **≤ 7** (record count; T1 may reduce it — pin new baseline in ledger), release exe **≤ 10,485,760 B** (currently 10,452,992).
- **Budget 8 is AT CAP (12/12) on Windows** — no new Windows thread. The Linux `qpid-pipe` listener reuses the sanctioned 4th-thread slot (linux-port.md §7).
- **Zero new runtime dependencies.** Winresource moves under `[target.'cfg(windows)'.build-dependencies]`; `libc` is NOT added (Linux thread-priority stays a no-op, §4).
- Never run/claim Linux budgets — no Linux numbers in `BENCH.md` until the friend measures them (linux-port.md §15).
- No VM, no local Linux emulation, no tagging without explicit user approval. Pushes to `origin/master` are in scope (CI is the Linux gate).
- MPRIS, Snap, PipeWire-native cpal, i686/ARMv7: out of scope (§17). Repeat mode addendum: NOT bundled.
- Subagents edit only, never commit (R4); controller commits after review; ledger at `.superpowers/sdd/<workspace>/progress.md` (gitignored, R2).
- Work on `master`, matching every prior phase.

---

### Task 1: Platform abstraction — `winapi.rs` → `src/platform/` (Phase 7)

**Files:**
- Create: `src/platform/mod.rs`, `src/platform/windows.rs`
- Delete: `src/winapi.rs`
- Modify: `src/main.rs:6,163-202`, `src/ui.rs:320-322`, `src/engine/mod.rs:210`, `Cargo.toml:39-41`

**Interfaces:**
- Produces: module `crate::platform` re-exporting the existing surface (in Task 1 only the functions that exist today — `state_path`/`hidden_flags` land in Task 3).
- Listener signature still `Fn(Vec<u8>)` in Task 1; changes in Task 3.

- [ ] **Step 1: Write `src/platform/mod.rs`**

```rust
//! Platform boundary (linux-port.md section 3). Free functions behind
//! #[cfg], re-exported so the rest of the codebase imports one path
//! regardless of OS. No dyn Platform, no traits — single small binary.
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
```

- [ ] **Step 2: Create `src/platform/windows.rs` as a pure move of `src/winapi.rs`**

Rules (mechanical, no behavior change):
- New module doc: `//! Windows platform surface (moved from winapi.rs, Phase 7). Everything here is best-effort: a failed call must never break the UI.`
- Delete **every** `#[cfg(windows)]` attribute (the module is gated by `mod.rs`) and delete **every** `#[cfg(not(windows))]` block and its stub body (old lines 36-37, 66-70, 105-108, 138-139, 195-196, 250-251).
- Rename `pub fn set_ecoqos` → `pub fn set_background_power_mode` (spec §3 neutral naming; the `[eco]` debug line inside stays as-is).
- Keep `static MEDIA_TX`, `MEDIA_HWND`, and all unsafe blocks byte-identical.

- [ ] **Step 3: Update call sites**

`src/main.rs`: `mod winapi;` → `mod platform;`; all 6 `winapi::` → `platform::` (lines 163, 164, 169, 175, 187, 202).

`src/ui.rs:320`: `crate::winapi::set_ecoqos(!visible);` → `crate::platform::set_background_power_mode(!visible);`
`src/ui.rs:322`: `crate::winapi::trim_working_set();` → `crate::platform::trim_working_set();`

`src/engine/mod.rs:210`: `crate::winapi::lower_thread_priority();` → `crate::platform::lower_thread_priority();`

- [ ] **Step 4: Gate `winresource` in `Cargo.toml`**

```toml
[build-dependencies]
slint-build = "1"

[target.'cfg(windows)'.build-dependencies]
winresource = "0.1"
```
(`build.rs` keeps its existing `#[cfg(target_os = "windows")]` gate — confirm it's there, do not re-add. Note: Windows→Linux *cross*-compilation of `build.rs` would break; we never cross-build — CI builds natively per platform.)

- [ ] **Step 5: Run Windows gates**

```powershell
cargo test                                   # expect 34 passed
cargo build --release 2>&1 | Select-String ': warning'   # record count; expect ≤ 7
(Get-Item target\release\qpid.exe).Length    # expect ~unchanged, cap 10,485,760
cargo tree --target x86_64-unknown-linux-gnu | Select-String 'windows|winresource'  # expect nothing
```
Expected: size delta ≈ 0 (identical code, different file name), warnings ≤ 7 (possibly fewer — pin the new number in the ledger as the gate baseline).

- [ ] **Step 6: Commit**

`Phase 7: platform abstraction — winapi.rs moves to src/platform, winresource target-gated`

---

### Task 2: CI workflow — the Linux compile/test gate (Phase 12 partial)

**Files:** Create `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Task 1 tree.
- Produces: on every push — Windows regression gate + Linux `cargo test`/`cargo build` gate. All later Linux code is verified ONLY through this workflow.

- [ ] **Step 1: Write `.github/workflows/ci.yml`**

```yaml
name: ci
on:
  push:
    branches: [master]
  pull_request:

jobs:
  windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Test
        run: cargo test
      - name: Release build + gates (warnings ≤ 7, size ≤ 10485760)
        shell: pwsh
        run: |
          $out = cargo build --release 2>&1 | Out-String
          $warn = ([regex]::Matches($out, '(?m)^\s*--> src[/\\]')).Count
          Write-Host "warning locations in src: $warn"
          if ($warn -gt 7) { Write-Host "::error::warning baseline exceeded"; exit 1 }
          $size = (Get-Item target\release\qpid.exe).Length
          Write-Host "exe size: $size"
          if ($size -gt 10485760) { Write-Host "::error::size budget exceeded"; exit 1 }

  linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build deps (cpal/ALSA)
        run: sudo apt-get update && sudo apt-get install -y libasound2-dev pkg-config
        # Contingency: if linking fails for winit's xkbcommon/wayland/X11 deps,
        # add: libxkbcommon-dev libwayland-dev libx11-dev
      - uses: dtolnay/rust-toolchain@stable
      - name: Test (includes linux.rs unit tests)
        run: cargo test
      - name: Release build (report size; no Linux budget yet — linux-port.md §15)
        run: cargo build --release && ls -l target/release/qpid
```

- [ ] **Step 2: Push, watch the first run**

Expected: `windows` green (34 tests, warnings ≤7, size ok); `linux` green = Task 1 compiles on Linux. If `linux` fails, fix and re-push before continuing (this loop is the substitute for local Linux builds).

- [ ] **Step 3: Commit + push**

`Phase 12: ci workflow — windows regression gates + linux build gate`

---

### Task 3: `platform/linux.rs` — state path, single instance, no-ops (Phase 9)

**Files:**
- Create: `src/platform/linux.rs`
- Modify: `src/platform/windows.rs` (add `state_path`, `hidden_flags`; listener signature), `src/store.rs:41-60` (delete `state_path`, comment on `key`), `src/playlist.rs:23-34` (hidden flags), `src/main.rs:173-188` (callback)

**Interfaces:**
- Produces (both platforms): `pub fn state_path() -> Option<PathBuf>`, `pub fn hidden_flags(path: &Path) -> u32`, `start_instance_listener(on_path: impl Fn(Option<String>) + Send + 'static)` (encoding/parsing now lives inside each platform impl).
- Consumes: `crate::engine::Command` for media-keys stubs.

- [ ] **Step 1: Windows side — move `state_path` and `hidden_flags` into `platform/windows.rs`**

```rust
/// `%LOCALAPPDATA%\qpid\state.json`; None when LOCALAPPDATA is unset
/// (persistence silently disabled, never crash).
pub fn state_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(std::path::PathBuf::from(base).join("qpid").join("state.json"))
}

/// Windows hidden/system file attributes (architecture.md 12.6).
pub fn hidden_flags(path: &std::path::Path) -> u32 {
    use std::os::windows::fs::MetadataExt;
    path.metadata().map(|m| m.file_attributes()).unwrap_or(0)
}
```

- [ ] **Step 2: Windows side — listener signature change**

Change `start_instance_listener(on_path: impl Fn(Vec<u8>) + Send + 'static)` to `impl Fn(Option<String>) + Send + 'static`. Inside the pipe thread, replace the raw-buf handoff with the UTF-16 decode **moved from main.rs**:

```rust
let path = if buf.len() >= 2 && buf.len() % 2 == 0 {
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16(&units).ok().filter(|s| !s.is_empty())
} else {
    None // empty payload = "just raise the window"
};
on_path(path);
```

`src/main.rs` call site becomes:

```rust
platform::start_instance_listener(move |path| {
    if let Some(s) = path {
        let _ = cmd_tx.send(Command::OpenPath(PathBuf::from(s)));
    }
    platform::raise_window();
});
```

- [ ] **Step 3: Write `src/platform/linux.rs` (complete file)**

```rust
//! Linux platform surface (linux-port.md sections 3-7). Best-effort by
//! design: a failed call must never break the UI. Every no-op below is a
//! documented ruling, not an TODO — see the comment on each function.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// XDG Base Directory state dir (section 6): $XDG_STATE_HOME, else the
/// spec's own fallback ~/.local/state. Relative XDG values are ignored
/// per the spec.
pub fn state_path() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    state_path_from(xdg, home)
}

fn state_path_from(xdg: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match xdg {
        Some(p) if p.is_absolute() => p,
        _ => home?.join(".local").join("state"),
    };
    Some(base.join("qpid").join("state.json"))
}

/// Dotfiles are the Unix equivalent of FILE_ATTRIBUTE_HIDDEN (same flag
/// value, so the shared is_playable() predicate in playlist.rs is
/// unchanged on both platforms).
pub fn hidden_flags(path: &Path) -> u32 {
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let dot = path
        .file_name()
        .map_or(false, |n| n.to_string_lossy().starts_with('.'));
    if dot { FILE_ATTRIBUTE_HIDDEN } else { 0 }
}

/// ponytail: no-op — the 3 s ring absorbs scheduling delay (§11.6
/// rationale). Upgrade path if a measured Linux budget run shows the
/// engine starving: libc::setpriority on this thread.
pub fn lower_thread_priority() {}

/// linux-port.md §4: EcoQoS is Windows-only; no Linux scheduling trick
/// is ported speculatively. If Linux budget 5 ever fails, investigate
/// nice/SCHED_IDLE as a measured follow-up.
pub fn set_background_power_mode(_enabled: bool) {}

/// linux-port.md §4: permanently out of scope on Linux — no equivalent
/// concept worth translating ("never", not "later").
pub fn trim_working_set() {}

/// Bound listener parked between claim_instance() (binds) and
/// start_instance_listener() (accept loop) — mirrors Windows, where the
/// mutex is created in claim and the pipe in the listener.
static LISTENER: Mutex<Option<UnixListener>> = Mutex::new(None);

/// Section 7: $XDG_RUNTIME_DIR is mode-0700 and user-owned. No /tmp
/// fallback (world-writable, spoofable) — no dir = fail open, every
/// launch runs as its own instance.
fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(dir).join("qpid.sock"))
}

pub fn claim_instance() -> bool {
    match socket_path() {
        Some(p) => claim_at(&p),
        None => true, // fail open (section 7)
    }
}

fn claim_at(path: &Path) -> bool {
    match UnixListener::bind(path) {
        Ok(l) => {
            *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some(l);
            true
        }
        Err(_) => match UnixStream::connect(path) {
            Ok(_) => false, // a live first instance owns the socket
            Err(_) => {
                // Stale socket (crashed first instance): remove, retry once.
                let _ = std::fs::remove_file(path);
                match UnixListener::bind(path) {
                    Ok(l) => {
                        *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some(l);
                        true
                    }
                    Err(_) => true, // unexpected failure -> run independently
                }
            }
        },
    }
}

/// Second instance: write the path (UTF-8; empty = raise only), then the
/// caller exits. Best-effort: 10 tries x 50 ms, same pacing as Windows.
pub fn send_to_first_instance(path: Option<&Path>) {
    let Some(sp) = socket_path() else { return };
    let mut payload: Vec<u8> = Vec::new();
    if let Some(p) = path {
        payload.extend_from_slice(p.to_string_lossy().as_bytes());
    }
    for _ in 0..10 {
        if write_to(&sp, &payload).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn write_to(sock: &Path, payload: &[u8]) -> std::io::Result<()> {
    let mut s = UnixStream::connect(sock)?;
    s.write_all(payload) // drop closes -> listener sees EOF
}

/// The sanctioned 4th thread (name it for diagnostics), blocked on
/// accept — costs nothing while idle. Empty payload = raise only.
pub fn start_instance_listener(on_path: impl Fn(Option<String>) + Send + 'static) {
    let listener = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(listener) = listener else { return };
    let _ = std::thread::Builder::new()
        .name("qpid-pipe".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                let path = if buf.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&buf).into_owned())
                };
                on_path(path);
            }
        });
}

/// linux-port.md §3: Wayland compositors refuse force-focus by design;
/// an X11 implementation would be platform-specific code for one nicety.
/// ponytail: no-op — delivering OpenPath is the observable effect.
/// Upgrade path: winit UserAttention/focus via the hook, if a user ever
/// reports the second-instance window never surfaces.
pub fn raise_window() {}

/// linux-port.md §8: MPRIS over D-Bus is its own future phase.
pub fn start_media_keys(_tx: std::sync::mpsc::Sender<crate::engine::Command>) {}

pub fn stop_media_keys() {}
```

- [ ] **Step 4: Add the `linux.rs` unit tests (same file)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn state_path_rules() {
        assert_eq!(
            state_path_from(Some("/x".into()), Some("/h".into())),
            Some(PathBuf::from("/x/qpid/state.json"))
        );
        // Relative XDG values are ignored (XDG spec).
        assert_eq!(
            state_path_from(Some("rel".into()), Some("/h".into())),
            Some(PathBuf::from("/h/.local/state/qpid/state.json"))
        );
        assert_eq!(state_path_from(None, None), None);
    }

    #[test]
    fn hidden_flags_dotfiles() {
        assert_eq!(hidden_flags(Path::new("/a/.hidden")), 0x2);
        assert_eq!(hidden_flags(Path::new("/a/track.mp3")), 0);
    }

    // One test for the whole flow: LISTENER is a process-wide static, so
    // the bind/claim/listen assertions must not run in parallel.
    #[test]
    fn single_instance_flow() {
        let dir = std::env::temp_dir().join(format!("qpid_sock_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("qpid.sock");
        let _ = std::fs::remove_file(&sock);

        assert!(claim_at(&sock));                  // first instance
        assert!(!claim_at(&sock));                 // live peer -> second instance
        let (tx, rx) = std::sync::mpsc::channel();
        start_instance_listener(move |p| { let _ = tx.send(p); });

        assert!(write_to(&sock, b"/home/u/Ch 1.mp3").is_ok());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some("/home/u/Ch 1.mp3".to_string())
        );
        assert!(write_to(&sock, b"").is_ok());     // empty = raise only
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), None);

        // Stale socket: close the listener, leave the file behind.
        drop(LISTENER.lock().unwrap().take());
        assert!(claim_at(&sock));                  // connect fails -> rebind

        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 5: `store.rs` and `playlist.rs` wiring**

`store.rs` — delete the whole `state_path` function (lines 55-60); replace its two uses in `load()`/`save()` with `crate::platform::state_path()`. Extend `key()`'s doc comment:

```rust
/// Lowercase + strip the Windows long-path prefix (section 9 rule 1).
/// Ruling (linux-port.md section 6): lowercasing is UNCONDITIONAL, on
/// Linux too — one shared code path and schema. Documented risk: two
/// files in one folder whose names differ only by case would collide
/// (rare on Linux, impossible on Windows). The `\\?\` strip is a no-op
/// on Unix paths (they never carry it).
```

Add the test (runs on both platforms — pure string):

```rust
#[test]
fn key_unix_paths_are_only_lowercased() {
    assert_eq!(key("/home/User/Books/Ch1.mp3"), "/home/user/books/ch1.mp3");
    assert_eq!(key(r"\\?\C:\A"), key("c:\\a")); // strip still works
}
```

`playlist.rs:29-34` — replace the Windows-only block:

```rust
.filter(|p| {
    if !p.is_file() { return false; }
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    is_playable(name, crate::platform::hidden_flags(p))
})
```
and remove `use std::os::windows::fs::MetadataExt;` (line 24). Add one line to `index_of`'s comment: `// Lowercase on all platforms too (linux-port.md section 6 ruling — same direction as store::key).`

- [ ] **Step 6: Windows gates + single-instance probe**

```powershell
cargo test                    # expect 35 passed (34 + key_unix test; linux tests not compiled on Windows)
cargo build --release 2>&1 | Select-String ': warning'   # count unchanged vs Task 1 baseline
(Get-Item target\release\qpid.exe).Length               # cap 10,485,760
```

Probe (adapted from the Phase 5 probe; run in `$env:TEMP`):

```powershell
cargo build
$p1 = Start-Process target\debug\qpid.exe -ArgumentList 'test_audio' -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
$t0 = Get-Date
$p2 = Start-Process target\debug\qpid.exe -ArgumentList 'test_audio\test.mp3' -PassThru -WindowStyle Hidden
$p2.WaitForExit(3000) | Out-Null
$ms = ((Get-Date) - $t0).TotalMilliseconds
$ok = $p2.HasExited -and $ms -lt 3000 -and -not $p1.HasExited
Write-Host "inst2 exit ms=$ms inst1_alive=$(-not $p1.HasExited) OK=$ok"
Stop-Process -Id $p1.Id -Force
if (-not $ok) { exit 1 }
```
Expected: inst2 exits < 3 s (handoff worked), inst1 alive (raised/received OpenPath).

- [ ] **Step 7: Push — Linux CI is the real gate for `linux.rs`**

Expected: `linux` job green, `cargo test` shows 35 + the 3 `linux.rs` tests (38 total on Linux). Any compile error here is the Task 2 loop doing its job — fix, re-push.

- [ ] **Step 8: Commit + push**

`Phase 9: linux platform impl — XDG state dir, unix-socket single instance, playlist hidden_flags`

---

### Task 4: `bench.py` Linux/X11 minimize (Phase 10 prep)

**Files:** Modify `tools/bench.py:33-36, 76-101`

**Interfaces:** none (standalone harness).

- [ ] **Step 1: Extend `minimize_window`** — keep the Windows body, replace the guard:

```python
def minimize_window(pid: int) -> bool:
    """Best-effort minimize. Windows: user32.ShowWindow. Linux/X11:
    xdotool (search by pid, then windowminimize). Wayland: not supported —
    compositors refuse external window control (same caveat as the
    window-raise note in linux-port.md section 3); returns False with a
    warning, so minimized-scenario budgets are X11-only for the first
    Linux bench pass."""
    system = platform.system()
    if system == "Linux":
        try:
            out = subprocess.run(
                ["xdotool", "search", "--pid", str(pid)],
                capture_output=True, text=True, timeout=5,
            )
            ids = out.stdout.split()
            if not ids:
                print("xdotool found no window (Wayland session?); cannot minimize", file=sys.stderr)
                return False
            subprocess.run(["xdotool", "windowminimize", ids[0]], timeout=5, check=False)
            return True
        except (FileNotFoundError, subprocess.TimeoutExpired) as e:
            print(f"minimize failed ({e}); install xdotool for X11 sessions", file=sys.stderr)
            return False
    if system != "Windows":
        print("minimize is not supported on this platform; skipping", file=sys.stderr)
        return False
    # ... existing Windows ctypes/user32 body unchanged ...
```

Also update the module docstring lines 33-36 ("minimize-dependent scenarios need Windows" → "need Windows or Linux/X11 with xdotool").

- [ ] **Step 2: Syntax check**

`python -m py_compile tools/bench.py` — expected: silent success. (Behavior can only be verified on the friend's machine — that is the Phase 10 gate, not this task.)

- [ ] **Step 3: Commit**

`Phase 10 prep: bench.py Linux/X11 minimize via xdotool (Wayland documented gap)`

---

### Task 5: License + desktop assets (Phase 11 part 1)

**Files:** Create `LICENSE`, `assets/icon.png`, `assets/qpid.desktop`; Modify `Cargo.toml` (`license` field), `README.md` (License section)

**Interfaces:** produces `assets/qpid.desktop` + `assets/icon.png` consumed by Tasks 6, 7, 8.

- [ ] **Step 1: `LICENSE`** — standard MIT text, `Copyright (c) 2026 Kaje Jeka`.

- [ ] **Step 2: `Cargo.toml`** — add under `[package]`: `license = "MIT"`.

- [ ] **Step 3: Extract `assets/icon.png` from the ICO** (spec §10's claim the PNG already exists is false — deviation recorded in ledger; extraction, not re-commissioning):

```powershell
python -c "from PIL import Image; im = Image.open(r'assets\icon.ico'); print(im.size, im.info); im.save(r'assets\icon.png')"
```
If Pillow is missing: `pip install pillow --user` first. Verify the PNG is 256×256 (`python -c "from PIL import Image; print(Image.open(r'assets\icon.png').size)"`). If the largest ICO entry turns out smaller than 256, save that size and install the icon at the matching `hicolor/<WxH>` path in Tasks 6-8 instead.

- [ ] **Step 4: `assets/qpid.desktop`**

```ini
[Desktop Entry]
Type=Application
Name=q-pid
Comment=Lightweight audio player for audiobooks, podcasts, and lectures
Exec=qpid %f
Icon=qpid
Terminal=false
Categories=AudioVideo;Audio;Player;
MimeType=audio/mpeg;audio/mp4;audio/flac;audio/ogg;audio/x-wav;
StartupWMClass=qpid
```
(`%f`, not `%F` — `run_ui` only reads `args.first()`.)

- [ ] **Step 5: README License section** — replace "No license file has been chosen yet." with `MIT. See [LICENSE](LICENSE).`

- [ ] **Step 6: Gates + commit** — `cargo test` still 35, build unaffected. Commit: `Phase 11: MIT license, assets/icon.png (from ico), qpid.desktop`

---

### Task 6: AUR packaging (Phase 11 part 2)

**Files:** Create `packaging/aur/PKGBUILD`, `packaging/aur/README.md`

**Interfaces:** consumes `assets/qpid.desktop`, `assets/icon.png` (Task 5). CI (Task 9) substitutes `@VERSION@`/`@SHA256@`.

- [ ] **Step 1: `packaging/aur/PKGBUILD`**

```bash
# Maintainer: Kaje Jeka <gabrielalmeidathe.vila1@gmail.com>
# Template: CI substitutes @VERSION@ and @SHA256@ at release time
# (linux-port.md section 13 — checksum computed, never invented).
pkgname=qpid
pkgver=@VERSION@
pkgrel=1
pkgdesc='Lightweight audio player for audiobooks, podcasts, and lectures'
arch=('x86_64' 'aarch64')
url='https://github.com/KajeJeka/qpid'
license=('MIT')
depends=('alsa-lib' 'gcc-libs' 'glibc')
makedepends=('cargo')
source=("$pkgname-$pkgver.tar.gz::https://github.com/KajeJeka/qpid/archive/refs/tags/v$pkgver.tar.gz")
sha256sums=('@SHA256@')

prepare() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked
}

build() {
  cd "$pkgname-$pkgver"
  cargo build --frozen --release
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 target/release/qpid "$pkgdir/usr/bin/qpid"
  install -Dm644 assets/qpid.desktop "$pkgdir/usr/share/applications/qpid.desktop"
  install -Dm644 assets/icon.png "$pkgdir/usr/share/icons/hicolor/256x256/apps/qpid.png"
}
```

- [ ] **Step 2: `packaging/aur/README.md`** — publish procedure: (1) CI renders a release copy of this PKGBUILD with the tag's sha256 (download `qpid-AUR` artifact from the release run); (2) on an Arch box: `updpkgsums` sanity check + `makepkg --printsrcinfo > .SRCINFO`; (3) push `PKGBUILD` + `.SRCINFO` to `ssh://aur@aur.archlinux.org/qpid.git` (separate repo, per §13 — **manual until the user has an AUR account and adds a CI SSH secret**; do not automate the push).

- [ ] **Step 3: Commit** — `Phase 11: AUR PKGBUILD template (checksum rendered by CI)`

---

### Task 7: Flatpak manifest (Phase 11 part 3)

**Files:** Create `packaging/flatpak/io.github.KajeJeka.qpid.yml`, `packaging/flatpak/io.github.KajeJeka.qpid.metainfo.xml`

**Interfaces:** consumes desktop/icon from Task 5; built by Task 9's `flatpak` job.

- [ ] **Step 1: `packaging/flatpak/io.github.KajeJeka.qpid.yml`**

```yaml
# Self-hosted Flatpak, bundled onto GitHub Releases (user decision
# 2026-10-06 — not Flathub). linux-port.md section 12.
app-id: io.github.KajeJeka.qpid
runtime: org.freedesktop.Platform
runtime-version: '26.08'   # verify the branch exists on Flathub when this runs; fallback 25.08
sdk: org.freedesktop.Sdk
command: qpid
finish-args:
  - --socket=pulseaudio     # standard Flatpak audio grant (ALSA-over-PulseAudio / PipeWire pulse socket)
  - --socket=wayland
  - --socket=fallback-x11
  # home:ro covers session restore of a last-folder outside ~/Music when
  # no dialog was used (section 12's documented quirk); file dialogs
  # themselves go through rfd's XDG portal and need no grant.
  # Contingency: if the friend reports NO SOUND, cpal's ALSA backend may
  # need raw device access -> add --device=all and re-test (deliberate
  # sandbox weakening, keep the comment either way).
  - --filesystem=home:ro
modules:
  - name: qpid
    buildsystem: simple
    build-options:
      build-args:
        # Self-hosted only: cargo needs the network inside the build
        # sandbox. Flathub would require cargo-vendored sources instead —
        # rework this line only if a Flathub submission ever happens.
        - --share=network
    build-commands:
      - cargo build --release --locked
      - install -Dm755 target/release/qpid /app/bin/qpid
      - sed 's/^Icon=qpid$/Icon=io.github.KajeJeka.qpid/'
          assets/qpid.desktop
          > /app/share/applications/io.github.KajeJeka.qpid.desktop
      - install -Dm644 assets/icon.png
          /app/share/icons/hicolor/256x256/apps/io.github.KajeJeka.qpid.png
      - install -Dm644 packaging/flatpak/io.github.KajeJeka.qpid.metainfo.xml
          /app/share/metainfo/io.github.KajeJeka.qpid.metainfo.xml
    sources:
      - type: dir
        path: ../..
```

- [ ] **Step 2: `io.github.KajeJeka.qpid.metainfo.xml`**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>io.github.KajeJeka.qpid</id>
  <name>q-pid</name>
  <summary>Lightweight audio player for audiobooks, podcasts, and lectures</summary>
  <metadata_license>MIT</metadata_license>
  <project_license>MIT</project_license>
  <description>
    <p>A folder-based audio player: naturally sorted playlists, seek bar,
    ±15 s skips, five pitch-preserving playback speeds, session restore,
    and strict resource budgets. Formats: mp3, m4a, m4b, aac, flac, ogg, wav.</p>
  </description>
  <launchable type="desktop-id">io.github.KajeJeka.qpid.desktop</launchable>
  <url type="homepage">https://github.com/KajeJeka/qpid</url>
  <releases>
    <release version="1.1.0" date="2026-10-06"/>
  </releases>
</component>
```
(version/date = the release tag at publish time; placeholder for now, Task 9 docs note the bump).

- [ ] **Step 3: Validate YAML shape locally** — `python -c "import yaml,sys; yaml.safe_load(open(r'packaging\flatpak\io.github.KajeJeka.qpid.yml')); print('ok')"` (PyYAML if present; else skip — CI is the validator). Commit: `Phase 11: Flatpak manifest + metainfo (io.github.KajeJeka.qpid)`

---

### Task 8: AppImage build script (Phase 11 part 4)

**Files:** Create `packaging/appimage/build.sh`

**Interfaces:** consumed by Task 9's AppImage jobs (inside `ubuntu:22.04` container); consumes desktop/icon from Task 5.

- [ ] **Step 1: `packaging/appimage/build.sh`**

```bash
#!/usr/bin/env bash
# q-pid AppImage (linux-port.md section 11). Run from the repo root inside
# an old-glibc container (ubuntu:22.04 = oldest LTS still receiving
# security updates as of this plan; glibc 2.35 baseline). Needs rustc,
# pkg-config, libasound2-dev. Produces qpid-*.AppImage in the repo root.
set -euo pipefail

ARCH="$(uname -m)"    # x86_64 | aarch64
cargo build --release --locked

rm -rf AppDir
mkdir -p AppDir/usr/bin
cp target/release/qpid AppDir/usr/bin/qpid

curl -fsSL -o linuxdeploy \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${ARCH}.AppImage"
chmod +x linuxdeploy

# CI containers have no FUSE: extract-and-run instead of mounting.
export APPIMAGE_EXTRACT_AND_RUN=1

./linuxdeploy \
  --appdir AppDir \
  --executable target/release/qpid \
  --desktop-file assets/qpid.desktop \
  --icon-file assets/icon.png \
  --output appimage
```

- [ ] **Step 2: shell syntax check** — `bash -n packaging/appimage/build.sh` (Git Bash available on this machine). Expected: silent success.

- [ ] **Step 3: Commit** — `Phase 11: AppImage build script (linuxdeploy, glibc 2.35 baseline)`

---

### Task 9: Release workflow — all artifacts (Phase 12)

**Files:** Create `.github/workflows/release.yml`

**Interfaces:** consumes Tasks 1-8; produces exe + `qpid-vX.Y.Z-x86_64.AppImage` + `qpid-vX.Y.Z-aarch64.AppImage` + `qpid-vX.Y.Z.flatpak` + AUR render, published to a GitHub Release on tag push (or as plain workflow artifacts on manual dispatch).

- [ ] **Step 1: Write `.github/workflows/release.yml`**

```yaml
name: release
on:
  push:
    tags: ['v*']
  workflow_dispatch:   # dry-run: builds everything, skips publishing

jobs:
  windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Gates (same as ci.yml)
        shell: pwsh
        run: |
          cargo test
          $out = cargo build --release 2>&1 | Out-String
          $warn = ([regex]::Matches($out, '(?m)^\s*--> src[/\\]')).Count
          Write-Host "warning locations in src: $warn"
          if ($warn -gt 7) { exit 1 }
          $size = (Get-Item target\release\qpid.exe).Length
          Write-Host "exe size: $size"
          if ($size -gt 10485760) { exit 1 }
      - run: Copy-Item target\release\qpid.exe "qpid-${env:GITHUB_REF_NAME}-windows-x86_64.exe"
      - uses: actions/upload-artifact@v4
        with: { name: windows, path: "qpid-*-windows-x86_64.exe" }

  appimage-x86_64:
    runs-on: ubuntu-latest
    container: ubuntu:22.04   # pinned glibc baseline — NEVER ubuntu-latest (section 14)
    steps:
      - uses: actions/checkout@v4
      - name: Toolchain
        run: |
          apt-get update && apt-get install -y curl ca-certificates build-essential pkg-config libasound2-dev git
          curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
          echo "$HOME/.cargo/bin" >> $GITHUB_PATH
      - run: bash packaging/appimage/build.sh
      - run: mv qpid-*.AppImage "qpid-${GITHUB_REF_NAME}-x86_64.AppImage"
      - uses: actions/upload-artifact@v4
        with: { name: appimage-x86_64, path: "qpid-*-x86_64.AppImage" }

  appimage-aarch64:
    runs-on: ubuntu-24.04-arm   # free for public repos; disable if unavailable on the account
    container: ubuntu:22.04
    steps:
      - uses: actions/checkout@v4
      - name: Toolchain
        run: |
          apt-get update && apt-get install -y curl ca-certificates build-essential pkg-config libasound2-dev git
          curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
          echo "$HOME/.cargo/bin" >> $GITHUB_PATH
      - run: bash packaging/appimage/build.sh
      - run: mv qpid-*.AppImage "qpid-${GITHUB_REF_NAME}-aarch64.AppImage"
      - uses: actions/upload-artifact@v4
        with: { name: appimage-aarch64, path: "qpid-*-aarch64.AppImage" }

  flatpak:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install flatpak tooling
        run: sudo apt-get update && sudo apt-get install -y flatpak flatpak-builder
      - name: Runtime (branch must match the manifest's runtime-version)
        run: |
          flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo
          flatpak install --user -y flathub org.freedesktop.Platform//26.08 org.freedesktop.Sdk//26.08
      - name: Build + bundle
        run: |
          flatpak-builder --force-clean --repo=repo build-dir packaging/flatpak/io.github.KajeJeka.qpid.yml
          flatpak build-bundle repo qpid.flatpak io.github.KajeJeka.qpid
      - run: mv qpid.flatpak "qpid-${GITHUB_REF_NAME}.flatpak"
      - uses: actions/upload-artifact@v4
        with: { name: flatpak, path: "qpid-*.flatpak" }

  aur:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Render PKGBUILD with the tag's real checksum (section 13)
        run: |
          TAG="${GITHUB_REF_NAME}"
          VERSION="${TAG#v}"
          curl -fsSL "https://github.com/${GITHUB_REPOSITORY}/archive/refs/tags/${TAG}.tar.gz" -o src.tgz
          SHA="$(sha256sum src.tgz | cut -d' ' -f1)"
          sed -e "s/@VERSION@/${VERSION}/" -e "s/@SHA256@/${SHA}/" packaging/aur/PKGBUILD > PKGBUILD
          mkdir out && cp PKGBUILD packaging/aur/README.md out/
      - uses: actions/upload-artifact@v4
        with: { name: aur, path: out/ }

  publish:
    if: startsWith(github.ref, 'refs/tags/')
    needs: [windows, appimage-x86_64, appimage-aarch64, flatpak, aur]
    runs-on: ubuntu-latest
    permissions: { contents: write }
    steps:
      - uses: actions/download-artifact@v4
        with: { path: dist, merge-multiple: true }
      - name: Create release, attach everything
        env: { GH_TOKEN: "${{ github.token }}" }
        run: |
          gh release create "$GITHUB_REF_NAME" dist/* \
            --repo "$GITHUB_REPOSITORY" \
            --title "q-pid $GITHUB_REF_NAME" \
            --notes "Windows exe, AppImage (x86_64 + aarch64), Flatpak bundle, AUR PKGBUILD render. Linux budgets: not yet measured (see BENCH.md)."
```

- [ ] **Step 2: Consistency pass** — `runtime-version: '26.08'` in manifest and workflow must match; `linuxdeploy-${ARCH}` in Task 8 script handles both arches; artifact names follow linux-port.md §14.

- [ ] **Step 3: Commit + push** — `Phase 12: release workflow — exe, 2 AppImages, flatpak bundle, AUR render`

---

### Task 10: Dry-run every artifact (no tag)

**Files:** none (workflow run); ledger entry.

- [ ] **Step 1:** On GitHub → Actions → `release` → **Run workflow** (dispatch). This builds all five jobs without publishing.

- [ ] **Step 2:** Watch all jobs. Failure triage map:
  - `linux`/AppImage link errors → add the contingency apt packages (Task 2 comment) in build script/job.
  - flatpak: Sdk missing ALSA headers → add a small alsa-lib module to the manifest (documented contingency in ledger before doing it).
  - flatpak: `--share=network` cargo fetch failures → retry; if persistent, pin `CARGO_NET_OFFLINE=false` env in build-options.
  - runtime branch gone → bump manifest + workflow to the current freedesktop branch.
  - aarch64 runner label unavailable → drop that job to `continue-on-error: true` and record it as a known gap (x86_64 AppImage still ships).

- [ ] **Step 3:** Download all five artifacts; record names + byte sizes in the ledger. Report to the user: artifact list, sizes, CI run URL — **then** ask whether to tag (default suggestion: `v1.1.0`, minor bump for a new platform; never tag unasked).

- [ ] **Step 4: No commit** — results go to the ledger + Task 11 docs.

---

### Task 11: Documentation sweep

**Files:** Modify `README.md`, `docs/USER_GUIDE.md`, `handoff.md`, `IMPLEMENTATION.md`, `BENCH.md`, `architecture.md`

- [ ] **Step 1: `architecture.md` §1** — replace `` Working name: `hush`. Rename freely. `` with `Project name: q-pid (renamed before v1.0.0; every shipped artifact uses q-pid).` (spec's one-line correction).

- [ ] **Step 2: README** — Download section: add AppImage/Flatpak/AUR bullets (links only after a release exists — mark "after the first Linux release"); Building: Linux deps (`libasound2-dev pkg-config`, `cargo build --release`), note the three package formats; License line already fixed in Task 5; repository-layout table: `src/winapi.rs` → `src/platform/`.

- [ ] **Step 3: `docs/USER_GUIDE.md`** — new "Linux" section: install via AppImage (`chmod +x`, run), Flatpak (`flatpak install --user qpid.flatpak`), AUR (`makepkg -si`); known first-release limitations: (a) single-instance does not coordinate across formats, (b) window raise on Wayland is best-effort (OpenPath still delivered), (c) Flatpak session-restore needs the file inside `$HOME`, (d) media keys/MPRIS not yet supported on Linux, (e) state lives at `~/.local/state/qpid/state.json`.

- [ ] **Step 4: `BENCH.md`** — add a short "Linux budgets — NOT YET MEASURED" note: no Linux number exists until the friend runs `tools/bench.py` on Linux (X11, xdotool installed); explicitly forbids inheriting Windows numbers (§15).

- [ ] **Step 5: `IMPLEMENTATION.md`** — status entries: Phase 7 (platform abstraction), Phase 8/9 (Linux compile via CI, Linux impl), Phase 10 (deferred — pending physical Linux run), Phase 11 (three formats), Phase 12 (CI/release). Record the two spec deviations found: playlist.rs Windows-only scan (spec §2 wrong) and missing icon.png (spec §10 wrong), both fixed.

- [ ] **Step 6: `handoff.md`** — session record: what was done, the deviation findings, CI-as-Linux-gate approach, open items (friend's test checklist: AppImage on 2 distros, Flatpak sound + sandbox quirks, AUR makepkg, full budget suite Phase 10, MPRIS future phase).

- [ ] **Step 7: Gates + commit** — `cargo test` 35, build unchanged (docs only). Commit: `Linux port: docs sweep (README, USER_GUIDE, BENCH not-measured note, architecture name fix)`
