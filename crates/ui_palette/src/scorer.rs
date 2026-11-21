//! Fuzzy subsequence scorer: the classic fzy dynamic programme (consecutive-match, word-boundary
//! and camelCase bonuses, small gap penalties) plus a prefix / contiguous-substring bonus, with
//! case and Spanish accent folding (`pestana` finds `Pestaña`).
//!
//! No dependencies and no allocations after warm-up: a [`Matcher`] owns its scratch buffers and is
//! reused for every candidate of a query. A query is split into whitespace tokens and **every
//! token must match**, in any order (`login feat` finds `feat/login-form`).

const GAP_LEADING: f32 = -0.005;
const GAP_TRAILING: f32 = -0.005;
const GAP_INNER: f32 = -0.01;
const MATCH_CONSECUTIVE: f32 = 1.0;
const MATCH_SLASH: f32 = 0.9;
const MATCH_WORD: f32 = 0.8;
const MATCH_CAPITAL: f32 = 0.7;
const MATCH_DOT: f32 = 0.6;
/// Extra raw score when the token is a prefix of the text / starts a word / is contiguous.
const BONUS_PREFIX: f32 = 1.5;
const BONUS_WORD_START: f32 = 0.8;
const BONUS_CONTIGUOUS: f32 = 0.4;
/// Raw score per char of a token that equals the whole text.
const EXACT_PER_CHAR: f32 = 3.0;
/// Longest text considered (chars); the rest is ignored.
const MAX_TEXT: usize = 256;
/// Most tokens of a query that are used.
const MAX_TOKENS: usize = 5;
const NEG_INF: f32 = f32::NEG_INFINITY;

/// Lowercases a char and drops the accents of the Spanish / Latin-1 letters.
pub fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' | 'ã' | 'Á' | 'À' | 'Ä' | 'Â' | 'Ã' => 'a',
        'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'õ' | 'Ó' | 'Ò' | 'Ö' | 'Ô' | 'Õ' => 'o',
        'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
        'ñ' | 'Ñ' => 'n',
        'ç' | 'Ç' => 'c',
        _ => c.to_lowercase().next().unwrap_or(c),
    }
}

/// A parsed query: folded tokens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Needle {
    tokens: Vec<Vec<char>>,
}

