//! File type detection: extension first, then a content sniff of the first 8 KB.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::document::Limits;

/// Bytes inspected by the content sniffer.
pub const SNIFF_BYTES: usize = 8 * 1024;

/// Languages the viewer knows by name. Whether a grammar is compiled in is a
/// separate question, see [`Lang::has_grammar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Json,
    Toml,
    Yaml,
    Python,
    Markdown,
    Css,
    Html,
    Bash,
    PowerShell,
    CSharp,
}

impl Lang {
    /// Every language, in a stable order.
    pub const ALL: [Lang; 14] = [
        Lang::Rust,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
        Lang::Json,
        Lang::Toml,
        Lang::Yaml,
        Lang::Python,
        Lang::Markdown,
        Lang::Css,
        Lang::Html,
        Lang::Bash,
        Lang::PowerShell,
        Lang::CSharp,
    ];

    /// Display name, also the label shown in a status bar.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "Rust",
            Lang::TypeScript => "TypeScript",
            Lang::Tsx => "TSX",
            Lang::JavaScript => "JavaScript",
            Lang::Json => "JSON",
            Lang::Toml => "TOML",
            Lang::Yaml => "YAML",
            Lang::Python => "Python",
            Lang::Markdown => "Markdown",
            Lang::Css => "CSS",
            Lang::Html => "HTML",
            Lang::Bash => "Bash",
            Lang::PowerShell => "PowerShell",
            Lang::CSharp => "C#",
        }
    }

    /// Language from a file extension (without the dot, case-insensitive).
    pub fn from_extension(ext: &str) -> Option<Lang> {
        let ext = ext.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => Lang::Rust,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "json" | "jsonc" | "json5" | "geojson" | "webmanifest" => Lang::Json,
            "toml" => Lang::Toml,
            "yaml" | "yml" => Lang::Yaml,
            "py" | "pyw" | "pyi" => Lang::Python,
            "md" | "markdown" | "mdx" => Lang::Markdown,
            "css" => Lang::Css,
            "html" | "htm" | "xhtml" => Lang::Html,
            "sh" | "bash" | "zsh" => Lang::Bash,
            "ps1" | "psm1" | "psd1" => Lang::PowerShell,
            "cs" | "csx" => Lang::CSharp,
            _ => return None,
        })
    }

    /// Language from a conventional file name (`Cargo.lock`, `.bashrc`...).
    pub fn from_file_name(name: &str) -> Option<Lang> {
        Some(match name {
            "Cargo.lock" | "Pipfile" | "poetry.lock" => Lang::Toml,
            ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" | "PKGBUILD" => Lang::Bash,
            ".eslintrc" | ".babelrc" | ".prettierrc" => Lang::Json,
            _ => return None,
        })
    }

    /// Language from a fenced-code info string or injection name (`rust`, `ts`, `sh`...).
    pub fn from_name(name: &str) -> Option<Lang> {
        let n = name.trim().to_ascii_lowercase();
        Some(match n.as_str() {
            "rust" | "rs" => Lang::Rust,
            "typescript" | "ts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "javascript" | "js" | "jsx" | "node" | "mjs" => Lang::JavaScript,
            "json" | "jsonc" => Lang::Json,
            "toml" => Lang::Toml,
            "yaml" | "yml" => Lang::Yaml,
            "python" | "py" | "python3" => Lang::Python,
            "markdown" | "md" => Lang::Markdown,
            "css" => Lang::Css,
            "html" | "htm" => Lang::Html,
            "bash" | "sh" | "shell" | "zsh" | "console" => Lang::Bash,
            "powershell" | "pwsh" | "ps1" => Lang::PowerShell,
            "csharp" | "c#" | "cs" => Lang::CSharp,
            _ => return None,
        })
    }

    /// Language from a `#!` line.
    pub fn from_shebang(first_line: &str) -> Option<Lang> {
        let rest = first_line.strip_prefix("#!")?.trim();
        let mut words = rest.split_whitespace();
        let mut prog = words.next()?;
        if prog.ends_with("/env") || prog == "env" {
            // skip `-S` style flags
            prog = words.find(|w| !w.starts_with('-'))?;
        }
        let base = prog.rsplit('/').next().unwrap_or(prog);
        if base.starts_with("python") {
            Some(Lang::Python)
        } else if matches!(base, "bash" | "sh" | "zsh" | "dash" | "ash") {
            Some(Lang::Bash)
        } else if matches!(base, "node" | "nodejs" | "deno" | "bun") {
            Some(Lang::JavaScript)
        } else if matches!(base, "pwsh" | "powershell") {
            Some(Lang::PowerShell)
        } else {
            None
        }
    }

    /// True when a tree-sitter grammar for this language is compiled into the crate.
    pub fn has_grammar(self) -> bool {
        crate::highlight::grammar_available(self)
    }
}

