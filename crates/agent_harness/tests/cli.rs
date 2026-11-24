#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The real `selachi-agent.exe`: usage, refusals (nothing real is ever started), and the commands that
//! read a run created in-process with a scripted agent (`status`, `result`, `diff`, `apply`).
//! The `#[ignore]` smoke runs `list --json` against this machine.

mod common;

use std::path::Path;
use std::process::{Command, Output, Stdio};

use agent_harness::{DelegationConfig, Permission, RunRequest, RunStatus};
use common::*;
use serde_json::Value;

fn bin(data_dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_selachi-agent"));
    c.env("SELACHI_DATA_DIR", data_dir)
        .env_remove("SELACHI_PARENT_PERMISSION")
        .env_remove("SELACHI_DELEGATION_DEPTH")
        .env_remove("SELACHI_PARENT_THREAD")
        .env_remove("SELACHI_PARENT_TAB")
        .stdin(Stdio::null());
    c
}

fn run(mut c: Command) -> (i32, String, String) {
    let o: Output = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn help_version_and_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out, _) = run({
        let mut c = bin(dir.path());
        c.arg("--help");
        c
    });
    assert_eq!(code, 0);
    for needle in [
        "selachi-agent run",
        "SELACHI_PARENT_PERMISSION",
        "--worktree",
        "EXIT CODES",
    ] {
        assert!(out.contains(needle), "{needle} missing from help:\n{out}");
    }
    let (code, out, _) = run({
        let mut c = bin(dir.path());
        c.arg("--version");
        c
    });
    assert_eq!((code, out.starts_with("selachi-agent ")), (0, true));
    let (code, _, err) = run({
        let mut c = bin(dir.path());
        c.args(["run", "codex"]);
        c
    });
    assert_eq!(code, 2);
    assert!(err.contains("needs a prompt"), "{err}");
    let (code, _, _) = run({
        let mut c = bin(dir.path());
        c.args(["frobnicate"]);
        c
    });
    assert_eq!(code, 2);
}

#[test]
fn run_refusals_print_json_and_exit_3_without_starting_anything() {
    let dir = tempfile::tempdir().unwrap();
    let check = |args: &[&str], code_name: &str| {
        let mut c = bin(dir.path());
        c.args(["run"]).args(args).arg("--json");
        let (code, out, _) = run(c);
        assert_eq!(code, 3, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "refused");
        assert!(v["run_id"].is_null());
        assert_eq!(v["code"], code_name, "{v}");
        assert!(!v["reason"].as_str().unwrap().is_empty());
    };
    // no SELACHI_PARENT_PERMISSION: the calling session is read-only
    check(
        &["codex", "x", "--permission", "accept-edits"],
        "permission_exceeds_parent",
    );
    check(
        &["codex", "x", "--permission", "ask"],
        "permission_exceeds_parent",
    );
    check(&["codex", "x", "--permission", "full"], "permission_full");
    // a depth-2 session cannot delegate any more
    let mut c = bin(dir.path());
    c.env("SELACHI_DELEGATION_DEPTH", "2")
        .args(["run", "codex", "x", "--json"]);
    let (code, out, _) = run(c);
    assert_eq!(code, 3);
    assert!(out.contains("depth_exceeded"), "{out}");
    // text output carries the reason too
    let mut c = bin(dir.path());
    c.args(["run", "codex", "x", "--permission", "ask"]);
    let (code, out, _) = run(c);
    assert_eq!(code, 3);
    assert!(
        out.contains("refused") && out.contains("SELACHI_PARENT_PERMISSION"),
        "{out}"
    );
}

#[test]
fn unknown_run_ids_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["status", "result", "cancel", "diff", "apply"] {
        let mut c = bin(dir.path());
        c.args([sub, "4242"]);
        let (code, _, err) = run(c);
        assert_eq!(code, 1, "{sub}");
        assert!(err.contains("no run 4242"), "{sub}: {err}");
    }
    let mut c = bin(dir.path());
    c.args(["status", "..\\evil"]);
    let (_, _, err) = run(c);
    assert!(err.contains("not a run id"), "{err}");
}

/// A finished worktree run created in-process; returns (setup, repo, run id).
fn finished_run(files: &[(&str, &str)]) -> (Setup, std::path::PathBuf, String) {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let s = setup(move |cfg, _, _| {
        for (path, body) in &files {
            let p = cfg.cwd.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        vec![working(), text("edited"), usage(10, 20, 0, 0), done()]
    });
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::AcceptEdits), DelegationConfig::default());
    let mut req = RunRequest::new("codex", "edit things", &repo);
    req.permission = Permission::AcceptEdits;
    req.worktree = true;
    let r = h.run(req);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let id = r.run_id.unwrap();
    drop(h);
    (s, repo, id)
}

