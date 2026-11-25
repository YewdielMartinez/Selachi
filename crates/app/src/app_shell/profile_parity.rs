//! The profiles editor (`ui_settings`) previews the prompt with its own pure function so the settings
//! crate does not depend on the delegation engine. This test keeps the two honest: for a few fixtures
//! the editor's preview must equal the final prompt `agent_harness` would send.

use std::path::Path;

use agent_harness::{Caller, Harness, RunRequest};
use ui_settings::{Profile, ProfileScope, final_prompt, profile_context_files, profile_to_toml};

const SECRET: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// What the harness would send for `profile` on `task` (a dry run: nothing starts).
fn harness_prompt(root: &Path, data: &Path, profile: &Profile, task: &str) -> String {
    write(
        &root.join(".selachi").join("agents").join("fixture.toml"),
        &profile_to_toml(profile),
    );
    let h = Harness::builder()
        .data_dir(data)
        .caller(Caller::default())
        .build()
        .unwrap();
    let mut req = RunRequest::new("", task, root);
    req.profile = Some(profile.name.clone());
    h.prepare(&req).unwrap().final_prompt
}

fn editor_prompt(root: &Path, profile: &Profile, task: &str) -> String {
    let files = profile_context_files(profile, Some(root));
    final_prompt(profile, task, &files)
}

fn fixtures() -> Vec<(&'static str, Profile)> {
    let base = Profile {
        name: "Fixture".into(),
        agent: "codex".into(),
        ..Profile::default()
    };
    vec![
        ("nothing to add", base.clone()),
        (
            "instructions with a secret",
            Profile {
                instructions: format!("Sé estricto.\nClave: {SECRET}"),
                ..base.clone()
            },
        ),
        (
            "codex: context, json, advisory tools",
            Profile {
                instructions: "Revisa solo lo pedido.".into(),
                context_files: vec!["docs/guia.md".into()],
                output_format: "json".into(),
                allowed_tools: vec!["Read".into()],
                denied_tools: vec!["Bash".into(), "Edit".into()],
                ..base.clone()
            },
        ),
        (
            "claude: diff, tools enforced (no advisory block)",
            Profile {
                agent: "claude".into(),
                instructions: "Propón parches.".into(),
                output_format: "diff".into(),
                denied_tools: vec!["Write".into()],
                ..base
            },
        ),
    ]
}

#[test]
fn the_editor_preview_equals_the_harness_final_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root.join("docs").join("guia.md"), "# Guía\nUsa tabs.\n");
    let data = dir.path().join("data");
    for (what, profile) in fixtures() {
        let mine = editor_prompt(&root, &profile, "  Audita el módulo auth  ");
        let theirs = harness_prompt(&root, &data, &profile, "  Audita el módulo auth  ");
        assert_eq!(mine, theirs, "fixture `{what}`");
        assert!(
            !mine.contains("sk-ant-api03"),
            "fixture `{what}` leaked a secret"
        );
    }
    // keep the scope import honest: fixtures are project profiles
    let _ = ProfileScope::Project;
}
