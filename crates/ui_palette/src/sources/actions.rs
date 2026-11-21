//! Acciones: the host's own commands (`tab.new`, `layout.toggle`...), with their shortcut.

use gpui::SharedString;
use ui_kit::Icon;

use super::{ordered_score, top_n};
use crate::item::{ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::scorer::{Matcher, Needle};
use crate::source::{PaletteSource, SourceQuery};

/// An action the host can run; picking it emits [`PaletteEvent::Action`] with `id`.
#[derive(Clone, Debug)]
pub struct ActionEntry {
    pub id: SharedString,
    pub title: SharedString,
    /// Shown on the right (`"Ctrl+Shift+T"`).
    pub shortcut: Option<SharedString>,
    pub icon: Option<Icon>,
    /// Extra words that find the action (`"nuevo crear"`).
    pub keywords: SharedString,
}

impl ActionEntry {
    pub fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self { id: id.into(), title: title.into(), shortcut: None, icon: None, keywords: SharedString::default() }
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn keywords(mut self, keywords: impl Into<SharedString>) -> Self {
        self.keywords = keywords.into();
        self
    }
}

/// A fixed list of [`ActionEntry`]s. Build a new one and
/// [`set_source`](crate::CommandPalette::set_source) it when the list changes.
pub struct ActionsSource {
    entries: Vec<ActionEntry>,
}

impl ActionsSource {
    pub fn new(entries: Vec<ActionEntry>) -> Self {
        Self { entries }
    }
}

impl PaletteSource for ActionsSource {
    fn id(&self) -> &'static str {
        "actions"
    }

    fn title(&self) -> &'static str {
        "Acciones"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Actions]
    }

    fn order(&self) -> u8 {
        20
    }

    fn empty_limit(&self) -> usize {
        6
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let needle = Needle::new(q.text);
        let mut matcher = Matcher::new();
        let mut out = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let score = if needle.is_empty() {
                Some(ordered_score(i))
            } else {
                matcher.score(&needle, &[(&e.title, 1.0), (&e.keywords, 0.7), (&e.id, 0.4)])
            };
            let Some(score) = score else { continue };
            let mut item = PaletteItem::new(
                format!("action:{}", e.id),
                e.title.clone(),
                ItemAction::emit("Ejecutar", PaletteEvent::Action(e.id.clone())),
            )
            .icon(e.icon.unwrap_or(Icon::ChevronRight))
            .score(score);
            item.meta = e.shortcut.clone();
            out.push(item);
        }
        top_n(out, q.limit)
    }
}
