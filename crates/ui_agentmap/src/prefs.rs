//! How the user wants the map organized, and what is remembered between runs.
//!
//! [`MapPrefs`] is what survives a restart (view, grouping, sort, filter, which groups are
//! collapsed, where the map lives). It is serialized to one small JSON string that the host keeps
//! in its state database. [`MapSession`] is what only lasts while the app runs (the search text,
//! «Ocultar terminados de hace más de X»). Pure data, unit-tested.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::view::MapMode;

/// Most collapsed ids kept (a long-lived install must not grow this forever).
pub const MAX_COLLAPSED: usize = 400;

/// How the delegations are grouped in the tree and the timeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GroupBy {
    /// The delegation tree: every orchestrator with its children.
    #[default]
    Parent,
    Agent,
    Status,
    Project,
}

impl GroupBy {
    pub const ALL: [GroupBy; 4] = [
        GroupBy::Parent,
        GroupBy::Agent,
        GroupBy::Status,
        GroupBy::Project,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GroupBy::Parent => "Padre",
            GroupBy::Agent => "Agente",
            GroupBy::Status => "Estado",
            GroupBy::Project => "Proyecto",
        }
    }

    pub fn id_word(self) -> &'static str {
        match self {
            GroupBy::Parent => "parent",
            GroupBy::Agent => "agent",
            GroupBy::Status => "status",
            GroupBy::Project => "project",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.id_word() == s)
    }
}

/// Order of the siblings of a group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortBy {
    /// Oldest first: the order they were delegated in.
    #[default]
    Start,
    /// Newest first.
    Recent,
    /// Longest first.
    Duration,
}

impl SortBy {
    pub const ALL: [SortBy; 3] = [SortBy::Start, SortBy::Recent, SortBy::Duration];

    pub fn label(self) -> &'static str {
        match self {
            SortBy::Start => "Inicio",
            SortBy::Recent => "Recientes",
            SortBy::Duration => "Duración",
        }
    }

    fn word(self) -> &'static str {
        match self {
            SortBy::Start => "start",
            SortBy::Recent => "recent",
            SortBy::Duration => "duration",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.word() == s)
    }
}

/// Which delegations are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StatusFilter {
    #[default]
    All,
    /// In flight (running or waiting for an approval).
    Running,
    Failed,
    /// Finished without failing (done, cancelled).
    Done,
    /// Agent CLIs launched outside the harness.
    External,
}

impl StatusFilter {
    pub const ALL: [StatusFilter; 5] = [
        StatusFilter::All,
        StatusFilter::Running,
        StatusFilter::Failed,
        StatusFilter::Done,
        StatusFilter::External,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StatusFilter::All => "Todos",
            StatusFilter::Running => "En marcha",
            StatusFilter::Failed => "Fallidos",
            StatusFilter::Done => "Terminados",
            StatusFilter::External => "Externos",
        }
    }

    fn word(self) -> &'static str {
        match self {
            StatusFilter::All => "all",
            StatusFilter::Running => "running",
            StatusFilter::Failed => "failed",
            StatusFilter::Done => "done",
            StatusFilter::External => "external",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.word() == s)
    }
}

/// Where the map lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Placement {
    /// Docked in the right zone (resizable there).
    #[default]
    Dock,
    /// A tab of the main zone.
    Tab,
}

impl Placement {
    fn word(self) -> &'static str {
        match self {
            Placement::Dock => "dock",
            Placement::Tab => "tab",
        }
    }
}

/// What is remembered between runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapPrefs {
    pub mode: MapMode,
    pub group: GroupBy,
    pub sort: SortBy,
    pub filter: StatusFilter,
    /// Every session at once instead of the selected orchestrator.
    pub all_sessions: bool,
    pub placement: Placement,
    /// Collapsed nodes (`n:<node id>`) and groups (`g:<grouping>:<group>`).
    pub collapsed: BTreeSet<String>,
}

impl Default for MapPrefs {
    fn default() -> Self {
        Self {
            mode: MapMode::Tree,
            group: GroupBy::Parent,
            sort: SortBy::Start,
            filter: StatusFilter::All,
            all_sessions: false,
            placement: Placement::Dock,
            collapsed: BTreeSet::new(),
        }
    }
}

impl MapPrefs {
    pub fn is_collapsed(&self, id: &str) -> bool {
        self.collapsed.contains(id)
    }

    /// Flips `id`; returns whether it is collapsed now.
    pub fn toggle_collapsed(&mut self, id: &str) -> bool {
        if self.collapsed.remove(id) {
            return false;
        }
        if self.collapsed.len() >= MAX_COLLAPSED
            && let Some(first) = self.collapsed.iter().next().cloned()
        {
            self.collapsed.remove(&first);
        }
        self.collapsed.insert(id.to_string());
        true
    }

    /// Collapses or expands every id of `ids` at once.
    pub fn set_all_collapsed(&mut self, ids: impl IntoIterator<Item = String>, collapsed: bool) {
        for id in ids {
            if collapsed {
                if self.collapsed.len() < MAX_COLLAPSED {
                    self.collapsed.insert(id);
                }
            } else {
                self.collapsed.remove(&id);
            }
        }
    }

