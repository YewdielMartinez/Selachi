//! Command history with "smart" search: FTS5 token-prefix matching, results
//! grouped by command text and ranked by recency + frequency + same-cwd boost.
//!
//! Privacy: commands that look like they carry secrets are never stored (see
//! [`looks_sensitive`]), nor are commands typed with a leading space.

use super::{now_ms, StateDb};
use crate::error::Result;
use crate::paths::{clean_display, normalize_key};
use rusqlite::params;

/// Longest command we store (longer ones are pasted blobs, not commands).
const MAX_COMMAND_LEN: usize = 4096;
/// Search considers at most this many of the most recent matching rows. This
/// bounds the cost on huge histories; older matches are still in the DB.
const MAX_CANDIDATES: i64 = 20_000;

/// A command to record.
#[derive(Debug, Clone, Default)]
pub struct NewCommand {
    pub command: String,
    pub cwd: String,
    /// Unix ms; `None` = now.
    pub ts: Option<i64>,
    /// Known when the command is recorded after it finished; otherwise call
    /// [`StateDb::finish_command`] later.
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub shell: Option<String>,
    pub session_id: Option<String>,
}

impl NewCommand {
    pub fn new(command: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            cwd: cwd.into(),
            ..Self::default()
        }
    }
}

/// One distinct command in the search results (aggregated over all its runs).
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub command: String,
    /// Times it was run (within the searched window).
    pub count: u32,
    /// Unix ms of the most recent run, and the details of that run.
    pub last_ts: i64,
    pub last_cwd: String,
    pub last_exit_code: Option<i32>,
    pub last_duration_ms: Option<i64>,
    pub shell: Option<String>,
    /// It was run at least once in the cwd passed to `search`.
    pub ran_in_cwd: bool,
    pub score: f64,
}

