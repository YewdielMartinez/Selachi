#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Subagent profiles (PLAN 21.3) through the harness with a scripted agent: resolution precedence,
//! minimum-of-limits, the final prompt, context files (caps, redaction, escapes), Claude's tool flags,
//! the JSON-schema handoff, the dry run and the registry fields.

mod common;

use std::path::Path;

use agent_harness::{
    Caller, DelegationConfig, Permission, RefusalCode, RunRequest, RunResult, RunStatus,
};
use agents_driver::{AgentKind, PermissionMode};
use chat_core::ChatStore;
use common::*;

const SECRET: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";

fn agent_that_answers(reply: &'static str) -> Setup {
    setup(move |_, _, _| vec![working(), text(reply), usage(10, 5, 0, 0), done()])
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn project_profile(repo: &Path, file: &str, toml: &str) {
    write(&repo.join(".selachi").join("agents").join(file), toml);
}

fn req(repo: &Path, profile: &str, prompt: &str) -> RunRequest {
    let mut r = RunRequest::new("", prompt, repo);
    r.profile = Some(profile.into());
    r
}

fn refused(r: &RunResult, code: RefusalCode) {
    assert_eq!(r.status, RunStatus::Refused, "{r:?}");
    assert_eq!(r.code, Some(code), "{:?}", r.reason);
    assert!(r.run_id.is_none());
}

const REVIEWER: &str = r#"
name = "Revisor"
description = "Security review"
agent = "claude"
model = "opus"
effort = "high"
permission = "read-only"
instructions = "Be strict."
allowed_tools = ["Read", "Grep"]
denied_tools = ["Edit"]
context_files = ["docs/guide.md"]
output_format = "json"
"#;

#[test]
fn a_profile_run_sends_the_final_prompt_and_records_everything() {
    let s = agent_that_answers("not json at all");
    let repo = init_repo(s.dir.path(), "proj");
    project_profile(&repo, "revisor.toml", REVIEWER);
    write(
        &repo.join("docs").join("guide.md"),
        &format!("Guide\nkey = {SECRET}\n"),
    );
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());

    let r = h.run(req(&repo, "revisor", "audit the auth module"));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(r.profile.as_deref(), Some("Revisor"));
    let run_id = r.run_id.clone().unwrap();

    // what the agent received
    let sent = s.factory.sent();
    assert_eq!(sent.len(), 1);
    let prompt = &sent[0];
    for needle in [
        "=== SELACHI PROFILE: Revisor ===",
        "Be strict.",
        "=== END SELACHI PROFILE ===",
        "=== CONTEXT FILE: docs/guide.md ===",
        "Guide",
        "=== OUTPUT FORMAT ===",
        "=== TASK ===\naudit the auth module",
    ] {
        assert!(prompt.contains(needle), "{needle} missing in:\n{prompt}");
    }
    let order = |n: &str| prompt.find(n).unwrap();
    assert!(
        order("SELACHI PROFILE") < order("CONTEXT FILE")
            && order("CONTEXT FILE") < order("OUTPUT FORMAT")
            && order("OUTPUT FORMAT") < order("=== TASK ==="),
        "the instructions come first, the task last"
    );
    assert!(!prompt.contains(SECRET), "the secret in the context file must be redacted");
    assert!(r.notes.iter().any(|n| n.contains("hidden")), "{:?}", r.notes);

    // how the agent was started
    let (kind, cfg, _) = s.factory.rec.lock().spawns[0].clone();
    assert_eq!(kind, AgentKind::ClaudeCode);
    assert_eq!(cfg.model.as_deref(), Some("opus"));
    assert_eq!(cfg.effort.as_deref(), Some("high"));
    assert_eq!(cfg.permission_mode, PermissionMode::ReadOnly);
    assert_eq!(
        cfg.extra_args,
        ["--allowedTools", "Read", "Grep", "--disallowedTools", "Edit"],
        "Claude gets the tool lists as flags"
    );

    // output_format = json and the reply is not JSON: the caller is told
    assert!(
        r.notes.iter().any(|n| n.contains("output_format = json")),
        "{:?}",
        r.notes
    );

    // the registry (what the map inspector reads)
    let rec = h.registry().load(&run_id).unwrap();
    assert_eq!(rec.profile.as_deref(), Some("Revisor"));
    assert_eq!(rec.launcher.as_deref(), Some("cli"));
    assert_eq!(rec.effort.as_deref(), Some("high"));
    assert_eq!(rec.output_format.as_deref(), Some("json"));
    assert_eq!(rec.context_files, ["docs/guide.md"]);
    assert_eq!(rec.prompt, "audit the auth module", "`prompt` stays the caller's task");
    assert_eq!(rec.final_prompt.as_deref(), Some(prompt.as_str()));
    let l = rec.limits.unwrap();
    assert_eq!(
        (l.permission.as_str(), l.parent_permission.as_str(), l.config_max_permission.as_str()),
        ("read-only", "read-only", "accept-edits")
    );
    assert_eq!(l.profile_permission.as_deref(), Some("read-only"));
    assert_eq!((l.depth, l.max_depth, l.timeout_secs), (1, 1, 900));
    assert_eq!(l.budget_tokens, None);
    assert!(l.tools_enforced && !l.worktree);
    assert_eq!(l.allowed_tools, ["Read", "Grep"]);
    assert_eq!(l.denied_tools, ["Edit"]);
    // and it is on the JSON the CLI prints
    let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(v["profile"], "Revisor");
}

