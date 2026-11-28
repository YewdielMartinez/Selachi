//! The composer: a rounded card with the text field, `+`, the context meter, the permission /
//! model / agent pills and the round send-or-stop button; the plan bar above it and the
//! branch and path line below. It never talks to chat_core: it shows a [`ComposerState`] pushed
//! by the view and reports what the user did as [`ComposerEvent`]s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agents_driver::TaskState;
use agents_driver::{AgentKind, PermissionMode};
use chat_core::{ContextUsage, Plan};
use gpui::{
    AnyElement, App, ClickEvent, ClipboardEntry, Context, Entity, EventEmitter, FontWeight,
    ImageFormat, IntoElement, ObjectFit, ParentElement as _, PathPromptOptions, Render,
    RenderImage, SharedString, Styled as _, Subscription, Window, div, img, prelude::*, px,
    relative,
};
use theme::{Radius, Theme};
use ui_kit::{
    Button, DitherMeter, Icon, IconButton, Nav, Pill, Placement, Popover, Spinner, TextArea,
    TextField, TextFieldEvent, Tooltip, icon, text_field::act as text_act,
};

use crate::hub::{ChatHub, FilesReady};
use crate::mentions::{MentionQuery, filter_files, mention_at};
use crate::options::{
    AgentOption, PERMISSION_ORDER, effort_label, efforts_for, is_dangerous, model_label,
    models_for, permission_hint, permission_label,
};
use crate::util::{file_name, format_tokens, format_window, tail_path};
use crate::widgets::{file_chip, menu_row};

/// How many files the `@` picker lists.
const MENTION_ROWS: usize = 8;

/// The thumbnail of an attached image (see [`crate::thumb`]).
enum Thumb {
    Loading,
    Ready(Arc<RenderImage>),
    /// Not decodable: the chip shows a plain square.
    Failed,
}

/// Everything the composer displays, pushed by the owner.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposerState {
    /// A turn is running: the round button is Stop.
    pub working: bool,
    pub context: Option<ContextUsage>,
    pub permission: PermissionMode,
    /// The pill is inert while a turn runs.
    pub can_edit_permission: bool,
    pub agent: AgentKind,
    pub model: Option<String>,
    /// Reasoning effort (`low`..`max`); the pill only exists for agents that support it.
    pub effort: Option<String>,
    pub agents: Vec<AgentOption>,
    /// Root of the project: where `@` mentions come from.
    pub project_root: Option<PathBuf>,
    /// Branch under the composer.
    pub branch: Option<String>,
    /// Worktree or working folder under the composer.
    pub path: Option<PathBuf>,
    pub plan: Option<Plan>,
    /// Placeholder of the field.
    pub placeholder: SharedString,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            working: false,
            context: None,
            permission: PermissionMode::Ask,
            can_edit_permission: true,
            agent: AgentKind::ClaudeCode,
            model: None,
            effort: None,
            agents: crate::options::default_agents(),
            project_root: None,
            branch: None,
            path: None,
            plan: None,
            placeholder: "Pide algo al agente…  (@ para mencionar archivos)".into(),
        }
    }
}

/// What the round button does right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonMode {
    /// A turn is running: interrupt it.
    Stop,
    /// Send the message.
    Send,
    /// Nothing to send.
    Disabled,
}

/// The round button's mode for a composer that is `working` and has (or not) something to send.
pub fn button_mode(working: bool, has_content: bool) -> ButtonMode {
    if working {
        ButtonMode::Stop
    } else if has_content {
        ButtonMode::Send
    } else {
        ButtonMode::Disabled
    }
}

/// What the user did in the composer.
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerEvent {
    Send {
        text: String,
        attachments: Vec<PathBuf>,
    },
    Stop,
    SetPermission(PermissionMode),
    SetModel(Option<String>),
    /// `None` = the agent's default effort.
    SetEffort(Option<String>),
    /// The user picked another agent in the menu: the owner decides (a draft just switches; a
    /// thread asks for the handoff preview and calls [`Composer::set_handoff_preview`]).
    PickAgent(AgentKind),
    /// The user confirmed the handoff popover.
    ConfirmSwitch {
        agent: AgentKind,
        model: Option<String>,
    },
}

/// The handoff text of a pending agent switch.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    Loading,
    /// The thread has nothing to hand over yet.
    Empty,
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Popup {
    Plus,
    Permission,
    Model,
    Effort,
    Agent,
    Handoff { agent: AgentKind, preview: Preview },
}

struct Mention {
    query: MentionQuery,
    items: Vec<String>,
    selected: usize,
}

/// The composer's own width below which its pills collapse to icons.
const COMPACT_BELOW: f32 = 480.0;

