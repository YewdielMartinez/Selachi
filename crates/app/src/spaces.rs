//! The "spaces" of the unified sidebar (docs/olas/ola-5.md 5.10): a space is a project (or
//! worktree) and the tabs that live in it, plus the favorite / recent projects that have no tab
//! open yet. Pure over [`TabInfo`], so it is unit tested; the shell adds the git data.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use panel_api::{RowStatus, TabInfo, format_elapsed};
use store::GroupBy;

use crate::grouping::{aggregate_status, group_tabs, project_key, project_name};

/// Branch and "has uncommitted changes" of a repository root.
pub type GitSummary = (String, bool);

/// One block of the sidebar: a header and its nested tab rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Space {
    /// Project root; empty for the groups that are not a project (status, agent, no project).
    pub root: PathBuf,
    pub name: String,
    /// Indices into the tab list, in display order. Empty = a project without open tabs.
    pub tabs: Vec<usize>,
    pub branch: Option<String>,
    pub dirty: bool,
    /// The most urgent status among its tabs.
    pub status: Option<RowStatus>,
    /// Tabs that belong to no project (settings, loose viewers): listed as plain rows, without a
    /// header.
    pub headless: bool,
}

impl Space {
    /// Has at least one open tab.
    pub fn is_open(&self) -> bool {
        !self.tabs.is_empty()
    }

    /// A stable key for the collapsed set (the root, or the label for non-project groups).
    pub fn key(&self) -> PathBuf {
        if self.root.as_os_str().is_empty() { PathBuf::from(format!("group:{}", self.name)) } else { self.root.clone() }
    }
}

/// Most favorite / recent projects shown without open tabs.
pub const MAX_CLOSED_SPACES: usize = 6;

/// Builds the spaces. `git` maps a repository root to its branch and dirty flag; `known` are the
/// favorite then recent projects (path, label) of the store, listed after the open spaces when
/// they have no tab (only for `GroupBy::Project`).
pub fn build_spaces(
    tabs: &[TabInfo],
    group_by: GroupBy,
    git: &HashMap<PathBuf, GitSummary>,
    known: &[(PathBuf, String)],
) -> Vec<Space> {
    let mut out: Vec<Space> = Vec::new();
    match group_by {
        GroupBy::Project | GroupBy::None => {
            // `None` still nests rows in spaces: the unified sidebar always groups by project.
            for (i, t) in tabs.iter().enumerate() {
                let key = project_key(t);
                let root = key.clone().unwrap_or_default();
                match out.iter_mut().find(|s| s.root == root) {
                    Some(s) => s.tabs.push(i),
                    None => {
                        let name = key.as_deref().map_or_else(|| "Sin proyecto".to_string(), project_name);
                        let (branch, dirty) = git
                            .iter()
                            .find(|(r, _)| key.as_deref().is_some_and(|k| same_path(r, k)))
                            .map_or((None, false), |(_, (b, d))| (Some(b.clone()), *d));
                        out.push(Space { root, name, tabs: vec![i], branch, dirty, status: None, headless: key.is_none() });
                    },
                }
            }
            for s in &mut out {
                s.status = aggregate_status(s.tabs.iter().map(|i| tabs[*i].status));
            }
            for (path, label) in known {
                if out.iter().any(|s| same_path(&s.root, path)) {
                    continue;
                }
                let name = if label.trim().is_empty() { project_name(path) } else { label.clone() };
                out.push(Space { root: path.clone(), name, tabs: Vec::new(), branch: None, dirty: false, status: None, headless: false });
            }
            // Project-less tabs go last, after the favorites.
            out.sort_by_key(|s| s.headless);
            // Closed projects are capped, open ones never are.
            let mut closed = 0;
            out.retain(|s| {
                if s.is_open() {
                    return true;
                }
                closed += 1;
                closed <= MAX_CLOSED_SPACES
            });
        },
        GroupBy::Status | GroupBy::Agent => {
            for g in group_tabs(tabs, group_by) {
                let name = g.label.as_ref().map(ToString::to_string).unwrap_or_default();
                let status = aggregate_status(g.tabs.iter().map(|i| tabs[*i].status));
                out.push(Space { root: PathBuf::new(), name, tabs: g.tabs, branch: None, dirty: false, status, headless: false });
            }
        },
    }
    out
}

/// Separator- and case-insensitive path equality.
fn same_path(a: &Path, b: &Path) -> bool {
    crate::paths::is_within(a, b) && crate::paths::is_within(b, a)
}

/// Label of a status in the second line of a row.
pub fn status_label(status: RowStatus) -> &'static str {
    match status {
        RowStatus::Working => "Working",
        RowStatus::Waiting => "Waiting",
        RowStatus::Done => "Done",
        RowStatus::Error => "Error",
    }
}

