//! The selection model of a group of text segments and the container that drives it.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::{Rc, Weak};

use gpui::{
    App, Bounds, ClipboardItem, Div, ElementId, FocusHandle, Hitbox, HitboxBehavior, Hsla,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, SharedString, Stateful, Styled, Window, actions,
    canvas, div, point, px,
};
use theme::{Radius, Rgba, Theme};

use super::text::LaidRef;
use super::{CONTEXT, clamp_boundary, line_range, word_range};
use crate::focus::{FocusNext, FocusPrev};

actions!(
    ui_kit_selectable,
    [
        /// `Ctrl+C` inside a [`SelectionGroup::container`]: copy the selection.
        Copy,
        /// `Ctrl+A` inside a [`SelectionGroup::container`]: select every segment of the group.
        SelectAll,
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
    ]);
}

/// How a segment is joined to the previous one in the copied text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gap {
    /// A single `\n` (consecutive code lines, list items).
    #[default]
    Line,
    /// A blank line (`\n\n`): a new block (paragraph, heading, code block).
    Block,
    /// A tab: the next cell of a table row.
    Tab,
}

impl Gap {
    pub fn as_str(self) -> &'static str {
        match self {
            Gap::Line => "\n",
            Gap::Block => "\n\n",
            Gap::Tab => "\t",
        }
    }
}