pub struct Composer {
    hub: Entity<ChatHub>,
    theme: Theme,
    state: ComposerState,
    field: Entity<TextArea>,
    model_field: Entity<TextField>,
    attachments: Vec<PathBuf>,
    /// Downscaled previews of the attached images, by path (dropped with the attachment).
    thumbs: HashMap<PathBuf, Thumb>,
    popup: Option<Popup>,
    mention: Option<Mention>,
    plan_open: bool,
    /// Own width (the chat column), not the window's: compact pills below it.
    measured: ui_kit::MeasuredWidth,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(hub: Entity<ChatHub>, theme: Theme, cx: &mut Context<Self>) -> Self {
        let state = ComposerState::default();
        // `Enter` sends, `Shift+Enter` adds a line (the area's default `EnterMode::Submit`).
        let field = cx.new(|cx| {
            TextArea::area(theme, cx)
                .bare()
                .lines(1, 10)
                .placeholder(state.placeholder.clone())
        });
        let model_field = cx.new(|cx| TextField::new(theme, cx).bare().placeholder("Otro modelo…"));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&field, |this, _f, ev: &TextFieldEvent, cx| {
            this.on_field(*ev, cx)
        }));
        subs.push(
            cx.subscribe(&model_field, |this, f, ev: &TextFieldEvent, cx| match ev {
                TextFieldEvent::Submit => {
                    let m = f.read(cx).text().trim().to_string();
                    if !m.is_empty() {
                        this.popup = None;
                        f.update(cx, |f, cx| f.set_text("", cx));
                        cx.emit(ComposerEvent::SetModel(Some(m)));
                        cx.notify();
                    }
                }
                TextFieldEvent::Cancel => this.close_popup(cx),
                _ => {}
            }),
        );
        subs.push(cx.subscribe(&hub, |_, _hub, _: &crate::hub::ModelsReady, cx| cx.notify()));
        subs.push(cx.subscribe(&hub, |this, _hub, ev: &FilesReady, cx| {
            if this.mention.is_some()
                && this.state.project_root.as_deref() == Some(ev.root.as_path())
            {
                this.refresh_mention(cx);
            }
        }));
        Self {
            hub,
            theme,
            state,
            field,
            model_field,
            attachments: Vec::new(),
            thumbs: HashMap::new(),
            popup: None,
            mention: None,
            plan_open: false,
            measured: ui_kit::MeasuredWidth::new(COMPACT_BELOW),
            _subs: subs,
        }
    }

    // ----- state ---------------------------------------------------------------------------

    pub fn state(&self) -> &ComposerState {
        &self.state
    }

    /// Replaces what is displayed. Returns whether anything changed (the owner notifies).
    pub fn set_state(&mut self, state: ComposerState, cx: &mut Context<Self>) -> bool {
        if self.state == state {
            return false;
        }
        if self.state.placeholder != state.placeholder {
            let p = state.placeholder.clone();
            self.field.update(cx, |f, cx| f.set_placeholder(p, cx));
        }
        if state.working != self.state.working {
            // A running turn closes pickers that would change it under its feet.
            if state.working && matches!(self.popup, Some(Popup::Permission)) {
                self.popup = None;
            }
        }
        self.state = state;
        true
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        self.field.update(cx, |f, cx| f.set_theme(theme, cx));
        self.model_field.update(cx, |f, cx| f.set_theme(theme, cx));
        cx.notify();
    }

    pub fn field(&self) -> &Entity<TextArea> {
        &self.field
    }

    pub fn text(&self, cx: &App) -> String {
        self.field.read(cx).text().to_string()
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.field.update(cx, |f, cx| f.set_text(text, cx));
        self.refresh_mention(cx);
        cx.notify();
    }

    pub fn attachments(&self) -> &[PathBuf] {
        &self.attachments
    }

    /// Starts decoding the thumbnails of newly attached images (off the UI thread) and drops
    /// those of removed attachments, texture included.
    fn sync_thumbs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gone: Vec<PathBuf> = self
            .thumbs
            .keys()
            .filter(|p| !self.attachments.contains(p))
            .cloned()
            .collect();
        for p in gone {
            if let Some(Thumb::Ready(img)) = self.thumbs.remove(&p) {
                let _ = window.drop_image(img);
            }
        }
        let fresh: Vec<PathBuf> = self
            .attachments
            .iter()
            .filter(|p| is_image_path(p) && !self.thumbs.contains_key(*p))
            .cloned()
            .collect();
        for path in fresh {
            self.thumbs.insert(path.clone(), Thumb::Loading);
            cx.spawn(async move |this, cx| {
                let p = path.clone();
                let thumb = cx
                    .background_executor()
                    .spawn(async move { crate::thumb::load_thumb(&p) })
                    .await;
                let _ = this.update(cx, |c, cx| {
                    // Removed while decoding: nothing to keep.
                    if c.attachments.contains(&path) {
                        c.thumbs
                            .insert(path, thumb.map_or(Thumb::Failed, Thumb::Ready));
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Thumbnails alive (memory regression checks).
    #[cfg(test)]
    pub(crate) fn thumb_count(&self) -> usize {
        self.thumbs.len()
    }

    #[cfg(test)]
    pub(crate) fn thumb_ready_count(&self) -> usize {
        self.thumbs
            .values()
            .filter(|t| matches!(t, Thumb::Ready(_)))
            .count()
    }

    pub fn add_attachments(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
        cx: &mut Context<Self>,
    ) {
        for p in paths {
            if !self.attachments.contains(&p) {
                self.attachments.push(p);
            }
        }
        cx.notify();
    }

    /// Files dropped from Explorer: attached to the message, and `@mentioned` in the text when
    /// they are files inside the project.
    pub fn add_dropped(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let root = self.state.project_root.clone();
        let mut mentions = String::new();
        for p in paths {
            if let Some(m) = root
                .as_deref()
                .filter(|_| p.is_file())
                .and_then(|r| crate::mentions::mention_for(r, p))
            {
                mentions.push_str(&m);
                mentions.push(' ');
            }
        }
        self.add_attachments(paths.iter().cloned(), cx);
        if !mentions.is_empty() {
            self.field.update(cx, |f, cx| f.insert(&mentions, cx));
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.field.read(cx).focus(window);
    }

    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    /// Opens a composer menu by name (`model`, `effort`, `permission`, `agent`): screenshots and
    /// demos drive the UI through this instead of synthetic clicks.
    pub fn open_popup_named(&mut self, name: &str, cx: &mut Context<Self>) {
        let p = match name {
            "model" => Popup::Model,
            "effort" => Popup::Effort,
            "permission" => Popup::Permission,
            "agent" => Popup::Agent,
            _ => return,
        };
        self.popup = Some(p);
        cx.notify();
    }

    pub fn close_popup(&mut self, cx: &mut Context<Self>) {
        if self.popup.take().is_some() {
            cx.notify();
        }
    }

    pub fn is_mention_open(&self) -> bool {
        self.mention.is_some()
    }

    /// The `@` picker: its entries and the highlighted one.
    #[cfg(test)]
    pub(crate) fn mention_state(&self) -> Option<(&[String], usize)> {
        self.mention
            .as_ref()
            .map(|m| (m.items.as_slice(), m.selected))
    }

    /// The answer to [`ComposerEvent::PickAgent`] for a thread: the handoff text, or `None` when
    /// there is nothing to hand over.
    pub fn set_handoff_preview(
        &mut self,
        agent: &AgentKind,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(Popup::Handoff { agent: a, preview }) = &mut self.popup
            && a == agent
        {
            *preview = match text {
                Some(t) => Preview::Text(t),
                None => Preview::Empty,
            };
            cx.notify();
        }
    }

    pub fn has_content(&self, cx: &App) -> bool {
        !self.field.read(cx).is_blank() || !self.attachments.is_empty()
    }

    pub fn button_mode(&self, cx: &App) -> ButtonMode {
        button_mode(self.state.working, self.has_content(cx))
    }

    // ----- actions -------------------------------------------------------------------------

    /// Sends what is typed (`Enter`, or the round button).
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.field.read(cx).text().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        let attachments = std::mem::take(&mut self.attachments);
        self.field.update(cx, |f, cx| {
            f.take_text(cx);
            f.set_capture_nav(false);
        });
        self.mention = None;
        self.popup = None;
        cx.emit(ComposerEvent::Send { text, attachments });
        cx.notify();
    }

    /// The round button.
    pub fn press_round_button(&mut self, cx: &mut Context<Self>) {
        match self.button_mode(cx) {
            ButtonMode::Stop => cx.emit(ComposerEvent::Stop),
            ButtonMode::Send => self.submit(cx),
            ButtonMode::Disabled => {}
        }
    }

    fn on_field(&mut self, ev: TextFieldEvent, cx: &mut Context<Self>) {
        match ev {
            TextFieldEvent::Changed => self.refresh_mention(cx),
            TextFieldEvent::Submit => {
                if self.mention.as_ref().is_some_and(|m| !m.items.is_empty()) {
                    self.accept_mention(None, cx);
                } else {
                    self.submit(cx);
                }
            }
            TextFieldEvent::Cancel => {
                if self.mention.is_some() || self.popup.is_some() {
                    self.mention = None;
                    self.popup = None;
                    self.field.update(cx, |f, _| f.set_capture_nav(false));
                    cx.notify();
                } else if self.state.working {
                    cx.emit(ComposerEvent::Stop);
                }
            }
            TextFieldEvent::Nav(nav) => {
                if let Some(m) = &mut self.mention {
                    let n = m.items.len();
                    if n > 0 {
                        match nav {
                            Nav::Up => m.selected = (m.selected + n - 1) % n,
                            Nav::Down => m.selected = (m.selected + 1) % n,
                            Nav::Tab => {
                                self.accept_mention(None, cx);
                                return;
                            }
                        }
                        cx.notify();
                    }
                }
            }
            TextFieldEvent::SubmitReverse | TextFieldEvent::Blur => {}
        }
    }

    /// Recomputes the `@` picker from the text and the caret.
    fn refresh_mention(&mut self, cx: &mut Context<Self>) {
        let (text, caret) = {
            let f = self.field.read(cx);
            (f.text().to_string(), f.caret())
        };
        let Some(q) = mention_at(&text, caret) else {
            self.close_mention(cx);
            return;
        };
        let Some(root) = self.state.project_root.clone() else {
            self.close_mention(cx);
            return;
        };
        let files = self.hub.read(cx).files_for(&root);
        let items = match files {
            Some(f) => filter_files(&f, &q.query, MENTION_ROWS),
            None => {
                self.hub.update(cx, |h, cx| h.load_files(root, cx));
                Vec::new()
            }
        };
        let selected = match &self.mention {
            Some(m) if m.query.range.start == q.range.start => {
                m.selected.min(items.len().saturating_sub(1))
            }
            _ => 0,
        };
        let capture = !items.is_empty();
        self.mention = Some(Mention {
            query: q,
            items,
            selected,
        });
        self.field.update(cx, |f, _| f.set_capture_nav(capture));
        cx.notify();
    }

    fn close_mention(&mut self, cx: &mut Context<Self>) {
        if self.mention.take().is_some() {
            self.field.update(cx, |f, _| f.set_capture_nav(false));
            cx.notify();
        }
    }

    /// Inserts the chosen path (`index`, or the highlighted one) in place of the `@query`.
    pub fn accept_mention(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(m) = self.mention.take() else { return };
        let ix = index.unwrap_or(m.selected);
        let Some(path) = m.items.get(ix) else { return };
        let insert = format!("@{path} ");
        self.field.update(cx, |f, cx| {
            f.set_capture_nav(false);
            f.replace_range(m.query.range.clone(), &insert, cx);
        });
        // `replace_range` re-ran the mention logic through `Changed`: the token is closed now.
        self.mention = None;
        cx.notify();
    }

    /// The `@` of the "mencionar archivo" menu entry.
    fn insert_at_sign(&mut self, cx: &mut Context<Self>) {
        self.popup = None;
        self.field.update(cx, |f, cx| f.insert("@", cx));
        cx.notify();
    }

    fn pick_files(&mut self, cx: &mut Context<Self>) {
        self.popup = None;
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |c, cx| c.add_attachments(paths, cx));
            }
        })
        .detach();
        cx.notify();
    }

    fn toggle_popup(&mut self, p: Popup, cx: &mut Context<Self>) {
        if p == Popup::Model {
            // The first time: ask the agent which models it has (no prompt is sent).
            let (agent, root) = (self.state.agent.clone(), self.state.project_root.clone());
            self.hub
                .update(cx, |h, cx| h.ensure_models(&agent, root, cx));
        }
        self.popup = if self.popup.as_ref().map(std::mem::discriminant)
            == Some(std::mem::discriminant(&p))
        {
            None
        } else {
            Some(p)
        };
        cx.notify();
    }

    fn pick_agent(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if kind == self.state.agent {
            self.popup = None;
        } else {
            self.popup = Some(Popup::Handoff {
                agent: kind.clone(),
                preview: Preview::Loading,
            });
            cx.emit(ComposerEvent::PickAgent(kind));
        }
        cx.notify();
    }

    fn confirm_switch(&mut self, agent: AgentKind, cx: &mut Context<Self>) {
        self.popup = None;
        cx.emit(ComposerEvent::ConfirmSwitch { agent, model: None });
        cx.notify();
    }

    /// Checks the clipboard for pasted images (raw image data or copied image files).
    /// Returns `true` when at least one image was attached so the caller stops paste propagation
    /// before `TextField::on_paste` runs; returns `false` for plain text so normal paste works.
    fn paste_images_from_clipboard(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(item) = cx.read_from_clipboard() {
            let mut saved = Vec::new();
            for entry in item.entries() {
                if let ClipboardEntry::Image(image) = entry
                    && !image.bytes.is_empty()
                {
                    let ext = format_ext(image.format);
                    match save_pasted_image(&image.bytes, ext) {
                        Ok(path) => saved.push(path),
                        Err(e) => log::warn!("composer: failed to save pasted image: {e}"),
                    }
                }
            }
            if !saved.is_empty() {
                self.add_attachments(saved, cx);
                return true;
            }
        }
        #[cfg(all(windows, not(test)))]
        if let Some(paths) = read_windows_clipboard_images()
            && !paths.is_empty()
        {
            self.add_attachments(paths, cx);
            return true;
        }
        false
    }
}

// ---------------------------------------------------------------------------------------------
// Clipboard image helpers
// ---------------------------------------------------------------------------------------------

static PASTE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Whether `path` has a common raster image extension.
pub fn is_image_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"
    )
}

