//! The two elements that shape and paint a [`TextField`]: [`LineElement`] (single line: one shaped
//! line, horizontal scroll, middle ellipsis while unfocused) and [`AreaElement`] (wrapped lines,
//! measured layout, vertical scroll). Both register the input handler and paint the caret.

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, ElementId, ElementInputHandler, Entity,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextAlign, TextRun, UnderlineStyle, Window, WrappedLine, fill,
    point, px, relative, size,
};

use super::{MASK, TextField};

/// The shaped line of the last frame and how it was fitted (single line).
#[derive(Clone)]
pub(crate) struct SingleLayout {
    pub line: ShapedLine,
    pub scroll_x: Pixels,
    /// The last paint showed a middle-ellipsized copy (unfocused and too long).
    pub ellipsized: bool,
    /// The text is wider than the box (a tooltip then carries the full value).
    pub overflow: bool,
}

/// Wrapped lines of the last frame (area).
#[derive(Clone)]
pub(crate) struct Laid {
    pub lines: Vec<WrappedLine>,
    pub starts: Vec<usize>,
    pub line_h: Pixels,
    pub placeholder: bool,
}

impl Laid {
    fn height_of(&self, i: usize) -> Pixels {
        self.lines[i].size(self.line_h).height
    }

    pub fn total_height(&self) -> Pixels {
        (0..self.lines.len())
            .map(|i| self.height_of(i))
            .fold(px(0.0), |a, b| a + b)
    }

    pub fn position_for_index(&self, offset: usize) -> Point<Pixels> {
        if self.placeholder {
            return point(px(0.0), px(0.0));
        }
        let mut y = px(0.0);
        for (i, line) in self.lines.iter().enumerate() {
            let start = self.starts[i];
            let end = start + line.len();
            if offset <= end || i + 1 == self.lines.len() {
                let local = offset.saturating_sub(start).min(line.len());
                let p = line
                    .position_for_index(local, self.line_h)
                    .unwrap_or_default();
                return point(p.x, y + p.y);
            }
            y += self.height_of(i);
        }
        point(px(0.0), px(0.0))
    }

    pub fn index_for_position(&self, p: Point<Pixels>) -> usize {
        if self.placeholder {
            return 0;
        }
        let mut top = px(0.0);
        for (i, line) in self.lines.iter().enumerate() {
            let h = self.height_of(i);
            if p.y < top + h || i + 1 == self.lines.len() {
                let local = point(p.x.max(px(0.0)), (p.y - top).max(px(0.0)));
                let idx = match line.closest_index_for_position(local, self.line_h) {
                    Ok(i) | Err(i) => i,
                };
                return self.starts[i] + idx.min(line.len());
            }
            top += h;
        }
        0
    }
}

/// Text runs for `shown`, underlining the IME composition `marked` (already in `shown` offsets).
fn runs(shown_len: usize, base: &TextRun, marked: Option<std::ops::Range<usize>>) -> Vec<TextRun> {
    let base = TextRun {
        len: shown_len,
        ..base.clone()
    };
    match marked {
        Some(m) if m.end <= shown_len && m.start < m.end => [
            TextRun {
                len: m.start,
                ..base.clone()
            },
            TextRun {
                len: m.end - m.start,
                underline: Some(UnderlineStyle {
                    color: Some(base.color),
                    thickness: px(1.0),
                    wavy: false,
                }),
                ..base.clone()
            },
            TextRun {
                len: shown_len - m.end,
                ..base
            },
        ]
        .into_iter()
        .filter(|r| r.len > 0)
        .collect(),
        _ => vec![base],
    }
}

