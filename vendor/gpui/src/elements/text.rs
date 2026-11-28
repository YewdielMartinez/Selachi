use crate::{
    ActiveTooltip, AnyView, App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId,
    HighlightStyle, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Size, TextOverflow,
    TextRun, TextStyle, TooltipId, WhiteSpace, Window, WrappedLine, WrappedLineLayout,
    px, register_tooltip_mouse_handlers, set_tooltip_on_window,
};
use anyhow::Context as _;
use smallvec::SmallVec;
use std::{
    cell::{Cell, RefCell},
    mem,
    ops::Range,
    rc::Rc,
    sync::Arc,
};
use util::ResultExt;

impl Element for &'static str {
    type RequestLayoutState = TextLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut state = TextLayout::default();
        let layout_id = state.layout(SharedString::from(*self), None, window, cx);
        (layout_id, state)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        text_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        text_layout.prepaint(bounds, self, window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        text_layout: &mut TextLayout,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        text_layout.paint(self, window, cx)
    }
}

impl IntoElement for &'static str {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl IntoElement for String {
    type Element = SharedString;

    fn into_element(self) -> Self::Element {
        self.into()
    }
}

impl Element for SharedString {
    type RequestLayoutState = TextLayout;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut state = TextLayout::default();
        let layout_id = state.layout(self.clone(), None, window, cx);
        (layout_id, state)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        text_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        text_layout.prepaint(bounds, self.as_ref(), window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        text_layout: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        text_layout.paint(self.as_ref(), window, cx)
    }
}

impl IntoElement for SharedString {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Renders text with runs of different styles.
///
/// Callers are responsible for setting the correct style for each run.
/// For text with a uniform style, you can usually avoid calling this constructor
/// and just pass text directly.
pub struct StyledText {
    text: SharedString,
    runs: Option<Vec<TextRun>>,
    delayed_highlights: Option<Vec<(Range<usize>, HighlightStyle)>>,
    layout: TextLayout,
}

impl StyledText {
    /// Construct a new styled text element from the given string.
    pub fn new(text: impl Into<SharedString>) -> Self {
        StyledText {
            text: text.into(),
            runs: None,
            delayed_highlights: None,
            layout: TextLayout::default(),
        }
    }

    /// Get the layout for this element. This can be used to map indices to pixels and vice versa.
    pub fn layout(&self) -> &TextLayout {
        &self.layout
    }

    /// Set the styling attributes for the given text, as well as
    /// as any ranges of text that have had their style customized.
    pub fn with_default_highlights(
        mut self,
        default_style: &TextStyle,
        highlights: impl IntoIterator<Item = (Range<usize>, HighlightStyle)>,
    ) -> Self {
        debug_assert!(
            self.delayed_highlights.is_none(),
            "Can't use `with_default_highlights` and `with_highlights`"
        );
        let runs = Self::compute_runs(&self.text, default_style, highlights);
        self.with_runs(runs)
    }

    /// Set the styling attributes for the given text, as well as
    /// as any ranges of text that have had their style customized.
    pub fn with_highlights(
        mut self,
        highlights: impl IntoIterator<Item = (Range<usize>, HighlightStyle)>,
    ) -> Self {
        debug_assert!(
            self.runs.is_none(),
            "Can't use `with_highlights` and `with_default_highlights`"
        );
        self.delayed_highlights = Some(
            highlights
                .into_iter()
                .inspect(|(run, _)| {
                    debug_assert!(self.text.is_char_boundary(run.start));
                    debug_assert!(self.text.is_char_boundary(run.end));
                })
                .collect::<Vec<_>>(),
        );
        self
    }

    fn compute_runs(
        text: &str,
        default_style: &TextStyle,
        highlights: impl IntoIterator<Item = (Range<usize>, HighlightStyle)>,
    ) -> Vec<TextRun> {
        let mut runs = Vec::new();
        let mut ix = 0;
        for (range, highlight) in highlights {
            if ix < range.start {
                debug_assert!(text.is_char_boundary(range.start));
                runs.push(default_style.clone().to_run(range.start - ix));
            }
            debug_assert!(text.is_char_boundary(range.end));
            runs.push(
                default_style
                    .clone()
                    .highlight(highlight)
                    .to_run(range.len()),
            );
            ix = range.end;
        }
        if ix < text.len() {
            runs.push(default_style.to_run(text.len() - ix));
        }
        runs
    }