fn format_ext(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Svg => "svg",
    }
}

pub(crate) fn save_pasted_image(bytes: &[u8], ext: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("selachi-paste");
    std::fs::create_dir_all(&dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let seq = PASTE_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("pasted-image-{ts}-{seq}.{ext}"));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

pub(crate) fn image_paths_from_drop(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|p| p.is_file() && is_image_path(p))
        .collect()
}

/// Converts a Windows `CF_DIB` / `CF_DIBV5` memory block (a BMP without its 14-byte file header)
/// into PNG bytes.
pub(crate) fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?);
    if (header_size as usize) < 40 || (header_size as usize) > dib.len() {
        return None;
    }
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    let clr_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);
    let color_table_entries = if clr_used > 0 {
        clr_used
    } else if bit_count <= 8 {
        1u32.checked_shl(bit_count as u32).unwrap_or(0)
    } else {
        0
    };
    let color_table_bytes = color_table_entries.checked_mul(4)?;
    // BITMAPINFOHEADER (40 bytes) with BI_BITFIELDS (3) is followed by 3 DWORD masks.
    let masks_bytes: u32 = if header_size == 40 && compression == 3 {
        12
    } else {
        0
    };
    let pixel_offset = 14u32
        .checked_add(header_size)?
        .checked_add(masks_bytes)?
        .checked_add(color_table_bytes)?;
    let file_size = 14u32.checked_add(u32::try_from(dib.len()).ok()?)?;

    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);

    let decoded = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp).ok()?;
    let mut png = Vec::new();
    decoded
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

