//! `ui_doctor`: "Selachi doctor" (backlog A14), the app's own health as a panel.
//!
//! * [`DoctorModel`]: samples the own process (RAM working set + private, CPU %, threads), its
//!   child processes, frame times, session counts, DB sizes and grammars, every second **only while
//!   the panel is visible**. The app feeds the parts only it knows: [`DoctorModel::set_app_stats`],
//!   [`DoctorModel::set_db_paths`], and frames either through the probes ([`frame_begin`] first and
//!   [`frame_end`] last in the window root, sharing [`DoctorModel::frame_stats`]) or
//!   [`DoctorModel::record_frame`].
//! * [`DoctorPanelProvider`]: the `PanelProvider` with id `"doctor"` (all four zones), with tonal
//!   tiles, dither sparklines (last 60 samples) and "Copiar diagnóstico" (markdown).
//! * [`thumbnail`]: the 1-bit wireframe for the layout panel.
#![cfg(windows)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod frames;
mod health;
mod model;
mod source;
mod view;

pub use frames::{FrameStats, frame_begin, frame_end};
pub use health::{
    AppStats, ChildProc, DbPaths, DbSize, FrameInfo, HISTORY, Health, RawSample, format_bytes,
    format_ms, format_pct,
};
pub use model::{DoctorModel, SAMPLE_INTERVAL};
pub use source::{DoctorSource, SystemSource};
pub use view::{DoctorPanel, DoctorPanelProvider, PANEL_ID, thumbnail};

#[cfg(test)]
mod tests;
