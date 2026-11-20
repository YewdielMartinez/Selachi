#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use viewer_svc::{Document, Encoding, FileKind, Limits};

fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.path().join(name);
    fs::write(&p, bytes).unwrap();
    p
}

fn open_indexed(p: &Path) -> Document {
    let d = Document::open_lazy(p, Limits::default()).unwrap();
    d.wait_indexed();
    d
}

#[test]
fn lines_lf_crlf_and_mixed() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(&dir, "a.txt", b"one\ntwo\r\nthree\n\nfive");
    let d = open_indexed(&p);
    assert_eq!(d.line_count(), 5);
    let l: Vec<_> = d.lines(0..5).into_iter().map(|c| c.into_owned()).collect();
    assert_eq!(l, ["one", "two", "three", "", "five"]);
    assert_eq!(d.line(1).as_deref(), Some("two"));
    assert_eq!(d.line(5), None);
}

#[test]
fn trailing_newline_does_not_add_a_phantom_line() {
    let dir = tempfile::tempdir().unwrap();
    let d = open_indexed(&write(&dir, "a.txt", b"a\nb\n"));
    assert_eq!(d.line_count(), 2);
    let d = open_indexed(&write(&dir, "b.txt", b"a\r\nb\r\n"));
    assert_eq!(d.line_count(), 2);
    assert_eq!(d.line(1).as_deref(), Some("b"));
    let d = open_indexed(&write(&dir, "c.txt", b"\n"));
    assert_eq!(d.line_count(), 1);
    assert_eq!(d.line(0).as_deref(), Some(""));
}

#[test]
fn empty_file_has_no_lines() {
    let dir = tempfile::tempdir().unwrap();
    let d = open_indexed(&write(&dir, "e.txt", b""));
    assert_eq!(d.line_count(), 0);
    assert!(d.lines(0..10).is_empty());
    assert!(d.is_indexed());
}

#[test]
fn ranges_are_clamped() {
    let dir = tempfile::tempdir().unwrap();
    let d = open_indexed(&write(&dir, "a.txt", b"a\nb\nc\n"));
    assert_eq!(d.lines(1..100).len(), 2);
    assert!(d.lines(5..9).is_empty());
    assert!(d.lines(2..2).is_empty());
    #[allow(clippy::reversed_empty_ranges)]
    let reversed = d.lines(3..1);
    assert!(reversed.is_empty());
}

#[test]
fn invalid_utf8_is_lossy() {
    let dir = tempfile::tempdir().unwrap();
    // mostly valid text with a stray byte late in a long file
    let mut body = b"ok line\n".repeat(2000);
    body.extend_from_slice(b"bad \xff\xfe here\nlast\n");
    let d = open_indexed(&write(&dir, "a.txt", &body));
    let n = d.line_count();
    let line = d.line(n - 2).unwrap();
    assert!(line.contains('\u{FFFD}'), "{line:?}");
    assert_eq!(d.line(n - 1).as_deref(), Some("last"));
}

#[test]
fn latin1_decodes_one_byte_per_char() {
    let dir = tempfile::tempdir().unwrap();
    let d = open_indexed(&write(&dir, "a.txt", b"caf\xe9\nna\xefve\n"));
    assert_eq!(d.encoding(), Encoding::Latin1);
    assert_eq!(d.line(0).as_deref(), Some("café"));
    assert_eq!(d.line(1).as_deref(), Some("naïve"));
}