#[cfg(all(windows, not(test)))]
fn read_windows_clipboard_images() -> Option<Vec<PathBuf>> {
    use std::sync::LazyLock;
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
        }
    }

    static FMT_PNG: LazyLock<u32> =
        LazyLock::new(|| unsafe { RegisterClipboardFormatW(windows::core::w!("PNG")) });
    static FMT_JFIF: LazyLock<u32> =
        LazyLock::new(|| unsafe { RegisterClipboardFormatW(windows::core::w!("JFIF")) });
    static FMT_GIF: LazyLock<u32> =
        LazyLock::new(|| unsafe { RegisterClipboardFormatW(windows::core::w!("GIF")) });

    unsafe { OpenClipboard(None) }.ok()?;
    let _guard = Guard;

    let read_bytes = |fmt: u32| -> Option<Vec<u8>> {
        if fmt == 0 || unsafe { IsClipboardFormatAvailable(fmt) }.is_err() {
            return None;
        }
        let handle = unsafe { GetClipboardData(fmt) }.ok()?;
        let global = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(global) };
        if size == 0 {
            return None;
        }
        let ptr = unsafe { GlobalLock(global) };
        if ptr.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) }.to_vec();
        let _ = unsafe { GlobalUnlock(global) };
        Some(bytes)
    };

    // 1. Copied image files from Explorer (CF_HDROP).
    if unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_ok()
        && let Ok(handle) = unsafe { GetClipboardData(CF_HDROP.0 as u32) }
    {
        let global = HGLOBAL(handle.0);
        let ptr = unsafe { GlobalLock(global) };
        if !ptr.is_null() {
            let hdrop = HDROP(ptr);
            let count = unsafe { DragQueryFileW(hdrop, 0xFFFF_FFFF, None) };
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
                if len == 0 {
                    continue;
                }
                let mut buf = vec![0u16; len + 1];
                let copied = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) } as usize;
                if copied > 0 {
                    paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..copied])));
                }
            }
            let _ = unsafe { GlobalUnlock(global) };
            let imgs = image_paths_from_drop(paths);
            if !imgs.is_empty() {
                return Some(imgs);
            }
        }
    }

    // 2. If plain text is on the clipboard, let TextField paste it instead of hijacking
    //    spreadsheets or rich selections that also publish a fallback bitmap.
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_ok() {
        return None;
    }

    // 3. Raw encoded image formats or DIB/DIBV5 screenshots (Win+Shift+S, PrintScreen, Paint).
    for (fmt, ext) in [(*FMT_PNG, "png"), (*FMT_JFIF, "jpg"), (*FMT_GIF, "gif")] {
        if let Some(bytes) = read_bytes(fmt)
            && let Ok(path) = save_pasted_image(&bytes, ext)
        {
            return Some(vec![path]);
        }
    }
    for fmt in [CF_DIBV5.0 as u32, CF_DIB.0 as u32] {
        if let Some(dib) = read_bytes(fmt)
            && let Some(png) = dib_to_png(&dib)
            && let Ok(path) = save_pasted_image(&png, "png")
        {
            return Some(vec![path]);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// `8% ▬ 1M` plus its tooltip, or `None` while the agent has not reported a window.
fn context_meter(theme: &Theme, ctx: Option<ContextUsage>, compact: bool) -> Option<AnyElement> {
    let ctx = ctx?;
    let c = theme.colors;
    let ty = theme.typography;
    let pct = if ctx.percent > 0.0 && ctx.percent < 1.0 {
        "<1%".to_string()
    } else {
        format!("{}%", ctx.percent.round() as u32)
    };
    let tip = format!(
        "Contexto: {} de {} tokens ({pct})",
        format_tokens(ctx.used_tokens),
        format_tokens(ctx.window_tokens)
    );
    let t = *theme;
    Some(
        div()
            .id("cmp-context")
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(6.0))
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_sm))
            .text_color(c.muted)
            .tooltip(Tooltip::text(&t, tip))
            .child(pct)
            .child(
                DitherMeter::new(theme)
                    .segments(6)
                    .value(Some((ctx.percent / 100.0).clamp(0.0, 1.0))),
            )
            .when(!compact, |d| d.child(format_window(ctx.window_tokens)))
            .into_any_element(),
    )
}

fn chevron(theme: &Theme) -> AnyElement {
    icon(Icon::ChevronDown, 14.0, theme.colors.muted).into_any_element()
}

