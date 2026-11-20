#![allow(clippy::unwrap_used, clippy::expect_used)]

use viewer_svc::{markdown_blocks, plain_text, MdAlign, MdBlock, MdInline};

fn t(s: &str) -> MdInline {
    MdInline::Text(s.to_string())
}

#[test]
fn headings_levels_and_inline_styles() {
    let b = markdown_blocks("# One\n\n### Three *x*\n");
    assert_eq!(
        b,
        vec![
            MdBlock::Heading {
                level: 1,
                inlines: vec![t("One")]
            },
            MdBlock::Heading {
                level: 3,
                inlines: vec![t("Three "), MdInline::Emph(vec![t("x")])]
            },
        ]
    );
}

#[test]
fn paragraph_inlines() {
    let b = markdown_blocks("a *em* **st** `code` [l](http://u) ~~gone~~ end");
    assert_eq!(
        b,
        vec![MdBlock::Paragraph(vec![
            t("a "),
            MdInline::Emph(vec![t("em")]),
            t(" "),
            MdInline::Strong(vec![t("st")]),
            t(" "),
            MdInline::Code("code".into()),
            t(" "),
            MdInline::Link {
                url: "http://u".into(),
                children: vec![t("l")]
            },
            t(" "),
            MdInline::Strike(vec![t("gone")]),
            t(" end"),
        ])]
    );
}