impl StateDb {
    /// Queue a command for the history. Returns immediately. Sensitive-looking,
    /// empty, space-prefixed and over-long commands are silently skipped.
    pub fn record_command(&self, cmd: NewCommand) -> Result<()> {
        if !should_record(&cmd.command) {
            return Ok(());
        }
        let ts = cmd.ts.unwrap_or_else(now_ms);
        let cwd = clean_display(&cmd.cwd);
        let cwd_key = normalize_key(&cwd);
        let command = cmd.command.trim_end().to_string();
        self.exec_async("record_command", move |conn| {
            conn.prepare_cached(
                "INSERT INTO command_history(ts, cwd, cwd_key, command, exit_code, duration_ms, shell, session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![
                ts,
                cwd,
                cwd_key,
                command,
                cmd.exit_code,
                cmd.duration_ms,
                cmd.shell,
                cmd.session_id
            ])?;
            Ok(())
        })
    }

    /// Attach the result to the newest still-running command of `session_id`
    /// (one foreground command per shell session, so no row id is needed and
    /// several Selachi processes can share the DB safely).
    pub fn finish_command(&self, session_id: &str, exit_code: i32, duration_ms: i64) -> Result<()> {
        let session_id = session_id.to_string();
        self.exec_async("finish_command", move |conn| {
            conn.prepare_cached(
                "UPDATE command_history SET exit_code = ?1, duration_ms = ?2
                 WHERE id = (SELECT max(id) FROM command_history
                             WHERE session_id = ?3 AND exit_code IS NULL)",
            )?
            .execute(params![exit_code, duration_ms, session_id])?;
            Ok(())
        })
    }

    /// Search the history. `query` words are matched as token prefixes, all of
    /// them required (`"git ch"` finds `git checkout main`); an empty query lists
    /// everything. Results are distinct commands ranked by recency, frequency and
    /// a boost when the command was run in `cwd` (a boost, not a filter), with a
    /// small penalty for commands that mostly fail.
    pub fn search(&self, query: &str, cwd: Option<&str>, limit: usize) -> Result<Vec<HistoryEntry>> {
        let cwd_key = cwd.map(|c| normalize_key(&clean_display(c))).unwrap_or_default();
        let query_lc = query.trim().to_lowercase();
        let fts = fts_query(&query_lc);
        let like = format!("%{}%", escape_like(&query_lc));

        let candidates = if query_lc.is_empty() {
            "SELECT id FROM command_history ORDER BY id DESC LIMIT ?2"
        } else if fts.is_some() {
            "SELECT rowid FROM command_fts WHERE command_fts MATCH ?3 ORDER BY rowid DESC LIMIT ?2"
        } else {
            "SELECT id FROM command_history WHERE command LIKE ?3 ESCAPE '\\' ORDER BY id DESC LIMIT ?2"
        };
        let sql = format!(
            "SELECT command, count(*), max(ts), cwd, exit_code, duration_ms, shell,
                    sum(cwd_key = ?1), sum(exit_code IS NOT NULL AND exit_code <> 0), sum(exit_code IS NOT NULL)
             FROM command_history WHERE id IN ({candidates}) GROUP BY command"
        );
        let match_arg = fts.unwrap_or(like);
        let now = now_ms();

        let mut entries = self.read(|conn| {
            let mut stmt = conn.prepare_cached(&sql)?;
            let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<HistoryEntry> {
                let count: i64 = r.get(1)?;
                let same: i64 = r.get(7)?;
                let failed: i64 = r.get(8)?;
                let finished: i64 = r.get(9)?;
                Ok(HistoryEntry {
                    command: r.get(0)?,
                    count: count.max(0) as u32,
                    last_ts: r.get(2)?,
                    last_cwd: r.get(3)?,
                    last_exit_code: r.get(4)?,
                    last_duration_ms: r.get(5)?,
                    shell: r.get(6)?,
                    ran_in_cwd: same > 0,
                    score: score(
                        now,
                        r.get::<_, i64>(2)?,
                        count,
                        same > 0,
                        if finished > 0 { failed as f64 / finished as f64 } else { 0.0 },
                    ),
                })
            };
            let rows = if query_lc.is_empty() {
                stmt.query_map(params![cwd_key, MAX_CANDIDATES], map_row)?
            } else {
                stmt.query_map(params![cwd_key, MAX_CANDIDATES, match_arg], map_row)?
            };
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
        })?;

        if !query_lc.is_empty() {
            for e in &mut entries {
                if e.command.to_lowercase().starts_with(&query_lc) {
                    e.score += PREFIX_BONUS;
                }
            }
        }
        entries.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.last_ts.cmp(&a.last_ts))
                .then_with(|| a.command.cmp(&b.command))
        });
        entries.truncate(limit);
        Ok(entries)
    }
}

const HALF_LIFE_DAYS: f64 = 7.0;
const W_RECENCY: f64 = 1.0;
const W_FREQUENCY: f64 = 0.7;
const W_CWD: f64 = 0.9;
const PREFIX_BONUS: f64 = 0.5;
const FAIL_PENALTY: f64 = 0.35;

fn score(now: i64, last_ts: i64, count: i64, in_cwd: bool, fail_ratio: f64) -> f64 {
    let age_days = ((now - last_ts).max(0) as f64) / 86_400_000.0;
    let recency = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
    // ln(1+n)/ln(33): 1 run = 0.2, 32 runs = 1.0 (capped).
    let frequency = (((1 + count.max(0)) as f64).ln() / 33f64.ln()).min(1.0);
    let cwd = if in_cwd { 1.0 } else { 0.0 };
    (W_RECENCY * recency + W_FREQUENCY * frequency + W_CWD * cwd) * (1.0 - FAIL_PENALTY * fail_ratio)
}

/// `"git"* "ch"*` from the alphanumeric words of `q`; `None` if there are none.
fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn should_record(command: &str) -> bool {
    !command.trim().is_empty()
        && !command.starts_with(' ')
        && command.len() <= MAX_COMMAND_LEN
        && !looks_sensitive(command)
}

