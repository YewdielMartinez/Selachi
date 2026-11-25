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

// ---------------------------------------------------------------------------
// Subagent profiles (PLAN 21.3)
// ---------------------------------------------------------------------------

fn json_of(out: &str) -> Value {
    serde_json::from_str(out).unwrap_or_else(|e| panic!("not JSON ({e}): {out}"))
}

#[test]
fn profiles_init_list_show_and_overwrite_protection() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let cmd = |args: &[&str]| {
        let mut c = bin(&data);
        c.arg("profiles").args(args);
        run(c)
    };
    // nothing is written automatically
    let (code, out, _) = cmd(&["list", "--json"]);
    assert_eq!(code, 0);
    assert!(json_of(&out)["profiles"].as_array().unwrap().is_empty());
    assert!(!data.join("agents").exists());

    // a dry run previews and writes nothing
    let (code, out, _) = cmd(&["init", "--dry-run"]);
    assert_eq!(code, 0);
    assert!(out.contains("Revisor de seguridad") && out.contains("nothing was written"));
    assert!(!data.join("agents").exists());

    let (code, out, err) = cmd(&["init"]);
    assert_eq!(code, 0, "{out}{err}");
    for f in ["revisor-de-seguridad", "tests", "docs", "investigador"] {
        assert!(
            data.join("agents").join(format!("{f}.toml")).is_file(),
            "{f}"
        );
    }
    // no overwrite without --force
    let (code, _, err) = cmd(&["init"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("refusing to overwrite") && err.contains("--force"),
        "{err}"
    );
    std::fs::write(
        data.join("agents").join("tests.toml"),
        "name = \"Tests\"\nagent = \"codex\"\n",
    )
    .unwrap();
    assert_eq!(cmd(&["init", "--only", "tests", "--force"]).0, 0);
    assert!(
        std::fs::read_to_string(data.join("agents").join("tests.toml"))
            .unwrap()
            .contains("accept-edits")
    );
    assert_eq!(cmd(&["init", "--only", "nope"]).0, 1);

    let (code, out, _) = cmd(&["list", "--json"]);
    assert_eq!(code, 0);
    let v = json_of(&out);
    let names: Vec<&str> = v["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Docs", "Investigador", "Revisor de seguridad", "Tests"]
    );
    let (_, text, _) = cmd(&["list"]);
    assert!(
        text.contains("NAME") && text.contains("Revisor de seguridad"),
        "{text}"
    );

    let (code, out, _) = cmd(&["show", "revisor de seguridad", "--json"]);
    assert_eq!(code, 0);
    let p = json_of(&out);
    assert_eq!(
        (p["agent"].as_str(), p["permission"].as_str()),
        (Some("claude"), Some("read-only"))
    );
    assert!(p["instructions"]
        .as_str()
        .unwrap()
        .contains("revisor de seguridad"));
    let (code, _, err) = cmd(&["show", "missing"]);
    assert_eq!(code, 1);
    assert!(err.contains("no profile named"), "{err}");
}

#[test]
fn profiles_dry_run_prints_the_final_prompt_and_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let repo = init_repo(dir.path(), "proj");
    let agents = repo.join(".selachi").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("t.toml"),
        "name = \"Tests\"\nagent = \"claude\"\npermission = \"accept-edits\"\ninstructions = \"Only tests.\"\n",
    )
    .unwrap();
    let dry = |parent: Option<&str>, extra: &[&str]| {
        let mut c = bin(&data);
        c.args(["profiles", "dry-run", "tests", "write tests", "--cwd"])
            .arg(&repo)
            .arg("--json")
            .args(extra);
        if let Some(p) = parent {
            c.env("SELACHI_PARENT_PERMISSION", p);
        }
        run(c)
    };
    let (code, out, err) = dry(None, &[]);
    assert_eq!(code, 0, "{out}{err}");
    let v = json_of(&out);
    assert_eq!(v["dry_run"], true);
    assert_eq!(
        v["limits"]["permission"], "read-only",
        "the parent is read-only: lowered"
    );
    assert_eq!(v["limits"]["profile_permission"], "accept-edits");
    assert!(v["final_prompt"]
        .as_str()
        .unwrap()
        .contains("=== SELACHI PROFILE: Tests ==="));
    assert!(v["final_prompt"]
        .as_str()
        .unwrap()
        .ends_with("=== TASK ===\nwrite tests"));
    assert_eq!(
        json_of(&dry(Some("accept-edits"), &[]).1)["limits"]["permission"],
        "accept-edits"
    );
    // full is refused even in a dry run
    let (code, out, _) = dry(Some("full"), &["--permission", "full"]);
    assert_eq!(code, 3, "{out}");
    assert_eq!(json_of(&out)["code"], "permission_full");
    // text form
    let mut c = bin(&data);
    c.args(["profiles", "dry-run", "tests", "go", "--cwd"])
        .arg(&repo);
    let (code, out, _) = run(c);
    assert_eq!(code, 0);
    assert!(
        out.contains("EFFECTIVE LIMITS")
            && out.contains("FINAL PROMPT")
            && out.contains("nothing was run"),
        "{out}"
    );
    // nothing was created
    assert!(!data.join("chat.db").exists() && !data.join("agent-runs").exists());
    // usage errors
    let mut c = bin(&data);
    c.args(["profiles", "dry-run", "tests"]);
    assert_eq!(run(c).0, 2);
    let mut c = bin(&data);
    c.args(["profiles", "bogus"]);
    assert_eq!(run(c).0, 2);
}

