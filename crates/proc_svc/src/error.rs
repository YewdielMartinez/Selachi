//! Error type shared by every module of the crate.

use std::fmt;

/// Why a termination request was refused before touching any process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefuseReason {
    /// Pid 0 (Idle) or 4 (System).
    SystemPid,
    /// The pid is the current (Selachi) process.
    SelfProcess,
    /// Critical OS process (csrss, wininit, services, lsass, smss, winlogon, ...).
    CriticalProcess(String),
    /// `explorer.exe` (refused unless [`crate::SafetyPolicy::allow_explorer`]).
    Explorer,
    /// Process lives in a different Windows session than ours.
    OtherSession {
        /// Session of the target.
        target: u32,
        /// Our session.
        ours: u32,
    },
    /// Process runs under a different user token.
    OtherUser,
    /// The process could not be inspected (access denied), so ownership cannot
    /// be verified; we refuse rather than guess.
    Unverifiable,
}

impl fmt::Display for RefuseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SystemPid => write!(f, "system pid (0/4)"),
            Self::SelfProcess => write!(f, "own process"),
            Self::CriticalProcess(n) => write!(f, "critical system process `{n}`"),
            Self::Explorer => write!(f, "explorer.exe (not allowed by policy)"),
            Self::OtherSession { target, ours } => {
                write!(f, "belongs to session {target} (ours is {ours})")
            }
            Self::OtherUser => write!(f, "belongs to another user"),
            Self::Unverifiable => write!(f, "ownership could not be verified (access denied)"),
        }
    }
}

/// Crate error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A Win32 call failed.
    #[error("{op} failed: {source}")]
    Win32 {
        /// Name of the failing API.
        op: &'static str,
        /// The underlying error.
        #[source]
        source: windows::core::Error,
    },
    /// A termination was refused by the safety policy.
    #[error("refusing to terminate pid {pid}: {reason}")]
    Refused {
        /// Offending pid.
        pid: u32,
        /// Why.
        reason: RefuseReason,
    },
    /// `JobGroup::notifications` was already called for this job.
    #[error("job notifications are already enabled for this job")]
    NotificationsAlreadyEnabled,
    /// I/O error (spawning).
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Unknown owner id.
    #[error("unknown owner {0}")]
    UnknownOwner(u64),
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn win32(op: &'static str) -> impl FnOnce(windows::core::Error) -> Error {
    move |source| Error::Win32 { op, source }
}