    /// Set the text runs for this piece of text.
    pub fn with_runs(mut self, runs: Vec<TextRun>) -> Self {
        let mut text = &**self.text;
        for run in &runs {
            text = text.get(run.len..).expect("invalid text run");
        }
        assert!(text.is_empty(), "invalid text run");
        self.runs = Some(runs);
        self
    }
}

impl Element for StyledText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let runs = self.runs.take().or_else(|| {
            self.delayed_highlights.take().map(|delayed_highlights| {
                Self::compute_runs(&self.text, &window.text_style(), delayed_highlights)
            })
        });

        let layout_id = self.layout.layout(self.text.clone(), runs, window, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.layout.prepaint(bounds, &self.text, window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.layout.paint(&self.text, window, cx)
    }
}

impl IntoElement for StyledText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The Layout for TextElement. This can be used to map indices to pixels and vice versa.
#[derive(Default, Clone)]
pub struct TextLayout(Rc<RefCell<Option<TextLayoutInner>>>);

/// Tolerance, in pixels, when comparing the width a text was shaped for with the width its box
/// finally got (DPI rounding: a box 0.5 px narrower than the text must not wrap its last word).
const FIT_TOLERANCE: f32 = 1.0;

/// What a [`TextLayout`] needs to shape its text again (at the final width, in `prepaint`).
struct Shaping {
    text: SharedString,
    runs: Vec<TextRun>,
    font_size: Pixels,
    wraps: bool,
    line_clamp: Option<usize>,
    /// `Some(suffix)` when the text style truncates with an ellipsis.
    truncate_with: Option<SharedString>,
}

/// The inputs a measure call shapes with: wrap width and truncate width.
type ShapeKey = (Option<Pixels>, Option<Pixels>);

struct TextLayoutInner {
    len: usize,
    lines: SmallVec<[WrappedLine; 1]>,
    line_height: Pixels,
    /// Wrap width the `lines` were shaped with.
    wrap_width: Option<Pixels>,
    /// Truncate width the `lines` were shaped with.
    truncate_width: Option<Pixels>,
    /// The `lines` were cut short with an ellipsis.
    truncated: bool,
    /// Size of `lines`; `None` until something was shaped.
    size: Option<Size<Pixels>>,
    bounds: Option<Bounds<Pixels>>,
    shaping: Shaping,
    /// Answers given to earlier measure calls, by exact key. Taffy asks the same text for its
    /// min-content, max-content and several definite widths while it lays a flex row out; every
    /// query gets the size for *its* width, never the one shaped for the previous query.
    measures: SmallVec<[(ShapeKey, Size<Pixels>); 4]>,
}

struct Shaped {
    lines: SmallVec<[WrappedLine; 1]>,
    len: usize,
    size: Size<Pixels>,
    truncated: bool,
}

impl Shaping {
    fn shape(
        &self,
        wrap_width: Option<Pixels>,
        truncate_width: Option<Pixels>,
        line_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Shaped> {
        // `truncate_line` rewrites the runs: every call gets its own copy.
        let mut runs = self.runs.clone();
        let (text, truncated) = match (truncate_width, &self.truncate_with) {
            (Some(width), Some(suffix)) => {
                let mut line_wrapper = cx
                    .text_system()
                    .line_wrapper(window.text_style().font(), self.font_size);
                let cut = line_wrapper.truncate_line(self.text.clone(), width, suffix, &mut runs);
                let truncated = cut.len() != self.text.len();
                (cut, truncated)
            }
            _ => (self.text.clone(), false),
        };
        let len = text.len();
        let lines = window
            .text_system()
            .shape_text(
                text,
                self.font_size,
                &runs,
                wrap_width,      // Wrap if we know the width.
                self.line_clamp, // Limit the number of lines if line_clamp is set.
            )
            .log_err()?;
        let mut size: Size<Pixels> = Size::default();
        for line in &lines {
            let line_size = line.size(line_height);
            size.height += line_size.height;
            size.width = size.width.max(line_size.width).ceil();
        }
        Some(Shaped {
            lines,
            len,
            size,
            truncated,
        })
    }
}

impl TextLayoutInner {
    /// Width of the widest unbreakable word of the text, cached across frames (the text element
    /// is rebuilt every frame and taffy asks for this on every layout).
    fn min_content_width(&self, window: &mut Window, cx: &mut App) -> Pixels {
        let key = min_content::key(&self.shaping.text, self.shaping.font_size, &self.shaping.runs);
        if let Some(w) = min_content::get(key) {
            return w;
        }
        let Some(shaped) = self.shaping.shape(None, None, self.line_height, window, cx) else {
            return Pixels::ZERO;
        };
        let width = shaped
            .lines
            .iter()
            .map(|l| min_content::widest_word(&l.text, &l.unwrapped_layout))
            .fold(Pixels::ZERO, Pixels::max)
            .ceil();
        min_content::put(key, width);
        width
    }