/// Second line of a tab row: the model / branch (`row_model`) and the live state, joined by ` · `
/// (`Opus 5.5 · Working`). `Done` is already the check mark, so it adds no text.
pub fn row_subtitle(info: &TabInfo, row_model: bool) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if row_model && let Some(s) = &info.subtitle {
        parts.push(s.to_string());
    }
    if let Some(st) = info.status
        && st != RowStatus::Done
    {
        parts.push(status_label(st).to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Elapsed time of a row (`ahora`, `5m`...), when the layout shows it.
pub fn row_time(info: &TabInfo, row_time: bool, now: Instant) -> Option<String> {
    if !row_time {
        return None;
    }
    info.last_activity.map(|t| format_elapsed(now.saturating_duration_since(t).as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use panel_api::{Icon, TabIcon};

    fn tab(cwd: &str, status: Option<RowStatus>, subtitle: Option<&str>) -> TabInfo {
        TabInfo {
            id: gpui::EntityId::from(1u64),
            title: "t".into(),
            icon: TabIcon::Kind(Icon::Terminal),
            subtitle: subtitle.map(|s| s.to_string().into()),
            status,
            last_activity: None,
            cwd: Some(PathBuf::from(cwd)),
            project_root: None,
            agent: None,
        }
    }

    #[test]
    fn open_tabs_nest_under_their_project_in_first_use_order() {
        let tabs = [tab("C:/dev/b", None, None), tab("C:/dev/a", Some(RowStatus::Working), None), tab("C:/dev/b", Some(RowStatus::Done), None)];
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &[]);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].name.as_str(), s[0].tabs.clone()), ("b", vec![0, 2]));
        assert_eq!(s[0].status, Some(RowStatus::Done));
        assert_eq!(s[1].status, Some(RowStatus::Working));
        assert!(s.iter().all(Space::is_open));
    }

    #[test]
    fn git_data_is_matched_case_insensitively() {
        let tabs = [tab("C:/dev/a", None, None)];
        let git = HashMap::from([(PathBuf::from(r"c:\DEV\a"), ("main".to_string(), true))]);
        let s = build_spaces(&tabs, GroupBy::Project, &git, &[]);
        assert_eq!(s[0].branch.as_deref(), Some("main"));
        assert!(s[0].dirty);
    }

    #[test]
    fn favorites_without_tabs_follow_and_are_not_duplicated() {
        let tabs = [tab("C:/dev/a", None, None)];
        let known = vec![
            (PathBuf::from(r"C:\dev\a"), String::new()),
            (PathBuf::from(r"C:\dev\fav"), "Favorito".to_string()),
            (PathBuf::from(r"C:\dev\recent"), String::new()),
        ];
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &known);
        assert_eq!(s.len(), 3);
        assert!(s[0].is_open() && !s[1].is_open());
        assert_eq!((s[1].name.as_str(), s[2].name.as_str()), ("Favorito", "recent"));
    }

    #[test]
    fn tabs_without_a_project_are_headless_and_last() {
        let mut loose = tab("C:/x", None, None);
        loose.cwd = None;
        let tabs = [loose, tab("C:/dev/a", None, None)];
        let known = vec![(PathBuf::from(r"C:\devav"), String::new())];
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &known);
        assert_eq!(s.len(), 3);
        assert!(!s[0].headless && !s[1].headless);
        assert!(s[2].headless && s[2].tabs == vec![0]);
    }

    #[test]
    fn closed_projects_are_capped() {
        let tabs = [tab("C:/dev/a", None, None)];
        let known: Vec<_> = (0..20).map(|i| (PathBuf::from(format!(r"C:\dev\p{i}")), String::new())).collect();
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &known);
        assert_eq!(s.iter().filter(|s| !s.is_open()).count(), MAX_CLOSED_SPACES);
        assert_eq!(s.iter().filter(|s| s.is_open()).count(), 1);
    }

    #[test]
    fn status_grouping_makes_label_spaces() {
        let tabs = [tab("C:/a", Some(RowStatus::Done), None), tab("C:/a", Some(RowStatus::Working), None)];
        let s = build_spaces(&tabs, GroupBy::Status, &HashMap::new(), &[]);
        assert_eq!(s[0].name, "Working");
        assert_eq!(s[0].key(), PathBuf::from("group:Working"));
    }

    #[test]
    fn subtitle_joins_model_and_state() {
        let t = tab("C:/a", Some(RowStatus::Working), Some("Opus 5.5"));
        assert_eq!(row_subtitle(&t, true).as_deref(), Some("Opus 5.5 · Working"));
        assert_eq!(row_subtitle(&t, false).as_deref(), Some("Working"));
        let done = tab("C:/a", Some(RowStatus::Done), Some("main"));
        assert_eq!(row_subtitle(&done, true).as_deref(), Some("main"));
        assert_eq!(row_subtitle(&done, false), None);
        assert_eq!(row_subtitle(&tab("C:/a", None, None), true), None);
    }

    #[test]
    fn time_is_optional() {
        let mut t = tab("C:/a", None, None);
        let now = Instant::now();
        assert_eq!(row_time(&t, true, now), None);
        t.last_activity = Some(now);
        assert_eq!(row_time(&t, false, now), None);
        assert_eq!(row_time(&t, true, now).as_deref(), Some("ahora"));
    }
}