#[test]
fn a_run_without_a_profile_is_unchanged() {
    let s = agent_that_answers("ok");
    let repo = init_repo(s.dir.path(), "proj");
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(RunRequest::new("codex", "plain task", &repo));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(s.factory.sent(), ["plain task"]);
    assert!(s.factory.rec.lock().spawns[0].1.extra_args.is_empty());
    let rec = h.registry().load(r.run_id.as_deref().unwrap()).unwrap();
    assert_eq!(rec.profile, None);
    assert_eq!(rec.final_prompt.as_deref(), Some("plain task"));
    assert_eq!(rec.launcher.as_deref(), Some("cli"));
    let l = rec.limits.unwrap();
    assert_eq!((l.permission.as_str(), l.timeout_secs, l.tools_enforced), ("read-only", 900, false));
}

#[test]
fn limits_are_the_minimum_of_profile_config_and_caller() {
    let s = agent_that_answers("ok");
    let repo = init_repo(s.dir.path(), "proj");
    project_profile(
        &repo,
        "t.toml",
        "name = \"Tests\"\nagent = \"claude\"\npermission = \"accept-edits\"\nworktree = true\ntimeout = \"30m\"\nbudget_tokens = 1000\n",
    );

    // a read-only caller: the profile's accept-edits is lowered, not refused
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(req(&repo, "tests", "write tests"));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(r.permission.as_deref(), Some("read-only"));
    assert!(r.notes.iter().any(|n| n.contains("lowered to read-only")), "{:?}", r.notes);
    assert_eq!(
        s.factory.rec.lock().spawns[0].1.permission_mode,
        PermissionMode::ReadOnly
    );
    let rec = h.registry().load(r.run_id.as_deref().unwrap()).unwrap();
    let l = rec.limits.unwrap();
    assert_eq!(l.timeout_secs, 900, "30 m is capped by [delegation] timeout_secs");
    assert_eq!(l.budget_tokens, Some(1000));
    assert!(l.worktree && rec.worktree_path.is_some());
    assert_eq!(l.profile_permission.as_deref(), Some("accept-edits"));
    assert_eq!(l.permission, "read-only");

    // an accept-edits caller gets what the profile asks for ...
    let h = s.harness(caller(Permission::AcceptEdits), DelegationConfig::default());
    let r = h.run(req(&repo, "tests", "write tests"));
    assert_eq!(r.permission.as_deref(), Some("accept-edits"), "{r:?}");
    // ... unless [delegation] max_permission is lower
    let low = DelegationConfig {
        max_permission: Permission::Ask,
        ..DelegationConfig::default()
    };
    let h = s.harness(caller(Permission::AcceptEdits), low);
    assert_eq!(
        h.run(req(&repo, "tests", "write tests")).permission.as_deref(),
        Some("ask")
    );

    // an explicit request can narrow the profile but never widen it
    let h = s.harness(caller(Permission::AcceptEdits), DelegationConfig::default());
    let mut narrow = req(&repo, "tests", "write tests");
    narrow.permission = Permission::ReadOnly;
    narrow.permission_explicit = true;
    narrow.budget_tokens = Some(400);
    let r = h.run(narrow);
    assert_eq!(r.permission.as_deref(), Some("read-only"));
    let rec = h.registry().load(r.run_id.as_deref().unwrap()).unwrap();
    assert_eq!(rec.limits.unwrap().budget_tokens, Some(400), "min(1000, 400)");
    project_profile(&repo, "ro.toml", "name = \"RO\"\nagent = \"claude\"\n");
    let mut widen = req(&repo, "ro", "x");
    widen.permission = Permission::AcceptEdits;
    widen.permission_explicit = true;
    assert_eq!(h.run(widen).permission.as_deref(), Some("read-only"));

    // `full` is still refused, with or without a profile
    let mut full = req(&repo, "tests", "x");
    full.permission = Permission::Full;
    full.permission_explicit = true;
    refused(&h.run(full), RefusalCode::PermissionFull);
}

