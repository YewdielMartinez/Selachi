//! File-type glyphs for the tree.
//!
//! The user designs the real per-type pixel icons (PLAN section 13: 16x16, the logo's bars and four
//! greys). Until they exist every file uses the generic `Icon::Files` tinted by its category, and
//! every directory `Icon::Folder`. `FileGlyph::svg_name` is the name each category's SVG must have
//! when it lands (`assets/icons/file-<name>.svg`); the list is `FileGlyph::ALL`.
//!
//! TODO-SVG: file-rust, file-script, file-python, file-config, file-markdown, file-image,
//! file-shell, file-web, file-lock, file-binary, file-text, file-generic, folder-open.

use theme::{Mode, Rgba, Theme, rgb};
use ui_kit::Icon;

/// Category of a file for its tree glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileGlyph {
    /// `.rs`
    Rust,
    /// `.ts .tsx .js .jsx .mjs .cjs`
    Script,
    /// `.py`
    Python,
    /// `.json .toml .yaml .yml .ini .cfg .env`
    Config,
    /// `.md .mdx .markdown`
    Markdown,
    /// images
    Image,
    /// `.sh .bash .ps1 .bat .cmd`
    Shell,
    /// `.html .css .scss .svg`
    Web,
    /// `Cargo.lock`, `package-lock.json`, `*.lock`
    Lock,
    /// `.exe .dll .zip .bin ...`
    Binary,
    /// `.txt .log .csv`
    Text,
    /// everything else
    Generic,
}

impl FileGlyph {
    /// Every category, for the report of needed SVGs and for tests.
    pub const ALL: [FileGlyph; 12] = [
        FileGlyph::Rust,
        FileGlyph::Script,
        FileGlyph::Python,
        FileGlyph::Config,
        FileGlyph::Markdown,
        FileGlyph::Image,
        FileGlyph::Shell,
        FileGlyph::Web,
        FileGlyph::Lock,
        FileGlyph::Binary,
        FileGlyph::Text,
        FileGlyph::Generic,
    ];

    /// Category of a file name.
    pub fn of(name: &str) -> FileGlyph {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".lock") || lower == "package-lock.json" || lower == "pnpm-lock.yaml" {
            return FileGlyph::Lock;
        }
        if matches!(lower.as_str(), ".env" | ".gitignore" | ".gitattributes" | ".editorconfig" | "dockerfile") {
            return FileGlyph::Config;
        }
        let ext = match lower.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => ext,
            _ => return FileGlyph::Generic,
        };
        match ext {
            "rs" => FileGlyph::Rust,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => FileGlyph::Script,
            "py" | "pyw" | "pyi" => FileGlyph::Python,
            "json" | "jsonc" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "env" | "xml" => FileGlyph::Config,
            "md" | "mdx" | "markdown" => FileGlyph::Markdown,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => FileGlyph::Image,
            "sh" | "bash" | "zsh" | "ps1" | "psm1" | "bat" | "cmd" => FileGlyph::Shell,
            "html" | "htm" | "css" | "scss" | "svg" => FileGlyph::Web,
            "exe" | "dll" | "bin" | "zip" | "7z" | "gz" | "tar" | "pdf" | "wasm" | "pdb" | "so" | "o" => FileGlyph::Binary,
            "txt" | "log" | "csv" | "tsv" => FileGlyph::Text,
            _ => FileGlyph::Generic,
        }
    }

    /// File stem of the future dedicated SVG (`assets/icons/file-<name>.svg`).
    pub fn svg_name(self) -> &'static str {
        match self {
            FileGlyph::Rust => "file-rust",
            FileGlyph::Script => "file-script",
            FileGlyph::Python => "file-python",
            FileGlyph::Config => "file-config",
            FileGlyph::Markdown => "file-markdown",
            FileGlyph::Image => "file-image",
            FileGlyph::Shell => "file-shell",
            FileGlyph::Web => "file-web",
            FileGlyph::Lock => "file-lock",
            FileGlyph::Binary => "file-binary",
            FileGlyph::Text => "file-text",
            FileGlyph::Generic => "file-generic",
        }
    }

    /// The generic icon used until the dedicated one exists.
    pub fn icon(self) -> Icon {
        // TODO-SVG: replace with the per-type icon named by `svg_name`.
        Icon::Files
    }

    /// Tint: restrained ANSI hues for code, `muted` for the rest.
    pub fn tint(self, theme: &Theme) -> Rgba {
        let a = |i: usize| rgb(theme.terminal.ansi[i]);
        let light = theme.mode == Mode::Light;
        let bright = |dark: usize, light_ix: usize| if light { a(light_ix) } else { a(dark) };
        match self {
            FileGlyph::Rust => bright(9, 1),
            FileGlyph::Script => bright(11, 3),
            FileGlyph::Python => bright(12, 4),
            FileGlyph::Config => bright(14, 6),
            FileGlyph::Markdown => bright(10, 2),
            FileGlyph::Image => bright(13, 5),
            FileGlyph::Shell => bright(10, 2),
            FileGlyph::Web => bright(9, 1),
            FileGlyph::Lock | FileGlyph::Binary | FileGlyph::Text | FileGlyph::Generic => theme.colors.outline,
        }
    }
}

/// The per-type SVGs the host should ask the designer for.
pub fn needed_svgs() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = FileGlyph::ALL.iter().map(|g| g.svg_name()).collect();
    names.push("folder-open");
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_by_name() {
        assert_eq!(FileGlyph::of("main.rs"), FileGlyph::Rust);
        assert_eq!(FileGlyph::of("App.TSX"), FileGlyph::Script);
        assert_eq!(FileGlyph::of("setup.py"), FileGlyph::Python);
        assert_eq!(FileGlyph::of("Cargo.toml"), FileGlyph::Config);
        assert_eq!(FileGlyph::of("Cargo.lock"), FileGlyph::Lock);
        assert_eq!(FileGlyph::of("package-lock.json"), FileGlyph::Lock);
        assert_eq!(FileGlyph::of("README.md"), FileGlyph::Markdown);
        assert_eq!(FileGlyph::of("icon.png"), FileGlyph::Image);
        assert_eq!(FileGlyph::of("build.ps1"), FileGlyph::Shell);
        assert_eq!(FileGlyph::of("index.html"), FileGlyph::Web);
        assert_eq!(FileGlyph::of("selachi.exe"), FileGlyph::Binary);
        assert_eq!(FileGlyph::of("notes.txt"), FileGlyph::Text);
        assert_eq!(FileGlyph::of("Makefile"), FileGlyph::Generic);
        assert_eq!(FileGlyph::of(".gitignore"), FileGlyph::Config);
        // a leading dot alone is not an extension
        assert_eq!(FileGlyph::of(".hidden"), FileGlyph::Generic);
    }

    #[test]
    fn every_category_has_a_unique_svg_name() {
        let names = needed_svgs();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
        assert!(names.iter().all(|n| n.starts_with("file-") || *n == "folder-open"));
    }
}
