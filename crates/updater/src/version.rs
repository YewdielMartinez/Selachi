//! Semantic versions: `major.minor.patch[-pre][+build]`, with an optional leading `v`.

use std::cmp::Ordering;
use std::fmt;

/// A parsed version. Build metadata (`+...`) is dropped: it never affects precedence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (`beta.1` -> `["beta", "1"]`); empty for a final release.
    pub pre: Vec<String>,
}

impl Version {
    /// `0.2.0`, `v0.2.0`, `1.0.0-beta.1`, `1.2` (missing parts are 0). `None` for anything else.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let s = s.split('+').next().unwrap_or("");
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = core.split('.');
        let major = nums.next()?.parse().ok()?;
        let minor = nums.next().map_or(Some(0), |n| n.parse().ok())?;
        let patch = nums.next().map_or(Some(0), |n| n.parse().ok())?;
        if nums.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let ids: Vec<String> = p.split('.').map(str::to_string).collect();
                let ok = ids.iter().all(|i| {
                    !i.is_empty() && i.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                });
                if !ok {
                    return None;
                }
                ids
            }
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // a final release is newer than any of its pre-releases
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => cmp_pre(&self.pre, &other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            // numeric identifiers sort before alphanumeric ones
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_the_usual_spellings() {
        assert_eq!(v("0.2.0"), v("v0.2.0"));
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("1.0.0+build5"), v("1.0.0"));
        assert_eq!(v("1.0.0-beta.1").to_string(), "1.0.0-beta.1");
        for bad in ["", "x", "1.2.3.4", "1..3", "1.0.0-", "1.0.0-a b", "v"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn orders_by_semver_precedence() {
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("0.10.0") > v("0.9.0"), "numeric, not lexicographic");
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha.9"));
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        assert_eq!(v("1.0.0").cmp(&v("1.0.0+x")), Ordering::Equal);
    }
}
