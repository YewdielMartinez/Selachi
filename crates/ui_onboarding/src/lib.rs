//! Selachi's first-run wizard (wave 5.9): six skippable steps (Bienvenida, Agentes, Espacios y
//! proyectos, Apariencia, Terminal, Listo) that write `config.toml` through `store`, keeping its
//! comments, and import projects and favorites into `state.db`.
//!
//! ```ignore
//! // Before the main window:
//! if ui_onboarding::needs_onboarding(&db) {
//!     let view = ui_onboarding::onboarding(config, Some(db.clone()), theme, cx);
//!     cx.subscribe(&view, |_, ev, cx| match ev {
//!         OnboardingEvent::Finished { config, projects, favorites } => { /* apply, open the shell */ }
//!         OnboardingEvent::Skipped => { /* open the shell */ }
//!         OnboardingEvent::OpenLoginTerminal { agent, command } => { /* new tab running `command` */ }
//!         OnboardingEvent::ThemePreview(mode) => { /* re-theme the window */ }
//!     }).detach();
//!     // root view of the window: view.clone(); then view.read(cx).focus(window)
//! }
//! ```
//!
//! "Volver a configurar" in Settings builds the same view at any time; it never consults the flag.
//!
//! Costs: nothing runs after the wizard is finished or skipped (tasks dropped, folder scan
//! cancelled, GIF frames freed). While it is open only the welcome step animates, and only while
//! that step is on screen.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod agents;
pub mod flag;
pub mod flow;
pub mod gif;
pub mod layouts;
pub mod model;
pub mod projects;
pub mod shells;

mod render;
mod steps;
mod view;
mod widgets;

pub use agents::{AgentCli, AgentRow, AgentStatus};
pub use flag::{mark_done, needs_onboarding, reset};
pub use flow::{Flow, Nav, Step};
pub use model::{Draft, Outcome, ThemeChoice, WorktreeLocation};
pub use view::{OnboardingEvent, OnboardingView, Services, onboarding, onboarding_with};

#[cfg(test)]
mod tests;
