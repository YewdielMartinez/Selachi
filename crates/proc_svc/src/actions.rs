//! Safe termination of processes, trees and jobs.
//!
//! Order of operations for [`terminate_tree`]:
//! 1. Snapshot, collect the tree (descendants validated by start time, so a
//!    reused pid is never mistaken for a child), children first.
//! 2. Open a handle to **every** process of the tree and run the safety checks
//!    on the handles. If any process is refused, nothing is touched. Holding
//!    handles also prevents pid reuse between check and kill.
//! 3. Graceful phase (only if something was actually asked to close): post
//!    `WM_CLOSE` to the visible top-level windows of the tree and, if safe,
//!    send `CTRL_BREAK` to the root's console. Wait up to `grace`.
//! 4. `TerminateProcess` on whatever is still alive, children first
//!    (descendants spawned during the grace period are included).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HWND, LPARAM};
use windows::Win32::System::Console::{
    AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleProcessList,
    SetConsoleCtrlHandler,
};
use windows::Win32::System::Threading::{
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};
use windows::core::{BOOL, HRESULT};

use crate::error::{Error, RefuseReason, Result, win32};
use crate::job::JobGroup;
use crate::snapshot::{ProcInfo, SnapshotOptions, snapshot_with};
use crate::sys::{self, OwnedHandle};

/// Image names that are never terminated (lower case, no `.exe`).
const CRITICAL: &[&str] = &[
    "system",
    "registry",
    "smss",
    "csrss",
    "wininit",
    "winlogon",
    "services",
    "lsass",
    "lsm",
    "svchost",
    "dwm",
    "fontdrvhost",
    "memory compression",
    "secure system",
    "system idle process",
];

/// What the safety layer may allow beyond the defaults.
#[derive(Debug, Clone, Copy, Default)]
pub struct SafetyPolicy {
    /// Allow terminating `explorer.exe` (the shell). Default `false`.
    pub allow_explorer: bool,
}

/// Options for termination.
#[derive(Debug, Clone, Copy)]
pub struct TerminateOptions {
    /// How long to wait after the polite request before forcing. `ZERO` skips
    /// the graceful phase entirely.
    pub grace: Duration,
    /// Safety policy.
    pub safety: SafetyPolicy,
    /// Post `WM_CLOSE` to visible top-level windows of the tree.
    pub window_close: bool,
    /// Send `CTRL_BREAK` to the root's console group when it is safe to do so
    /// (we own no console ourselves; see [`console_break`] caveats).
    pub console_break: bool,
}

impl Default for TerminateOptions {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(3),
            safety: SafetyPolicy::default(),
            window_close: true,
            console_break: true,
        }
    }
}

/// What happened to each process of a termination request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminateOutcome {
    /// Every pid that was targeted (children first).
    pub targeted: Vec<u32>,
    /// Exited during the grace period.
    pub exited_gracefully: Vec<u32>,
    /// Had to be force-terminated.
    pub killed: Vec<u32>,
    /// Could not be terminated, with the reason.
    pub failed: Vec<(u32, String)>,
}

