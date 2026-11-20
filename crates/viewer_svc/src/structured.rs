//! JSON / TOML / YAML as one uniform, collapsible tree.

use std::ops::Range;

use crate::error::{Error, Result};

/// Children shown per container before the rest is reported as hidden.
pub const MAX_CHILDREN: usize = 1000;
/// Nodes materialised eagerly by [`Structured::root`]. Once spent, containers stop
/// loading children (`Node::hidden()` reports how many are missing) and the rest is
/// fetched with [`Structured::children`].
pub const NODE_BUDGET: usize = 20_000;
const PREVIEW_CHARS: usize = 120;

/// Source formats understood by [`Structured::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Toml,
    Yaml,
}

/// Shape of a node's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
    /// TOML date/time.
    DateTime,
}

/// One row of the tree view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Object key, `[i]` for array items, `$` for the root.
    pub key: String,
    pub kind: NodeKind,
    /// Short single-line rendering: `{3 keys}`, `[10 items]`, `"text..."`, `42`.
    pub value_preview: String,
    /// Loaded children (at most [`MAX_CHILDREN`]).
    pub children: Vec<Node>,
    /// Real number of children of this container.
    pub child_count: usize,
}

impl Node {
    /// Children not present in `children` (beyond the 1000 cap or not loaded yet).
    pub fn hidden(&self) -> usize {
        self.child_count - self.children.len()
    }

    pub fn is_container(&self) -> bool {
        matches!(self.kind, NodeKind::Object | NodeKind::Array)
    }
}

/// Parse `bytes` as JSON (comments and trailing commas tolerated, so `tsconfig.json`
/// opens) into a tree.
pub fn json_tree(bytes: &[u8]) -> Result<Node> {
    Ok(Structured::parse(Format::Json, bytes)?.root())
}

/// Parse `bytes` as TOML into a tree.
pub fn toml_tree(bytes: &[u8]) -> Result<Node> {
    Ok(Structured::parse(Format::Toml, bytes)?.root())
}

/// Parse `bytes` as YAML into a tree. A multi-document stream becomes an array of documents.
pub fn yaml_tree(bytes: &[u8]) -> Result<Node> {
    Ok(Structured::parse(Format::Yaml, bytes)?.root())
}

/// A parsed document that can hand out (lazy) nodes.
#[derive(Debug, Clone)]
pub struct Structured {
    root: Value,
}

