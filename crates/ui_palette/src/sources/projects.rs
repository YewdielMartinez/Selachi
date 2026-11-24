//! Proyectos: the store's projects (pinned and recent first) plus frecent directories.
//! Enter opens a new tab there, Ctrl+Enter `cd`s the active tab.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use store::{ProjectKind, StateDb, paths::normalize_key};
use ui_kit::Icon;

use super::{ordered_score, top_n};
use crate::item::{Effect, ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::scorer::{Matcher, Needle};
use crate::source::{Latency, PaletteSource, SourceQuery, display_path};

pub struct ProjectsSource {
    db: StateDb,
}

impl ProjectsSource {
    pub fn new(db: StateDb) -> Self {
        Self { db }
    }
}

fn kind_label(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::Git => "Git",
        ProjectKind::Node => "Node",
        ProjectKind::Rust => "Rust",
        ProjectKind::Dotnet => ".NET",
        ProjectKind::Python => "Python",
        ProjectKind::Other => "Carpeta",
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

fn item(path: &str, title: String, meta: &str, score: f32) -> PaletteItem {
    let p = PathBuf::from(path);
    PaletteItem::new(
        format!("project:{}", normalize_key(path)),
        title,
        ItemAction::emit(
            "Abrir en pestaña nueva",
            PaletteEvent::OpenPath {
                path: p.clone(),
                new_tab: true,
            },
        )
        .with_effect(Effect::PathOpened(path.to_string())),
    )
    .alt(
        ItemAction::emit(
            "Abrir aquí",
            PaletteEvent::OpenPath {
                path: p,
                new_tab: false,
            },
        )
        .with_effect(Effect::PathOpened(path.to_string())),
    )
    .subtitle(display_path(path))
    .meta(meta.to_string())
    .icon(Icon::Folder)
    .score(score)
}

impl PaletteSource for ProjectsSource {
    fn id(&self) -> &'static str {
        "projects"
    }

    fn title(&self) -> &'static str {
        "Proyectos"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Projects]
    }

    fn latency(&self) -> Latency {
        Latency::Store
    }

    fn order(&self) -> u8 {
        30
    }

    fn empty_limit(&self) -> usize {
        5
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let needle = Needle::new(q.text);
        let mut matcher = Matcher::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<PaletteItem> = Vec::new();

        let projects = self.db.projects().unwrap_or_default();
        if q.cancel.is_cancelled() {
            return out;
        }
        for (i, p) in projects.iter().enumerate() {
            let score = if needle.is_empty() {
                // Pinned first, then most recently opened (the store already ordered them).
                Some(ordered_score(i) + if p.pinned { 0.5 } else { 0.0 })
            } else {
                matcher
                    .score(&needle, &[(&p.name, 1.0), (&p.path, 0.5)])
                    .map(|s| s + if p.pinned { 0.2 } else { 0.0 })
            };
            let Some(score) = score else { continue };
            if seen.insert(normalize_key(&p.path)) {
                out.push(item(&p.path, p.name.clone(), kind_label(p.kind), score));
            }
        }

        // Frecent directories that are not projects (the store's keyword search also applies).
        let frecent = self.db.frecent(q.text, 80).unwrap_or_default();
        if q.cancel.is_cancelled() {
            return out;
        }
        for (i, e) in frecent.iter().enumerate() {
            if !seen.insert(normalize_key(&e.path)) {
                continue;
            }
            let name = file_name(&e.path);
            let score = if needle.is_empty() {
                Some(ordered_score(i) - 0.3)
            } else {
                matcher
                    .score(&needle, &[(&name, 1.0), (&e.path, 0.5)])
                    .map(|s| s - 0.1)
            };
            let Some(score) = score else { continue };
            out.push(item(&e.path, name, "reciente", score));
        }

        // Only offer folders that still exist.
        let mut out = top_n(out, q.limit.saturating_mul(2));
        out.retain(|it| match &it.primary.kind {
            crate::item::ActionKind::Emit(PaletteEvent::OpenPath { path, .. }) => path.is_dir(),
            _ => true,
        });
        out.truncate(q.limit);
        out
    }
}