#[test]
fn nested_emphasis_and_link_with_formatting() {
    let b = markdown_blocks("**bold *and em***\n\n[**x** y](z)");
    assert_eq!(
        b[0],
        MdBlock::Paragraph(vec![MdInline::Strong(vec![
            t("bold "),
            MdInline::Emph(vec![t("and em")])
        ])])
    );
    match &b[1] {
        MdBlock::Paragraph(i) => match &i[0] {
            MdInline::Link { url, children } => {
                assert_eq!(url, "z");
                assert_eq!(plain_text(children), "x y");
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn soft_and_hard_breaks() {
    let b = markdown_blocks("a\nb  \nc");
    assert_eq!(
        b,
        vec![MdBlock::Paragraph(vec![
            t("a"),
            MdInline::SoftBreak,
            t("b"),
            MdInline::HardBreak,
            t("c")
        ])]
    );
}

#[test]
fn code_blocks_fenced_indented() {
    let b =
        markdown_blocks("```rust ignore\nfn main() {}\n```\n\n```\nplain\n```\n\n    indented\n");
    assert_eq!(
        b,
        vec![
            MdBlock::CodeBlock {
                lang: Some("rust".into()),
                text: "fn main() {}".into()
            },
            MdBlock::CodeBlock {
                lang: None,
                text: "plain".into()
            },
            MdBlock::CodeBlock {
                lang: None,
                text: "indented".into()
            },
        ]
    );
}

#[test]
fn lists_bullet_ordered_nested_and_tasks() {
    let b = markdown_blocks("- one\n- two\n  - nested\n\n3. x\n4. y\n\n- [x] done\n- [ ] todo\n");
    let MdBlock::List {
        ordered,
        start,
        items,
    } = &b[0]
    else {
        panic!("{b:?}")
    };
    assert!(!ordered);
    assert_eq!(*start, 1);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].blocks, vec![MdBlock::Paragraph(vec![t("one")])]);
    assert_eq!(items[1].blocks.len(), 2);
    assert!(matches!(
        &items[1].blocks[1],
        MdBlock::List { ordered: false, items, .. } if items.len() == 1
    ));

    let MdBlock::List {
        ordered,
        start,
        items,
    } = &b[1]
    else {
        panic!("{b:?}")
    };
    assert!(*ordered);
    assert_eq!(*start, 3);
    assert_eq!(items.len(), 2);

    let MdBlock::List { items, .. } = &b[2] else {
        panic!("{b:?}")
    };
    assert_eq!(items[0].checked, Some(true));
    assert_eq!(items[1].checked, Some(false));
    assert_eq!(items[0].blocks, vec![MdBlock::Paragraph(vec![t("done")])]);
}

#[test]
fn loose_list_items_keep_paragraphs() {
    let b = markdown_blocks("- a\n\n- b\n");
    let MdBlock::List { items, .. } = &b[0] else {
        panic!("{b:?}")
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].blocks, vec![MdBlock::Paragraph(vec![t("b")])]);
}

#[test]
fn quote_and_rule() {
    let b = markdown_blocks("> quoted\n> more\n\n---\n\nafter");
    assert_eq!(
        b,
        vec![
            MdBlock::Quote(vec![MdBlock::Paragraph(vec![
                t("quoted"),
                MdInline::SoftBreak,
                t("more")
            ])]),
            MdBlock::Rule,
            MdBlock::Paragraph(vec![t("after")]),
        ]
    );
}

#[test]
fn nested_quote_with_list() {
    let b = markdown_blocks("> - a\n> - b\n");
    let MdBlock::Quote(inner) = &b[0] else {
        panic!("{b:?}")
    };
    assert!(matches!(&inner[0], MdBlock::List { items, .. } if items.len() == 2));
}

#[test]
fn tables() {
    let b = markdown_blocks("| a | b |\n|---|:-:|\n| 1 | *2* |\n| 3 | 4 |\n");
    let MdBlock::Table(tbl) = &b[0] else {
        panic!("{b:?}")
    };
    assert_eq!(tbl.aligns, vec![MdAlign::None, MdAlign::Center]);
    assert_eq!(tbl.header, vec![vec![t("a")], vec![t("b")]]);
    assert_eq!(tbl.rows.len(), 2);
    assert_eq!(tbl.rows[0][1], vec![MdInline::Emph(vec![t("2")])]);
    assert_eq!(tbl.rows[1][0], vec![t("3")]);
}

#[test]
fn image_blocks_and_inline_images() {
    let b = markdown_blocks("![alt text](img/pic.png)\n\ntext ![i](a.png) more");
    assert_eq!(
        b[0],
        MdBlock::Image {
            src: "img/pic.png".into(),
            alt: "alt text".into()
        }
    );
    match &b[1] {
        MdBlock::Paragraph(i) => assert!(i.contains(&MdInline::Image {
            src: "a.png".into(),
            alt: "i".into()
        })),
        other => panic!("{other:?}"),
    }
}

#[test]
fn html_blocks_are_kept_verbatim() {
    let b = markdown_blocks("<div>\nhello\n</div>\n\nafter");
    assert!(matches!(&b[0], MdBlock::Html(h) if h.contains("<div>")));
    assert_eq!(b[1], MdBlock::Paragraph(vec![t("after")]));
}

#[test]
fn empty_and_whitespace_input() {
    assert!(markdown_blocks("").is_empty());
    assert!(markdown_blocks("\n\n  \n").is_empty());
}

#[test]
fn fixture_has_every_block_type() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.md"),
    )
    .unwrap();
    let b = markdown_blocks(&src);
    let has = |f: &dyn Fn(&MdBlock) -> bool| b.iter().any(f);
    assert!(has(&|x| matches!(x, MdBlock::Heading { level: 1, .. })));
    assert!(has(&|x| matches!(x, MdBlock::Paragraph(_))));
    assert!(has(&|x| matches!(x, MdBlock::List { ordered: false, .. })));
    assert!(has(&|x| matches!(x, MdBlock::List { ordered: true, .. })));
    assert!(has(&|x| matches!(x, MdBlock::Quote(_))));
    assert!(has(
        &|x| matches!(x, MdBlock::CodeBlock { lang: Some(l), .. } if l == "rust")
    ));
    assert!(has(&|x| matches!(x, MdBlock::Rule)));
    assert!(has(&|x| matches!(x, MdBlock::Table(_))));
    assert!(has(&|x| matches!(x, MdBlock::Image { .. })));
}
