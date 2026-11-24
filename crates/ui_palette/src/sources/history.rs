//! Historial: the command history (FTS5 token-prefix search, ranked by recency, frequency and
//! the current folder). Enter runs the command in the active tab, Ctrl+Enter only inserts it.

use store::{StateDb, now_ms};
use ui_kit::{Icon, RowStatus};

use crate::item::{ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::source::{Latency, PaletteSource, SourceQuery, ago, display_path};

pub struct HistorySource {
    db: StateDb,
}

impl HistorySource {
    pub fn new(db: StateDb) -> Self {
        Self { db }
    }
}

impl PaletteSource for HistorySource {
    fn id(&self) -> &'static str {
        "history"
    }

    fn title(&self) -> &'static str {
        "Historial"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::History]
    }

    fn latency(&self) -> Latency {
        Latency::Store
    }

    fn order(&self) -> u8 {
        60
    }

    /// Below exact action / tab matches.
    fn weight(&self) -> f32 {
        -0.25
    }

    fn empty_limit(&self) -> usize {
        4
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let cwd = q.ctx.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let rows = match self.db.search(q.text.trim(), cwd.as_deref(), q.limit) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        if q.cancel.is_cancelled() {
            return Vec::new();
        }
        let now = now_ms();
        rows.into_iter()
            .enumerate()
            .map(|(i, h)| {
                let line = h.command.lines().next().unwrap_or("").to_string();
                let mut meta = ago(now, h.last_ts);
                if h.count > 1 {
                    meta = format!("{}× · {meta}", h.count);
                }
                let failed = h.last_exit_code.is_some_and(|c| c != 0);
                let mut item = PaletteItem::new(
                    format!("history:{}", h.command),
                    line,
                    ItemAction::emit(
                        "Ejecutar",
                        PaletteEvent::RunCommand {
                            line: h.command.clone(),
                            cwd: q.ctx.cwd.clone(),
                            new_tab: false,
                        },
                    ),
                )
                .alt(ItemAction::emit(
                    "Insertar",
                    PaletteEvent::InsertText(h.command.clone()),
                ))
                .subtitle(display_path(&h.last_cwd))
                .meta(meta)
                .icon(Icon::History)
                // The store already ranked them; keep its order.
                .score((1.0 - i as f32 * 0.02).max(0.1));
                if failed {
                    item.status = Some(RowStatus::Error);
                }
                item
            })
            .collect()
    }
}
