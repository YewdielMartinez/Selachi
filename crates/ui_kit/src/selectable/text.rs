//! [`SelectableText`]: a text element like gpui's `StyledText` that also exposes its glyph layout
//! to a [`SelectionGroup`] (hit-testing) and paints the selection behind its glyphs.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AbsoluteLength, App, AvailableSpace, Bounds, CursorStyle, Element, ElementId, GlobalElementId,
    HighlightStyle, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, Pixels,
    Point, SharedString, Size, TextRun, WhiteSpace, Window, WrappedLine, fill, point, px, size,
};

use super::group::{Entry, SelectionGroup};

/// Width of the stub painted where a selection includes a line break.
const NEWLINE_STUB: f32 = 4.0;

/// The shaped lines of one element in the last frame.
pub(crate) struct Laid {
    lines: Vec<WrappedLine>,
    line_height: Pixels,
    wrap_width: Option<Pixels>,
    size: Option<Size<Pixels>>,
    bounds: Option<Bounds<Pixels>>,
    len: usize,
}

pub(crate) type LaidRef = Rc<RefCell<Option<Laid>>>;

impl Laid {
    /// Byte index of the glyph that starts wrapped row `r - 1` of `line` (`r >= 1`).
    fn boundary_index(line: &WrappedLine, r: usize) -> usize {
        let b = line.wrap_boundaries[r - 1];
        line.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix].index
    }

    /// The character boundary closest to `rel` (relative to the element's origin).
    pub(crate) fn closest_index(&self, rel: Point<Pixels>) -> usize {
        self.index_at(rel, true).unwrap_or(0)
    }

    /// The glyph under `rel`, if the point is over text.
    pub(crate) fn exact_index(&self, rel: Point<Pixels>) -> Option<usize> {
        self.index_at(rel, false)
    }

    fn index_at(&self, rel: Point<Pixels>, closest: bool) -> Option<usize> {
        let mut top = px(0.0);
        let mut start = 0;
        let n = self.lines.len();
        for (i, line) in self.lines.iter().enumerate() {
            let h = line.size(self.line_height).height;
            if rel.y < top + h || i + 1 == n {
                let local = point(
                    rel.x,
                    (rel.y - top).max(px(0.0)).min((h - px(0.01)).max(px(0.0))),
                );
                let r = if closest {
                    line.closest_index_for_position(local, self.line_height)
                } else {
                    line.index_for_position(local, self.line_height)
                };
                return match r {
                    Ok(ix) => Some(start + ix.min(line.len())),
                    Err(ix) if closest => Some(start + ix.min(line.len())),
                    Err(_) => None,
                };
            }
            top += h;
            start += line.len() + 1;
        }
        None
    }

    /// Where byte `index` is, relative to the element's origin (top-left of its glyph).
    pub(crate) fn index_pos(&self, index: usize) -> Option<Point<Pixels>> {
        let mut top = px(0.0);
        let mut start = 0;
        for line in &self.lines {
            let end = start + line.len();
            if index <= end {
                let p = line.position_for_index(index - start, self.line_height)?;
                return Some(point(p.x, top + p.y));
            }
            top += line.size(self.line_height).height;
            start = end + 1;
        }
        None
    }

    /// The quads of the selection `range` (relative to the element's origin). `extend`: the
    /// selection goes on into the next segment, so the end of the last row gets a line-break stub.
    pub(crate) fn selection_rects(&self, range: Range<usize>, extend: bool) -> Vec<Bounds<Pixels>> {
        let mut out = Vec::new();
        let mut top = px(0.0);
        let mut start = 0;
        let n = self.lines.len();
        for (li, line) in self.lines.iter().enumerate() {
            let line_end = start + line.len();
            let rows = line.wrap_boundaries.len();
            let last_line = li + 1 == n;
            for r in 0..=rows {
                let rs = if r == 0 {
                    0
                } else {
                    Self::boundary_index(line, r)
                };
                let re = if r < rows {
                    Self::boundary_index(line, r + 1)
                } else {
                    line.len()
                };
                let (abs_s, abs_e) = (start + rs, start + re);
                let lo = range.start.clamp(abs_s, abs_e);
                let hi = range.end.clamp(abs_s, abs_e);
                let stub = r == rows
                    && range.start <= line_end
                    && if last_line {
                        extend
                    } else {
                        range.end > line_end
                    };
                if hi > lo || stub {
                    let x0 = line.unwrapped_layout.x_for_index(rs);
                    let left = line.unwrapped_layout.x_for_index(lo - start) - x0;
                    let right = line.unwrapped_layout.x_for_index(hi - start) - x0;
                    let width =
                        (right - left).max(px(0.0)) + if stub { px(NEWLINE_STUB) } else { px(0.0) };
                    out.push(Bounds::new(point(left, top), size(width, self.line_height)));
                }
                top += self.line_height;
            }
            start = line_end + 1;
        }
        out
    }
}