impl Composer {
    fn plan_bar(&self, plan: &Plan, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let (done, total) = plan.progress();
        let current = plan
            .items
            .iter()
            .find(|i| i.state == TaskState::InProgress)
            .or_else(|| plan.items.iter().find(|i| i.state != TaskState::Done))
            .map(|i| i.text.clone());
        let open = self.plan_open;
        let header = div()
            .id("cmp-plan-toggle")
            .h(px(34.0))
            .px(px(14.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .text_size(px(ty.ui_size_md))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.plan_open = !this.plan_open;
                cx.notify();
            }))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.text)
                    .child(format!("{done}/{total}")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(c.muted)
                    .child(SharedString::from(match (&current, done == total) {
                        (_, true) => "Plan completado".to_string(),
                        (Some(cur), _) if !open => cur.clone(),
                        _ => "Plan".to_string(),
                    })),
            )
            .child(icon(
                if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                14.0,
                c.muted,
            ));
        let items = open.then(|| {
            div()
                .px(px(14.0))
                .pb(px(10.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .children(plan.items.iter().enumerate().map(|(i, item)| {
                    let glyph: AnyElement = match item.state {
                        TaskState::Done => {
                            icon(Icon::Check, 16.0, t.colors.success).into_any_element()
                        }
                        TaskState::InProgress => Spinner::new(c.accent).into_any_element(),
                        TaskState::Pending => div()
                            .size(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().size(px(8.0)).rounded_full().bg(c.surface_4))
                            .into_any_element(),
                    };
                    div()
                        .id(("plan-item", i))
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(px(10.0))
                        .text_size(px(ty.ui_size_md))
                        .text_color(if item.state == TaskState::Done {
                            c.muted
                        } else {
                            c.text
                        })
                        .child(glyph)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(SharedString::from(item.text.clone())),
                        )
                }))
        });
        div()
            .w_full()
            .rounded(px(Radius::MD))
            .bg(c.surface_1)
            .flex()
            .flex_col()
            .child(header)
            .children(items)
            .into_any_element()
    }

    fn mention_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.mention.as_ref()?;
        let t = self.theme;
        let c = t.colors;
        let body: AnyElement = if m.items.is_empty() {
            let loading = self
                .state
                .project_root
                .as_ref()
                .is_some_and(|r| self.hub.read(cx).files_for(r).is_none());
            div()
                .px(px(10.0))
                .py(px(8.0))
                .text_color(c.muted)
                .child(if loading {
                    "Buscando archivos…"
                } else {
                    "Sin coincidencias"
                })
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(m.items.iter().enumerate().map(|(i, path)| {
                    let name = file_name(path).to_string();
                    let dir = path
                        .strip_suffix(name.as_str())
                        .unwrap_or("")
                        .trim_end_matches('/')
                        .to_string();
                    let lead = crate::widgets::file_glyph(&name, c.muted);
                    let hint = (!dir.is_empty()).then(|| SharedString::from(dir));
                    menu_row(
                        SharedString::from(format!("mention-{path}")),
                        &t,
                        Some(lead),
                        name,
                        hint,
                        i == m.selected,
                        true,
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.accept_mention(Some(i), cx);
                        }),
                    )
                }))
                .into_any_element()
        };
        Some(
            div()
                .absolute()
                .bottom(relative(1.0))
                .left(px(0.0))
                .mb(px(8.0))
                .w(px(420.0))
                .max_w_full()
                .p(px(6.0))
                .rounded(px(Radius::SM))
                .bg(c.surface_2)
                .border_1()
                .border_color(c.outline_soft)
                .occlude()
                .child(body)
                .into_any_element(),
        )
    }

    fn permission_popover(&self, pill_open: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let mode = self.state.permission;
        let this = cx.entity();
        let mut pill = Pill::new("cmp-perm", &t)
            .label(permission_label(mode))
            .child(chevron(&t))
            .tooltip(if self.state.can_edit_permission {
                permission_hint(mode).to_string()
            } else {
                "No se puede cambiar mientras el agente trabaja".to_string()
            });
        if is_dangerous(mode) {
            pill = pill.icon(Icon::Alert);
        }
        if self.state.can_edit_permission {
            pill =
                pill.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.toggle_popup(Popup::Permission, cx)
                }));
        }
        Popover::new("cmp-perm-pop", &t)
            .open(pill_open && self.state.can_edit_permission)
            .placement(Placement::TopEnd)
            .width(300.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(pill)
            .children(PERMISSION_ORDER.iter().map(|m| {
                let m = *m;
                let lead = icon(
                    if is_dangerous(m) {
                        Icon::Alert
                    } else {
                        Icon::Check
                    },
                    16.0,
                    if is_dangerous(m) {
                        t.colors.warning
                    } else {
                        c.muted
                    },
                );
                menu_row(
                    ("perm", m as usize),
                    &t,
                    Some(lead.into_any_element()),
                    permission_label(m),
                    Some(permission_hint(m).into()),
                    m == mode,
                    true,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.popup = None;
                        if this.state.permission != m {
                            cx.emit(ComposerEvent::SetPermission(m));
                        }
                        cx.notify();
                    }),
                )
                .into_any_element()
            }))
            .into_any_element()
    }

    fn model_popover(&self, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let current = self.state.model.clone();
        let pill = Pill::new("cmp-model", &t)
            .label(model_label(current.as_deref()))
            .child(chevron(&t))
            .tooltip("Modelo")
            .on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Model, cx)),
            );
        let catalog = self.hub.read(cx).models_of(&self.state.agent).to_vec();
        let known = models_for(&self.state.agent, &catalog);
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            menu_row(
                "model-default",
                &t,
                None,
                "Predeterminado del agente",
                None,
                current.is_none(),
                true,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.popup = None;
                    cx.emit(ComposerEvent::SetModel(None));
                    cx.notify();
                }),
            )
            .into_any_element(),
        );
        for m in known.iter() {
            let id = m.id.clone();
            let selected = current.as_deref() == Some(id.as_str());
            rows.push(
                menu_row(
                    SharedString::from(format!("model-{}", m.id)),
                    &t,
                    None,
                    m.label.clone(),
                    Some(id.clone().into()),
                    selected,
                    true,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.popup = None;
                        cx.emit(ComposerEvent::SetModel(Some(id.clone())));
                        cx.notify();
                    }),
                )
                .into_any_element(),
            );
        }
        if let Some(cur) = current
            .as_deref()
            .filter(|m| !known.iter().any(|k| k.id == *m))
        {
            rows.push(
                menu_row(
                    "model-current",
                    &t,
                    None,
                    model_label(Some(cur)),
                    Some(cur.to_string().into()),
                    true,
                    false,
                    |_, _, _| {},
                )
                .into_any_element(),
            );
        }
        rows.push(
            div()
                .mt(px(4.0))
                .mx(px(4.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(Radius::PILL))
                .bg(c.surface_3)
                .child(self.model_field.clone())
                .into_any_element(),
        );
        Popover::new("cmp-model-pop", &t)
            .open(open)
            .placement(Placement::TopEnd)
            .width(260.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(pill)
            .children(rows)
            .into_any_element()
    }

    /// The effort pill (Bajo .. Máx). `None` (hidden) for agents without the knob.
    fn effort_popover(&self, open: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let catalog = self.hub.read(cx).models_of(&self.state.agent).to_vec();
        let levels = efforts_for(&self.state.agent, self.state.model.as_deref(), &catalog);
        if levels.is_empty() {
            return None;
        }
        let this = cx.entity();
        let current = self.state.effort.clone();
        let pill = Pill::new("cmp-effort", &t)
            .label(
                current
                    .as_deref()
                    .map_or_else(|| "Esfuerzo".to_string(), effort_label),
            )
            .child(chevron(&t))
            .tooltip("Esfuerzo de razonamiento")
            .on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Effort, cx)),
            );
        let mut rows: Vec<AnyElement> = vec![
            menu_row(
                "effort-default",
                &t,
                None,
                "Predeterminado del agente",
                None,
                current.is_none(),
                true,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.popup = None;
                    cx.emit(ComposerEvent::SetEffort(None));
                    cx.notify();
                }),
            )
            .into_any_element(),
        ];
        for (value, label) in levels {
            rows.push(
                menu_row(
                    SharedString::from(format!("effort-{value}")),
                    &t,
                    None,
                    label,
                    None,
                    current.as_deref() == Some(value),
                    true,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.popup = None;
                        cx.emit(ComposerEvent::SetEffort(Some(value.to_string())));
                        cx.notify();
                    }),
                )
                .into_any_element(),
            );
        }
        Some(
            Popover::new("cmp-effort-pop", &t)
                .open(open)
                .placement(Placement::TopEnd)
                .width(220.0)
                .padding(6.0)
                .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
                .trigger(pill)
                .children(rows)
                .into_any_element(),
        )
    }

    fn agent_popover(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let ty = self.theme.typography;
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let cur = self
            .state
            .agents
            .iter()
            .find(|a| a.kind == self.state.agent)
            .cloned()
            .unwrap_or_else(|| AgentOption::new(self.state.agent.clone()));
        let mut pill = Pill::new("cmp-agent", &t)
            .agent(cur.mark())
            .child(chevron(&t))
            .tooltip(format!("Agente: {}", cur.label))
            .on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Agent, cx)),
            );
        if !compact {
            pill = pill.label(cur.label);
        }
        let open = matches!(self.popup, Some(Popup::Agent) | Some(Popup::Handoff { .. }));
        let body: Vec<AnyElement> = match &self.popup {
            Some(Popup::Handoff { agent, preview }) => {
                let label = self
                    .state
                    .agents
                    .iter()
                    .find(|a| &a.kind == agent)
                    .map(|a| a.label.to_string())
                    .unwrap_or_else(|| chat_core::agent_label(agent));
                let text: AnyElement = match preview {
                    Preview::Loading => div()
                        .text_color(c.muted)
                        .child("Preparando el resumen…")
                        .into_any_element(),
                    Preview::Empty => div()
                        .text_color(c.muted)
                        .child("Todavía no hay conversación que traspasar.")
                        .into_any_element(),
                    Preview::Text(s) => div()
                        .id("handoff-text")
                        .max_h(px(220.0))
                        .overflow_y_scroll()
                        .p(px(10.0))
                        .rounded(px(Radius::PILL))
                        .bg(c.surface_3)
                        .text_size(px(ty.ui_size_sm))
                        .text_color(c.muted)
                        .child(SharedString::from(s.clone()))
                        .into_any_element(),
                };
                let target = agent.clone();
                let ready = !matches!(preview, Preview::Loading);
                vec![
                    div()
                        .px(px(6.0))
                        .pt(px(4.0))
                        .pb(px(8.0))
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("Cambiar a {label}")),
                        )
                        .child(
                            div()
                                .text_size(px(ty.ui_size_sm))
                                .text_color(c.muted)
                                .child(
                                    "El nuevo agente recibirá este resumen como primer mensaje:",
                                ),
                        )
                        .child(text)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap(px(8.0))
                                .child(Button::tonal("handoff-cancel", &t, "Cancelar").on_click(
                                    cx.listener(|this, _: &ClickEvent, _, cx| this.close_popup(cx)),
                                ))
                                .child(
                                    Button::primary("handoff-ok", &t, "Cambiar")
                                        .disabled(!ready)
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, _, cx| {
                                                this.confirm_switch(target.clone(), cx);
                                            },
                                        )),
                                ),
                        )
                        .into_any_element(),
                ]
            }
            _ => self
                .state
                .agents
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let kind = a.kind.clone();
                    let hint: Option<SharedString> = if !a.available {
                        Some("No instalado".into())
                    } else {
                        a.version.clone().map(Into::into)
                    };
                    menu_row(
                        ("agent", i),
                        &t,
                        Some(ui_kit::agent_icon(a.mark(), 16.0, c.muted).into_any_element()),
                        a.label.clone(),
                        hint,
                        a.kind == self.state.agent,
                        a.available,
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.pick_agent(kind.clone(), cx)
                        }),
                    )
                    .into_any_element()
                })
                .collect(),
        };
        Popover::new("cmp-agent-pop", &t)
            .open(open)
            .placement(Placement::TopEnd)
            .width(if matches!(self.popup, Some(Popup::Handoff { .. })) {
                380.0
            } else {
                240.0
            })
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(pill)
            .children(body)
            .into_any_element()
    }

    fn plus_popover(&self, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        let btn = IconButton::new("cmp-plus", &t, Icon::Plus)
            .size(32.0)
            .emphasize_on_hover(true)
            .tooltip("Adjuntar o mencionar archivos")
            .on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_popup(Popup::Plus, cx)),
            );
        Popover::new("cmp-plus-pop", &t)
            .open(open)
            .placement(Placement::TopStart)
            .width(260.0)
            .padding(6.0)
            .on_dismiss(move |_, cx| this.update(cx, |c, cx| c.close_popup(cx)))
            .trigger(btn)
            .child(menu_row(
                "plus-attach",
                &t,
                Some(icon(Icon::Files, 16.0, c.muted).into_any_element()),
                "Adjuntar archivos…",
                Some("Se envían como rutas en el mensaje".into()),
                false,
                true,
                cx.listener(|this, _: &ClickEvent, _, cx| this.pick_files(cx)),
            ))
            .child(menu_row(
                "plus-mention",
                &t,
                Some(icon(Icon::Search, 16.0, c.muted).into_any_element()),
                "Mencionar archivo",
                Some("Escribe @ y elige del proyecto".into()),
                false,
                self.state.project_root.is_some(),
                cx.listener(|this, _: &ClickEvent, _, cx| this.insert_at_sign(cx)),
            ))
            .into_any_element()
    }

    fn round_button(&self, mode: ButtonMode, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let (bg, fg) = match mode {
            ButtonMode::Disabled => (c.surface_3, c.muted),
            _ => (c.text, c.bg),
        };
        let inner: AnyElement = match mode {
            ButtonMode::Stop => icon(Icon::Stop, 16.0, fg).into_any_element(),
            _ => icon(Icon::Send, 16.0, fg).into_any_element(),
        };
        let tip = match mode {
            ButtonMode::Stop => "Detener",
            ButtonMode::Send => "Enviar (Enter)",
            ButtonMode::Disabled => "Escribe un mensaje",
        };
        div()
            .id("cmp-send")
            .occlude()
            .size(px(34.0))
            .flex_none()
            .rounded_full()
            .bg(bg)
            .flex()
            .items_center()
            .justify_center()
            .when(mode != ButtonMode::Disabled, |d| {
                d.cursor_pointer().hover(|s| s.opacity(0.88))
            })
            .tooltip(Tooltip::text(&t, tip))
            .child(inner)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.press_round_button(cx)))
            .into_any_element()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let compact = self.measured.narrow(window);
        let mode = self.button_mode(cx);
        let popup = self.popup.clone();
        self.sync_thumbs(window, cx);

        let chips = (!self.attachments.is_empty()).then(|| {
            div()
                .w_full()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .pb(px(8.0))
                .children(self.attachments.iter().map(|p| {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.display().to_string());
                    let close =
                        IconButton::new(format!("attach-x-{}", p.display()), &t, Icon::Close)
                            .size(18.0)
                            .icon_size(10.0)
                            .tooltip("Quitar")
                            .on_click({
                                let path = p.clone();
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.attachments.retain(|a| a != &path);
                                    cx.notify();
                                })
                            });
                    if is_image_path(p) {
                        div()
                            .id(SharedString::from(format!("attach-{}", p.display())))
                            .h(px(36.0))
                            .pl(px(4.0))
                            .pr(px(6.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .rounded(px(Radius::CONTROL))
                            .bg(c.surface_3)
                            .text_size(px(ty.ui_size_sm))
                            .text_color(c.text)
                            .tooltip(Tooltip::text(&t, p.display().to_string()))
                            .child(match self.thumbs.get(p) {
                                Some(Thumb::Ready(i)) => img(i.clone())
                                    .size(px(28.0))
                                    .rounded(px(Radius::CHIP))
                                    .object_fit(ObjectFit::Cover)
                                    .into_any_element(),
                                _ => div()
                                    .size(px(28.0))
                                    .rounded(px(Radius::CHIP))
                                    .bg(c.surface_2)
                                    .into_any_element(),
                            })
                            .child(
                                div()
                                    .max_w(px(180.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(name)),
                            )
                            .child(close)
                            .into_any_element()
                    } else {
                        file_chip(&t, &name, c.text)
                            .id(SharedString::from(format!("attach-{}", p.display())))
                            .tooltip(Tooltip::text(&t, p.display().to_string()))
                            .child(close)
                            .into_any_element()
                    }
                }))
        });

        let controls = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(self.plus_popover(matches!(popup, Some(Popup::Plus)), cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap(px(2.0))
                    .children(context_meter(&t, self.state.context, compact))
                    .child(self.permission_popover(matches!(popup, Some(Popup::Permission)), cx))
                    .child(self.model_popover(matches!(popup, Some(Popup::Model)), cx))
                    .children(self.effort_popover(matches!(popup, Some(Popup::Effort)), cx))
                    .child(self.agent_popover(compact, cx))
                    .child(div().w(px(6.0)).h(px(1.0)))
                    .child(self.round_button(mode, cx)),
            );

        let card = div()
            .relative()
            .w_full()
            .rounded(px(Radius::LG))
            .bg(c.surface_1)
            .px(px(16.0))
            .pt(px(14.0))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children(chips)
            .child(
                div()
                    .w_full()
                    .capture_action(cx.listener(|this, _: &text_act::Paste, _, cx| {
                        if this.paste_images_from_clipboard(cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .child(self.field.clone()),
            )
            .child(controls)
            .children(self.mention_overlay(cx))
            .child({
                let this = cx.entity();
                ui_kit::DropZone::new("composer-drop", &t, "Soltar para adjuntar")
                    .radius(Radius::LG)
                    .on_drop(move |paths, _, cx| this.update(cx, |c, cx| c.add_dropped(paths, cx)))
            });

        let path_line = (self.state.branch.is_some() || self.state.path.is_some()).then(|| {
            div()
                .w_full()
                .px(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .font_family(ty.ui_family)
                .text_size(px(ty.ui_size_sm))
                .text_color(c.muted)
                .when_some(self.state.branch.clone(), |d, b| {
                    d.child(icon(Icon::GitBranch, 14.0, c.muted))
                        .child(div().flex_none().child(SharedString::from(b)))
                })
                .when_some(self.state.path.clone(), |d, p| {
                    d.child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(tail_path(&p, 3))),
                    )
                })
        });

        let plan = self
            .state
            .plan
            .clone()
            .filter(|p| !p.items.is_empty())
            .map(|p| self.plan_bar(&p, cx));

        div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .font_family(ty.ui_family)
            .child(self.measured.probe())
            .children(plan)
            .child(card)
            .children(path_line)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use chat_core::ChatStore;
    use gpui::{ClipboardItem, Image, TestAppContext};

    use super::*;
    use crate::fake::{FactoryHandle, FakeFactory};

    fn tiny_png() -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 170, 255, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn tiny_dib_2x2_bgr24() -> Vec<u8> {
        // 40-byte BITMAPINFOHEADER + 2 rows of 2 BGR pixels (6 bytes + 2 bytes row padding each).
        let mut dib = Vec::with_capacity(40 + 16);
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&2i32.to_le_bytes()); // biWidth
        dib.extend_from_slice(&2i32.to_le_bytes()); // biHeight (bottom-up)
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
        dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
        dib.extend_from_slice(&16u32.to_le_bytes()); // biSizeImage
        dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
        dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
        // 2 rows: BGR = (255, 170, 0) -> RGB = (0, 170, 255), plus 2 padding bytes per row.
        for _ in 0..2 {
            dib.extend_from_slice(&[255, 170, 0, 255, 170, 0, 0, 0]);
        }
        dib
    }

    #[test]
    fn is_image_path_matches_image_extensions_case_insensitively() {
        for ok in [
            "a.png",
            "SHOT.PNG",
            "photo.jpg",
            "photo.JPEG",
            "anim.gif",
            "pic.webp",
            "raw.bmp",
            "scan.tif",
            "scan.TIFF",
        ] {
            assert!(is_image_path(Path::new(ok)), "expected image: {ok}");
        }
        for no in ["main.rs", "README.md", "notes.txt", "noext", ""] {
            assert!(!is_image_path(Path::new(no)), "expected non-image: {no}");
        }
    }

    #[test]
    fn dib_to_png_converts_valid_dib_and_rejects_truncated() {
        assert!(dib_to_png(&[]).is_none());
        assert!(dib_to_png(&[0u8; 20]).is_none());

        let dib = tiny_dib_2x2_bgr24();
        let png = dib_to_png(&dib).expect("valid 24-bit DIB should encode to PNG");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.get_pixel(0, 0).0, [0, 170, 255]);
    }

    #[test]
    fn image_paths_from_drop_filters_to_existing_image_files() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        let jpg = dir.path().join("pic.JPG");
        let rs = dir.path().join("lib.rs");
        let missing = dir.path().join("gone.png");
        let subdir = dir.path().join("folder.png");
        std::fs::write(&png, tiny_png()).unwrap();
        std::fs::write(&jpg, b"jpg").unwrap();
        std::fs::write(&rs, b"fn f() {}").unwrap();
        std::fs::create_dir(&subdir).unwrap();

        let kept = image_paths_from_drop(vec![png.clone(), rs, missing, subdir, jpg.clone()]);
        assert_eq!(kept, vec![png, jpg]);
    }

    fn setup_composer(
        cx: &mut TestAppContext,
    ) -> (
        tempfile::TempDir,
        Entity<Composer>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<Vec<ComposerEvent>>>,
    ) {
        cx.update(crate::init);
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::builder()
            .path(dir.path().join("chat.db"))
            .factory(Arc::new(FactoryHandle(FakeFactory::silent())))
            .open()
            .unwrap();
        let hub = cx.update(|cx| ChatHub::create(store, Theme::dark(), cx));
        let (c, vcx) = cx.add_window_view(move |_, cx| Composer::new(hub, Theme::dark(), cx));
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        let sub = vcx.update(|_, cx| {
            cx.subscribe(&c, move |_, ev: &ComposerEvent, _| {
                log.borrow_mut().push(ev.clone());
            })
        });
        std::mem::forget(sub);
        vcx.update(|window, cx| c.read(cx).focus(window, cx));
        (dir, c, vcx, events)
    }

    #[gpui::test]
    fn ctrl_v_with_clipboard_image_attaches_and_is_removable(cx: &mut TestAppContext) {
        use ui_kit::testing::{click_id, press};

        let (_dir, c, vcx, events) = setup_composer(cx);
        let png = tiny_png();
        vcx.update(|window, cx| {
            c.read(cx).focus(window, cx);
            cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
                ImageFormat::Png,
                png.clone(),
            )));
        });

        press(vcx, "ctrl-v");

        let attached = c.read_with(vcx, |c, _| c.attachments().to_vec());
        assert_eq!(attached.len(), 1, "expected 1 pasted image attachment");
        assert!(is_image_path(&attached[0]));
        assert_eq!(std::fs::read(&attached[0]).unwrap(), png);
        assert_eq!(
            c.read_with(vcx, |c, cx| c.text(cx)),
            "",
            "image paste must not insert text"
        );

        // Sending emits the attached image path, then clears attachments.
        press(vcx, "enter");
        assert_eq!(
            *events.borrow(),
            vec![ComposerEvent::Send {
                text: String::new(),
                attachments: attached.clone(),
            }]
        );
        assert!(c.read_with(vcx, |c, _| c.attachments().is_empty()));

        // Paste again and verify removal via the chip's close button.
        press(vcx, "ctrl-v");
        let second = c.read_with(vcx, |c, _| c.attachments().to_vec());
        assert_eq!(second.len(), 1);
        vcx.run_until_parked();
        click_id(vcx, &format!("attach-x-{}", second[0].display()));
        assert!(c.read_with(vcx, |c, _| c.attachments().is_empty()));

        let _ = std::fs::remove_file(&attached[0]);
        let _ = std::fs::remove_file(&second[0]);
    }

    #[gpui::test]
    fn ctrl_v_with_plain_text_still_pastes_text(cx: &mut TestAppContext) {
        use ui_kit::testing::press;

        let (_dir, c, vcx, _events) = setup_composer(cx);
        vcx.update(|window, cx| {
            c.read(cx).focus(window, cx);
            cx.write_to_clipboard(ClipboardItem::new_string("hola selachi".into()));
        });

        press(vcx, "ctrl-v");

        assert_eq!(c.read_with(vcx, |c, cx| c.text(cx)), "hola selachi");
        assert!(c.read_with(vcx, |c, _| c.attachments().is_empty()));
    }
}
