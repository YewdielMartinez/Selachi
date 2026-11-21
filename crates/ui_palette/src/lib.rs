//! Selachi command palette (PLAN A1, `Ctrl+K`).
//!
//! A centered overlay card with a search field and results grouped by section. Everything the
//! user can do "in two keys": run an action, jump to a tab, open a project, re-run a command from
//! the history, launch a project task, fill and insert a snippet, run a git action, switch the
//! layout preset.
//!
//! The palette **executes nothing**: it emits a [`PaletteEvent`] and closes; the host (the app
//! shell) handles it.
//!
//! # Wiring (the integrator's checklist)
//!
//! ```ignore
//! // 1. Startup: key bindings (+ Ctrl+K) and the entity.
//! ui_palette::init(cx);
//! ui_palette::bind_toggle_key(cx);
//! let sources = ui_palette::standard_sources(Some(db.clone()), my_actions(), custom_preset_names);
//! let palette = ui_palette::command_palette(theme, Some(db), sources, cx);
//! cx.subscribe(&palette, |shell, _, ev: &PaletteEvent, cx| shell.on_palette(ev.clone(), cx)).detach();
//!
//! // 2. Ctrl+K: toggle with a fresh snapshot of the tabs.
//! cx.on_action(|_: &ui_palette::TogglePalette, cx| { /* shell.palette.update(cx, |p, cx| p.toggle(ctx, window, cx)) */ });
//!
//! // 3. Render: the LAST child of the window root (an absolute, full-size overlay; the root must
//! //    fill the window). It renders nothing while closed.
//! root.child(self.palette.clone())
//! ```
//!
//! # Input scopes
//!
//! `>` acciones · `#` historial · `@` proyectos · `!` tareas · `$` snippets · nothing = all,
//! ranked. Tab / Shift+Tab cycle the scope.
//!
//! # Performance
//!
//! Every source runs on the background executor, one task each, and answers are merged as they
//! arrive: in-memory sources (actions, tabs, git, presets) are queried on every keystroke,
//! store-backed ones (history, projects, tasks, snippets) after a 30 ms debounce. A newer query
//! cancels the older one ([`Cancel`]). While closed there are no tasks and no timers; sources drop
//! their per-session caches in [`PaletteSource::on_close`].
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod form;
mod input;
mod item;
mod model;
mod query;
mod scorer;
mod source;
mod sources;
mod view;

pub use form::{FormSpec, FormState, FormSubmit};
pub use item::{ActionKind, Effect, ItemAction, PaletteEvent, PaletteItem};
pub use model::{Confirmed, LayoutRow, PaletteModel, Results, Section, plan, run_blocking, run_source};
pub use query::{Query, Scope};
pub use scorer::{Matcher, Needle, fold};
pub use source::{
    ALL_TYPED_LIMIT, Cancel, Latency, PaletteSource, SCOPED_LIMIT, SourceCtx, SourceQuery, ago, display_path,
};
pub use sources::{
    ActionEntry, ActionsSource, GitSource, HistorySource, PresetsSource, ProjectsSource, SnippetsSource, TabsSource,
    TasksSource,
};
pub use view::{
    CommandPalette, Confirm, ConfirmAlt, DEBOUNCE, Dismiss, NextScope, OpenContext, PageDown, PageUp, PrevScope,
    SelectNext, SelectPrev, Timings, TogglePalette, bind_toggle_key, command_palette, init,
};

use std::sync::Arc;

/// Every built-in source, ready to hand to [`command_palette`]: tabs, the host's `actions`, git,
/// layout presets (`custom_presets` are the names of the user's own) and, when `db` is given,
/// projects, history, tasks and snippets.
pub fn standard_sources(
    db: Option<store::StateDb>,
    actions: Vec<ActionEntry>,
    custom_presets: Vec<String>,
) -> Vec<Arc<dyn PaletteSource>> {
    let mut v: Vec<Arc<dyn PaletteSource>> = vec![
        Arc::new(TabsSource),
        Arc::new(ActionsSource::new(actions)),
        Arc::new(GitSource),
        Arc::new(PresetsSource::new(custom_presets)),
        Arc::new(TasksSource::new(db.clone())),
    ];
    if let Some(db) = db {
        v.push(Arc::new(ProjectsSource::new(db.clone())));
        v.push(Arc::new(SnippetsSource::new(db.clone())));
        v.push(Arc::new(HistorySource::new(db)));
    }
    v
}
