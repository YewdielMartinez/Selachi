//! Short task titles for the map (PLAN section 21.2, ola 11): what a delegation is *about*, in a
//! few words, instead of the first raw line of its prompt (`agy: Repo: C:\Users\vent…`).
//!
//! The title of a run is, in order:
//! 1. the first meaningful sentence of its prompt (boilerplate skipped: `Repo: …`, `HARD RULES`
//!    blocks, `=== SELACHI PROFILE … ===` blocks, code fences, bare paths, `Tarea:` prefixes);
//! 2. the name of its profile;
//! 3. the stored thread title without the `agent: ` prefix the harness adds;
//! 4. `Tarea de <agente>`.
//!
//! The full prompt is always one click away (inspector, tab Prompt). Pure and unit-tested.

/// Longest title (characters, ellipsis included).
pub const MAX_TITLE_CHARS: usize = 64;

/// What a title is derived from. Everything is optional: the registry may be missing, a thread may
/// be gone, the agent may be unknown.
#[derive(Clone, Copy, Debug, Default)]
pub struct TitleInput<'a> {
    /// The (redacted) prompt the run was given.
    pub prompt: Option<&'a str>,
    /// Profile of the run.
    pub profile: Option<&'a str>,
    /// Title stored in the chat thread (`agy: Repo: C:\Users\vent…` for harness runs).
    pub thread_title: Option<&'a str>,
    /// Agent id or name (`agy`, `Codex`).
    pub agent: &'a str,
}

/// The short title of a delegated run. Never empty.
pub fn task_title(input: &TitleInput<'_>) -> String {
    if let Some(t) = input.prompt.and_then(meaningful_sentence) {
        return t;
    }
    if let Some(p) = input.profile.map(str::trim).filter(|p| !p.is_empty()) {
        return clip(p, MAX_TITLE_CHARS);
    }
    if let Some(t) = input
        .thread_title
        .map(strip_agent_prefix)
        .filter(|t| !t.is_empty() && !is_boilerplate(&strip_markup(t)))
        .map(|t| clip(&strip_markup(&t), MAX_TITLE_CHARS))
        .filter(|t| !t.is_empty())
    {
        return t;
    }
    let name = crate::tree::agent_display(input.agent);
    if name.is_empty() {
        "Tarea delegada".to_string()
    } else {
        format!("Tarea de {name}")
    }
}

/// `agy: Repo: C:\x` -> `Repo: C:\x`: the `<agent>: ` the harness puts before the first prompt line.
pub fn strip_agent_prefix(title: &str) -> String {
    let t = title.trim();
    if let Some((head, rest)) = t.split_once(": ") {
        let h = head.trim().to_lowercase();
        let known = matches!(
            h.as_str(),
            "claude" | "claude code" | "codex" | "opencode" | "gemini" | "agy" | "antigravity"
        ) || h.starts_with("acp:");
        if known {
            return rest.trim().to_string();
        }
    }
    t.to_string()
}

/// The title of an agent CLI started outside Selachi, from its command line (`agy -p "revisa los
/// tests de auth" --effort medium` -> `Revisa los tests de auth`). `None` when no prompt is found.
pub fn external_title(cmdline: &str) -> Option<String> {
    let tokens = shell_tokens(cmdline);
    let flagged = tokens.windows(2).find_map(|w| {
        matches!(w[0].as_str(), "-p" | "--prompt" | "--message" | "--print").then(|| w[1].clone())
    });
    let prompt = flagged.or_else(|| {
        tokens
            .iter()
            .skip(1)
            .filter(|t| t.contains(' ') && !t.starts_with('-'))
            .max_by_key(|t| t.len())
            .cloned()
    })?;
    meaningful_sentence(&prompt)
}

/// Splits a command line the way a shell would (quotes group, no escapes beyond `\"`).
fn shell_tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `text` clipped to `max` characters at a word boundary, with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let cut: String = t.chars().take(max.saturating_sub(1)).collect();
    let at_word = cut
        .rfind(char::is_whitespace)
        .filter(|i| cut[..*i].chars().count() >= max / 2);
    let cut = match at_word {
        Some(i) => cut[..i].trim_end().to_string(),
        None => cut,
    };
    format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))
}

