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

        // Live listener, no accept thread needed: connect() succeeds via backlog.
        assert!(claim_at(&sock)); // first instance
        assert!(!claim_at(&sock)); // live peer -> second instance

        // Stale socket: close the listener, leave the file behind.
        LISTENER.lock().unwrap().take();
        assert!(claim_at(&sock)); // bind fails, connect fails -> rebind

        // Round trip through the accept thread.
        let (tx, rx) = std::sync::mpsc::channel();
        start_instance_listener(move |p| { let _ = tx.send(p); });
        assert!(write_to(&sock, b"/home/u/Ch 1.mp3").is_ok());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some("/home/u/Ch 1.mp3".to_string())
        );
        assert!(write_to(&sock, b"").is_ok()); // empty = raise only
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), None);

        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
