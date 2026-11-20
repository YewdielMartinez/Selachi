//! Read-only document: memory-mapped bytes, lazily built line index, hex preview,
//! minimap data and the glue to the highlighter.

use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use memmap2::Mmap;

use crate::detect::{classify_with_encoding, Encoding, FileKind, Lang, SNIFF_BYTES};
use crate::error::{Error, Result};
use crate::highlight::{self, HighlightKind, Span};

/// Size limits and tuning knobs for [`Document::open`].
#[derive(Debug, Clone)]
pub struct Limits {
    /// Beyond this size text is not opened as lines, only [`Document::hex_preview`] works.
    pub max_bytes: u64,
    /// Files bigger than this are not syntax highlighted (plain text).
    pub max_highlight_bytes: u64,
    /// Files up to this size are read into memory; bigger ones are memory-mapped.
    ///
    /// Small files are copied on purpose: a live memory map of a file that an agent
    /// truncates or rewrites would fault the viewer (and, on Windows, block the writer).
    pub mmap_threshold: u64,
    /// Bytes scanned per step of the incremental line index.
    pub index_chunk_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 50 * 1024 * 1024,
            max_highlight_bytes: 5 * 1024 * 1024,
            mmap_threshold: 8 * 1024 * 1024,
            index_chunk_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Snapshot of the incremental line index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexProgress {
    pub scanned_bytes: u64,
    pub total_bytes: u64,
    /// Lines discovered so far (final once `complete`).
    pub lines: usize,
    pub complete: bool,
}

impl IndexProgress {
    /// Fraction of the file scanned, `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        if self.complete || self.total_bytes == 0 {
            1.0
        } else {
            (self.scanned_bytes as f64 / self.total_bytes as f64) as f32
        }
    }
}

/// One 16-byte row of the hex view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexLine {
    pub offset: u64,
    pub bytes: Vec<u8>,
    /// Printable ASCII rendering, `.` for everything else.
    pub ascii: String,
}

impl HexLine {
    /// Bytes as `"48 65 6c 6c ..."`.
    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(self.bytes.len() * 3);
        for (i, b) in self.bytes.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
}

/// Bytes per row in [`Document::hex_preview`].
pub const HEX_ROW: usize = 16;

enum Data {
    Mmap(Mmap),
    Owned(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Mmap(m) => m,
            Data::Owned(v) => v,
        }
    }
}

#[derive(Debug)]
struct LineIndex {
    /// Start offset of every discovered line, relative to the text bytes.
    offsets: Vec<u64>,
    scanned: u64,
    complete: bool,
}

pub(crate) enum HlState {
    Idle,
    Running,
    Ready(Arc<Vec<Span>>),
}

struct Inner {
    path: PathBuf,
    kind: FileKind,
    encoding: Encoding,
    limits: Limits,
    data: Data,
    /// UTF-8 re-encoding of UTF-16 files.
    transcoded: Option<Vec<u8>>,
    text_start: usize,
    textual: bool,
    index: Mutex<LineIndex>,
    hl: Mutex<HlState>,
    cancel: Arc<AtomicUsize>,
}

/// Sets the cancel flag when the last [`Document`] clone goes away so background
/// threads stop early.
struct CancelGuard(Arc<AtomicUsize>);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.store(1, Ordering::Relaxed);
    }
}

