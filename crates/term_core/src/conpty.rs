//! ConPTY child creation (Windows), adapted from `alacritty_terminal::tty::windows`
//! (Copyright The Alacritty Project Contributors, licensed under the Apache License 2.0,
//! <http://www.apache.org/licenses/LICENSE-2.0>).
//!
//! Why not `alacritty_terminal::tty::new`: it builds the `STARTUPINFOEXW` attribute list
//! internally with a single attribute (`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`), so there is no way
//! to also pass `PROC_THREAD_ATTRIBUTE_JOB_LIST`. With the job list the shell is *born inside*
//! the tab's Job Object: there is no window between `CreateProcess` and the assignment in which
//! the shell could have started a grandchild that stays outside the job.
//!
//! Differences from the original besides the job list:
//! - Handles are not leaked: the pseudoconsole-side pipe ends, the thread handle and the process
//!   handle are closed.
//! - Failures return errors instead of panicking (`assert_eq!(result, S_OK)`).
//! - `ClosePseudoConsole` can block until the output pipe is drained. It runs on a helper thread
//!   and the reader thread keeps draining (and discarding) after the consumer is gone, so
//!   dropping a session can neither hang the PTY thread nor lose the final output.
//! - The environment block is sorted (as `CreateProcessW` documents) and the program is quoted
//!   when it contains spaces.
//! - If the job list is rejected (old Windows, access problems) the shell is created suspended,
//!   assigned with `AssignProcessToJobObject` and then resumed: still race-free.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io::{self, Read, Write};
use std::iter::once;
use std::marker::PhantomData;
use std::mem;
use std::os::windows::ffi::OsStrExt;
#[cfg(test)]
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use piper::{Reader, Writer, pipe};
use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, PollMode, Poller};
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE, S_OK};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    RegisterWaitForSingleObject, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UnregisterWaitEx, UpdateProcThreadAttribute, WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
};
use windows_sys::core::{HRESULT, PWSTR, s, w};

use crate::session::JobAttachment;

/// Poll keys, shared with the PTY loop (`PTY_READ_WRITE_TOKEN` / `PTY_CHILD_EVENT_TOKEN`).
const PTY_CHILD_EVENT_TOKEN: usize = 1;
const PTY_READ_WRITE_TOKEN: usize = 2;
/// Capacity of the in-process pipes between the blocking I/O threads and the PTY loop.
const PIPE_CAPACITY: usize = 0x10_0000;

// ---------------------------------------------------------------------------
// ConPTY API (conpty.dll from Windows Terminal when available, else kernel32)
// ---------------------------------------------------------------------------

type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

#[derive(Clone, Copy)]
struct ConptyApi {
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

impl ConptyApi {
    /// The conpty.dll from the Windows Terminal project (next to the executable or in `PATH`)
    /// supports `OpenConsole.exe`, which fixes many issues of the in-box ConPTY.
    fn get() -> Self {
        static API: OnceLock<ConptyApi> = OnceLock::new();
        *API.get_or_init(|| match Self::load_conpty() {
            Some(api) => {
                log::info!("using conpty.dll for the pseudoconsole");
                api
            }
            None => {
                log::info!("using the Windows API for the pseudoconsole");
                Self {
                    create: CreatePseudoConsole,
                    resize: ResizePseudoConsole,
                    close: ClosePseudoConsole,
                }
            }
        })
    }

