//! Input and prompt for "Redactar con agente" (backlog B3): a commit message or a pull request
//! description drafted by the agent the user picked.
//!
//! Tokens are spent **only when the user clicks the button**: nothing here runs on its own.
//! The diff that goes into the prompt is capped ([`MAX_DIFF_BYTES`]) and redacted
//! ([`store::redact`]) before it leaves this module, and the prompt itself is a pure function
//! ([`draft_prompt`]) in Spanish or English following the language of the repository history
//! ([`detect_language`]).

use crate::error::Result;
use crate::exec::{run_git, LOCAL_TIMEOUT};
use crate::repo::Repo;

/// Largest diff sent to the agent.
pub const MAX_DIFF_BYTES: usize = 60 * 1024;
/// Recent subjects collected (language and style of the history).
pub const RECENT_SUBJECTS: usize = 15;

/// What is being drafted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DraftKind {
    /// The commit message of what is staged.
    Commit,
    /// The title and description of a pull request for the current branch.
    PullRequest,
}

/// Language of the repository history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Es,
    En,
}

/// Everything the prompt is built from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftInput {
    pub diff_stat: String,
    /// Redacted and capped at [`MAX_DIFF_BYTES`].
    pub diff: String,
    pub recent_subjects: Vec<String>,
}

impl DraftInput {
    /// There is nothing to describe.
    pub fn is_empty(&self) -> bool {
        self.diff.trim().is_empty() && self.diff_stat.trim().is_empty()
    }
}

const ES_WORDS: &[&str] = &[
    "el",
    "la",
    "los",
    "las",
    "de",
    "del",
    "para",
    "con",
    "sin",
    "por",
    "un",
    "una",
    "y",
    "en",
    "se",
    "añade",
    "agrega",
    "corrige",
    "actualiza",
    "elimina",
    "quita",
    "mejora",
    "cambia",
    "crea",
    "permite",
    "usa",
    "evita",
    "maneja",
    "muestra",
    "arregla",
    "ajusta",
    "mueve",
    "documenta",
    "implementa",
    "habilita",
    "al",
    "su",
    "que",
    "más",
    "nuevo",
    "nueva",
];
const EN_WORDS: &[&str] = &[
    "the",
    "of",
    "for",
    "with",
    "without",
    "and",
    "to",
    "in",
    "on",
    "add",
    "adds",
    "fix",
    "fixes",
    "update",
    "updates",
    "remove",
    "removes",
    "improve",
    "change",
    "create",
    "allow",
    "use",
    "avoid",
    "handle",
    "show",
    "move",
    "implement",
    "enable",
    "when",
    "from",
    "a",
    "an",
    "is",
    "are",
    "new",
    "support",
];

/// Spanish or English from the recent commit subjects: counts stop-words and verbs of each
/// language (the conventional-commit prefix is ignored). A tie, or no history, is Spanish (the
/// interface language).
pub fn detect_language(subjects: &[String]) -> Lang {
    let (mut es, mut en) = (0usize, 0usize);
    for subj in subjects {
        let text = subj
            .split_once(':')
            .filter(|(p, _)| {
                p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "()!-_/".contains(c))
            })
            .map_or(subj.as_str(), |(_, rest)| rest)
            .to_lowercase();
        if text.chars().any(|c| "áéíóúñü¿¡".contains(c)) {
            es += 2;
        }
        for w in text.split(|c: char| !c.is_alphabetic()) {
            if w.is_empty() {
                continue;
            }
            if ES_WORDS.contains(&w) {
                es += 1;
            }
            if EN_WORDS.contains(&w) {
                en += 1;
            }
        }
    }
    if en > es {
        Lang::En
    } else {
        Lang::Es
    }
}

