//! Job Objects: the reliable way to know "who launched what" on Windows.
//!
//! A process assigned to a job stays in it (together with every descendant it
//! spawns afterwards) even after its parent dies, which is exactly the orphan
//! case. `ParentProcessId` is useless there because Windows never reparents.
//!
//! # Putting a child inside a job
//!
//! There are three ways, from most to least race-free:
//!
//! 1. **`PROC_THREAD_ATTRIBUTE_JOB_LIST`** (Windows 10+): pass
//!    [`JobGroup::raw_handle`] in the `STARTUPINFOEX` attribute list of
//!    `CreateProcessW`. The process is born inside the job; nothing to race.
//!    Use this if you already build a `STARTUPINFOEX` (ConPTY does, for
//!    `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`; both attributes can be combined).
//! 2. **`CREATE_SUSPENDED` + assign + resume**: create the process suspended,
//!    call [`JobGroup::assign_handle`] with `PROCESS_INFORMATION::hProcess`, then
//!    `ResumeThread(hThread)`. [`JobGroup::spawn_command`] does exactly this for
//!    `std::process::Command`, and [`resume_process`] resumes by pid.
//! 3. **Assign right after spawn** ([`JobGroup::assign`] /
//!    [`JobGroup::assign_child`]): simplest, but there is a window between
//!    `CreateProcess` and the assignment in which the child may already have
//!    spawned grandchildren that then stay *outside* the job. Acceptable for a
//!    shell (it takes milliseconds to start), not for a fast-forking tool.
//!
//! Nested jobs are supported on Windows 8+, so being launched inside somebody
//! else's job (a terminal, VS Code, CI) does not prevent assignment.
//!
//! By default a job does **not** have `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`:
//! the processes survive Selachi, which is what "keep my dev server running"
//! needs. Use [`JobOptions::kill_on_close`] for the opposite.

use std::os::windows::io::{AsRawHandle, RawHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Foundation::{ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectAssociateCompletionPortInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{
    OpenThread, ResumeThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE, THREAD_SUSPEND_RESUME,
};
use windows::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus, OVERLAPPED,
};

use crate::error::{win32, Error, Result};
use crate::sys::{self, OwnedHandle};

const CREATE_SUSPENDED: u32 = 0x0000_0004;

// JOB_OBJECT_MSG_* values (winnt.h).
const MSG_END_OF_JOB_TIME: u32 = 1;
const MSG_END_OF_PROCESS_TIME: u32 = 2;
const MSG_ACTIVE_PROCESS_LIMIT: u32 = 3;
const MSG_ACTIVE_PROCESS_ZERO: u32 = 4;
const MSG_NEW_PROCESS: u32 = 6;
const MSG_EXIT_PROCESS: u32 = 7;
const MSG_ABNORMAL_EXIT_PROCESS: u32 = 8;
const MSG_PROCESS_MEMORY_LIMIT: u32 = 9;
const MSG_JOB_MEMORY_LIMIT: u32 = 10;

const JOB_KEY: usize = 0x5E1A_C410;
const SHUTDOWN_KEY: usize = 0x5E1A_DEAD;

/// Limit flags for a new job.
#[derive(Debug, Clone, Copy, Default)]
pub struct JobOptions {
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: kill every member when the last
    /// handle to the job closes (i.e. when the [`JobGroup`] is dropped).
    /// Default `false`: members survive Selachi.
    pub kill_on_close: bool,
    /// `JOB_OBJECT_LIMIT_BREAKAWAY_OK`: members may opt out with
    /// `CREATE_BREAKAWAY_FROM_JOB`. Default `false` (no escape).
    pub breakaway_ok: bool,
}

/// Event reported by a job's completion port (see [`JobGroup::notifications`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEvent {
    /// A process was added to the job (assigned, or spawned by a member).
    NewProcess(u32),
    /// A member exited normally.
    ExitProcess(u32),
    /// A member exited abnormally (crash / unhandled exception).
    AbnormalExitProcess(u32),
    /// The job has no active processes left.
    ActiveProcessZero,
    /// Any other `JOB_OBJECT_MSG_*` (limits); raw message id and pid (0 if n/a).
    Other {
        /// Raw message id.
        msg: u32,
        /// Associated pid, if any.
        pid: u32,
    },
}

