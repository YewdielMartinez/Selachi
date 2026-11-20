#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use viewer_svc::{detect, detect_with, Encoding, FileKind, ImageFormat, Lang, Limits};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.path().join(name);
    fs::write(&p, bytes).unwrap();
    p
}

fn text(lang: Option<Lang>) -> FileKind {
    FileKind::Text {
        lang,
        encoding: Encoding::Utf8,
    }
}

#[test]
fn source_files_by_extension() {
    assert_eq!(detect(&fixture("sample.rs")), text(Some(Lang::Rust)));
    assert_eq!(detect(&fixture("sample.ts")), text(Some(Lang::TypeScript)));
    assert_eq!(detect(&fixture("sample.js")), text(Some(Lang::JavaScript)));
    assert_eq!(detect(&fixture("sample.py")), text(Some(Lang::Python)));
    assert_eq!(detect(&fixture("sample.css")), text(Some(Lang::Css)));
    assert_eq!(detect(&fixture("sample.html")), text(Some(Lang::Html)));
    assert_eq!(detect(&fixture("sample.sh")), text(Some(Lang::Bash)));
    assert_eq!(detect(&fixture("sample.cs")), text(Some(Lang::CSharp)));
}

#[test]
fn structured_and_markdown_kinds() {
    assert_eq!(detect(&fixture("sample.json")), FileKind::Json);
    assert_eq!(detect(&fixture("sample.toml")), FileKind::Toml);
    assert_eq!(detect(&fixture("sample.yaml")), FileKind::Yaml);
    assert_eq!(detect(&fixture("sample.md")), FileKind::Markdown);
}

#[test]
fn extension_is_case_insensitive_and_unknown_is_plain_text() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(&dir, "MAIN.RS", b"fn main() {}\n");
    assert_eq!(detect(&p), text(Some(Lang::Rust)));
    let p = write(&dir, "notes.xyz", b"just words\n");
    assert_eq!(detect(&p), text(None));
    let p = write(&dir, "noext", b"just words\n");
    assert_eq!(detect(&p), text(None));
}

#[test]
fn shebang_selects_language_without_extension() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(&dir, "run", b"#!/usr/bin/env python3\nprint(1)\n");
    assert_eq!(detect(&p), text(Some(Lang::Python)));
    let p = write(&dir, "go", b"#!/bin/bash\necho hi\n");
    assert_eq!(detect(&p), text(Some(Lang::Bash)));
    let p = write(&dir, "tool", b"#!/usr/bin/env -S node --flag\n1\n");
    assert_eq!(detect(&p), text(Some(Lang::JavaScript)));
}

#[test]
fn magic_numbers_beat_extension() {
    let dir = tempfile::tempdir().unwrap();
    let png = [&b"\x89PNG\r\n\x1a\n"[..], &[0u8; 32]].concat();
    let p = write(&dir, "fake.txt", &png);
    assert_eq!(
        detect(&p),
        FileKind::Image {
            format: ImageFormat::Png
        }
    );

    let jpg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F', 0];
    let p = write(&dir, "a.dat", &jpg);
    assert_eq!(
        detect(&p),
        FileKind::Image {
            format: ImageFormat::Jpeg
        }
    );

    let p = write(&dir, "a.gif", b"GIF89a\x01\x00\x01\x00\x00\x00\x00;");
    assert_eq!(
        detect(&p),
        FileKind::Image {
            format: ImageFormat::Gif
        }
    );

    let mut webp = b"RIFF".to_vec();
    webp.extend_from_slice(&[0x24, 0, 0, 0]);
    webp.extend_from_slice(b"WEBPVP8 ");
    webp.extend_from_slice(&[0; 16]);
    let p = write(&dir, "a.webp", &webp);
    assert_eq!(
        detect(&p),
        FileKind::Image {
            format: ImageFormat::WebP
        }
    );
}

