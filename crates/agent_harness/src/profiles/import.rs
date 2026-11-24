//! Import Claude Code subagents (`.claude/agents/*.md`) as profiles (PLAN section 21.3). An explicit
//! command only (`selachi-agent profiles import-claude`), never automatic.
//!
//! A Claude subagent file is a Markdown file with a YAML frontmatter:
//!
//! ```text
//! ---
//! name: code-reviewer
//! description: Reviews code for quality
//! tools: Read, Grep, Glob        # or a YAML list; absent = every tool
//! model: sonnet                   # sonnet | opus | haiku | inherit
//! ---
//! You are a senior code reviewer...
//! ```
//!
//! The conversion: `name`, `description` and `model` carry over (`inherit` = no model), `tools` becomes
//! `allowed_tools`, the body becomes `instructions`, `agent = "claude"` and the permission is always
//! **read-only** (a Claude subagent says nothing about permissions; raise it yourself in the TOML if
//! you want to, still capped by `[delegation]`). Other frontmatter keys are reported, not imported.

use std::path::{Path, PathBuf};

use super::{emit_toml, slugify, ProfileFile, MAX_INSTRUCTIONS_CHARS, MAX_NAME_CHARS};

/// A subagent file converted to a profile, ready to be previewed and written.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    /// The `.md` it came from.
    pub source: PathBuf,
    pub profile: ProfileFile,
    /// Frontmatter keys that have no profile equivalent.
    pub ignored: Vec<String>,
}

/// Split `---` frontmatter from the body. `None` when the file has no frontmatter.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    let bytes = v.as_bytes();
    if v.len() >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'') && bytes[v.len() - 1] == bytes[0] {
        let inner = &v[1..v.len() - 1];
        if bytes[0] == b'"' {
            return inner
                .replace("\\n", "\n")
                .replace("\\\"", "\"")
                .replace("\\\\", "\\");
        }
        return inner.replace("''", "'");
    }
    v.to_string()
}

/// The small YAML subset subagent files use: `key: value`, quoted values, `>` / `|` block scalars and
/// `- item` lists.
fn parse_frontmatter(fm: &str) -> Vec<(String, Vec<String>, bool)> {
    // (key, values, is_list)
    let mut out: Vec<(String, Vec<String>, bool)> = Vec::new();
    let lines: Vec<&str> = fm.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.trim().is_empty()
            || line.trim_start().starts_with('#')
            || line.starts_with([' ', '\t'])
        {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();
        if value.is_empty() {
            // a list (`- a`) on the following lines
            let mut items = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with("- ") {
                items.push(unquote(lines[i].trim_start().trim_start_matches("- ")));
                i += 1;
            }
            out.push((key, items, true));
        } else if value == ">" || value == "|" || value == ">-" || value == "|-" {
            let folded = value.starts_with('>');
            let mut parts = Vec::new();
            while i < lines.len()
                && (lines[i].starts_with([' ', '\t']) || lines[i].trim().is_empty())
            {
                parts.push(lines[i].trim().to_string());
                i += 1;
            }
            let joined = parts.join(if folded { " " } else { "\n" });
            out.push((key, vec![joined.trim().to_string()], false));
        } else {
            out.push((key, vec![unquote(value)], false));
        }
    }
    out
}

/// Convert the text of one subagent file. `file_stem` names the profile when the frontmatter has no `name`.
pub fn convert(text: &str, source: &Path) -> Result<Converted, String> {
    let (fm, body) = split_frontmatter(text)
        .ok_or("no YAML frontmatter (`---` ... `---`) at the top of the file")?;
    let entries = parse_frontmatter(fm);
    let get = |k: &str| {
        entries
            .iter()
            .find(|(key, _, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, v, list)| (v.clone(), *list))
    };
    let single = |k: &str| get(k).and_then(|(v, _)| v.into_iter().next());
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = single("name")
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(stem);
    if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("the name must be 1-{MAX_NAME_CHARS} characters"));
    }
    let model = single("model")
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("inherit"));
    let allowed_tools: Vec<String> = match get("tools") {
        None => Vec::new(),
        Some((v, true)) => v,
        Some((v, false)) => v
            .first()
            .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_default(),
    }
    .into_iter()
    .filter(|t| !t.is_empty())
    .collect();
    let body = body.trim();
    if body.chars().count() > MAX_INSTRUCTIONS_CHARS {
        return Err(format!(
            "the body is longer than {MAX_INSTRUCTIONS_CHARS} characters, which a profile does not accept"
        ));
    }
    let known = ["name", "description", "tools", "model"];
    let ignored = entries
        .iter()
        .map(|(k, _, _)| k.clone())
        .filter(|k| !known.iter().any(|n| n.eq_ignore_ascii_case(k)))
        .collect();
    Ok(Converted {
        source: source.to_path_buf(),
        profile: ProfileFile {
            name: name.trim().to_string(),
            description: single("description").unwrap_or_default(),
            agent: "claude".into(),
            model,
            permission: Some("read-only".into()),
            allowed_tools,
            instructions: body.to_string(),
            ..ProfileFile::default()
        },
        ignored,
    })
}