    fn load_conpty() -> Option<Self> {
        type LoadedFn = unsafe extern "system" fn() -> isize;
        // SAFETY: plain library loading; the transmutes match the documented signatures of the
        // three exported functions.
        unsafe {
            let hmodule = LoadLibraryW(w!("conpty.dll"));
            if hmodule.is_null() {
                return None;
            }
            let create = GetProcAddress(hmodule, s!("CreatePseudoConsole"))?;
            let resize = GetProcAddress(hmodule, s!("ResizePseudoConsole"))?;
            let close = GetProcAddress(hmodule, s!("ClosePseudoConsole"))?;
            Some(Self {
                create: mem::transmute::<LoadedFn, CreatePseudoConsoleFn>(create),
                resize: mem::transmute::<LoadedFn, ResizePseudoConsoleFn>(resize),
                close: mem::transmute::<LoadedFn, ClosePseudoConsoleFn>(close),
            })
        }
    }
}

fn coord(size: WindowSize) -> COORD {
    COORD {
        X: size.num_cols as i16,
        Y: size.num_lines as i16,
    }
}

/// RAII pseudoconsole.
struct Conpty {
    handle: HPCON,
    api: ConptyApi,
}

// SAFETY: a ConPTY handle can be used from any thread.
unsafe impl Send for Conpty {}

impl Drop for Conpty {
    fn drop(&mut self) {
        // `ClosePseudoConsole` blocks until the conout pipe is drained (see
        // https://docs.microsoft.com/en-us/windows/console/closepseudoconsole), and closing it
        // is also what ends the attached console clients. Do it off-thread; the reader thread
        // keeps draining until the host closes its end.
        let (handle, close) = (self.handle, self.api.close);
        let spawned = thread::Builder::new()
            .name("selachi-conpty-close".into())
            .spawn(move || {
                // SAFETY: `handle` came from `CreatePseudoConsole` and is closed exactly once.
                unsafe { close(handle) }
            });
        if let Err(e) = spawned {
            log::warn!("could not start the ConPTY close thread ({e}); closing inline");
            // SAFETY: as above.
            unsafe { (self.api.close)(self.handle) }
        }
    }
}

impl OnResize for Conpty {
    fn on_resize(&mut self, size: WindowSize) {
        // SAFETY: valid pseudoconsole handle.
        let result = unsafe { (self.api.resize)(self.handle, coord(size)) };
        if result != S_OK {
            log::warn!("ResizePseudoConsole failed: HRESULT {result:#010X}");
        }
    }
}

// ---------------------------------------------------------------------------
// Public (crate) entry point
// ---------------------------------------------------------------------------

/// Everything needed to start the shell.
pub(crate) struct SpawnSpec<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: Option<&'a Path>,
    /// Variables for the child; they win over the inherited environment.
    pub env: &'a [(String, String)],
    /// Raw handle of the Job Object the shell must live in (see `SpawnOptions::job`).
    pub job: Option<isize>,
    pub size: WindowSize,
    /// Skip the `JOB_LIST` attribute and use the suspended + assign + resume path (tests).
    pub force_job_fallback: bool,
}

/// A running ConPTY child with its I/O, ready to be handed to the PTY loop.
pub(crate) struct Pty {
    // XXX: `backend` must stay the first field: it is dropped before the pipes.
    backend: Conpty,
    conout: UnblockedReader,
    conin: UnblockedWriter,
    child: ChildWatcher,
    pid: u32,
    attachment: JobAttachment,
}

impl Pty {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn attachment(&self) -> JobAttachment {
        self.attachment
    }
}

