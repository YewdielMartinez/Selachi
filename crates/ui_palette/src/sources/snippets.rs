//! Snippets: saved command templates with `{param}` placeholders (A12). A snippet without
//! parameters is inserted (Enter) or run (Ctrl+Enter); one with parameters opens the inline form
//! first (see [`crate::FormSpec`]). Using one is recorded with `record_snippet_use`.

use store::{Snippet, StateDb};
use ui_kit::Icon;

use super::{ordered_score, top_n};
use crate::form::FormSpec;
use crate::item::{Effect, ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::scorer::{Matcher, Needle};
use crate::source::{Latency, PaletteSource, SourceQuery};

pub struct SnippetsSource {
    db: StateDb,
}

impl SnippetsSource {
    pub fn new(db: StateDb) -> Self {
        Self { db }
    }
}

fn one_line(template: &str) -> String {
    template.lines().next().unwrap_or("").trim().to_string()
}

/// The item for `s`, with the form or the direct actions.
fn snippet_item(s: &Snippet, cwd: Option<&std::path::Path>, score: f32) -> PaletteItem {
    let params = s.params();
    let subtitle = if s.description.is_empty() { one_line(&s.template) } else { s.description.clone() };
    let key = format!("snippet:{}", s.id);
    let used = Effect::SnippetUsed(s.id);
    let (primary, alt, meta) = if params.is_empty() {
        // Render still unescapes `{{` / `}}`.
        let line = store::render(&s.template, &Default::default(), store::Quoting::PowerShell)
            .unwrap_or_else(|_| s.template.clone());
        (
            ItemAction::emit("Insertar", PaletteEvent::InsertText(line.clone())).with_effect(used.clone()),
            Some(
                ItemAction::emit(
                    "Ejecutar",
                    PaletteEvent::RunCommand { line, cwd: cwd.map(Into::into), new_tab: false },
                )
                .with_effect(used),
            ),
            if s.uses > 0 { format!("{}×", s.uses) } else { String::new() },
        )
    } else {
        let n = params.len();
        let spec = FormSpec::snippet(s.id, s.name.clone(), s.template.clone(), params, cwd.map(Into::into));
        (
            ItemAction::form("Rellenar parámetros", spec),
            None,
            format!("{n} {}", if n == 1 { "parámetro" } else { "parámetros" }),
        )
    };
    let mut item = PaletteItem::new(key, s.name.clone(), primary).subtitle(subtitle).icon(Icon::Snippet).score(score);
    item.alt = alt;
    if !meta.is_empty() {
        item.meta = Some(meta.into());
    }
    item
}

impl PaletteSource for SnippetsSource {
    fn id(&self) -> &'static str {
        "snippets"
    }

    fn title(&self) -> &'static str {
        "Snippets"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Snippets]
    }

    fn latency(&self) -> Latency {
        Latency::Store
    }

    fn order(&self) -> u8 {
        35
    }

    fn empty_limit(&self) -> usize {
        3
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let cwd_text = q.ctx.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let cwd = q.ctx.cwd.as_deref();
        if q.is_empty() {
            // Most used first (the store orders by usage and recency).
            let rows = self.db.search_snippets("", cwd_text.as_deref()).unwrap_or_default();
            return rows
                .iter()
                .take(q.limit)
                .enumerate()
                .map(|(i, s)| snippet_item(s, cwd, ordered_score(i)))
                .collect();
        }
        let all = self.db.list_snippets().unwrap_or_default();
        if q.cancel.is_cancelled() {
            return Vec::new();
        }
        let needle = Needle::new(q.text);
        let mut matcher = Matcher::new();
        let mut out = Vec::new();
        for s in all.iter().filter(|s| s.visible_in(cwd_text.as_deref())) {
            let tags = s.tags.join(" ");
            let Some(score) = matcher.score(
                &needle,
                &[(&s.name, 1.0), (&s.description, 0.6), (&tags, 0.6)],
            ) else {
                continue;
            };
            // A little weight for what you really use.
            let boost = (1.0 + f64::from(s.uses)).ln() as f32 * 0.05;
            out.push(snippet_item(s, cwd, score + boost));
        }
        top_n(out, q.limit)
    }
}
