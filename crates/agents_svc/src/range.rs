//! Time ranges resolved in a caller-supplied timezone (local time in the app, fixed offsets in tests).

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Weekday};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Range {
    /// Everything recorded for one session id (any time).
    Session(String),
    /// Local calendar day containing "now".
    Today,
    /// Local calendar week containing "now", starting on the configured weekday (default Monday).
    ThisWeek,
    /// Rolling `now - 7 days .. now`.
    Last7Days,
    /// `[start_ms, end_ms)` unix milliseconds.
    Custom { start_ms: i64, end_ms: i64 },
    /// No time filter.
    All,
}

/// Resolved filter: half-open `[start_ms, end_ms)` plus an optional session filter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bounds {
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub session: Option<String>,
}

/// Unix ms of local midnight at the start of `date`. DST gaps at midnight fall forward to the first
/// valid instant of the day.
fn midnight_ms<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> i64 {
    for h in 0..24 {
        if let Some(t) = date.and_hms_opt(h, 0, 0) {
            if let Some(dt) = tz.from_local_datetime(&t).earliest() {
                return dt.timestamp_millis();
            }
        }
    }
    // Unreachable in practice; fall back to treating the date as UTC.
    date.and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc().timestamp_millis())
        .unwrap_or(0)
}

impl Range {
    pub fn resolve<Tz: TimeZone>(&self, now: &DateTime<Tz>, week_start: Weekday) -> Bounds {
        let tz = now.timezone();
        let today = now.date_naive();
        match self {
            Range::Session(s) => Bounds {
                session: Some(s.clone()),
                ..Default::default()
            },
            Range::Today => Bounds {
                start_ms: Some(midnight_ms(&tz, today)),
                end_ms: Some(midnight_ms(&tz, today + Duration::days(1))),
                session: None,
            },
            Range::ThisWeek => {
                let back = (today.weekday().num_days_from_monday() + 7
                    - week_start.num_days_from_monday())
                    % 7;
                let start = today - Duration::days(back as i64);
                Bounds {
                    start_ms: Some(midnight_ms(&tz, start)),
                    end_ms: Some(midnight_ms(&tz, start + Duration::days(7))),
                    session: None,
                }
            }
            Range::Last7Days => {
                let end = now.timestamp_millis();
                Bounds {
                    start_ms: Some(end - 7 * 86_400_000),
                    end_ms: Some(end + 1),
                    session: None,
                }
            }
            Range::Custom { start_ms, end_ms } => Bounds {
                start_ms: Some(*start_ms),
                end_ms: Some(*end_ms),
                session: None,
            },
            Range::All => Bounds::default(),
        }
    }
}