/// Creates the pseudoconsole and the shell process.
pub(crate) fn spawn(spec: &SpawnSpec<'_>) -> io::Result<Pty> {
    let api = ConptyApi::get();

    let (conout_read, conout_write) = create_pipe()?;
    let (conin_read, conin_write) = match create_pipe() {
        Ok(p) => p,
        Err(e) => {
            close(conout_read);
            close(conout_write);
            return Err(e);
        }
    };

    let mut hpc: HPCON = 0;
    // SAFETY: valid pipe handles; `hpc` is a valid out pointer. Size 0 flags = default behavior.
    let hr = unsafe { (api.create)(coord(spec.size), conin_read, conout_write, 0, &raw mut hpc) };
    // ConPTY duplicates the handles it needs: our copies of its two ends must go away, otherwise
    // the output pipe never reports EOF when the host exits.
    close(conin_read);
    close(conout_write);
    if hr != S_OK {
        close(conin_write);
        close(conout_read);
        return Err(io::Error::other(format!(
            "CreatePseudoConsole failed: HRESULT {hr:#010X}"
        )));
    }

    // From here on every early return drops these, which closes the pseudoconsole (off-thread)
    // while the reader thread drains the output.
    // SAFETY: both handles are valid, owned, and used by exactly one `File`.
    let (conout_file, conin_file) = unsafe {
        (
            File::from_raw_handle(conout_read),
            File::from_raw_handle(conin_write),
        )
    };
    let conout = UnblockedReader::new(conout_file, PIPE_CAPACITY);
    let conin = UnblockedWriter::new(conin_file, PIPE_CAPACITY);
    let backend = Conpty { handle: hpc, api };

    let (info, attachment) = start_process(spec, hpc)?;
    let child = match ChildWatcher::new(info.hProcess) {
        Ok(child) => child,
        Err(e) => {
            // SAFETY: valid process handle owned by us; the child dies with the pseudoconsole.
            unsafe { CloseHandle(info.hProcess) };
            return Err(e);
        }
    };
    Ok(Pty {
        backend,
        conout,
        conin,
        child,
        pid: info.dwProcessId,
        attachment,
    })
}

