//! What each text field does with the text the user typed.

use gpui::Context;

use crate::validate::{
    APPROVAL_HELP, TIMEOUT_HELP, approval_secs, parse_count, parse_number, split_args,
    timeout_secs_from_minutes,
};
use crate::view::SettingsView;

/// Fields that apply their text on Enter or when they lose focus, never per keystroke: a half-typed
/// number, colour or URL is not a value yet (and every URL change refetches the plugin index).
pub(crate) fn commits_on_leave(key: &str) -> bool {
    matches!(
        key,
        "accent_hex"
            | "scrollback"
            | "deleg_concurrent"
            | "deleg_timeout"
            | "deleg_budget"
            | "deleg_weekly"
            | "deleg_approval"
            | "index_url"
    )
}

/// `#RRGGBB` exactly (the `#` and six hex digits), as the canonical upper case.
pub(crate) fn full_hex(text: &str) -> Option<String> {
    let digits = text.strip_prefix('#')?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{}", digits.to_ascii_uppercase()))
}

/// Apply the text of field `key`. Invalid text leaves the config untouched and sets an inline
/// error on the field; valid text is applied (and saved) immediately.
pub(crate) fn apply(
    view: &mut SettingsView,
    key: &str,
    text: &str,
    cx: &mut Context<SettingsView>,
) {
    let trimmed = text.trim().to_string();
    match key {
        "ui_font" | "mono_font" => {
            if trimmed.is_empty() {
                set_error(view, key, "Escribe el nombre de una fuente.", cx);
                return;
            }
            if key == "ui_font" {
                view.edit_field(key, cx, |c| c.appearance.ui_font = trimmed);
            } else {
                view.edit_field(key, cx, |c| c.appearance.mono_font = trimmed);
            }
        }
        "accent_hex" => match full_hex(&trimmed) {
            Some(hex) => view.edit_field(key, cx, |c| c.appearance.accent = hex),
            None => set_error(
                view,
                key,
                "Escribe un color completo, por ejemplo #00AAFF.",
                cx,
            ),
        },
        "scrollback" => match parse_count(&trimmed) {
            Ok(n) => view.edit_field(key, cx, |c| c.terminal.scrollback = n),
            Err(m) => set_error(view, key, &m, cx),
        },
        "accept_word_key" => {
            if trimmed.is_empty() {
                set_error(
                    view,
                    key,
                    "Escribe una tecla (o «native» para dejar la del shell).",
                    cx,
                );
                return;
            }
            view.edit_field(key, cx, |c| c.terminal.completion.accept_word_key = trimmed);
        }
        "deleg_concurrent" => match parse_count(&trimmed) {
            Ok(n) => view.edit_field(key, cx, |c| c.delegation.max_concurrent = n),
            Err(m) => set_error(view, key, &m, cx),
        },
        "deleg_approval" => match parse_count(&trimmed) {
            Ok(n) => match approval_secs(n) {
                Some(secs) => {
                    view.edit_field(key, cx, |c| c.delegation.approval_timeout_secs = secs)
                }
                None => set_error(view, key, APPROVAL_HELP, cx),
            },
            Err(m) => set_error(view, key, &m, cx),
        },
        "deleg_budget" => match parse_count(&trimmed) {
            Ok(n) => view.edit_field(key, cx, |c| c.delegation.daily_token_budget = u64::from(n)),
            Err(m) => set_error(view, key, &m, cx),
        },
        "deleg_timeout" => match parse_number(&trimmed) {
            Ok(min) => match timeout_secs_from_minutes(min) {
                Some(secs) => view.edit_field(key, cx, |c| c.delegation.timeout_secs = secs),
                None => set_error(view, key, TIMEOUT_HELP, cx),
            },
            Err(m) => set_error(view, key, &m, cx),
        },
        "deleg_weekly" => match parse_number(&trimmed) {
            Ok(p) => view.edit_field(key, cx, |c| c.delegation.weekly_usage_threshold_percent = p),
            Err(m) => set_error(view, key, &m, cx),
        },
        // typing in the add field only clears its error; Enter adds (see `add_extra_agent`)
        "extra_agent_add" => {
            view.errors.remove(key);
            view.sync_invalid(cx);
        }
        "external_editor" => view.edit_field(key, cx, |c| c.git.external_editor = trimmed),
        "index_url" => view.edit_field(key, cx, |c| c.plugins.index_url = trimmed),
        k if k.starts_with("profile.") => profile_field(view, k, text, cx),
        _ => {}
    }
}