#[test]
fn utf8_bom_is_stripped_utf16_is_transcoded() {
    let dir = tempfile::tempdir().unwrap();
    let d = open_indexed(&write(&dir, "a.txt", b"\xEF\xBB\xBFhello\nworld\n"));
    assert_eq!(d.line(0).as_deref(), Some("hello"));

    let mut le = vec![0xFF, 0xFE];
    for u in "héllo\r\nwörld\r\n".encode_utf16() {
        le.extend_from_slice(&u.to_le_bytes());
    }
    let d = open_indexed(&write(&dir, "le.txt", &le));
    assert_eq!(d.encoding(), Encoding::Utf16Le);
    assert_eq!(d.line_count(), 2);
    assert_eq!(d.line(0).as_deref(), Some("héllo"));
    assert_eq!(d.line(1).as_deref(), Some("wörld"));

    let mut be = vec![0xFE, 0xFF];
    for u in "a\nb\n".encode_utf16() {
        be.extend_from_slice(&u.to_be_bytes());
    }
    let d = open_indexed(&write(&dir, "be.txt", &be));
    assert_eq!(d.line(1).as_deref(), Some("b"));
}

#[test]
fn binary_has_no_lines_but_has_hex() {
    let dir = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0u8..=255).collect();
    let d = Document::open(&write(&dir, "b.bin", &bytes), Limits::default()).unwrap();
    assert_eq!(d.kind(), FileKind::Binary);
    assert!(!d.is_textual());
    assert!(d.lines(0..10).is_empty());
    assert_eq!(d.line_count(), 0);

    let hex = d.hex_preview(0, 40);
    assert_eq!(hex.len(), 3); // 16 + 16 + 8
    assert_eq!(hex[0].offset, 0);
    assert_eq!(
        hex[0].hex(),
        "00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f"
    );
    assert_eq!(hex[0].ascii, "................");
    assert_eq!(hex[2].bytes.len(), 8);
    // offset is aligned down to a row
    let hex = d.hex_preview(0x41, 16);
    assert_eq!(hex[0].offset, 0x40);
    assert_eq!(hex[0].ascii, "@ABCDEFGHIJKLMNO");
    // past the end
    assert!(d.hex_preview(10_000, 16).is_empty());
}

#[test]
fn over_the_limit_only_hex_is_offered() {
    let dir = tempfile::tempdir().unwrap();
    let mut body = Vec::new();
    for i in 0..500 {
        writeln!(body, "line {i}").unwrap();
    }
    let p = write(&dir, "big.txt", &body);
    let limits = Limits {
        max_bytes: 1000,
        ..Limits::default()
    };
    let d = Document::open(&p, limits).unwrap();
    assert_eq!(d.kind(), FileKind::TooLarge);
    assert!(d.lines(0..10).is_empty());
    assert_eq!(d.line_count(), 0);
    let hex = d.hex_preview(0, 32);
    assert_eq!(hex.len(), 2);
    assert_eq!(hex[0].ascii, "line 0.line 1.li");
}

#[test]
fn open_errors_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Document::open(&dir.path().join("missing.txt"), Limits::default()).is_err());
    assert!(Document::open(dir.path(), Limits::default()).is_err());
}

#[test]
fn minimap_buckets_take_the_longest_line() {
    let dir = tempfile::tempdir().unwrap();
    // lengths: 1, 5, 2, 9, 0, 3
    let d = open_indexed(&write(&dir, "a.txt", b"a\nbbbbb\ncc\nddddddddd\n\neee\n"));
    assert_eq!(d.minimap(0..6, 6), vec![1, 5, 2, 9, 0, 3]);
    assert_eq!(d.minimap(0..6, 3), vec![5, 9, 3]);
    assert_eq!(d.minimap(0..6, 2), vec![5, 9]);
    assert_eq!(d.minimap(0..6, 100), vec![1, 5, 2, 9, 0, 3]);
    assert_eq!(d.minimap(2..4, 2), vec![2, 9]);
    assert!(d.minimap(0..6, 0).is_empty());
    assert!(d.minimap(9..12, 4).is_empty());
}

#[test]
fn minimap_ignores_crlf_and_handles_unterminated_last_line() {
    let dir = tempfile::tempdir().unwrap();
    let d = open_indexed(&write(&dir, "a.txt", b"abc\r\nde\r\nfghij"));
    assert_eq!(d.minimap(0..3, 3), vec![3, 2, 5]);
}

