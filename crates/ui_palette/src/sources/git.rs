//! Git: the typed `git_svc::GitAction`s for the active project. The palette only emits
//! [`PaletteEvent::GitAction`]; actions that publish or rewrite state are flagged
//! "requiere confirmación" and the host asks before running them (`GitAction::is_sensitive`).
//! `commit` / `switch` / `checkout` need text, so they open the inline form first.

use std::path::Path;

use git_svc::GitAction;
use ui_kit::Icon;

use super::top_n;
use crate::form::FormSpec;
use crate::item::{ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::scorer::{Matcher, Needle};
use crate::source::{Latency, PaletteSource, SourceQuery};

pub struct GitSource;

/// `(key, title, spanish keywords, action or form)`.
enum Entry {
    Direct(&'static str, &'static str, &'static str, GitAction),
    Form(&'static str, &'static str, &'static str, fn() -> FormSpec, GitAction),
}

fn entries() -> Vec<Entry> {
    vec![
        Entry::Direct("status", "Git: estado", "status cambios", GitAction::Status),
        Entry::Direct("add", "Git: añadir todo", "add stage preparar", GitAction::AddAll),
        Entry::Form("commit", "Git: commit…", "confirmar guardar mensaje", FormSpec::git_commit, GitAction::Commit {
            message: String::new(),
        }),
        Entry::Direct("push", "Git: push", "subir publicar enviar", GitAction::Push),
        Entry::Direct("pull", "Git: pull", "bajar traer actualizar", GitAction::Pull),
        Entry::Direct("fetch", "Git: fetch", "traer remoto prune", GitAction::Fetch),
        Entry::Form("switch", "Git: cambiar de rama…", "switch rama branch", FormSpec::git_switch, GitAction::Switch {
            branch: String::new(),
        }),
        Entry::Form("checkout", "Git: checkout…", "rama commit destino", FormSpec::git_checkout, GitAction::Checkout {
            target: String::new(),
        }),
        Entry::Direct("log", "Git: log", "historial commits grafo", GitAction::Log),
        Entry::Direct("diff", "Git: diff", "cambios diferencias", GitAction::Diff),
    ]
}

/// Walks up from `start` looking for `.git` (a folder, or a file in worktrees).
fn in_repo(start: &Path) -> bool {
    start.ancestors().any(|d| d.join(".git").exists())
}

impl PaletteSource for GitSource {
    fn id(&self) -> &'static str {
        "git"
    }

    fn title(&self) -> &'static str {
        "Git"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Actions]
    }

    /// A few `stat`s walking up the tree.
    fn latency(&self) -> Latency {
        Latency::Memory
    }

    fn order(&self) -> u8 {
        40
    }

    /// Hidden until the user types (it would crowd the empty view).
    fn empty_limit(&self) -> usize {
        0
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let Some(root) = q.ctx.root() else { return Vec::new() };
        if !in_repo(root) {
            return Vec::new();
        }
        let needle = Needle::new(q.text);
        let mut matcher = Matcher::new();
        let mut out = Vec::new();
        for (i, e) in entries().into_iter().enumerate() {
            let (key, title, keywords, action, form) = match e {
                Entry::Direct(k, t, kw, a) => (k, t, kw, a, None),
                Entry::Form(k, t, kw, f, a) => (k, t, kw, a, Some(f)),
            };
            let argv = if form.is_some() {
                // The argument is not known yet.
                match &action {
                    GitAction::Commit { .. } => "git commit -m <mensaje>".to_string(),
                    GitAction::Switch { .. } => "git switch <rama>".to_string(),
                    _ => "git checkout <destino>".to_string(),
                }
            } else {
                action.argv().join(" ")
            };
            let score = if needle.is_empty() {
                Some(super::ordered_score(i))
            } else {
                matcher.score(&needle, &[(title, 1.0), (keywords, 0.7), (&argv, 0.5)])
            };
            let Some(score) = score else { continue };
            let primary = match form {
                Some(f) => ItemAction::form("Pedir datos", f()),
                None => ItemAction::emit("Ejecutar", PaletteEvent::GitAction(action.clone())),
            };
            let mut item =
                PaletteItem::new(format!("git:{key}"), title, primary).subtitle(argv).icon(Icon::GitBranch).score(score);
            if action.is_sensitive() {
                item.meta = Some("requiere confirmación".into());
            }
            out.push(item);
        }
        top_n(out, q.limit)
    }
}
