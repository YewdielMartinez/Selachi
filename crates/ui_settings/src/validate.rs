//! Validation glue: `Config::validate()` problems -> inline, per-field Spanish messages, plus the
//! small parsers the text fields need.

use store::Config;

/// A problem tied to one field of the UI (`field` is the config key, e.g. `appearance.mono_size`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

/// Map the raw `Config::validate()` strings onto fields with friendly messages.
pub fn field_errors(config: &Config) -> Vec<FieldError> {
    config.validate().iter().map(|p| friendly(p)).collect()
}

/// The first problem of `config`, if it is invalid.
pub fn first_error(config: &Config) -> Option<FieldError> {
    field_errors(config).into_iter().next()
}

fn friendly(problem: &str) -> FieldError {
    let key = problem.split([' ', '=']).next().unwrap_or("").to_string();
    let known = |k: &str| key == k;
    let message = if known("appearance.accent") {
        "Usa un color hexadecimal, por ejemplo #00AAFF.".to_string()
    } else if known("appearance.mono_size") {
        "El tamaño debe estar entre 4 y 96.".to_string()
    } else if known("appearance.line_height") {
        "El interlineado debe estar entre 0,8 y 3,0.".to_string()
    } else if key.starts_with("appearance.translucency") {
        "La translucidez debe estar entre 0 y 100 %.".to_string()
    } else if known("terminal.scrollback") {
        "Máximo 1 000 000 de líneas.".to_string()
    } else if key.starts_with("terminal.completion.accept_word_key") {
        "Tecla no válida. Ejemplo: Ctrl+RightArrow.".to_string()
    } else if known("plugins.index_url") {
        "La URL del índice debe empezar por https://".to_string()
    } else if known("git.external_editor") {
        "El comando no puede tener saltos de línea.".to_string()
    } else if key.starts_with("profiles[") {
        "El perfil necesita un nombre y un programa.".to_string()
    } else {
        problem.to_string()
    };
    let field = if key.starts_with("profiles[") {
        // `profiles[3]` -> `profiles.3`
        key.replace('[', ".").replace(']', "")
    } else {
        key
    };
    FieldError { field, message }
}

/// Parse a number typed by the user (`13`, `13.5`, `13,5`).
pub fn parse_number(text: &str) -> Result<f64, String> {
    let t = text.trim().replace(',', ".");
    match t.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err("Escribe un número.".to_string()),
    }
}

/// Parse a non-negative integer typed by the user (`10000`, `10 000`).
pub fn parse_count(text: &str) -> Result<u32, String> {
    let t: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect();
    t.parse::<u32>()
        .map_err(|_| "Escribe un número entero.".to_string())
}

/// Split a command line into arguments: whitespace separated, `"double quotes"` group.
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for ch in text.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Inverse of [`split_args`]: arguments with spaces (or empty ones) are double-quoted.
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_values_map_to_their_fields() {
        let mut c = Config::default();
        c.appearance.mono_size = 2.0;
        let e = first_error(&c).unwrap();
        assert_eq!(e.field, "appearance.mono_size");
        assert!(e.message.contains("4 y 96"));

        let mut c = Config::default();
        c.appearance.accent = "azul".into();
        assert_eq!(first_error(&c).unwrap().field, "appearance.accent");

        let mut c = Config::default();
        c.terminal.scrollback = 5_000_000;
        assert_eq!(first_error(&c).unwrap().field, "terminal.scrollback");

        let mut c = Config::default();
        c.terminal.completion.accept_word_key = "a'b".into();
        assert_eq!(
            first_error(&c).unwrap().field,
            "terminal.completion.accept_word_key"
        );

        let mut c = Config::default();
        c.profiles.push(store::config::Profile::default());
        let e = first_error(&c).unwrap();
        assert_eq!(e.field, "profiles.0");
        assert!(e.message.contains("nombre"));

        assert!(first_error(&Config::default()).is_none());
    }

    #[test]
    fn numbers_accept_comma_and_reject_text() {
        assert_eq!(parse_number("13,5"), Ok(13.5));
        assert_eq!(parse_number(" 14 "), Ok(14.0));
        assert!(parse_number("trece").is_err());
        assert!(parse_number("").is_err());
        assert!(parse_number("inf").is_err());
        assert_eq!(parse_count("10 000"), Ok(10_000));
        assert!(parse_count("-1").is_err());
        assert!(parse_count("1.5").is_err());
    }

    #[test]
    fn args_roundtrip() {
        assert_eq!(
            split_args("-NoLogo -Command \"Write-Host hi\""),
            vec!["-NoLogo", "-Command", "Write-Host hi"]
        );
        assert_eq!(split_args("  a   b "), vec!["a", "b"]);
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args("\"\""), vec![""]);
        let args = vec![
            "-File".to_string(),
            "C:\\a b\\x.ps1".to_string(),
            String::new(),
        ];
        assert_eq!(split_args(&join_args(&args)), args);
    }
}
