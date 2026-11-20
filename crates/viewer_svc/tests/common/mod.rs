#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Helpers shared by the grammar plugin tests.

use std::fs;
use std::path::{Path, PathBuf};

use viewer_svc::grammars::{sha256_hex, Manifest};

/// Fresh scratch folder under cargo's target tmp dir (loaded DLLs lock their files, so
/// the folder is wiped at the *start* of the next run instead of on drop).
pub fn scratch(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("grammars")
        .join(name);
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// `packaging/grammars/dist/plugins` (output of build.ps1).
pub fn dist_plugins() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packaging")
        .join("grammars")
        .join("dist")
}

/// Copy the built plugin folders `ids` from `dist/plugins` into `dst`.
pub fn copy_dist(dst: &Path, ids: &[&str]) {
    for id in ids {
        let src = dist_plugins().join("plugins").join(id);
        assert!(
            src.join("manifest.toml").exists(),
            "{} missing: run packaging/grammars/build.ps1 first",
            src.display()
        );
        let to = dst.join(id);
        fs::create_dir_all(&to).unwrap();
        for f in fs::read_dir(&src).unwrap().flatten() {
            fs::copy(f.path(), to.join(f.file_name())).unwrap();
        }
    }
}

/// Everything a fake plugin folder needs.
pub struct Fake<'a> {
    pub id: &'a str,
    pub extensions: &'a [&'a str],
    pub abi: u32,
    pub symbol: &'a str,
    pub dll: &'a [u8],
    pub highlights: &'a str,
}

impl<'a> Fake<'a> {
    pub fn new(id: &'a str) -> Fake<'a> {
        Fake {
            id,
            extensions: &[],
            abi: 14,
            symbol: "tree_sitter_fake",
            dll: b"this is not a dll",
            highlights: "(comment) @comment",
        }
    }

    /// Write `<root>/<id>/` with correct hashes for the given bytes.
    pub fn write(&self, root: &Path) -> PathBuf {
        let dir = root.join(self.id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("grammar.dll"), self.dll).unwrap();
        fs::write(dir.join("highlights.scm"), self.highlights).unwrap();
        let m = Manifest {
            name: self.id.to_owned(),
            version: "1.0.0".into(),
            display_name: self.id.to_uppercase(),
            file_extensions: self.extensions.iter().map(|s| (*s).to_owned()).collect(),
            names: vec![],
            tree_sitter_abi: self.abi,
            symbol: self.symbol.to_owned(),
            files: [
                ("grammar.dll".to_owned(), sha256_hex(self.dll)),
                (
                    "highlights.scm".to_owned(),
                    sha256_hex(self.highlights.as_bytes()),
                ),
            ]
            .into_iter()
            .collect(),
            size_bytes: self.dll.len() as u64,
        };
        fs::write(dir.join("manifest.toml"), m.to_toml()).unwrap();
        dir
    }
}

/// Zip the files of a plugin folder (flat, stored).
pub fn zip_dir(dir: &Path) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for f in fs::read_dir(dir).unwrap().flatten() {
            zw.start_file(f.file_name().to_string_lossy(), opts)
                .unwrap();
            zw.write_all(&fs::read(f.path()).unwrap()).unwrap();
        }
        zw.finish().unwrap();
    }
    buf.into_inner()
}