    pub fn to_json(&self) -> String {
        json!({
            "v": 1,
            "mode": match self.mode {
                MapMode::Tree => "tree",
                MapMode::Graph => "graph",
                MapMode::Timeline => "timeline",
            },
            "group": self.group.id_word(),
            "sort": self.sort.word(),
            "filter": self.filter.word(),
            "all_sessions": self.all_sessions,
            "placement": self.placement.word(),
            "collapsed": self.collapsed.iter().collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// Reads what [`Self::to_json`] wrote. Tolerant: anything missing or ill-typed keeps its
    /// default, text that is not JSON gives the defaults.
    pub fn from_json(text: &str) -> Self {
        let mut p = Self::default();
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return p;
        };
        let word = |k: &str| v.get(k).and_then(Value::as_str);
        p.mode = match word("mode") {
            Some("graph") => MapMode::Graph,
            Some("timeline") => MapMode::Timeline,
            _ => MapMode::Tree,
        };
        p.group = word("group").and_then(GroupBy::parse).unwrap_or_default();
        p.sort = word("sort").and_then(SortBy::parse).unwrap_or_default();
        p.filter = word("filter")
            .and_then(StatusFilter::parse)
            .unwrap_or_default();
        p.all_sessions = v
            .get("all_sessions")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        p.placement = match word("placement") {
            Some("tab") => Placement::Tab,
            _ => Placement::Dock,
        };
        if let Some(list) = v.get("collapsed").and_then(Value::as_array) {
            p.collapsed = list
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| s.len() <= 200)
                .take(MAX_COLLAPSED)
                .map(str::to_string)
                .collect();
        }
        p
    }
}

/// Choices of «Ocultar terminados de hace más de…» (minutes; `None` = show everything).
pub const HIDE_OLDER_CHOICES: [Option<u32>; 4] = [None, Some(5), Some(15), Some(60)];

pub fn hide_older_label(minutes: Option<u32>) -> &'static str {
    match minutes {
        None => "No",
        Some(5) => "5 min",
        Some(15) => "15 min",
        Some(60) => "1 h",
        Some(_) => "…",
    }
}

/// What only lasts while the app runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapSession {
    /// The search box.
    pub query: String,
    /// «Ocultar terminados de hace más de X minutos».
    pub hide_older_min: Option<u32>,
}

impl MapSession {
    pub fn hide_older_ms(&self) -> Option<i64> {
        self.hide_older_min.map(|m| i64::from(m) * 60_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_round_trip_through_json() {
        let mut p = MapPrefs {
            mode: MapMode::Graph,
            group: GroupBy::Agent,
            sort: SortBy::Duration,
            filter: StatusFilter::Failed,
            all_sessions: true,
            placement: Placement::Tab,
            ..MapPrefs::default()
        };
        p.toggle_collapsed("n:t:7");
        p.toggle_collapsed("g:agent:codex");
        let back = MapPrefs::from_json(&p.to_json());
        assert_eq!(back, p);
    }

    #[test]
    fn garbage_and_unknown_values_fall_back_to_the_defaults() {
        assert_eq!(MapPrefs::from_json("no es json"), MapPrefs::default());
        let p = MapPrefs::from_json(r#"{"mode":"hologram","group":"x","collapsed":[1,"a"],"filter":3}"#);
        assert_eq!(p.mode, MapMode::Tree);
        assert_eq!(p.group, GroupBy::Parent);
        assert_eq!(p.filter, StatusFilter::All);
        assert_eq!(p.collapsed.len(), 1);
    }

    #[test]
    fn collapsed_ids_toggle_and_are_capped() {
        let mut p = MapPrefs::default();
        assert!(p.toggle_collapsed("n:t:1"));
        assert!(p.is_collapsed("n:t:1"));
        assert!(!p.toggle_collapsed("n:t:1"));
        assert!(!p.is_collapsed("n:t:1"));
        for i in 0..(MAX_COLLAPSED + 50) {
            p.toggle_collapsed(&format!("n:t:{i}"));
        }
        assert_eq!(p.collapsed.len(), MAX_COLLAPSED);
        p.set_all_collapsed(["x".to_string(), "y".to_string()], true);
        assert!(p.is_collapsed("x") || p.collapsed.len() == MAX_COLLAPSED);
        p.set_all_collapsed(["x".to_string(), "y".to_string()], false);
        assert!(!p.is_collapsed("x") && !p.is_collapsed("y"));
    }

    #[test]
    fn the_session_hide_option_is_in_minutes() {
        let s = MapSession {
            hide_older_min: Some(15),
            ..MapSession::default()
        };
        assert_eq!(s.hide_older_ms(), Some(900_000));
        assert_eq!(MapSession::default().hide_older_ms(), None);
        assert_eq!(hide_older_label(Some(60)), "1 h");
    }
}