/// The prompt sent to the agent. Pure: same inputs, same text. `diff` should already be redacted
/// and capped (see [`Repo::draft_input`]); it is embedded as is.
pub fn draft_prompt(
    kind: DraftKind,
    diff_stat: &str,
    diff: &str,
    recent_subjects: &[String],
) -> String {
    let lang = detect_language(recent_subjects);
    let es = lang == Lang::Es;
    let mut out = String::new();
    let (task, format) = match (kind, es) {
        (DraftKind::Commit, true) => (
            "Redacta el mensaje de commit para los cambios en staged que aparecen abajo.",
            "Formato: una primera línea de resumen (máximo 72 caracteres, en imperativo o como \
             la historia reciente), una línea en blanco y, solo si aporta, un cuerpo breve que \
             explique el porqué. Si la historia usa Conventional Commits (feat:, fix:, ...), \
             sigue esa convención.",
        ),
        (DraftKind::Commit, false) => (
            "Write the commit message for the staged changes below.",
            "Format: a first summary line (72 characters at most, imperative mood or matching \
             the recent history), a blank line and, only if useful, a short body explaining the \
             why. If the history uses Conventional Commits (feat:, fix:, ...), follow it.",
        ),
        (DraftKind::PullRequest, true) => (
            "Redacta el título y la descripción de un pull request para los cambios de esta \
             rama respecto a la rama base.",
            "Formato: la primera línea es el título (máximo 72 caracteres), luego una línea en \
             blanco y la descripción en Markdown con las secciones «Resumen» (qué y por qué), \
             «Cambios» (lista corta) y «Pruebas» (cómo se verificó, si se deduce del diff).",
        ),
        (DraftKind::PullRequest, false) => (
            "Write the title and description of a pull request for this branch's changes \
             against the base branch.",
            "Format: the first line is the title (72 characters at most), then a blank line and \
             a Markdown description with the sections \"Summary\" (what and why), \"Changes\" \
             (a short list) and \"Testing\" (how it was verified, if it can be inferred from the \
             diff).",
        ),
    };
    out.push_str(task);
    out.push_str("\n\n");
    out.push_str(format);
    out.push_str("\n\n");
    out.push_str(if es {
        "Responde SOLO con el texto del mensaje, en español, sin bloques de código, sin \
         comillas y sin explicaciones. No ejecutes comandos ni modifiques archivos: todo lo que \
         necesitas está en este mensaje."
    } else {
        "Reply ONLY with the message text, in English, with no code fences, no quotes and no \
         explanations. Do not run commands or modify files: everything you need is in this \
         message."
    });
    out.push_str("\n\n");
    if !recent_subjects.is_empty() {
        out.push_str(if es {
            "Asuntos de commits recientes (estilo del repositorio):\n"
        } else {
            "Recent commit subjects (repository style):\n"
        });
        for s in recent_subjects.iter().take(RECENT_SUBJECTS) {
            out.push_str("- ");
            out.push_str(s.trim());
            out.push('\n');
        }
        out.push('\n');
    }
    if !diff_stat.trim().is_empty() {
        out.push_str(if es {
            "Resumen del diff:\n"
        } else {
            "Diff summary:\n"
        });
        out.push_str("```\n");
        out.push_str(diff_stat.trim_end());
        out.push_str("\n```\n\n");
    }
    out.push_str("Diff:\n");
    out.push_str("```diff\n");
    out.push_str(diff.trim_end());
    out.push_str("\n```\n");
    out
}

/// Splits a drafted message into `(subject, body)`: the first non-empty line and what follows
/// the blank line after it. Code fences and surrounding quotes the agent may have added are
/// dropped.
pub fn split_message(text: &str) -> (String, String) {
    let mut t = text.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // ```text\n...\n```
        let rest = rest.split_once('\n').map_or("", |(_, r)| r);
        t = rest.trim_end().strip_suffix("```").unwrap_or(rest).trim();
    }
    let mut lines = t.lines();
    let subject = lines
        .by_ref()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .trim_matches(|c| c == '"' || c == '«' || c == '»')
        .trim_start_matches("# ")
        .to_owned();
    let body = lines.collect::<Vec<_>>().join("\n");
    (subject, body.trim().to_owned())
}

/// Caps `diff` to [`MAX_DIFF_BYTES`] at a line boundary (head kept, marker appended), then
/// redacts it.
pub fn cap_and_redact(diff: &str) -> String {
    let capped = if diff.len() > MAX_DIFF_BYTES {
        let mut end = MAX_DIFF_BYTES;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        let cut = diff[..end].rfind('\n').map_or(end, |i| i + 1);
        format!(
            "{}\n[... diff recortado a {} KB ...]\n",
            &diff[..cut],
            MAX_DIFF_BYTES / 1024
        )
    } else {
        diff.to_owned()
    };
    store::redact(&capped).text
}