    fn store(&mut self, key: ShapeKey, shaped: Shaped) {
        self.lines = shaped.lines;
        self.len = shaped.len;
        self.size = Some(shaped.size);
        self.truncated = shaped.truncated;
        self.wrap_width = key.0;
        self.truncate_width = key.1;
    }

    /// The key the text must be shaped with for a box `width` wide, when the shaping it holds does
    /// not already serve that box. `None`: keep the lines.
    fn key_for_final_width(&self, width: Pixels) -> Option<ShapeKey> {
        let wrap = self.shaping.wraps.then_some(width);
        let max_lines = self.shaping.line_clamp.map_or(1.0, |n| n as f32);
        let truncate = self
            .shaping
            .truncate_with
            .as_ref()
            .map(|_| width * max_lines);
        let fit = ShapeFit {
            wraps: self.shaping.wraps,
            truncates: self.shaping.truncate_with.is_some(),
            measured: self.size.is_some(),
            content_width: self
                .lines
                .iter()
                .map(|l| l.width())
                .fold(Pixels::ZERO, Pixels::max),
            widest_unwrapped: self
                .lines
                .iter()
                .map(|l| l.unwrapped_layout.width)
                .fold(Pixels::ZERO, Pixels::max),
            has_wraps: self.lines.iter().any(|l| !l.wrap_boundaries().is_empty()),
            wrap_width: self.wrap_width,
            truncate_width: self.truncate_width,
            truncated: self.truncated,
        };
        fit.is_stale_for(width, max_lines)
            .then_some((wrap, truncate))
    }
}

/// What a shaped text knows about itself, to decide whether it still serves the box it ended up
/// in. Plain data so the rule is testable without a window.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct ShapeFit {
    /// The text style wraps (`white-space: normal`).
    pub wraps: bool,
    /// The text style truncates with an ellipsis.
    pub truncates: bool,
    /// Something was shaped at all.
    pub measured: bool,
    /// Width the lines paint (`min(wrap width, text width)`, not rounded up).
    pub content_width: Pixels,
    /// Widest line before wrapping.
    pub widest_unwrapped: Pixels,
    /// Some line was soft-wrapped.
    pub has_wraps: bool,
    /// Wrap width the lines were shaped with.
    pub wrap_width: Option<Pixels>,
    /// Truncate width the lines were shaped with.
    pub truncate_width: Option<Pixels>,
    /// The lines were cut short with an ellipsis.
    pub truncated: bool,
}

impl ShapeFit {
    /// Must the text be shaped again for a box `width` wide?
    ///
    /// * painting wider than the box (past a 1 px tolerance: a box 0.5 px narrower than its text
    ///   keeps the text on one line) -> yes;
    /// * wrapped for a narrower width than the box now has -> yes (it would stay wrapped);
    /// * cut short with an ellipsis for a narrower width -> yes (it would hide text that fits);
    /// * nowrap text without truncation -> never (nothing could change).
    pub fn is_stale_for(&self, width: Pixels, max_lines: f32) -> bool {
        if !self.measured {
            return true;
        }
        if !self.wraps && !self.truncates {
            return false;
        }
        let tol = px(FIT_TOLERANCE);
        let stale_wrap = self.wraps
            && (self.content_width > width + tol
                || (self.has_wraps && self.wrap_width.is_none_or(|w| w < width - tol)));
        let truncate = width * max_lines;
        let stale_truncate = self.truncates
            && (self.widest_unwrapped > truncate + tol
                || (self.truncated && self.truncate_width.is_none_or(|w| w < truncate - tol)));
        stale_wrap || stale_truncate
    }
}

impl TextLayout {
    fn layout(
        &self,
        text: SharedString,
        runs: Option<Vec<TextRun>>,
        window: &mut Window,
        _: &mut App,
    ) -> LayoutId {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = text_style
            .line_height
            .to_pixels(font_size.into(), window.rem_size());

        let runs = if let Some(runs) = runs {
            runs
        } else {
            vec![text_style.to_run(text.len())]
        };

        let wraps = text_style.white_space == WhiteSpace::Normal;
        let line_clamp = text_style.line_clamp;
        let truncate_with = text_style
            .text_overflow
            .clone()
            .map(|TextOverflow::Truncate(s)| s);
        let len = text.len();
        *self.0.borrow_mut() = Some(TextLayoutInner {
            len,
            lines: SmallVec::new(),
            line_height,
            wrap_width: None,
            truncate_width: None,
            truncated: false,
            size: None,
            bounds: None,
            shaping: Shaping {
                text,
                runs,
                font_size,
                wraps,
                line_clamp,
                truncate_with,
            },
            measures: SmallVec::new(),
        });

        window.request_measured_layout(Default::default(), {
            let element_state = self.clone();

            move |known_dimensions, available_space, window, cx| {
                let mut guard = element_state.0.borrow_mut();
                let Some(inner) = guard.as_mut() else {
                    return Size::default();
                };
                // Min-content of wrapping text = its widest unbreakable word (CSS). Taffy uses it as
                // the automatic minimum size of a flex item, so answering max-content (upstream)
                // pushed items out of their row, and answering 0 collapsed pills, fields and error
                // texts into columns of one letter. Nothing is stored: the real shaping happens
                // for the definite width (a word wider than that box is broken then, not before).
                if inner.shaping.wraps
                    && known_dimensions.width.is_none()
                    && matches!(available_space.width, crate::AvailableSpace::MinContent)
                {
                    let width = inner.min_content_width(window, cx);
                    return Size {
                        width,
                        height: inner.line_height,
                    };
                }
                let wrap_width = if inner.shaping.wraps {
                    known_dimensions.width.or(match available_space.width {
                        crate::AvailableSpace::Definite(x) => Some(x),
                        _ => None,
                    })
                } else {
                    None
                };

                let truncate_width = if inner.shaping.truncate_with.is_some() {
                    known_dimensions.width.or(match available_space.width {
                        crate::AvailableSpace::Definite(x) => match inner.shaping.line_clamp {
                            Some(max_lines) => Some(x * max_lines),
                            None => Some(x),
                        },
                        _ => None,
                    })
                } else {
                    None
                };

                let key = (wrap_width, truncate_width);
                if let Some((_, size)) = inner.measures.iter().find(|(k, _)| *k == key) {
                    return *size;
                }

                let line_height = inner.line_height;
                let Some(shaped) =
                    inner
                        .shaping
                        .shape(wrap_width, truncate_width, line_height, window, cx)
                else {
                    inner.lines = SmallVec::new();
                    inner.len = 0;
                    inner.size = Some(Size::default());
                    inner.wrap_width = wrap_width;
                    inner.truncate_width = truncate_width;
                    inner.truncated = false;
                    return Size::default();
                };
                let size = shaped.size;
                inner.store(key, shaped);
                if inner.measures.len() == inner.measures.capacity() {
                    inner.measures.remove(0);
                }
                inner.measures.push((key, size));
                size
            }
        })
    }

