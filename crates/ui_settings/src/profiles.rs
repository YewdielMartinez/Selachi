//! Subagent profiles (PLAN section 21.3): the data model, the TOML reader / writer, validation,
//! clamping against the Delegación limits, the "final prompt" preview and the starter templates.
//! Pure (no gpui): everything here is unit-tested.
//!
//! # File format (must stay aligned with `agent_harness`'s parser)
//!
//! One profile per file: `%APPDATA%\Selachi\agents\*.toml` (global) and
//! `<project>\.selachi\agents\*.toml` (per project, versionable). Flat keys:
//!
//! ```toml
//! name = "Revisor de seguridad"          # required; unique per scope
//! description = "Busca vulnerabilidades"
//! agent = "claude"                       # claude | codex | opencode | gemini | agy | acp:<command>
//! model = "opus"                         # optional
//! effort = "high"                        # optional: low | medium | high | xhigh | max
//! permission = "read-only"               # read-only | ask | accept-edits  (never above Delegación)
//! worktree = false
//! timeout = 900                          # seconds (a "15m" / "90s" string is also read)
//! budget_tokens = 200000                 # billable tokens of one run
//! allowed_tools = ["Read", "Grep"]       # if the agent supports it
//! denied_tools = ["Bash"]
//! context_files = ["docs/PLAN.md"]       # project-relative; attached to the prompt
//! output_format = "text"                 # text | json | diff
//! instructions = """
//! Markdown: the system prompt or prefix.
//! """
//! ```
//!
//! Unknown keys are an error, exactly like the harness (`deny_unknown_fields`): a typo must not silently
//! drop a restriction. `output_schema` (json only) is kept on round trips.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Agents a profile can name (`acp:<command>` is also accepted).
pub const AGENT_IDS: [&str; 5] = ["claude", "codex", "opencode", "gemini", "agy"];
/// Reasoning efforts.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Spanish label of a reasoning effort (the value stored in the file stays the raw one).
pub fn effort_label(effort: &str) -> &str {
    match effort {
        "low" => "Bajo",
        "medium" => "Medio",
        "high" => "Alto",
        "xhigh" => "Muy alto",
        "max" => "Máximo",
        other => other,
    }
}
/// Permissions a profile may ask for (`full` can never be delegated).
pub const PERMISSIONS: [&str; 3] = ["read-only", "ask", "accept-edits"];
/// Output formats.
pub const OUTPUT_FORMATS: [&str; 3] = ["text", "json", "diff"];
/// Longest instructions text.
pub const MAX_INSTRUCTIONS: usize = 32_000;
/// Most context files one profile can attach.
pub const MAX_CONTEXT_FILES: usize = 32;

/// Where a profile file lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// `%APPDATA%\Selachi\agents`.
    Global,
    /// `<project>\.selachi\agents`.
    Project,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => "Global",
            Scope::Project => "Proyecto",
        }
    }
}

/// A subagent profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub description: String,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission: String,
    pub worktree: bool,
    /// Seconds.
    pub timeout: Option<u64>,
    pub budget_tokens: Option<u64>,
    pub instructions: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub context_files: Vec<String>,
    pub output_format: String,
    /// JSON Schema file (relative to the profile file's folder); only with `output_format = "json"`.
    pub output_schema: Option<String>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            agent: "claude".into(),
            model: None,
            effort: None,
            permission: "read-only".into(),
            worktree: false,
            timeout: None,
            budget_tokens: None,
            instructions: String::new(),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            context_files: Vec::new(),
            output_format: "text".into(),
            output_schema: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------------------------------

fn escape_basic(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", escape_basic(s))
}

fn multiline(s: &str) -> String {
    let mut body = String::with_capacity(s.len());
    for ch in s.replace("\r\n", "\n").chars() {
        match ch {
            '\\' => body.push_str("\\\\"),
            '\n' | '\t' => body.push(ch),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                body.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => body.push(c),
        }
    }
    // three quotes in a row would end the string: break the run
    let body = body.replace("\"\"\"", "\"\"\\\"");
    let trailing = if body.ends_with('\n') { "" } else { "\n" };
    format!("\"\"\"\n{body}{trailing}\"\"\"")
}

fn list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|i| quoted(i)).collect();
    format!("[{}]", parts.join(", "))
}

/// The file text of a profile (stable key order, empty optionals left out).
pub fn to_toml(p: &Profile) -> String {
    let mut out = String::new();
    out.push_str(&format!("name = {}\n", quoted(&p.name)));
    if !p.description.trim().is_empty() {
        out.push_str(&format!("description = {}\n", quoted(p.description.trim())));
    }
    out.push_str(&format!("agent = {}\n", quoted(&p.agent)));
    if let Some(m) = p.model.as_deref().filter(|m| !m.trim().is_empty()) {
        out.push_str(&format!("model = {}\n", quoted(m.trim())));
    }
    if let Some(e) = p.effort.as_deref().filter(|e| !e.trim().is_empty()) {
        out.push_str(&format!("effort = {}\n", quoted(e.trim())));
    }
    out.push_str(&format!("permission = {}\n", quoted(&p.permission)));
    out.push_str(&format!("worktree = {}\n", p.worktree));
    if let Some(t) = p.timeout {
        out.push_str(&format!("timeout = {t}\n"));
    }
    if let Some(b) = p.budget_tokens {
        out.push_str(&format!("budget_tokens = {b}\n"));
    }
    if !p.allowed_tools.is_empty() {
        out.push_str(&format!("allowed_tools = {}\n", list(&p.allowed_tools)));
    }
    if !p.denied_tools.is_empty() {
        out.push_str(&format!("denied_tools = {}\n", list(&p.denied_tools)));
    }
    if !p.context_files.is_empty() {
        out.push_str(&format!("context_files = {}\n", list(&p.context_files)));
    }
    out.push_str(&format!("output_format = {}\n", quoted(&p.output_format)));
    if let Some(s) = p.output_schema.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("output_schema = {}\n", quoted(s.trim())));
    }
    if !p.instructions.trim().is_empty() {
        out.push_str(&format!(
            "instructions = {}\n",
            multiline(p.instructions.trim_end())
        ));
    }
    out
}

