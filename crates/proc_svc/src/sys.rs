//! Small internal Win32 helpers: owned handles, FILETIME conversion, token/session queries.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Security::{
    GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW,
    WaitForSingleObject,
};
use windows::core::PWSTR;

/// A Win32 handle closed on drop. Kernel handles are thread-safe.
#[derive(Debug)]
pub(crate) struct OwnedHandle(HANDLE);

// SAFETY: kernel object handles may be used and closed from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: see above; all uses are through thread-safe kernel calls.
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    pub(crate) fn new(h: HANDLE) -> Self {
        Self(h)
    }
    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(crate) fn open_process(
    pid: u32,
    access: PROCESS_ACCESS_RIGHTS,
) -> windows::core::Result<OwnedHandle> {
    // SAFETY: plain FFI call; the returned handle is wrapped immediately.
    unsafe { OpenProcess(access, false, pid).map(OwnedHandle::new) }
}

/// Handle good enough to query info and to wait for exit.
pub(crate) fn open_process_for_wait(pid: u32) -> windows::core::Result<OwnedHandle> {
    open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)
}

/// True while the process behind `h` (opened with SYNCHRONIZE) has not exited.
pub(crate) fn is_alive(h: &OwnedHandle) -> bool {
    // SAFETY: valid handle owned by `h`.
    unsafe { WaitForSingleObject(h.raw(), 0) == WAIT_TIMEOUT }
}

/// Waits up to `timeout` for exit; returns true when it exited.
pub(crate) fn wait_exit(h: &OwnedHandle, timeout: Duration) -> bool {
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: valid handle owned by `h`.
    unsafe { WaitForSingleObject(h.raw(), ms) != WAIT_TIMEOUT }
}

pub(crate) fn filetime_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// FILETIME ticks (100 ns since 1601) to `SystemTime`.
pub(crate) fn filetime_to_system_time(ticks: u64) -> Option<SystemTime> {
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    let since_unix = ticks.checked_sub(EPOCH_DIFF_100NS)?;
    Some(UNIX_EPOCH + Duration::from_nanos(since_unix.saturating_mul(100)))
}

/// Full Win32 image path of a process, if readable.
pub(crate) fn image_path(h: &OwnedHandle) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is valid for `len` u16s; `len` is updated by the call.
    let ok = unsafe {
        QueryFullProcessImageNameW(h.raw(), PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    };
    ok.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

pub(crate) fn session_id(pid: u32) -> Option<u32> {
    let mut s = 0u32;
    // SAFETY: `s` is a valid out pointer.
    unsafe { ProcessIdToSessionId(pid, &mut s).ok()? };
    Some(s)
}

/// SID bytes of the user owning the process behind `h`.
pub(crate) fn user_sid(h: &OwnedHandle) -> Option<Vec<u8>> {
    sid_of_process(h.raw())
}

fn sid_of_process(process: HANDLE) -> Option<Vec<u8>> {
    let mut token = HANDLE::default();
    // SAFETY: `token` is a valid out pointer; closed by OwnedHandle below.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()? };
    let token = OwnedHandle::new(token);
    let mut needed = 0u32;
    // SAFETY: size probe call with a null buffer.
    unsafe {
        let _ = GetTokenInformation(token.raw(), TokenUser, None, 0, &mut needed);
    }
    if needed == 0 {
        return None;
    }
    // u64 elements guarantee 8-byte alignment for TOKEN_USER.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` has at least `needed` bytes; TOKEN_USER is read within it.
    unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .ok()?;
        let tu = &*(buf.as_ptr().cast::<TOKEN_USER>());
        let sid = tu.User.Sid;
        if sid.0.is_null() {
            return None;
        }
        let len = GetLengthSid(sid) as usize;
        Some(std::slice::from_raw_parts(sid.0.cast::<u8>(), len).to_vec())
    }
}

/// SID bytes of the current process' user (cached).
pub(crate) fn own_user_sid() -> Option<&'static Vec<u8>> {
    use std::sync::OnceLock;
    static SID: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    SID.get_or_init(|| {
        // SAFETY: pseudo-handle, always valid.
        let me = unsafe { GetCurrentProcess() };
        sid_of_process(me)
    })
    .as_ref()
}

/// Session id of the current process (cached).
pub(crate) fn own_session_id() -> Option<u32> {
    use std::sync::OnceLock;
    static S: OnceLock<Option<u32>> = OnceLock::new();
    *S.get_or_init(|| session_id(std::process::id()))
}