/// A run of selectable text: `StyledText` plus selection. Put it inside
/// [`SelectionGroup::container`]. `seg` is its index in the group's segments (document order).
pub struct SelectableText {
    group: SelectionGroup,
    seg: usize,
    text: SharedString,
    runs: Option<Vec<TextRun>>,
    highlights: Option<Vec<(Range<usize>, HighlightStyle)>>,
    links: Rc<[(Range<usize>, SharedString)]>,
    laid: LaidRef,
    /// What `request_layout` shaped with (runs, font size, line height, wraps), kept so `prepaint`
    /// can reshape to the final width: taffy caches measures, so the last measure call is not
    /// always the one the final layout used (a short message could paint unwrapped past its
    /// bubble).
    shaping: Option<(Vec<TextRun>, Pixels, Pixels, bool)>,
}

impl SelectableText {
    pub fn new(group: &SelectionGroup, seg: usize, text: impl Into<SharedString>) -> Self {
        Self {
            group: group.clone(),
            seg,
            text: text.into(),
            runs: None,
            highlights: None,
            links: Rc::from(Vec::new()),
            laid: Rc::new(RefCell::new(None)),
            shaping: None,
        }
    }

    /// Full text runs (use when fonts differ inside the text).
    pub fn with_runs(mut self, runs: Vec<TextRun>) -> Self {
        self.runs = Some(runs);
        self
    }

    /// Styled ranges over the inherited text style (sorted, not overlapping).
    pub fn with_highlights(
        mut self,
        highlights: impl IntoIterator<Item = (Range<usize>, HighlightStyle)>,
    ) -> Self {
        self.highlights = Some(highlights.into_iter().collect());
        self
    }

