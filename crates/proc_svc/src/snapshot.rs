//! Process snapshot: ToolHelp32 enumeration enriched with per-process queries.
//!
//! Everything here uses `PROCESS_QUERY_LIMITED_INFORMATION`, which succeeds for
//! all same-user processes (elevated ones included) and for most services.
//! Processes that cannot be opened (protected / other users) are still listed
//! with `access_denied = true` and zeroed metrics; no call ever panics or fails
//! the whole snapshot.

use std::mem::size_of;
use std::time::{Duration, SystemTime};

use windows::Wdk::System::Threading::{NtQueryInformationProcess, PROCESSINFOCLASS};
use windows::Win32::Foundation::{FILETIME, UNICODE_STRING};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{GetProcessTimes, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::error::{Result, win32};
use crate::sys::{self, OwnedHandle};

/// `ProcessCommandLineInformation` (Windows 8.1+).
const PROCESS_COMMAND_LINE_INFORMATION: PROCESSINFOCLASS = PROCESSINFOCLASS(60);

/// One process at snapshot time.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    /// Process id.
    pub pid: u32,
    /// Parent pid as recorded at creation. **May be stale or reused**: validate
    /// with [`start_time`](Self::start_time) (a real parent started earlier).
    pub ppid: u32,
    /// Image name (e.g. `node.exe`).
    pub name: String,
    /// Full image path, if readable.
    pub exe_path: Option<String>,
    /// Full command line, if readable (via `NtQueryInformationProcess`).
    pub cmdline: Option<String>,
    /// Creation time; together with `pid` it identifies the process uniquely.
    pub start_time: Option<SystemTime>,
    /// Working set in bytes (0 if unknown).
    pub working_set: u64,
    /// Private (committed) bytes (0 if unknown).
    pub private_bytes: u64,
    /// Total CPU time, kernel + user (zero if unknown).
    pub cpu_time: Duration,
    /// Number of threads (ToolHelp).
    pub threads: u32,
    /// The process could not be opened; metrics are unavailable.
    pub access_denied: bool,
}

/// Options for [`snapshot_with`].
#[derive(Debug, Clone, Copy)]
pub struct SnapshotOptions {
    /// Read command lines (one extra syscall pair per process). Disable for
    /// high-frequency CPU/RAM sampling.
    pub cmdline: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self { cmdline: true }
    }
}

/// Snapshot of all processes, with command lines. Never fails: on a ToolHelp
/// error it logs and returns an empty list. See [`try_snapshot`].
pub fn snapshot() -> Vec<ProcInfo> {
    snapshot_with(SnapshotOptions::default())
}

/// [`snapshot`] with options.
pub fn snapshot_with(opts: SnapshotOptions) -> Vec<ProcInfo> {
    match try_snapshot(opts) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("process snapshot failed: {e}");
            Vec::new()
        }
    }
}

/// Fallible snapshot.
pub fn try_snapshot(opts: SnapshotOptions) -> Result<Vec<ProcInfo>> {
    // SAFETY: plain snapshot of all processes.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(win32("CreateToolhelp32Snapshot"))?;
    let snap = OwnedHandle::new(snap);

    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut out = Vec::with_capacity(256);
    // SAFETY: `dwSize` is set; standard First/Next iteration.
    let mut more = unsafe { Process32FirstW(snap.raw(), &mut entry) }.is_ok();
    while more {
        // Exited-but-not-reaped processes (someone holds a handle) linger in
        // the ToolHelp list with zero threads; skip them.
        if entry.cntThreads > 0 || entry.th32ProcessID == 0 {
            out.push(describe(&entry, opts));
        }
        // SAFETY: same protocol.
        more = unsafe { Process32NextW(snap.raw(), &mut entry) }.is_ok();
    }
    Ok(out)
}

