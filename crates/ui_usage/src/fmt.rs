//! Spanish formatting of tokens, costs, durations and dates. Pure functions (the clock and the
//! timezone are arguments) so they are easy to test.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Weekday};

/// `12_400_000` -> `12.4M`, `842_000` -> `842K`, `999` -> `999` (same shape as the status bar).
pub fn compact_tokens(n: u64) -> String {
    let f = n as f64;
    let (v, suffix) = match n {
        0..=999 => return n.to_string(),
        1_000..=999_999 => (f / 1e3, "K"),
        1_000_000..=999_999_999 => (f / 1e6, "M"),
        _ => (f / 1e9, "B"),
    };
    // One decimal below 100, none above (`842K`, `12.4M`).
    if v >= 100.0 { format!("{v:.0}{suffix}") } else { format!("{v:.1}{suffix}").replace(".0", "") }
}

/// `1234567` -> `1.234.567` (Spanish thousands separator), for tooltips.
pub fn full_tokens(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

/// `4.1` -> `$4.10`, `0.004` -> `<$0.01`, `142.3` -> `$142`.
pub fn format_cost(usd: f64) -> String {
    if !usd.is_finite() || usd < 0.0 {
        return "—".to_string();
    }
    if usd == 0.0 {
        "$0.00".to_string()
    } else if usd < 0.01 {
        "<$0.01".to_string()
    } else if usd >= 100.0 {
        format!("${usd:.0}")
    } else {
        format!("${usd:.2}")
    }
}

/// `2 h 13 min`, `45 min`, `3 d 4 h`, `<1 min`. `ms` is a duration (negative counts as zero).
pub fn format_duration(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    match mins {
        0 => "<1 min".to_string(),
        1..=59 => format!("{mins} min"),
        60..=1_439 => match mins % 60 {
            0 => format!("{} h", mins / 60),
            m => format!("{} h {m} min", mins / 60),
        },
        _ => match (mins % 1_440) / 60 {
            0 => format!("{} d", mins / 1_440),
            h => format!("{} d {h} h", mins / 1_440),
        },
    }
}

/// `hace 2 min`, `hace 3 h`, `hace 2 d`, `hace un momento`.
pub fn format_age(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    match mins {
        0 => "hace un momento".to_string(),
        1..=59 => format!("hace {mins} min"),
        60..=1_439 => format!("hace {} h", mins / 60),
        _ => format!("hace {} d", mins / 1_440),
    }
}

pub fn weekday_es(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "lunes",
        Weekday::Tue => "martes",
        Weekday::Wed => "miércoles",
        Weekday::Thu => "jueves",
        Weekday::Fri => "viernes",
        Weekday::Sat => "sábado",
        Weekday::Sun => "domingo",
    }
}

pub fn weekday_short_es(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "lun",
        Weekday::Tue => "mar",
        Weekday::Wed => "mié",
        Weekday::Thu => "jue",
        Weekday::Fri => "vie",
        Weekday::Sat => "sáb",
        Weekday::Sun => "dom",
    }
}

pub fn month_short_es(month: u32) -> &'static str {
    const M: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];
    M.get(month.saturating_sub(1) as usize).copied().unwrap_or("")
}

/// `mié 19 nov` for chart tooltips.
pub fn day_label(date: NaiveDate) -> String {
    format!("{} {} {}", weekday_short_es(date.weekday()), date.day(), month_short_es(date.month()))
}

/// When a forecast lands, relative to `now` in its own timezone: `hoy 16:30`, `mañana 09:00`,
/// `el jueves 14:00` (within a week), `el 3 dic 14:00` otherwise.
pub fn format_when<Tz: TimeZone>(now: &DateTime<Tz>, at_ms: i64) -> String {
    let Some(at) = now.timezone().timestamp_millis_opt(at_ms).single() else {
        return "—".to_string();
    };
    let days = (at.date_naive() - now.date_naive()).num_days();
    let time = format!("{:02}:{:02}", at.hour(), at.minute());
    match days {
        ..=0 => format!("hoy {time}"),
        1 => format!("mañana {time}"),
        2..=6 => format!("el {} {time}", weekday_es(at.weekday())),
        _ => format!("el {} {} {time}", at.day(), month_short_es(at.month())),
    }
}

/// The calendar date `back` days before the date of `now`.
pub fn days_back<Tz: TimeZone>(now: &DateTime<Tz>, back: i64) -> NaiveDate {
    now.date_naive() - Duration::days(back)
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    #[test]
    fn tokens_compact_and_full() {
        assert_eq!(compact_tokens(999), "999");
        assert_eq!(compact_tokens(842_000), "842K");
        assert_eq!(compact_tokens(12_400_000), "12.4M");
        assert_eq!(compact_tokens(2_000_000), "2M");
        assert_eq!(full_tokens(0), "0");
        assert_eq!(full_tokens(999), "999");
        assert_eq!(full_tokens(1_234_567), "1.234.567");
        assert_eq!(full_tokens(12_345), "12.345");
    }

    #[test]
    fn costs() {
        assert_eq!(format_cost(4.1), "$4.10");
        assert_eq!(format_cost(0.004), "<$0.01");
        assert_eq!(format_cost(142.3), "$142");
        assert_eq!(format_cost(f64::NAN), "—");
    }

    #[test]
    fn durations_and_ages() {
        assert_eq!(format_duration(30_000), "<1 min");
        assert_eq!(format_duration(45 * 60_000), "45 min");
        assert_eq!(format_duration((2 * 60 + 13) * 60_000), "2 h 13 min");
        assert_eq!(format_duration(3 * 3_600_000), "3 h");
        assert_eq!(format_duration((3 * 24 + 4) * 3_600_000), "3 d 4 h");
        assert_eq!(format_duration(-5), "<1 min");
        assert_eq!(format_age(10_000), "hace un momento");
        assert_eq!(format_age(2 * 60_000), "hace 2 min");
        assert_eq!(format_age(3 * 3_600_000), "hace 3 h");
        assert_eq!(format_age(2 * 86_400_000), "hace 2 d");
    }

    #[test]
    fn when_is_relative_to_now() {
        let tz = FixedOffset::east_opt(0).unwrap();
        // 2026-11-18 (a Wednesday) 10:00.
        let now = tz.with_ymd_and_hms(2026, 11, 18, 10, 0, 0).unwrap();
        let at = |d: i64, h: i64| now.timestamp_millis() + d * 86_400_000 + h * 3_600_000;
        assert_eq!(format_when(&now, at(0, 4)), "hoy 14:00");
        assert_eq!(format_when(&now, at(1, -1)), "mañana 09:00");
        assert_eq!(format_when(&now, at(2, 4)), "el viernes 14:00");
        assert_eq!(format_when(&now, at(6, 0)), "el martes 10:00");
        assert_eq!(format_when(&now, at(10, 0)), "el 28 nov 10:00");
    }

    #[test]
    fn spanish_day_labels() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap();
        assert_eq!(day_label(d), "jue 19 nov");
    }
}