/// Strips the markdown a line may start with (`# `, `- `, `* `, `> `, `1. `, `**`, backticks).
fn strip_markup(line: &str) -> String {
    let mut s = line.trim();
    loop {
        let before = s;
        s = s.trim_start_matches(['#', '>', '*', '-', '•', ' ', '\t']);
        // `1.` / `2)` list numbers
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && digits <= 2 {
            let rest = &s[digits..];
            if let Some(r) = rest
                .strip_prefix(". ")
                .or_else(|| rest.strip_prefix(") "))
            {
                s = r;
            }
        }
        if s == before {
            break;
        }
    }
    s.replace("**", "")
        .replace('`', "")
        .trim()
        .trim_matches(['"', '\'', '«', '»'])
        .trim()
        .to_string()
}

/// A line that carries no task: instructions about the environment, a path, a marker.
fn is_boilerplate(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    if l.is_empty() {
        return true;
    }
    const STARTS: &[&str] = &[
        "repo:",
        "repo :",
        "repository:",
        "repositorio:",
        "cwd:",
        "directorio:",
        "directory:",
        "working directory",
        "project:",
        "proyecto:",
        "branch:",
        "rama:",
        "hard rules",
        "reglas",
        "rules:",
        "rules ",
        "=== ",
        "---",
        "***",
        "```",
        "<",
        "#!",
        "context:",
        "contexto:",
        "role:",
        "rol:",
        "model:",
        "modelo:",
    ];
    if STARTS.iter().any(|s| l.starts_with(s)) {
        return true;
    }
    // no letters at all: separators, numbers, symbols
    if !l.chars().any(char::is_alphabetic) {
        return true;
    }
    looks_like_path(&l)
}

/// A line that is only a path (`C:\Users\x\repo`, `/home/x/repo`, `.\src\lib.rs`).
fn looks_like_path(l: &str) -> bool {
    if l.contains(' ') {
        return false;
    }
    let b = l.as_bytes();
    let drive = b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    drive || l.starts_with('/') || l.starts_with(".\\") || l.starts_with("./") || l.starts_with("~/")
}

/// `Tarea: x` -> `x`; a bare `Tarea:` header -> empty (skip it).
fn strip_task_label(s: &str) -> Option<String> {
    let l = s.to_lowercase();
    for label in [
        "tu tarea:",
        "tarea:",
        "tarea -",
        "task:",
        "your task:",
        "objetivo:",
        "goal:",
        "pedido:",
        "request:",
        "encargo:",
        "prompt:",
    ] {
        if l.starts_with(label) {
            let rest = s[label.len()..].trim();
            return Some(rest.to_string());
        }
    }
    None
}

/// The first meaningful sentence of a prompt, short; `None` when everything is boilerplate.
pub fn meaningful_sentence(text: &str) -> Option<String> {
    let mut in_fence = false;
    // Skipping a `=== SELACHI PROFILE … ===` block (until `=== END`) or a `HARD RULES` list (until a
    // blank line).
    let mut skip_until_end = false;
    let mut skip_until_blank = false;
    for raw in text.lines() {
        let line = raw.trim();
        let lower = line.to_lowercase();
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if skip_until_end {
            if lower.starts_with("=== end") || lower.starts_with("=== fin") {
                skip_until_end = false;
            }
            continue;
        }
        if skip_until_blank {
            if line.is_empty() {
                skip_until_blank = false;
            }
            continue;
        }
        if lower.starts_with("=== selachi profile") || lower.starts_with("=== perfil") {
            skip_until_end = true;
            continue;
        }
        if lower.starts_with("hard rules") || lower.starts_with("reglas") {
            skip_until_blank = true;
            continue;
        }
        let stripped = strip_markup(line);
        if is_boilerplate(&stripped) || is_boilerplate(line) {
            continue;
        }
        let candidate = match strip_task_label(&stripped) {
            Some(rest) if rest.is_empty() => continue,
            Some(rest) => rest,
            None => stripped,
        };
        let sentence = first_sentence(&candidate);
        if sentence.chars().filter(|c| c.is_alphabetic()).count() < 3 {
            continue;
        }
        return Some(clip(&upper_first(&sentence), MAX_TITLE_CHARS));
    }
    None
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if f.is_lowercase() => f.to_uppercase().chain(c).collect(),
        _ => s.to_string(),
    }
}

