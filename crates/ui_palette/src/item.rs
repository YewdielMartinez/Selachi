//! What the palette lists ([`PaletteItem`]) and what it tells the host ([`PaletteEvent`]).

use std::path::PathBuf;
use std::sync::Arc;

use gpui::SharedString;
use panel_api::TabId;
use store::PresetId;
use ui_kit::{Icon, RowStatus};

use crate::form::FormSpec;

/// The palette never executes anything itself: it emits one of these and closes. The host
/// (`app_shell`) subscribes and acts.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteEvent {
    /// Type `line` + Enter in a terminal. `new_tab` = open a fresh tab at `cwd` first; otherwise
    /// the active tab (after a `cd` when `cwd` differs).
    RunCommand {
        line: String,
        cwd: Option<PathBuf>,
        new_tab: bool,
    },
    /// Paste `text` at the prompt of the active tab **without** pressing Enter.
    InsertText(String),
    /// Open a folder: in a new tab, or `cd` in the active one.
    OpenPath { path: PathBuf, new_tab: bool },
    /// Activate that tab.
    FocusTab(TabId),
    /// A host action registered through [`crate::ActionEntry::id`] (`"tab.new"`, `"layout.toggle"`...).
    Action(SharedString),
    /// Switch to a built-in or custom layout preset.
    ApplyPreset(PresetId),
    /// Run a git action. The host asks for confirmation when [`git_svc::GitAction::is_sensitive`].
    GitAction(git_svc::GitAction),
    /// The palette closed (Esc, click outside or after any of the events above, which are emitted
    /// first). Focus has already been given back to what had it before opening.
    Closed,
}

/// Bookkeeping the palette does itself after a successful pick (non-blocking writes to the
/// state database).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// `store::StateDb::record_snippet_use`.
    SnippetUsed(i64),
    /// `store::StateDb::mark_project_opened` + `record_visit`.
    PathOpened(String),
}

/// What happens when the user confirms an item.
#[derive(Clone, Debug)]
pub enum ActionKind {
    /// Emit this event and close.
    Emit(PaletteEvent),
    /// Ask for parameters first (snippets with `{params}`, `git commit`).
    Form(Arc<FormSpec>),
}

/// One of the two actions of an item: Enter (primary) or Ctrl+Enter (alternative).
#[derive(Clone, Debug)]
pub struct ItemAction {
    /// Short verb for the footer hint ("Ejecutar", "Insertar", "Abrir en pestaña nueva").
    pub label: &'static str,
    pub kind: ActionKind,
    pub effect: Option<Effect>,
}

impl ItemAction {
    pub fn emit(label: &'static str, event: PaletteEvent) -> Self {
        Self {
            label,
            kind: ActionKind::Emit(event),
            effect: None,
        }
    }

    pub fn form(label: &'static str, spec: FormSpec) -> Self {
        Self {
            label,
            kind: ActionKind::Form(Arc::new(spec)),
            effect: None,
        }
    }

    pub fn with_effect(mut self, effect: Effect) -> Self {
        self.effect = Some(effect);
        self
    }
}

/// A result row. Built on a background thread, hence `Send`.
#[derive(Clone, Debug)]
pub struct PaletteItem {
    /// Unique across sources (`"tab:12"`, `"task:npm:package.json:dev"`); selection survives
    /// result updates by key.
    pub key: String,
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    /// Right-aligned muted text: a shortcut, "requiere confirmación", "hace 2 h".
    pub meta: Option<SharedString>,
    pub icon: Option<Icon>,
    pub status: Option<RowStatus>,
    /// Higher is better; only compared inside one source (and as the best score of a section).
    pub score: f32,
    pub primary: ItemAction,
    pub alt: Option<ItemAction>,
}

impl PaletteItem {
    pub fn new(
        key: impl Into<String>,
        title: impl Into<SharedString>,
        primary: ItemAction,
    ) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            subtitle: None,
            meta: None,
            icon: None,
            status: None,
            score: 0.0,
            primary,
            alt: None,
        }
    }

    pub fn subtitle(mut self, s: impl Into<SharedString>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    pub fn meta(mut self, s: impl Into<SharedString>) -> Self {
        self.meta = Some(s.into());
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn status(mut self, status: RowStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn score(mut self, score: f32) -> Self {
        self.score = score;
        self
    }

    pub fn alt(mut self, alt: ItemAction) -> Self {
        self.alt = Some(alt);
        self
    }
}
