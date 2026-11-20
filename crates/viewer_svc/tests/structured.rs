#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use viewer_svc::{
    json_tree, toml_tree, yaml_tree, Error, Format, Node, NodeKind, Structured, MAX_CHILDREN,
};

fn fixture(name: &str) -> Vec<u8> {
    let p: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    fs::read(p).unwrap()
}

fn child<'a>(n: &'a Node, key: &str) -> &'a Node {
    n.children.iter().find(|c| c.key == key).unwrap_or_else(|| {
        panic!(
            "no child {key} in {:?}",
            n.children.iter().map(|c| &c.key).collect::<Vec<_>>()
        )
    })
}

#[test]
fn json_fixture() {
    let root = json_tree(&fixture("sample.json")).unwrap();
    assert_eq!(root.key, "$");
    assert_eq!(root.kind, NodeKind::Object);
    assert_eq!(root.value_preview, "{4 keys}");
    assert_eq!(root.child_count, 4);
    // insertion order is preserved
    let keys: Vec<_> = root.children.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["name", "version", "tags", "nested"]);

    let name = child(&root, "name");
    assert_eq!(name.kind, NodeKind::String);
    assert_eq!(name.value_preview, "\"selachi\"");
    assert!(name.children.is_empty());

    assert_eq!(child(&root, "version").kind, NodeKind::Number);
    assert_eq!(child(&root, "version").value_preview, "1");

    let tags = child(&root, "tags");
    assert_eq!(tags.kind, NodeKind::Array);
    assert_eq!(tags.value_preview, "[4 items]");
    assert_eq!(tags.children[0].key, "[0]");
    assert_eq!(tags.children[2].kind, NodeKind::Null);
    assert_eq!(tags.children[3].kind, NodeKind::Bool);

    let nested = child(&root, "nested");
    assert_eq!(child(nested, "pi").value_preview, "3.14");
    assert_eq!(child(nested, "empty").value_preview, "{0 keys}");
    assert!(child(nested, "empty").is_container());
}

#[test]
fn json_scalars_and_errors() {
    let n = json_tree(b"42").unwrap();
    assert_eq!(n.kind, NodeKind::Number);
    assert!(!n.is_container());
    let n = json_tree(b"[1]").unwrap();
    assert_eq!(n.value_preview, "[1 item]");

    let err = json_tree(b"{ \"a\": ").unwrap_err();
    assert!(
        matches!(err, Error::Parse { format: "JSON", .. }),
        "{err:?}"
    );
    assert!(json_tree(b"").is_err());
}

#[test]
fn jsonc_comments_and_trailing_commas() {
    let src = br#"{
        // line comment
        "a": 1, /* block */
        "url": "http://x//y",
        "list": [1, 2,],
    }"#;
    let n = json_tree(src).unwrap();
    assert_eq!(n.child_count, 3);
    assert_eq!(child(&n, "url").value_preview, "\"http://x//y\"");
    assert_eq!(child(&n, "list").child_count, 2);
}

#[test]
fn json_bom_is_tolerated() {
    let mut src = vec![0xEF, 0xBB, 0xBF];
    src.extend_from_slice(b"{\"a\":1}");
    assert_eq!(json_tree(&src).unwrap().child_count, 1);
}

#[test]
fn long_strings_are_previewed_on_one_line() {
    let long = "x".repeat(500);
    let src = format!("{{\"s\": \"{long}\\nsecond\"}}");
    let n = json_tree(src.as_bytes()).unwrap();
    let p = &child(&n, "s").value_preview;
    assert!(p.chars().count() < 140, "{}", p.len());
    assert!(p.ends_with("…\""));
    assert!(!p.contains('\n'));
    let n = json_tree(b"{\"s\": \"a\\nb\"}").unwrap();
    assert_eq!(child(&n, "s").value_preview, "\"a\\nb\"");
}

