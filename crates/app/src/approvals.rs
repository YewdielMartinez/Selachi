//! The approvals server: the GUI side of `agent_harness::approvals` (protocol v1).
//!
//! A delegated agent never approves itself: `selachi-agent` connects to the named pipe
//! `\\.\pipe\selachi-approvals-<user>`, writes one JSON line (`ping` or `approval_request`), and
//! waits for one JSON line back. This module is the server:
//!
//! * One thread blocks in `ConnectNamedPipe`: **zero cost while idle** (no timer, no polling).
//! * Each connection is served by its own short-lived thread: it reads the request (10 s to send
//!   it, 64 KB cap), hands it to the GUI through a channel ([`ServerEvent::Request`]) and waits for
//!   the user's answer for at most `timeout_secs` of the request; no answer is a **denial**.
//! * The pipe is created with `FILE_FLAG_FIRST_PIPE_INSTANCE` (nobody can squat the name before
//!   us), remote clients refused, and a protected DACL for the owner and SYSTEM only (another local
//!   user cannot fill the queue with fake requests).
//! * At most [`MAX_CONNECTIONS`] connections are served at once; the rest get an `error`.
//!
//! The GUI side lives in `approval_queue.rs` (attention items, cards, toasts).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use agent_harness::{ApprovalRequest, ClientMessage, Decision, PROTOCOL_VERSION, ServerMessage};
use futures::channel::mpsc::UnboundedSender;

/// Largest request line accepted.
pub const MAX_LINE_BYTES: usize = 64 * 1024;
/// How long a client has to send its request after connecting.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at the same time.
pub const MAX_CONNECTIONS: usize = 16;
/// Smallest / largest time the user is given, whatever the client asks for.
pub const MIN_WAIT_SECS: u64 = 5;
pub const MAX_WAIT_SECS: u64 = 3600;

/// What the user (or the timeout) decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub decision: Decision,
    /// Why, for a denial the user did not choose (shown to the agent).
    pub reason: Option<String>,
}

/// One request waiting for the user. Answering consumes it; dropping it unanswered denies.
#[derive(Debug)]
pub struct Incoming {
    pub request: ApprovalRequest,
    reply: Sender<Answer>,
}

impl Incoming {
    /// Sends the answer to the waiting connection (it is written back to the agent).
    pub fn answer(self, decision: Decision, reason: Option<String>) {
        let _ = self.reply.send(Answer { decision, reason });
    }
}

/// What the server tells the GUI.
#[derive(Debug)]
pub enum ServerEvent {
    Request(Incoming),
    /// The wait ended without an answer (the connection already got its denial): drop the card.
    Expired(String),
}

/// A request that is acceptable to show: right version, sane sizes.
fn validate(req: &ApprovalRequest) -> Result<(), String> {
    if req.v != PROTOCOL_VERSION {
        return Err(format!(
            "protocol version {} is not supported (this Selachi speaks {PROTOCOL_VERSION})",
            req.v
        ));
    }
    if req.request_id.is_empty() || req.request_id.len() > 128 {
        return Err("request_id must be 1-128 characters".into());
    }
    if req.agent.len() > 64 || req.tool.len() > 256 || req.detail.len() > 16 * 1024 {
        return Err("request fields are too long".into());
    }
    Ok(())
}

/// Wait granted to the user for a request.
pub fn wait_for(req: &ApprovalRequest) -> Duration {
    Duration::from_secs(req.timeout_secs.clamp(MIN_WAIT_SECS, MAX_WAIT_SECS))
}

