//! Prefix scopes: the first character of the input narrows the palette to one kind of result.
//!
//! | prefix | scope |
//! |---|---|
//! | `>` | acciones (app actions, git, layout presets) |
//! | `#` | historial |
//! | `@` | proyectos |
//! | `!` | tareas |
//! | `$` | snippets |
//! | none | everything, ranked |

/// Which sources a query goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    All,
    Actions,
    History,
    Projects,
    Tasks,
    Snippets,
}

impl Scope {
    /// Order of the Tab cycle.
    pub const CYCLE: [Scope; 6] =
        [Scope::All, Scope::Actions, Scope::History, Scope::Projects, Scope::Tasks, Scope::Snippets];

    pub const fn prefix(self) -> Option<char> {
        match self {
            Scope::All => None,
            Scope::Actions => Some('>'),
            Scope::History => Some('#'),
            Scope::Projects => Some('@'),
            Scope::Tasks => Some('!'),
            Scope::Snippets => Some('$'),
        }
    }

    pub const fn from_prefix(c: char) -> Option<Scope> {
        match c {
            '>' => Some(Scope::Actions),
            '#' => Some(Scope::History),
            '@' => Some(Scope::Projects),
            '!' => Some(Scope::Tasks),
            '$' => Some(Scope::Snippets),
            _ => None,
        }
    }

    /// Chip text shown next to the input.
    pub const fn label(self) -> &'static str {
        match self {
            Scope::All => "Todo",
            Scope::Actions => "Acciones",
            Scope::History => "Historial",
            Scope::Projects => "Proyectos",
            Scope::Tasks => "Tareas",
            Scope::Snippets => "Snippets",
        }
    }

    pub const fn placeholder(self) -> &'static str {
        match self {
            Scope::All => "Buscar pestañas, acciones, proyectos, comandos…",
            Scope::Actions => "Buscar una acción, comando git o diseño…",
            Scope::History => "Buscar en el historial de comandos…",
            Scope::Projects => "Abrir un proyecto o carpeta reciente…",
            Scope::Tasks => "Tareas del proyecto (npm, dotnet, cargo…)…",
            Scope::Snippets => "Buscar un snippet…",
        }
    }

    fn cycle_index(self) -> usize {
        Self::CYCLE.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn next(self) -> Scope {
        Self::CYCLE[(self.cycle_index() + 1) % Self::CYCLE.len()]
    }

    pub fn prev(self) -> Scope {
        Self::CYCLE[(self.cycle_index() + Self::CYCLE.len() - 1) % Self::CYCLE.len()]
    }
}

/// A parsed input line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub scope: Scope,
    /// The text after the prefix, left-trimmed.
    pub text: String,
}

impl Query {
    /// `"> nueva"` -> `Actions` + `"nueva"`; `"nueva"` -> `All` + `"nueva"`.
    pub fn parse(raw: &str) -> Query {
        let mut chars = raw.chars();
        match chars.next().and_then(Scope::from_prefix) {
            Some(scope) => Query { scope, text: chars.as_str().trim_start().to_string() },
            None => Query { scope: Scope::All, text: raw.trim_start().to_string() },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// The raw input that selects `scope` and keeps this query's text (what Tab writes back).
    pub fn rewritten(&self, scope: Scope) -> String {
        match scope.prefix() {
            Some(p) if self.text.is_empty() => p.to_string(),
            Some(p) => format!("{p}{}", self.text),
            None => self.text.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_routing_table() {
        let table = [
            ("", Scope::All, ""),
            ("git", Scope::All, "git"),
            ("  git", Scope::All, "git"),
            (">", Scope::Actions, ""),
            ("> nueva pestaña", Scope::Actions, "nueva pestaña"),
            (">nueva", Scope::Actions, "nueva"),
            ("#git push", Scope::History, "git push"),
            ("@selachi", Scope::Projects, "selachi"),
            ("!dev", Scope::Tasks, "dev"),
            ("$migr", Scope::Snippets, "migr"),
            // Only the first char is a prefix.
            ("git > x", Scope::All, "git > x"),
            // `:` (go to line) is not routed.
            (":12", Scope::All, ":12"),
        ];
        for (raw, scope, text) in table {
            let q = Query::parse(raw);
            assert_eq!((q.scope, q.text.as_str()), (scope, text), "input {raw:?}");
        }
    }

    #[test]
    fn tab_cycles_every_scope_and_back() {
        let mut s = Scope::All;
        let mut seen = Vec::new();
        for _ in 0..Scope::CYCLE.len() {
            seen.push(s);
            s = s.next();
        }
        assert_eq!(s, Scope::All);
        assert_eq!(seen, Scope::CYCLE);
        assert_eq!(Scope::All.prev(), Scope::Snippets);
        for sc in Scope::CYCLE {
            assert_eq!(sc.next().prev(), sc);
        }
    }

    #[test]
    fn rewriting_keeps_the_text_and_swaps_the_prefix() {
        let q = Query::parse("#git push");
        assert_eq!(q.rewritten(Scope::Projects), "@git push");
        assert_eq!(q.rewritten(Scope::All), "git push");
        assert_eq!(Query::parse("").rewritten(Scope::Tasks), "!");
        // Round trip through parse.
        for sc in Scope::CYCLE {
            assert_eq!(Query::parse(&q.rewritten(sc)).scope, sc);
        }
    }
}