impl Needle {
    pub fn new(query: &str) -> Self {
        let tokens = query
            .split_whitespace()
            .take(MAX_TOKENS)
            .map(|t| t.chars().map(fold).collect())
            .collect();
        Self { tokens }
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Reusable scratch space; create one per query (or per thread) and score many candidates.
#[derive(Default)]
pub struct Matcher {
    folded: Vec<char>,
    bonus: Vec<f32>,
    d_prev: Vec<f32>,
    m_prev: Vec<f32>,
    d_cur: Vec<f32>,
    m_cur: Vec<f32>,
}

impl Matcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Score of `needle` against weighted `fields` (`(text, weight)`), or `None` when some token
    /// matches none of them. Higher is better; roughly 0..3 per token. An empty needle scores 0.
    pub fn score(&mut self, needle: &Needle, fields: &[(&str, f32)]) -> Option<f32> {
        if needle.tokens.is_empty() {
            return Some(0.0);
        }
        let mut total = 0.0;
        for tok in &needle.tokens {
            let mut best: Option<f32> = None;
            for (text, weight) in fields {
                if let Some(s) = self.score_token(tok, text) {
                    let s = s * weight;
                    best = Some(best.map_or(s, |b| b.max(s)));
                }
            }
            total += best?;
        }
        Some(total / needle.tokens.len() as f32)
    }

    /// Score of one token against one text, normalised per token char.
    fn score_token(&mut self, tok: &[char], text: &str) -> Option<f32> {
        let m = tok.len();
        if m == 0 {
            return Some(0.0);
        }
        self.folded.clear();
        self.bonus.clear();
        let mut prev: Option<char> = None;
        for c in text.chars().take(MAX_TEXT) {
            self.folded.push(fold(c));
            self.bonus.push(match prev {
                None => MATCH_SLASH,
                Some(p) => bonus_after(p, c),
            });
            prev = Some(c);
        }
        let n = self.folded.len();
        if m > n || !is_subsequence(tok, &self.folded) {
            return None;
        }
        if m == n {
            // Same length and a subsequence: the same text.
            return Some(EXACT_PER_CHAR);
        }

        self.d_prev.clear();
        self.d_prev.resize(n, NEG_INF);
        self.m_prev.clear();
        self.m_prev.resize(n, NEG_INF);
        self.d_cur.clear();
        self.d_cur.resize(n, NEG_INF);
        self.m_cur.clear();
        self.m_cur.resize(n, NEG_INF);
        for (i, &nc) in tok.iter().enumerate() {
            let gap = if i + 1 == m { GAP_TRAILING } else { GAP_INNER };
            let mut prev_score = NEG_INF;
            for j in 0..n {
                if nc == self.folded[j] {
                    let score = if i == 0 {
                        j as f32 * GAP_LEADING + self.bonus[j]
                    } else if j > 0 {
                        (self.m_prev[j - 1] + self.bonus[j]).max(self.d_prev[j - 1] + MATCH_CONSECUTIVE)
                    } else {
                        NEG_INF
                    };
                    self.d_cur[j] = score;
                    prev_score = score.max(prev_score + gap);
                } else {
                    self.d_cur[j] = NEG_INF;
                    prev_score += gap;
                }
                self.m_cur[j] = prev_score;
            }
            std::mem::swap(&mut self.d_prev, &mut self.d_cur);
            std::mem::swap(&mut self.m_prev, &mut self.m_cur);
        }
        let mut raw = self.m_prev[n - 1];
        if raw == NEG_INF {
            return None;
        }
        // Prefix / contiguous bonuses.
        if let Some(at) = find(tok, &self.folded) {
            raw += if at == 0 {
                BONUS_PREFIX
            } else if self.bonus[at] >= MATCH_CAPITAL {
                BONUS_WORD_START
            } else {
                BONUS_CONTIGUOUS
            };
        }
        Some(raw / m as f32)
    }
}

fn bonus_after(prev: char, cur: char) -> f32 {
    match prev {
        '/' | '\\' => MATCH_SLASH,
        '.' => MATCH_DOT,
        p if !p.is_alphanumeric() => MATCH_WORD,
        p if p.is_lowercase() && cur.is_uppercase() => MATCH_CAPITAL,
        _ => 0.0,
    }
}

fn is_subsequence(needle: &[char], hay: &[char]) -> bool {
    let mut it = hay.iter();
    needle.iter().all(|n| it.any(|h| h == n))
}

/// First contiguous occurrence of `needle` in `hay`.
fn find(needle: &[char], hay: &[char]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rank(query: &str, candidates: &[&str]) -> Vec<String> {
        let needle = Needle::new(query);
        let mut m = Matcher::new();
        let mut scored: Vec<(f32, &str)> = candidates
            .iter()
            .filter_map(|c| m.score(&needle, &[(c, 1.0)]).map(|s| (s, *c)))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().map(|(_, c)| c.to_string()).collect()
    }

    #[test]
    fn ranking_table() {
        // (query, candidates, expected order of the ones that match)
        let table: &[(&str, &[&str], &[&str])] = &[
            // Word starts beat scattered letters.
            (
                "gs",
                &["git status", "gist search results", "agents settings", "xgxsx"],
                &["git status", "gist search results", "agents settings", "xgxsx"],
            ),
            // Prefix beats contained.
            ("term", &["Nueva terminal", "Terminal", "determinar"], &["Terminal", "Nueva terminal", "determinar"]),
            // Exact beats prefix beats the rest.
            ("git", &["git", "git status", "digit"], &["git", "git status", "digit"]),
            // Consecutive beats gapped.
            ("sta", &["status", "s_t_a"], &["status", "s_t_a"]),
            // Accents and case are folded.
            ("pestana", &["Nueva Pestaña", "Cerrar pestana", "Perfil"], &["Nueva Pestaña", "Cerrar pestana"]),
            ("PEST", &["pestaña", "mi pest"], &["pestaña", "mi pest"]),
            // Tokens in any order, all required.
            ("login feat", &["feat/login-form", "fix/login", "feat/signup"], &["feat/login-form"]),
            // Path separators are word boundaries.
            (
                "ter",
                &["C:/Users/venta/Desktop/Terminal", "C:/Users/venta/otter"],
                &["C:/Users/venta/Desktop/Terminal", "C:/Users/venta/otter"],
            ),
            // camelCase initials.
            ("fb", &["fooBar", "foobar"], &["fooBar", "foobar"]),
            // No subsequence, no match.
            ("zzz", &["git", "terminal"], &[]),
        ];
        for (q, cands, expected) in table {
            assert_eq!(rank(q, cands), *expected, "query {q:?}");
        }
    }

    #[test]
    fn empty_needle_matches_everything_with_zero() {
        let mut m = Matcher::new();
        assert_eq!(m.score(&Needle::new("   "), &[("x", 1.0)]), Some(0.0));
    }

    #[test]
    fn field_weights_prefer_the_title() {
        let mut m = Matcher::new();
        let n = Needle::new("deploy");
        let in_title = m.score(&n, &[("deploy prod", 1.0), ("misc", 0.5)]).unwrap();
        let in_sub = m.score(&n, &[("misc", 1.0), ("deploy prod", 0.5)]).unwrap();
        assert!(in_title > in_sub);
    }

    #[test]
    fn long_text_is_truncated_not_rejected() {
        let mut m = Matcher::new();
        let text = format!("abc{}", "x".repeat(10_000));
        assert!(m.score(&Needle::new("abc"), &[(&text, 1.0)]).is_some());
    }

    #[test]
    fn scoring_is_fast() {
        // 5 000 candidates x a 3-token query must stay far below a frame budget even unoptimised.
        let cands: Vec<String> =
            (0..5000).map(|i| format!("C:/Users/venta/Desktop/project-{i}/src/module_{i}.rs")).collect();
        let needle = Needle::new("proj mod rs");
        let mut m = Matcher::new();
        let t = std::time::Instant::now();
        let hits = cands.iter().filter(|c| m.score(&needle, &[(c, 1.0)]).is_some()).count();
        assert_eq!(hits, 5000);
        assert!(t.elapsed().as_millis() < 1500, "{:?}", t.elapsed());
    }
}