#[test]
fn minimap_saturates() {
    let dir = tempfile::tempdir().unwrap();
    let mut body = vec![b'x'; 70_000];
    body.push(b'\n');
    let d = open_indexed(&write(&dir, "a.txt", &body));
    assert_eq!(d.minimap(0..1, 1), vec![u16::MAX]);
}

/// Generates ~20 MB of text and checks that the first lines are readable before the
/// full index is built.
#[test]
fn big_file_first_lines_before_full_index() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.log");
    let mut f = std::io::BufWriter::new(fs::File::create(&p).unwrap());
    let mut n = 0usize;
    let mut total = 0usize;
    while total < 20 * 1024 * 1024 {
        let line =
            format!("{n:08} the quick brown fox jumps over the lazy dog 0123456789 abcdef\n");
        total += line.len();
        f.write_all(line.as_bytes()).unwrap();
        n += 1;
    }
    f.flush().unwrap();
    drop(f);

    let limits = Limits {
        index_chunk_bytes: 256 * 1024,
        ..Limits::default()
    };
    // No background thread: deterministic "before indexing completes".
    let d = Document::open_lazy(&p, limits).unwrap();
    assert!(!d.is_indexed());
    assert_eq!(d.index_progress().scanned_bytes, 0);

    let first = d.lines(0..100);
    assert_eq!(first.len(), 100);
    assert!(first[0].starts_with("00000000 the quick"));
    assert!(first[99].starts_with("00000099 the quick"));
    let p1 = d.index_progress();
    assert!(
        !p1.complete,
        "first lines must not need the whole file: {p1:?}"
    );
    assert!(p1.scanned_bytes < p1.total_bytes / 4);
    assert!(d.estimated_line_count() > 100_000);

    // Drive the index to the end in steps; progress is monotonic.
    let mut last = p1.scanned_bytes;
    let mut steps = 0;
    loop {
        let p = d.index_step(2 * 1024 * 1024);
        assert!(p.scanned_bytes >= last);
        last = p.scanned_bytes;
        steps += 1;
        if p.complete {
            assert_eq!(p.lines, n);
            assert!((p.fraction() - 1.0).abs() < f32::EPSILON);
            break;
        }
        assert!(steps < 100);
    }
    assert!(steps > 3, "index must be incremental");
    assert!(d.is_indexed());
    assert_eq!(d.line_count(), n);
    let last_line = d.line(n - 1).unwrap();
    assert!(last_line.starts_with(&format!("{:08}", n - 1)));
}

#[test]
fn background_indexer_completes() {
    let dir = tempfile::tempdir().unwrap();
    let body = "x\n".repeat(200_000);
    let d = Document::open(&write(&dir, "a.txt", body.as_bytes()), Limits::default()).unwrap();
    // lines are available immediately...
    assert_eq!(d.lines(0..3).len(), 3);
    // ...and the worker finishes on its own.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !d.is_indexed() {
        assert!(Instant::now() < deadline, "indexer did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(d.line_count(), 200_000);
}

#[test]
fn memory_mapped_path_matches_owned_path() {
    let dir = tempfile::tempdir().unwrap();
    let body = "alpha\nbeta\r\ngamma\n".repeat(1000);
    let p = write(&dir, "a.txt", body.as_bytes());
    let mapped = Document::open_lazy(
        &p,
        Limits {
            mmap_threshold: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    mapped.wait_indexed();
    let owned = open_indexed(&p);
    assert_eq!(mapped.line_count(), 3000);
    assert_eq!(mapped.lines(0..3000), owned.lines(0..3000));
}

#[test]
fn clone_shares_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let d = open_indexed(&write(&dir, "a.txt", b"a\nb\n"));
    let c = d.clone();
    assert_eq!(c.line_count(), 2);
    assert_eq!(c.path(), d.path());
}
