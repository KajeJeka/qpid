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
        let state = PROCESS_POWER_THROTTLING_STATE {
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
            // HANDLE is Copy (no Drop), so this is a pure "keep the value"
            // marker: the mutex lives until the process exits either way.
            #[allow(forgetting_copy_types)]
            std::mem::forget(h); // keep the mutex handle alive for the process
            first
        }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

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
        use windows::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
            PIPE_TYPE_BYTE, PIPE_WAIT,
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
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
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