/// A file opened for viewing. Cheap to clone (shared state), `Send + Sync`.
#[derive(Clone)]
pub struct Document {
    inner: Arc<Inner>,
    _guard: Arc<CancelGuard>,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("path", &self.inner.path)
            .field("kind", &self.inner.kind)
            .field("len", &self.inner.data.bytes().len())
            .finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a worker thread panicked; the data is still usable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Document {
    /// Open `path`. Returns quickly: nothing is scanned yet. A background thread
    /// builds the line index (and, for small source files, the syntax highlight);
    /// every accessor also works before they finish.
    pub fn open(path: &Path, limits: Limits) -> Result<Document> {
        let doc = Self::open_lazy(path, limits)?;
        doc.spawn_indexer();
        doc.start_highlighting();
        Ok(doc)
    }

    /// Like [`Document::open`] but without background threads: the caller drives
    /// the index with [`Document::index_step`] (or lets `lines()` index on demand).
    pub fn open_lazy(path: &Path, limits: Limits) -> Result<Document> {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        let meta = file.metadata().map_err(|e| Error::io(path, e))?;
        if !meta.is_file() {
            return Err(Error::InvalidInput(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        let len = meta.len();
        let data = if len == 0 {
            Data::Owned(Vec::new())
        } else if len > limits.mmap_threshold {
            // SAFETY: the map is read-only. If another process truncates the file while
            // it is mapped, access can fault; that is why small files are copied instead
            // (see `Limits::mmap_threshold`).
            match unsafe { Mmap::map(&file) } {
                Ok(m) => Data::Mmap(m),
                Err(_) if len <= limits.max_bytes => Data::Owned(read_all(&file, len, path)?),
                Err(e) => return Err(Error::io(path, e)),
            }
        } else {
            Data::Owned(read_all(&file, len, path)?)
        };

        let bytes = data.bytes();
        let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
        let (kind, enc) = classify_with_encoding(path, head, bytes.len() as u64, &limits);
        let textual = kind.is_textual();
        let encoding = enc.unwrap_or(Encoding::Utf8);

        let (transcoded, text_start) = if !textual {
            (None, 0)
        } else {
            match encoding {
                Encoding::Utf16Le => (Some(transcode_utf16(bytes, true)), 0),
                Encoding::Utf16Be => (Some(transcode_utf16(bytes, false)), 0),
                other => (None, other.bom_len().min(bytes.len())),
            }
        };

        let text_len = match &transcoded {
            Some(t) => t.len(),
            None if textual => bytes.len() - text_start,
            None => 0,
        };
        let index = LineIndex {
            offsets: if text_len > 0 { vec![0] } else { Vec::new() },
            scanned: 0,
            complete: text_len == 0,
        };

        let cancel = Arc::new(AtomicUsize::new(0));
        Ok(Document {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                kind,
                encoding,
                limits,
                data,
                transcoded,
                text_start,
                textual,
                index: Mutex::new(index),
                hl: Mutex::new(HlState::Idle),
                cancel: Arc::clone(&cancel),
            }),
            _guard: Arc::new(CancelGuard(cancel)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn kind(&self) -> FileKind {
        self.inner.kind
    }

    pub fn lang(&self) -> Option<Lang> {
        self.inner.kind.lang()
    }

    pub fn encoding(&self) -> Encoding {
        self.inner.encoding
    }

    pub fn limits(&self) -> &Limits {
        &self.inner.limits
    }

    /// Size of the file on disk in bytes.
    pub fn len(&self) -> u64 {
        self.inner.data.bytes().len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True when `lines()` returns content (false for binary, images and too-large files).
    pub fn is_textual(&self) -> bool {
        self.inner.textual
    }

    /// Raw file bytes exactly as on disk.
    pub fn raw_bytes(&self) -> &[u8] {
        self.inner.data.bytes()
    }

    /// UTF-8 text bytes (BOM stripped, UTF-16 re-encoded). Empty when not textual.
    /// Latin-1 files are returned as-is (not UTF-8).
    pub fn text_bytes(&self) -> &[u8] {
        if !self.inner.textual {
            return &[];
        }
        match &self.inner.transcoded {
            Some(t) => t,
            None => self
                .inner
                .data
                .bytes()
                .get(self.inner.text_start..)
                .unwrap_or(&[]),
        }
    }

    /// The whole text, lossily decoded. Intended for Markdown / JSON / TOML sources
    /// (already bounded by `Limits::max_bytes`).
    pub fn to_text(&self) -> Cow<'_, str> {
        decode(self.text_bytes(), self.inner.encoding)
    }

    // ---- line index ------------------------------------------------------------

    /// Lines discovered so far; the final count once [`Document::is_indexed`].
    pub fn line_count(&self) -> usize {
        lock(&self.inner.index).offsets.len()
    }

    /// Best guess of the final line count (exact once indexed). For sizing a scrollbar.
    pub fn estimated_line_count(&self) -> usize {
        let idx = lock(&self.inner.index);
        if idx.complete || idx.scanned == 0 {
            return idx.offsets.len().max(usize::from(!idx.complete));
        }
        let total = self.text_bytes().len() as f64;
        (idx.offsets.len() as f64 * total / idx.scanned as f64).ceil() as usize
    }

    pub fn is_indexed(&self) -> bool {
        lock(&self.inner.index).complete
    }

    pub fn index_progress(&self) -> IndexProgress {
        let idx = lock(&self.inner.index);
        IndexProgress {
            scanned_bytes: idx.scanned,
            total_bytes: self.text_bytes().len() as u64,
            lines: idx.offsets.len(),
            complete: idx.complete,
        }
    }

    /// Scan up to `budget` more bytes for newlines. Returns the new progress.
    pub fn index_step(&self, budget: usize) -> IndexProgress {
        let text = self.text_bytes();
        let mut idx = lock(&self.inner.index);
        scan(&mut idx, text, budget);
        IndexProgress {
            scanned_bytes: idx.scanned,
            total_bytes: text.len() as u64,
            lines: idx.offsets.len(),
            complete: idx.complete,
        }
    }

    /// Block until the whole file is indexed (tests, small files).
    pub fn wait_indexed(&self) -> IndexProgress {
        loop {
            let p = self.index_step(self.inner.limits.index_chunk_bytes.max(64 * 1024));
            if p.complete {
                return p;
            }
        }
    }

    fn spawn_indexer(&self) {
        if self.is_indexed() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let chunk = inner.limits.index_chunk_bytes.max(64 * 1024);
        // The worker only holds `inner`, never the cancel guard, so dropping the last
        // `Document` clone stops it.
        let _ = std::thread::Builder::new()
            .name("viewer-index".into())
            .spawn(move || {
                let text: &[u8] = match &inner.transcoded {
                    Some(t) => t,
                    None => inner.data.bytes().get(inner.text_start..).unwrap_or(&[]),
                };
                loop {
                    if inner.cancel.load(Ordering::Relaxed) != 0 {
                        return;
                    }
                    let mut idx = lock(&inner.index);
                    if idx.complete {
                        return;
                    }
                    scan(&mut idx, text, chunk);
                    if idx.complete {
                        return;
                    }
                    drop(idx);
                    std::thread::yield_now();
                }
            });
    }

    /// Make sure line `upto` (exclusive) is bounded, indexing on demand.
    fn ensure<'a>(&'a self, upto: usize) -> MutexGuard<'a, LineIndex> {
        let text = self.text_bytes();
        let chunk = self.inner.limits.index_chunk_bytes.max(64 * 1024);
        let mut idx = lock(&self.inner.index);
        while !idx.complete && idx.offsets.len() <= upto {
            scan(&mut idx, text, chunk);
        }
        idx
    }

    /// Byte range of line `i` in [`Document::text_bytes`], without its line ending.
    fn span_of(&self, idx: &LineIndex, i: usize) -> Option<Range<usize>> {
        let text = self.text_bytes();
        let start = *idx.offsets.get(i)? as usize;
        let end = match idx.offsets.get(i + 1) {
            Some(&n) => n as usize,
            None if idx.complete => text.len(),
            None => return None,
        };
        let mut end = end.min(text.len());
        if end > start && text.get(end - 1) == Some(&b'\n') {
            end -= 1;
        }
        if end > start && text.get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
        Some(start..end)
    }

    /// Byte ranges (in `text_bytes()`) of the lines in `range`, clamped to the file.
    pub fn line_spans(&self, range: Range<usize>) -> Vec<Range<usize>> {
        if !self.inner.textual || range.start >= range.end {
            return Vec::new();
        }
        let idx = self.ensure(range.end);
        let end = range.end.min(idx.offsets.len());
        (range.start..end)
            .filter_map(|i| self.span_of(&idx, i))
            .collect()
    }

    /// Lines `range`, lossily decoded, with `\n` / `\r\n` removed. Indexes on demand,
    /// so the first lines of a huge file are available immediately. Out-of-range
    /// indices are dropped.
    pub fn lines(&self, range: Range<usize>) -> Vec<Cow<'_, str>> {
        let text = self.text_bytes();
        let enc = self.inner.encoding;
        self.line_spans(range)
            .into_iter()
            .map(|r| decode(text.get(r).unwrap_or(&[]), enc))
            .collect()
    }

    /// One line, or `None` past the end.
    pub fn line(&self, i: usize) -> Option<Cow<'_, str>> {
        self.lines(i..i + 1).into_iter().next()
    }

    // ---- hex ---------------------------------------------------------------------

    /// 16-byte rows covering `offset..offset+len` of the raw file. Works for every
    /// kind, including [`FileKind::TooLarge`] and binaries, without reading the rest.
    pub fn hex_preview(&self, offset: u64, len: usize) -> Vec<HexLine> {
        let data = self.inner.data.bytes();
        let start = (offset as usize).min(data.len());
        let start = start - start % HEX_ROW;
        let end = (offset as usize).saturating_add(len).min(data.len());
        data.get(start..end)
            .unwrap_or(&[])
            .chunks(HEX_ROW)
            .enumerate()
            .map(|(i, chunk)| HexLine {
                offset: (start + i * HEX_ROW) as u64,
                bytes: chunk.to_vec(),
                ascii: chunk
                    .iter()
                    .map(|&b| {
                        if (0x20..0x7f).contains(&b) {
                            b as char
                        } else {
                            '.'
                        }
                    })
                    .collect(),
            })
            .collect()
    }

    // ---- minimap -----------------------------------------------------------------

    /// Line-length histogram for the pixel-bar minimap.
    ///
    /// Splits the lines in `range` into at most `buckets` equal groups and returns the
    /// longest line (in bytes, saturating at `u16::MAX`) of each group. With fewer
    /// lines than buckets there is one entry per line. Uses the line index only, it
    /// does not read the file contents; it indexes up to `range.end` if needed.
    pub fn minimap(&self, range: Range<usize>, buckets: usize) -> Vec<u16> {
        if !self.inner.textual || buckets == 0 || range.start >= range.end {
            return Vec::new();
        }
        let idx = self.ensure(range.end);
        let end = range.end.min(idx.offsets.len());
        if range.start >= end {
            return Vec::new();
        }
        let lengths = (range.start..end).map(|i| {
            let len = self.span_of(&idx, i).map_or(0, |r| r.len());
            len.min(usize::from(u16::MAX)) as u16
        });
        minimap_from_lengths(lengths, end - range.start, buckets)
    }

    // ---- highlighting ------------------------------------------------------------

    /// Whether the size/encoding of this document allow highlighting at all.
    fn highlightable(&self) -> bool {
        let inner = &self.inner;
        inner.textual
            && inner.encoding != Encoding::Latin1
            && (inner
                .transcoded
                .as_ref()
                .map_or(inner.data.bytes().len(), Vec::len) as u64)
                <= inner.limits.max_highlight_bytes
    }

    /// Id of the grammar that will highlight this document: a compiled-in one or an
    /// installed plugin (see [`crate::grammars`]). `None` when the file is not source
    /// code, too big, or its language has no usable grammar (it shows without colour).
    pub fn grammar_id(&self) -> Option<String> {
        if !self.highlightable() {
            return None;
        }
        match self.inner.kind.lang() {
            Some(lang) => lang.has_grammar().then(|| lang.id().to_owned()),
            // Languages the viewer does not know by name can still come from a plugin
            // that declares the extension in its manifest.
            None if matches!(self.inner.kind, FileKind::Text { .. }) => {
                let ext = self.inner.path.extension()?.to_str()?;
                highlight::plugin_id_for_extension(ext)
            }
            None => None,
        }
    }

    /// When the document's language has an installed plugin that failed to load (hash
    /// mismatch, incompatible ABI, missing symbol...), the reason. The file is shown
    /// without colour; the UI can surface this as a warning.
    pub fn highlight_warning(&self) -> Option<String> {
        match self.inner.kind.lang() {
            Some(lang) => highlight::plugin_failure(Some(lang.id()), None),
            None => {
                let ext = self.inner.path.extension()?.to_str()?;
                highlight::plugin_failure(None, Some(ext))
            }
        }
    }

    /// Kick off the whole-file parse on a background thread (done by `open`). No-op
    /// when the file is too big, not source code, or has no compiled grammar.
    pub fn start_highlighting(&self) {
        let Some(lang) = self.grammar_id() else {
            return;
        };
        {
            let mut st = lock(&self.inner.hl);
            if !matches!(*st, HlState::Idle) {
                return;
            }
            *st = HlState::Running;
        }
        let inner = Arc::clone(&self.inner);
        let spawned = std::thread::Builder::new()
            .name("viewer-highlight".into())
            .spawn(move || {
                let text: &[u8] = match &inner.transcoded {
                    Some(t) => t,
                    None => inner.data.bytes().get(inner.text_start..).unwrap_or(&[]),
                };
                let spans = highlight::highlight_bytes(&lang, text, Some(&inner.cancel));
                let mut st = lock(&inner.hl);
                *st = if inner.cancel.load(Ordering::Relaxed) != 0 {
                    HlState::Idle
                } else {
                    HlState::Ready(Arc::new(spans))
                };
            });
        if spawned.is_err() {
            *lock(&self.inner.hl) = HlState::Idle;
        }
    }

    /// Run the whole-file highlight on the calling thread (tests, tools).
    pub fn highlight_blocking(&self) {
        let Some(lang) = self.grammar_id() else {
            return;
        };
        let spans = highlight::highlight_bytes(&lang, self.text_bytes(), None);
        *lock(&self.inner.hl) = HlState::Ready(Arc::new(spans));
    }

    /// True once the whole-file parse finished (highlights are then exact).
    pub fn highlight_ready(&self) -> bool {
        matches!(*lock(&self.inner.hl), HlState::Ready(_))
    }

    /// Highlight spans for the lines in `range`: one `Vec` per returned line (same
    /// length and order as [`Document::lines`]), with byte ranges relative to the
    /// start of that line's text. Unlisted bytes are [`HighlightKind::Plain`].
    ///
    /// Uses the whole-file parse when finished; until then it highlights just the
    /// visible window (plus up to 100 lines of lead-in), which is instant but can be
    /// wrong inside constructs that start above the window. Files above
    /// `Limits::max_highlight_bytes`, binaries and languages without a grammar
    /// return empty span lists.
    pub fn highlight_lines(&self, range: Range<usize>) -> Vec<Vec<(Range<usize>, HighlightKind)>> {
        let lines = self.line_spans(range.clone());
        let empty = |n: usize| vec![Vec::new(); n];
        let Some(lang) = self.grammar_id() else {
            return empty(lines.len());
        };
        let text = self.text_bytes();

        let ready = match &*lock(&self.inner.hl) {
            HlState::Ready(s) => Some(Arc::clone(s)),
            _ => None,
        };
        if let Some(spans) = ready {
            return highlight::slice_spans(&spans, 0, &lines);
        }

        // Window fallback.
        let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
            return Vec::new();
        };
        const LEAD_IN: usize = 100;
        const MAX_WINDOW: usize = 512 * 1024;
        let lead = range.start.saturating_sub(LEAD_IN);
        let win_start = if lead < range.start {
            self.line_spans(lead..range.start + 1)
                .first()
                .map_or(first.start, |r| r.start)
        } else {
            first.start
        };
        let win_end = last.end;
        if win_end.saturating_sub(win_start) > MAX_WINDOW {
            return empty(lines.len());
        }
        let Some(window) = text.get(win_start..win_end) else {
            return empty(lines.len());
        };
        let spans = highlight::highlight_bytes(&lang, window, None);
        highlight::slice_spans(&spans, win_start, &lines)
    }
}

/// Bucket a sequence of `n` line lengths into at most `buckets` maxima.
pub fn minimap_from_lengths(
    lengths: impl IntoIterator<Item = u16>,
    n: usize,
    buckets: usize,
) -> Vec<u16> {
    if n == 0 || buckets == 0 {
        return Vec::new();
    }
    let out_len = n.min(buckets);
    let mut out = vec![0u16; out_len];
    for (i, len) in lengths.into_iter().take(n).enumerate() {
        let b = (i as u128 * out_len as u128 / n as u128) as usize;
        if let Some(slot) = out.get_mut(b) {
            *slot = (*slot).max(len);
        }
    }
    out
}

fn scan(idx: &mut LineIndex, text: &[u8], budget: usize) {
    if idx.complete {
        return;
    }
    let start = idx.scanned as usize;
    let end = text.len().min(start.saturating_add(budget.max(1)));
    let total = text.len();
    if let Some(slice) = text.get(start..end) {
        for pos in memchr::memchr_iter(b'\n', slice) {
            let next = start + pos + 1;
            if next < total {
                idx.offsets.push(next as u64);
            }
        }
    }
    idx.scanned = end as u64;
    if end >= total {
        idx.complete = true;
    }
}

fn read_all(file: &File, len: u64, path: &Path) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(len as usize);
    let mut f = file;
    f.read_to_end(&mut buf).map_err(|e| Error::io(path, e))?;
    Ok(buf)
}

fn decode(bytes: &[u8], enc: Encoding) -> Cow<'_, str> {
    match enc {
        Encoding::Latin1 => Cow::Owned(bytes.iter().map(|&b| b as char).collect()),
        _ => String::from_utf8_lossy(bytes),
    }
}

fn transcode_utf16(raw: &[u8], le: bool) -> Vec<u8> {
    let body = raw.get(2..).unwrap_or(&[]);
    let units = body.as_chunks::<2>().0.iter().map(|&[a, b]| {
        if le {
            u16::from_le_bytes([a, b])
        } else {
            u16::from_be_bytes([a, b])
        }
    });
    let mut s = String::with_capacity(body.len() / 2);
    for r in char::decode_utf16(units) {
        s.push(r.unwrap_or('\u{FFFD}'));
    }
    s.into_bytes()
}