#[derive(Debug, Clone)]
enum Value {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Date(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Structured {
    pub fn parse(format: Format, bytes: &[u8]) -> Result<Structured> {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        let root = match format {
            Format::Json => parse_json(bytes)?,
            Format::Toml => parse_toml(bytes)?,
            Format::Yaml => parse_yaml(bytes)?,
        };
        Ok(Structured { root })
    }

    /// The root node with children materialised up to [`NODE_BUDGET`] nodes.
    pub fn root(&self) -> Node {
        let mut budget = NODE_BUDGET;
        build("$".to_string(), &self.root, &mut budget)
    }

    /// Children `range` of the container reached by following child indices `path`
    /// from the root. Used for "show more" on huge arrays and for unloaded containers.
    /// Returns an empty list when `path` does not lead to a container.
    pub fn children(&self, path: &[usize], range: Range<usize>) -> Vec<Node> {
        let mut cur = &self.root;
        for &i in path {
            cur = match cur {
                Value::Arr(items) => match items.get(i) {
                    Some(v) => v,
                    None => return Vec::new(),
                },
                Value::Obj(fields) => match fields.get(i) {
                    Some((_, v)) => v,
                    None => return Vec::new(),
                },
                _ => return Vec::new(),
            };
        }
        let mut budget = NODE_BUDGET;
        match cur {
            Value::Arr(items) => slice_range(items.len(), &range)
                .map(|i| build(format!("[{i}]"), &items[i], &mut budget))
                .collect(),
            Value::Obj(fields) => slice_range(fields.len(), &range)
                .map(|i| build(fields[i].0.clone(), &fields[i].1, &mut budget))
                .collect(),
            _ => Vec::new(),
        }
    }
}

fn slice_range(len: usize, range: &Range<usize>) -> Range<usize> {
    let start = range.start.min(len);
    start..range.end.min(len).max(start)
}

fn build(key: String, v: &Value, budget: &mut usize) -> Node {
    *budget = budget.saturating_sub(1);
    match v {
        Value::Null => leaf(key, NodeKind::Null, "null".into()),
        Value::Bool(b) => leaf(key, NodeKind::Bool, b.to_string()),
        Value::Num(n) => leaf(key, NodeKind::Number, n.clone()),
        Value::Date(d) => leaf(key, NodeKind::DateTime, d.clone()),
        Value::Str(s) => leaf(key, NodeKind::String, preview_string(s)),
        Value::Arr(items) => {
            let n = items.len();
            let children = if *budget == 0 {
                Vec::new()
            } else {
                let mut out = Vec::with_capacity(n.min(MAX_CHILDREN));
                for (i, item) in items.iter().take(MAX_CHILDREN).enumerate() {
                    if *budget == 0 {
                        break;
                    }
                    out.push(build(format!("[{i}]"), item, budget));
                }
                out
            };
            Node {
                key,
                kind: NodeKind::Array,
                value_preview: format!("[{n} item{}]", if n == 1 { "" } else { "s" }),
                children,
                child_count: n,
            }
        }
        Value::Obj(fields) => {
            let n = fields.len();
            let children = if *budget == 0 {
                Vec::new()
            } else {
                let mut out = Vec::with_capacity(n.min(MAX_CHILDREN));
                for (k, item) in fields.iter().take(MAX_CHILDREN) {
                    if *budget == 0 {
                        break;
                    }
                    out.push(build(k.clone(), item, budget));
                }
                out
            };
            Node {
                key,
                kind: NodeKind::Object,
                value_preview: format!("{{{n} key{}}}", if n == 1 { "" } else { "s" }),
                children,
                child_count: n,
            }
        }
    }
}

fn leaf(key: String, kind: NodeKind, value_preview: String) -> Node {
    Node {
        key,
        kind,
        value_preview,
        children: Vec::new(),
        child_count: 0,
    }
}

fn preview_string(s: &str) -> String {
    let mut out = String::from("\"");
    for (n, c) in s.chars().enumerate() {
        if n >= PREVIEW_CHARS {
            out.push('…');
            break;
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---- JSON --------------------------------------------------------------------------

fn parse_json(bytes: &[u8]) -> Result<Value> {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => Ok(from_json(v)),
        Err(first) => {
            // JSONC / JSON5-lite: comments and trailing commas.
            let cleaned = strip_jsonc(bytes);
            match serde_json::from_slice::<serde_json::Value>(&cleaned) {
                Ok(v) => Ok(from_json(v)),
                Err(_) => Err(Error::Parse {
                    format: "JSON",
                    message: first.to_string(),
                }),
            }
        }
    }
}

fn from_json(v: serde_json::Value) -> Value {
    use serde_json::Value as J;
    match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(b),
        J::Number(n) => Value::Num(n.to_string()),
        J::String(s) => Value::Str(s),
        J::Array(a) => Value::Arr(a.into_iter().map(from_json).collect()),
        J::Object(o) => Value::Obj(o.into_iter().map(|(k, v)| (k, from_json(v))).collect()),
    }
}

/// Remove `//` and `/* */` comments and trailing commas, leaving strings untouched.
pub(crate) fn strip_jsonc(src: &[u8]) -> Vec<u8> {
    // Pass 1: comments.
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < src.len() {
        let c = src[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < src.len() {
                out.push(src[i + 1]);
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match (c, src.get(i + 1)) {
            (b'"', _) => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            (b'/', Some(b'/')) => {
                while i < src.len() && src[i] != b'\n' {
                    i += 1;
                }
            }
            (b'/', Some(b'*')) => {
                i += 2;
                while i + 1 < src.len() && !(src[i] == b'*' && src[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                out.push(b' ');
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Pass 2: trailing commas.
    let mut res = Vec::with_capacity(out.len());
    let mut in_str = false;
    let mut i = 0;
    while i < out.len() {
        let c = out[i];
        if in_str {
            res.push(c);
            if c == b'\\' && i + 1 < out.len() {
                res.push(out[i + 1]);
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
            res.push(c);
        } else if c == b',' {
            let next = out[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
            if !matches!(next, Some(b'}') | Some(b']')) {
                res.push(c);
            }
        } else {
            res.push(c);
        }
        i += 1;
    }
    res
}

// ---- TOML --------------------------------------------------------------------------

fn parse_toml(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
        format: "TOML",
        message: e.to_string(),
    })?;
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| Error::Parse {
        format: "TOML",
        message: e.to_string(),
    })?;
    Ok(from_toml(toml::Value::Table(table)))
}

fn from_toml(v: toml::Value) -> Value {
    use toml::Value as T;
    match v {
        T::String(s) => Value::Str(s),
        T::Integer(i) => Value::Num(i.to_string()),
        T::Float(f) => Value::Num(f.to_string()),
        T::Boolean(b) => Value::Bool(b),
        T::Datetime(d) => Value::Date(d.to_string()),
        T::Array(a) => Value::Arr(a.into_iter().map(from_toml).collect()),
        T::Table(t) => Value::Obj(t.into_iter().map(|(k, v)| (k, from_toml(v))).collect()),
    }
}

// ---- YAML --------------------------------------------------------------------------

fn parse_yaml(bytes: &[u8]) -> Result<Value> {
    let text = String::from_utf8_lossy(bytes);
    let docs = yaml_rust2::YamlLoader::load_from_str(&text).map_err(|e| Error::Parse {
        format: "YAML",
        message: e.to_string(),
    })?;
    Ok(match docs.len() {
        0 => Value::Null,
        1 => from_yaml(&docs[0]),
        _ => Value::Arr(docs.iter().map(from_yaml).collect()),
    })
}

fn from_yaml(y: &yaml_rust2::Yaml) -> Value {
    use yaml_rust2::Yaml as Y;
    match y {
        Y::Real(s) => Value::Num(s.clone()),
        Y::Integer(i) => Value::Num(i.to_string()),
        Y::String(s) => Value::Str(s.clone()),
        Y::Boolean(b) => Value::Bool(*b),
        Y::Array(a) => Value::Arr(a.iter().map(from_yaml).collect()),
        Y::Hash(h) => Value::Obj(h.iter().map(|(k, v)| (yaml_key(k), from_yaml(v))).collect()),
        Y::Null | Y::BadValue | Y::Alias(_) => Value::Null,
    }
}

fn yaml_key(k: &yaml_rust2::Yaml) -> String {
    use yaml_rust2::Yaml as Y;
    match k {
        Y::Real(s) | Y::String(s) => s.clone(),
        Y::Integer(i) => i.to_string(),
        Y::Boolean(b) => b.to_string(),
        Y::Null => "null".to_string(),
        Y::Array(_) | Y::Hash(_) => "{…}".to_string(),
        Y::BadValue | Y::Alias(_) => "?".to_string(),
    }
}
