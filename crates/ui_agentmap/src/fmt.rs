//! Spanish number and time formats of the map (decimal comma, short units).

/// `45 s`, `2 min`, `1 h 05`. Negative values read as zero.
pub fn duration_ms(ms: i64) -> String {
    let secs = (ms.max(0) / 1000) as u64;
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        let (m, s) = (secs / 60, secs % 60);
        if m < 10 && s != 0 {
            format!("{m} min {s:02}")
        } else {
            format!("{m} min")
        }
    } else {
        format!("{} h {:02}", secs / 3600, secs % 3600 / 60)
    }
}

/// `812`, `45 k`, `1,2 M`.
pub fn tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        let k = n as f64 / 1_000.0;
        if k < 10.0 {
            format!("{k:.1} k").replace('.', ",")
        } else {
            format!("{:.0} k", k)
        }
    } else {
        format!("{:.1} M", n as f64 / 1_000_000.0).replace('.', ",")
    }
}

/// `$0,40`; below one cent `<$0,01`; zero is `$0`.
pub fn cost(usd: f64) -> String {
    if usd <= 0.0 {
        "$0".to_string()
    } else if usd < 0.01 {
        "<$0,01".to_string()
    } else {
        format!("${usd:.2}").replace('.', ",")
    }
}

/// `1 archivo` / `3 archivos`.
pub fn files(n: usize) -> String {
    if n == 1 {
        "1 archivo".to_string()
    } else {
        format!("{n} archivos")
    }
}

/// `1 comando aprobado solo` / `12 comandos aprobados solos`.
pub fn auto_approved(n: u32) -> String {
    if n == 1 {
        "1 comando aprobado solo".to_string()
    } else {
        format!("{n} comandos aprobados solos")
    }
}

/// Last path component for pills (`C:\repo.worktrees\fix-tests` -> `fix-tests`).
pub fn leaf(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Short reasoning-effort label (`high` -> `alto`).
pub fn effort(e: &str) -> String {
    match e.trim().to_ascii_lowercase().as_str() {
        "low" => "bajo".into(),
        "medium" => "medio".into(),
        "high" => "alto".into(),
        "xhigh" => "muy alto".into(),
        "max" => "máximo".into(),
        other => other.to_string(),
    }
}

/// `model · effort` as the tree shows it (either part may be missing).
pub fn model_effort(model: Option<&str>, eff: Option<&str>) -> Option<String> {
    match (
        model.filter(|m| !m.is_empty()),
        eff.filter(|e| !e.is_empty()),
    ) {
        (Some(m), Some(e)) => Some(format!("{m} · {}", effort(e))),
        (Some(m), None) => Some(m.to_string()),
        (None, Some(e)) => Some(effort(e)),
        (None, None) => None,
    }
}

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

/// `340 MB`, `1,2 GB`, `12 KB`. Binary units, matching `ui_procs`.
pub fn bytes(bytes: u64) -> String {
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

/// CPU percentage (`0%`, `3,4%`, `12%`).
pub fn cpu(pct: f32) -> String {
    if !pct.is_finite() || pct < 0.05 {
        "0%".to_string()
    } else if pct < 10.0 {
        format!("{}%", format!("{pct:.1}").replace('.', ","))
    } else {
        format!("{}%", pct.round() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_ms(-5), "0 s");
        assert_eq!(duration_ms(45_000), "45 s");
        assert_eq!(duration_ms(125_000), "2 min 05");
        assert_eq!(duration_ms(900_000), "15 min");
        assert_eq!(duration_ms(3_900_000), "1 h 05");
    }

    #[test]
    fn tokens_and_cost() {
        assert_eq!(tokens(812), "812");
        assert_eq!(tokens(4_500), "4,5 k");
        assert_eq!(tokens(45_000), "45 k");
        assert_eq!(tokens(1_200_000), "1,2 M");
        assert_eq!(cost(0.0), "$0");
        assert_eq!(cost(0.004), "<$0,01");
        assert_eq!(cost(0.4), "$0,40");
        assert_eq!(
            model_effort(Some("gpt-5"), Some("high")).unwrap(),
            "gpt-5 · alto"
        );
        assert_eq!(model_effort(None, None), None);
        assert_eq!(bytes(12 * KIB), "12 KB");
        assert_eq!(bytes(340 * MIB), "340 MB");
        assert_eq!(bytes(GIB + GIB / 5), "1,2 GB");
        assert_eq!(cpu(0.0), "0%");
        assert_eq!(cpu(3.44), "3,4%");
        assert_eq!(cpu(12.6), "13%");
    }
}