    /// Records the final bounds. Taffy answers some layouts from its own cache, so the lines left
    /// by the last measure call can belong to another width than `bounds`: when they do not serve
    /// it (they would paint past the box, or stay wrapped for a narrower one), shape again for it.
    fn prepaint(&self, bounds: Bounds<Pixels>, text: &str, window: &mut Window, cx: &mut App) {
        let mut element_state = self.0.borrow_mut();
        let element_state = element_state
            .as_mut()
            .with_context(|| format!("measurement has not been performed on {text}"))
            .unwrap();
        element_state.bounds = Some(bounds);
        if bounds.size.width <= Pixels::ZERO {
            return;
        }
        if let Some(key) = element_state.key_for_final_width(bounds.size.width) {
            let line_height = element_state.line_height;
            if let Some(shaped) = element_state
                .shaping
                .shape(key.0, key.1, line_height, window, cx)
            {
                element_state.store(key, shaped);
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        text_probe::record(element_state, bounds);
    }

    fn paint(&self, text: &str, window: &mut Window, cx: &mut App) {
        let element_state = self.0.borrow();
        let element_state = element_state
            .as_ref()
            .with_context(|| format!("measurement has not been performed on {text}"))
            .unwrap();
        let bounds = element_state
            .bounds
            .with_context(|| format!("prepaint has not been performed on {text}"))
            .unwrap();

        let line_height = element_state.line_height;
        let mut line_origin = bounds.origin;
        let text_style = window.text_style();
        for line in &element_state.lines {
            line.paint_background(
                line_origin,
                line_height,
                text_style.text_align,
                Some(bounds),
                window,
                cx,
            )
            .log_err();
            line.paint(
                line_origin,
                line_height,
                text_style.text_align,
                Some(bounds),
                window,
                cx,
            )
            .log_err();
            line_origin.y += line.size(line_height).height;
        }
    }

    /// Get the byte index into the input of the pixel position.
    pub fn index_for_position(&self, mut position: Point<Pixels>) -> Result<usize, usize> {
        let element_state = self.0.borrow();
        let element_state = element_state
            .as_ref()
            .expect("measurement has not been performed");
        let bounds = element_state
            .bounds
            .expect("prepaint has not been performed");

        if position.y < bounds.top() {
            return Err(0);
        }

        let line_height = element_state.line_height;
        let mut line_origin = bounds.origin;
        let mut line_start_ix = 0;
        for line in &element_state.lines {
            let line_bottom = line_origin.y + line.size(line_height).height;
            if position.y > line_bottom {
                line_origin.y = line_bottom;
                line_start_ix += line.len() + 1;
            } else {
                let position_within_line = position - line_origin;
                match line.index_for_position(position_within_line, line_height) {
                    Ok(index_within_line) => return Ok(line_start_ix + index_within_line),
                    Err(index_within_line) => return Err(line_start_ix + index_within_line),
                }
            }
        }

        Err(line_start_ix.saturating_sub(1))
    }

    /// Get the pixel position for the given byte index.
    pub fn position_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        let element_state = self.0.borrow();
        let element_state = element_state
            .as_ref()
            .expect("measurement has not been performed");
        let bounds = element_state
            .bounds
            .expect("prepaint has not been performed");
        let line_height = element_state.line_height;

        let mut line_origin = bounds.origin;
        let mut line_start_ix = 0;

        for line in &element_state.lines {
            let line_end_ix = line_start_ix + line.len();
            if index < line_start_ix {
                break;
            } else if index > line_end_ix {
                line_origin.y += line.size(line_height).height;
                line_start_ix = line_end_ix + 1;
                continue;
            } else {
                let ix_within_line = index - line_start_ix;
                return Some(line_origin + line.position_for_index(ix_within_line, line_height)?);
            }
        }

        None
    }

    /// Retrieve the layout for the line containing the given byte index.
    pub fn line_layout_for_index(&self, index: usize) -> Option<Arc<WrappedLineLayout>> {
        let element_state = self.0.borrow();
        let element_state = element_state
            .as_ref()
            .expect("measurement has not been performed");
        let bounds = element_state
            .bounds
            .expect("prepaint has not been performed");
        let line_height = element_state.line_height;

        let mut line_origin = bounds.origin;
        let mut line_start_ix = 0;

        for line in &element_state.lines {
            let line_end_ix = line_start_ix + line.len();
            if index < line_start_ix {
                break;
            } else if index > line_end_ix {
                line_origin.y += line.size(line_height).height;
                line_start_ix = line_end_ix + 1;
                continue;
            } else {
                return Some(line.layout.clone());
            }
        }

        None
    }

    /// The bounds of this layout.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().as_ref().unwrap().bounds.unwrap()
    }

