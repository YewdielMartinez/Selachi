//! Groups tabs for the sidebar (`group_by`), and derives projects / worktrees from tab data.
//! Pure over [`TabInfo`], so it is unit tested.

use std::path::{Path, PathBuf};

use gpui::SharedString;
use panel_api::{RowStatus, TabInfo};
use store::GroupBy;

use crate::paths;

/// One block of rows under an optional header.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// `None` = ungrouped (no header).
    pub label: Option<SharedString>,
    /// Indices into the tab list, in display order.
    pub tabs: Vec<usize>,
}

/// Folder name of a project path (`C:\Users\me\dev\selachi` -> `selachi`).
pub fn project_name(path: &Path) -> String {
    paths::tab_label(path)
}

/// The directory that identifies a tab's project: its git root, else its cwd.
pub fn project_key(tab: &TabInfo) -> Option<PathBuf> {
    tab.project_root.clone().or_else(|| tab.cwd.clone())
}

/// Status bucket label of the `status` grouping.
fn status_bucket(status: Option<RowStatus>) -> (u8, &'static str) {
    match status {
        Some(RowStatus::Working) => (0, "Working"),
        Some(RowStatus::Waiting) => (1, "Waiting"),
        Some(RowStatus::Done | RowStatus::Error) | None => (2, "Settled"),
    }
}

/// Splits `tabs` into groups. Within a group tabs keep their original order; `project` and
/// `agent` groups appear in order of first use, `status` groups in the fixed order
/// Working / Waiting / Settled (empty buckets are omitted).
pub fn group_tabs(tabs: &[TabInfo], by: GroupBy) -> Vec<Group> {
    match by {
        GroupBy::None => vec![Group {
            label: None,
            tabs: (0..tabs.len()).collect(),
        }],
        GroupBy::Status => {
            let mut buckets: [Vec<usize>; 3] = Default::default();
            for (i, t) in tabs.iter().enumerate() {
                buckets[usize::from(status_bucket(t.status).0)].push(i);
            }
            ["Working", "Waiting", "Settled"]
                .into_iter()
                .zip(buckets)
                .filter(|(_, v)| !v.is_empty())
                .map(|(label, tabs)| Group {
                    label: Some(label.into()),
                    tabs,
                })
                .collect()
        }
        GroupBy::Project => group_by_key(tabs, |t| match project_key(t) {
            Some(p) => project_name(&p),
            None => "Sin proyecto".to_string(),
        }),
        GroupBy::Agent => group_by_key(tabs, |t| {
            t.agent
                .as_ref()
                .map_or_else(|| "Terminal".to_string(), ToString::to_string)
        }),
    }
}

fn group_by_key(tabs: &[TabInfo], key: impl Fn(&TabInfo) -> String) -> Vec<Group> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, t) in tabs.iter().enumerate() {
        let k = key(t);
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(i),
            None => groups.push((k, vec![i])),
        }
    }
    groups
        .into_iter()
        .map(|(k, tabs)| Group {
            label: Some(k.into()),
            tabs,
        })
        .collect()
}

/// A distinct project among the open tabs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub root: PathBuf,
    pub name: String,
    /// Number of tabs in it.
    pub tabs: usize,
    /// Index of its first tab.
    pub first_tab: usize,
}

/// Distinct projects, in order of first use.
pub fn projects(tabs: &[TabInfo]) -> Vec<ProjectEntry> {
    let mut out: Vec<ProjectEntry> = Vec::new();
    for (i, t) in tabs.iter().enumerate() {
        let Some(root) = project_key(t) else { continue };
        match out.iter_mut().find(|p| p.root == root) {
            Some(p) => p.tabs += 1,
            None => out.push(ProjectEntry {
                name: project_name(&root),
                root,
                tabs: 1,
                first_tab: i,
            }),
        }
    }
    out
}

/// The most urgent status of a set of tabs: Working > Waiting > Error > Done.
pub fn aggregate_status(tabs: impl IntoIterator<Item = Option<RowStatus>>) -> Option<RowStatus> {
    let rank = |s: RowStatus| match s {
        RowStatus::Working => 3,
        RowStatus::Waiting => 2,
        RowStatus::Error => 1,
        RowStatus::Done => 0,
    };
    tabs.into_iter().flatten().max_by_key(|s| rank(*s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lbl(g: &Group) -> Option<&str> {
        g.label.as_ref().map(AsRef::as_ref)
    }

    fn tab(cwd: &str, status: Option<RowStatus>, agent: Option<&str>) -> TabInfo {
        TabInfo {
            id: gpui::EntityId::from(1u64),
            title: "t".into(),
            icon: panel_api::TabIcon::Kind(panel_api::Icon::Terminal),
            subtitle: None,
            status,
            last_activity: None,
            cwd: Some(PathBuf::from(cwd)),
            project_root: None,
            agent: agent.map(|a| SharedString::from(a.to_string())),
        }
    }

    #[test]
    fn none_is_one_headerless_group() {
        let tabs = [tab("C:/a", None, None), tab("C:/b", None, None)];
        let g = group_tabs(&tabs, GroupBy::None);
        assert_eq!(
            g,
            vec![Group {
                label: None,
                tabs: vec![0, 1]
            }]
        );
    }

    #[test]
    fn project_groups_by_folder_in_first_use_order() {
        let tabs = [
            tab("C:/dev/b", None, None),
            tab("C:/dev/a", None, None),
            tab("C:/dev/b", None, None),
        ];
        let g = group_tabs(&tabs, GroupBy::Project);
        assert_eq!(g.len(), 2);
        assert_eq!(lbl(&g[0]), Some("b"));
        assert_eq!(g[0].tabs, vec![0, 2]);
        assert_eq!(g[1].tabs, vec![1]);
    }

    #[test]
    fn status_groups_have_a_fixed_order_and_skip_empty_buckets() {
        let tabs = [
            tab("C:/a", Some(RowStatus::Done), None),
            tab("C:/a", Some(RowStatus::Working), None),
            tab("C:/a", None, None),
        ];
        let g = group_tabs(&tabs, GroupBy::Status);
        let labels: Vec<_> = g.iter().map(|g| lbl(g).unwrap().to_string()).collect();
        assert_eq!(labels, ["Working", "Settled"]);
        assert_eq!(g[0].tabs, vec![1]);
        assert_eq!(g[1].tabs, vec![0, 2]);
    }

    #[test]
    fn agent_groups_fall_back_to_terminal() {
        let tabs = [tab("C:/a", None, Some("claude")), tab("C:/a", None, None)];
        let g = group_tabs(&tabs, GroupBy::Agent);
        assert_eq!(lbl(&g[0]), Some("claude"));
        assert_eq!(lbl(&g[1]), Some("Terminal"));
    }

    #[test]
    fn projects_are_distinct_and_counted() {
        let tabs = [
            tab("C:/dev/a", None, None),
            tab("C:/dev/b", None, None),
            tab("C:/dev/a", None, None),
        ];
        let p = projects(&tabs);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].name.as_str(), p[0].tabs, p[0].first_tab), ("a", 2, 0));
    }

    #[test]
    fn aggregate_picks_the_most_urgent() {
        use RowStatus::*;
        assert_eq!(
            aggregate_status([Some(Done), Some(Working), None]),
            Some(Working)
        );
        assert_eq!(aggregate_status([Some(Done), Some(Error)]), Some(Error));
        assert_eq!(aggregate_status([None, None]), None);
    }
}