fn str_field(t: &toml::Table, key: &str) -> Result<Option<String>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` debe ser texto")),
    }
}

fn list_field(t: &toml::Table, key: &str) -> Result<Vec<String>, String> {
    match t.get(key) {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                toml::Value::String(s) => Ok(s.clone()),
                _ => Err(format!("`{key}` solo admite textos")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` debe ser una lista de textos")),
    }
}

fn count_field(t: &toml::Table, key: &str) -> Result<Option<u64>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n >= 0 => Ok(Some(*n as u64)),
        Some(toml::Value::Integer(_)) => Err(format!("`{key}` no puede ser negativo")),
        Some(toml::Value::String(s)) if key == "timeout" => {
            parse_duration_text(s).map(Some).ok_or_else(|| {
                format!("`timeout` no se entiende: «{s}» (usa segundos, «15m» o «90s»)")
            })
        }
        Some(_) => Err(format!("`{key}` debe ser un número")),
    }
}

/// `"15m"`, `"90s"`, `"1h"` or plain seconds.
pub fn parse_duration_text(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mult) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else {
        (s.as_str(), 1)
    };
    num.trim().parse::<u64>().ok().map(|n| n * mult)
}

/// Keys a profile file may have (the harness parses with `deny_unknown_fields`).
pub const KNOWN_KEYS: [&str; 15] = [
    "name",
    "description",
    "agent",
    "model",
    "effort",
    "permission",
    "worktree",
    "timeout",
    "budget_tokens",
    "instructions",
    "allowed_tools",
    "denied_tools",
    "context_files",
    "output_format",
    "output_schema",
];

/// Reads a profile from the text of a `.toml` file.
pub fn from_toml(text: &str) -> Result<Profile, String> {
    let t: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| format!("TOML no válido: {}", e.message()))?;
    // Like the harness: an unknown key is an error (a typo must not silently drop a restriction).
    if let Some(k) = t.keys().find(|k| !KNOWN_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "clave desconocida `{k}` (el harness rechaza los archivos con claves que no conoce)"
        ));
    }
    let d = Profile::default();
    Ok(Profile {
        name: str_field(&t, "name")?.unwrap_or_default(),
        description: str_field(&t, "description")?.unwrap_or_default(),
        agent: str_field(&t, "agent")?.unwrap_or(d.agent),
        model: str_field(&t, "model")?.filter(|m| !m.trim().is_empty()),
        effort: str_field(&t, "effort")?.filter(|m| !m.trim().is_empty()),
        permission: str_field(&t, "permission")?
            .map(|p| p.trim().to_ascii_lowercase().replace('_', "-"))
            .unwrap_or(d.permission),
        worktree: match t.get("worktree") {
            None => false,
            Some(toml::Value::Boolean(b)) => *b,
            Some(_) => return Err("`worktree` debe ser true o false".into()),
        },
        timeout: count_field(&t, "timeout")?,
        budget_tokens: count_field(&t, "budget_tokens")?,
        instructions: str_field(&t, "instructions")?
            .map(|s| s.trim_end().to_string())
            .unwrap_or_default(),
        allowed_tools: list_field(&t, "allowed_tools")?,
        denied_tools: list_field(&t, "denied_tools")?,
        context_files: list_field(&t, "context_files")?,
        output_format: str_field(&t, "output_format")?.unwrap_or(d.output_format),
        output_schema: str_field(&t, "output_schema")?.filter(|s| !s.trim().is_empty()),
    })
}

/// Explicit conversion of a Claude Code subagent (`.claude/agents/*.md`: frontmatter + body).
/// Read-only: the profile asks for `read-only` whatever the original allowed, and the user reviews it.
pub fn from_claude_md(text: &str, fallback_name: &str) -> Result<Profile, String> {
    let text = text.replace("\r\n", "\n");
    let rest = text
        .strip_prefix("---\n")
        .ok_or("El archivo no empieza con un frontmatter (---)")?;
    let (front, body) = rest
        .split_once("\n---")
        .ok_or("Falta el cierre (---) del frontmatter")?;
    let body = body.trim_start_matches('-').trim_start_matches('\n');
    let mut p = Profile {
        name: fallback_name.to_string(),
        agent: "claude".into(),
        permission: "read-only".into(),
        instructions: body.trim().to_string(),
        ..Profile::default()
    };
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').trim();
        match k.trim() {
            "name" if !v.is_empty() => p.name = v.to_string(),
            "description" => p.description = v.to_string(),
            "model" if !v.is_empty() && v != "inherit" => p.model = Some(v.to_string()),
            "tools" => {
                p.allowed_tools = v
                    .trim_matches(['[', ']'])
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            _ => {}
        }
    }
    Ok(p)
}

// ---------------------------------------------------------------------------------------------
// Names and paths
// ---------------------------------------------------------------------------------------------

