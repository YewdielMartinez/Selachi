//! Memory usage of the current process (PLAN section 12, differentiator #5).

/// Working set (resident memory) of the current process in bytes, if it can be read.
#[cfg(windows)]
pub fn working_set_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: `counters` is a properly sized, zero-initialized PROCESS_MEMORY_COUNTERS and
    // `GetCurrentProcess` returns a pseudo-handle that is always valid and needs no closing.
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        counters.cb = size;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) != 0 {
            Some(counters.WorkingSetSize as u64)
        } else {
            None
        }
    }
}

/// Non-Windows fallback: unsupported.
#[cfg(not(windows))]
pub fn working_set_bytes() -> Option<u64> {
    None
}

/// Formats a byte count as whole megabytes, e.g. `"58 MB"`.
pub fn format_mb(bytes: u64) -> String {
    format!("{} MB", bytes / (1024 * 1024))
}

/// Time elapsed since this process was created (kernel creation time), for startup phase logs.
#[cfg(windows)]
pub fn since_process_start() -> Option<std::time::Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    // SAFETY: all out-pointers are valid FILETIMEs; the pseudo-handle needs no closing.
    let created_ms = unsafe {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user,
        ) == 0
        {
            return None;
        }
        // 100 ns ticks since 1601-01-01; the Unix epoch is 11_644_473_600 s later.
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        (ticks / 10_000).checked_sub(11_644_473_600_000)?
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(std::time::Duration::from_millis(
        now_ms.saturating_sub(created_ms),
    ))
}

/// Non-Windows fallback: unsupported.
#[cfg(not(windows))]
pub fn since_process_start() -> Option<std::time::Duration> {
    None
}

/// `"123 ms"` since process start, or `"?"`; used to label startup phases in logs.
pub fn uptime_label() -> String {
    since_process_start().map_or_else(|| "?".into(), |d| format!("{} ms", d.as_millis()))
}