fn describe(entry: &PROCESSENTRY32W, opts: SnapshotOptions) -> ProcInfo {
    let pid = entry.th32ProcessID;
    let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
    let name = String::from_utf16_lossy(&entry.szExeFile[..len]);

    let mut info = ProcInfo {
        pid,
        ppid: entry.th32ParentProcessID,
        name,
        exe_path: None,
        cmdline: None,
        start_time: None,
        working_set: 0,
        private_bytes: 0,
        cpu_time: Duration::ZERO,
        threads: entry.cntThreads,
        access_denied: false,
    };
    if pid == 0 || pid == 4 {
        info.access_denied = true;
        return info;
    }
    let Ok(h) = sys::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        info.access_denied = true;
        return info;
    };

    info.exe_path = sys::image_path(&h);
    fill_times(&h, &mut info);
    fill_memory(&h, &mut info);
    if opts.cmdline {
        info.cmdline = command_line(&h);
    }
    info
}

fn fill_times(h: &OwnedHandle, info: &mut ProcInfo) {
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: valid out pointers.
    if unsafe { GetProcessTimes(h.raw(), &mut c, &mut e, &mut k, &mut u) }.is_ok() {
        info.start_time = sys::filetime_to_system_time(sys::filetime_u64(c));
        let ticks = sys::filetime_u64(k).saturating_add(sys::filetime_u64(u));
        info.cpu_time = Duration::from_nanos(ticks.saturating_mul(100));
    }
}

fn fill_memory(h: &OwnedHandle, info: &mut ProcInfo) {
    let mut m = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: PROCESS_MEMORY_COUNTERS_EX starts with a PROCESS_MEMORY_COUNTERS
    // and `cb` tells the API which layout we pass.
    let ok = unsafe {
        GetProcessMemoryInfo(
            h.raw(),
            std::ptr::from_mut(&mut m).cast::<PROCESS_MEMORY_COUNTERS>(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    if ok.is_ok() {
        info.working_set = m.WorkingSetSize as u64;
        info.private_bytes = m.PrivateUsage as u64;
    }
}

/// Reads the command line without WMI (`NtQueryInformationProcess`, class 60).
/// Returns `None` when the OS refuses (protected process, Windows < 8.1).
fn command_line(h: &OwnedHandle) -> Option<String> {
    let mut needed = 0u32;
    // SAFETY: size probe with a null buffer.
    unsafe {
        let _ = NtQueryInformationProcess(
            h.raw(),
            PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 || needed > 1 << 20 {
        return None;
    }
    // Retry a couple of times: the command line length can change under us.
    let mut size = needed as usize + 64;
    for _ in 0..3 {
        let mut buf = vec![0u64; size.div_ceil(8)];
        let mut ret = 0u32;
        // SAFETY: `buf` is 8-aligned and `size` bytes long.
        let st = unsafe {
            NtQueryInformationProcess(
                h.raw(),
                PROCESS_COMMAND_LINE_INFORMATION,
                buf.as_mut_ptr().cast(),
                size as u32,
                &mut ret,
            )
        };
        if st.is_ok() {
            // SAFETY: on success the buffer starts with a UNICODE_STRING whose
            // `Buffer` points into the same allocation.
            return unsafe {
                let us = &*(buf.as_ptr().cast::<UNICODE_STRING>());
                if us.Buffer.is_null() || us.Length == 0 {
                    return None;
                }
                let base = buf.as_ptr() as usize;
                let p = us.Buffer.0 as usize;
                let n = usize::from(us.Length) / 2;
                if p < base || p + n * 2 > base + size {
                    return None;
                }
                let slice = std::slice::from_raw_parts(us.Buffer.0, n);
                Some(String::from_utf16_lossy(slice))
            };
        }
        if ret as usize <= size {
            return None;
        }
        size = ret as usize + 64;
    }
    None
}

/// Creation time of a single process (None if it cannot be opened / is gone).
pub fn start_time_of(pid: u32) -> Option<SystemTime> {
    let h = sys::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut info = ProcInfo {
        pid,
        ppid: 0,
        name: String::new(),
        exe_path: None,
        cmdline: None,
        start_time: None,
        working_set: 0,
        private_bytes: 0,
        cpu_time: Duration::ZERO,
        threads: 0,
        access_denied: false,
    };
    fill_times(&h, &mut info);
    info.start_time
}
