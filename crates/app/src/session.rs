//! Session restore (docs/PLAN.md A5): the shape of the window, saved as the `session_layout` JSON
//! of the state database and restored on the next launch.
//!
//! What is saved: the tabs (kind, working directory, title override), which tab each split pane
//! shows, the active pane, the split ratios and the sidebar / right panel widths. **Never**:
//! scrollback, environment variables or anything the shells printed. The module is pure (no gpui)
//! so the JSON round trip and the validation are unit tested.
//!
//! `[session] restore = true|false` in `config.toml` (`store::Config::session`) turns the restore
//! off. Only terminals are saved: viewer tabs are files that reopen in one click.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Format version, so a future shape can be told apart.
pub const VERSION: u32 = 1;
/// Most tabs a restore will open (a corrupt file must not fork-bomb the machine).
const MAX_TABS: usize = 32;

/// What kind of tab to reopen. Only terminals exist today; unknown kinds are skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabKind {
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub kind: TabKind,
    pub cwd: Option<PathBuf>,
    /// User-chosen title (none yet: tabs are titled after their directory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub v: u32,
    pub tabs: Vec<SavedTab>,
    /// Per split pane: index into `tabs` of the tab it shows.
    pub panes: Vec<Option<usize>>,
    /// The focused pane (its tab is "the active tab").
    pub focused: usize,
    pub ratio_x: f32,
    pub ratio_y: f32,
    pub sidebar_w: Option<u16>,
    pub right_w: Option<f32>,
}

impl SessionState {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parses and sanitises a saved session; `None` when it is unusable (corrupt, other version,
    /// no tab left).
    pub fn from_json(json: &str) -> Option<Self> {
        let mut s: SessionState = serde_json::from_str(json).ok()?;
        if s.v != VERSION {
            return None;
        }
        s.tabs.truncate(MAX_TABS);
        if s.tabs.is_empty() {
            return None;
        }
        let n = s.tabs.len();
        for p in &mut s.panes {
            if p.is_some_and(|i| i >= n) {
                *p = None;
            }
        }
        // A tab can be shown by one pane only.
        let mut seen = vec![false; n];
        for p in &mut s.panes {
            if let Some(i) = *p {
                if seen[i] {
                    *p = None;
                } else {
                    seen[i] = true;
                }
            }
        }
        if s.panes.is_empty() {
            s.panes.push(Some(0));
        }
        s.focused = s.focused.min(s.panes.len() - 1);
        let finite = |r: f32| if r.is_finite() { r } else { 0.5 };
        s.ratio_x = finite(s.ratio_x);
        s.ratio_y = finite(s.ratio_y);
        s.right_w = s.right_w.filter(|w| w.is_finite());
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionState {
        SessionState {
            v: VERSION,
            tabs: vec![
                SavedTab { kind: TabKind::Terminal, cwd: Some(PathBuf::from("C:\\proj\\a")), title: None },
                SavedTab { kind: TabKind::Terminal, cwd: None, title: Some("build".into()) },
                SavedTab { kind: TabKind::Terminal, cwd: Some(PathBuf::from("C:\\proj\\c")), title: None },
            ],
            panes: vec![Some(1), Some(0)],
            focused: 1,
            ratio_x: 0.4,
            ratio_y: 0.5,
            sidebar_w: Some(300),
            right_w: Some(380.0),
        }
    }

    #[test]
    fn round_trip() {
        let s = sample();
        assert_eq!(SessionState::from_json(&s.to_json()), Some(s));
    }

    #[test]
    fn json_has_no_scrollback_or_env() {
        let j = sample().to_json();
        for forbidden in ["scrollback", "env", "output", "history"] {
            assert!(!j.contains(forbidden), "{forbidden} in {j}");
        }
    }

    #[test]
    fn corrupt_or_foreign_json_is_rejected() {
        assert!(SessionState::from_json("").is_none());
        assert!(SessionState::from_json("{").is_none());
        assert!(SessionState::from_json("{\"tabs\":[1]}").is_none());
        let mut s = sample();
        s.v = 99;
        assert!(SessionState::from_json(&s.to_json()).is_none());
        s.v = VERSION;
        s.tabs.clear();
        assert!(SessionState::from_json(&s.to_json()).is_none());
    }

    #[test]
    fn indexes_are_sanitised() {
        let mut s = sample();
        s.panes = vec![Some(7), Some(2), Some(2)];
        s.focused = 9;
        let back = SessionState::from_json(&s.to_json()).unwrap();
        assert_eq!(back.panes, vec![None, Some(2), None]);
        assert_eq!(back.focused, 2);
    }

    #[test]
    fn too_many_tabs_are_cut() {
        let mut s = sample();
        s.tabs = (0..100).map(|_| SavedTab { kind: TabKind::Terminal, cwd: None, title: None }).collect();
        s.panes = vec![Some(0), Some(50)];
        let back = SessionState::from_json(&s.to_json()).unwrap();
        assert_eq!(back.tabs.len(), MAX_TABS);
        assert_eq!(back.panes, vec![Some(0), None]);
    }
}
