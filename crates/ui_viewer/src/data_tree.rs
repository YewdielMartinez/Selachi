//! The collapsible JSON / TOML / YAML tree: the `viewer_svc` node tree plus an expansion set and a
//! flattened row list for virtualization. Containers whose children were not materialised (budget
//! or the 1000-child cap) load them lazily when expanded.

use std::collections::HashSet;

use viewer_svc::{Format, MAX_CHILDREN, Node, NodeKind, Structured};

/// One visible row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataRow {
    /// Child indices from the root.
    pub path: Vec<usize>,
    pub depth: usize,
    pub key: String,
    pub kind: NodeKind,
    pub preview: String,
    pub expandable: bool,
    pub expanded: bool,
    /// For a "N more" row: how many children are not loaded under the parent at `path`'s parent.
    pub more: Option<usize>,
}

/// Parsed document + expansion state.
pub struct DataTree {
    structured: Structured,
    root: Node,
    expanded: HashSet<Vec<usize>>,
    rows: Vec<DataRow>,
}

impl DataTree {
    /// Parses `bytes`. The root and its direct children start expanded.
    pub fn parse(format: Format, bytes: &[u8]) -> Result<DataTree, String> {
        let structured = Structured::parse(format, bytes).map_err(|e| e.to_string())?;
        let root = structured.root();
        let mut expanded = HashSet::new();
        expanded.insert(Vec::new());
        for (i, c) in root.children.iter().enumerate() {
            if c.is_container() && root.children.len() <= 40 {
                expanded.insert(vec![i]);
            }
        }
        let mut t = DataTree { structured, root, expanded, rows: Vec::new() };
        t.rebuild();
        Ok(t)
    }

    pub fn rows(&self) -> &[DataRow] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn node_at(&self, path: &[usize]) -> Option<&Node> {
        let mut cur = &self.root;
        for &i in path {
            cur = cur.children.get(i)?;
        }
        Some(cur)
    }

    fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let mut cur = &mut self.root;
        for &i in path {
            cur = cur.children.get_mut(i)?;
        }
        Some(cur)
    }

    /// Expands or collapses the container at `path`; expanding one with unloaded children loads the
    /// first batch.
    pub fn toggle(&mut self, path: &[usize]) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else if self.node_at(path).is_some_and(Node::is_container) {
            self.expanded.insert(path.to_vec());
            self.load_more(path);
        }
        self.rebuild();
    }

    /// Loads the next batch of children of the container at `path` (the "N more" row).
    pub fn load_more(&mut self, path: &[usize]) {
        let Some(node) = self.node_at(path) else { return };
        let loaded = node.children.len();
        let total = node.child_count;
        if loaded >= total {
            return;
        }
        let end = (loaded + MAX_CHILDREN).min(total);
        let more = self.structured.children(path, loaded..end);
        if let Some(node) = self.node_at_mut(path) {
            node.children.extend(more);
        }
        self.rebuild();
    }

    /// Expands every loaded container (bounded by what is materialised).
    pub fn expand_all(&mut self) {
        let mut stack = vec![(Vec::new(), &self.root)];
        let mut paths = Vec::new();
        while let Some((path, node)) = stack.pop() {
            if node.is_container() {
                paths.push(path.clone());
                for (i, c) in node.children.iter().enumerate() {
                    let mut p = path.clone();
                    p.push(i);
                    stack.push((p, c));
                }
            }
        }
        self.expanded.extend(paths);
        self.rebuild();
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.expanded.insert(Vec::new());
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let mut path = Vec::new();
        flatten(&self.root, &mut path, 0, &self.expanded, &mut rows);
        self.rows = rows;
    }
}

