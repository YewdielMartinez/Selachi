//! Number and command formatting for rows (Spanish decimal comma, compact units).

use std::time::Duration;

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

/// `340 MB`, `1,2 GB`, `12 KB`. Binary units, like the status bar's `RAM 76 MB`.
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= GIB {
        let gb = bytes as f64 / GIB as f64;
        if gb >= 10.0 {
            format!("{} GB", gb.round() as u64)
        } else {
            format!("{} GB", format!("{gb:.1}").replace('.', ","))
        }
    } else if bytes >= MIB {
        format!("{} MB", bytes / MIB)
    } else {
        format!("{} KB", bytes / KIB)
    }
}

/// CPU as a percentage of the whole machine: `0%`, `3,4%`, `12%`.
pub fn format_cpu(pct: f32) -> String {
    if !pct.is_finite() || pct < 0.05 {
        "0%".to_string()
    } else if pct < 10.0 {
        format!("{}%", format!("{pct:.1}").replace('.', ","))
    } else {
        format!("{}%", pct.round() as u32)
    }
}

/// Compact uptime: `45s`, `12m`, `2h 05m`, `3d 4h`.
pub fn format_uptime(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3_599 => format!("{}m", s / 60),
        3_600..=86_399 => format!("{}h {:02}m", s / 3_600, (s % 3_600) / 60),
        _ => format!("{}d {}h", s / 86_400, (s % 86_400) / 3_600),
    }
}

/// `1 huérfano` / `3 huérfanos`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Splits a Windows command line on whitespace, honouring double quotes.
fn split_command_line(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for ch in cmd.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

fn last_component(token: &str) -> &str {
    token.rsplit(['\\', '/']).find(|s| !s.is_empty()).unwrap_or(token)
}

fn strip_exe(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".bat") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

/// A short, readable command: the executable without directory or `.exe`, then the arguments
/// with their directories dropped.
///
/// ```text
/// "C:\Program Files\nodejs\node.exe" C:\app\node_modules\vite\bin\vite.js --port 5173
///   -> node vite.js --port 5173
/// ```
pub fn shorten_command(name: &str, cmdline: Option<&str>, max_chars: usize) -> String {
    let Some(cmd) = cmdline.map(str::trim).filter(|c| !c.is_empty()) else {
        return strip_exe(name).to_string();
    };
    let toks = split_command_line(cmd);
    let Some((exe, args)) = toks.split_first() else {
        return strip_exe(name).to_string();
    };
    let mut out = strip_exe(last_component(exe)).to_string();
    for a in args {
        out.push(' ');
        if a.contains(['\\', '/']) && !a.starts_with('-') && !a.starts_with("http") {
            out.push_str(last_component(a));
        } else {
            out.push_str(a);
        }
    }
    if out.chars().count() > max_chars {
        let cut: String = out.chars().take(max_chars.saturating_sub(1)).collect();
        out = format!("{}…", cut.trim_end());
    }
    out
}

/// Capitalises the first letter (`claude` -> `Claude`).
pub fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_spanish_decimals() {
        assert_eq!(format_bytes(12 * KIB), "12 KB");
        assert_eq!(format_bytes(340 * MIB), "340 MB");
        assert_eq!(format_bytes(GIB + GIB / 5), "1,2 GB");
        assert_eq!(format_bytes(12 * GIB + 1), "12 GB");
    }

    #[test]
    fn cpu_and_uptime() {
        assert_eq!(format_cpu(0.0), "0%");
        assert_eq!(format_cpu(3.44), "3,4%");
        assert_eq!(format_cpu(12.6), "13%");
        assert_eq!(format_cpu(f32::NAN), "0%");
        assert_eq!(format_uptime(Duration::from_secs(45)), "45s");
        assert_eq!(format_uptime(Duration::from_secs(12 * 60 + 3)), "12m");
        assert_eq!(format_uptime(Duration::from_secs(2 * 3_600 + 5 * 60)), "2h 05m");
        assert_eq!(format_uptime(Duration::from_secs(3 * 86_400 + 4 * 3_600)), "3d 4h");
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "huérfano", "huérfanos"), "1 huérfano");
        assert_eq!(plural(3, "huérfano", "huérfanos"), "3 huérfanos");
    }

    #[test]
    fn commands_are_shortened() {
        assert_eq!(
            shorten_command(
                "node.exe",
                Some(r#""C:\Program Files\nodejs\node.exe" C:\app\node_modules\vite\bin\vite.js --port 5173"#),
                60
            ),
            "node vite.js --port 5173"
        );
        assert_eq!(shorten_command("dotnet.exe", None, 60), "dotnet");
        assert_eq!(shorten_command("x.exe", Some("   "), 60), "x");
        let long = shorten_command("a.exe", Some(&format!("a.exe {}", "z".repeat(100))), 20);
        assert_eq!(long.chars().count(), 20);
        assert!(long.ends_with('…'));
        assert_eq!(title_case("claude"), "Claude");
    }
}
