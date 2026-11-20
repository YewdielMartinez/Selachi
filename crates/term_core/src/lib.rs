//! `term_core`: a UI-agnostic terminal session engine.
//!
//! Wraps `alacritty_terminal` (ConPTY on Windows) and exposes sessions, snapshots of the
//! visible viewport with resolved colors, and xterm key encoding. No GUI dependencies.
//!
//! Shell integration: the default PowerShell is started with a script that reports its working
//! directory (OSC 7) and prompt marks (OSC 133), surfaced as [`SessionEvent::Cwd`] and
//! [`SessionEvent::CommandFinished`].
//!
//! Typical use: [`Session::spawn`], then on every [`SessionEvent::Wakeup`] call
//! [`Session::snapshot`] and repaint; forward key presses through [`encode_key`] into
//! [`Session::write`].
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod color;
mod keys;
mod osc;
mod session;
mod shell;
mod snapshot;
mod tap;

pub use color::{Palette, Rgb8, indexed_color};
pub use keys::{KeyInput, encode_key};
pub use session::{Session, SessionConfig, SessionEvent, TermError};
pub use shell::{PWSH_INTEGRATION, default_shell, install_pwsh_integration, pwsh_integration_args};
pub use snapshot::{CellFlags, CellView, Snapshot};
