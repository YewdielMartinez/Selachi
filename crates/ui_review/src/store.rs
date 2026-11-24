//! In-memory drafts per `(root, source)`, so closing a review tab and opening it again (or opening
//! the same review from another entry point) does not lose the notes. The host persists them
//! with [`ReviewStore::export_all`] / [`ReviewStore::import_all`] (JSON) if it wants them to
//! survive a restart.

use std::collections::BTreeMap;
use std::path::Path;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::diff::ReviewSource;
use crate::note::{Note, NoteSet};

/// `(normalised root, source key)`.
pub type DraftKey = (String, String);

/// Root as a comparable string: forward slashes, no trailing slash, lowercase (Windows).
pub fn root_key(root: &Path) -> String {
    root.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

pub fn draft_key(source: &ReviewSource) -> DraftKey {
    (root_key(source.root()), source.key())
}

#[derive(Serialize, Deserialize)]
struct Entry {
    root: String,
    source: String,
    notes: Vec<Note>,
}

#[derive(Serialize, Deserialize)]
struct Export {
    version: u32,
    reviews: Vec<Entry>,
}

/// The drafts of every review of the session (a gpui [`Global`], created on first use).
#[derive(Default)]
pub struct ReviewStore {
    map: BTreeMap<DraftKey, NoteSet>,
}

impl Global for ReviewStore {}

impl ReviewStore {
    pub fn get(&self, key: &DraftKey) -> Option<&NoteSet> {
        self.map.get(key)
    }

    /// Stores `notes` for `key`; an empty set removes the entry.
    pub fn set(&mut self, key: DraftKey, notes: NoteSet) {
        if notes.is_empty() {
            self.map.remove(&key);
        } else {
            self.map.insert(key, notes);
        }
    }

    /// Number of reviews with notes.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Every draft as JSON, for the host to save (`state.db`, a file...).
    pub fn export_all(&self) -> String {
        let reviews = self
            .map
            .iter()
            .map(|((root, source), set)| Entry {
                root: root.clone(),
                source: source.clone(),
                notes: set.notes().to_vec(),
            })
            .collect();
        serde_json::to_string_pretty(&Export {
            version: 1,
            reviews,
        })
        .unwrap_or_else(|_| "{}".to_string())
    }

    /// Merges what [`ReviewStore::export_all`] produced. Returns how many notes were added.
    pub fn import_all(&mut self, json: &str) -> Result<usize, String> {
        let export: Export =
            serde_json::from_str(json).map_err(|e| format!("borradores ilegibles: {e}"))?;
        if export.version != 1 {
            return Err(format!(
                "versión de borradores no soportada: {}",
                export.version
            ));
        }
        let mut added = 0;
        for e in export.reviews {
            let set = self.map.entry((e.root, e.source)).or_default();
            let json =
                serde_json::to_string(&serde_json::json!({ "version": 1, "notes": e.notes }))
                    .map_err(|e| e.to_string())?;
            added += set.merge_json(&json)?;
        }
        Ok(added)
    }

    /// The store of the app (created empty on first use).
    pub fn global(cx: &mut App) -> &mut ReviewStore {
        cx.default_global::<ReviewStore>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{NoteId, Side, SideLines};

    fn lines() -> SideLines {
        (1..=5).map(|i| (i, format!("l{i}"))).collect()
    }

    fn set_with(text: &str) -> NoteSet {
        let mut s = NoteSet::new();
        let id = s.next_id();
        s.push(Note::new(id, "a.rs", Side::New, 2, 2, text, &lines(), 0));
        s
    }

    #[test]
    fn keys_normalise_the_root() {
        let a = ReviewSource::Worktree {
            root: "C:\\Users\\Me\\Repo\\".into(),
        };
        let b = ReviewSource::Worktree {
            root: "c:/users/me/repo".into(),
        };
        assert_eq!(draft_key(&a), draft_key(&b));
        let c = ReviewSource::SinceCheckpoint {
            root: "c:/users/me/repo".into(),
            id: "1-a".into(),
        };
        assert_ne!(draft_key(&a), draft_key(&c));
    }

    #[test]
    fn set_get_and_empty_removes() {
        let mut st = ReviewStore::default();
        let key = ("r".to_string(), "worktree".to_string());
        st.set(key.clone(), set_with("hola"));
        assert_eq!(st.get(&key).map(NoteSet::len), Some(1));
        st.set(key.clone(), NoteSet::new());
        assert!(st.get(&key).is_none());
        assert!(st.is_empty());
    }

    #[test]
    fn export_import_round_trip() {
        let mut st = ReviewStore::default();
        st.set(("r1".into(), "worktree".into()), set_with("uno"));
        st.set(("r2".into(), "checkpoint:9".into()), set_with("dos"));
        let json = st.export_all();

        let mut other = ReviewStore::default();
        assert_eq!(other.import_all(&json), Ok(2));
        assert_eq!(other.len(), 2);
        assert_eq!(
            other
                .get(&("r2".to_string(), "checkpoint:9".to_string()))
                .map(|s| s.notes()[0].text.clone()),
            Some("dos".to_string())
        );
        assert_eq!(
            other.import_all(&json),
            Ok(0),
            "importing twice adds nothing"
        );
        assert!(other.import_all("[]").is_err());
        let _ = NoteId(0);
    }
}