/// File stem for a profile name (`"Revisor de seguridad"` -> `revisor-de-seguridad`).
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.trim().chars() {
        let ch = match ch {
            'á' | 'à' | 'ä' | 'â' | 'Á' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'Ó' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' => 'u',
            'ñ' | 'Ñ' => 'n',
            c => c,
        };
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "perfil".into() } else { out }
}

/// Global profiles folder: `<data dir>\agents`.
pub fn global_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agents")
}

/// Project profiles folder: `<project>\.selachi\agents`.
pub fn project_dir(project_root: &Path) -> PathBuf {
    project_root.join(".selachi").join("agents")
}

/// The project that contains `cwd`: the nearest folder with `.git` or `.selachi`, else `cwd`.
pub fn find_project_root(cwd: &Path) -> PathBuf {
    let mut cur = Some(cwd);
    while let Some(dir) = cur {
        if dir.join(".git").exists() || dir.join(".selachi").exists() {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    cwd.to_path_buf()
}

/// A profile found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub scope: Scope,
    pub path: PathBuf,
    /// The parsed profile, or what is wrong with the file.
    pub profile: Result<Profile, String>,
}

impl Entry {
    pub fn title(&self) -> String {
        match &self.profile {
            Ok(p) if !p.name.trim().is_empty() => p.name.clone(),
            _ => self
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// Reads every `*.toml` of `dir`, sorted by file name. A missing folder is empty.
pub fn scan_dir(dir: &Path, scope: Scope) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let profile = std::fs::read_to_string(&path)
                .map_err(|e| format!("no se pudo leer: {e}"))
                .and_then(|t| from_toml(&t));
            Entry {
                scope,
                path,
                profile,
            }
        })
        .collect()
}

/// Writes `p` as `<dir>/<slug>.toml` (atomically: temp file + rename). Returns the path.
pub fn write_profile(dir: &Path, p: &Profile) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.toml", slug(&p.name)));
    let tmp = dir.join(format!("{}.toml.tmp", slug(&p.name)));
    std::fs::write(&tmp, to_toml(p))?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Relative files of the project for the context picker: bounded depth and count, skipping
/// hidden folders, `target`, `node_modules` and the like. Always inside `root`.
pub fn project_files(root: &Path, limit: usize) -> Vec<String> {
    const SKIP: [&str; 8] = [
        "target",
        "node_modules",
        "dist",
        "build",
        "bin",
        "obj",
        "vendor",
        "__pycache__",
    ];
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < 4 && !SKIP.contains(&name.as_str()) {
                    stack.push((e.path(), depth + 1));
                }
            } else if ft.is_file()
                && let Ok(rel) = e.path().strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= limit {
                    out.sort();
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

/// A problem with one field of a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// `name`, `agent`, `timeout`...
    pub field: &'static str,
    pub message: String,
}

fn issue(field: &'static str, message: impl Into<String>) -> Issue {
    Issue {
        field,
        message: message.into(),
    }
}

/// Whether `path` is a safe project-relative path: no drive, no leading slash, no `..`.
pub fn context_path_error(path: &str) -> Option<&'static str> {
    let p = path.trim();
    if p.is_empty() {
        return Some("La ruta está vacía.");
    }
    let norm = p.replace('\\', "/");
    if norm.starts_with('/') || norm.starts_with("//") || norm.chars().nth(1) == Some(':') {
        return Some("Debe ser una ruta relativa al proyecto.");
    }
    if norm.split('/').any(|c| c == "..") {
        return Some("No puede salir de la carpeta del proyecto (..).");
    }
    None
}

/// Turns what the user picked or typed into the stored form: forward slashes, relative to
/// `project_root` when it is an absolute path inside it. `None` when it points outside.
pub fn normalize_context_path(input: &str, project_root: Option<&Path>) -> Option<String> {
    let p = input.trim().replace('\\', "/");
    if let Some(root) = project_root {
        let root_s = root.to_string_lossy().replace('\\', "/");
        let root_s = root_s.trim_end_matches('/');
        if p.len() > root_s.len()
            && p[..root_s.len()].eq_ignore_ascii_case(root_s)
            && p[root_s.len()..].starts_with('/')
        {
            return Some(p[root_s.len() + 1..].to_string());
        }
    }
    context_path_error(&p).is_none().then_some(p)
}