impl Repo {
    /// The base branch a PR would target: `origin/HEAD`, else `origin/main|master`, else a local
    /// `main|master`. `None` when none exists.
    pub fn base_ref(&self) -> Option<String> {
        let head = run_git(
            &self.root,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
            LOCAL_TIMEOUT,
        )
        .ok()
        .filter(crate::exec::Raw::success)
        .map(|r| r.stdout_string().trim().to_owned())
        .filter(|r| !r.is_empty());
        if head.is_some() {
            return head;
        }
        ["origin/main", "origin/master", "main", "master"]
            .into_iter()
            .find(|r| {
                run_git(
                    &self.root,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{r}^{{commit}}"),
                    ],
                    LOCAL_TIMEOUT,
                )
                .is_ok_and(|o| o.success())
            })
            .map(str::to_owned)
    }

    /// Collects what [`draft_prompt`] needs: for a commit, the staged diff; for a PR, the diff of
    /// the branch against its base (`base...HEAD`), falling back to the staged diff when there is
    /// no base. The diff is capped and redacted. Local `git` only; spends nothing.
    pub fn draft_input(&self, kind: DraftKind) -> Result<DraftInput> {
        let range = match kind {
            DraftKind::Commit => None,
            DraftKind::PullRequest => self.base_ref().map(|b| format!("{b}...HEAD")),
        };
        let mut stat_args = vec!["diff", "--stat", "--no-color", "--no-ext-diff"];
        let mut diff_args = vec!["diff", "--no-color", "--no-ext-diff"];
        match &range {
            Some(r) => {
                stat_args.push(r);
                diff_args.push(r);
            }
            None => {
                stat_args.push("--cached");
                diff_args.push("--cached");
            }
        }
        let stat = crate::exec::run_git_ok(&self.root, &stat_args)?.stdout_string();
        let diff = crate::exec::run_git_ok(&self.root, &diff_args)?.stdout_string();
        let subjects = crate::exec::run_git(
            &self.root,
            &["log", "-n", &RECENT_SUBJECTS.to_string(), "--format=%s"],
            LOCAL_TIMEOUT,
        )
        .ok()
        .filter(crate::exec::Raw::success)
        .map(|r| {
            r.stdout_string()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
        Ok(DraftInput {
            diff_stat: store::redact(stat.trim_end()).text,
            diff: cap_and_redact(&diff),
            recent_subjects: subjects,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subj(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn language_follows_the_history() {
        let es = subj(&[
            "feat(config): añade config.toml con hot-reload",
            "fix(parser): maneja entrada vacía sin entrar en pánico",
            "docs: describe los checkpoints",
        ]);
        let en = subj(&[
            "feat: add UI layout tests for keyboard and pointer",
            "fix: handle empty input without a panic",
            "docs: describe the checkpoints",
        ]);
        assert_eq!(detect_language(&es), Lang::Es);
        assert_eq!(detect_language(&en), Lang::En);
        // No history, or nothing decisive: Spanish.
        assert_eq!(detect_language(&[]), Lang::Es);
        assert_eq!(detect_language(&subj(&["wip", "x"])), Lang::Es);
        assert_eq!(detect_language(&subj(&["feat(ui): wip"])), Lang::Es);
    }

    #[test]
    fn prompt_is_pure_and_follows_the_kind_and_language() {
        let es = subj(&["fix: corrige el error de la barra"]);
        let a = draft_prompt(DraftKind::Commit, " a.rs | 2 +-", "+x\n-y", &es);
        assert_eq!(
            a,
            draft_prompt(DraftKind::Commit, " a.rs | 2 +-", "+x\n-y", &es)
        );
        assert!(a.contains("mensaje de commit"));
        assert!(a.contains("en español"));
        assert!(a.contains("- fix: corrige el error de la barra"));
        assert!(a.contains("```diff\n+x\n-y\n```"));
        assert!(a.contains(" a.rs | 2 +-"));
        assert!(a.contains("No ejecutes comandos"));

        let en = subj(&["fix: handle the empty case"]);
        let b = draft_prompt(DraftKind::PullRequest, "", "+x", &en);
        assert!(b.contains("pull request"));
        assert!(b.contains("Reply ONLY"));
        assert!(b.contains("\"Summary\""));
        assert!(!b.contains("Diff summary"), "no stat, no section");

        let c = draft_prompt(DraftKind::PullRequest, "s", "+x", &es);
        assert!(c.contains("«Resumen»"));
    }

    #[test]
    fn drafted_text_splits_into_subject_and_body() {
        assert_eq!(
            split_message("feat: x\n\nPorque sí.\nSegunda línea.\n"),
            ("feat: x".into(), "Porque sí.\nSegunda línea.".into())
        );
        assert_eq!(
            split_message("solo asunto"),
            ("solo asunto".into(), String::new())
        );
        // Fences and quotes an agent may add.
        assert_eq!(
            split_message("```text\nfix: a\n\ncuerpo\n```"),
            ("fix: a".into(), "cuerpo".into())
        );
        assert_eq!(split_message("\"fix: a\"").0, "fix: a");
        assert_eq!(split_message("# Título\n\ncuerpo").0, "Título");
        assert_eq!(split_message("  \n "), (String::new(), String::new()));
    }

    #[test]
    fn diff_is_capped_at_a_line_and_redacted() {
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let mut diff = format!("+token = \"{secret}\"\n");
        while diff.len() < MAX_DIFF_BYTES * 2 {
            diff.push_str("+let a_reasonably_long_line = compute(1, 2, 3);\n");
        }
        let out = cap_and_redact(&diff);
        assert!(!out.contains(secret));
        assert!(out.contains("oculto"));
        assert!(out.len() < MAX_DIFF_BYTES + 200);
        assert!(out.contains("[... diff recortado a 60 KB ...]"));
        // Small diffs pass through untouched.
        assert_eq!(cap_and_redact("+a\n-b\n"), "+a\n-b\n");
    }
}
