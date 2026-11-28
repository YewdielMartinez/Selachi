//! The "spaces" of the unified sidebar (docs/olas/ola-5.md 5.10): a space is a project (or
//! worktree) and the tabs that live in it, plus the favorite / recent projects that have no tab
//! open yet. Pure over [`TabInfo`], so it is unit tested; the shell adds the git data.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use panel_api::{RowStatus, TabInfo, format_elapsed};
use store::GroupBy;

use crate::grouping::{aggregate_status, group_tabs, project_key, project_name};
use crate::paths::same_path;
use crate::projects::Known;

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
    /// The user added this project by hand (it stays in the list without tabs).
    pub explicit: bool,
    /// Its folder is gone (the shell checks; `build_spaces` leaves it `false`).
    pub missing: bool,
}

impl Space {
    /// Has at least one open tab.
    pub fn is_open(&self) -> bool {
        !self.tabs.is_empty()
    }

    /// A stable key for the collapsed set (the root, or the label for non-project groups).
    pub fn key(&self) -> PathBuf {
        if self.root.as_os_str().is_empty() {
            PathBuf::from(format!("group:{}", self.name))
        } else {
            self.root.clone()
        }
    }
}

/// Most *recent* projects shown without open tabs. The projects the user added are never trimmed.
pub const MAX_CLOSED_SPACES: usize = 6;

