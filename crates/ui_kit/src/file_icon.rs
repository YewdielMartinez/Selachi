//! File-type icons (`assets/icons/file-*.svg`, 16x16): [`FileIcon::for_path`] picks one by name.

use std::path::Path;

use gpui::{Hsla, Svg, prelude::*, px, svg};

/// One of the file-type icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileIcon {
    Generic,
    Code,
    Ts,
    Js,
    Json,
    Html,
    Css,
    Markdown,
    Config,
    Image,
    Lock,
    Go,
    Rust,
    Python,
    Shell,
}

impl FileIcon {
    /// Every icon, for galleries, tests and the asset list.
    pub const ALL: [FileIcon; 15] = [
        FileIcon::Generic,
        FileIcon::Code,
        FileIcon::Ts,
        FileIcon::Js,
        FileIcon::Json,
        FileIcon::Html,
        FileIcon::Css,
        FileIcon::Markdown,
        FileIcon::Config,
        FileIcon::Image,
        FileIcon::Lock,
        FileIcon::Go,
        FileIcon::Rust,
        FileIcon::Python,
        FileIcon::Shell,
    ];

    /// File stem of the svg (`"file-rust"`).
    pub const fn name(self) -> &'static str {
        match self {
            FileIcon::Generic => "file-generic",
            FileIcon::Code => "file-code",
            FileIcon::Ts => "file-ts",
            FileIcon::Js => "file-js",
            FileIcon::Json => "file-json",
            FileIcon::Html => "file-html",
            FileIcon::Css => "file-css",
            FileIcon::Markdown => "file-markdown",
            FileIcon::Config => "file-config",
            FileIcon::Image => "file-image",
            FileIcon::Lock => "file-lock",
            FileIcon::Go => "file-go",
            FileIcon::Rust => "file-rust",
            FileIcon::Python => "file-python",
            FileIcon::Shell => "file-shell",
        }
    }

    /// Asset path served by [`crate::Assets`].
    pub fn path(self) -> String {
        format!("icons/{}.svg", self.name())
    }

    /// The icon of a file path (only its name matters; nothing is read from disk).
    pub fn for_path(path: &Path) -> FileIcon {
        path.file_name().map_or(FileIcon::Generic, |n| Self::for_name(&n.to_string_lossy()))
    }

    /// The icon of a file name.
    pub fn for_name(name: &str) -> FileIcon {
        let lower = name.to_ascii_lowercase();
        if matches!(lower.as_str(), "cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "bun.lock" | "bun.lockb")
            || lower.ends_with(".lock")
        {
            return FileIcon::Lock;
        }
        if matches!(lower.as_str(), ".env" | ".editorconfig") || lower.starts_with(".env.") {
            return FileIcon::Config;
        }
        let ext = match lower.rsplit_once('.') {
            Some((_, ext)) => ext,
            None => return FileIcon::Generic,
        };
        match ext {
            "rs" => FileIcon::Rust,
            "ts" | "tsx" | "mts" | "cts" => FileIcon::Ts,
            "js" | "jsx" | "mjs" | "cjs" => FileIcon::Js,
            "json" | "jsonc" => FileIcon::Json,
            "html" | "htm" | "svg" | "xml" => FileIcon::Html,
            "css" | "scss" | "less" => FileIcon::Css,
            "md" | "mdx" => FileIcon::Markdown,
            "toml" | "yaml" | "yml" | "ini" | "env" | "editorconfig" | "config" | "csproj" | "props" => FileIcon::Config,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" => FileIcon::Image,
            "go" => FileIcon::Go,
            "py" => FileIcon::Python,
            "ps1" | "psm1" | "sh" | "bash" | "cmd" | "bat" => FileIcon::Shell,
            "cs" | "c" | "cpp" | "h" | "java" | "kt" | "swift" | "rb" | "php" | "lua" | "sql" | "vue" | "svelte" => {
                FileIcon::Code
            },
            _ => FileIcon::Generic,
        }
    }
}

/// A file-type icon drawn at `size` logical px in `color`.
pub fn file_icon(icon: FileIcon, size: f32, color: impl Into<Hsla>) -> Svg {
    svg().path(icon.path()).flex_none().size(px(size)).text_color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping() {
        let f = |n: &str| FileIcon::for_path(Path::new(n));
        assert_eq!(f("src/main.rs"), FileIcon::Rust);
        assert_eq!(f("App.TSX"), FileIcon::Ts);
        assert_eq!(f("a.mjs"), FileIcon::Js);
        assert_eq!(f("tsconfig.jsonc"), FileIcon::Json);
        assert_eq!(f("logo.svg"), FileIcon::Html);
        assert_eq!(f("a.scss"), FileIcon::Css);
        assert_eq!(f("README.md"), FileIcon::Markdown);
        assert_eq!(f("Cargo.toml"), FileIcon::Config);
        assert_eq!(f("app.csproj"), FileIcon::Config);
        assert_eq!(f(".env"), FileIcon::Config);
        assert_eq!(f("icon.PNG"), FileIcon::Image);
        assert_eq!(f("Cargo.lock"), FileIcon::Lock);
        assert_eq!(f("poetry.lock"), FileIcon::Lock);
        assert_eq!(f("bun.lockb"), FileIcon::Lock);
        assert_eq!(f("main.go"), FileIcon::Go);
        assert_eq!(f("a.py"), FileIcon::Python);
        assert_eq!(f("build.ps1"), FileIcon::Shell);
        assert_eq!(f("Program.cs"), FileIcon::Code);
        assert_eq!(f("notes.txt"), FileIcon::Generic);
        assert_eq!(f("selachi.exe"), FileIcon::Generic);
        assert_eq!(f("Makefile"), FileIcon::Generic);
    }

    #[test]
    fn names_are_unique() {
        let mut n: Vec<_> = FileIcon::ALL.iter().map(|i| i.name()).collect();
        n.sort_unstable();
        n.dedup();
        assert_eq!(n.len(), FileIcon::ALL.len());
    }
}