fn flatten(node: &Node, path: &mut Vec<usize>, depth: usize, expanded: &HashSet<Vec<usize>>, out: &mut Vec<DataRow>) {
    let expandable = node.is_container() && node.child_count > 0;
    let is_open = expandable && expanded.contains(path.as_slice());
    out.push(DataRow {
        path: path.clone(),
        depth,
        key: node.key.clone(),
        kind: node.kind,
        preview: node.value_preview.clone(),
        expandable,
        expanded: is_open,
        more: None,
    });
    if !is_open {
        return;
    }
    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        flatten(child, path, depth + 1, expanded, out);
        path.pop();
    }
    let hidden = node.hidden();
    if hidden > 0 {
        out.push(DataRow {
            path: path.clone(),
            depth: depth + 1,
            key: String::new(),
            kind: NodeKind::Null,
            preview: format!("{hidden} m\u{e1}s"),
            expandable: false,
            expanded: false,
            more: Some(hidden),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"name":"selachi","deps":{"gpui":"0.2","ignore":"0.4"},"tags":["a","b"],"n":3,"ok":true,"nil":null}"#;

    fn tree() -> DataTree {
        DataTree::parse(Format::Json, JSON.as_bytes()).unwrap()
    }

    #[test]
    fn root_and_first_level_start_expanded() {
        let t = tree();
        // root + 6 children + the children of the two containers (deps: 2, tags: 2)
        assert_eq!(t.len(), 1 + 6 + 2 + 2);
        assert_eq!(t.rows()[0].key, "$");
        assert!(t.rows()[0].expanded);
        assert_eq!(t.rows()[0].depth, 0);
    }

    #[test]
    fn toggle_collapses_and_expands() {
        let mut t = tree();
        let deps = t.rows().iter().find(|r| r.key == "deps").unwrap().path.clone();
        t.toggle(&deps);
        assert_eq!(t.len(), 1 + 6 + 2);
        assert!(!t.rows().iter().any(|r| r.key == "gpui"));
        t.toggle(&deps);
        assert!(t.rows().iter().any(|r| r.key == "gpui"));
    }

    #[test]
    fn leaves_do_not_expand() {
        let mut t = tree();
        let before = t.len();
        let n = t.rows().iter().find(|r| r.key == "n").unwrap().path.clone();
        t.toggle(&n);
        assert_eq!(t.len(), before);
        assert!(!t.rows().iter().find(|r| r.key == "n").unwrap().expandable);
    }

    #[test]
    fn collapse_all_keeps_the_root_open() {
        let mut t = tree();
        t.collapse_all();
        assert_eq!(t.len(), 7);
        t.expand_all();
        assert_eq!(t.len(), 11);
    }

    #[test]
    fn huge_arrays_load_lazily_in_batches() {
        let big = format!("[{}]", (0..2_500).map(|i| i.to_string()).collect::<Vec<_>>().join(","));
        let mut t = DataTree::parse(Format::Json, big.as_bytes()).unwrap();
        // Capped at MAX_CHILDREN plus the "more" row.
        assert_eq!(t.len(), 1 + MAX_CHILDREN + 1);
        let last = t.rows().last().unwrap().clone();
        assert_eq!(last.more, Some(2_500 - MAX_CHILDREN));
        t.load_more(&last.path);
        assert_eq!(t.len(), 1 + 2 * MAX_CHILDREN + 1);
        t.load_more(&[]);
        assert_eq!(t.len(), 1 + 2_500);
        assert!(t.rows().iter().all(|r| r.more.is_none()));
    }

    #[test]
    fn toml_and_yaml_parse() {
        let t = DataTree::parse(Format::Toml, b"[package]\nname = \"x\"\n").unwrap();
        assert!(t.rows().iter().any(|r| r.key == "package"));
        let y = DataTree::parse(Format::Yaml, b"a:\n  - 1\n  - 2\n").unwrap();
        assert!(y.rows().iter().any(|r| r.key == "a"));
    }

    #[test]
    fn invalid_input_is_an_error_not_a_panic() {
        assert!(DataTree::parse(Format::Json, b"{ nope").is_err());
    }
}