/// Builds the spaces. `git` maps a repository root to its branch and dirty flag; `known` are the
/// projects the user added, then the recent ones, listed after the open spaces when they have no
/// tab (only for `GroupBy::Project`).
pub fn build_spaces(
    tabs: &[TabInfo],
    group_by: GroupBy,
    git: &HashMap<PathBuf, GitSummary>,
    known: &[Known],
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
                        let name = key
                            .as_deref()
                            .map_or_else(|| "Sin proyecto".to_string(), project_name);
                        let (branch, dirty) = git
                            .iter()
                            .find(|(r, _)| key.as_deref().is_some_and(|k| same_path(r, k)))
                            .map_or((None, false), |(_, (b, d))| (Some(b.clone()), *d));
                        out.push(Space {
                            root,
                            name,
                            tabs: vec![i],
                            branch,
                            dirty,
                            status: None,
                            headless: key.is_none(),
                            explicit: false,
                            missing: false,
                        });
                    }
                }
            }
            for s in &mut out {
                s.status = aggregate_status(s.tabs.iter().map(|i| tabs[*i].status));
            }
            for k in known {
                if let Some(open) = out
                    .iter_mut()
                    .find(|s| !s.headless && same_path(&s.root, &k.path))
                {
                    // An open project the user added keeps its label and stays when its tabs close.
                    open.explicit |= k.explicit;
                    if k.explicit && !k.label.trim().is_empty() {
                        open.name.clone_from(&k.label);
                    }
                    continue;
                }
                let name = if k.label.trim().is_empty() {
                    project_name(&k.path)
                } else {
                    k.label.clone()
                };
                out.push(Space {
                    root: k.path.clone(),
                    name,
                    tabs: Vec::new(),
                    branch: None,
                    dirty: false,
                    status: None,
                    headless: false,
                    explicit: k.explicit,
                    missing: false,
                });
            }
            // Project-less tabs go last, after the favorites.
            out.sort_by_key(|s| s.headless);
            // Recent projects without tabs are capped; open ones and the ones the user added
            // never are.
            let mut closed = 0;
            out.retain(|s| {
                if s.is_open() || s.explicit {
                    return true;
                }
                closed += 1;
                closed <= MAX_CLOSED_SPACES
            });
        }
        GroupBy::Status | GroupBy::Agent => {
            for g in group_tabs(tabs, group_by) {
                let name = g
                    .label
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let status = aggregate_status(g.tabs.iter().map(|i| tabs[*i].status));
                out.push(Space {
                    root: PathBuf::new(),
                    name,
                    tabs: g.tabs,
                    branch: None,
                    dirty: false,
                    status,
                    headless: false,
                    explicit: false,
                    missing: false,
                });
            }
        }
    }
    out
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
    info.last_activity
        .map(|t| format_elapsed(now.saturating_duration_since(t).as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use panel_api::{Icon, TabIcon};

    fn known(path: &str, label: &str, explicit: bool) -> Known {
        Known::new(PathBuf::from(path), label.to_string(), explicit)
    }

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
        let tabs = [
            tab("C:/dev/b", None, None),
            tab("C:/dev/a", Some(RowStatus::Working), None),
            tab("C:/dev/b", Some(RowStatus::Done), None),
        ];
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
            known(r"C:\dev\a", "", true),
            known(r"C:\dev\fav", "Favorito", true),
            known(r"C:\dev\recent", "", false),
        ];
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &known);
        assert_eq!(s.len(), 3);
        assert!(s[0].is_open() && !s[1].is_open());
        assert_eq!(
            (s[1].name.as_str(), s[2].name.as_str()),
            ("Favorito", "recent")
        );
    }

    #[test]
    fn tabs_without_a_project_are_headless_and_last() {
        let mut loose = tab("C:/x", None, None);
        loose.cwd = None;
        let tabs = [loose, tab("C:/dev/a", None, None)];
        let known = vec![known(r"C:\dev\fav", "", false)];
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &known);
        assert_eq!(s.len(), 3);
        assert!(!s[0].headless && !s[1].headless);
        assert!(s[2].headless && s[2].tabs == vec![0]);
    }

    #[test]
    fn closed_projects_are_capped() {
        let tabs = [tab("C:/dev/a", None, None)];
        let known: Vec<_> = (0..20)
            .map(|i| known(&format!(r"C:\dev\p{i}"), "", false))
            .collect();
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &known);
        assert_eq!(s.iter().filter(|s| !s.is_open()).count(), MAX_CLOSED_SPACES);
        assert_eq!(s.iter().filter(|s| s.is_open()).count(), 1);
    }

    #[test]
    fn projects_the_user_added_are_never_trimmed_and_come_before_the_recents() {
        let tabs = [tab("C:/dev/a", None, None)];
        let mut list: Vec<_> = (0..20)
            .map(|i| known(&format!(r"C:\dev\mine{i}"), "", true))
            .collect();
        list.extend((0..20).map(|i| known(&format!(r"C:\dev\recent{i}"), "", false)));
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &list);
        assert_eq!(s.iter().filter(|s| s.explicit).count(), 20);
        assert_eq!(
            s.iter().filter(|s| !s.is_open() && !s.explicit).count(),
            MAX_CLOSED_SPACES
        );
        assert!(s[1].explicit && s[20].explicit && !s[21].explicit);
    }

    #[test]
    fn an_added_project_with_tabs_is_one_space_that_keeps_its_label() {
        let tabs = [tab("C:/dev/a", None, None)];
        let list = vec![known(r"c:\DEV\a", "Mi app", true)];
        let s = build_spaces(&tabs, GroupBy::Project, &HashMap::new(), &list);
        assert_eq!(s.len(), 1);
        assert!(s[0].is_open() && s[0].explicit);
        assert_eq!(s[0].name, "Mi app");
    }

    #[test]
    fn status_grouping_makes_label_spaces() {
        let tabs = [
            tab("C:/a", Some(RowStatus::Done), None),
            tab("C:/a", Some(RowStatus::Working), None),
        ];
        let s = build_spaces(&tabs, GroupBy::Status, &HashMap::new(), &[]);
        assert_eq!(s[0].name, "Working");
        assert_eq!(s[0].key(), PathBuf::from("group:Working"));
    }

    #[test]
    fn subtitle_joins_model_and_state() {
        let t = tab("C:/a", Some(RowStatus::Working), Some("Opus 5.5"));
        assert_eq!(
            row_subtitle(&t, true).as_deref(),
            Some("Opus 5.5 · Working")
        );
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
