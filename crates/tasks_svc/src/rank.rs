//! Ranking by shell history: tasks the user actually runs come first.

use crate::model::TaskDef;
use std::collections::HashMap;
use store::{HistoryEntry, StateDb};

/// How many distinct history commands are considered.
const HISTORY_WINDOW: usize = 3000;

/// A task with the usage evidence behind its position.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedTask {
    pub task: TaskDef,
    pub score: f64,
    /// Times the task's command (or that command plus extra arguments) was run.
    pub runs: u32,
    /// Unix ms of the most recent run.
    pub last_run: Option<i64>,
}

/// Order `tasks` by how often and how recently their command appears in the
/// history, boosted when it was run in `cwd`. Tasks never run keep their
/// discovery order after the ones that were. Reads the history (blocking, one
/// query): background thread. A history error just returns the discovery order.
pub fn rank_tasks(tasks: Vec<TaskDef>, db: &StateDb, cwd: &str) -> Vec<RankedTask> {
    let history = match db.search("", Some(cwd), HISTORY_WINDOW) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("tasks_svc: history unavailable for ranking: {e}");
            Vec::new()
        }
    };
    rank_with_history(tasks, &history, store::now_ms())
}

pub(crate) fn rank_with_history(
    tasks: Vec<TaskDef>,
    history: &[HistoryEntry],
    now: i64,
) -> Vec<RankedTask> {
    let by_command: HashMap<&str, &HistoryEntry> =
        history.iter().map(|h| (h.command.trim(), h)).collect();

    let mut ranked: Vec<(usize, RankedTask)> = tasks
        .into_iter()
        .enumerate()
        .map(|(i, task)| {
            let cmd = task.command.trim();
            let mut runs = 0u32;
            let mut last: Option<i64> = None;
            let mut in_cwd = false;
            let mut score = 0.0;
            let mut add = |h: &HistoryEntry, weight: f64| {
                runs += h.count;
                last = last.max(Some(h.last_ts));
                in_cwd |= h.ran_in_cwd;
                let days = ((now - h.last_ts).max(0) as f64) / 86_400_000.0;
                score += weight * (1.0 + f64::from(h.count).ln() + 2.0 / (1.0 + days));
            };
            if let Some(h) = by_command.get(cmd) {
                add(h, 1.0);
            }
            // `npm run dev -- --port 3000`: same task with extra arguments.
            let prefix = format!("{cmd} ");
            for h in history {
                let c = h.command.trim();
                if c.len() > prefix.len() && c.starts_with(&prefix) {
                    add(h, 0.5);
                }
            }
            if runs > 0 && in_cwd {
                score += 1.5;
            }
            (
                i,
                RankedTask {
                    task,
                    score,
                    runs,
                    last_run: last,
                },
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then(a.0.cmp(&b.0)));
    ranked.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskKind;
    use std::path::PathBuf;
    use store::NewCommand;

    const DAY: i64 = 86_400_000;

    fn task(label: &str, command: &str) -> TaskDef {
        TaskDef {
            id: format!("npm:package.json:{label}"),
            label: label.into(),
            kind: TaskKind::Npm,
            command: command.into(),
            cwd: PathBuf::from("C:\\p"),
            source_file: PathBuf::from("C:\\p\\package.json"),
            description: None,
        }
    }

    fn labels(r: &[RankedTask]) -> Vec<&str> {
        r.iter().map(|r| r.task.label.as_str()).collect()
    }

    #[test]
    fn no_history_keeps_discovery_order() {
        let tasks = vec![task("a", "npm run a"), task("b", "npm run b")];
        let r = rank_with_history(tasks, &[], 0);
        assert_eq!(labels(&r), ["a", "b"]);
        assert!(r.iter().all(|r| r.runs == 0 && r.last_run.is_none()));
    }

    #[test]
    fn history_from_the_db_boosts_run_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        let now = store::now_ms();
        for ts in [now - 2 * DAY, now - DAY, now - 1000] {
            db.record_command(NewCommand {
                ts: Some(ts),
                ..NewCommand::new("npm run dev", "C:\\p")
            })
            .unwrap();
        }
        db.record_command(NewCommand {
            ts: Some(now - 50 * DAY),
            ..NewCommand::new("npm run build", "C:\\p")
        })
        .unwrap();
        db.flush().unwrap();
        let tasks = vec![
            task("lint", "npm run lint"),
            task("build", "npm run build"),
            task("dev", "npm run dev"),
        ];
        let r = rank_tasks(tasks, &db, "C:\\p");
        assert_eq!(labels(&r), ["dev", "build", "lint"]);
        assert_eq!(r[0].runs, 3);
        assert_eq!(r[1].runs, 1);
        assert!(r[0].last_run.unwrap() > now - 5000);
        assert_eq!(r[2].last_run, None);
    }

    #[test]
    fn recent_beats_old_at_equal_frequency() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        let now = store::now_ms();
        db.record_command(NewCommand {
            ts: Some(now - 90 * DAY),
            ..NewCommand::new("npm run old", "C:\\p")
        })
        .unwrap();
        db.record_command(NewCommand {
            ts: Some(now - 1000),
            ..NewCommand::new("npm run fresh", "C:\\p")
        })
        .unwrap();
        db.flush().unwrap();
        let r = rank_tasks(
            vec![task("old", "npm run old"), task("fresh", "npm run fresh")],
            &db,
            "C:\\p",
        );
        assert_eq!(labels(&r), ["fresh", "old"]);
    }

    #[test]
    fn same_cwd_boosts() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        let now = store::now_ms();
        for (cmd, cwd) in [("npm run a", "C:\\other"), ("npm run b", "C:\\p")] {
            db.record_command(NewCommand {
                ts: Some(now - 1000),
                ..NewCommand::new(cmd, cwd)
            })
            .unwrap();
        }
        db.flush().unwrap();
        let r = rank_tasks(
            vec![task("a", "npm run a"), task("b", "npm run b")],
            &db,
            "C:\\p",
        );
        assert_eq!(labels(&r), ["b", "a"]);
    }

    #[test]
    fn extra_arguments_still_count() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db")).unwrap();
        db.record_command(NewCommand::new("npm run dev -- --port 3000", "C:\\p"))
            .unwrap();
        db.record_command(NewCommand::new("npm run development", "C:\\p"))
            .unwrap();
        db.flush().unwrap();
        let r = rank_tasks(
            vec![task("x", "npm run x"), task("dev", "npm run dev")],
            &db,
            "C:\\p",
        );
        assert_eq!(labels(&r), ["dev", "x"]);
        assert_eq!(r[0].runs, 1); // `npm run development` is not `npm run dev`
    }
}