#[test]
fn huge_arrays_are_capped_and_loadable_lazily() {
    let items: Vec<String> = (0..5000).map(|i| i.to_string()).collect();
    let src = format!("{{\"big\": [{}], \"after\": true}}", items.join(","));
    let s = Structured::parse(Format::Json, src.as_bytes()).unwrap();
    let root = s.root();
    let big = child(&root, "big");
    assert_eq!(big.child_count, 5000);
    assert_eq!(big.children.len(), MAX_CHILDREN);
    assert_eq!(big.hidden(), 4000);
    assert_eq!(big.value_preview, "[5000 items]");
    assert_eq!(child(&root, "after").kind, NodeKind::Bool);

    // "show more": children 1000..1010 of root.big (child index 0)
    let more = s.children(&[0], 1000..1010);
    assert_eq!(more.len(), 10);
    assert_eq!(more[0].key, "[1000]");
    assert_eq!(more[9].value_preview, "1009");
    // clamped
    assert_eq!(s.children(&[0], 4995..9000).len(), 5);
    assert!(s.children(&[0], 6000..7000).is_empty());
    // not a container / bad path
    assert!(s.children(&[1], 0..5).is_empty());
    assert!(s.children(&[9, 9], 0..5).is_empty());
}

#[test]
fn deep_documents_unload_beyond_the_node_budget() {
    // 200 arrays of 200 numbers = 40k leaves + 200 containers: over the 20k budget.
    let row = format!("[{}]", vec!["1"; 200].join(","));
    let src = format!("[{}]", vec![row; 200].join(","));
    let s = Structured::parse(Format::Json, src.as_bytes()).unwrap();
    let root = s.root();
    assert_eq!(root.child_count, 200);
    // The budget ran out part-way: the tail is reported as hidden, not dropped.
    let loaded = root.children.len();
    assert!(loaded < 200, "expected a partial load, got {loaded}");
    assert_eq!(root.hidden(), 200 - loaded);
    // and can be fetched on demand
    let rest = s.children(&[], loaded..200);
    assert_eq!(rest.len(), 200 - loaded);
    assert_eq!(rest[0].child_count, 200);
    assert_eq!(s.children(&[loaded], 0..1000).len(), 200);
}

#[test]
fn toml_fixture() {
    let root = toml_tree(&fixture("sample.toml")).unwrap();
    let pkg = child(&root, "package");
    assert_eq!(pkg.kind, NodeKind::Object);
    assert_eq!(child(pkg, "name").value_preview, "\"selachi\"");
    assert_eq!(child(pkg, "published").kind, NodeKind::Bool);
    assert_eq!(child(pkg, "authors").value_preview, "[2 items]");
    let deps = child(&root, "dependencies");
    let serde = child(deps, "serde");
    assert_eq!(serde.kind, NodeKind::Object);
    assert_eq!(child(serde, "features").child_count, 1);
    // array of tables
    let bin = child(&root, "bin");
    assert_eq!(bin.kind, NodeKind::Array);
    let first = &bin.children[0];
    assert_eq!(child(first, "when").kind, NodeKind::DateTime);
    assert_eq!(child(first, "when").value_preview, "1979-05-27T07:32:00Z");
}

#[test]
fn toml_errors() {
    let err = toml_tree(b"a = ").unwrap_err();
    assert!(
        matches!(err, Error::Parse { format: "TOML", .. }),
        "{err:?}"
    );
    assert!(toml_tree(&[0xff, 0xfe, 0x00]).is_err());
}

#[test]
fn yaml_fixture() {
    let root = yaml_tree(&fixture("sample.yaml")).unwrap();
    assert_eq!(root.kind, NodeKind::Object);
    assert_eq!(child(&root, "name").value_preview, "\"selachi\"");
    assert_eq!(child(&root, "version").kind, NodeKind::Number);
    assert_eq!(child(&root, "flag").kind, NodeKind::Bool);
    assert_eq!(child(&root, "nothing").kind, NodeKind::Null);
    let items = child(&root, "items");
    assert_eq!(items.child_count, 2);
    assert_eq!(items.children[1].kind, NodeKind::Object);
    assert_eq!(child(&items.children[1], "c").value_preview, "[3 items]");
}

#[test]
fn yaml_multi_document_and_errors() {
    let n = yaml_tree(b"a: 1\n---\nb: 2\n").unwrap();
    assert_eq!(n.kind, NodeKind::Array);
    assert_eq!(n.child_count, 2);
    let n = yaml_tree(b"").unwrap();
    assert_eq!(n.kind, NodeKind::Null);
    let err = yaml_tree(b"a: [1, 2\nb: }").unwrap_err();
    assert!(
        matches!(err, Error::Parse { format: "YAML", .. }),
        "{err:?}"
    );
}
