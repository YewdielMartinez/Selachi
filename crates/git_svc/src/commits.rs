//! Recent commits (`git log`) model and parser.

use crate::error::{Error, Result};

/// `git log` format argument used by [`crate::Repo::recent_commits`].
pub(crate) const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    /// Author date, seconds since the Unix epoch.
    pub timestamp: i64,
    pub subject: String,
}

/// Parse output of `git log --format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e`.
pub fn parse_log(text: &str) -> Result<Vec<Commit>> {
    let mut out = Vec::new();
    for rec in text.split('\u{1e}') {
        let rec = rec.trim_matches(|c| c == '\n' || c == '\r');
        if rec.is_empty() {
            continue;
        }
        let mut f = rec.splitn(5, '\u{1f}');
        let (Some(hash), Some(short), Some(author), Some(ts), subject) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            return Err(Error::Parse(format!("bad log record: {rec:?}")));
        };
        out.push(Commit {
            hash: hash.to_owned(),
            short_hash: short.to_owned(),
            author: author.to_owned(),
            timestamp: ts.parse().unwrap_or(0),
            subject: subject.unwrap_or("").to_owned(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records() {
        let s = "aaa\u{1f}a\u{1f}Ann Lee\u{1f}1700000000\u{1f}feat: x y\u{1e}\nbbb\u{1f}b\u{1f}Zoë\u{1f}1690000000\u{1f}\u{1e}\n";
        let c = parse_log(s).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].author, "Ann Lee");
        assert_eq!(c[0].timestamp, 1_700_000_000);
        assert_eq!(c[0].subject, "feat: x y");
        assert_eq!(c[1].author, "Zoë");
        assert_eq!(c[1].subject, "");
    }

    #[test]
    fn empty_is_ok() {
        assert!(parse_log("").unwrap().is_empty());
    }

    #[test]
    fn malformed_is_error() {
        assert!(parse_log("only-one-field\u{1e}").is_err());
    }
}
