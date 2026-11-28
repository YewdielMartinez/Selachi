//! Path globs for a role's `when`: `*` (inside one path segment), `**` (any number of segments, also
//! none), `?` (one character). Case-insensitive, `/` and `\` are the same separator. A pattern with
//! no separator matches the file name at any depth (`*.tsx` = `**/*.tsx`), like `.gitignore`.

/// Why a `when` pattern is not usable (`None` = fine).
pub fn pattern_problem(p: &str) -> Option<&'static str> {
    let t = p.trim();
    if t.is_empty() {
        return Some("is empty");
    }
    if t.chars().count() > 200 {
        return Some("is longer than 200 characters");
    }
    if t.chars().any(|c| c.is_control()) {
        return Some("has a control character");
    }
    let norm = t.replace('\\', "/");
    if norm.starts_with('/') || norm.contains(':') {
        return Some("must be relative to the project (no drive, no leading `/`)");
    }
    if norm.split('/').any(|s| s == "..") {
        return Some("must not contain `..`");
    }
    None
}

fn segments(s: &str) -> Vec<String> {
    s.replace('\\', "/")
        .split('/')
        .filter(|x| !x.is_empty() && *x != ".")
        .map(|x| x.to_lowercase())
        .collect()
}

/// `*` and `?` inside one segment.
fn seg_match(pat: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while t < text.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) && pat[p] != '*' {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

fn match_segments(pat: &[String], path: &[String]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            // zero or more segments
            (0..=path.len()).any(|skip| match_segments(rest, &path[skip..]))
        }
        Some((first, rest)) => match path.split_first() {
            None => false,
            Some((head, tail)) => {
                let p: Vec<char> = first.chars().collect();
                let t: Vec<char> = head.chars().collect();
                seg_match(&p, &t) && match_segments(rest, tail)
            }
        },
    }
}

/// Does `pattern` match the project-relative `path`?
pub fn matches(pattern: &str, path: &str) -> bool {
    let mut pat = segments(pattern);
    if pat.is_empty() {
        return false;
    }
    if !pattern.replace('\\', "/").trim().contains('/') {
        pat.insert(0, "**".to_string());
    }
    // a pattern that ends with a folder name (`src/ui/`) covers everything under it
    if pattern.trim_end().ends_with(['/', '\\']) {
        pat.push("**".to_string());
    }
    match_segments(&pat, &segments(path))
}

/// The files of `files` that match any of `patterns`.
pub fn matching<'a>(patterns: &[String], files: &'a [String]) -> Vec<&'a str> {
    files
        .iter()
        .filter(|f| patterns.iter().any(|p| matches(p, f)))
        .map(String::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_follow_gitignore_habits() {
        assert!(matches("**/*.tsx", "src/ui/App.tsx"));
        assert!(matches("**/*.tsx", "App.tsx"));
        assert!(matches("*.tsx", "src/deep/er/App.tsx"));
        assert!(!matches("*.tsx", "src/App.ts"));
        assert!(matches("src/*.rs", "src/main.rs"));
        assert!(!matches("src/*.rs", "src/a/main.rs"));
        assert!(matches("src/**/*.rs", "src/a/b/main.rs"));
        assert!(matches("src/**", "src/a/b/main.rs"));
        assert!(matches("src/ui/", "src/ui/x/y.rs"));
        assert!(!matches("src/ui/", "src/uix/y.rs"));
        assert!(matches("SRC\\*.RS", "src/Main.rs"), "case and separators");
        assert!(matches("crates/*/src/lib.rs", "crates/store/src/lib.rs"));
        assert!(matches("a?c.txt", "abc.txt"));
        assert!(!matches("a?c.txt", "ac.txt"));
        assert!(!matches("", "x"));
    }

    #[test]
    fn bad_patterns_are_named() {
        assert!(pattern_problem("**/*.tsx").is_none());
        assert_eq!(pattern_problem("  "), Some("is empty"));
        assert!(pattern_problem("../x").unwrap().contains(".."));
        assert!(pattern_problem("C:/x").unwrap().contains("relative"));
        assert!(pattern_problem("/etc").unwrap().contains("relative"));
    }

    #[test]
    fn matching_filters_a_list() {
        let files: Vec<String> = ["a/b.tsx", "c.rs", "d.css"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pats = vec!["*.tsx".to_string(), "*.css".to_string()];
        assert_eq!(matching(&pats, &files), ["a/b.tsx", "d.css"]);
        assert!(matching(&["*.go".to_string()], &files).is_empty());
    }
}
