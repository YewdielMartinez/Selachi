//! «Permitir siempre en este perfil» (ola 9B): the user answered `always_profile` to an approval, so
//! the command's pattern goes into the profile's `allowed_commands`.
//!
//! * [`normalize_pattern`]: `cargo test -p x` -> `cargo test *` (binary + first subcommand + ` *`; a
//!   command with no arguments stays as it is). Only commands that can be described safely get a
//!   pattern: no shell metacharacters, no interpreter / shell / destructive programs, a subcommand that
//!   is a plain word (`git -C x status` would otherwise become `git *`), nothing the hard blocklist
//!   denies, and the pattern must pass the allowlist filter (`approval_policy::pattern_problem`).
//! * [`add_to_profile`]: appends the pattern with `toml_edit`, keeping comments and layout, and checks
//!   that the result is still a valid profile before an atomic replace. Never creates a profile.

use std::path::{Path, PathBuf};

use crate::approval_policy::{
    blocked, command_allowed, has_shell_meta, pattern_problem, OwnedScope, Scope, WRAPPERS,
};
use crate::profiles::{Profile, ProfileFile, Scope as ProfileScope};

/// Programs whose arguments are code or destroy data: a standing permission for them would be a
/// blank cheque, so they are only ever allowed one request at a time.
const NEVER_STANDING: &[&str] = &[
    "python",
    "python3",
    "py",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "npx",
    "uvx",
    "pipx",
    "curl",
    "wget",
    "ssh",
    "scp",
    "sftp",
    "ftp",
    "telnet",
    "reg",
    "regedit",
    "schtasks",
    "sc",
    "net",
    "takeown",
    "icacls",
    "attrib",
    "rm",
    "del",
    "erase",
    "rd",
    "rmdir",
    "mv",
    "move",
    "ren",
    "rename",
    "format",
    "diskpart",
    "remove-item",
    "mshta",
    "rundll32",
    "regsvr32",
    "certutil",
    "bitsadmin",
    "msiexec",
    "taskkill",
    "shutdown",
    "xcopy",
    "robocopy",
];

/// `C:\tools\Git.exe` -> `git`.
fn stem(token: &str) -> String {
    let low = token.trim().to_lowercase();
    let name = low.rsplit(['/', '\\']).next().unwrap_or(&low).to_string();
    for ext in [".exe", ".cmd", ".bat", ".com", ".ps1"] {
        if let Some(s) = name.strip_suffix(ext) {
            return s.to_string();
        }
    }
    name
}

fn is_word(t: &str) -> bool {
    t.starts_with(|c: char| c.is_ascii_alphabetic())
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The pattern to remember for `command`, or why none can be.
pub fn normalize_pattern(command: &str, scope: &OwnedScope) -> Result<String, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("it is not a shell command".into());
    }
    if has_shell_meta(cmd) || cmd.ends_with('…') {
        return Err("it chains, redirects or substitutes (or was cut short), so it can never match an allowlist".into());
    }
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let bin = tokens[0];
    if bin.contains(['"', '\'', '*', '?']) || bin.contains('=') {
        return Err("its program name cannot be generalized".into());
    }
    let s = stem(bin);
    if WRAPPERS.contains(&s.as_str()) || NEVER_STANDING.contains(&s.as_str()) {
        return Err(format!(
            "`{s}` runs arbitrary code or changes things too freely for a standing permission"
        ));
    }
    let pattern = match tokens.get(1) {
        None => bin.to_string(),
        Some(sub) if is_word(sub) => format!("{bin} {sub} *"),
        Some(_) => {
            return Err(format!(
            "the first argument of `{s}` is not a plain subcommand, so a pattern would be too wide"
        ))
        }
    };
    if let Some(why) = pattern_problem(&pattern) {
        return Err(format!("the pattern {pattern:?} {why}"));
    }
    let roots: Vec<&Path> = scope.roots.iter().map(PathBuf::as_path).collect();
    let sc = Scope {
        cwd: &scope.cwd,
        roots: &roots,
    };
    let base = pattern.strip_suffix(" *").unwrap_or(&pattern);
    for sample in [cmd, base, pattern.as_str()] {
        if let Some(why) = blocked("Bash", sample, sc) {
            return Err(format!("it is on the blocklist ({why})"));
        }
    }
    if !command_allowed(cmd, std::slice::from_ref(&pattern)) {
        return Err(format!(
            "the pattern {pattern:?} would not match the command"
        ));
    }
    Ok(pattern)
}

/// What [`add_to_profile`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    Added,
    /// The profile already had an equivalent entry.
    AlreadyThere,
}

fn same(a: &str, b: &str) -> bool {
    let n = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    n(a) == n(b)
}

/// Append to a list. A list written one item per line gets the new item on its own line, and a comment
/// after the last item stays with that item.
fn push_keeping_layout(arr: &mut toml_edit::Array, pattern: &str) {
    let old = arr.trailing().as_str().unwrap_or("").to_string();
    let Some(nl) = old.find(char::from(10u8)) else {
        arr.push(pattern);
        return;
    };
    let (keep, rest) = old.split_at(nl + 1);
    let mut v = toml_edit::Value::from(pattern);
    v.decor_mut().set_prefix(format!("{keep}  "));
    arr.push_formatted(v);
    arr.set_trailing(format!("{}{rest}", char::from(10u8)));
    arr.set_trailing_comma(true);
}