fn create_pipe() -> io::Result<(HANDLE, HANDLE)> {
    let (mut read, mut write): (HANDLE, HANDLE) = (ptr::null_mut(), ptr::null_mut());
    // SAFETY: valid out pointers; null attributes make the handles non-inheritable; size 0
    // selects the default buffer size.
    if unsafe { CreatePipe(&raw mut read, &raw mut write, ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

fn close(handle: HANDLE) {
    // SAFETY: the caller owns `handle`.
    unsafe { CloseHandle(handle) };
}

// ---------------------------------------------------------------------------
// Process creation
// ---------------------------------------------------------------------------

/// `PROC_THREAD_ATTRIBUTE_LIST` storage, aligned for pointers.
struct AttributeList {
    buf: Vec<usize>,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: the sizing call is documented to fail with ERROR_INSUFFICIENT_BUFFER.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), count, 0, &raw mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut list = Self {
            buf: vec![0usize; size.div_ceil(mem::size_of::<usize>())],
        };
        // SAFETY: `buf` is at least `size` bytes and pointer aligned.
        if unsafe { InitializeProcThreadAttributeList(list.ptr(), count, 0, &raw mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn ptr(&mut self) -> *mut c_void {
        self.buf.as_mut_ptr().cast()
    }

    /// # Safety
    /// `value` must stay valid (and unmoved) until the process was created.
    unsafe fn set(&mut self, attribute: u32, value: *const c_void, size: usize) -> io::Result<()> {
        // SAFETY: forwarded to the caller.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                attribute as usize,
                value,
                size,
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: initialized in `new`.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

/// Starts the shell, inside the job when one is given. Returns how the job was joined.
fn start_process(
    spec: &SpawnSpec<'_>,
    hpc: HPCON,
) -> io::Result<(PROCESS_INFORMATION, JobAttachment)> {
    let Some(job) = spec.job else {
        return Ok((create_process(spec, hpc, None, false)?, JobAttachment::None));
    };
    let job = job as HANDLE;

    if !spec.force_job_fallback {
        match create_process(spec, hpc, Some(job), false) {
            Ok(info) => return Ok((info, JobAttachment::AtBirth)),
            Err(e) => log::warn!(
                "PROC_THREAD_ATTRIBUTE_JOB_LIST rejected ({e}); falling back to suspended start + \
                 AssignProcessToJobObject"
            ),
        }
    }

    // Fallback: the process is created suspended, so it cannot spawn anything before it is in
    // the job (a plain assign-after-spawn would race with its first grandchildren).
    let info = create_process(spec, hpc, None, true)?;
    // SAFETY: both are valid handles; the job handle is borrowed from the caller.
    let assigned = unsafe { AssignProcessToJobObject(job, info.hProcess) } != 0;
    let attachment = if assigned {
        JobAttachment::AssignedAfterSpawn
    } else {
        log::warn!(
            "AssignProcessToJobObject failed: {}; the shell runs outside the job",
            io::Error::last_os_error()
        );
        JobAttachment::AssignFailed
    };
    // SAFETY: valid thread handle of the suspended process.
    if unsafe { ResumeThread(info.hThread) } == u32::MAX {
        let err = io::Error::last_os_error();
        // SAFETY: handles are valid and owned by us.
        unsafe {
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
        }
        return Err(err);
    }
    // SAFETY: valid thread handle owned by us.
    unsafe { CloseHandle(info.hThread) };
    Ok((
        PROCESS_INFORMATION {
            hThread: ptr::null_mut(),
            ..info
        },
        attachment,
    ))
}

/// One `CreateProcessW` call. On success the thread handle is already closed (unless the
/// process was created suspended, when the caller needs it to resume).
fn create_process(
    spec: &SpawnSpec<'_>,
    hpc: HPCON,
    job: Option<HANDLE>,
    suspended: bool,
) -> io::Result<PROCESS_INFORMATION> {
    let mut attrs = AttributeList::new(if job.is_some() { 2 } else { 1 })?;
    // The job list array must outlive the CreateProcessW call below.
    let jobs: [HANDLE; 1] = [job.unwrap_or(ptr::null_mut())];
    // SAFETY: `hpc` is a live pseudoconsole; `jobs` lives until the end of this function.
    unsafe {
        attrs.set(
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            hpc as *const c_void,
            mem::size_of::<HPCON>(),
        )?;
        if job.is_some() {
            attrs.set(
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                jobs.as_ptr().cast(),
                mem::size_of_val(&jobs),
            )?;
        }
    }

    // SAFETY: all-zero is a valid STARTUPINFOEXW.
    let mut startup: STARTUPINFOEXW = unsafe { mem::zeroed() };
    startup.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    // With no handles set this keeps the child from inheriting ours.
    startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
    startup.lpAttributeList = attrs.ptr();

    let mut cmdline = wide(&command_line(spec.program, spec.args));
    let cwd = spec.cwd.map(|p| wide(p.as_os_str()));
    let env = environment_block(spec.env);
    let mut flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;
    if suspended {
        flags |= CREATE_SUSPENDED;
    }

    // SAFETY: all-zero is a valid PROCESS_INFORMATION (it is an out parameter).
    let mut info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    // SAFETY: every pointer is valid for the duration of the call; `cmdline` is mutable as
    // CreateProcessW requires; `env` is a double-NUL-terminated UTF-16 block.
    let ok = unsafe {
        CreateProcessW(
            ptr::null(),
            cmdline.as_mut_ptr() as PWSTR,
            ptr::null(),
            ptr::null(),
            FALSE,
            flags,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            &raw const startup.StartupInfo,
            &raw mut info,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if !suspended {
        // SAFETY: valid thread handle owned by us.
        unsafe { CloseHandle(info.hThread) };
        info.hThread = ptr::null_mut();
    }
    Ok(info)
}

fn wide<S: AsRef<OsStr> + ?Sized>(value: &S) -> Vec<u16> {
    value.as_ref().encode_wide().chain(once(0)).collect()
}

// Modified per stdlib implementation.
// https://github.com/rust-lang/rust/blob/6707bf0f59485cf054ac1095725df43220e4be20/library/std/src/sys/args/windows.rs#L174
fn push_escaped_arg(cmd: &mut String, arg: &str) {
    let quote = arg.bytes().any(|c| c == b' ' || c == b'\t') || arg.is_empty();
    if quote {
        cmd.push('"');
    }
    let mut backslashes: usize = 0;
    for x in arg.chars() {
        if x == '\\' {
            backslashes += 1;
        } else {
            if x == '"' {
                // Add n+1 backslashes to total 2n+1 before internal '"'.
                cmd.extend((0..=backslashes).map(|_| '\\'));
            }
            backslashes = 0;
        }
        cmd.push(x);
    }
    if quote {
        // Add n backslashes to total 2n before ending '"'.
        cmd.extend((0..backslashes).map(|_| '\\'));
        cmd.push('"');
    }
}

fn command_line(program: &str, args: &[String]) -> String {
    let mut cmd = String::new();
    // A program path with spaces would otherwise be ambiguous for CreateProcessW.
    let needs_quotes = program.contains([' ', '\t']) && !program.starts_with('"');
    if needs_quotes {
        cmd.push('"');
    }
    cmd.push_str(program);
    if needs_quotes {
        cmd.push('"');
    }
    for arg in args {
        cmd.push(' ');
        push_escaped_arg(&mut cmd, arg);
    }
    cmd
}

/// The child's environment: `custom` first, then the inherited variables that are not
/// overridden. Names are case-insensitive on Windows, so duplicates are dropped. The block is
/// sorted by upper-cased name as `CreateProcessW` documents.
fn environment_block(custom: &[(String, String)]) -> Vec<u16> {
    let mut seen = HashSet::new();
    let mut entries: Vec<(OsString, OsString, OsString)> = Vec::new();
    let mut add = |key: OsString, value: OsString, warn: bool| {
        let upper = key.to_ascii_uppercase();
        if seen.insert(upper.clone()) {
            entries.push((upper, key, value));
        } else if warn {
            log::warn!(
                "omitting environment variable with duplicate name: {}",
                key.to_string_lossy()
            );
        }
    };
    for (key, value) in custom {
        add(key.into(), value.into(), true);
    }
    for (key, value) in std::env::vars_os() {
        add(key, value, false);
    }
    entries.sort_by(|a, b| a.0.encode_wide().cmp(b.0.encode_wide()));

    let mut block = Vec::new();
    for (_, key, value) in entries {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    // An empty block still needs two terminators.
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

// ---------------------------------------------------------------------------
// Child exit notification
// ---------------------------------------------------------------------------

struct Interest {
    poller: Arc<Poller>,
    event: Event,
}

/// Shared with the thread-pool callback that fires when the process exits.
struct ExitShared {
    sender: mpsc::Sender<ChildEvent>,
    interest: Mutex<Option<Interest>>,
    process: AtomicPtr<c_void>,
}

/// WinAPI callback to run when the child process exits.
unsafe extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: u8) {
    if timed_out != 0 {
        return;
    }
    // SAFETY: `ctx` is the `Arc<ExitShared>` pointer kept alive by `ChildWatcher` until
    // `UnregisterWaitEx` has returned; we only borrow it.
    let shared = unsafe { &*(ctx as *const ExitShared) };
    let mut code = 0u32;
    let process = shared.process.load(Ordering::Acquire) as HANDLE;
    // SAFETY: the process handle is closed only after the wait was unregistered.
    let status = if unsafe { GetExitCodeProcess(process, &raw mut code) } == 0 {
        None
    } else {
        Some(ExitStatus::from_raw(code))
    };
    let _ = shared.sender.send(ChildEvent::Exited(status));
    if let Some(interest) = shared
        .interest
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        let _ = interest.poller.post(CompletionPacket::new(interest.event));
    }
}

/// Watches the shell process and owns its handle.
struct ChildWatcher {
    wait: HANDLE,
    ctx: *const ExitShared,
    shared: Arc<ExitShared>,
    events: mpsc::Receiver<ChildEvent>,
    process: HANDLE,
}

// SAFETY: the raw handles are plain kernel handles, usable from any thread.
unsafe impl Send for ChildWatcher {}

impl ChildWatcher {
    /// Takes ownership of `process` (closed on drop); on error the caller still owns it.
    fn new(process: HANDLE) -> io::Result<Self> {
        let (sender, events) = mpsc::channel();
        let shared = Arc::new(ExitShared {
            sender,
            interest: Mutex::new(None),
            process: AtomicPtr::new(process),
        });
        let ctx = Arc::into_raw(Arc::clone(&shared));
        let mut wait: HANDLE = ptr::null_mut();
        // SAFETY: `ctx` stays valid until `Drop` reclaims it after unregistering the wait.
        let ok = unsafe {
            RegisterWaitForSingleObject(
                &raw mut wait,
                process,
                Some(child_exit_callback),
                ctx.cast(),
                INFINITE,
                WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            // SAFETY: reclaim the reference leaked above; the callback never got registered.
            drop(unsafe { Arc::from_raw(ctx) });
            return Err(err);
        }
        Ok(Self {
            wait,
            ctx,
            shared,
            events,
            process,
        })
    }

    fn register(&self, poller: &Arc<Poller>, event: Event) {
        *self
            .shared
            .interest
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Interest {
            poller: poller.clone(),
            event,
        });
    }

    fn deregister(&self) {
        *self
            .shared
            .interest
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl Drop for ChildWatcher {
    fn drop(&mut self) {
        // SAFETY: `wait` was returned by `RegisterWaitForSingleObject`. INVALID_HANDLE_VALUE
        // blocks until a running callback finished, after which `ctx` and the process handle
        // are no longer used by anyone else.
        unsafe {
            UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE);
            drop(Arc::from_raw(self.ctx));
            CloseHandle(self.process);
        }
    }
}

// ---------------------------------------------------------------------------
// Blocking pipe I/O on helper threads, driven through `polling`
// (adapted from alacritty_terminal::tty::windows::blocking)
// ---------------------------------------------------------------------------

struct Registration {
    interest: Mutex<Option<PollInterest>>,
    end: PipeEnd,
}

#[derive(Copy, Clone)]
enum PipeEnd {
    Reader,
    Writer,
}

struct PollInterest {
    event: Event,
    poller: Arc<Poller>,
    mode: PollMode,
}

impl Wake for Registration {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let mut guard = self.interest.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(interest) = guard.as_ref() {
            let send_event = match self.end {
                PipeEnd::Reader => interest.event.readable,
                PipeEnd::Writer => interest.event.writable,
            };
            if send_event {
                let _ = interest.poller.post(CompletionPacket::new(interest.event));
                // Clear the event if we're in oneshot mode.
                if matches!(interest.mode, PollMode::Oneshot | PollMode::EdgeOneshot) {
                    *guard = None;
                }
            }
        }
    }
}

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls a blocking reader that runs on its own thread.
pub(crate) struct UnblockedReader {
    interest: Arc<Registration>,
    pipe: Reader,
    first_register: bool,
    _marker: PhantomData<File>,
}

impl UnblockedReader {
    fn new(mut source: File, capacity: usize) -> Self {
        let (reader, mut writer) = pipe(capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::new(None),
            end: PipeEnd::Reader,
        });

        let spawned = thread::Builder::new()
            .name("selachi-conout".into())
            .spawn(move || {
                let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
                let mut context = Context::from_waker(&waker);
                loop {
                    match writer.poll_fill(&mut context, &mut source) {
                        // Either the consumer is gone or the host closed its end (EOF).
                        Poll::Ready(Ok(0)) => break,
                        Poll::Ready(Ok(_)) => {}
                        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                        Poll::Ready(Err(e)) => {
                            log::error!("error reading the pty output: {e}");
                            break;
                        }
                        // The consumer has to make room first.
                        Poll::Pending => thread::park(),
                    }
                }
                // Keep emptying the pipe until the host closes it: `ClosePseudoConsole` blocks
                // while there is unread output. At EOF this returns immediately.
                let mut sink = [0u8; 4096];
                while matches!(source.read(&mut sink), Ok(n) if n > 0) {}
            });
        if let Err(e) = spawned {
            log::error!("could not start the pty reader thread: {e}");
        }
        Self {
            interest,
            pipe: reader,
            first_register: true,
            _marker: PhantomData,
        }
    }

    fn register(&mut self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
        *self
            .interest
            .interest
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(PollInterest {
            event,
            poller: poller.clone(),
            mode,
        });
        // Report data that is already waiting (and once at the very start).
        if (!self.pipe.is_empty() && event.readable) || self.first_register {
            self.first_register = false;
            let _ = poller.post(CompletionPacket::new(event));
        }
    }

    fn deregister(&self) {
        *self
            .interest
            .interest
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl Read for UnblockedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let waker = Waker::from(self.interest.clone());
        match self
            .pipe
            .poll_drain_bytes(&mut Context::from_waker(&waker), buf)
        {
            Poll::Pending => Ok(0),
            Poll::Ready(n) => Ok(n),
        }
    }
}

/// Polls a blocking writer that runs on its own thread.
pub(crate) struct UnblockedWriter {
    interest: Arc<Registration>,
    pipe: Writer,
    _marker: PhantomData<File>,
}

impl UnblockedWriter {
    fn new(mut sink: File, capacity: usize) -> Self {
        let (mut reader, writer) = pipe(capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::new(None),
            end: PipeEnd::Writer,
        });

        let spawned = thread::Builder::new()
            .name("selachi-conin".into())
            .spawn(move || {
                let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
                let mut context = Context::from_waker(&waker);
                loop {
                    match reader.poll_drain(&mut context, &mut sink) {
                        // The producer is gone or the pipe is full.
                        Poll::Ready(Ok(0)) => return,
                        Poll::Ready(Ok(_)) => {}
                        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                        Poll::Ready(Err(e)) => {
                            log::error!("error writing to the pty: {e}");
                            return;
                        }
                        Poll::Pending => thread::park(),
                    }
                }
            });
        if let Err(e) = spawned {
            log::error!("could not start the pty writer thread: {e}");
        }
        Self {
            interest,
            pipe: writer,
            _marker: PhantomData,
        }
    }

    fn register(&self, poller: &Arc<Poller>, event: Event, mode: PollMode) {
        *self
            .interest
            .interest
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(PollInterest {
            event,
            poller: poller.clone(),
            mode,
        });
        // There is room for data right away.
        if !self.pipe.is_full() && event.writable {
            let _ = poller.post(CompletionPacket::new(event));
        }
    }

    fn deregister(&self) {
        *self
            .interest
            .interest
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl Write for UnblockedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let waker = Waker::from(self.interest.clone());
        match self
            .pipe
            .poll_fill_bytes(&mut Context::from_waker(&waker), buf)
        {
            Poll::Pending => Ok(0),
            Poll::Ready(n) => Ok(n),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PTY traits for the loop
// ---------------------------------------------------------------------------

fn with_key(mut event: Event, key: usize) -> Event {
    event.key = key;
    event
}

impl EventedReadWrite for Pty {
    type Reader = UnblockedReader;
    type Writer = UnblockedWriter;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.reregister(poll, interest, mode)
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.conin
            .register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), mode);
        self.conout
            .register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), mode);
        self.child
            .register(poll, with_key(interest, PTY_CHILD_EVENT_TOKEN));
        Ok(())
    }

    fn deregister(&mut self, _poll: &Arc<Poller>) -> io::Result<()> {
        self.conin.deregister();
        self.conout.deregister();
        self.child.deregister();
        Ok(())
    }

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.conout
    }

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.conin
    }
}

impl EventedPty for Pty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        match self.child.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(ChildEvent::Exited(None)),
        }
    }
}

