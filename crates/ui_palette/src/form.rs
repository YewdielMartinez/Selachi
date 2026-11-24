//! The inline parameter form: snippets with `{params}` (defaults, `{a|x|y}` options) and the git
//! actions that need text (`commit` message, `switch` branch).
//!
//! [`FormState`] is pure (no gpui): the view feeds it keystrokes and it answers with the live
//! preview and, on submit, the event to emit. Values are rendered with
//! [`store::Quoting::PowerShell`] so a value can never break out of its argument.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use git_svc::GitAction;
use store::{Param, Quoting};

use crate::item::{Effect, PaletteEvent};

/// What a form produces when it is submitted.
#[derive(Clone, Debug)]
pub enum FormSubmit {
    /// Enter inserts the rendered line at the prompt, Ctrl+Enter runs it.
    Snippet {
        id: i64,
        template: String,
        cwd: Option<PathBuf>,
    },
    /// `git commit -m <mensaje>`.
    GitCommit,
    /// `git switch <rama>`.
    GitSwitch,
    /// `git checkout <destino>`.
    GitCheckout,
}

/// A form to show before an action can run.
#[derive(Clone, Debug)]
pub struct FormSpec {
    pub title: String,
    pub params: Vec<Param>,
    pub submit: FormSubmit,
}

impl FormSpec {
    pub fn snippet(
        id: i64,
        title: impl Into<String>,
        template: impl Into<String>,
        params: Vec<Param>,
        cwd: Option<PathBuf>,
    ) -> Self {
        Self {
            title: title.into(),
            params,
            submit: FormSubmit::Snippet {
                id,
                template: template.into(),
                cwd,
            },
        }
    }

    fn text_param(name: &str) -> Param {
        Param {
            name: name.to_string(),
            default: None,
            options: Vec::new(),
        }
    }

    pub fn git_commit() -> Self {
        Self {
            title: "git commit".into(),
            params: vec![Self::text_param("mensaje")],
            submit: FormSubmit::GitCommit,
        }
    }

    pub fn git_switch() -> Self {
        Self {
            title: "git switch".into(),
            params: vec![Self::text_param("rama")],
            submit: FormSubmit::GitSwitch,
        }
    }

    pub fn git_checkout() -> Self {
        Self {
            title: "git checkout".into(),
            params: vec![Self::text_param("destino")],
            submit: FormSubmit::GitCheckout,
        }
    }

    /// Footer verbs of the form: `(Enter, Ctrl+Enter)`.
    pub fn labels(&self) -> (&'static str, Option<&'static str>) {
        match self.submit {
            FormSubmit::Snippet { .. } => ("Insertar", Some("Ejecutar")),
            _ => ("Ejecutar", None),
        }
    }

    /// The command line or git argv text for `values`, or the reason it cannot be built yet.
    pub fn render(&self, values: &HashMap<String, String>) -> Result<String, String> {
        for p in &self.params {
            let empty = values.get(&p.name).is_none_or(|v| v.trim().is_empty());
            if empty && p.default.is_none() {
                return Err(format!("Falta «{}»", p.name));
            }
        }
        match &self.submit {
            FormSubmit::Snippet { template, .. } => {
                store::render(template, values, Quoting::PowerShell).map_err(|e| e.to_string())
            }
            _ => self.git_action(values).map(|a| display_argv(&a.argv())),
        }
    }

