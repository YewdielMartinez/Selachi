//! Snippets (A12): saved command templates with `{parametros}` (see
//! [`crate::snippets`]), global or tied to a project folder. Six useful ones are
//! seeded by the schema migration on first run; deleting them is permanent.

use super::{now_ms, StateDb};
use crate::error::{Result, StoreError};
use crate::paths::{clean_display, normalize_key};
use crate::snippets::{parse_template, Param};
use rusqlite::{params, Row};

const MAX_TEMPLATE_LEN: usize = 8192;

/// Where a snippet is offered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SnippetScope {
    /// Everywhere.
    #[default]
    Global,
    /// Only when the cwd is this folder or inside it.
    Project(String),
}

/// A snippet to create or to replace an existing one.
#[derive(Debug, Clone, Default)]
pub struct NewSnippet {
    pub name: String,
    pub template: String,
    pub description: String,
    pub tags: Vec<String>,
    pub scope: SnippetScope,
}

impl NewSnippet {
    pub fn new(name: impl Into<String>, template: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            template: template.into(),
            ..Self::default()
        }
    }
}

/// A stored snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub template: String,
    pub description: String,
    /// Lowercase, no commas.
    pub tags: Vec<String>,
    pub scope: SnippetScope,
    /// Unix ms.
    pub created: i64,
    pub last_used: Option<i64>,
    pub uses: u32,
}

impl Snippet {
    /// The placeholders of the template.
    pub fn params(&self) -> Vec<Param> {
        parse_template(&self.template)
    }

    /// Visible from `cwd`: global, or a project that contains it.
    pub fn visible_in(&self, cwd: Option<&str>) -> bool {
        match &self.scope {
            SnippetScope::Global => true,
            SnippetScope::Project(root) => cwd.is_some_and(|c| {
                let c = normalize_key(&clean_display(c));
                let r = normalize_key(root);
                c == r || c.strip_prefix(&r).is_some_and(|rest| rest.starts_with('/'))
            }),
        }
    }
}

const COLUMNS: &str =
    "id, name, template, description, tags, scope, scope_path, created, last_used, uses";

fn row_to_snippet(r: &Row<'_>) -> rusqlite::Result<Snippet> {
    let tags: String = r.get(4)?;
    let scope: String = r.get(5)?;
    let scope_path: String = r.get(6)?;
    Ok(Snippet {
        id: r.get(0)?,
        name: r.get(1)?,
        template: r.get(2)?,
        description: r.get(3)?,
        tags: tags
            .split(',')
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        scope: if scope == "project" {
            SnippetScope::Project(scope_path)
        } else {
            SnippetScope::Global
        },
        created: r.get(7)?,
        last_used: r.get(8)?,
        uses: r.get::<_, i64>(9)?.max(0) as u32,
    })
}

struct Prepared {
    name: String,
    template: String,
    description: String,
    tags: String,
    scope: &'static str,
    scope_path: String,
    scope_key: String,
}