#[test]
fn status_result_and_diff_through_the_binary() {
    let (s, _repo, id) = finished_run(&[("a.txt", "alpha\nbeta\n")]);
    let data = s.data_dir();

    let (code, out, _) = run({
        let mut c = bin(&data);
        c.args(["status", &id, "--json"]);
        c
    });
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (
            v["status"].as_str(),
            v["agent"].as_str(),
            v["applied"].as_bool()
        ),
        (Some("done"), Some("codex"), Some(false))
    );

    let (code, out, _) = run({
        let mut c = bin(&data);
        c.args(["result", &id, "--json"]);
        c
    });
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["run_id"], id.as_str());
    assert_eq!(v["files_changed"], serde_json::json!(["a.txt"]));
    assert_eq!(v["usage"]["total"], 30);
    let (code, text_out, _) = run({
        let mut c = bin(&data);
        c.args(["result", &id]);
        c
    });
    assert_eq!(code, 0);
    assert!(
        text_out.contains(&format!("run {id} · done")) && text_out.contains("a.txt"),
        "{text_out}"
    );

    let (code, out, _) = run({
        let mut c = bin(&data);
        c.args(["diff", &id]);
        c
    });
    assert_eq!(code, 0);
    assert!(out.contains("+beta"), "{out}");
    let (_, out, _) = run({
        let mut c = bin(&data);
        c.args(["diff", &id, "--stat"]);
        c
    });
    assert!(out.contains("1 file changed"), "{out}");
}

#[test]
fn apply_dry_run_prints_the_plan_and_changes_nothing() {
    let (s, repo, id) = finished_run(&[("a.txt", "alpha\nbeta\n"), ("c.txt", "c\n")]);
    let (code, out, _) = run({
        let mut c = bin(&s.data_dir());
        c.args(["apply", &id, "--dry-run", "--json"]);
        c
    });
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["can_apply"], true);
    assert_eq!(v["requires_confirmation"], true);
    assert_eq!(v["risk"], "normal");
    assert_eq!(v["files"].as_array().unwrap().len(), 2);
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(!repo.join("c.txt").exists());

    let (code, out, _) = run({
        let mut c = bin(&s.data_dir());
        c.args(["apply", &id, "--dry-run"]);
        c
    });
    assert_eq!(code, 0);
    assert!(out.contains("dry run") && out.contains("c.txt"), "{out}");
}

#[test]
fn apply_needs_a_terminal_or_yes_and_never_applies_silently() {
    let (s, repo, id) = finished_run(&[("a.txt", "alpha\nbeta\n")]);
    // stdin is not a terminal and there is no --yes
    let (code, _, err) = run({
        let mut c = bin(&s.data_dir());
        c.args(["apply", &id]);
        c
    });
    assert_eq!(code, 1);
    assert!(err.contains("needs confirmation"), "{err}");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\n"
    );

    // --yes is fine for a normal-risk plan
    let (code, out, err) = run({
        let mut c = bin(&s.data_dir());
        c.args(["apply", &id, "--yes"]);
        c
    });
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    let (_, _, err) = run({
        let mut c = bin(&s.data_dir());
        c.args(["apply", &id, "--yes"]);
        c
    });
    assert!(err.contains("already applied"), "{err}");
}

#[test]
fn apply_yes_is_refused_for_full_risk_changes() {
    let (s, repo, id) = finished_run(&[(".github/workflows/ci.yml", "on: push\n")]);
    let (code, out, _) = run({
        let mut c = bin(&s.data_dir());
        c.args(["apply", &id, "--dry-run", "--json"]);
        c
    });
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["risk"], "full");
    let (code, _, err) = run({
        let mut c = bin(&s.data_dir());
        c.args(["apply", &id, "--yes"]);
        c
    });
    assert_eq!(code, 1);
    assert!(
        err.contains("--yes is not allowed for a full-risk apply"),
        "{err}"
    );
    assert!(!repo.join(".github").exists());
}

/// Real machine: detects the installed agents without starting any of them (no prompt, no handshake).
/// `cargo test -p agent_harness -- --ignored`
#[test]
#[ignore]
fn smoke_list_json_on_this_machine() {
    let o = Command::new(env!("CARGO_BIN_EXE_selachi-agent"))
        .args(["list", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{out}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    let agents = v["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 5, "claude, codex, opencode, gemini, agy");
    eprintln!("{}", serde_json::to_string_pretty(&v).unwrap());
    for a in agents {
        assert!(a["name"].is_string() && a["installed"].is_boolean());
        if a["installed"] == true {
            assert!(Path::new(a["path"].as_str().unwrap()).is_file());
        }
    }
    assert_eq!(v["delegation"]["max_concurrent"], 3);
    assert!(v["delegation"]["caller_permission"].is_string());
}