impl OnResize for Pty {
    fn on_resize(&mut self, size: WindowSize) {
        self.backend.on_resize(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lossy(value: &[u16]) -> String {
        OsString::from_wide(value).to_string_lossy().into_owned()
    }

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_string()).collect()
    }

    #[test]
    fn escapes_arguments_like_the_c_runtime() {
        let cases = [
            ("abc", "abc"),
            ("", "\"\""),
            ("ab c", "\"ab c\""),
            ("ab\\c", "ab\\c"),
            ("ab\"c", "ab\\\"c"),
            ("ab \"c", "\"ab \\\"c\""),
            ("C:\\Program Files\\", "\"C:\\Program Files\\\\\""),
        ];
        for (input, expected) in cases {
            let mut out = String::new();
            push_escaped_arg(&mut out, input);
            assert_eq!(out, expected, "for {input:?}");
        }
    }

    #[test]
    fn command_line_quotes_program_with_spaces() {
        assert_eq!(
            command_line("pwsh.exe", &s(&["-NoLogo", "a b"])),
            "pwsh.exe -NoLogo \"a b\""
        );
        assert_eq!(
            command_line("C:\\Program Files\\PowerShell\\7\\pwsh.exe", &s(&["-x"])),
            "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -x"
        );
        assert_eq!(
            command_line("\"C:\\a b\\x.exe\"", &[]),
            "\"C:\\a b\\x.exe\""
        );
    }