    fn git_action(&self, values: &HashMap<String, String>) -> Result<GitAction, String> {
        let get = |name: &str| {
            values
                .get(name)
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        let action = match self.submit {
            FormSubmit::GitCommit => GitAction::Commit {
                message: get("mensaje"),
            },
            FormSubmit::GitSwitch => GitAction::Switch {
                branch: get("rama"),
            },
            FormSubmit::GitCheckout => GitAction::Checkout {
                target: get("destino"),
            },
            FormSubmit::Snippet { .. } => return Err("no es una acción git".into()),
        };
        action.validate().map_err(|e| e.to_string())?;
        Ok(action)
    }

    /// The event to emit for `values` (`alt` = Ctrl+Enter), plus the bookkeeping effect.
    pub fn finish(
        &self,
        values: &HashMap<String, String>,
        alt: bool,
    ) -> Result<(PaletteEvent, Option<Effect>), String> {
        match &self.submit {
            FormSubmit::Snippet { id, cwd, .. } => {
                let line = self.render(values)?;
                let event = if alt {
                    PaletteEvent::RunCommand {
                        line,
                        cwd: cwd.clone(),
                        new_tab: false,
                    }
                } else {
                    PaletteEvent::InsertText(line)
                };
                Ok((event, Some(Effect::SnippetUsed(*id))))
            }
            _ => Ok((PaletteEvent::GitAction(self.git_action(values)?), None)),
        }
    }
}

/// `git commit -m "fix: a b"` for display (quotes only what needs them).
fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The form being filled.
#[derive(Clone, Debug)]
pub struct FormState {
    pub spec: Arc<FormSpec>,
    pub values: Vec<String>,
    pub active: usize,
    /// Why the last submit failed.
    pub error: Option<String>,
}

impl FormState {
    pub fn new(spec: Arc<FormSpec>) -> Self {
        let values = spec
            .params
            .iter()
            .map(|p| p.default.clone().unwrap_or_default())
            .collect();
        Self {
            spec,
            values,
            active: 0,
            error: None,
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn active_param(&self) -> Option<&Param> {
        self.spec.params.get(self.active)
    }

    /// The active field is a `{a|x|y}` choice (no free text).
    pub fn active_is_choice(&self) -> bool {
        self.active_param().is_some_and(|p| !p.options.is_empty())
    }

    pub fn active_value(&self) -> &str {
        self.values.get(self.active).map_or("", String::as_str)
    }

    pub fn set_active_value(&mut self, text: &str) {
        if let Some(v) = self.values.get_mut(self.active)
            && v != text
        {
            *v = text.to_string();
            self.error = None;
        }
    }

    /// Moves to the next (`+1`) or previous (`-1`) field, wrapping.
    pub fn move_active(&mut self, delta: isize) {
        let n = self.len() as isize;
        if n > 0 {
            self.active = (self.active as isize + delta).rem_euclid(n) as usize;
        }
    }

    pub fn is_last(&self) -> bool {
        self.active + 1 >= self.len()
    }

    /// Cycles the active choice field.
    pub fn cycle_choice(&mut self, delta: isize) {
        let Some(p) = self.spec.params.get(self.active) else {
            return;
        };
        if p.options.is_empty() {
            return;
        }
        let cur = p
            .options
            .iter()
            .position(|o| *o == self.values[self.active])
            .unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(p.options.len() as isize) as usize;
        self.values[self.active] = p.options[next].clone();
        self.error = None;
    }

    pub fn values_map(&self) -> HashMap<String, String> {
        self.spec
            .params
            .iter()
            .zip(&self.values)
            .map(|(p, v)| (p.name.clone(), v.clone()))
            .collect()
    }

    /// Live preview of the final line, or why it is not ready.
    pub fn preview(&self) -> Result<String, String> {
        self.spec.render(&self.values_map())
    }

    /// Validates and returns what to emit; on failure remembers the message in `error`.
    pub fn submit(&mut self, alt: bool) -> Option<(PaletteEvent, Option<Effect>)> {
        match self.spec.finish(&self.values_map(), alt) {
            Ok(done) => Some(done),
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::parse_template;

    fn snippet(template: &str) -> FormState {
        let spec = FormSpec::snippet(
            7,
            "demo",
            template,
            parse_template(template),
            Some(PathBuf::from("C:/p")),
        );
        FormState::new(Arc::new(spec))
    }

    #[test]
    fn defaults_and_options_prefill() {
        let f = snippet("dotnet build -c {cfg|Debug|Release} -v {v:minimal} {proyecto}");
        assert_eq!(f.values, ["Debug", "minimal", ""]);
        assert!(f.active_is_choice());
    }

    #[test]
    fn snippet_param_flow_inserts_then_runs_with_powershell_quoting() {
        let mut f = snippet("dotnet ef migrations add {nombre} -c {cfg|Debug|Release}");
        // Required value missing: no event, an error to show.
        assert_eq!(f.preview(), Err("Falta «nombre»".to_string()));
        assert!(f.submit(false).is_none());
        assert_eq!(f.error.as_deref(), Some("Falta «nombre»"));

        // Type a value with a space and a quote: it must stay one literal argument.
        f.set_active_value("Add user's table");
        assert!(f.error.is_none());
        assert_eq!(
            f.preview().unwrap(),
            "dotnet ef migrations add 'Add user''s table' -c Debug"
        );

        // Move to the choice field and cycle it.
        f.move_active(1);
        assert!(f.active_is_choice() && f.is_last());
        f.cycle_choice(1);
        assert_eq!(f.active_value(), "Release");
        f.cycle_choice(1);
        assert_eq!(f.active_value(), "Debug");
        f.cycle_choice(-1);
        assert_eq!(f.active_value(), "Release");

        // Enter inserts, Ctrl+Enter runs; both record the use.
        let (ev, effect) = f.submit(false).unwrap();
        assert_eq!(
            ev,
            PaletteEvent::InsertText(
                "dotnet ef migrations add 'Add user''s table' -c Release".into()
            )
        );
        assert_eq!(effect, Some(Effect::SnippetUsed(7)));
        let (ev, effect) = f.submit(true).unwrap();
        assert_eq!(
            ev,
            PaletteEvent::RunCommand {
                line: "dotnet ef migrations add 'Add user''s table' -c Release".into(),
                cwd: Some(PathBuf::from("C:/p")),
                new_tab: false
            }
        );
        assert_eq!(effect, Some(Effect::SnippetUsed(7)));
    }

    #[test]
    fn dangerous_values_never_break_out() {
        let mut f = snippet("echo {x}");
        f.set_active_value("$(calc); rm -rf /");
        assert_eq!(f.preview().unwrap(), "echo '$(calc); rm -rf /'");
        // A line break is rejected instead of being typed into the terminal.
        f.set_active_value("a\nb");
        assert!(f.submit(false).is_none());
        assert!(f.error.is_some());
    }

    #[test]
    fn field_navigation_wraps() {
        let mut f = snippet("{a} {b} {c}");
        assert_eq!(f.active, 0);
        f.move_active(-1);
        assert_eq!(f.active, 2);
        f.move_active(1);
        assert_eq!(f.active, 0);
        assert!(!f.is_last());
    }

    #[test]
    fn git_forms_build_validated_actions() {
        let mut f = FormState::new(Arc::new(FormSpec::git_commit()));
        assert!(f.submit(false).is_none());
        f.set_active_value("fix: arreglar el parser");
        assert_eq!(
            f.preview().unwrap(),
            "git commit -m \"fix: arreglar el parser\""
        );
        let (ev, effect) = f.submit(false).unwrap();
        assert_eq!(
            ev,
            PaletteEvent::GitAction(GitAction::Commit {
                message: "fix: arreglar el parser".into()
            })
        );
        assert_eq!(effect, None);

        let mut s = FormState::new(Arc::new(FormSpec::git_switch()));
        s.set_active_value("--detach");
        assert!(s.submit(false).is_none(), "options are rejected by git_svc");
        s.set_active_value("feature/x");
        assert_eq!(
            s.submit(false).unwrap().0,
            PaletteEvent::GitAction(GitAction::Switch {
                branch: "feature/x".into()
            })
        );
    }
}