    /// The line height for this layout.
    pub fn line_height(&self) -> Pixels {
        self.0.borrow().as_ref().unwrap().line_height
    }

    /// The UTF-8 length of the underlying text.
    pub fn len(&self) -> usize {
        self.0.borrow().as_ref().unwrap().len
    }

    /// The text for this layout.
    pub fn text(&self) -> String {
        self.0
            .borrow()
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .map(|s| s.text.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The text for this layout (with soft-wraps as newlines)
    pub fn wrapped_text(&self) -> String {
        let mut lines = Vec::new();
        for wrapped in self.0.borrow().as_ref().unwrap().lines.iter() {
            let mut seen = 0;
            for boundary in wrapped.layout.wrap_boundaries.iter() {
                let index = wrapped.layout.unwrapped_layout.runs[boundary.run_ix].glyphs
                    [boundary.glyph_ix]
                    .index;

                lines.push(wrapped.text[seen..index].to_string());
                seen = index;
            }
            lines.push(wrapped.text[seen..].to_string());
        }

        lines.join("\n")
    }
}

/// A text element that can be interacted with.
pub struct InteractiveText {
    element_id: ElementId,
    text: StyledText,
    click_listener:
        Option<Box<dyn Fn(&[Range<usize>], InteractiveTextClickEvent, &mut Window, &mut App)>>,
    hover_listener: Option<Box<dyn Fn(Option<usize>, MouseMoveEvent, &mut Window, &mut App)>>,
    tooltip_builder: Option<Rc<dyn Fn(usize, &mut Window, &mut App) -> Option<AnyView>>>,
    tooltip_id: Option<TooltipId>,
    clickable_ranges: Vec<Range<usize>>,
}

struct InteractiveTextClickEvent {
    mouse_down_index: usize,
    mouse_up_index: usize,
}

#[doc(hidden)]
#[derive(Default)]
pub struct InteractiveTextState {
    mouse_down_index: Rc<Cell<Option<usize>>>,
    hovered_index: Rc<Cell<Option<usize>>>,
    active_tooltip: Rc<RefCell<Option<ActiveTooltip>>>,
}

/// InteractiveTest is a wrapper around StyledText that adds mouse interactions.
impl InteractiveText {
    /// Creates a new InteractiveText from the given text.
    pub fn new(id: impl Into<ElementId>, text: StyledText) -> Self {
        Self {
            element_id: id.into(),
            text,
            click_listener: None,
            hover_listener: None,
            tooltip_builder: None,
            tooltip_id: None,
            clickable_ranges: Vec::new(),
        }
    }

    /// on_click is called when the user clicks on one of the given ranges, passing the index of
    /// the clicked range.
    pub fn on_click(
        mut self,
        ranges: Vec<Range<usize>>,
        listener: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.click_listener = Some(Box::new(move |ranges, event, window, cx| {
            for (range_ix, range) in ranges.iter().enumerate() {
                if range.contains(&event.mouse_down_index) && range.contains(&event.mouse_up_index)
                {
                    listener(range_ix, window, cx);
                }
            }
        }));
        self.clickable_ranges = ranges;
        self
    }

    /// on_hover is called when the mouse moves over a character within the text, passing the
    /// index of the hovered character, or None if the mouse leaves the text.
    pub fn on_hover(
        mut self,
        listener: impl Fn(Option<usize>, MouseMoveEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover_listener = Some(Box::new(listener));
        self
    }

    /// tooltip lets you specify a tooltip for a given character index in the string.
    pub fn tooltip(
        mut self,
        builder: impl Fn(usize, &mut Window, &mut App) -> Option<AnyView> + 'static,
    ) -> Self {
        self.tooltip_builder = Some(Rc::new(builder));
        self
    }
}

impl Element for InteractiveText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.element_id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.text.request_layout(None, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        window.with_optional_element_state::<InteractiveTextState, _>(
            global_id,
            |interactive_state, window| {
                let mut interactive_state = interactive_state
                    .map(|interactive_state| interactive_state.unwrap_or_default());

                if let Some(interactive_state) = interactive_state.as_mut() {
                    if self.tooltip_builder.is_some() {
                        self.tooltip_id =
                            set_tooltip_on_window(&interactive_state.active_tooltip, window);
                    } else {
                        // If there is no longer a tooltip builder, remove the active tooltip.
                        interactive_state.active_tooltip.take();
                    }
                }

                self.text
                    .prepaint(None, inspector_id, bounds, state, window, cx);
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                (hitbox, interactive_state)
            },
        )
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let current_view = window.current_view();
        let text_layout = self.text.layout().clone();
        window.with_element_state::<InteractiveTextState, _>(
            global_id.unwrap(),
            |interactive_state, window| {
                let mut interactive_state = interactive_state.unwrap_or_default();
                if let Some(click_listener) = self.click_listener.take() {
                    let mouse_position = window.mouse_position();
                    if let Ok(ix) = text_layout.index_for_position(mouse_position)
                        && self
                            .clickable_ranges
                            .iter()
                            .any(|range| range.contains(&ix))
                    {
                        window.set_cursor_style(crate::CursorStyle::PointingHand, hitbox)
                    }

                    let text_layout = text_layout.clone();
                    let mouse_down = interactive_state.mouse_down_index.clone();
                    if let Some(mouse_down_index) = mouse_down.get() {
                        let hitbox = hitbox.clone();
                        let clickable_ranges = mem::take(&mut self.clickable_ranges);
                        window.on_mouse_event(
                            move |event: &MouseUpEvent, phase, window: &mut Window, cx| {
                                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                                    if let Ok(mouse_up_index) =
                                        text_layout.index_for_position(event.position)
                                    {
                                        click_listener(
                                            &clickable_ranges,
                                            InteractiveTextClickEvent {
                                                mouse_down_index,
                                                mouse_up_index,
                                            },
                                            window,
                                            cx,
                                        )
                                    }

                                    mouse_down.take();
                                    window.refresh();
                                }
                            },
                        );
                    } else {
                        let hitbox = hitbox.clone();
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, _| {
                            if phase == DispatchPhase::Bubble
                                && hitbox.is_hovered(window)
                                && let Ok(mouse_down_index) =
                                    text_layout.index_for_position(event.position)
                            {
                                mouse_down.set(Some(mouse_down_index));
                                window.refresh();
                            }
                        });
                    }
                }

                window.on_mouse_event({
                    let mut hover_listener = self.hover_listener.take();
                    let hitbox = hitbox.clone();
                    let text_layout = text_layout.clone();
                    let hovered_index = interactive_state.hovered_index.clone();
                    move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                            let current = hovered_index.get();
                            let updated = text_layout.index_for_position(event.position).ok();
                            if current != updated {
                                hovered_index.set(updated);
                                if let Some(hover_listener) = hover_listener.as_ref() {
                                    hover_listener(updated, event.clone(), window, cx);
                                }
                                cx.notify(current_view);
                            }
                        }
                    }
                });

                if let Some(tooltip_builder) = self.tooltip_builder.clone() {
                    let active_tooltip = interactive_state.active_tooltip.clone();
                    let build_tooltip = Rc::new({
                        let tooltip_is_hoverable = false;
                        let text_layout = text_layout.clone();
                        move |window: &mut Window, cx: &mut App| {
                            text_layout
                                .index_for_position(window.mouse_position())
                                .ok()
                                .and_then(|position| tooltip_builder(position, window, cx))
                                .map(|view| (view, tooltip_is_hoverable))
                        }
                    });

                    // Use bounds instead of testing hitbox since this is called during prepaint.
                    let check_is_hovered_during_prepaint = Rc::new({
                        let source_bounds = hitbox.bounds;
                        let text_layout = text_layout.clone();
                        let pending_mouse_down = interactive_state.mouse_down_index.clone();
                        move |window: &Window| {
                            text_layout
                                .index_for_position(window.mouse_position())
                                .is_ok()
                                && source_bounds.contains(&window.mouse_position())
                                && pending_mouse_down.get().is_none()
                        }
                    });

                    let check_is_hovered = Rc::new({
                        let hitbox = hitbox.clone();
                        let text_layout = text_layout.clone();
                        let pending_mouse_down = interactive_state.mouse_down_index.clone();
                        move |window: &Window| {
                            text_layout
                                .index_for_position(window.mouse_position())
                                .is_ok()
                                && hitbox.is_hovered(window)
                                && pending_mouse_down.get().is_none()
                        }
                    });

                    register_tooltip_mouse_handlers(
                        &active_tooltip,
                        self.tooltip_id,
                        build_tooltip,
                        check_is_hovered,
                        check_is_hovered_during_prepaint,
                        window,
                    );
                }

                self.text
                    .paint(None, inspector_id, bounds, &mut (), &mut (), window, cx);

                ((), interactive_state)
            },
        );
    }
}