/// Heuristic secret detector. Errs on the side of not storing: a missing history
/// entry is cheap, a leaked token is not.
pub fn looks_sensitive(command: &str) -> bool {
    let lc = command.to_lowercase();
    const KEYWORDS: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "api-key",
        "access_key",
        "private_key",
        "credential",
    ];
    for kw in KEYWORDS {
        let mut from = 0;
        while let Some(i) = lc[from..].find(kw) {
            let start = from + i;
            let end = start + kw.len();
            // `--password ...` / `-token ...` flag form.
            if lc[..start].ends_with("--") || lc[..start].ends_with(" -") {
                return true;
            }
            // `PASSWORD=...`, `$env:X_TOKEN = "..."`, `secret: ...`
            let after = lc[end..].trim_start_matches(|c: char| c == '"' || c == '\'' || c == ' ');
            if after.starts_with('=') || (after.starts_with(':') && !after.starts_with("::")) {
                return true;
            }
            from = end;
        }
    }
    if lc.contains("authorization:") || lc.contains("bearer ") || lc.contains("-asplaintext")
        || lc.contains("begin rsa private") || lc.contains("begin openssh private") {
        return true;
    }
    // Well-known token shapes: prefix followed by a long run of token characters.
    const PREFIXES: &[&str] = &["ghp_", "gho_", "ghs_", "github_pat_", "sk-ant-", "sk-", "xoxb-", "xoxp-", "akia"];
    for p in PREFIXES {
        let mut from = 0;
        while let Some(i) = lc[from..].find(p) {
            let end = from + i + p.len();
            let run = lc[end..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .count();
            if run >= 16 {
                return true;
            }
            from = end;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::temp_db;

    const DAY: i64 = 86_400_000;

    fn cmd(command: &str, cwd: &str, ts: i64) -> NewCommand {
        NewCommand {
            ts: Some(ts),
            ..NewCommand::new(command, cwd)
        }
    }

    #[test]
    fn sensitive_commands_detected() {
        for s in [
            "curl -H 'Authorization: Bearer abc' x",
            "$env:GITHUB_TOKEN = \"abc\"",
            "set PASSWORD=hunter2",
            "mycli --password hunter2",
            "export API_KEY=abc",
            "git clone https://x:ghp_abcdefghijklmnopqrstuvwxyz0123456789@github.com/a/b",
            "echo sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "ConvertTo-SecureString \"x\" -AsPlainText -Force",
        ] {
            assert!(looks_sensitive(s), "should flag: {s}");
        }
        for s in [
            "cargo test token_parser",
            "git checkout main",
            "cd C:\\secrets-docs",
            "npm run build -- --release",
            "ls sk-short",
        ] {
            assert!(!looks_sensitive(s), "should not flag: {s}");
        }
    }

    #[test]
    fn record_skips_secrets_and_space_prefixed() {
        let (_d, db) = temp_db();
        db.record_command(NewCommand::new("export API_KEY=abc", "C:\\a")).unwrap();
        db.record_command(NewCommand::new(" ls", "C:\\a")).unwrap();
        db.record_command(NewCommand::new("   ", "C:\\a")).unwrap();
        db.record_command(NewCommand::new("ls", "C:\\a")).unwrap();
        db.flush().unwrap();
        let all = db.search("", None, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].command, "ls");
    }

    #[test]
    fn finish_command_updates_latest_open_row_of_session() {
        let (_d, db) = temp_db();
        let mut a = cmd("make", "C:\\p", 1000);
        a.session_id = Some("s1".into());
        let mut b = cmd("make test", "C:\\p", 2000);
        b.session_id = Some("s1".into());
        let mut other = cmd("ls", "C:\\p", 3000);
        other.session_id = Some("s2".into());
        db.record_command(a).unwrap();
        db.finish_command("s1", 0, 50).unwrap();
        db.record_command(b).unwrap();
        db.record_command(other).unwrap();
        db.finish_command("s1", 2, 700).unwrap();
        db.flush().unwrap();
        let r = db.search("make test", None, 5).unwrap();
        let e = r.iter().find(|e| e.command == "make test").unwrap();
        assert_eq!((e.last_exit_code, e.last_duration_ms), (Some(2), Some(700)));
        let e = db.search("make", None, 5).unwrap().into_iter().find(|e| e.command == "make").unwrap();
        assert_eq!((e.last_exit_code, e.last_duration_ms), (Some(0), Some(50)));
        let e = db.search("ls", None, 5).unwrap().into_iter().find(|e| e.command == "ls").unwrap();
        assert_eq!(e.last_exit_code, None);
    }

    #[test]
    fn fts_token_prefix_matching() {
        let (_d, db) = temp_db();
        let now = now_ms();
        for c in ["git checkout main", "git commit -m x", "cargo build --release", "ls"] {
            db.record_command(cmd(c, "C:\\p", now)).unwrap();
        }
        db.flush().unwrap();
        let names = |q: &str| -> Vec<String> {
            let mut v: Vec<String> = db.search(q, None, 10).unwrap().into_iter().map(|e| e.command).collect();
            v.sort();
            v
        };
        assert_eq!(names("git ch"), vec!["git checkout main"]);
        assert_eq!(names("git c"), vec!["git checkout main", "git commit -m x"]);
        assert_eq!(names("rel"), vec!["cargo build --release"]);
        assert_eq!(names("--release"), vec!["cargo build --release"]);
        assert_eq!(names("CHECKOUT"), vec!["git checkout main"]);
        assert_eq!(names("zzz"), Vec::<String>::new());
        assert_eq!(names("%"), Vec::<String>::new()); // punctuation-only goes through LIKE, escaped
        assert_eq!(names("").len(), 4);
    }

    #[test]
    fn deleted_rows_leave_fts() {
        let (_d, db) = temp_db();
        db.record_command(cmd("old thing", "C:\\p", 1)).unwrap();
        db.flush().unwrap();
        assert_eq!(db.search("old", None, 5).unwrap().len(), 1);
        db.exec_sync(|c| Ok(c.execute("DELETE FROM command_history", [])?)).unwrap();
        assert!(db.search("old", None, 5).unwrap().is_empty());
    }

    #[test]
    fn ranking_recency_frequency_cwd() {
        let (_d, db) = temp_db();
        let now = now_ms();

        // Recency: same shape, one fresh, one 60 days old.
        db.record_command(cmd("deploy stale", "C:\\a", now - 60 * DAY)).unwrap();
        db.record_command(cmd("deploy fresh", "C:\\a", now - 1000)).unwrap();
        db.flush().unwrap();
        let r = db.search("deploy", None, 5).unwrap();
        assert_eq!(r[0].command, "deploy fresh");

        // Frequency: both a week old, one run 10 times.
        for i in 0..10 {
            db.record_command(cmd("build often", "C:\\a", now - 7 * DAY + i)).unwrap();
        }
        db.record_command(cmd("build once", "C:\\a", now - 7 * DAY)).unwrap();
        db.flush().unwrap();
        let r = db.search("build", None, 5).unwrap();
        assert_eq!(r[0].command, "build often");
        assert_eq!(r[0].count, 10);

        // Same-cwd boost beats a somewhat fresher command from elsewhere.
        db.record_command(cmd("serve here", "C:\\proj\\web", now - 3 * DAY)).unwrap();
        db.record_command(cmd("serve there", "C:\\other", now - DAY)).unwrap();
        db.flush().unwrap();
        let r = db.search("serve", Some("c:/proj/web/"), 5).unwrap();
        assert_eq!(r[0].command, "serve here");
        assert!(r[0].ran_in_cwd && !r[1].ran_in_cwd);
        // Without a cwd the fresher one wins.
        let r = db.search("serve", None, 5).unwrap();
        assert_eq!(r[0].command, "serve there");

        // Failing commands rank below equally recent passing ones.
        let mut ok = cmd("test ok", "C:\\a", now - 500);
        ok.exit_code = Some(0);
        let mut bad = cmd("test bad", "C:\\a", now - 400);
        bad.exit_code = Some(1);
        db.record_command(ok).unwrap();
        db.record_command(bad).unwrap();
        db.flush().unwrap();
        let r = db.search("test", None, 5).unwrap();
        assert_eq!(r[0].command, "test ok");

        // Whole-command prefix beats a mid-string token match.
        db.record_command(cmd("echo cargo", "C:\\a", now - 100)).unwrap();
        db.record_command(cmd("cargo run", "C:\\a", now - 200)).unwrap();
        db.flush().unwrap();
        let r = db.search("cargo", None, 5).unwrap();
        assert_eq!(r[0].command, "cargo run");
    }

    #[test]
    fn limit_and_grouping() {
        let (_d, db) = temp_db();
        let now = now_ms();
        for i in 0..5 {
            db.record_command(cmd("same", "C:\\a", now - i)).unwrap();
            db.record_command(cmd(&format!("uniq {i}"), "C:\\a", now - i)).unwrap();
        }
        db.flush().unwrap();
        let all = db.search("", None, 100).unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(all.iter().find(|e| e.command == "same").unwrap().count, 5);
        assert_eq!(db.search("", None, 2).unwrap().len(), 2);
    }

    /// Phase-2 criterion: smart history search < 50 ms on 100k rows. Run with
    /// `cargo test -p store --release -- --nocapture perf` to see the numbers.
    #[test]
    fn perf_search_100k_rows() {
        let (_d, db) = temp_db();
        let now = now_ms();
        let verbs = [
            "git", "cargo", "npm", "dotnet", "docker", "kubectl", "pnpm", "python", "node", "rg",
        ];
        let subs = [
            "status", "checkout", "commit", "build", "test", "run", "push", "pull", "log", "diff",
            "install", "start", "clippy", "fmt", "compose", "logs", "exec", "watch", "bench", "doc",
        ];
        let args = [
            "--release", "-m wip", "main", "feature/login", "src/main.rs", "-p store", "--all",
            "--watch", "-A", "origin", "",
        ];
        let cwds: Vec<String> = (0..60).map(|i| format!("C:\\Users\\me\\code\\proj{i}")).collect();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };

        let t_ins = std::time::Instant::now();
        for i in 0..100_000u64 {
            // ~4000 distinct command strings, skewed so some are very frequent.
            let k = (next() % 4000).min(next() % 4000);
            let command = format!(
                "{} {} {} #{}",
                verbs[(k % 10) as usize],
                subs[((k / 10) % 20) as usize],
                args[((k / 200) % 11) as usize],
                k % 7
            );
            db.record_command(NewCommand {
                ts: Some(now - (next() % (90 * DAY as u64)) as i64),
                exit_code: Some(if next() % 10 == 0 { 1 } else { 0 }),
                duration_ms: Some((next() % 5000) as i64),
                shell: Some("pwsh".into()),
                session_id: Some(format!("s{}", i % 5)),
                ..NewCommand::new(command, cwds[(next() % 60) as usize].clone())
            })
            .unwrap();
        }
        db.flush().unwrap();
        let ins = t_ins.elapsed();
        let total: i64 = db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM command_history", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(total, 100_000);

        let cwd = Some("C:\\Users\\me\\code\\proj7");
        let queries = ["", "git", "git c", "cargo b", "dotnet test", "--release", "kubectl logs", "zzzz"];
        // Warm up (statement cache, page cache).
        for q in queries {
            let _ = db.search(q, cwd, 20).unwrap();
        }
        let mut worst = std::time::Duration::ZERO;
        let mut sum = std::time::Duration::ZERO;
        let mut n = 0u32;
        for q in queries {
            let mut best = std::time::Duration::MAX;
            let mut hits = 0;
            for _ in 0..5 {
                let t = std::time::Instant::now();
                let r = db.search(q, cwd, 20).unwrap();
                let el = t.elapsed();
                hits = r.len();
                best = best.min(el);
                worst = worst.max(el);
                sum += el;
                n += 1;
            }
            println!("perf: search({q:?}) -> {hits} hits, best of 5 = {best:?}");
        }
        println!(
            "perf: 100k rows inserted in {ins:?}; search avg {:?}, worst {worst:?}",
            sum / n
        );
        let limit = if cfg!(debug_assertions) {
            std::time::Duration::from_millis(500)
        } else {
            std::time::Duration::from_millis(50)
        };
        assert!(worst < limit, "worst search {worst:?} exceeds {limit:?}");
    }
}