/// Text encodings the viewer decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Not valid UTF-8 and no BOM: shown byte-per-char (Latin-1 / Windows-1252 approximation).
    Latin1,
}

impl Encoding {
    /// Label for a status bar.
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Latin1 => "Latin-1",
        }
    }

    /// Length of the byte-order mark for this encoding.
    pub fn bom_len(self) -> usize {
        match self {
            Encoding::Utf8Bom => 3,
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            _ => 0,
        }
    }
}

/// Raster formats recognised by magic number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Ico,
}

impl ImageFormat {
    pub fn name(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Gif => "GIF",
            ImageFormat::WebP => "WebP",
            ImageFormat::Bmp => "BMP",
            ImageFormat::Ico => "ICO",
        }
    }
}

/// What the viewer should do with a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Plain or source text. `lang` selects the highlighter.
    Text {
        lang: Option<Lang>,
        encoding: Encoding,
    },
    Markdown,
    Json,
    Toml,
    Yaml,
    Image {
        format: ImageFormat,
    },
    Svg,
    Binary,
    /// A text file bigger than `Limits::max_bytes`: only the hex preview is offered.
    TooLarge,
}

impl FileKind {
    /// Language of the file, when it has one (also for Markdown/JSON/TOML/YAML).
    pub fn lang(&self) -> Option<Lang> {
        match self {
            FileKind::Text { lang, .. } => *lang,
            FileKind::Markdown => Some(Lang::Markdown),
            FileKind::Json => Some(Lang::Json),
            FileKind::Toml => Some(Lang::Toml),
            FileKind::Yaml => Some(Lang::Yaml),
            _ => None,
        }
    }

    /// True when the content can be shown as lines of text.
    pub fn is_textual(&self) -> bool {
        matches!(
            self,
            FileKind::Text { .. }
                | FileKind::Markdown
                | FileKind::Json
                | FileKind::Toml
                | FileKind::Yaml
                | FileKind::Svg
        )
    }
}

/// Result of sniffing the first bytes of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sniff {
    Image(ImageFormat),
    Text(Encoding),
    Binary,
}

/// Detect the kind of `path` with the default limits.
///
/// Never fails: unreadable or missing files are reported as [`FileKind::Binary`].
pub fn detect(path: &Path) -> FileKind {
    detect_with(path, &Limits::default())
}

/// Like [`detect`] with explicit limits.
pub fn detect_with(path: &Path, limits: &Limits) -> FileKind {
    let Ok(meta) = std::fs::metadata(path) else {
        return FileKind::Binary;
    };
    if !meta.is_file() {
        return FileKind::Binary;
    }
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    match File::open(path) {
        Ok(f) => {
            if f.take(SNIFF_BYTES as u64).read_to_end(&mut head).is_err() {
                return FileKind::Binary;
            }
        }
        Err(_) => return FileKind::Binary,
    }
    classify(path, &head, meta.len(), limits)
}

/// Classify from already-read bytes (the first 8 KB are enough) and the full file size.
pub fn detect_bytes(path_hint: &Path, head: &[u8], total_len: u64, limits: &Limits) -> FileKind {
    classify(path_hint, head, total_len, limits)
}

pub(crate) fn classify(path: &Path, head: &[u8], total_len: u64, limits: &Limits) -> FileKind {
    classify_with_encoding(path, head, total_len, limits).0
}