/// A planned import: what to write where, and what could not be converted.
#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    /// `(target file, TOML text, converted)`.
    pub items: Vec<(PathBuf, String, Converted)>,
    /// `(source file, why)`.
    pub skipped: Vec<(PathBuf, String)>,
    /// Target files that already exist (refused without `--force`).
    pub conflicts: Vec<PathBuf>,
}

/// Plan the import of every `*.md` in `source_dir` into `target_dir`. Nothing is written.
pub fn plan(source_dir: &Path, target_dir: &Path) -> Result<ImportPlan, String> {
    let rd = std::fs::read_dir(source_dir).map_err(|e| {
        format!(
            "cannot read {}: {e} (no Claude Code subagents there?)",
            source_dir.display()
        )
    })?;
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
        .collect();
    files.sort();
    let mut plan = ImportPlan::default();
    let mut used: Vec<String> = Vec::new();
    for f in files.into_iter().take(super::MAX_PROFILES_PER_DIR) {
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(e) => {
                plan.skipped.push((f, format!("cannot read it: {e}")));
                continue;
            }
        };
        match convert(&text, &f) {
            Ok(c) => {
                // the converted file must be a valid profile before it is offered
                let toml = emit_toml(
                    &c.profile,
                    Some(&format!("Imported from {} by `selachi-agent profiles import-claude`.\nRead-only on purpose: raise `permission` yourself if you want to.", f.display())),
                );
                let target = target_dir.join(format!("{}.toml", slugify(&c.profile.name)));
                let checked = super::Profile::from_toml(&toml, &target, super::Scope::Global);
                if let Err(e) = checked {
                    plan.skipped.push((f, e.problems.join("; ")));
                    continue;
                }
                let key = target.to_string_lossy().to_lowercase();
                if used.contains(&key) {
                    plan.skipped.push((
                        f,
                        format!("another subagent already maps to {}", target.display()),
                    ));
                    continue;
                }
                used.push(key);
                if target.exists() {
                    plan.conflicts.push(target.clone());
                }
                plan.items.push((target, toml, c));
            }
            Err(e) => plan.skipped.push((f, e)),
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Permission;
    use crate::profiles::{Profile, Scope};

    const REVIEWER: &str = "---\nname: code-reviewer\ndescription: \"Reviews code, carefully\"\ntools: Read, Grep, Glob\nmodel: sonnet\ncolor: blue\n---\nYou are a senior reviewer.\n\nBe brief.\n";

    #[test]
    fn converts_frontmatter_and_body() {
        let c = convert(REVIEWER, Path::new("C:/x/code-reviewer.md")).unwrap();
        let p = &c.profile;
        assert_eq!(p.name, "code-reviewer");
        assert_eq!(p.description, "Reviews code, carefully");
        assert_eq!(p.agent, "claude");
        assert_eq!(p.model.as_deref(), Some("sonnet"));
        assert_eq!(p.allowed_tools, ["Read", "Grep", "Glob"]);
        assert_eq!(p.permission.as_deref(), Some("read-only"));
        assert_eq!(p.instructions, "You are a senior reviewer.\n\nBe brief.");
        assert_eq!(c.ignored, ["color"]);
    }

    #[test]
    fn yaml_variants() {
        let c = convert(
            "---\r\nname: x\r\ndescription: >\r\n  one\r\n  two\r\ntools:\r\n  - Read\r\n  - \"Bash(git log:*)\"\r\nmodel: inherit\r\n---\r\nbody",
            Path::new("x.md"),
        )
        .unwrap();
        assert_eq!(c.profile.description, "one two");
        assert_eq!(c.profile.allowed_tools, ["Read", "Bash(git log:*)"]);
        assert_eq!(c.profile.model, None);
        // no name: the file name is used; no tools: all (empty list)
        let c = convert("---\ndescription: d\n---\nb", Path::new("C:/a/helper.md")).unwrap();
        assert_eq!(c.profile.name, "helper");
        assert!(c.profile.allowed_tools.is_empty());
        assert!(convert("no frontmatter", Path::new("x.md")).is_err());
        assert!(convert("---\nname: x\nnever closed", Path::new("x.md")).is_err());
    }

    #[test]
    fn plan_previews_without_writing_and_flags_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("code-reviewer.md"), REVIEWER).unwrap();
        std::fs::write(src.join("bad.md"), "no frontmatter").unwrap();
        std::fs::write(src.join("notes.txt"), "x").unwrap();
        std::fs::write(dst.join("code-reviewer.toml"), "old").unwrap();
        let plan = plan(&src, &dst).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.conflicts, [dst.join("code-reviewer.toml")]);
        assert_eq!(
            std::fs::read_to_string(dst.join("code-reviewer.toml")).unwrap(),
            "old",
            "plan writes nothing"
        );
        // what would be written is a valid, read-only Claude profile
        let (target, text, _) = &plan.items[0];
        let p = Profile::from_toml(text, target, Scope::Global).unwrap();
        assert_eq!(
            (p.agent.as_str(), p.permission),
            ("claude", Some(Permission::ReadOnly))
        );
        assert_eq!(p.allowed_tools, ["Read", "Grep", "Glob"]);
        assert!(text.contains("Imported from"));
        assert!(super::plan(&src.join("missing"), &dst).is_err());
    }
}