impl IntoElement for InteractiveText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}


/// Min-content width of wrapping text: the widest run of characters the line wrapper cannot break.
pub(crate) mod min_content {
    use crate::{LineLayout, Pixels, TextRun, text_system::LineWrapper};
    use std::{
        cell::RefCell,
        collections::HashMap,
        hash::{Hash, Hasher},
        ops::Range,
    };

    /// Cached widths are tiny (a key and a float); the cache is emptied when it grows past this.
    const CACHE_LIMIT: usize = 4096;

    thread_local! {
        static CACHE: RefCell<HashMap<u64, Pixels>> = RefCell::new(HashMap::new());
    }

    /// Key of a text with its style: text, font size and the fonts / lengths of its runs.
    pub(crate) fn key(text: &str, font_size: Pixels, runs: &[TextRun]) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        font_size.0.to_bits().hash(&mut h);
        for run in runs {
            run.len.hash(&mut h);
            run.font.hash(&mut h);
        }
        h.finish()
    }

    pub(crate) fn get(key: u64) -> Option<Pixels> {
        CACHE.with(|c| c.borrow().get(&key).copied())
    }

    pub(crate) fn put(key: u64, width: Pixels) {
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() >= CACHE_LIMIT {
                c.clear();
            }
            c.insert(key, width);
        });
    }

    /// The byte ranges between the places where the line wrapper may break `text` (the same rule
    /// as `LineWrapper::wrap_line`: after a space that precedes a word, and before a character
    /// that is not a word character, never before the first non-space). Trailing spaces belong to
    /// the segment they follow.
    pub(crate) fn segments(text: &str) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut seen = false;
        let mut prev = '\0';
        for (ix, c) in text.char_indices() {
            if c == '\n' {
                continue;
            }
            let candidate = seen
                && if LineWrapper::is_word_char(c) {
                    prev == ' ' && c != ' '
                } else {
                    c != ' '
                };
            if candidate && ix > start {
                out.push(start..ix);
                start = ix;
            }
            if c != ' ' {
                seen = true;
            }
            prev = c;
        }
        if start < text.len() || out.is_empty() {
            out.push(start..text.len());
        }
        out
    }

    /// Width of the widest [`segments`] of an already shaped, unwrapped line.
    pub(crate) fn widest_word(text: &str, layout: &LineLayout) -> Pixels {
        let segs = segments(text);
        let mut bounds: Vec<usize> = segs.iter().map(|s| s.start).collect();
        bounds.push(text.len());
        // x of the first glyph at or after each boundary, in one pass over the glyphs.
        let mut xs: Vec<Pixels> = Vec::with_capacity(bounds.len());
        'glyphs: for run in &layout.runs {
            for glyph in &run.glyphs {
                while xs.len() < bounds.len() && glyph.index >= bounds[xs.len()] {
                    xs.push(glyph.position.x);
                }
                if xs.len() == bounds.len() {
                    break 'glyphs;
                }
            }
        }
        while xs.len() < bounds.len() {
            xs.push(layout.width);
        }
        // the last boundary is the end of the text: the width of the line
        let last = xs.len() - 1;
        xs[last] = layout.width;
        xs.windows(2)
            .map(|w| w[1] - w[0])
            .fold(Pixels::ZERO, Pixels::max)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn words(text: &str) -> Vec<&str> {
            segments(text).into_iter().map(|r| &text[r]).collect()
        }

        #[test]
        fn words_keep_their_trailing_space() {
            assert_eq!(
                words("Escribe un número entero"),
                ["Escribe ", "un ", "número ", "entero"]
            );
        }

        #[test]
        fn a_single_word_is_one_segment() {
            assert_eq!(words("Próximamente"), ["Próximamente"]);
            assert_eq!(words(""), [""]);
        }

        #[test]
        fn leading_spaces_do_not_open_a_break() {
            assert_eq!(words("  ab cd"), ["  ab ", "cd"]);
        }

        #[test]
        fn word_characters_glue_hyphens_and_paths() {
            // `-`, `_`, `.`, `:` and `=` are word characters for the wrapper
            assert_eq!(words("pre-commit a_b.c"), ["pre-commit ", "a_b.c"]);
            assert_eq!(words("--login -i"), ["--login ", "-i"]);
        }

        #[test]
        fn a_non_word_character_may_start_a_line() {
            // the wrapper breaks before `/` and `(`: the word before it is a segment of its own
            assert_eq!(words("src/lib"), ["src", "/lib"]);
        }
    }
}

