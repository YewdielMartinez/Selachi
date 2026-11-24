//! One Selachi per user. The first instance holds a named mutex and listens on a named pipe; a
//! second launch (the Explorer entry "Abrir en Selachi", a shortcut) hands its folder to the first
//! one over that pipe and exits, and the first one opens a tab there and comes to the front.
//!
//! No idle cost: the listener is a thread blocked in `ConnectNamedPipe` (it wakes only when
//! someone connects) and its messages reach the UI through a channel awaited by a task, which is
//! a waker, not a timer.
//!
//! Both names carry the user name: the mutex lives in the session's `Local\` namespace, and the
//! pipe's default security lets other users read but not write it.

use std::path::PathBuf;

use futures::channel::mpsc::UnboundedSender;

/// What another launch asked of this instance.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// Open a tab in this folder (`None`: just come to the front).
    Open(Option<PathBuf>),
}

/// Result of [`acquire`].
pub enum Instance {
    /// This process is the (only) instance; keep the guard alive.
    Primary(Guard),
    /// Another instance got the request: exit.
    Forwarded,
}

/// Keeps the single-instance mutex for the life of the process.
pub struct Guard {
    #[cfg(windows)]
    _mutex: isize,
}

/// A user name made of characters that are safe in a kernel object name.
pub fn user_tag() -> String {
    let raw = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    let tag: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if tag.is_empty() { "user".into() } else { tag }
}

/// The wire format: one line, `open <path>` or `focus`.
pub fn encode(req: &Request) -> String {
    match req {
        Request::Open(Some(p)) => format!("open {}\n", p.display()),
        Request::Open(None) => "focus\n".to_string(),
    }
}

/// Parses a line of [`encode`]. Anything else is `None`: the pipe is not a command channel.
pub fn decode(line: &str) -> Option<Request> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line == "focus" {
        return Some(Request::Open(None));
    }
    let path = line.strip_prefix("open ")?;
    let p = PathBuf::from(path);
    p.is_dir().then_some(Request::Open(Some(p)))
}

#[cfg(windows)]
mod imp {
    use std::io::Write as _;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, ReadFile};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    use super::*;

    /// Longest message the pipe accepts (a path).
    const MAX_MSG: usize = 4096;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn pipe_name() -> String {
        format!(r"\\.\pipe\Selachi.{}", user_tag())
    }

    pub fn acquire(forward: &Request) -> Instance {
        let name = wide(&format!(r"Local\Selachi.SingleInstance.{}", user_tag()));
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if mutex.is_null() {
            // No mutex, no single-instance guarantee: start normally rather than not at all.
            return Instance::Primary(Guard { _mutex: 0 });
        }
        // SAFETY: reads the calling thread's last error.
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if !exists {
            return Instance::Primary(Guard {
                _mutex: mutex as isize,
            });
        }
        // SAFETY: the handle is ours and unused from here on.
        unsafe { CloseHandle(mutex) };
        // The first instance may still be creating its pipe: retry for ~2 s.
        for _ in 0..20 {
            if let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(pipe_name())
                && pipe.write_all(encode(forward).as_bytes()).is_ok()
            {
                return Instance::Forwarded;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Unreachable first instance (hung?): this one runs too.
        Instance::Primary(Guard { _mutex: 0 })
    }

    pub fn listen(tx: UnboundedSender<Request>) {
        let name = wide(&pipe_name());
        let spawned = std::thread::Builder::new()
            .name("selachi-single-instance".into())
            .spawn(move || {
                loop {
                    // SAFETY: `name` is NUL-terminated and outlives the call; null security
                    // attributes mean the default descriptor.
                    let pipe = unsafe {
                        CreateNamedPipeW(
                            name.as_ptr(),
                            PIPE_ACCESS_INBOUND,
                            PIPE_TYPE_BYTE
                                | PIPE_READMODE_BYTE
                                | PIPE_WAIT
                                | PIPE_REJECT_REMOTE_CLIENTS,
                            PIPE_UNLIMITED_INSTANCES,
                            0,
                            MAX_MSG as u32,
                            0,
                            std::ptr::null(),
                        )
                    };
                    if pipe == INVALID_HANDLE_VALUE {
                        return;
                    }
                    // Blocks until a client connects (or one already did).
                    // SAFETY: `pipe` is a valid pipe handle; no OVERLAPPED (blocking mode).
                    let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0
                        || unsafe { GetLastError() }
                            == windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED;
                    if connected {
                        let mut buf = vec![0u8; MAX_MSG];
                        let mut len = 0usize;
                        loop {
                            let mut read = 0u32;
                            // SAFETY: the buffer slice is valid for `MAX_MSG - len` bytes.
                            let ok = unsafe {
                                ReadFile(
                                    pipe,
                                    buf.as_mut_ptr().add(len),
                                    (MAX_MSG - len) as u32,
                                    &mut read,
                                    std::ptr::null_mut(),
                                )
                            };
                            if ok == 0 || read == 0 {
                                break;
                            }
                            len += read as usize;
                            if len >= MAX_MSG {
                                break;
                            }
                        }
                        let text = String::from_utf8_lossy(&buf[..len]);
                        if let Some(req) = text.lines().next().and_then(decode)
                            && tx.unbounded_send(req).is_err()
                        {
                            // SAFETY: `pipe` is ours.
                            unsafe { CloseHandle(pipe) };
                            return;
                        }
                    }
                    // SAFETY: `pipe` is ours; it is closed once and not used afterwards.
                    unsafe {
                        DisconnectNamedPipe(pipe);
                        CloseHandle(pipe);
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("single-instance listener not started: {e}");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn acquire(_forward: &Request) -> Instance {
        Instance::Primary(Guard {})
    }

    pub fn listen(_tx: UnboundedSender<Request>) {}
}

/// Takes the single-instance mutex, or hands `forward` to the instance that has it.
pub fn acquire(forward: &Request) -> Instance {
    imp::acquire(forward)
}

/// Starts listening for the requests of later launches.
pub fn listen(tx: UnboundedSender<Request>) {
    imp::listen(tx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_format_round_trips() {
        let dir = std::env::temp_dir();
        let req = Request::Open(Some(dir));
        assert_eq!(decode(&encode(&req)), Some(req));
        assert_eq!(
            decode(&encode(&Request::Open(None))),
            Some(Request::Open(None))
        );
    }

    #[test]
    fn garbage_and_missing_folders_are_not_requests() {
        assert_eq!(decode("rm -rf /"), None);
        assert_eq!(decode("open C:\\definitely\\not\\here\\selachi"), None);
        assert_eq!(decode(""), None);
    }

    #[test]
    fn the_user_tag_is_safe_in_an_object_name() {
        let tag = user_tag();
        assert!(!tag.is_empty());
        assert!(tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
}
