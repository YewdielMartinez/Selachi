//! Small formatting helpers (Spanish: decimal comma).

/// `5_300_000` -> `5,1 MB`, `409_775` -> `400 KB`, `900` -> `900 B`.
pub fn bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    let v = n as f64;
    if v < KB {
        format!("{n} B")
    } else if v < KB * KB {
        format!("{} KB", trim_decimal(v / KB, 0))
    } else if v < KB * KB * KB {
        format!("{} MB", trim_decimal(v / (KB * KB), 1))
    } else {
        format!("{} GB", trim_decimal(v / (KB * KB * KB), 1))
    }
}

fn trim_decimal(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    s.replace('.', ",")
}

/// Compare dotted versions numerically (`0.23.5` < `0.24.0`). Non-numeric parts compare as text.
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| -> Vec<(u64, String)> {
        s.trim()
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|p| match p.parse::<u64>() {
                Ok(n) => (n, String::new()),
                Err(_) => (0, p.to_string()),
            })
            .collect()
    };
    key(a).cmp(&key(b))
}

/// Is `candidate` strictly newer than `installed`?
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    version_cmp(candidate, installed) == std::cmp::Ordering::Greater
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_a_decimal_comma() {
        assert_eq!(bytes(900), "900 B");
        assert_eq!(bytes(409_775), "400 KB");
        assert_eq!(bytes(5_500_000), "5,2 MB");
        assert_eq!(bytes(0), "0 B");
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.24.0", "0.23.5"));
        assert!(is_newer("0.23.10", "0.23.9"));
        assert!(!is_newer("0.23.5", "0.23.5"));
        assert!(!is_newer("0.23.4", "0.23.5"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(is_newer("1.0.0.1", "1.0.0"));
    }
}