/// Every problem of `p`. `taken` are the names of the other profiles of the same scope.
pub fn validate(p: &Profile, taken: &[String]) -> Vec<Issue> {
    let mut v = Vec::new();
    let name = p.name.trim();
    if name.is_empty() {
        v.push(issue("name", "Escribe un nombre."));
    } else if name.chars().count() > 64 {
        v.push(issue("name", "Máximo 64 caracteres."));
    } else if name
        .chars()
        .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
    {
        v.push(issue("name", "No uses \\ / : * ? \" < > | en el nombre."));
    } else if taken.iter().any(|t| t.trim().eq_ignore_ascii_case(name)) {
        v.push(issue(
            "name",
            "Ya existe un perfil con ese nombre en este ámbito.",
        ));
    }
    let agent_ok = AGENT_IDS.contains(&p.agent.as_str())
        || p.agent
            .strip_prefix("acp:")
            .is_some_and(|c| !c.trim().is_empty());
    if !agent_ok {
        v.push(issue(
            "agent",
            "Elige un agente (claude, codex, opencode, gemini, agy) o acp:<comando>.",
        ));
    }
    if let Some(e) = p.effort.as_deref()
        && !EFFORTS.contains(&e)
    {
        v.push(issue(
            "effort",
            "Esfuerzo no válido (low, medium, high, xhigh o max).",
        ));
    }
    if p.permission == "full" {
        v.push(issue(
            "permission",
            "El acceso completo no se puede delegar. Elige accept-edits como máximo.",
        ));
    } else if !PERMISSIONS.contains(&p.permission.as_str()) {
        v.push(issue(
            "permission",
            "Permiso no válido (read-only, ask o accept-edits).",
        ));
    }
    if let Some(t) = p.timeout
        && !(10..=86_400).contains(&t)
    {
        v.push(issue("timeout", "Entre 10 segundos y 24 horas."));
    }
    if p.budget_tokens == Some(0) {
        v.push(issue(
            "budget_tokens",
            "Usa un número mayor que 0, o déjalo vacío para no limitar.",
        ));
    }
    if !OUTPUT_FORMATS.contains(&p.output_format.as_str()) {
        v.push(issue(
            "output_format",
            "Formato no válido (text, json o diff).",
        ));
    }
    if p.instructions.chars().count() > MAX_INSTRUCTIONS {
        v.push(issue(
            "instructions",
            format!("Máximo {MAX_INSTRUCTIONS} caracteres."),
        ));
    }
    let allow: BTreeSet<String> = p
        .allowed_tools
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    if let Some(both) = p
        .denied_tools
        .iter()
        .find(|t| allow.contains(&t.to_ascii_lowercase()))
    {
        v.push(issue(
            "denied_tools",
            format!("«{both}» está a la vez permitida y prohibida."),
        ));
    }
    if p.context_files.len() > MAX_CONTEXT_FILES {
        v.push(issue(
            "context_files",
            format!("Máximo {MAX_CONTEXT_FILES} archivos de contexto."),
        ));
    }
    if let Some(bad) = p
        .context_files
        .iter()
        .find(|f| context_path_error(f).is_some())
    {
        v.push(issue(
            "context_files",
            format!(
                "«{bad}»: {}",
                context_path_error(bad).unwrap_or("ruta no válida")
            ),
        ));
    }
    if let Some(glob) = p
        .context_files
        .iter()
        .find(|f| f.contains('*') || f.contains('?'))
    {
        v.push(issue(
            "context_files",
            format!("«{glob}»: no se admiten comodines, lista los archivos."),
        ));
    }
    if let Some(bad) = p
        .allowed_tools
        .iter()
        .chain(p.denied_tools.iter())
        .find(|t| t.trim().is_empty() || t.len() > 200 || t.trim().starts_with('-'))
    {
        v.push(issue(
            "allowed_tools",
            format!("«{bad}» no es un nombre de herramienta válido (no puede empezar con `-`)."),
        ));
    }
    if let Some(s) = p.output_schema.as_deref().filter(|s| !s.trim().is_empty()) {
        if p.output_format != "json" {
            v.push(issue(
                "output_schema",
                "El esquema solo se usa con el formato JSON.",
            ));
        } else if context_path_error(s).is_some() {
            v.push(issue(
                "output_schema",
                "Ruta relativa a la carpeta del perfil, sin salir de ella.",
            ));
        }
    }
    v
}

// ---------------------------------------------------------------------------------------------
// Limits (Delegación) and the dry run
// ---------------------------------------------------------------------------------------------

/// The Delegación limits a profile can never exceed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// `read-only | ask | accept-edits`.
    pub max_permission: String,
    pub timeout_secs: u64,
    pub max_depth: u32,
    pub daily_token_budget: u64,
}

impl Limits {
    /// From the `[delegation]` section of the config (`read_only` / `accept_edits` spellings accepted).
    pub fn from_delegation(d: &store::config::Delegation) -> Self {
        Self {
            max_permission: normalize_permission(&d.max_permission),
            timeout_secs: d.timeout_secs,
            max_depth: d.max_depth,
            daily_token_budget: d.daily_token_budget,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::from_delegation(&store::config::Delegation::default())
    }
}

/// `read_only` -> `read-only`; anything above `accept-edits` (`full`) is clamped to it, an unknown
/// value to `read-only`.
pub fn normalize_permission(p: &str) -> String {
    match p.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "read-only" | "readonly" => "read-only".into(),
        "ask" => "ask".into(),
        "accept-edits" | "full" => "accept-edits".into(),
        _ => "read-only".into(),
    }
}

fn rank(p: &str) -> usize {
    PERMISSIONS.iter().position(|x| *x == p).unwrap_or(0)
}

/// The permission the run gets: the lower of the profile's and the Delegación maximum. The flag says
/// whether it was lowered.
pub fn clamp_permission(requested: &str, max: &str) -> (String, bool) {
    let req = normalize_permission(requested);
    let max = normalize_permission(max);
    if rank(&req) > rank(&max) {
        (max, true)
    } else {
        (req, false)
    }
}

/// Spanish word of a permission.
pub fn permission_label(p: &str) -> &'static str {
    match p {
        "read-only" => "Solo lectura",
        "ask" => "Preguntar",
        "accept-edits" => "Aceptar ediciones",
        "full" => "Acceso completo",
        _ => "—",
    }
}

/// What a run of the profile would really get after the Delegación limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub permission: String,
    pub clamped: bool,
    /// The profile asked for no worktree but `accept-edits` only runs in one.
    pub worktree: bool,
    pub worktree_forced: bool,
    pub timeout_secs: u64,
    pub timeout_clamped: bool,
    pub budget_tokens: Option<u64>,
    pub depth: u32,
    pub notes: Vec<String>,
}