/// Test support: what every text element of a window painted, to assert layouts without pixels.
///
/// ```ignore
/// gpui::text_probe::start();
/// /* draw */
/// for t in gpui::text_probe::take() { assert!(t.painted_width <= t.bounds.size.width + px(1.0)); }
/// ```
#[cfg(any(test, feature = "test-support"))]
pub mod text_probe {
    use super::TextLayoutInner;
    use crate::{Bounds, Pixels};
    use std::cell::RefCell;

    /// One painted text element.
    #[derive(Clone, Debug)]
    pub struct TextProbe {
        /// The text that was shaped (truncated text included, with its ellipsis).
        pub text: String,
        /// The box the element got.
        pub bounds: Bounds<Pixels>,
        /// Painted rows, soft wraps included.
        pub rows: usize,
        /// Width of the widest painted row.
        pub painted_width: Pixels,
        /// A soft wrap fell inside a word (between two non-space characters): the text was
        /// squeezed into a box narrower than its longest word.
        pub broke_a_word: bool,
    }

    thread_local! {
        static PROBE: RefCell<Option<Vec<TextProbe>>> = const { RefCell::new(None) };
    }

    /// Starts (or restarts) recording on this thread.
    pub fn start() {
        PROBE.with(|p| *p.borrow_mut() = Some(Vec::new()));
    }

