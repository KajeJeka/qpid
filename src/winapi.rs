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