#[test]
fn profile_precedence_project_over_global_and_agent_override() {
    let s = agent_that_answers("ok");
    let repo = init_repo(s.dir.path(), "proj");
    write(
        &s.data_dir().join("agents").join("who.toml"),
        "name = \"Who\"\nagent = \"codex\"\n",
    );
    write(
        &s.data_dir().join("agents").join("only-global.toml"),
        "name = \"Only global\"\nagent = \"gemini\"\n",
    );
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());

    // global only
    assert_eq!(h.run(req(&repo, "who", "x")).status, RunStatus::Done);
    assert_eq!(s.factory.rec.lock().spawns[0].0, AgentKind::Codex);
    // the project's profile of the same name wins
    project_profile(&repo, "who.toml", "name = \"who\"\nagent = \"claude\"\n");
    assert_eq!(h.run(req(&repo, "WHO", "x")).status, RunStatus::Done);
    assert_eq!(s.factory.rec.lock().spawns[1].0, AgentKind::ClaudeCode);
    // an explicit agent overrides the profile's
    let mut over = req(&repo, "who", "x");
    over.agent = "opencode".into();
    assert_eq!(h.run(over).status, RunStatus::Done);
    assert_eq!(s.factory.rec.lock().spawns[2].0, AgentKind::OpenCode);
    // the global one is still reachable from another project
    let other = init_repo(s.dir.path(), "other");
    assert_eq!(h.run(req(&other, "only global", "x")).status, RunStatus::Done);

    // [agent_profiles] project_profiles = false ignores the project's profile
    write(
        &s.data_dir().join("config.toml"),
        "[agent_profiles]\nproject_profiles = false\n",
    );
    let h2 = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    assert_eq!(h2.run(req(&repo, "who", "x")).status, RunStatus::Done);
    assert_eq!(s.factory.rec.lock().spawns[4].0, AgentKind::Codex);
}