#[test]
fn profiles_import_claude_is_explicit_previewed_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let src = dir.path().join("claude-agents");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: Reviews code\ntools: Read, Grep\nmodel: sonnet\n---\nYou review code.\n",
    )
    .unwrap();
    let imp = |args: &[&str]| {
        let mut c = bin(&data);
        c.args(["profiles", "import-claude", "--from"])
            .arg(&src)
            .args(args);
        run(c)
    };
    // preview only
    let (code, out, _) = imp(&["--dry-run"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("name = \"code-reviewer\"")
            && out.contains("read-only")
            && out.contains("nothing was written"),
        "{out}"
    );
    assert!(!data.join("agents").exists());
    // no terminal and no --yes: it will not write
    let (code, _, err) = imp(&[]);
    assert_eq!(code, 1);
    assert!(err.contains("needs confirmation"), "{err}");
    assert!(!data.join("agents").exists());
    // confirmed
    let (code, out, err) = imp(&["--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(data.join("agents").join("code-reviewer.toml").is_file());
    // overwrite refused without --force
    let (code, _, err) = imp(&["--yes"]);
    assert_eq!(code, 1);
    assert!(err.contains("refusing to overwrite"), "{err}");
    assert_eq!(imp(&["--yes", "--force"]).0, 0);
    // the imported profile is usable
    let mut c = bin(&data);
    c.args(["profiles", "show", "code-reviewer", "--json"]);
    let (code, out, _) = run(c);
    assert_eq!(code, 0);
    let p = json_of(&out);
    assert_eq!(
        (
            p["agent"].as_str(),
            p["permission"].as_str(),
            p["model"].as_str()
        ),
        (Some("claude"), Some("read-only"), Some("sonnet"))
    );
    assert_eq!(p["allowed_tools"], serde_json::json!(["Read", "Grep"]));
}

#[test]
fn run_with_a_profile_uses_the_fake_agent_and_records_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let repo = init_repo(dir.path(), "proj");
    let agents = repo.join(".selachi").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("r.toml"),
        "name = \"Research\"\nagent = \"agy\"\ninstructions = \"Cite files.\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let script = data.join("fake.json");
    std::fs::write(&script, r#"{"reply": "found it", "usage": [10, 5]}"#).unwrap();
    let mut c = bin(&data);
    c.env("SELACHI_AGENT_FAKE", &script)
        .args(["run", "--profile", "research", "--json", "--cwd"])
        .arg(&repo)
        .arg("where is auth?");
    let (code, out, err) = run(c);
    assert_eq!(code, 0, "{out}{err}");
    let v = json_of(&out);
    assert_eq!(
        (
            v["status"].as_str(),
            v["agent"].as_str(),
            v["profile"].as_str()
        ),
        (Some("done"), Some("agy"), Some("Research"))
    );
    let id = v["run_id"].as_str().unwrap();
    let rec = json_of(
        &std::fs::read_to_string(data.join("agent-runs").join(format!("{id}.json"))).unwrap(),
    );
    assert_eq!(rec["profile"], "Research");
    assert_eq!(rec["launcher"], "cli");
    assert!(rec["final_prompt"]
        .as_str()
        .unwrap()
        .contains("Cite files."));
    assert_eq!(rec["prompt"], "where is auth?");
    assert_eq!(rec["limits"]["permission"], "read-only");
    // an unknown profile is a refusal (exit 3), nothing started
    let mut c = bin(&data);
    c.env("SELACHI_AGENT_FAKE", &script)
        .args(["run", "--profile", "nope", "--json", "--cwd"])
        .arg(&repo)
        .arg("x");
    let (code, out, _) = run(c);
    assert_eq!(code, 3, "{out}");
    assert_eq!(json_of(&out)["code"], "unknown_profile");
    // no agent and no profile is a usage error
    let mut c = bin(&data);
    c.args(["run", "--json"]);
    assert_eq!(run(c).0, 2);
}
