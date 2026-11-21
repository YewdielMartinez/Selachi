//! The extension point: a [`PaletteSource`] turns a query into [`PaletteItem`]s.
//!
//! Sources run on gpui's background executor, one task each, so a slow one (history, a first
//! task scan) never delays a fast one (actions, tabs): the view merges results as they arrive.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use panel_api::{TabId, TabInfo};

use crate::item::PaletteItem;
use crate::query::Scope;

/// Cooperative cancellation: the palette flips it when a newer query replaces the one a source is
/// still working on. Long loops should poll [`Cancel::is_cancelled`] and return early (the result
/// is dropped anyway).
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// How quickly a source can answer; decides debouncing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Latency {
    /// In-memory data: queried on every keystroke without waiting.
    Memory,
    /// Hits the database or the disk: queried after the 30 ms debounce.
    Store,
}

/// What the host knows when it opens the palette. Snapshot: sources read it from any thread.
#[derive(Clone, Debug, Default)]
pub struct SourceCtx {
    /// Working directory of the active tab.
    pub cwd: Option<PathBuf>,
    /// Project (git root) that contains `cwd`.
    pub project_root: Option<PathBuf>,
    /// Every tab, in strip order.
    pub tabs: Vec<TabInfo>,
    pub active_tab: Option<TabId>,
}

impl SourceCtx {
    /// `project_root`, else `cwd`: where tasks and git actions apply.
    pub fn root(&self) -> Option<&Path> {
        self.project_root.as_deref().or(self.cwd.as_deref())
    }
}

/// One query for one source.
pub struct SourceQuery<'a> {
    /// The text after the scope prefix (may be empty).
    pub text: &'a str,
    pub scope: Scope,
    /// Return at most this many items.
    pub limit: usize,
    pub ctx: &'a SourceCtx,
    pub cancel: &'a Cancel,
}

impl SourceQuery<'_> {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// Most rows a source adds to the "everything" view when the user has typed something.
pub const ALL_TYPED_LIMIT: usize = 5;
/// Most rows a source adds when the scope is narrowed to it.
pub const SCOPED_LIMIT: usize = 60;

/// A provider of palette results. Implement it to plug something new in (a host can register
/// extra sources with [`crate::CommandPalette::set_source`]).
pub trait PaletteSource: Send + Sync + 'static {
    /// Stable id, unique among sources (registering the same id replaces the source).
    fn id(&self) -> &'static str;

    /// Section header ("Acciones").
    fn title(&self) -> &'static str;

    /// The narrowed scopes that include this source (it is always part of [`Scope::All`]).
    fn scopes(&self) -> &'static [Scope];

    fn latency(&self) -> Latency {
        Latency::Memory
    }

    /// Position of its section when the query is empty (lower first) and the tie-break otherwise.
    fn order(&self) -> u8 {
        50
    }

    /// Bias added to the section's best score when sections are ordered for a typed query
    /// (history is negative so it does not jump above exact action matches).
    fn weight(&self) -> f32 {
        0.0
    }

    /// Rows shown in the "everything" view before the user types anything (0 = none).
    fn empty_limit(&self) -> usize {
        4
    }

    /// Produce items for `q`, best first or not (the palette sorts by score). Runs on a
    /// background thread; poll `q.cancel` in long loops.
    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem>;

    /// The palette closed: drop caches.
    fn on_close(&self) {}
}

impl dyn PaletteSource {
    /// Whether a query in `scope` is routed to this source.
    pub fn applies(&self, scope: Scope) -> bool {
        scope == Scope::All || self.scopes().contains(&scope)
    }

    /// How many items this source may add for the query.
    pub fn limit_for(&self, scope: Scope, empty: bool) -> usize {
        if scope != Scope::All {
            SCOPED_LIMIT
        } else if empty {
            self.empty_limit()
        } else {
            ALL_TYPED_LIMIT
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers shared by the built-in sources
// ---------------------------------------------------------------------------------------------

/// `C:\Users\me\Desktop\Terminal` -> `~/Desktop/Terminal`; long paths keep their last segments.
pub fn display_path(path: &str) -> String {
    let mut s = path.replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    if let Some(home) = std::env::var_os("USERPROFILE").and_then(|h| h.into_string().ok()) {
        let home = home.replace('\\', "/");
        let home = home.trim_end_matches('/');
        if s.len() >= home.len() && s[..home.len()].eq_ignore_ascii_case(home) {
            let rest = &s[home.len()..];
            if rest.is_empty() || rest.starts_with('/') {
                s = format!("~{rest}");
            }
        }
    }
    const MAX: usize = 52;
    if s.chars().count() > MAX {
        let parts: Vec<&str> = s.split('/').collect();
        let mut tail: Vec<&str> = Vec::new();
        let mut len = 2;
        for part in parts.iter().rev() {
            if len + part.chars().count() + 1 > MAX && !tail.is_empty() {
                break;
            }
            len += part.chars().count() + 1;
            tail.push(part);
        }
        tail.reverse();
        s = format!("…/{}", tail.join("/"));
    }
    s
}

/// `hace 5 min`, `hace 3 h`, `ayer`, `hace 12 d`.
pub fn ago(now_ms: i64, then_ms: i64) -> String {
    let secs = ((now_ms - then_ms) / 1000).max(0);
    match secs {
        0..=59 => "ahora".to_string(),
        60..=3599 => format!("hace {} min", secs / 60),
        3600..=86_399 => format!("hace {} h", secs / 3600),
        86_400..=172_799 => "ayer".to_string(),
        _ => format!("hace {} d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_buckets() {
        let now = 10_000_000_000;
        assert_eq!(ago(now, now - 5_000), "ahora");
        assert_eq!(ago(now, now - 5 * 60_000), "hace 5 min");
        assert_eq!(ago(now, now - 3 * 3_600_000), "hace 3 h");
        assert_eq!(ago(now, now - 30 * 3_600_000), "ayer");
        assert_eq!(ago(now, now - 12 * 86_400_000), "hace 12 d");
    }

    #[test]
    fn long_paths_keep_their_tail() {
        let p = display_path("C:/a/very/long/path/with/many/segments/that/goes/on/and/on/project-name");
        assert!(p.starts_with("…/") && p.ends_with("project-name"), "{p}");
        assert!(p.chars().count() <= 56);
        assert_eq!(display_path("C:\\x\\y"), "C:/x/y");
    }

    #[test]
    fn cancel_flips_for_every_clone() {
        let c = Cancel::new();
        let d = c.clone();
        assert!(!d.is_cancelled());
        c.cancel();
        assert!(d.is_cancelled());
    }
}