/// Append `pattern` to `allowed_commands` of the profile file at `path`, preserving comments.
pub fn add_to_profile(path: &Path, scope: ProfileScope, pattern: &str) -> Result<Added, String> {
    if let Some(why) = pattern_problem(pattern) {
        return Err(format!("the pattern {pattern:?} {why}"));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read the profile {}: {e}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("the profile {} is not valid TOML: {e}", path.display()))?;
    let item = doc
        .entry("allowed_commands")
        .or_insert_with(|| toml_edit::value(toml_edit::Array::new()));
    let arr = item
        .as_array_mut()
        .ok_or_else(|| "`allowed_commands` of the profile is not a list".to_string())?;
    if arr
        .iter()
        .any(|v| v.as_str().is_some_and(|s| same(s, pattern)))
    {
        return Ok(Added::AlreadyThere);
    }
    push_keeping_layout(arr, pattern);
    let new_text = doc.to_string();
    // the file must still be a valid profile with the pattern in it
    let parsed = ProfileFile::parse(&new_text)
        .map_err(|e| format!("the edited profile would not parse: {e}"))?;
    let profile: Profile = parsed
        .validate(path, scope)
        .map_err(|e| format!("the edited profile would not validate: {e}"))?;
    if !profile.allowed_commands.iter().any(|c| same(c, pattern)) {
        return Err("the pattern did not end up in the profile".into());
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, new_text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })?;
    Ok(Added::Added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> OwnedScope {
        OwnedScope::new(Path::new(r"C:\proj"), Path::new(r"C:\proj"))
    }

    #[test]
    fn commands_become_binary_subcommand_star() {
        let s = scope();
        assert_eq!(
            normalize_pattern("cargo test -p x", &s).unwrap(),
            "cargo test *"
        );
        assert_eq!(
            normalize_pattern("git   diff --stat", &s).unwrap(),
            "git diff *"
        );
        assert_eq!(normalize_pattern("npm run build", &s).unwrap(), "npm run *");
        assert_eq!(normalize_pattern("ls", &s).unwrap(), "ls");
        assert_eq!(
            normalize_pattern("cargo build", &s).unwrap(),
            "cargo build *"
        );
    }

    #[test]
    fn what_cannot_be_generalized_is_refused() {
        let s = scope();
        for cmd in [
            "cargo test && rm -rf x",
            "cargo test | tee out",
            "cargo build > out.txt",
            "git push origin main",
            "git -C x status",
            "cargo --version",
            "powershell -c ls",
            "python script.py",
            "rm -rf build",
            "",
            "$(whoami)",
        ] {
            assert!(normalize_pattern(cmd, &s).is_err(), "should refuse: {cmd}");
        }
    }

    const PROFILE: &str = "# my tests profile\nname = \"Tests\"\nagent = \"codex\"\n\n# only cargo\nallowed_commands = [\n  \"cargo build *\", # build\n]\n\n[extra]\nkeep = 1\n";

    #[test]
    fn the_pattern_is_added_keeping_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.toml");
        std::fs::write(&p, PROFILE).unwrap();
        assert!(
            add_to_profile(&p, ProfileScope::Project, "cargo test *").is_err(),
            "a profile with unknown keys is left alone"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), PROFILE);
    }

    #[test]
    fn comments_and_layout_survive() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.toml");
        let text = "# my tests profile\nname = \"Tests\"\nagent = \"codex\"\n\n# only cargo\nallowed_commands = [\n  \"cargo build *\", # build\n]\n";
        std::fs::write(&p, text).unwrap();
        assert_eq!(
            add_to_profile(&p, ProfileScope::Project, "cargo test *").unwrap(),
            Added::Added
        );
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(
            after.contains("# my tests profile")
                && after.contains("# only cargo")
                && after.contains("# build"),
            "{after}"
        );
        let prof = Profile::from_toml(&after, &p, ProfileScope::Project).unwrap();
        assert_eq!(prof.allowed_commands, ["cargo build *", "cargo test *"]);
        // the comment stays with its item and the new one gets its own line
        assert!(
            after.contains(
                "\"cargo build *\", # build
  \"cargo test *\",
]"
            ),
            "{after:?}"
        );
        // again: nothing changes
        assert_eq!(
            add_to_profile(&p, ProfileScope::Project, "Cargo  test *").unwrap(),
            Added::AlreadyThere
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), after);
    }

    #[test]
    fn a_profile_without_the_key_gets_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.toml");
        std::fs::write(
            &p,
            "name = \"T\"\nagent = \"codex\"\ninstructions = \"\"\"\nhi\n\"\"\"\n",
        )
        .unwrap();
        add_to_profile(&p, ProfileScope::Global, "rg *").unwrap();
        let prof = Profile::load(&p, ProfileScope::Global).unwrap();
        assert_eq!(prof.allowed_commands, ["rg *"]);
        assert!(std::fs::read_to_string(&p).unwrap().contains("hi"));
    }

    #[test]
    fn bad_patterns_and_files_are_refused_and_nothing_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.toml");
        assert!(add_to_profile(&missing, ProfileScope::Project, "cargo *").is_err());
        assert!(!missing.exists());
        let p = dir.path().join("t.toml");
        std::fs::write(&p, "name = \"T\"\nagent = \"codex\"\n").unwrap();
        assert!(add_to_profile(&p, ProfileScope::Project, "*").is_err());
        assert!(add_to_profile(&p, ProfileScope::Project, "a | b").is_err());
    }
}