    /// Clickable ranges `(range, url)`.
    pub fn links<U: Into<SharedString>>(
        mut self,
        links: impl IntoIterator<Item = (Range<usize>, U)>,
    ) -> Self {
        self.links = links.into_iter().map(|(r, u)| (r, u.into())).collect();
        self
    }
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn compute_runs(
    text: &str,
    default_style: &gpui::TextStyle,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut ix = 0;
    for (range, highlight) in highlights {
        if range.start < ix || range.end > text.len() {
            continue;
        }
        if ix < range.start {
            runs.push(default_style.to_run(range.start - ix));
        }
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

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

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
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = text_style
            .line_height
            .to_pixels(AbsoluteLength::from(font_size), window.rem_size());
        let runs = match (self.runs.take(), self.highlights.take()) {
            (Some(runs), _) => runs,
            (None, Some(h)) => compute_runs(&self.text, &text_style, h),
            (None, None) => vec![text_style.to_run(self.text.len())],
        };
        let text = self.text.clone();
        let laid = self.laid.clone();
        let wraps = text_style.white_space == WhiteSpace::Normal;
        self.shaping = Some((runs.clone(), font_size, line_height, wraps));
        let layout_id = window.request_measured_layout(
            Default::default(),
            move |known, available, window, _| {
                // Wrapping text can break down to any width when a flex parent asks for
                // `MinContent` (for `min-width: auto`); returning max-content here forces flex
                // bubbles wider than their column. Do not overwrite `laid` so cached definite
                // shaping survives.
                if wraps
                    && known.width.is_none()
                    && matches!(available.width, AvailableSpace::MinContent)
                {
                    return size(px(0.0), line_height);
                }
                let wrap_width = if wraps {
                    known.width.or(match available.width {
                        AvailableSpace::Definite(x) => Some(x),
                        _ => None,
                    })
                } else {
                    None
                };
                if let Some(l) = laid.borrow().as_ref()
                    && l.size.is_some()
                    // Exact match only: answering a max-content query (None) with a size shaped
                    // for a narrower wrap made shrink-to-fit boxes (chat bubbles) too narrow.
                    && wrap_width == l.wrap_width
                {
                    return l.size.unwrap_or_default();
                }
                let shaped = window.text_system().shape_text(
                    text.clone(),
                    font_size,
                    &runs,
                    wrap_width,
                    None,
                );
                let Ok(shaped) = shaped else {
                    laid.borrow_mut().replace(Laid {
                        lines: Vec::new(),
                        line_height,
                        wrap_width,
                        size: Some(Size::default()),
                        bounds: None,
                        len: 0,
                    });
                    return Size::default();
                };
                let lines: Vec<WrappedLine> = shaped.into_iter().collect();
                let mut total: Size<Pixels> = Size::default();
                for line in &lines {
                    let s = line.size(line_height);
                    total.height += s.height;
                    total.width = total.width.max(s.width).ceil();
                }
                laid.borrow_mut().replace(Laid {
                    lines,
                    line_height,
                    wrap_width,
                    size: Some(total),
                    bounds: None,
                    len: text.len(),
                });
                total
            },
        );
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        if let Some((runs, font_size, line_height, wraps)) = &self.shaping {
            let needs_reshape = {
                let laid = self.laid.borrow();
                match laid.as_ref() {
                    None => true,
                    Some(l) if *wraps && bounds.size.width > px(0.0) => {
                        let too_wide = l.size.is_some_and(|s| s.width > bounds.size.width + px(0.5))
                            && l.wrap_width != Some(bounds.size.width);
                        let wrong_wrap = l
                            .wrap_width
                            .is_some_and(|w| (w - bounds.size.width).abs() > px(0.5));
                        too_wide || wrong_wrap
                    }
                    Some(_) => false,
                }
            };
            if needs_reshape {
                let wrap = (*wraps && bounds.size.width > px(0.0)).then_some(bounds.size.width);
                if let Ok(shaped) = window.text_system().shape_text(
                    self.text.clone(),
                    *font_size,
                    runs,
                    wrap,
                    None,
                ) {
                    let lines: Vec<WrappedLine> = shaped.into_iter().collect();
                    let mut total: Size<Pixels> = Size::default();
                    for line in &lines {
                        let s = line.size(*line_height);
                        total.height += s.height;
                        total.width = total.width.max(s.width).ceil();
                    }
                    self.laid.borrow_mut().replace(Laid {
                        lines,
                        line_height: *line_height,
                        wrap_width: wrap,
                        size: Some(total),
                        bounds: Some(bounds),
                        len: self.text.len(),
                    });
                }
            }
        }
        if let Some(l) = self.laid.borrow_mut().as_mut() {
            l.bounds = Some(bounds);
        }
        self.group.register(Entry {
            seg: self.seg,
            bounds,
            laid: self.laid.clone(),
            text: self.text.clone(),
            links: self.links.clone(),
        });
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let laid = self.laid.borrow();
        let Some(l) = laid.as_ref() else { return };
        let align = window.text_style().text_align;
        let lh = l.line_height;
        let mut origin = bounds.origin;
        for line in &l.lines {
            let _ = line.paint_background(origin, lh, align, Some(bounds), window, cx);
            origin.y += line.size(lh).height;
        }
        if let Some((range, extend)) = self.group.range_for(self.seg, l.len) {
            let color = self.group.color();
            for r in l.selection_rects(range, extend) {
                window.paint_quad(fill(Bounds::new(bounds.origin + r.origin, r.size), color));
            }
        }
        let mut origin = bounds.origin;
        for line in &l.lines {
            let _ = line.paint(origin, lh, align, Some(bounds), window, cx);
            origin.y += line.size(lh).height;
        }
        let link = self.group.hover_link_of(self.seg).is_some();
        window.set_cursor_style(
            if link {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            },
            hitbox,
        );
    }
}