/// Applies the Delegación limits: the minimum wins, `accept-edits` forces a worktree.
pub fn effective(p: &Profile, limits: &Limits) -> Effective {
    let (permission, clamped) = clamp_permission(&p.permission, &limits.max_permission);
    let forced = permission == "accept-edits" && !p.worktree;
    let timeout_req = p.timeout.unwrap_or(limits.timeout_secs);
    let timeout_secs = timeout_req.min(limits.timeout_secs);
    let mut notes = Vec::new();
    if clamped {
        notes.push(format!(
            "El permiso se baja a «{}» porque Delegación no permite más.",
            permission_label(&permission)
        ));
    }
    if forced {
        notes.push("Con «Aceptar ediciones» la ejecución va siempre en un worktree propio.".into());
    }
    if timeout_secs < timeout_req {
        notes.push("El tiempo se limita al máximo de Delegación.".into());
    }
    if limits.daily_token_budget > 0 {
        notes.push(format!(
            "Además rige el presupuesto diario de Delegación ({} tokens).",
            limits.daily_token_budget
        ));
    }
    Effective {
        permission,
        clamped,
        worktree: p.worktree || forced,
        worktree_forced: forced,
        timeout_secs,
        timeout_clamped: timeout_secs < timeout_req,
        budget_tokens: p.budget_tokens,
        depth: limits.max_depth,
        notes,
    }
}

/// A context file as the dry run sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
    pub bytes: Option<u64>,
    /// Start of the content (the preview cuts long files; the harness attaches up to 200 KiB in total).
    pub preview: Option<String>,
}

/// Characters of each file the preview shows.
const PREVIEW_CHARS: usize = 1_200;

/// Reads the context files inside `project_root` (`None` bytes: missing or outside the project).
pub fn context_files(p: &Profile, project_root: Option<&Path>) -> Vec<ContextFile> {
    p.context_files
        .iter()
        .map(|f| {
            let path = project_root
                .filter(|_| context_path_error(f).is_none())
                .map(|r| r.join(f));
            let meta = path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .filter(|m| m.is_file());
            let preview = meta.as_ref().and_then(|_| {
                let text = std::fs::read_to_string(path.as_ref()?).ok()?;
                let cut: String = text.chars().take(PREVIEW_CHARS).collect();
                Some(if text.chars().count() > PREVIEW_CHARS {
                    format!("{cut}\n[… recortado en la vista previa]")
                } else {
                    cut
                })
            });
            ContextFile {
                path: f.clone(),
                bytes: meta.map(|m| m.len()),
                preview,
            }
        })
        .collect()
}

/// The prompt a run would send. **Same layout as `agent_harness::profiles::prepare::build_final_prompt`**
/// (delimited blocks: profile, context files, output format, advisory tool policy, task); with nothing
/// to add it is the task itself. Claude enforces the tool lists with command-line flags, so only the
/// other agents get the advisory block. Secrets are redacted. This crate does not depend on the
/// harness (it would pull the delegation engine into the settings UI), so the app crate keeps the two
/// equal with a parity test (`app_shell::profile_parity`): when the harness changes the layout, that test
/// fails until this follows.
pub fn final_prompt(p: &Profile, task: &str, files: &[ContextFile]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !p.instructions.trim().is_empty() {
        parts.push(format!(
            "=== SELACHI PROFILE: {} ===\n{}\n=== END SELACHI PROFILE ===",
            p.name.trim(),
            p.instructions.trim()
        ));
    }
    for f in files {
        let body = match (&f.preview, f.bytes) {
            (Some(t), _) => t.trim_end().to_string(),
            (None, Some(b)) => format!("[{}]", human_bytes(b)),
            (None, None) => "[no existe o está fuera del proyecto]".to_string(),
        };
        parts.push(format!(
            "=== CONTEXT FILE: {} ===\n{body}\n=== END CONTEXT FILE ===",
            f.path
        ));
    }
    let output = match p.output_format.as_str() {
        "json" => Some(match p.output_schema.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(s) => format!(
                "Reply with ONLY one JSON document: no prose before or after it and no code fences. \
                 It must be valid against this JSON Schema:\n[contenido de {s}]"
            ),
            None => "Reply with ONLY one valid JSON document: no prose before or after it and no code fences."
                .to_string(),
        }),
        "diff" => Some(
            "Reply with ONLY a unified diff (git format, with `diff --git` headers) of every change you \
             propose, and nothing else."
                .to_string(),
        ),
        _ => None,
    };
    if let Some(o) = output {
        parts.push(format!(
            "=== OUTPUT FORMAT ===\n{o}\n=== END OUTPUT FORMAT ==="
        ));
    }
    if p.agent != "claude" && (!p.allowed_tools.is_empty() || !p.denied_tools.is_empty()) {
        let mut s = String::from("This is not enforced by the runtime, so respect it yourself.");
        if !p.allowed_tools.is_empty() {
            s.push_str(&format!(
                " Use only these tools: {}.",
                p.allowed_tools.join(", ")
            ));
        }
        if !p.denied_tools.is_empty() {
            s.push_str(&format!(" Never use: {}.", p.denied_tools.join(", ")));
        }
        parts.push(format!(
            "=== TOOL POLICY (advisory) ===\n{s}\n=== END TOOL POLICY ==="
        ));
    }
    // The task goes through exactly as given, like the harness (which does not trim it either).
    let out = if parts.is_empty() {
        task.to_string()
    } else {
        parts.push(format!("=== TASK ===\n{task}"));
        parts.join("\n\n")
    };
    store::redact(&out).text
}