/// Same as [`classify`] but also returns the sniffed text encoding when textual.
pub(crate) fn classify_with_encoding(
    path: &Path,
    head: &[u8],
    total_len: u64,
    limits: &Limits,
) -> (FileKind, Option<Encoding>) {
    let head = &head[..head.len().min(SNIFF_BYTES)];
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    let sniffed = sniff(head, ext.as_deref(), total_len > head.len() as u64);
    let encoding = match sniffed {
        Sniff::Image(format) => return (FileKind::Image { format }, None),
        Sniff::Binary => return (FileKind::Binary, None),
        Sniff::Text(enc) => enc,
    };
    if total_len > limits.max_bytes {
        return (FileKind::TooLarge, Some(encoding));
    }

    if ext.as_deref() == Some("svg") {
        return (FileKind::Svg, Some(encoding));
    }
    let lang = ext
        .as_deref()
        .and_then(Lang::from_extension)
        .or_else(|| Lang::from_file_name(name))
        .or_else(|| shebang_lang(head, encoding));
    let kind = match lang {
        Some(Lang::Markdown) => FileKind::Markdown,
        Some(Lang::Json) => FileKind::Json,
        Some(Lang::Toml) => FileKind::Toml,
        Some(Lang::Yaml) => FileKind::Yaml,
        other => FileKind::Text {
            lang: other,
            encoding,
        },
    };
    (kind, Some(encoding))
}

fn shebang_lang(head: &[u8], encoding: Encoding) -> Option<Lang> {
    if matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be) {
        return None;
    }
    let start = encoding.bom_len();
    let body = head.get(start..)?;
    if !body.starts_with(b"#!") {
        return None;
    }
    let end = memchr::memchr(b'\n', body).unwrap_or(body.len().min(200));
    let line = std::str::from_utf8(&body[..end]).ok()?;
    Lang::from_shebang(line.trim_end())
}

/// Magic numbers, then BOM / NUL / UTF-8 validity.
pub(crate) fn sniff(head: &[u8], ext: Option<&str>, truncated: bool) -> Sniff {
    if let Some(format) = magic_image(head, ext) {
        return Sniff::Image(format);
    }
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Sniff::Text(Encoding::Utf8Bom);
    }
    // UTF-32 BOMs: FF FE 00 00 / 00 00 FE FF. Checked before UTF-16 LE.
    if head.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) || head.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Sniff::Binary;
    }
    if head.starts_with(&[0xFF, 0xFE]) {
        return Sniff::Text(Encoding::Utf16Le);
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return Sniff::Text(Encoding::Utf16Be);
    }
    if memchr::memchr(0, head).is_some() {
        return Sniff::Binary;
    }
    match std::str::from_utf8(head) {
        Ok(_) => Sniff::Text(Encoding::Utf8),
        // A multi-byte sequence cut by the 8 KB window is not an error.
        Err(e) if truncated && e.error_len().is_none() => Sniff::Text(Encoding::Utf8),
        // Mostly-printable text with stray high bytes is Latin-1-ish; control-heavy is binary.
        Err(_) if control_ratio(head) > 0.10 => Sniff::Binary,
        Err(_) => Sniff::Text(Encoding::Latin1),
    }
}

fn control_ratio(head: &[u8]) -> f32 {
    if head.is_empty() {
        return 0.0;
    }
    let ctrl = head
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t' | 0x0C | 0x1B))
        .count();
    ctrl as f32 / head.len() as f32
}

fn magic_image(head: &[u8], ext: Option<&str>) -> Option<ImageFormat> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageFormat::Png);
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some(ImageFormat::WebP);
    }
    // BMP ("BM") and ICO are weak signatures: require the extension too.
    if head.starts_with(b"BM") && head.len() >= 14 && ext == Some("bmp") {
        return Some(ImageFormat::Bmp);
    }
    if head.starts_with(&[0, 0, 1, 0]) && head.len() >= 6 && ext == Some("ico") {
        return Some(ImageFormat::Ico);
    }
    None
}

/// True when `ext` (no dot) is an extension the viewer recognises. Used by the
/// terminal path detector to accept bare `name.ext` tokens.
pub(crate) fn is_known_extension(ext: &str) -> bool {
    const EXTRA: &[&str] = &[
        "txt", "log", "lock", "c", "h", "cpp", "hpp", "cc", "go", "java", "kt", "swift", "rb",
        "php", "lua", "sql", "xml", "vue", "svelte", "scss", "less", "ini", "cfg", "conf", "env",
        "csv", "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "pdf", "zip", "gz", "bat",
        "cmd", "sln", "csproj", "props", "targets", "diff", "patch", "rst", "tex", "gradle",
        "proto", "graphql", "wasm", "dll", "exe", "pdb", "lnk", "db", "sqlite",
    ];
    Lang::from_extension(ext).is_some() || EXTRA.contains(&ext.to_ascii_lowercase().as_str())
}