fn prepare(new: NewSnippet) -> Result<Prepared> {
    let name = new.name.trim().to_string();
    let template = new.template.trim_end().to_string();
    if name.is_empty() {
        return Err(StoreError::Invalid("snippet name is empty".into()));
    }
    if template.trim().is_empty() {
        return Err(StoreError::Invalid("snippet template is empty".into()));
    }
    if template.len() > MAX_TEMPLATE_LEN {
        return Err(StoreError::Invalid(format!(
            "snippet template is longer than {MAX_TEMPLATE_LEN} bytes"
        )));
    }
    let mut tags: Vec<String> = Vec::new();
    for t in new.tags {
        let t = t
            .to_lowercase()
            .replace(',', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    }
    let (scope, scope_path, scope_key) = match new.scope {
        SnippetScope::Global => ("global", String::new(), String::new()),
        SnippetScope::Project(p) => {
            let display = clean_display(&p);
            if display.trim().is_empty() {
                return Err(StoreError::Invalid("snippet project path is empty".into()));
            }
            let key = normalize_key(&display);
            ("project", display, key)
        }
    };
    Ok(Prepared {
        name,
        template,
        description: new.description.trim().to_string(),
        tags: tags.join(","),
        scope,
        scope_path,
        scope_key,
    })
}

impl StateDb {
    /// Create a snippet; returns its id.
    pub fn add_snippet(&self, new: NewSnippet) -> Result<i64> {
        let p = prepare(new)?;
        let now = now_ms();
        self.exec_sync(move |conn| {
            conn.execute(
                "INSERT INTO snippets(name, template, description, tags, scope, scope_path, scope_key, created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![p.name, p.template, p.description, p.tags, p.scope, p.scope_path, p.scope_key, now],
            )?;
            Ok(conn.last_insert_rowid())
        })
    }

    /// Replace a snippet's content (usage stats are kept). `false` if it does not exist.
    pub fn update_snippet(&self, id: i64, new: NewSnippet) -> Result<bool> {
        let p = prepare(new)?;
        self.exec_sync(move |conn| {
            let n = conn.execute(
                "UPDATE snippets SET name = ?2, template = ?3, description = ?4, tags = ?5,
                        scope = ?6, scope_path = ?7, scope_key = ?8
                 WHERE id = ?1",
                params![
                    id,
                    p.name,
                    p.template,
                    p.description,
                    p.tags,
                    p.scope,
                    p.scope_path,
                    p.scope_key
                ],
            )?;
            Ok(n > 0)
        })
    }

    /// `false` if it did not exist.
    pub fn delete_snippet(&self, id: i64) -> Result<bool> {
        self.exec_sync(move |conn| {
            Ok(conn.execute("DELETE FROM snippets WHERE id = ?1", params![id])? > 0)
        })
    }

    pub fn get_snippet(&self, id: i64) -> Result<Option<Snippet>> {
        self.read(|conn| {
            let sql = format!("SELECT {COLUMNS} FROM snippets WHERE id = ?1");
            match conn.query_row(&sql, params![id], row_to_snippet) {
                Ok(s) => Ok(Some(s)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    /// Every snippet, by name.
    pub fn list_snippets(&self) -> Result<Vec<Snippet>> {
        self.read(|conn| {
            let sql = format!("SELECT {COLUMNS} FROM snippets ORDER BY name COLLATE NOCASE, id");
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map([], row_to_snippet)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
    }

    /// Count a use (call when the snippet was actually run). Returns immediately.
    pub fn record_snippet_use(&self, id: i64) -> Result<()> {
        let now = now_ms();
        self.exec_async("record_snippet_use", move |conn| {
            conn.execute(
                "UPDATE snippets SET uses = uses + 1, last_used = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            Ok(())
        })
    }

    /// Snippets visible from `cwd` (global ones plus those of the project that
    /// contains it) matching every word of `query` in name, description, tags or
    /// template. Ranked by name match, then project scope, usage and recency; an
    /// empty query lists the visible ones by usage.
    pub fn search_snippets(&self, query: &str, cwd: Option<&str>) -> Result<Vec<Snippet>> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let now = now_ms();
        let mut scored: Vec<(f64, Snippet)> = self
            .list_snippets()?
            .into_iter()
            .filter(|s| s.visible_in(cwd))
            .filter_map(|s| {
                let name = s.name.to_lowercase();
                let hay = format!(
                    "{name}\n{}\n{}\n{}",
                    s.description.to_lowercase(),
                    s.tags.join(" "),
                    s.template.to_lowercase()
                );
                if !words.iter().all(|w| hay.contains(w.as_str())) {
                    return None;
                }
                let mut score = 0.0;
                for w in &words {
                    if name.starts_with(w.as_str()) {
                        score += 3.0;
                    } else if name
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|t| t.starts_with(w.as_str()))
                    {
                        score += 2.5;
                    } else if name.contains(w.as_str()) {
                        score += 2.0;
                    } else if s.tags.iter().any(|t| t == w) {
                        score += 1.5;
                    } else {
                        score += 1.0;
                    }
                }
                if matches!(s.scope, SnippetScope::Project(_)) {
                    score += 1.5;
                }
                score += (1.0 + f64::from(s.uses)).ln();
                if let Some(t) = s.last_used {
                    let days = ((now - t).max(0) as f64) / 86_400_000.0;
                    score += 2.0 / (1.0 + days);
                }
                Some((score, s))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
        });
        Ok(scored.into_iter().map(|(_, s)| s).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::temp_db;
    use crate::snippets::{render, Quoting};
    use std::collections::HashMap;

    #[test]
    fn six_defaults_are_seeded_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let db = StateDb::open(&path).unwrap();
        let all = db.list_snippets().unwrap();
        assert_eq!(all.len(), 6);
        let templates: Vec<&str> = all.iter().map(|s| s.template.as_str()).collect();
        for t in [
            "dotnet ef migrations add {nombre}",
            "git switch -c {rama}",
            "npm i {paquete}",
            "docker compose logs -f {servicio}",
            "cargo test -p {crate}",
            "git log --oneline -n {n:20}",
        ] {
            assert!(templates.contains(&t), "missing seed {t}");
        }
        assert!(all.iter().all(|s| s.scope == SnippetScope::Global));
        // Delete one, reopen: not re-seeded.
        let id = all[0].id;
        assert!(db.delete_snippet(id).unwrap());
        drop(db);
        let db = StateDb::open(&path).unwrap();
        assert_eq!(db.list_snippets().unwrap().len(), 5);
    }

    #[test]
    fn crud_roundtrip() {
        let (_d, db) = temp_db();
        let id = db
            .add_snippet(NewSnippet {
                name: "  Deploy ".into(),
                template: "kubectl rollout restart deploy/{app}  ".into(),
                description: "restart".into(),
                tags: vec!["K8s".into(), "ops, prod".into(), "k8s".into(), " ".into()],
                scope: SnippetScope::Global,
            })
            .unwrap();
        let s = db.get_snippet(id).unwrap().unwrap();
        assert_eq!(s.name, "Deploy");
        assert_eq!(s.template, "kubectl rollout restart deploy/{app}");
        assert_eq!(s.tags, ["k8s", "ops prod"]);
        assert_eq!(s.uses, 0);
        assert_eq!(s.last_used, None);
        assert_eq!(s.params().len(), 1);

        assert!(db
            .update_snippet(id, NewSnippet::new("Deploy 2", "echo {x}"))
            .unwrap());
        let s = db.get_snippet(id).unwrap().unwrap();
        assert_eq!(
            (s.name.as_str(), s.template.as_str()),
            ("Deploy 2", "echo {x}")
        );
        assert!(!db.update_snippet(9999, NewSnippet::new("a", "b")).unwrap());

        assert!(db.delete_snippet(id).unwrap());
        assert!(!db.delete_snippet(id).unwrap());
        assert_eq!(db.get_snippet(id).unwrap(), None);
    }

    #[test]
    fn invalid_snippets_are_rejected() {
        let (_d, db) = temp_db();
        assert!(db.add_snippet(NewSnippet::new("", "x")).is_err());
        assert!(db.add_snippet(NewSnippet::new("x", "  ")).is_err());
        assert!(db
            .add_snippet(NewSnippet::new("x", "a".repeat(MAX_TEMPLATE_LEN + 1)))
            .is_err());
        assert!(db
            .add_snippet(NewSnippet {
                scope: SnippetScope::Project(" ".into()),
                ..NewSnippet::new("x", "y")
            })
            .is_err());
    }

    #[test]
    fn usage_is_counted() {
        let (_d, db) = temp_db();
        let id = db.add_snippet(NewSnippet::new("a", "echo 1")).unwrap();
        db.record_snippet_use(id).unwrap();
        db.record_snippet_use(id).unwrap();
        db.flush().unwrap();
        let s = db.get_snippet(id).unwrap().unwrap();
        assert_eq!(s.uses, 2);
        assert!(s.last_used.is_some());
    }

    #[test]
    fn search_matches_words_everywhere() {
        let (_d, db) = temp_db();
        let hits = db.search_snippets("migrations", None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].template, "dotnet ef migrations add {nombre}");
        // every word must match, in any field
        assert_eq!(db.search_snippets("git switch", None).unwrap().len(), 1);
        assert!(db.search_snippets("git zzz", None).unwrap().is_empty());
        // empty query lists all visible
        assert_eq!(db.search_snippets("", None).unwrap().len(), 6);
        // tags
        assert!(!db.search_snippets("docker", None).unwrap().is_empty());
    }

    #[test]
    fn project_scope_visibility() {
        let (_d, db) = temp_db();
        db.add_snippet(NewSnippet {
            scope: SnippetScope::Project("C:\\work\\api".into()),
            ..NewSnippet::new("Run api", "dotnet run --project {p:src/Api}")
        })
        .unwrap();
        let names = |cwd: Option<&str>| -> Vec<String> {
            db.search_snippets("run api", cwd)
                .unwrap()
                .into_iter()
                .map(|s| s.name)
                .collect()
        };
        assert!(names(None).is_empty());
        assert!(names(Some("C:\\work\\other")).is_empty());
        assert!(names(Some("C:\\work\\api-old")).is_empty());
        assert_eq!(names(Some("C:\\work\\api")), ["Run api"]);
        assert_eq!(names(Some("c:/WORK/api/src/Api")), ["Run api"]);
        // project snippets rank first for an empty query inside the project
        let all = db.search_snippets("", Some("C:\\work\\api")).unwrap();
        assert_eq!(all[0].name, "Run api");
        assert_eq!(all.len(), 7);
    }

    #[test]
    fn usage_boosts_ranking() {
        let (_d, db) = temp_db();
        let a = db
            .add_snippet(NewSnippet::new("zz alpha", "echo a"))
            .unwrap();
        let b = db
            .add_snippet(NewSnippet::new("zz beta", "echo b"))
            .unwrap();
        for _ in 0..5 {
            db.record_snippet_use(b).unwrap();
        }
        db.flush().unwrap();
        let hits = db.search_snippets("zz", None).unwrap();
        assert_eq!(hits[0].id, b);
        assert_eq!(hits[1].id, a);
    }

    #[test]
    fn seed_renders_for_powershell() {
        let (_d, db) = temp_db();
        let s = db
            .search_snippets("migrations", None)
            .unwrap()
            .pop()
            .unwrap();
        let mut v = HashMap::new();
        v.insert("nombre".to_string(), "Add Users".to_string());
        assert_eq!(
            render(&s.template, &v, Quoting::PowerShell).unwrap(),
            "dotnet ef migrations add 'Add Users'"
        );
    }
}