    #[test]
    fn environment_block_is_sorted_deduped_and_double_terminated() {
        let custom = [
            ("zeta".to_string(), "1".to_string()),
            ("Term".to_string(), "xterm".to_string()),
            ("TERM".to_string(), "dup".to_string()),
        ];
        let block = environment_block(&custom);
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        let text: Vec<String> = block[..block.len() - 1]
            .split(|c| *c == 0)
            .filter(|e| !e.is_empty())
            .map(lossy)
            .collect();
        assert!(text.contains(&"Term=xterm".to_string()));
        assert!(!text.iter().any(|e| e.starts_with("TERM=")));
        assert!(text.contains(&"zeta=1".to_string()));
        let names: Vec<String> = text
            .iter()
            .map(|e| {
                e.split_at(e.find('=').map_or(0, |i| i.max(1)))
                    .0
                    .to_uppercase()
            })
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "block must be sorted by upper-cased name");
    }

    fn spec_for<'a>(job: Option<isize>, force: bool, args: &'a [String]) -> SpawnSpec<'a> {
        SpawnSpec {
            program: "cmd.exe",
            args,
            cwd: None,
            env: &[],
            job,
            size: WindowSize {
                num_lines: 24,
                num_cols: 80,
                cell_width: 8,
                cell_height: 16,
            },
            force_job_fallback: force,
        }
    }

    /// The suspended + assign + resume path (what runs when `JOB_LIST` is rejected).
    #[test]
    fn job_fallback_assigns_before_the_shell_runs() {
        let job = proc_svc::JobGroup::with_options(
            "conpty-fallback",
            proc_svc::JobOptions {
                kill_on_close: true,
                ..Default::default()
            },
        )
        .unwrap();
        let args = s(&["/k"]);
        let handle = job.raw_handle() as isize;
        let pty = spawn(&spec_for(Some(handle), true, &args)).unwrap();
        assert_eq!(pty.attachment(), JobAttachment::AssignedAfterSpawn);
        assert!(
            job.contains(pty.pid()),
            "{:?} / {}",
            job.members(),
            pty.pid()
        );
        drop(pty);
        job.terminate_raw(1).unwrap();
    }

    #[test]
    fn job_list_puts_the_shell_in_the_job_at_birth() {
        let job = proc_svc::JobGroup::with_options(
            "conpty-birth",
            proc_svc::JobOptions {
                kill_on_close: true,
                ..Default::default()
            },
        )
        .unwrap();
        let args = s(&["/k"]);
        let handle = job.raw_handle() as isize;
        let pty = spawn(&spec_for(Some(handle), false, &args)).unwrap();
        assert_eq!(pty.attachment(), JobAttachment::AtBirth);
        assert!(job.contains(pty.pid()));
        drop(pty);
        job.terminate_raw(1).unwrap();
    }

    #[test]
    fn no_job_means_no_attachment() {
        let args = s(&["/c", "exit"]);
        let pty = spawn(&spec_for(None, false, &args)).unwrap();
        assert_eq!(pty.attachment(), JobAttachment::None);
        assert_ne!(pty.pid(), 0);
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let mut spec = spec_for(None, false, &[]);
        spec.program = "definitely-not-a-real-program-selachi.exe";
        assert!(spawn(&spec).is_err());
    }
}