/// The text of every segment of a group, by index (also those not on screen).
pub trait SegmentSource {
    fn len(&self) -> usize;
    fn text(&self, seg: usize) -> SharedString;
    /// How `seg` joins the one before it when copying.
    fn gap_before(&self, _seg: usize) -> Gap {
        Gap::Line
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An owned [`SegmentSource`]: built while rendering a message (`push` in document order).
#[derive(Clone, Debug, Default)]
pub struct Segments {
    items: Vec<(SharedString, Gap)>,
}

impl Segments {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a segment and returns its index (the `seg` of its [`super::SelectableText`]).
    pub fn push(&mut self, text: impl Into<SharedString>, gap: Gap) -> usize {
        self.items.push((text.into(), gap));
        self.items.len() - 1
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Every segment joined like a copy of the whole group.
    pub fn joined(&self) -> String {
        let mut out = String::new();
        for (i, (t, gap)) in self.items.iter().enumerate() {
            if i > 0 {
                out.push_str(gap.as_str());
            }
            out.push_str(t);
        }
        out
    }
}

impl SegmentSource for Segments {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn text(&self, seg: usize) -> SharedString {
        self.items
            .get(seg)
            .map(|(t, _)| t.clone())
            .unwrap_or_default()
    }

    fn gap_before(&self, seg: usize) -> Gap {
        self.items.get(seg).map_or(Gap::Line, |(_, g)| *g)
    }
}

/// A caret position: segment index and byte offset into its text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelPos {
    pub seg: usize,
    pub offset: usize,
}

impl SelPos {
    pub fn new(seg: usize, offset: usize) -> Self {
        Self { seg, offset }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LinkRef {
    seg: usize,
    ix: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Char,
    Word,
    Line,
}

/// One segment as painted in the last frame.
pub(crate) struct Entry {
    pub seg: usize,
    pub bounds: Bounds<Pixels>,
    pub laid: LaidRef,
    pub text: SharedString,
    pub links: Rc<[(Range<usize>, SharedString)]>,
}

type CopyFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
type LinkFn = Rc<dyn Fn(&str, &mut Window, &mut App)>;

struct State {
    source: Option<Rc<dyn SegmentSource>>,
    anchor: Option<SelPos>,
    head: Option<SelPos>,
    mode: Mode,
    base: Option<(SelPos, SelPos)>,
    dragging: bool,
    moved: bool,
    press_link: Option<LinkRef>,
    hover_link: Option<LinkRef>,
    entries: Vec<Entry>,
    color: Hsla,
    on_copy: Option<CopyFn>,
    on_link: Option<LinkFn>,
}

struct Inner {
    st: RefCell<State>,
    focus: FocusHandle,
}

thread_local! {
    /// The group that owns the live selection: pressing in another one clears it.
    static ACTIVE: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) };
}

/// The selection model of one message / panel. Cheap to clone (shared state).
#[derive(Clone)]
pub struct SelectionGroup(Rc<Inner>);

/// What a point resolves to.
struct Hit {
    pos: SelPos,
    link: Option<LinkRef>,
    inside: bool,
}

fn dist2(b: &Bounds<Pixels>, p: Point<Pixels>) -> f32 {
    let dx = (f32::from(b.left()) - f32::from(p.x))
        .max(f32::from(p.x) - f32::from(b.right()))
        .max(0.0);
    let dy = (f32::from(b.top()) - f32::from(p.y))
        .max(f32::from(p.y) - f32::from(b.bottom()))
        .max(0.0);
    dx * dx + dy * dy
}

/// Web and mail links only (a relative path is not something the system can open).
fn openable(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:")
}

impl SelectionGroup {
    /// A group that is a tab stop of its window (`Tab` reaches it, `Ctrl+C` / `Ctrl+A` work once
    /// focused).
    pub fn new(cx: &mut App) -> Self {
        Self::with_tab_stop(true, cx)
    }

    pub fn with_tab_stop(tab_stop: bool, cx: &mut App) -> Self {
        crate::ensure_init(cx);
        let accent = Theme::dark().colors.accent;
        Self(Rc::new(Inner {
            focus: cx.focus_handle().tab_index(0).tab_stop(tab_stop),
            st: RefCell::new(State {
                source: None,
                anchor: None,
                head: None,
                mode: Mode::Char,
                base: None,
                dragging: false,
                moved: false,
                press_link: None,
                hover_link: None,
                entries: Vec::new(),
                color: Rgba { a: 0.25, ..accent }.into(),
                on_copy: None,
                on_link: None,
            }),
        }))
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.0.focus
    }

    /// Replaces the text of the group (call it every render, after building the segments).
    pub fn set_segments(&self, segments: Segments) {
        self.set_source(Rc::new(segments));
    }

    pub fn set_source(&self, source: Rc<dyn SegmentSource>) {
        self.0.st.borrow_mut().source = Some(source);
    }

    /// Selection color (default: the dark theme accent at 25 %; [`Self::container`] sets the
    /// theme's).
    pub fn set_color(&self, color: impl Into<Hsla>) {
        self.0.st.borrow_mut().color = color.into();
    }

    pub(crate) fn color(&self) -> Hsla {
        self.0.st.borrow().color
    }

    /// Replaces what `Ctrl+C` does with the selected text (default: write it to the clipboard).
    /// A host that redacts secrets or shows a hint hooks in here.
    pub fn on_copy(&self, f: impl Fn(String, &mut Window, &mut App) + 'static) {
        self.0.st.borrow_mut().on_copy = Some(Rc::new(f));
    }

    /// Replaces what a plain click on a link does (default: `cx.open_url` for web and mail links).
    pub fn on_link(&self, f: impl Fn(&str, &mut Window, &mut App) + 'static) {
        self.0.st.borrow_mut().on_link = Some(Rc::new(f));
    }

    // ----- model -----------------------------------------------------------------------------

    /// The selection, ordered, or `None` when empty / collapsed.
    pub fn selection(&self) -> Option<(SelPos, SelPos)> {
        let st = self.0.st.borrow();
        let (a, h) = (st.anchor?, st.head?);
        (a != h).then(|| if a <= h { (a, h) } else { (h, a) })
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// Selects from `anchor` to `head` (any order). For galleries, demos and tests.
    pub fn set_selection(&self, anchor: SelPos, head: SelPos) {
        let mut st = self.0.st.borrow_mut();
        st.anchor = Some(anchor);
        st.head = Some(head);
    }

    pub fn clear(&self) {
        let mut st = self.0.st.borrow_mut();
        st.anchor = None;
        st.head = None;
        st.base = None;
        st.dragging = false;
    }

    /// Selects every segment.
    pub fn select_all(&self) {
        let (n, last_len) = {
            let st = self.0.st.borrow();
            match st.source.as_ref().filter(|s| !s.is_empty()) {
                Some(s) => (s.len(), s.text(s.len() - 1).len()),
                None => {
                    let last = st.entries.iter().max_by_key(|e| e.seg);
                    (
                        last.map_or(0, |e| e.seg + 1),
                        last.map_or(0, |e| e.text.len()),
                    )
                }
            }
        };
        if n > 0 {
            self.set_selection(SelPos::new(0, 0), SelPos::new(n - 1, last_len));
        }
    }

    fn seg_text(&self, st: &State, seg: usize) -> Option<SharedString> {
        st.entries
            .iter()
            .find(|e| e.seg == seg)
            .map(|e| e.text.clone())
            .or_else(|| {
                st.source
                    .as_ref()
                    .filter(|s| seg < s.len())
                    .map(|s| s.text(seg))
            })
    }

    /// The selected text, segments joined by their [`Gap`]. `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selection()?;
        let st = self.0.st.borrow();
        let mut out = String::new();
        for seg in s.seg..=e.seg {
            let text = self.seg_text(&st, seg).unwrap_or_default();
            let lo = if seg == s.seg {
                clamp_boundary(&text, s.offset)
            } else {
                0
            };
            let hi = if seg == e.seg {
                clamp_boundary(&text, e.offset)
            } else {
                text.len()
            };
            if seg > s.seg {
                let gap = st
                    .source
                    .as_ref()
                    .map_or(Gap::Line, |src| src.gap_before(seg));
                out.push_str(gap.as_str());
            }
            if lo < hi {
                out.push_str(&text[lo..hi]);
            }
        }
        Some(out)
    }

    /// Copies the selection (through [`Self::on_copy`] when set). `false` when nothing is selected.
    pub fn copy(&self, window: &mut Window, cx: &mut App) -> bool {
        let Some(text) = self.selected_text() else {
            return false;
        };
        self.copy_text(text, window, cx);
        true
    }

    /// Sends `text` where a copy of this group goes (the [`Self::on_copy`] hook, else the
    /// clipboard): for the "Copiar" buttons of a block, so they follow the same policy as `Ctrl+C`.
    pub fn copy_text(&self, text: String, window: &mut Window, cx: &mut App) {
        let hook = self.0.st.borrow().on_copy.clone();
        match hook {
            Some(f) => f(text, window, cx),
            None => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        }
    }

    /// The selected part of segment `seg` (`len` bytes long) and whether the selection continues
    /// past its end (into the next segment): what [`super::SelectableText`] paints.
    pub(crate) fn range_for(&self, seg: usize, len: usize) -> Option<(Range<usize>, bool)> {
        let (s, e) = self.selection()?;
        if seg < s.seg || seg > e.seg {
            return None;
        }
        let lo = if seg == s.seg { s.offset.min(len) } else { 0 };
        let hi = if seg == e.seg { e.offset.min(len) } else { len };
        Some((lo..hi.max(lo), seg < e.seg))
    }

    pub(crate) fn hover_link_of(&self, seg: usize) -> Option<usize> {
        self.0
            .st
            .borrow()
            .hover_link
            .filter(|l| l.seg == seg)
            .map(|l| l.ix)
    }

    pub(crate) fn register(&self, e: Entry) {
        self.0.st.borrow_mut().entries.push(e);
    }

    pub(crate) fn begin_frame(&self) {
        self.0.st.borrow_mut().entries.clear();
    }

    /// Where the caret `pos` is on screen (window coordinates), if its segment was painted.
    pub fn position_of(&self, pos: SelPos) -> Option<Point<Pixels>> {
        let st = self.0.st.borrow();
        let e = st.entries.iter().find(|e| e.seg == pos.seg)?;
        let laid = e.laid.borrow();
        let l = laid.as_ref()?;
        let rel = l.index_pos(pos.offset)?;
        Some(e.bounds.origin + rel)
    }

    /// Bounds of segment `seg` in the last frame.
    pub fn bounds_of(&self, seg: usize) -> Option<Bounds<Pixels>> {
        let st = self.0.st.borrow();
        st.entries.iter().find(|e| e.seg == seg).map(|e| e.bounds)
    }

    /// The selection quads of segment `seg` in the last frame (window coordinates): one per
    /// visual row. Empty when the segment is outside the selection or not painted.
    pub fn selection_rects(&self, seg: usize) -> Vec<Bounds<Pixels>> {
        let st = self.0.st.borrow();
        let Some(e) = st.entries.iter().find(|e| e.seg == seg) else {
            return Vec::new();
        };
        let Some((range, extend)) = self.range_for(seg, e.text.len()) else {
            return Vec::new();
        };
        let laid = e.laid.borrow();
        laid.as_ref().map_or_else(Vec::new, |l| {
            l.selection_rects(range, extend)
                .into_iter()
                .map(|r| Bounds::new(e.bounds.origin + r.origin, r.size))
                .collect()
        })
    }

    /// Segments painted in the last frame, ascending.
    pub fn painted_segments(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.0.st.borrow().entries.iter().map(|e| e.seg).collect();
        v.sort_unstable();
        v
    }

    // ----- hit testing -----------------------------------------------------------------------

    fn hit(&self, p: Point<Pixels>) -> Option<Hit> {
        let st = self.0.st.borrow();
        let mut best: Option<(&Entry, f32)> = None;
        for e in &st.entries {
            let d = dist2(&e.bounds, p);
            let better = match best {
                None => true,
                Some((b, bd)) => d < bd || (d == bd && e.seg < b.seg),
            };
            if better {
                best = Some((e, d));
            }
        }
        let (e, _) = best?;
        let inside = e.bounds.contains(&p);
        let eps = px(0.01);
        let q = point(
            p.x.min(e.bounds.right() - eps).max(e.bounds.left()),
            p.y.min(e.bounds.bottom() - eps).max(e.bounds.top()),
        );
        let rel = q - e.bounds.origin;
        let laid = e.laid.borrow();
        let l = laid.as_ref()?;
        let offset = clamp_boundary(&e.text, l.closest_index(rel));
        let link = if inside && !e.links.is_empty() {
            l.exact_index(rel).and_then(|i| {
                e.links
                    .iter()
                    .position(|(r, _)| r.contains(&i))
                    .map(|ix| LinkRef { seg: e.seg, ix })
            })
        } else {
            None
        };
        Some(Hit {
            pos: SelPos::new(e.seg, offset),
            link,
            inside,
        })
    }

    /// Expands `pos` by the click mode: the word or line around it.
    fn unit_at(&self, st: &State, pos: SelPos, mode: Mode) -> (SelPos, SelPos) {
        let text = self.seg_text(st, pos.seg).unwrap_or_default();
        let r = match mode {
            Mode::Word => word_range(&text, pos.offset),
            Mode::Line => line_range(&text, pos.offset),
            Mode::Char => pos.offset..pos.offset,
        };
        (SelPos::new(pos.seg, r.start), SelPos::new(pos.seg, r.end))
    }

    // ----- events ----------------------------------------------------------------------------

    fn mouse_down(&self, e: &MouseDownEvent, window: &mut Window) {
        // The selection of any other group ends here.
        ACTIVE.with(|a| {
            let prev = a.borrow().upgrade();
            if let Some(prev) = prev
                && !Rc::ptr_eq(&prev, &self.0)
            {
                SelectionGroup(prev).clear();
            }
            *a.borrow_mut() = Rc::downgrade(&self.0);
        });
        window.focus(&self.0.focus);
        let Some(hit) = self.hit(e.position) else {
            return;
        };
        {
            let mut st = self.0.st.borrow_mut();
            st.dragging = true;
            st.moved = false;
            st.press_link = hit.link;
            match e.click_count {
                0 | 1 => {
                    st.mode = Mode::Char;
                    st.base = None;
                    if e.modifiers.shift && st.anchor.is_some() {
                        st.head = Some(hit.pos);
                        st.moved = true;
                    } else {
                        st.anchor = Some(hit.pos);
                        st.head = Some(hit.pos);
                    }
                }
                n => {
                    let mode = if n == 2 { Mode::Word } else { Mode::Line };
                    let (a, h) = self.unit_at(&st, hit.pos, mode);
                    st.mode = mode;
                    st.base = Some((a, h));
                    st.anchor = Some(a);
                    st.head = Some(h);
                    st.moved = true;
                    st.press_link = None;
                }
            }
        }
        window.refresh();
    }

    fn drag_to(&self, p: Point<Pixels>) -> bool {
        let Some(hit) = self.hit(p) else {
            return false;
        };
        let mut st = self.0.st.borrow_mut();
        match (st.mode, st.base) {
            (Mode::Char, _) | (_, None) => {
                if st.head != Some(hit.pos) {
                    st.head = Some(hit.pos);
                    st.moved = st.moved || st.head != st.anchor;
                    return true;
                }
                false
            }
            (mode, Some((b0, b1))) => {
                let (u0, u1) = self.unit_at(&st, hit.pos, mode);
                let (anchor, head) = if hit.pos < b0 { (b1, u0) } else { (b0, u1) };
                let changed = st.anchor != Some(anchor) || st.head != Some(head);
                st.anchor = Some(anchor);
                st.head = Some(head);
                changed
            }
        }
    }

    fn mouse_move(&self, e: &MouseMoveEvent, hitbox: &Hitbox, window: &mut Window) {
        let dragging = self.0.st.borrow().dragging;
        if dragging {
            if e.pressed_button != Some(MouseButton::Left) {
                // The button was released outside the window.
                self.0.st.borrow_mut().dragging = false;
                return;
            }
            if self.drag_to(e.position) {
                window.refresh();
            }
            return;
        }
        let over = if hitbox.is_hovered(window) {
            self.hit(e.position).and_then(|h| h.link)
        } else {
            None
        };
        let changed = {
            let mut st = self.0.st.borrow_mut();
            let changed = st.hover_link != over;
            st.hover_link = over;
            changed
        };
        if changed {
            window.refresh();
        }
    }

    fn mouse_up(&self, e: &MouseUpEvent, window: &mut Window, cx: &mut App) {
        let (was, moved, pressed) = {
            let mut st = self.0.st.borrow_mut();
            let was = st.dragging;
            st.dragging = false;
            (was, st.moved, st.press_link.take())
        };
        if !was {
            return;
        }
        if !moved
            && e.click_count <= 1
            && let Some(press) = pressed
            && let Some(hit) = self.hit(e.position)
            && hit.inside
            && hit.link == Some(press)
        {
            let url = {
                let st = self.0.st.borrow();
                st.entries
                    .iter()
                    .find(|en| en.seg == press.seg)
                    .and_then(|en| en.links.get(press.ix))
                    .map(|(_, u)| u.clone())
            };
            if let Some(url) = url {
                let hook = self.0.st.borrow().on_link.clone();
                match hook {
                    Some(f) => f(&url, window, cx),
                    None if openable(&url) => cx.open_url(&url),
                    None => {}
                }
            }
        }
        window.refresh();
    }

    /// A press outside the container ends its selection (a collapsed caret needs no repaint).
    fn press_outside(&self, window: &mut Window) {
        if self.has_selection() {
            self.clear();
            window.refresh();
        }
    }

    /// The mouse handling of the group. Registered by [`Self::container`]; public for hosts that
    /// build their own wrapper (it must be the first child of a `relative` parent).
    pub fn handlers(&self) -> impl IntoElement {
        let g = self.clone();
        let g_paint = self.clone();
        canvas(
            move |bounds, window, _| {
                g.begin_frame();
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |_, hitbox: Hitbox, window, _| {
                let down = (g_paint.clone(), hitbox.clone());
                window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
                    if phase != gpui::DispatchPhase::Bubble || e.button != MouseButton::Left {
                        return;
                    }
                    if down.1.is_hovered(window) {
                        down.0.mouse_down(e, window);
                    } else {
                        down.0.press_outside(window);
                    }
                });
                let mv = (g_paint.clone(), hitbox);
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _| {
                    if phase == gpui::DispatchPhase::Capture {
                        mv.0.mouse_move(e, &mv.1, window);
                    }
                });
                let g_up = g_paint.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                    if phase == gpui::DispatchPhase::Capture && e.button == MouseButton::Left {
                        g_up.mouse_up(e, window, cx);
                    }
                });
            },
        )
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .size_full()
    }

    /// The wrapper to put the [`super::SelectableText`]s in: a keyboard-focusable, `relative` box
    /// with the mouse handling, `Ctrl+C` / `Ctrl+A`, `Tab` movement and the keyboard focus ring.
    /// Style it and add children as with any `div`.
    pub fn container(&self, id: impl Into<ElementId>, theme: &Theme) -> Stateful<Div> {
        self.set_color(Rgba {
            a: 0.25,
            ..theme.colors.accent
        });
        let id = id.into();
        let (copy, all) = (self.clone(), self.clone());
        div()
            .id(id.clone())
            .relative()
            .key_context(CONTEXT)
            .track_focus(&self.0.focus)
            .on_action(move |_: &Copy, window, cx| {
                if copy.copy(window, cx) {
                    cx.stop_propagation();
                }
            })
            .on_action(move |_: &SelectAll, window, cx| {
                all.select_all();
                window.refresh();
                cx.stop_propagation();
            })
            .on_action(|_: &FocusNext, window, _| window.focus_next())
            .on_action(|_: &FocusPrev, window, _| window.focus_prev())
            .child(self.handlers())
            .child(crate::focus::ring_element(
                self.0.focus.clone(),
                id.to_string(),
                Radius::SM,
                theme.colors.accent,
            ))
    }
}
