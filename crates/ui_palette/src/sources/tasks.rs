//! Tareas: runnable tasks of the active project (`package.json` scripts, .NET, Cargo, Make...)
//! from `tasks_svc`, those you actually run first. Enter runs in a new tab (dev servers),
//! Ctrl+Enter in the current one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use store::StateDb;
use tasks_svc::{RankedTask, TaskCache, rank_tasks};
use ui_kit::Icon;

use super::{ordered_score, top_n};
use crate::item::{ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::scorer::{Matcher, Needle};
use crate::source::{Latency, PaletteSource, SourceQuery, display_path};

type Ranked = Arc<Vec<RankedTask>>;

pub struct TasksSource {
    db: Option<StateDb>,
    cache: TaskCache,
    /// Ranked tasks of the root used by the current palette session (the ranking reads up to 3 000
    /// history rows, so it is done once per opening, not per keystroke). Dropped on close.
    session: Mutex<Option<(PathBuf, Ranked)>>,
}

impl TasksSource {
    /// `db` ranks the tasks by what you ran before; without it they keep the discovery order.
    pub fn new(db: Option<StateDb>) -> Self {
        Self { db, cache: TaskCache::new(), session: Mutex::new(None) }
    }

    fn ranked(&self, root: &PathBuf, cwd: &str) -> Ranked {
        if let Ok(guard) = self.session.lock()
            && let Some((r, ranked)) = guard.as_ref()
            && r == root
        {
            return ranked.clone();
        }
        let tasks = self.cache.get_or_scan(root);
        let ranked: Ranked = Arc::new(match &self.db {
            Some(db) => rank_tasks(tasks.to_vec(), db, cwd),
            None => tasks
                .iter()
                .map(|t| RankedTask { task: t.clone(), score: 0.0, runs: 0, last_run: None })
                .collect(),
        });
        if let Ok(mut guard) = self.session.lock() {
            *guard = Some((root.clone(), ranked.clone()));
        }
        ranked
    }
}

impl PaletteSource for TasksSource {
    fn id(&self) -> &'static str {
        "tasks"
    }

    fn title(&self) -> &'static str {
        "Tareas"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Tasks]
    }

    fn latency(&self) -> Latency {
        Latency::Store
    }

    fn order(&self) -> u8 {
        25
    }

    fn empty_limit(&self) -> usize {
        4
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let Some(root) = q.ctx.root() else { return Vec::new() };
        let root = root.to_path_buf();
        let cwd = q.ctx.cwd.as_deref().unwrap_or(&root).to_string_lossy().into_owned();
        let ranked = self.ranked(&root, &cwd);
        if q.cancel.is_cancelled() {
            return Vec::new();
        }
        let needle = Needle::new(q.text);
        let mut matcher = Matcher::new();
        let mut out = Vec::new();
        for (i, r) in ranked.iter().enumerate() {
            let t = &r.task;
            let kind = t.kind.as_str();
            let score = if needle.is_empty() {
                Some(ordered_score(i))
            } else {
                matcher
                    .score(
                        &needle,
                        &[
                            (&t.label, 1.0),
                            (&t.command, 0.6),
                            (kind, 0.5),
                            (t.description.as_deref().unwrap_or(""), 0.3),
                        ],
                    )
                    .map(|s| s + (r.score as f32).min(6.0) * 0.03)
            };
            let Some(score) = score else { continue };
            let meta = if r.runs > 0 { format!("{kind} · {}×", r.runs) } else { kind.to_string() };
            out.push(
                PaletteItem::new(
                    format!("task:{}", t.id),
                    t.label.clone(),
                    ItemAction::emit(
                        "Ejecutar en pestaña nueva",
                        PaletteEvent::RunCommand { line: t.command.clone(), cwd: Some(t.cwd.clone()), new_tab: true },
                    ),
                )
                .alt(ItemAction::emit(
                    "Ejecutar aquí",
                    PaletteEvent::RunCommand { line: t.command.clone(), cwd: Some(t.cwd.clone()), new_tab: false },
                ))
                .subtitle(if t.cwd == root {
                    t.command.clone()
                } else {
                    format!("{}  ·  {}", t.command, display_path(&t.cwd.to_string_lossy()))
                })
                .meta(meta)
                .icon(Icon::Task)
                .score(score),
            );
        }
        top_n(out, q.limit)
    }

    fn on_close(&self) {
        if let Ok(mut guard) = self.session.lock() {
            *guard = None;
        }
    }
}