struct Port(OwnedHandle);

/// A Windows Job Object grouping the processes of one owner (tab / thread).
pub struct JobGroup {
    handle: OwnedHandle,
    label: String,
    options: JobOptions,
    port: Mutex<Option<Arc<Port>>>,
}

impl std::fmt::Debug for JobGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobGroup")
            .field("label", &self.label)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl JobGroup {
    /// Creates an unnamed job with default options (members survive us).
    ///
    /// `label` is only kept in memory for diagnostics; the kernel object is
    /// anonymous so nothing else can open it by name.
    pub fn new(label: impl Into<String>) -> Result<Self> {
        Self::with_options(label, JobOptions::default())
    }

    /// Creates a job with explicit [`JobOptions`].
    pub fn with_options(label: impl Into<String>, options: JobOptions) -> Result<Self> {
        // SAFETY: anonymous job, no security attributes.
        let h =
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(win32("CreateJobObjectW"))?;
        let handle = OwnedHandle::new(h);

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if options.kill_on_close {
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        if options.breakaway_ok {
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        // SAFETY: `info` is a valid, fully initialised struct of the declared size.
        unsafe {
            SetInformationJobObject(
                handle.raw(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(win32("SetInformationJobObject(ExtendedLimit)"))?;

        Ok(Self {
            handle,
            label: label.into(),
            options,
            port: Mutex::new(None),
        })
    }

    /// Diagnostic label given at creation.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Options the job was created with.
    pub fn options(&self) -> JobOptions {
        self.options
    }

    /// Raw job handle, for `PROC_THREAD_ATTRIBUTE_JOB_LIST`. Owned by the
    /// `JobGroup`; do not close it.
    pub fn raw_handle(&self) -> RawHandle {
        self.handle.raw().0
    }

    /// Assigns a running process by pid (opens it with `SET_QUOTA|TERMINATE`).
    pub fn assign(&self, pid: u32) -> Result<()> {
        let h = sys::open_process(pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE)
            .map_err(win32("OpenProcess"))?;
        self.assign_owned(&h)
    }

    /// Assigns a process by raw handle (needs `PROCESS_SET_QUOTA` and
    /// `PROCESS_TERMINATE`). The handle is borrowed, not closed.
    pub fn assign_handle(&self, process: RawHandle) -> Result<()> {
        // SAFETY: caller supplies a valid process handle; we only borrow it.
        unsafe { AssignProcessToJobObject(self.handle.raw(), HANDLE(process)) }
            .map_err(win32("AssignProcessToJobObject"))
    }

    fn assign_owned(&self, h: &OwnedHandle) -> Result<()> {
        // SAFETY: `h` is a valid process handle owned by the caller.
        unsafe { AssignProcessToJobObject(self.handle.raw(), h.raw()) }
            .map_err(win32("AssignProcessToJobObject"))
    }

    /// Assigns a `std::process::Child` (see the module docs about the race).
    pub fn assign_child(&self, child: &Child) -> Result<()> {
        self.assign_handle(child.as_raw_handle())
    }

    /// Spawns `cmd` suspended, assigns it to the job, then resumes it, so no
    /// descendant can escape. `extra_creation_flags` are OR-ed with
    /// `CREATE_SUSPENDED` (this overrides any `creation_flags` already set on
    /// `cmd`). If anything fails the child is killed.
    pub fn spawn_command(&self, cmd: &mut Command, extra_creation_flags: u32) -> Result<Child> {
        cmd.creation_flags(CREATE_SUSPENDED | extra_creation_flags);
        let mut child = cmd.spawn()?;
        let r = self
            .assign_child(&child)
            .and_then(|()| resume_process(child.id()));
        if let Err(e) = r {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok(child)
    }

    /// Pids currently active in the job (includes processes spawned by
    /// members, even if their parent already exited).
    pub fn members(&self) -> Vec<u32> {
        match self.try_members() {
            Ok(v) => v,
            Err(e) => {
                log::warn!("job `{}`: members query failed: {e}", self.label);
                Vec::new()
            }
        }
    }

    /// Fallible version of [`members`](Self::members).
    pub fn try_members(&self) -> Result<Vec<u32>> {
        let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
        let mut cap: usize = 64;
        for _ in 0..12 {
            let bytes = header + cap * size_of::<usize>();
            let mut buf = vec![0u64; bytes.div_ceil(8)];
            // SAFETY: `buf` is 8-aligned and at least `bytes` long; the kernel
            // writes a JOBOBJECT_BASIC_PROCESS_ID_LIST in it.
            let r = unsafe {
                QueryInformationJobObject(
                    Some(self.handle.raw()),
                    JobObjectBasicProcessIdList,
                    buf.as_mut_ptr().cast(),
                    bytes as u32,
                    None,
                )
            };
            let list = buf.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            match r {
                Ok(()) => {
                    // SAFETY: the call succeeded so the header and
                    // `NumberOfProcessIdsInList` entries are initialised.
                    let (n, ids) = unsafe {
                        let n = (*list).NumberOfProcessIdsInList as usize;
                        (n, std::ptr::addr_of!((*list).ProcessIdList).cast::<usize>())
                    };
                    let mut out = Vec::with_capacity(n);
                    for i in 0..n.min(cap) {
                        // SAFETY: i < cap entries fit in the buffer.
                        out.push(unsafe { ids.add(i).read_unaligned() } as u32);
                    }
                    return Ok(out);
                }
                Err(e) if e.code() == HRESULT::from_win32(ERROR_MORE_DATA.0) => {
                    // SAFETY: header is initialised even on ERROR_MORE_DATA.
                    let assigned = unsafe { (*list).NumberOfAssignedProcesses } as usize;
                    cap = (cap * 2).max(assigned + 16);
                }
                Err(e) => return Err(win32("QueryInformationJobObject")(e)),
            }
        }
        Err(Error::Win32 {
            op: "QueryInformationJobObject",
            source: windows::core::Error::from_hresult(HRESULT::from_win32(ERROR_MORE_DATA.0)),
        })
    }

    /// True if `pid` is currently a member.
    pub fn contains(&self, pid: u32) -> bool {
        self.members().contains(&pid)
    }

    /// `TerminateJobObject`: hard-kills every member at once. This is the raw
    /// primitive with **no safety checks**; prefer [`crate::terminate_job`].
    pub fn terminate_raw(&self, exit_code: u32) -> Result<()> {
        // SAFETY: valid job handle.
        unsafe { TerminateJobObject(self.handle.raw(), exit_code) }
            .map_err(win32("TerminateJobObject"))
    }

    /// Zero-polling lifecycle events for this job via a completion port.
    ///
    /// A background thread blocks in `GetQueuedCompletionStatus` (0% CPU while
    /// idle) and forwards [`JobEvent`]s. It ends when the receiver is dropped
    /// (on the next event) or when the `JobGroup` is dropped. May only be
    /// called once per job (a job has a single completion port).
    pub fn notifications(&self) -> Result<UnboundedReceiver<JobEvent>> {
        let mut slot = self.port.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            return Err(Error::NotificationsAlreadyEnabled);
        }
        // SAFETY: creates a fresh completion port (no file handle associated).
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }
            .map_err(win32("CreateIoCompletionPort"))?;
        let port = Arc::new(Port(OwnedHandle::new(port)));

        let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: JOB_KEY as *mut _,
            CompletionPort: port.0.raw(),
        };
        // SAFETY: `assoc` is valid for the declared size.
        unsafe {
            SetInformationJobObject(
                self.handle.raw(),
                JobObjectAssociateCompletionPortInformation,
                std::ptr::from_ref(&assoc).cast(),
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        }
        .map_err(win32("SetInformationJobObject(CompletionPort)"))?;

        let (tx, rx) = unbounded();
        let thread_port = Arc::clone(&port);
        std::thread::Builder::new()
            .name(format!("job-events-{}", self.label))
            .spawn(move || pump(&thread_port, &tx))?;
        *slot = Some(port);
        Ok(rx)
    }
}

impl Drop for JobGroup {
    fn drop(&mut self) {
        let slot = self.port.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(port) = slot.as_ref() {
            // Wake the pump thread so it exits.
            // SAFETY: valid port handle; null OVERLAPPED is allowed.
            unsafe {
                let _ = PostQueuedCompletionStatus(port.0.raw(), 0, SHUTDOWN_KEY, None);
            }
        }
    }
}

fn pump(port: &Port, tx: &UnboundedSender<JobEvent>) {
    loop {
        let mut msg = 0u32;
        let mut key = 0usize;
        let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
        // SAFETY: all out pointers are valid locals.
        let r = unsafe {
            GetQueuedCompletionStatus(port.0.raw(), &mut msg, &mut key, &mut ov, u32::MAX)
        };
        if r.is_err() && ov.is_null() {
            break; // port closed / fatal
        }
        if key == SHUTDOWN_KEY {
            break;
        }
        if key != JOB_KEY {
            continue;
        }
        let pid = ov as usize as u32;
        let ev = match msg {
            MSG_NEW_PROCESS => JobEvent::NewProcess(pid),
            MSG_EXIT_PROCESS => JobEvent::ExitProcess(pid),
            MSG_ABNORMAL_EXIT_PROCESS => JobEvent::AbnormalExitProcess(pid),
            MSG_ACTIVE_PROCESS_ZERO => JobEvent::ActiveProcessZero,
            MSG_END_OF_JOB_TIME
            | MSG_END_OF_PROCESS_TIME
            | MSG_ACTIVE_PROCESS_LIMIT
            | MSG_PROCESS_MEMORY_LIMIT
            | MSG_JOB_MEMORY_LIMIT => JobEvent::Other { msg, pid },
            other => JobEvent::Other { msg: other, pid },
        };
        if tx.unbounded_send(ev).is_err() {
            break; // receiver dropped
        }
    }
}

/// Resumes every thread of a process (use after `CREATE_SUSPENDED`). Returns
/// an error if no suspended thread was found.
pub fn resume_process(pid: u32) -> Result<()> {
    // SAFETY: thread snapshot of the whole system.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
        .map_err(win32("CreateToolhelp32Snapshot(thread)"))?;
    let snap = OwnedHandle::new(snap);
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut resumed = 0u32;
    // SAFETY: `entry.dwSize` is set; iteration uses the documented protocol.
    let mut more = unsafe { Thread32First(snap.raw(), &mut entry) }.is_ok();
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: opening a thread by id; handle wrapped immediately.
            if let Ok(h) = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) } {
                let h = OwnedHandle::new(h);
                // SAFETY: valid thread handle with SUSPEND_RESUME access.
                if unsafe { ResumeThread(h.raw()) } != u32::MAX {
                    resumed += 1;
                }
            }
        }
        // SAFETY: same protocol as above.
        more = unsafe { Thread32Next(snap.raw(), &mut entry) }.is_ok();
    }
    if resumed == 0 {
        return Err(Error::Win32 {
            op: "ResumeThread",
            source: windows::core::Error::from_hresult(HRESULT::from_win32(
                windows::Win32::Foundation::ERROR_NOT_FOUND.0,
            )),
        });
    }
    Ok(())
}