/// Handles one decoded client message: the reply line to write, and (for a request) the GUI
/// round trip. Pure over the channel, so it is tested without a pipe.
pub fn handle_message(
    msg: ClientMessage,
    events: &UnboundedSender<ServerEvent>,
    wait: impl Fn(&ApprovalRequest) -> Duration,
) -> ServerMessage {
    match msg {
        ClientMessage::Ping { .. } => ServerMessage::Pong {
            v: PROTOCOL_VERSION,
            accepts_approvals: true,
        },
        ClientMessage::ApprovalRequest(req) => {
            if let Err(reason) = validate(&req) {
                return ServerMessage::Error {
                    v: PROTOCOL_VERSION,
                    reason,
                };
            }
            let (tx, rx): (Sender<Answer>, Receiver<Answer>) = channel();
            let id = req.request_id.clone();
            let limit = wait(&req);
            let incoming = Incoming {
                request: req,
                reply: tx,
            };
            if events
                .unbounded_send(ServerEvent::Request(incoming))
                .is_err()
            {
                return ServerMessage::Error {
                    v: PROTOCOL_VERSION,
                    reason: "Selachi is closing".into(),
                };
            }
            let answer = match rx.recv_timeout(limit) {
                Ok(a) => a,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let _ = events.unbounded_send(ServerEvent::Expired(id.clone()));
                    Answer {
                        decision: Decision::Deny,
                        reason: Some(format!(
                            "the user did not answer within {} s",
                            limit.as_secs()
                        )),
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Answer {
                    decision: Decision::Deny,
                    reason: Some("Selachi dropped the request without an answer".into()),
                },
            };
            ServerMessage::ApprovalResponse {
                v: PROTOCOL_VERSION,
                request_id: id,
                decision: answer.decision,
                reason: answer.reason.filter(|_| answer.decision == Decision::Deny),
            }
        }
    }
}

#[cfg(windows)]
pub use pipe::start;

#[cfg(windows)]
mod pipe {
    use std::fs::File;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::IO::CancelIoEx;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    use super::*;

    /// Owner and SYSTEM only, no inheritance from the default DACL.
    const SDDL: &str = "D:P(A;;GA;;;OW)(A;;GA;;;SY)";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// One pipe instance with the protective DACL. `first` adds `FILE_FLAG_FIRST_PIPE_INSTANCE`.
    fn create_instance(path: &[u16], first: bool) -> std::io::Result<HANDLE> {
        let sddl = wide(SDDL);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor we free below.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // Fail closed: without the DACL the default one lets every local user connect.
            return Err(std::io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let open_mode = PIPE_ACCESS_DUPLEX
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: `path` is NUL-terminated, `attrs` outlives the call.
        let handle = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &attrs,
            )
        };
        let err = std::io::Error::last_os_error();
        // SAFETY: `sd` came from the call above and is not used after this.
        unsafe { LocalFree(sd) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(err);
        }
        Ok(handle)
    }

    /// Starts serving `path` (`\\.\pipe\selachi-approvals-<user>`). The first instance is created
    /// here so a failure (the name is taken, the DACL could not be built) is reported to the
    /// caller; the accept loop runs on its own thread.
    pub fn start(path: &str, events: UnboundedSender<ServerEvent>) -> std::io::Result<()> {
        let wpath = wide(path);
        let first = create_instance(&wpath, true)? as usize;
        let active = Arc::new(AtomicUsize::new(0));
        let spawned = std::thread::Builder::new()
            .name("selachi-approvals".into())
            .spawn(move || {
                let mut instance = first as HANDLE;
                loop {
                    // Blocks until a client connects: nothing runs while nobody asks.
                    // SAFETY: `instance` is a valid pipe handle we own.
                    let connected = unsafe { ConnectNamedPipe(instance, std::ptr::null_mut()) } != 0
                        // SAFETY: reads the thread's last error.
                        || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
                    let client = instance;
                    // The next instance first: a client arriving meanwhile finds the pipe open.
                    instance = match create_instance(&wpath, false) {
                        Ok(h) => h,
                        Err(e) => {
                            log::warn!("approvals pipe: cannot create the next instance: {e}");
                            // SAFETY: closing handles we own.
                            unsafe { CloseHandle(client) };
                            return;
                        }
                    };
                    if !connected {
                        // SAFETY: as above.
                        unsafe { CloseHandle(client) };
                        continue;
                    }
                    if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        active.fetch_sub(1, Ordering::SeqCst);
                        refuse(client, "too many pending approvals");
                        continue;
                    }
                    let (ev, act) = (events.clone(), Arc::clone(&active));
                    let handle_bits = client as usize;
                    let spawned = std::thread::Builder::new()
                        .name("selachi-approval-conn".into())
                        .spawn(move || {
                            serve(handle_bits as HANDLE, &ev);
                            act.fetch_sub(1, Ordering::SeqCst);
                        });
                    if spawned.is_err() {
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })?;
        drop(spawned);
        Ok(())
    }

    fn write_line(file: &mut File, msg: &ServerMessage) {
        if let Ok(mut line) = serde_json::to_string(msg) {
            line.push('\n');
            // `sync_all` is FlushFileBuffers: it returns once the client has read everything, so the
            // `DisconnectNamedPipe` that follows cannot throw the reply away.
            let _ = file
                .write_all(line.as_bytes())
                .and_then(|()| file.sync_all());
        }
    }

    fn refuse(client: HANDLE, reason: &str) {
        // SAFETY: `client` is a connected pipe handle we own; `File` closes it on drop.
        let mut file = unsafe { File::from_raw_handle(client) };
        write_line(
            &mut file,
            &ServerMessage::Error {
                v: PROTOCOL_VERSION,
                reason: reason.into(),
            },
        );
        // SAFETY: flushes before the client sees the disconnect.
        unsafe { DisconnectNamedPipe(client) };
    }

    /// Reads the client's line with a 10 s watchdog (a synchronous read cannot time out by itself:
    /// the watchdog cancels it).
    fn read_request(file: &File) -> Result<String, String> {
        let done = Arc::new(AtomicBool::new(false));
        let (stop_tx, stop_rx) = channel::<()>();
        let raw = file.as_raw_handle() as usize;
        let flag = Arc::clone(&done);
        let dog = std::thread::Builder::new()
            .name("selachi-approval-dog".into())
            .spawn(move || {
                if stop_rx.recv_timeout(READ_TIMEOUT).is_err() && !flag.load(Ordering::SeqCst) {
                    // SAFETY: cancels the pending read on a handle that is still open (the main
                    // thread waits for this watchdog before closing it).
                    unsafe { CancelIoEx(raw as HANDLE, std::ptr::null()) };
                }
            })
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        let res = BufReader::new(file.take(MAX_LINE_BYTES as u64 + 1)).read_line(&mut line);
        done.store(true, Ordering::SeqCst);
        let _ = stop_tx.send(());
        let _ = dog.join();
        match res {
            Ok(0) => Err("the client closed the connection without sending a request".into()),
            Ok(n) if n > MAX_LINE_BYTES => Err("the request is larger than 64 KB".into()),
            Ok(_) => Ok(line),
            Err(e) => Err(format!("could not read the request: {e}")),
        }
    }

    fn serve(client: HANDLE, events: &UnboundedSender<ServerEvent>) {
        // SAFETY: `client` is a connected pipe handle owned by this thread; `File` closes it.
        let mut file = unsafe { File::from_raw_handle(client) };
        let reply = match read_request(&file) {
            Err(reason) => ServerMessage::Error {
                v: PROTOCOL_VERSION,
                reason,
            },
            Ok(line) => match serde_json::from_str::<ClientMessage>(line.trim()) {
                Ok(msg) => handle_message(msg, events, wait_for),
                Err(e) => ServerMessage::Error {
                    v: PROTOCOL_VERSION,
                    reason: format!("unreadable message: {e}"),
                },
            },
        };
        write_line(&mut file, &reply);
        // SAFETY: lets the client read the reply before the handle is closed.
        unsafe { DisconnectNamedPipe(client) };
    }
}

#[cfg(test)]
mod tests;