#[test]
fn bad_profiles_are_refused_before_anything_starts() {
    let s = agent_that_answers("ok");
    let repo = init_repo(s.dir.path(), "proj");
    write(&s.dir.path().join("outside.txt"), "outside");
    project_profile(
        &repo,
        "escape.toml",
        "name = \"Escape\"\nagent = \"claude\"\ncontext_files = [\"../outside.txt\"]\n",
    );
    project_profile(
        &repo,
        "missing.toml",
        "name = \"Missing\"\nagent = \"claude\"\ncontext_files = [\"nope.md\"]\n",
    );
    project_profile(&repo, "broken.toml", "name = \"Broken\"\n"); // no agent
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());

    let r = h.run(req(&repo, "escape", "x"));
    refused(&r, RefusalCode::InvalidProfile);
    assert!(r.reason.unwrap().contains("outside the project"));
    let r = h.run(req(&repo, "missing", "x"));
    refused(&r, RefusalCode::InvalidProfile);
    let r = h.run(req(&repo, "broken", "x"));
    refused(&r, RefusalCode::UnknownProfile);
    assert!(r.reason.as_deref().unwrap().contains("have errors"), "{:?}", r.reason);
    let r = h.run(req(&repo, "nothing-like-it", "x"));
    refused(&r, RefusalCode::UnknownProfile);
    assert!(r.reason.unwrap().contains("available:"));
    // no agent and no profile
    refused(
        &h.run(RunRequest::new("", "x", &repo)),
        RefusalCode::BadRequest,
    );
    assert_eq!(s.factory.spawns(), 0);
    let chat = s.data_dir().join("chat.db");
    if chat.exists() {
        assert!(ChatStore::open_at(chat).unwrap().delegated_threads().unwrap().is_empty());
    }

    // profiles turned off
    write(
        &s.data_dir().join("config.toml"),
        "[agent_profiles]\nenabled = false\n",
    );
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(req(&repo, "escape", "x"));
    refused(&r, RefusalCode::UnknownProfile);
    assert!(r.reason.unwrap().contains("turned off"));
}

#[test]
fn context_files_are_capped_and_redacted() {
    let s = agent_that_answers("ok");
    let repo = init_repo(s.dir.path(), "proj");
    write(&repo.join("a.md"), &format!("token {SECRET}\n{}", "a".repeat(2000)));
    write(&repo.join("b.md"), &"b".repeat(5000));
    write(&repo.join("c.md"), "never reached");
    project_profile(
        &repo,
        "ctx.toml",
        "name = \"Ctx\"\nagent = \"claude\"\ncontext_files = [\"a.md\", \"b.md\", \"c.md\"]\n",
    );
    // a 3 KiB cap via config.toml
    write(
        &s.data_dir().join("config.toml"),
        "[agent_profiles]\nmax_context_kb = 2\n",
    );
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(req(&repo, "ctx", "go"));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let prompt = s.factory.sent().remove(0);
    assert!(!prompt.contains(SECRET));
    assert!(prompt.contains("«oculto:anthropic»"), "the redaction marker is there");
    assert!(prompt.contains("=== CONTEXT FILE: a.md ==="));
    assert!(prompt.contains("=== CONTEXT FILE: b.md ==="));
    assert!(!prompt.contains("never reached"), "the third file found the cap used up");
    let b_bytes = prompt.matches('b').count();
    assert!(
        (900..1100).contains(&b_bytes),
        "b.md was cut to what is left of 3 KiB after a.md ({b_bytes} b's)"
    );
    assert!(r.notes.iter().any(|n| n.contains("truncated")), "{:?}", r.notes);
    assert!(r.notes.iter().any(|n| n.contains("left out")), "{:?}", r.notes);
    let rec = h.registry().load(r.run_id.as_deref().unwrap()).unwrap();
    assert_eq!(rec.context_files, ["a.md", "b.md"]);
}

#[test]
fn tool_lists_are_enforced_for_claude_only() {
    let s = agent_that_answers("ok");
    let repo = init_repo(s.dir.path(), "proj");
    project_profile(
        &repo,
        "t.toml",
        "name = \"Tools\"\nagent = \"codex\"\nallowed_tools = [\"Read\"]\ndenied_tools = [\"Bash\"]\n",
    );
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(req(&repo, "tools", "x"));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert!(s.factory.rec.lock().spawns[0].1.extra_args.is_empty());
    assert!(r.notes.iter().any(|n| n.contains("only enforced for Claude")), "{:?}", r.notes);
    let prompt = s.factory.sent().remove(0);
    assert!(prompt.contains("TOOL POLICY (advisory)") && prompt.contains("Never use: Bash"));
    let l = h.registry().load(r.run_id.as_deref().unwrap()).unwrap().limits.unwrap();
    assert!(!l.tools_enforced);
    assert_eq!((l.allowed_tools, l.denied_tools), (vec!["Read".to_string()], vec!["Bash".to_string()]));

    // the same profile pointed at Claude (explicit agent) is enforced
    let mut claude = req(&repo, "tools", "x");
    claude.agent = "claude".into();
    let r = h.run(claude);
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    assert_eq!(
        s.factory.rec.lock().spawns[1].1.extra_args,
        ["--allowedTools", "Read", "--disallowedTools", "Bash"]
    );
    assert!(!s.factory.sent()[1].contains("TOOL POLICY"));
}

