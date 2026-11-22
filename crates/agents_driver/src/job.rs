//! Spawning an agent process inside a Windows Job Object (PLAN.md section 15/16).
//!
//! The host (`proc_svc::JobGroup`) owns the job; we only receive its raw `HANDLE` as an
//! integer ([`crate::SpawnOpts::job`]) and never close it.
//!
//! # How the child gets in without a race
//!
//! 1. The process is created `CREATE_SUSPENDED | CREATE_NO_WINDOW`: its only thread has not run a
//!    single instruction, so it cannot have spawned any descendant yet.
//! 2. `AssignProcessToJobObject(job, child_handle)`. From here on every descendant is a member
//!    too (jobs are inherited; nested jobs are fine on Windows 8+, so being under a terminal's
//!    or CI's job does not matter). When the command is an npm `.cmd` shim that could not be
//!    resolved to the real exe, the process we create is `cmd.exe` and `node`/`claude.exe` run
//!    as its children, so the whole chain lands in the job as well.
//! 3. Resume. `std::process::Child` does not expose the primary thread handle, so we enumerate
//!    the threads of the new pid with Toolhelp32 (`CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)`)
//!    and `ResumeThread` them. This is the documented API (unlike `NtResumeProcess` from ntdll)
//!    and the same approach as `proc_svc::resume_process`; a freshly created process has exactly
//!    one thread, so there is nothing to resume twice. We retry briefly if the snapshot does not
//!    list it yet, and kill the child if it cannot be resumed (it would hang forever).
//!
//! If the assignment itself fails (e.g. the process is already in a job that forbids nesting) we
//! log a warning and still resume: the host's `register_root(pid)` fallback can assign later.
//! A job problem never fails a spawn.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::AssignProcessToJobObject;
use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) fn spawn_in_job(cmd: &mut Command, job: usize) -> io::Result<Child> {
    cmd.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = cmd.spawn()?;
    let job_handle = HANDLE(job as *mut core::ffi::c_void);
    // SAFETY: `job` is a live job handle owned by the host, `child` a valid process handle
    // (created with PROCESS_ALL_ACCESS); both are only borrowed for the call.
    let assigned = unsafe { AssignProcessToJobObject(job_handle, HANDLE(child.as_raw_handle())) };
    if let Err(e) = assigned {
        log::warn!("agents_driver: could not assign pid {} to the job object: {e}", child.id());
    }
    if let Err(e) = resume(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(child)
}

/// Resume every thread of `pid` (a just-created `CREATE_SUSPENDED` process).
fn resume(pid: u32) -> io::Result<()> {
    for attempt in 0..5 {
        if resume_threads(pid)? > 0 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10 * (attempt + 1)));
    }
    Err(io::Error::other(format!("no thread of pid {pid} could be resumed")))
}

fn resume_threads(pid: u32) -> io::Result<u32> {
    // SAFETY: system-wide thread snapshot; the handle is closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(io::Error::other)?;
    let mut entry = THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
    let mut resumed = 0;
    // SAFETY: `entry.dwSize` is set; documented iteration protocol.
    let mut more = unsafe { Thread32First(snap, &mut entry) }.is_ok();
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: open by id, closed right after use.
            if let Ok(h) = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) } {
                // SAFETY: valid thread handle with SUSPEND_RESUME access.
                if unsafe { ResumeThread(h) } != u32::MAX {
                    resumed += 1;
                }
                // SAFETY: we own `h`.
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
        }
        // SAFETY: same protocol as above.
        more = unsafe { Thread32Next(snap, &mut entry) }.is_ok();
    }
    // SAFETY: we own `snap`.
    unsafe {
        let _ = CloseHandle(snap);
    }
    Ok(resumed)
}