fn base_run(style: &gpui::TextStyle, color: gpui::Rgba) -> TextRun {
    TextRun {
        len: 0,
        font: style.font(),
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Single line
// ---------------------------------------------------------------------------------------------

pub(crate) struct LineElement {
    pub input: Entity<TextField>,
}

pub(crate) struct LinePrepaint {
    layout: SingleLayout,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for LineElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for LineElement {
    type RequestLayoutState = ();
    type PrepaintState = LinePrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> LinePrepaint {
        let input = self.input.read(cx);
        let c = input.theme.colors;
        let style = window.text_style();
        let empty = input.buf.text.is_empty();
        let focused = input.focus.is_focused(window);
        let font_size = style.font_size.to_pixels(window.rem_size());
        let shown: SharedString = if empty {
            input.placeholder.clone()
        } else if input.password {
            input
                .buf
                .text
                .chars()
                .map(|_| MASK)
                .collect::<String>()
                .into()
        } else {
            input.buf.text.clone().into()
        };
        let color = if empty { c.outline } else { c.text };
        let base = base_run(&style, color);
        let marked = input
            .buf
            .marked
            .as_ref()
            .filter(|_| !empty)
            .map(|m| input.offset_to_display(m.start)..input.offset_to_display(m.end));
        let run_list = runs(shown.len(), &base, marked);
        let mut line = window
            .text_system()
            .shape_line(shown.clone(), font_size, &run_list, None);
        let overflow = line.width > bounds.size.width;
        let mut ellipsized = false;
        let mut scroll = px(0.0);
        let cursor_disp = input.offset_to_display(input.buf.cursor());
        if overflow {
            if focused && !empty {
                // Keep the caret inside the box: scroll just enough, never past the end.
                let caret_x = line.x_for_index(cursor_disp);
                let max_scroll = line.width - bounds.size.width + px(2.0);
                scroll = input.single.as_ref().map_or(px(0.0), |s| s.scroll_x);
                if caret_x - scroll > bounds.size.width - px(2.0) {
                    scroll = caret_x - bounds.size.width + px(2.0);
                }
                if caret_x < scroll {
                    scroll = caret_x;
                }
                scroll = scroll.clamp(px(0.0), max_scroll.max(px(0.0)));
            } else if !input.password {
                // Not being edited: the middle is elided so both the start and the end stay readable.
                let measure = |t: &str| -> f32 {
                    let r = [TextRun {
                        len: t.len(),
                        ..base.clone()
                    }];
                    f32::from(
                        window
                            .text_system()
                            .shape_line(t.to_string().into(), font_size, &r, None)
                            .width,
                    )
                };
                let text = super::middle_ellipsis(&shown, f32::from(bounds.size.width), &measure);
                let r = [TextRun {
                    len: text.len(),
                    ..base.clone()
                }];
                line = window
                    .text_system()
                    .shape_line(text.into(), font_size, &r, None);
                ellipsized = true;
            }
        }
        let sel = input.offset_to_display(input.buf.sel.start)
            ..input.offset_to_display(input.buf.sel.end);
        let origin_x = bounds.left() - scroll;
        let (selection, cursor) = if ellipsized && !empty {
            (None, None)
        } else if sel.is_empty() {
            let x = if empty {
                px(0.0)
            } else {
                line.x_for_index(cursor_disp)
            };
            let quad = fill(
                Bounds::new(
                    point(origin_x + x, bounds.top()),
                    size(px(1.5), bounds.size.height),
                ),
                c.accent,
            );
            (None, Some(quad))
        } else {
            let mut sel_color = c.accent;
            sel_color.a = 0.3;
            let quad = fill(
                Bounds::from_corners(
                    point(origin_x + line.x_for_index(sel.start), bounds.top()),
                    point(origin_x + line.x_for_index(sel.end), bounds.bottom()),
                ),
                sel_color,
            );
            (Some(quad), None)
        };
        LinePrepaint {
            layout: SingleLayout {
                line,
                scroll_x: scroll,
                ellipsized,
                overflow,
            },
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        pre: &mut LinePrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let (scroll, line_h) = (pre.layout.scroll_x, window.line_height());
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(sel) = pre.selection.take() {
                window.paint_quad(sel);
            }
            let _ = pre.layout.line.paint(
                point(bounds.origin.x - scroll, bounds.origin.y),
                line_h,
                window,
                cx,
            );
            if focus.is_focused(window)
                && let Some(cursor) = pre.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        let layout = pre.layout.clone();
        self.input.update(cx, |input, cx| {
            let tooltip_changed =
                input.single.as_ref().is_none_or(|s| s.overflow) != layout.overflow;
            input.last_bounds = Some(bounds);
            input.single = Some(layout);
            if tooltip_changed {
                cx.notify();
            }
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Area
// ---------------------------------------------------------------------------------------------

pub(crate) struct AreaElement {
    pub input: Entity<TextField>,
}

impl IntoElement for AreaElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for AreaElement {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        let c = input.theme.colors;
        let style = window.text_style();
        let empty = input.buf.text.is_empty();
        let shown: SharedString = if empty {
            input.placeholder.clone()
        } else {
            input.buf.text.clone().into()
        };
        let color = if empty { c.muted } else { c.text };
        let base = base_run(&style, color);
        let run_list = runs(
            shown.len(),
            &base,
            input.buf.marked.clone().filter(|_| !empty),
        );
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_h = window.line_height();
        let min_h = line_h * input.min_lines as f32;
        let max_h = line_h * input.max_lines as f32;
        let laid = input.laid.clone();
        let scroll = input.scroll.clone();
        let caret = input.buf.cursor();

        let measure = move |known: gpui::Size<Option<Pixels>>,
                            avail: gpui::Size<AvailableSpace>,
                            window: &mut Window,
                            _: &mut App| {
            let width = known.width.or(match avail.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let shaped = window
                .text_system()
                .shape_text(shown.clone(), font_size, &run_list, width, None)
                .map(|l| l.into_vec())
                .unwrap_or_default();
            let mut starts = Vec::with_capacity(shaped.len());
            let mut at = 0usize;
            for l in &shaped {
                starts.push(at);
                at += l.len() + 1;
            }
            let l = Laid {
                lines: shaped,
                starts,
                line_h,
                placeholder: empty,
            };
            let total = l.total_height();
            let h = total.max(min_h).min(max_h);
            // Keep the caret inside the visible part.
            let mut s = scroll.get();
            let cy = l.position_for_index(caret).y;
            if cy < s {
                s = cy;
            }
            if cy + line_h > s + h {
                s = cy + line_h - h;
            }
            scroll.set(s.max(px(0.0)).min((total - h).max(px(0.0))));
            let w = width.unwrap_or_else(|| {
                l.lines
                    .iter()
                    .map(|x| x.width())
                    .fold(px(0.0), |a, b| a.max(b))
            });
            *laid.borrow_mut() = Some(l);
            size(w, h)
        };
        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.0).into();
        (window.request_measured_layout(layout_style, measure), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, theme, sel, caret, laid_rc, scroll) = {
            let i = self.input.read(cx);
            (
                i.focus.clone(),
                i.theme,
                i.buf.sel.clone(),
                i.buf.cursor(),
                i.laid.clone(),
                i.scroll.get(),
            )
        };
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let c = theme.colors;
        let laid = laid_rc.borrow().clone();
        if let Some(laid) = &laid {
            let top = bounds.top() - scroll;
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if !sel.is_empty() && !laid.placeholder {
                    let mut tint = c.accent;
                    tint.a = 0.3;
                    let width = bounds.size.width;
                    for (i, line) in laid.lines.iter().enumerate() {
                        let (ls, le) = (laid.starts[i], laid.starts[i] + line.len());
                        if sel.end < ls || sel.start > le {
                            continue;
                        }
                        let a = laid.position_for_index(sel.start.max(ls));
                        let b = laid.position_for_index(sel.end.min(le));
                        let row = |x0: Pixels, x1: Pixels, y: Pixels| {
                            fill(
                                Bounds::new(
                                    point(bounds.left() + x0, top + y),
                                    size((x1 - x0).max(px(2.0)), laid.line_h),
                                ),
                                tint,
                            )
                        };
                        if a.y == b.y {
                            window.paint_quad(row(a.x, b.x, a.y));
                        } else {
                            window.paint_quad(row(a.x, width, a.y));
                            let mut y = a.y + laid.line_h;
                            while y < b.y {
                                window.paint_quad(row(px(0.0), width, y));
                                y += laid.line_h;
                            }
                            window.paint_quad(row(px(0.0), b.x, b.y));
                        }
                    }
                }
                let mut y = px(0.0);
                for (i, line) in laid.lines.iter().enumerate() {
                    let _ = line.paint(
                        point(bounds.left(), top + y),
                        laid.line_h,
                        TextAlign::Left,
                        Some(bounds),
                        window,
                        cx,
                    );
                    y += laid.height_of(i);
                }
                if sel.is_empty() && focus.is_focused(window) {
                    let p = laid.position_for_index(caret);
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + p.x, top + p.y),
                            size(px(1.5), laid.line_h),
                        ),
                        c.accent,
                    ));
                }
            });
        }
        self.input
            .update(cx, |input, _| input.last_bounds = Some(bounds));
    }
}
