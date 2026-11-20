//! `term_core`: a UI-agnostic terminal session engine.
//!
//! Wraps `alacritty_terminal` (ConPTY on Windows) and exposes sessions, snapshots of the
//! visible viewport with resolved colors, and xterm key encoding. No GUI dependencies.
//!
//! Typical use: [`Session::spawn`], then on every [`SessionEvent::Wakeup`] call
//! [`Session::snapshot`] and repaint; forward key presses through [`encode_key`] into
//! [`Session::write`].

mod color;
mod keys;
mod session;
mod shell;
mod snapshot;

pub use color::{Palette, Rgb8, indexed_color};
pub use keys::{KeyInput, encode_key};
pub use session::{Session, SessionConfig, SessionEvent, TermError};
pub use shell::default_shell;
pub use snapshot::{CellFlags, CellView, Snapshot};