#[test]
fn svg_is_its_own_kind() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(
        &dir,
        "logo.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>",
    );
    assert_eq!(detect(&p), FileKind::Svg);
}

#[test]
fn nul_bytes_mean_binary() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(&dir, "data.rs", b"fn main() {\0}\n");
    assert_eq!(detect(&p), FileKind::Binary);
    let p = write(&dir, "tool.exe", b"MZ\x90\x00\x03\x00\x00\x00");
    assert_eq!(detect(&p), FileKind::Binary);
}

#[test]
fn boms_and_utf16() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(&dir, "a.txt", b"\xEF\xBB\xBFhello\n");
    assert_eq!(
        detect(&p),
        FileKind::Text {
            lang: None,
            encoding: Encoding::Utf8Bom
        }
    );

    let mut le = vec![0xFF, 0xFE];
    for u in "hi\n".encode_utf16() {
        le.extend_from_slice(&u.to_le_bytes());
    }
    let p = write(&dir, "le.txt", &le);
    assert_eq!(
        detect(&p),
        FileKind::Text {
            lang: None,
            encoding: Encoding::Utf16Le
        }
    );

    let mut be = vec![0xFE, 0xFF];
    for u in "hi\n".encode_utf16() {
        be.extend_from_slice(&u.to_be_bytes());
    }
    let p = write(&dir, "be.rs", &be);
    assert_eq!(
        detect(&p),
        FileKind::Text {
            lang: Some(Lang::Rust),
            encoding: Encoding::Utf16Be
        }
    );

    // UTF-32 LE BOM is not text we can show.
    let p = write(&dir, "u32.txt", &[0xFF, 0xFE, 0, 0, b'a', 0, 0, 0]);
    assert_eq!(detect(&p), FileKind::Binary);
}

#[test]
fn latin1_text_is_text_but_control_garbage_is_binary() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(&dir, "caf.txt", b"caf\xe9 au lait\n");
    assert_eq!(
        detect(&p),
        FileKind::Text {
            lang: None,
            encoding: Encoding::Latin1
        }
    );
    let junk: Vec<u8> = (1u8..32).cycle().take(512).collect();
    let p = write(&dir, "junk.txt", &junk);
    assert_eq!(detect(&p), FileKind::Binary);
}

#[test]
fn multibyte_char_cut_by_sniff_window_is_still_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let mut body = vec![b'a'; 8191];
    body.extend_from_slice("é".as_bytes()); // straddles byte 8192
    body.extend_from_slice(b" more text\n");
    let p = write(&dir, "wide.txt", &body);
    assert_eq!(detect(&p), text(None));
}

#[test]
fn too_large_text_but_binaries_keep_their_kind() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits {
        max_bytes: 1024,
        ..Limits::default()
    };
    let p = write(&dir, "big.rs", &vec![b'a'; 4096]);
    assert_eq!(detect_with(&p, &limits), FileKind::TooLarge);
    assert_eq!(detect(&p), text(Some(Lang::Rust)));

    let mut bin = vec![0u8; 4096];
    bin[0] = 1;
    let p = write(&dir, "big.bin", &bin);
    assert_eq!(detect_with(&p, &limits), FileKind::Binary);
}

#[test]
fn missing_file_and_directory_are_binary() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(detect(&dir.path().join("nope.rs")), FileKind::Binary);
    assert_eq!(detect(dir.path()), FileKind::Binary);
}

#[test]
fn empty_file_is_text() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(&dir, "empty.rs", b"");
    assert_eq!(detect(&p), text(Some(Lang::Rust)));
}

#[test]
fn lang_name_lookup() {
    assert_eq!(Lang::from_name("ts"), Some(Lang::TypeScript));
    assert_eq!(Lang::from_name("C#"), Some(Lang::CSharp));
    assert_eq!(Lang::from_name("nope"), None);
    assert_eq!(Lang::from_extension("YML"), Some(Lang::Yaml));
}