impl TerminateOutcome {
    /// True when every targeted process is gone.
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Checks whether `pid` may be terminated under `policy` without killing it.
pub fn check_safe(pid: u32, policy: &SafetyPolicy) -> Result<()> {
    open_target(pid, policy).map(|_| ())
}

/// Terminates `pid` and all its descendants: graceful first, then force after
/// `grace`. Uses the default [`TerminateOptions`] otherwise.
pub fn terminate_tree(pid: u32, grace: Duration) -> Result<TerminateOutcome> {
    terminate_tree_with(pid, &TerminateOptions { grace, ..TerminateOptions::default() })
}

/// [`terminate_tree`] with full options.
pub fn terminate_tree_with(pid: u32, opts: &TerminateOptions) -> Result<TerminateOutcome> {
    let procs = snapshot_with(SnapshotOptions { cmdline: false });
    let order = collect_tree(pid, &procs);
    let mut outcome = TerminateOutcome::default();

    // Open + check everything before touching anything.
    let mut targets: Vec<Target> = Vec::new();
    for p in &order {
        match open_target(*p, &opts.safety) {
            Ok(t) => targets.push(t),
            Err(e @ Error::Refused { .. }) => return Err(e),
            Err(e) => {
                if *p == pid {
                    log::debug!("terminate_tree: root {pid} not openable: {e}");
                }
            }
        }
    }
    if targets.is_empty() {
        return Ok(outcome);
    }
    outcome.targeted = targets.iter().map(|t| t.pid).collect();

    // Graceful phase.
    let mut asked = false;
    if !opts.grace.is_zero() {
        let pids: HashSet<u32> = targets.iter().map(|t| t.pid).collect();
        if opts.window_close {
            asked |= post_close(&pids) > 0;
        }
        if opts.console_break && targets.iter().any(|t| t.pid == pid) {
            asked |= console_break(pid);
        }
        if asked {
            wait_all(&targets, opts.grace);
        }
    }
    for t in &targets {
        if !sys::is_alive(&t.handle) {
            outcome.exited_gracefully.push(t.pid);
        }
    }

    // Include descendants spawned meanwhile by survivors, then force.
    let survivors: Vec<u32> =
        targets.iter().filter(|t| sys::is_alive(&t.handle)).map(|t| t.pid).collect();
    if !survivors.is_empty() {
        let known: HashSet<u32> = targets.iter().map(|t| t.pid).collect();
        let procs = snapshot_with(SnapshotOptions { cmdline: false });
        let mut extra: Vec<Target> = Vec::new();
        for s in &survivors {
            for p in collect_tree(*s, &procs) {
                if !known.contains(&p) && !extra.iter().any(|t| t.pid == p) {
                    match open_target(p, &opts.safety) {
                        Ok(t) => extra.push(t),
                        Err(e) => log::warn!("skipping new descendant {p}: {e}"),
                    }
                }
            }
        }
        outcome.targeted.extend(extra.iter().map(|t| t.pid));
        // Newest descendants first, then the original order (children first).
        let mut all = extra;
        all.extend(targets);
        force_kill(&all, &mut outcome);
    }
    Ok(outcome)
}

/// Terminates every process of `job` (hard `TerminateJobObject`), after the
/// same safety checks as [`terminate_tree`] and without a graceful phase.
pub fn terminate_job(job: &JobGroup) -> Result<TerminateOutcome> {
    terminate_job_with(job, &TerminateOptions { grace: Duration::ZERO, ..Default::default() })
}

/// [`terminate_job`] with options; a non-zero `grace` first posts `WM_CLOSE`
/// to the members' windows and waits.
pub fn terminate_job_with(job: &JobGroup, opts: &TerminateOptions) -> Result<TerminateOutcome> {
    let members = job.try_members()?;
    let mut targets = Vec::new();
    for pid in members {
        match open_target(pid, &opts.safety) {
            Ok(t) => targets.push(t),
            Err(e @ Error::Refused { .. }) => return Err(e),
            Err(_) => {} // already gone
        }
    }
    let mut outcome = TerminateOutcome {
        targeted: targets.iter().map(|t| t.pid).collect(),
        ..Default::default()
    };
    if targets.is_empty() {
        return Ok(outcome);
    }
    if !opts.grace.is_zero() && opts.window_close {
        let pids: HashSet<u32> = targets.iter().map(|t| t.pid).collect();
        if post_close(&pids) > 0 {
            wait_all(&targets, opts.grace);
        }
    }
    for t in &targets {
        if !sys::is_alive(&t.handle) {
            outcome.exited_gracefully.push(t.pid);
        }
    }
    if targets.iter().any(|t| sys::is_alive(&t.handle)) {
        job.terminate_raw(1)?;
        for t in &targets {
            if sys::is_alive(&t.handle) {
                if sys::wait_exit(&t.handle, Duration::from_secs(2)) {
                    outcome.killed.push(t.pid);
                } else {
                    outcome.failed.push((t.pid, "did not exit after TerminateJobObject".into()));
                }
            }
        }
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// tree collection (pure, testable)
// ---------------------------------------------------------------------------

/// Returns `root` and its descendants in kill order (children before parents).
/// A process counts as a child only if its `ppid` matches **and** it started
/// strictly after the parent (guards against reused parent pids). Returns an
/// empty vec when `root` is not in `procs`.
pub fn collect_tree(root: u32, procs: &[ProcInfo]) -> Vec<u32> {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    if !by_pid.contains_key(&root) {
        return Vec::new();
    }
    let mut children: HashMap<u32, Vec<&ProcInfo>> = HashMap::new();
    for p in procs {
        if p.pid != p.ppid {
            children.entry(p.ppid).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    visit(root, &by_pid, &children, &mut seen, &mut out);
    out
}

fn visit(
    pid: u32,
    by_pid: &HashMap<u32, &ProcInfo>,
    children: &HashMap<u32, Vec<&ProcInfo>>,
    seen: &mut HashSet<u32>,
    out: &mut Vec<u32>,
) {
    if !seen.insert(pid) {
        return;
    }
    let parent_start = by_pid.get(&pid).and_then(|p| p.start_time);
    if let Some(kids) = children.get(&pid) {
        for k in kids {
            let valid = matches!((parent_start, k.start_time), (Some(ps), Some(ks)) if ks > ps);
            if valid {
                visit(k.pid, by_pid, children, seen, out);
            }
        }
    }
    out.push(pid);
}

// ---------------------------------------------------------------------------
// safety + handles
// ---------------------------------------------------------------------------

struct Target {
    pid: u32,
    handle: OwnedHandle,
}

fn open_target(pid: u32, policy: &SafetyPolicy) -> Result<Target> {
    if pid == 0 || pid == 4 {
        return Err(Error::Refused { pid, reason: RefuseReason::SystemPid });
    }
    if pid == std::process::id() {
        return Err(Error::Refused { pid, reason: RefuseReason::SelfProcess });
    }
    let handle = sys::open_process(
        pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
    )
    .map_err(|e| {
        if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
            Error::Refused { pid, reason: RefuseReason::Unverifiable }
        } else {
            win32("OpenProcess")(e)
        }
    })?;
    check_handle(pid, &handle, policy)?;
    Ok(Target { pid, handle })
}

fn check_handle(pid: u32, h: &OwnedHandle, policy: &SafetyPolicy) -> Result<()> {
    let refuse = |reason| Err(Error::Refused { pid, reason });
    let Some(path) = sys::image_path(h) else {
        return refuse(RefuseReason::Unverifiable);
    };
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_ascii_lowercase();
    let stem = file.strip_suffix(".exe").unwrap_or(&file);
    if stem == "explorer" {
        if !policy.allow_explorer {
            return refuse(RefuseReason::Explorer);
        }
    } else if CRITICAL.contains(&stem) {
        return refuse(RefuseReason::CriticalProcess(file));
    }

    let (Some(ours), Some(theirs)) = (sys::own_session_id(), sys::session_id(pid)) else {
        return refuse(RefuseReason::Unverifiable);
    };
    if ours != theirs {
        return refuse(RefuseReason::OtherSession { target: theirs, ours });
    }
    match (sys::own_user_sid(), sys::user_sid(h)) {
        (Some(a), Some(b)) if *a == b => Ok(()),
        (Some(_), Some(_)) => refuse(RefuseReason::OtherUser),
        _ => refuse(RefuseReason::Unverifiable),
    }
}

fn wait_all(targets: &[Target], grace: Duration) {
    let deadline = Instant::now() + grace;
    loop {
        if targets.iter().all(|t| !sys::is_alive(&t.handle)) || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn force_kill(all: &[Target], outcome: &mut TerminateOutcome) {
    let mut killed = Vec::new();
    for t in all {
        if !sys::is_alive(&t.handle) {
            continue;
        }
        // SAFETY: valid handle with PROCESS_TERMINATE.
        match unsafe { TerminateProcess(t.handle.raw(), 1) } {
            Ok(()) => killed.push(t),
            Err(e) => {
                if sys::is_alive(&t.handle) {
                    outcome.failed.push((t.pid, format!("TerminateProcess: {e}")));
                }
            }
        }
    }
    for t in killed {
        if sys::wait_exit(&t.handle, Duration::from_secs(2)) {
            outcome.killed.push(t.pid);
        } else {
            outcome.failed.push((t.pid, "still running after TerminateProcess".into()));
        }
    }
}

// ---------------------------------------------------------------------------
// graceful requests
// ---------------------------------------------------------------------------

struct EnumCtx<'a> {
    pids: &'a HashSet<u32>,
    posted: usize,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the address of the live `EnumCtx` passed by `post_close`.
    let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx<'_>) };
    let mut pid = 0u32;
    // SAFETY: valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // SAFETY: plain query on a window handle that may vanish; failure is benign.
    if ctx.pids.contains(&pid) && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        // SAFETY: posting a message is safe for any handle value.
        if unsafe { PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default()) }.is_ok() {
            ctx.posted += 1;
        }
    }
    BOOL(1)
}

/// Posts `WM_CLOSE` to the visible top-level windows owned by `pids`.
fn post_close(pids: &HashSet<u32>) -> usize {
    let mut ctx = EnumCtx { pids, posted: 0 };
    // SAFETY: `ctx` outlives the synchronous EnumWindows call.
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(std::ptr::from_mut(&mut ctx) as isize)) };
    ctx.posted
}

static CONSOLE_LOCK: Mutex<()> = Mutex::new(());

/// Sends `CTRL_BREAK` to the console process group whose id is `pid`.
///
/// Only effective for processes created with `CREATE_NEW_PROCESS_GROUP`; for
/// others it is a harmless no-op (and the caller falls back to force after the
/// grace period). It is attempted **only if we own no console ourselves**
/// (a GUI-subsystem Selachi never does); otherwise `AttachConsole` would
/// detach our own console, so it is skipped and `false` is returned. Process
/// wide console state is serialised with a mutex. Returns true if an event
/// was generated.
pub fn console_break(pid: u32) -> bool {
    let _g = CONSOLE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut probe = [0u32; 1];
    // SAFETY: valid buffer of 1 element. 0 means we have no console.
    if unsafe { GetConsoleProcessList(&mut probe) } != 0 {
        return false;
    }
    // SAFETY: we have no console, so attaching cannot steal ours.
    if unsafe { AttachConsole(pid) }.is_err() {
        return false;
    }
    // SAFETY: ignore Ctrl events in this process while attached, then restore.
    unsafe {
        let _ = SetConsoleCtrlHandler(None, true);
        let r = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid).is_ok();
        let _ = FreeConsole();
        let _ = SetConsoleCtrlHandler(None, false);
        r
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    fn p(pid: u32, ppid: u32, start: Option<u64>) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            name: format!("p{pid}.exe"),
            exe_path: None,
            cmdline: None,
            start_time: start.map(|s| UNIX_EPOCH + Duration::from_secs(s)),
            working_set: 0,
            private_bytes: 0,
            cpu_time: Duration::ZERO,
            threads: 1,
            access_denied: false,
        }
    }

    #[test]
    fn tree_is_children_first_and_validates_start_time() {
        let procs = [
            p(10, 1, Some(100)),
            p(11, 10, Some(110)),
            p(12, 11, Some(120)),
            p(13, 10, Some(130)),
            // claims parent 10 but started BEFORE it: pid reuse, must be ignored
            p(14, 10, Some(50)),
            // unknown start: ignored
            p(15, 10, None),
        ];
        let t = collect_tree(10, &procs);
        assert_eq!(*t.last().unwrap(), 10);
        let pos = |x| t.iter().position(|&p| p == x).unwrap();
        assert!(pos(12) < pos(11) && pos(11) < pos(10) && pos(13) < pos(10));
        assert!(!t.contains(&14) && !t.contains(&15));
        assert_eq!(t.len(), 4);
        assert!(collect_tree(999, &procs).is_empty());
        let _ = SystemTime::now();
    }

    #[test]
    fn tree_survives_cycles() {
        let procs = [p(1, 2, Some(10)), p(2, 1, Some(20))];
        let t = collect_tree(1, &procs);
        assert!(t.contains(&1));
    }

    #[test]
    fn refuses_system_and_self() {
        let pol = SafetyPolicy::default();
        for pid in [0u32, 4] {
            assert!(matches!(
                check_safe(pid, &pol),
                Err(Error::Refused { reason: RefuseReason::SystemPid, .. })
            ));
        }
        assert!(matches!(
            check_safe(std::process::id(), &pol),
            Err(Error::Refused { reason: RefuseReason::SelfProcess, .. })
        ));
    }
}