fn human_bytes(b: u64) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KB", b as f64 / 1024.0).replace('.', ",")
    } else {
        format!("{:.1} MB", b as f64 / 1_048_576.0).replace('.', ",")
    }
}

// ---------------------------------------------------------------------------------------------
// Starter templates
// ---------------------------------------------------------------------------------------------

/// The starter profiles. Writing them is always an explicit action of the user.
pub fn templates() -> Vec<Profile> {
    vec![
        Profile {
            name: "Revisor de seguridad".into(),
            description: "Busca vulnerabilidades y malas prácticas sin tocar el código.".into(),
            agent: "claude".into(),
            effort: Some("high".into()),
            permission: "read-only".into(),
            timeout: Some(900),
            instructions: "Eres un revisor de seguridad. Revisa el código indicado buscando inyecciones, \
                manejo inseguro de secretos, validación ausente y dependencias vulnerables. \
                Clasifica cada hallazgo por severidad, cita archivo y línea y propone el arreglo. \
                No modifiques nada."
                .into(),
            denied_tools: vec!["Bash".into(), "Edit".into(), "Write".into()],
            ..Profile::default()
        },
        Profile {
            name: "Tests".into(),
            description: "Escribe y ejecuta pruebas para el cambio indicado.".into(),
            agent: "codex".into(),
            permission: "accept-edits".into(),
            worktree: true,
            timeout: Some(1200),
            instructions: "Escribe pruebas para el comportamiento descrito, siguiendo el estilo de las \
                pruebas existentes. Ejecútalas y corrige solo las pruebas, no el código de producción. \
                Resume qué cubren y qué quedó sin cubrir."
                .into(),
            ..Profile::default()
        },
        Profile {
            name: "Docs".into(),
            description: "Redacta o actualiza documentación a partir del código.".into(),
            agent: "gemini".into(),
            permission: "accept-edits".into(),
            worktree: true,
            timeout: Some(900),
            instructions: "Escribe documentación clara y breve a partir del código: qué hace, cómo se usa \
                y un ejemplo. Respeta el idioma y el formato de los documentos existentes. No cambies código."
                .into(),
            output_format: "diff".into(),
            ..Profile::default()
        },
        Profile {
            name: "Investigador".into(),
            description: "Explora el repositorio y responde con evidencia, sin cambios.".into(),
            agent: "claude".into(),
            effort: Some("medium".into()),
            permission: "read-only".into(),
            timeout: Some(600),
            instructions: "Investiga la pregunta explorando el repositorio. Responde con hallazgos \
                concretos: archivo, línea y una frase de por qué importa. Si no encuentras evidencia, dilo."
                .into(),
            ..Profile::default()
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effort_has_a_spanish_label() {
        let labels: Vec<&str> = EFFORTS.iter().map(|e| effort_label(e)).collect();
        assert_eq!(labels, ["Bajo", "Medio", "Alto", "Muy alto", "Máximo"]);
        assert_eq!(effort_label("otro"), "otro");
    }

    fn full() -> Profile {
        Profile {
            name: "Revisor \"estricto\" \\ ñ".into(),
            description: "Línea 1".into(),
            agent: "acp:gemini --acp".into(),
            model: Some("opus".into()),
            effort: Some("high".into()),
            permission: "ask".into(),
            worktree: true,
            timeout: Some(900),
            budget_tokens: Some(200_000),
            instructions: "Primera línea.\n\n\"\"\"comillas\"\"\" y \\ barra\n\tcon tab\nfin"
                .into(),
            allowed_tools: vec!["Read".into(), "Grep".into()],
            denied_tools: vec!["Bash".into()],
            context_files: vec!["docs/PLAN.md".into(), "src/a b.rs".into()],
            output_format: "json".into(),
            output_schema: Some("schemas/out.json".into()),
        }
    }

    #[test]
    fn toml_round_trips_every_field() {
        let p = full();
        let text = to_toml(&p);
        assert!(text.contains("instructions = \"\"\""), "{text}");
        let back = from_toml(&text).unwrap();
        assert_eq!(
            back,
            Profile {
                instructions: p.instructions.clone(),
                ..p
            }
        );
    }

    #[test]
    fn a_minimal_profile_round_trips_and_defaults_apply() {
        let p = Profile {
            name: "Mínimo".into(),
            ..Profile::default()
        };
        let text = to_toml(&p);
        assert!(!text.contains("model"), "{text}");
        assert_eq!(from_toml(&text).unwrap(), p);
        let d = from_toml("name = \"x\"").unwrap();
        assert_eq!(
            (
                d.agent.as_str(),
                d.permission.as_str(),
                d.output_format.as_str()
            ),
            ("claude", "read-only", "text")
        );
    }

    #[test]
    fn reading_normalizes_and_is_strict_with_types() {
        let p = from_toml(
            "name = \"x\"
permission = \"READ_ONLY\"
timeout = \"15m\"",
        )
        .unwrap();
        assert_eq!(p.permission, "read-only");
        assert_eq!(p.timeout, Some(900));
        assert!(from_toml("name = 3").unwrap_err().contains("name"));
        assert!(from_toml("name = \"x\"\nallowed_tools = \"Read\"").is_err());
        assert!(from_toml("name = \"x\"\ntimeout = -5").is_err());
        assert!(from_toml("name = [").unwrap_err().contains("TOML"));
        assert_eq!(parse_duration_text("2h"), Some(7200));
        assert_eq!(parse_duration_text("abc"), None);
    }

    #[test]
    fn claude_agents_convert_explicitly_and_read_only() {
        let md = "---\nname: code-reviewer\ndescription: Reviews code\ntools: Read, Grep, Glob\nmodel: sonnet\n---\nYou are a reviewer.\nBe brief.\n";
        let p = from_claude_md(md, "fallback").unwrap();
        assert_eq!(p.name, "code-reviewer");
        assert_eq!(p.agent, "claude");
        assert_eq!(p.permission, "read-only");
        assert_eq!(p.allowed_tools, vec!["Read", "Grep", "Glob"]);
        assert_eq!(p.model.as_deref(), Some("sonnet"));
        assert!(p.instructions.starts_with("You are a reviewer."));
        assert!(from_claude_md("sin frontmatter", "x").is_err());
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(slug("Revisor de seguridad"), "revisor-de-seguridad");
        assert_eq!(slug("  Docs / API: v2 "), "docs-api-v2");
        assert_eq!(slug("ñandú"), "nandu");
        assert_eq!(slug("***"), "perfil");
    }

    #[test]
    fn validation_reports_each_field() {
        let ok = Profile {
            name: "Bien".into(),
            ..Profile::default()
        };
        assert!(validate(&ok, &[]).is_empty());
        let fields = |p: &Profile, taken: &[String]| -> Vec<&'static str> {
            validate(p, taken).into_iter().map(|i| i.field).collect()
        };
        assert_eq!(fields(&Profile::default(), &[]), vec!["name"]);
        assert_eq!(
            fields(&ok, &["bien".to_string()]),
            vec!["name"],
            "duplicate, case-insensitive"
        );
        let bad = Profile {
            name: "a/b".into(),
            agent: "nope".into(),
            effort: Some("extreme".into()),
            permission: "full".into(),
            timeout: Some(3),
            budget_tokens: Some(0),
            output_format: "xml".into(),
            allowed_tools: vec!["Bash".into()],
            denied_tools: vec!["bash".into()],
            context_files: vec!["../secrets.txt".into()],
            instructions: "x".repeat(MAX_INSTRUCTIONS + 1),
            ..Profile::default()
        };
        let got = fields(&bad, &[]);
        for f in [
            "name",
            "agent",
            "effort",
            "permission",
            "timeout",
            "budget_tokens",
            "output_format",
            "denied_tools",
            "context_files",
            "instructions",
        ] {
            assert!(got.contains(&f), "missing {f} in {got:?}");
        }
        assert!(
            validate(
                &Profile {
                    agent: "acp:".into(),
                    name: "x".into(),
                    ..Profile::default()
                },
                &[]
            )
            .iter()
            .any(|i| i.field == "agent")
        );
    }

    #[test]
    fn context_paths_stay_inside_the_project() {
        for bad in [
            "/etc/passwd",
            "C:\\Windows\\x",
            "..\\x",
            "a/../../b",
            "//srv/share",
            "",
        ] {
            assert!(context_path_error(bad).is_some(), "{bad}");
        }
        assert!(context_path_error("docs/PLAN.md").is_none());
        let root = Path::new("C:\\Users\\me\\proj");
        assert_eq!(
            normalize_context_path("C:\\Users\\me\\proj\\src\\a.rs", Some(root)).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            normalize_context_path("C:\\Users\\me\\other\\a.rs", Some(root)),
            None
        );
        assert_eq!(
            normalize_context_path("src\\a.rs", Some(root)).as_deref(),
            Some("src/a.rs")
        );
    }

    #[test]
    fn permission_is_clamped_to_delegation_max() {
        assert_eq!(
            clamp_permission("accept-edits", "ask"),
            ("ask".into(), true)
        );
        assert_eq!(
            clamp_permission("ask", "accept_edits"),
            ("ask".into(), false)
        );
        assert_eq!(
            clamp_permission("read-only", "read_only"),
            ("read-only".into(), false)
        );
        // full can never be delegated, whatever the max says
        assert_eq!(
            clamp_permission("full", "accept-edits"),
            ("accept-edits".into(), false)
        );
        assert_eq!(normalize_permission("full"), "accept-edits");
        assert_eq!(normalize_permission("???"), "read-only");
    }

    #[test]
    fn the_dry_run_applies_the_minimum_of_profile_and_delegation() {
        let limits = Limits {
            max_permission: "ask".into(),
            timeout_secs: 600,
            max_depth: 1,
            daily_token_budget: 0,
        };
        let p = Profile {
            name: "x".into(),
            permission: "accept-edits".into(),
            timeout: Some(3600),
            ..Profile::default()
        };
        let e = effective(&p, &limits);
        assert_eq!(e.permission, "ask");
        assert!(e.clamped && e.timeout_clamped);
        assert_eq!(e.timeout_secs, 600);
        assert!(!e.worktree_forced, "ask does not need a worktree");
        assert!(e.notes.len() >= 2);
        // accept-edits within the limits runs in a worktree, forced when the profile did not ask
        let open = Limits {
            max_permission: "accept-edits".into(),
            ..limits
        };
        let e = effective(&p, &open);
        assert!(e.worktree && e.worktree_forced && !e.clamped);
        // no timeout in the profile: the Delegación default
        let e = effective(
            &Profile {
                name: "x".into(),
                ..Profile::default()
            },
            &open,
        );
        assert_eq!(e.timeout_secs, 600);
        assert!(!e.timeout_clamped);
    }

    #[test]
    fn the_final_prompt_follows_the_harness_layout_and_redacts() {
        let p = Profile {
            name: "x".into(),
            agent: "codex".into(),
            instructions: "Sé breve. Clave: sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"
                .into(),
            allowed_tools: vec!["Read".into()],
            denied_tools: vec!["Bash".into()],
            context_files: vec!["a.md".into(), "falta.md".into()],
            output_format: "json".into(),
            ..Profile::default()
        };
        let files = vec![
            ContextFile {
                path: "a.md".into(),
                bytes: Some(2048),
                preview: Some("contenido de a\n".into()),
            },
            ContextFile {
                path: "falta.md".into(),
                bytes: None,
                preview: None,
            },
        ];
        let out = final_prompt(&p, "Revisa auth", &files);
        assert!(
            out.starts_with("=== SELACHI PROFILE: x ===\nSé breve."),
            "{out}"
        );
        assert!(out.contains("=== END SELACHI PROFILE ==="));
        assert!(
            out.contains("=== CONTEXT FILE: a.md ===\ncontenido de a\n=== END CONTEXT FILE ===")
        );
        assert!(out.contains("=== CONTEXT FILE: falta.md ===\n[no existe"));
        assert!(out.contains("=== OUTPUT FORMAT ===\nReply with ONLY one valid JSON document"));
        // codex cannot enforce the tool lists: advisory block; claude gets none
        assert!(out.contains("=== TOOL POLICY (advisory) ===") && out.contains("Never use: Bash."));
        let claude = Profile {
            agent: "claude".into(),
            ..p
        };
        assert!(!final_prompt(&claude, "t", &[]).contains("TOOL POLICY"));
        assert!(out.trim_end().ends_with("=== TASK ===\nRevisa auth"));
        assert!(!out.contains("sk-ant-api03"), "{out}");
        // nothing to add: the prompt is the task itself
        assert_eq!(
            final_prompt(&Profile::default(), "solo la tarea", &[]),
            "solo la tarea"
        );
    }

    #[test]
    fn unknown_keys_are_rejected_like_the_harness_does() {
        let err = from_toml("name = \"x\"\nunknwon = 1").unwrap_err();
        assert!(err.contains("unknwon"), "{err}");
        // output_schema survives a round trip
        let p = Profile {
            name: "J".into(),
            output_format: "json".into(),
            output_schema: Some("schemas/out.json".into()),
            ..Profile::default()
        };
        assert_eq!(from_toml(&to_toml(&p)).unwrap(), p);
        assert!(
            validate(
                &Profile {
                    output_schema: Some("a.json".into()),
                    ..p.clone()
                },
                &[]
            )
            .is_empty()
        );
        let bad = Profile {
            output_format: "text".into(),
            ..p
        };
        assert!(
            validate(&bad, &[])
                .iter()
                .any(|i| i.field == "output_schema")
        );
        let globs = Profile {
            name: "G".into(),
            context_files: vec!["src/*.rs".into()],
            allowed_tools: vec!["-x".into()],
            ..Profile::default()
        };
        let got: Vec<_> = validate(&globs, &[]).into_iter().map(|i| i.field).collect();
        assert!(got.contains(&"context_files") && got.contains(&"allowed_tools"));
    }

    #[test]
    fn templates_are_valid_distinct_and_never_above_accept_edits() {
        let t = templates();
        assert_eq!(t.len(), 4);
        let names: BTreeSet<_> = t.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names.len(), 4);
        for p in &t {
            assert!(
                validate(p, &[]).is_empty(),
                "{}: {:?}",
                p.name,
                validate(p, &[])
            );
            assert_ne!(p.permission, "full");
            if p.permission == "accept-edits" {
                assert!(p.worktree, "{} edits only in a worktree", p.name);
            }
            assert_eq!(from_toml(&to_toml(p)).unwrap(), *p);
        }
    }

    #[test]
    fn disk_round_trip_scan_and_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        let path = write_profile(&agents, &full()).unwrap();
        assert!(path.ends_with("revisor-estricto-n.toml"), "{path:?}");
        std::fs::write(agents.join("roto.toml"), "name = [").unwrap();
        std::fs::write(agents.join("nota.txt"), "x").unwrap();
        let found = scan_dir(&agents, Scope::Global);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|e| {
            e.profile
                .as_ref()
                .is_ok_and(|p| p.agent.starts_with("acp:"))
        }));
        assert!(found.iter().any(|e| e.profile.is_err()));
        assert!(scan_dir(&dir.path().join("none"), Scope::Project).is_empty());
        assert!(!agents.join("revisor-estricto-n.toml.tmp").exists());
    }

    #[test]
    fn project_root_and_file_listing_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("README.md"), "x").unwrap();
        std::fs::write(root.join("src/a.rs"), "x").unwrap();
        std::fs::write(root.join("target/junk.o"), "x").unwrap();
        std::fs::write(root.join(".hidden"), "x").unwrap();
        assert_eq!(find_project_root(&root.join("src/deep")), root);
        let files = project_files(root, 100);
        assert_eq!(files, vec!["README.md", "src/a.rs"]);
        assert_eq!(project_files(root, 1).len(), 1);
        let cf = context_files(
            &Profile {
                context_files: vec!["README.md".into(), "../x".into(), "no.md".into()],
                ..Profile::default()
            },
            Some(root),
        );
        assert_eq!(cf[0].bytes, Some(1));
        assert_eq!(cf[1].bytes, None);
        assert_eq!(cf[2].bytes, None);
    }
}