    /// Stops recording and returns what was recorded since [`start`].
    pub fn take() -> Vec<TextProbe> {
        PROBE.with(|p| p.borrow_mut().take().unwrap_or_default())
    }

    pub(super) fn record(inner: &TextLayoutInner, bounds: Bounds<Pixels>) {
        PROBE.with(|p| {
            if let Some(v) = p.borrow_mut().as_mut() {
                v.push(TextProbe {
                    text: inner
                        .lines
                        .iter()
                        .map(|l| l.text.to_string())
                        .collect::<Vec<_>>()
                        .join("
"),
                    bounds,
                    rows: inner
                        .lines
                        .iter()
                        .map(|l| l.wrap_boundaries().len() + 1)
                        .sum(),
                    painted_width: inner
                        .lines
                        .iter()
                        .map(|l| l.width())
                        .fold(Pixels::ZERO, Pixels::max),
                    broke_a_word: inner.lines.iter().any(|l| {
                        l.wrap_boundaries().iter().any(|b| {
                            let ix = l.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix].index;
                            l.text.is_char_boundary(ix)
                                && matches!(
                                    (l.text[..ix].chars().next_back(), l.text[ix..].chars().next()),
                                    (Some(a), Some(b)) if a.is_alphanumeric() && b.is_alphanumeric()
                                )
                        })
                    }),
                });
            }
        });
    }
}

/// The unbreakable segments of `text` as the line wrapper sees them (tests of the hosts).
#[doc(hidden)]
pub fn min_content_segments(text: &str) -> Vec<Range<usize>> {
    min_content::segments(text)
}