#[test]
fn json_output_format_hands_the_schema_to_the_agent() {
    let s = agent_that_answers("{\"findings\": []}");
    let repo = init_repo(s.dir.path(), "proj");
    project_profile(
        &repo,
        "j.toml",
        "name = \"Json\"\nagent = \"claude\"\noutput_format = \"json\"\noutput_schema = \"schemas/out.json\"\n",
    );
    write(
        &repo.join(".selachi/agents/schemas/out.json"),
        "{\n  \"type\": \"object\",\n  \"required\": [\"findings\"]\n}\n",
    );
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let r = h.run(req(&repo, "json", "review"));
    assert_eq!(r.status, RunStatus::Done, "{r:?}");
    let prompt = s.factory.sent().remove(0);
    assert!(prompt.contains("ONLY one JSON document"));
    assert!(prompt.contains("{\"required\":[\"findings\"],\"type\":\"object\"}") || prompt.contains("\"required\":[\"findings\"]"), "{prompt}");
    assert!(
        !r.notes.iter().any(|n| n.contains("output_format")),
        "a JSON reply raises no note: {:?}",
        r.notes
    );
    // a schema that is not there fails the run before it starts
    project_profile(
        &repo,
        "k.toml",
        "name = \"NoSchema\"\nagent = \"claude\"\noutput_format = \"json\"\noutput_schema = \"schemas/gone.json\"\n",
    );
    let before = s.factory.spawns();
    refused(&h.run(req(&repo, "noschema", "x")), RefusalCode::InvalidProfile);
    assert_eq!(s.factory.spawns(), before);
}

#[test]
fn prepare_is_a_dry_run_that_starts_nothing() {
    let s = agent_that_answers("ok");
    let repo = init_repo(s.dir.path(), "proj");
    project_profile(&repo, "revisor.toml", REVIEWER);
    write(&repo.join("docs").join("guide.md"), "guide text\n");
    let h = s.harness(caller(Permission::ReadOnly), DelegationConfig::default());
    let prep = h.prepare(&req(&repo, "revisor", &format!("check {SECRET}"))).unwrap();
    assert_eq!(prep.profile.as_deref(), Some("Revisor"));
    assert_eq!(prep.agent, "claude");
    assert!(prep.final_prompt.contains("Be strict.") && prep.final_prompt.contains("guide text"));
    assert!(!prep.final_prompt.contains(SECRET), "redacted");
    assert!(prep.final_prompt.ends_with(&prep.task_prompt));
    assert!(prep.tools_enforced);
    let text = prep.to_text();
    assert!(text.contains("EFFECTIVE LIMITS") && text.contains("FINAL PROMPT") && text.contains("guide text"));
    let v = prep.to_value();
    assert_eq!(v["limits"]["permission"], "read-only");
    assert_eq!(v["context_files"][0]["path"], "docs/guide.md");

    // nothing happened: no agent, no thread, no record
    assert_eq!(s.factory.spawns(), 0);
    assert!(h.registry().list().is_empty());
    assert!(!s.data_dir().join("chat.db").exists());
    // and the rules apply to a dry run too
    let mut full = req(&repo, "revisor", "x");
    full.permission = Permission::Full;
    full.permission_explicit = true;
    assert_eq!(h.prepare(&full).unwrap_err().code, RefusalCode::PermissionFull);
    let deep = Caller {
        depth: 1,
        ..caller(Permission::ReadOnly)
    };
    let h = s.harness(deep, DelegationConfig::default());
    assert_eq!(
        h.prepare(&req(&repo, "revisor", "x")).unwrap_err().code,
        RefusalCode::DepthExceeded
    );
}