fn set_error(view: &mut SettingsView, key: &str, message: &str, cx: &mut Context<SettingsView>) {
    view.errors.insert(key.to_string(), message.to_string());
    view.sync_invalid(cx);
    cx.notify();
}

/// `profile.<i>.<name|program|args|cwd>` and `profile.<i>.env.<j>.<k|v>`.
fn profile_field(view: &mut SettingsView, key: &str, text: &str, cx: &mut Context<SettingsView>) {
    let parts: Vec<&str> = key.split('.').collect();
    let Some(i) = parts.get(1).and_then(|s| s.parse::<usize>().ok()) else {
        return;
    };
    if i >= view.cfg.profiles.len() {
        return;
    }
    match parts.get(2).copied() {
        Some("name") => {
            let name = text.trim().to_string();
            if name.is_empty() {
                set_error(view, key, "Escribe un nombre.", cx);
                return;
            }
            let dup = view
                .cfg
                .profiles
                .iter()
                .enumerate()
                .any(|(j, p)| j != i && p.name.trim().eq_ignore_ascii_case(&name));
            if dup {
                set_error(view, key, "Ya existe un perfil con ese nombre.", cx);
                return;
            }
            let old = view.cfg.profiles[i].name.clone();
            view.edit_field(key, cx, move |c| {
                c.profiles[i].name = name.clone();
                if c.terminal.default_profile == old {
                    c.terminal.default_profile = name;
                }
            });
        }
        Some("program") => {
            let program = text.trim().to_string();
            if program.is_empty() {
                set_error(view, key, "Escribe el programa (por ejemplo pwsh.exe).", cx);
                return;
            }
            view.edit_field(key, cx, move |c| c.profiles[i].program = program);
        }
        Some("args") => {
            let args = split_args(text);
            view.edit_field(key, cx, move |c| c.profiles[i].args = args);
        }
        Some("cwd") => {
            let cwd = text.trim().to_string();
            view.edit_field(key, cx, move |c| {
                c.profiles[i].cwd = (!cwd.is_empty()).then_some(cwd)
            });
        }
        Some("env") => {
            let Some(j) = parts.get(3).and_then(|s| s.parse::<usize>().ok()) else {
                return;
            };
            let which = parts.get(4).copied().unwrap_or("");
            if let Some(editor) = view.profile_editor.as_mut().filter(|e| e.index == i)
                && let Some(row) = editor.env.get_mut(j)
            {
                match which {
                    "k" => row.0 = text.trim().to_string(),
                    "v" => row.1 = text.to_string(),
                    _ => {}
                }
            }
            commit_env(view, i, key, cx);
        }
        _ => {}
    }
}

/// Write the env draft of profile `i` into the config when it has no empty or repeated names.
pub(crate) fn commit_env(
    view: &mut SettingsView,
    i: usize,
    key: &str,
    cx: &mut Context<SettingsView>,
) {
    let Some(editor) = view.profile_editor.as_ref().filter(|e| e.index == i) else {
        return;
    };
    let rows = editor.env.clone();
    let mut map = std::collections::BTreeMap::new();
    for (k, v) in &rows {
        if k.is_empty() {
            set_error(view, key, "Una variable no tiene nombre.", cx);
            return;
        }
        if map.insert(k.clone(), v.clone()).is_some() {
            set_error(view, key, "Hay dos variables con el mismo nombre.", cx);
            return;
        }
    }
    view.edit_field(key, cx, move |c| c.profiles[i].env = map);
}