/// Up to the first `.`, `?` or `!` that ends a sentence (followed by a space or the end).
fn first_sentence(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if matches!(c, '.' | '?' | '!' | '…') && chars.get(i + 1).is_none_or(|n| n.is_whitespace())
        {
            // `Dr.`-like and file extensions (`a.rs`) do not end a sentence: only at a space or end
            if i >= 8 || chars.get(i + 1).is_none() {
                return chars[..i].iter().collect::<String>().trim().to_string();
            }
        }
    }
    line.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(prompt: &str) -> String {
        task_title(&TitleInput {
            prompt: Some(prompt),
            agent: "agy",
            ..TitleInput::default()
        })
    }

    #[test]
    fn a_plain_prompt_gives_its_first_sentence() {
        assert_eq!(
            t("Revisa los tests de src/auth y añade los que falten. No toques producción."),
            "Revisa los tests de src/auth y añade los que falten"
        );
        assert_eq!(t("revisa la migración"), "Revisa la migración");
    }

    #[test]
    fn repo_hard_rules_and_profile_boilerplate_are_skipped() {
        let prompt = "Repo: C:\\Users\\venta\\Desktop\\Terminal\nHARD RULES\n- No edites nada\n- Solo lectura\n\nAudita el manejo del parámetro state en el callback OAuth. Cita archivo y línea.";
        assert_eq!(t(prompt), "Audita el manejo del parámetro state en el callback OAuth");

        let prompt = "=== SELACHI PROFILE: Revisor ===\nSolo lectura. Cita archivo y línea.\n=== END SELACHI PROFILE ===\n\n=== TASK ===\nEscribe la migración de sesiones";
        assert_eq!(t(prompt), "Escribe la migración de sesiones");
    }

    #[test]
    fn task_labels_fences_paths_and_markdown_are_cleaned() {
        assert_eq!(t("Tarea: documenta los endpoints"), "Documenta los endpoints");
        assert_eq!(t("TAREA:\nrevisa los logs"), "Revisa los logs");
        assert_eq!(t("```\nsecreto\n```\n# **Corrige** el lint"), "Corrige el lint");
        assert_eq!(t("C:\\Users\\x\\repo\n1. Lista los TODO del repo"), "Lista los TODO del repo");
    }

    #[test]
    fn long_titles_are_clipped_at_a_word() {
        let long = t("Investiga por qué el proceso de sincronización de sesiones falla de forma intermitente cuando hay muchos agentes");
        assert!(long.chars().count() <= MAX_TITLE_CHARS, "{long}");
        assert!(long.ends_with('…'));
    }

    #[test]
    fn without_a_usable_prompt_the_profile_then_the_thread_title_then_the_agent() {
        let only_boilerplate = "Repo: C:\\x\nHARD RULES\n- a\n";
        let with_profile = TitleInput {
            prompt: Some(only_boilerplate),
            profile: Some("Revisor de seguridad"),
            agent: "agy",
            ..TitleInput::default()
        };
        assert_eq!(task_title(&with_profile), "Revisor de seguridad");
        let with_thread = TitleInput {
            prompt: None,
            thread_title: Some("agy: Corrige el lint del módulo"),
            agent: "agy",
            ..TitleInput::default()
        };
        assert_eq!(task_title(&with_thread), "Corrige el lint del módulo");
        let raw_thread = TitleInput {
            thread_title: Some("agy: Repo: C:\\Users\\vent"),
            agent: "agy",
            ..TitleInput::default()
        };
        assert_eq!(task_title(&raw_thread), "Tarea de Antigravity");
        assert_eq!(
            task_title(&TitleInput {
                agent: "",
                ..TitleInput::default()
            }),
            "Tarea delegada"
        );
    }

    #[test]
    fn the_agent_prefix_of_stored_titles_is_removed_only_for_agents() {
        assert_eq!(strip_agent_prefix("agy: Repo: C:\\x"), "Repo: C:\\x");
        assert_eq!(strip_agent_prefix("Codex: arregla"), "arregla");
        assert_eq!(strip_agent_prefix("Nota: algo"), "Nota: algo");
    }

    #[test]
    fn external_agents_are_named_after_their_prompt_argument() {
        assert_eq!(
            external_title("agy -p \"revisa los tests de auth\" --effort medium").as_deref(),
            Some("Revisa los tests de auth")
        );
        assert_eq!(
            external_title("codex exec --model gpt-5 'Lista los TODO. Luego ordénalos'").as_deref(),
            Some("Lista los TODO")
        );
        assert_eq!(external_title("agy --version"), None);
    }
}
