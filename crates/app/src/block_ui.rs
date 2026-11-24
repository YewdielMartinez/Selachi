//! The terminal's block UI (PLAN A2/A3) and Ctrl+click paths: pointer state, hover detection, the
//! floating action bar and the actions behind it.
//!
//! Costs nothing at rest: all of this runs from pointer events (and only repaints when the hovered
//! block or path *changes*), the layout of rows is recomputed in `prepaint` from the same
//! snapshot, and the bar element exists only while a block is hovered.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use gpui::{
    AnyElement, ClipboardItem, Context, IntoElement, ModifiersChangedEvent, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, SharedString, div, prelude::*, px,
};
use theme::Radius;
use term_core::{Block, BlockId, BlockState, Session};
use ui_kit::{ActionButton, Icon, Pill};

use crate::app_shell::TERM_PAD;
use crate::block_layout::{self, VisRow};
use crate::links::{LinkCache, LinkHit};
use crate::terminal_view::{TerminalEvent, TerminalView};

/// Longest block output copied or sent to an agent (the middle is cut past it).
const OUTPUT_CAP: usize = 64 * 1024;
/// Output budget of the agent prompt.
const PROMPT_OUTPUT_CAP: usize = 16 * 1024;
/// Height of the bar with only pills, and with the action button.
const BAR_H: f32 = 28.0;
const BAR_H_WITH_ACTION: f32 = 44.0;

/// State of the block UI of one terminal. Interior mutability only where `prepaint` writes.
pub struct BlockUi {
    /// Copy of `Session::blocks()`, refreshed on every block event (never per frame).
    pub blocks: Vec<Block>,
    pub folded: HashSet<BlockId>,
    /// The block the action bar is shown for.
    pub hover: Option<BlockId>,
    /// "Copiado · incluye 2 secretos ocultos", shown in the bar of that block.
    pub note: Option<(BlockId, SharedString)>,
    /// Rows as last painted (written by `prepaint`).
    pub rows: RefCell<Vec<VisRow>>,
    /// Window position of the grid's top-left corner, and the cell size in px.
    pub origin: Cell<Point<Pixels>>,
    pub cell: Cell<(f32, f32)>,
    /// Last pointer position (window coordinates) while it is over the terminal.
    pub mouse: Option<Point<Pixels>>,
    pub ctrl: bool,
    /// Ctrl-hovered path and the painted row it is on.
    pub link: Option<(usize, LinkHit)>,
    pub links: LinkCache,
}

impl Default for BlockUi {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            folded: HashSet::new(),
            hover: None,
            note: None,
            rows: RefCell::new(Vec::new()),
            origin: Cell::new(Point::default()),
            cell: Cell::new((0.0, 0.0)),
            mouse: None,
            ctrl: false,
            link: None,
            links: LinkCache::default(),
        }
    }
}

/// `1 secreto oculto`, `3 secretos ocultos`.
pub fn secrets_hint(n: usize) -> String {
    if n == 1 {
        "incluye 1 secreto oculto".to_string()
    } else {
        format!("incluye {n} secretos ocultos")
    }
}

impl TerminalView {
    /// Re-reads the blocks of the session (called on block events only).
    pub(crate) fn refresh_blocks(&mut self) {
        let blocks = self
            .session
            .as_ref()
            .map(Session::blocks)
            .unwrap_or_default();
        self.block_ui
            .folded
            .retain(|id| blocks.iter().any(|b| b.id == *id));
        if self
            .block_ui
            .hover
            .is_some_and(|id| !blocks.iter().any(|b| b.id == id))
        {
            self.block_ui.hover = None;
        }
        self.block_ui.blocks = blocks;
    }

    /// Fills `block_ui.rows` for the current viewport (called from `prepaint`).
    pub(crate) fn compute_vis_rows(&self, session: &Session, rows: usize, at_bottom: bool) {
        let ui = &self.block_ui;
        let mut out = ui.rows.borrow_mut();
        out.clear();
        if !ui.blocks.is_empty() {
            let row_block: Vec<Option<BlockId>> =
                (0..rows).map(|r| session.block_at_line(r)).collect();
            if row_block.iter().any(Option::is_some) {
                let abs = block_layout::viewport_abs(&row_block, &ui.blocks);
                *out =
                    block_layout::layout_rows(&row_block, &abs, &ui.blocks, &ui.folded, at_bottom);
                return;
            }
        }
        out.extend((0..rows).map(|r| VisRow {
            term_row: Some(r),
            block: None,
            abs: None,
            summary: None,
        }));
    }

    /// The painted row, column and gutter flag under the pointer.
    fn pointer_cell(&self) -> Option<(usize, usize, bool)> {
        let ui = &self.block_ui;
        let m = ui.mouse?;
        let (cw, ch) = ui.cell.get();
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }
        let o = ui.origin.get();
        let (x, y) = (
            f32::from(m.x) - f32::from(o.x),
            f32::from(m.y) - f32::from(o.y),
        );
        if y < 0.0 {
            return None;
        }
        let vis = (y / ch) as usize;
        (vis < ui.rows.borrow().len())
            .then(|| (vis, if x < 0.0 { 0 } else { (x / cw) as usize }, x < 0.0))
    }

    /// Recomputes what the pointer is over; repaints only when it changed.
    fn update_hover(&mut self, cx: &mut Context<Self>) {
        let cell = self.pointer_cell();
        let (hover, link) = match cell {
            None => (None, None),
            Some((vis, col, gutter)) => {
                let ui = &self.block_ui;
                let rows = ui.rows.borrow();
                let hover = if ui.ctrl {
                    None
                } else {
                    block_layout::hover_target(&rows, &ui.blocks, vis, gutter)
                };
                let link = (ui.ctrl && !gutter)
                    .then(|| rows.get(vis).and_then(|v| v.term_row))
                    .flatten();
                drop(rows);
                (
                    hover,
                    link.and_then(|r| self.link_at(r, col)).map(|h| (vis, h)),
                )
            }
        };
        let mut changed = false;
        if self.block_ui.hover != hover {
            self.block_ui.hover = hover;
            if self
                .block_ui
                .note
                .as_ref()
                .is_some_and(|(id, _)| Some(*id) != hover)
            {
                self.block_ui.note = None;
            }
            changed = true;
        }
        if self.block_ui.link != link {
            self.block_ui.link = link;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// The existing path at column `col` of viewport row `term_row`.
    fn link_at(&mut self, term_row: usize, col: usize) -> Option<LinkHit> {
        let cwd = self
            .cwd()
            .map(std::path::Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())?;
        let state = self.paint_state.borrow();
        let cells = state.snap.lines.get(term_row)?;
        let hash = *state.snap.row_hash.get(term_row)?;
        self.block_ui.links.hit(hash, cells, col, &cwd)
    }

    pub(crate) fn on_pointer_move(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        self.block_ui.mouse = Some(e.position);
        self.block_ui.ctrl = e.modifiers.control;
        self.update_hover(cx);
    }

    pub(crate) fn on_pointer_leave(&mut self, cx: &mut Context<Self>) {
        // Over the bar (its pills occlude the terminal) the pointer has not really left.
        let on_bar = self.block_ui.hover.is_some() && self.pointer_near_bar();
        if on_bar {
            return;
        }
        let ui = &mut self.block_ui;
        ui.mouse = None;
        if ui.hover.take().is_some() | ui.link.take().is_some() | ui.note.take().is_some() {
            cx.notify();
        }
    }

    /// Is the last known pointer position close to the bar of the hovered block?
    fn pointer_near_bar(&self) -> bool {
        let ui = &self.block_ui;
        let (Some(m), Some(id)) = (ui.mouse, ui.hover) else {
            return false;
        };
        let rows = ui.rows.borrow();
        let Some(vis) = block_layout::anchor_row(&rows, &ui.blocks, id) else {
            return false;
        };
        let (_, ch) = ui.cell.get();
        let o = ui.origin.get();
        let y = f32::from(m.y) - f32::from(o.y);
        // The bar hangs on the right edge of the anchor row and may extend a few rows down.
        let top = vis as f32 * ch - BAR_H;
        let bottom = vis as f32 * ch + BAR_H_WITH_ACTION + ch;
        (top..bottom).contains(&y)
    }

    pub(crate) fn on_modifiers(&mut self, e: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        if self.block_ui.ctrl != e.modifiers.control {
            self.block_ui.ctrl = e.modifiers.control;
            if self.block_ui.mouse.is_some() {
                self.update_hover(cx);
            } else {
                self.block_ui.link = None;
            }
        }
    }

    /// Handles a left click that is not for the shell: Ctrl+click on a path, a click on a fold
    /// summary. Returns `true` when it was consumed.
    pub(crate) fn click_special(&mut self, e: &MouseDownEvent, cx: &mut Context<Self>) -> bool {
        self.block_ui.mouse = Some(e.position);
        self.block_ui.ctrl = e.modifiers.control;
        if e.modifiers.control {
            self.update_hover(cx);
            if let Some((_, hit)) = self.block_ui.link.clone() {
                cx.emit(TerminalEvent::OpenPath {
                    path: hit.path,
                    line: hit.line,
                    col: hit.col,
                    is_dir: hit.is_dir,
                });
                return true;
            }
            return false;
        }
        if let Some((vis, _, false)) = self.pointer_cell() {
            let summary = self
                .block_ui
                .rows
                .borrow()
                .get(vis)
                .and_then(|v| v.summary.and(v.block));
            if let Some(id) = summary {
                self.toggle_fold(id, cx);
                return true;
            }
        }
        false
    }

    // ---- actions --------------------------------------------------------------------------

    fn block(&self, id: BlockId) -> Option<&Block> {
        block_layout::find_block(&self.block_ui.blocks, id)
    }

    /// Plegar / Desplegar: the renderer hides the output rows, the PTY is untouched.
    pub(crate) fn toggle_fold(&mut self, id: BlockId, cx: &mut Context<Self>) {
        if !self.block_ui.folded.remove(&id) && self.block(id).is_some_and(block_layout::foldable) {
            self.block_ui.folded.insert(id);
        }
        cx.notify();
    }

    /// Copiar salida: redacted before it reaches the clipboard.
    pub(crate) fn copy_block(&mut self, id: BlockId, cx: &mut Context<Self>) {
        let Some(text) = self
            .session
            .as_ref()
            .and_then(|s| s.block_output_text(id, OUTPUT_CAP))
        else {
            self.block_ui.note = Some((id, "No se pudo leer la salida".into()));
            cx.notify();
            return;
        };
        let (clean, hidden) = term_core::redact(&text);
        cx.write_to_clipboard(ClipboardItem::new_string(clean));
        let note = if hidden > 0 {
            format!("Copiado · {}", secrets_hint(hidden))
        } else {
            "Copiado".to_string()
        };
        self.block_ui.note = Some((id, note.into()));
        cx.notify();
    }

    /// Re-ejecutar: types the command again at the prompt and presses Enter.
    pub(crate) fn rerun_block(&mut self, id: BlockId, cx: &mut Context<Self>) {
        if let Some(cmd) = self.block(id).and_then(|b| b.command.clone()) {
            self.send_text(&cmd, true, cx);
        }
    }

    /// Arreglar con agente: hands the block (and its output) to the host.
    pub(crate) fn fix_block(&mut self, id: BlockId, cx: &mut Context<Self>) {
        let Some(block) = self.block(id).cloned() else {
            return;
        };
        let output = self
            .session
            .as_ref()
            .and_then(|s| s.block_output_text(id, PROMPT_OUTPUT_CAP))
            .unwrap_or_default();
        cx.emit(TerminalEvent::FixWithAgent { block, output });
    }

    /// Ctrl+Up / Ctrl+Down: scroll to the previous / next block. `false` when there are no
    /// blocks (the keys then go to the shell).
    pub(crate) fn jump_block(&mut self, dir: i32, cx: &mut Context<Self>) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        if self.block_ui.blocks.is_empty() {
            return false;
        }
        let target = if dir < 0 {
            session.prev_block(None)
        } else {
            session.next_block(None)
        };
        match target {
            Some(id) => {
                session.scroll_to_block(id);
            }
            None if dir > 0 => session.scroll_to_bottom(),
            None => {}
        }
        cx.notify();
        true
    }

    // ---- the bar --------------------------------------------------------------------------

    /// The floating action bar of the hovered block, or `None` (the common case: zero elements).
    pub(crate) fn block_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = &self.block_ui;
        let id = ui.hover?;
        let block = self.block(id)?;
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let view = cx.entity();
        let rows = ui.rows.borrow();
        let vis = block_layout::anchor_row(&rows, &ui.blocks, id)?;
        drop(rows);
        let (_, ch) = ui.cell.get();
        let failed = block.state == BlockState::Failed;
        let bar_h = if failed { BAR_H_WITH_ACTION } else { BAR_H };
        let row_center = TERM_PAD + ch * vis as f32 + ch / 2.0;
        let top = (row_center - bar_h / 2.0).max(2.0);

        let folded = ui.folded.contains(&id);
        let mut bar = div()
            .id("block-bar")
            .absolute()
            .top(px(top))
            .right(px(TERM_PAD + 6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .p(px(4.0))
            .rounded(px(Radius::PILL))
            .bg(c.surface_2)
            .font_family(ty.ui_family)
            .text_size(px(ty.ui_size_sm));

        if let Some((nid, note)) = &ui.note
            && *nid == id
        {
            bar = bar.child(div().px(px(6.0)).text_color(c.muted).child(note.clone()));
        }
        if block_layout::foldable(block) || folded {
            let v = view.clone();
            bar = bar.child(
                Pill::compact("block-fold", &theme)
                    .icon(if folded {
                        Icon::ChevronRight
                    } else {
                        Icon::ChevronDown
                    })
                    .label(if folded { "Desplegar" } else { "Plegar" })
                    .tooltip("Oculta la salida en pantalla; el proceso no se toca")
                    .on_click(move |_, _, cx| v.update(cx, |t, cx| t.toggle_fold(id, cx))),
            );
        }
        if block.state != BlockState::Running {
            let v = view.clone();
            bar = bar.child(
                Pill::compact("block-copy", &theme)
                    .label("Copiar salida")
                    .tooltip("Copia la salida sin secretos")
                    .on_click(move |_, _, cx| v.update(cx, |t, cx| t.copy_block(id, cx))),
            );
            if block.command.is_some() {
                let v = view.clone();
                bar = bar.child(
                    Pill::compact("block-rerun", &theme)
                        .label("Re-ejecutar")
                        .tooltip("Escribe el comando otra vez en el prompt")
                        .on_click(move |_, _, cx| v.update(cx, |t, cx| t.rerun_block(id, cx))),
                );
            }
        }
        if failed {
            let v = view;
            bar = bar.child(
                ActionButton::new("block-fix", &theme, "Arreglar con agente")
                    .icon(Icon::Agents)
                    .tooltip("Abre un agente con el comando y su salida (sin secretos)")
                    .on_click(move |_, _, cx| v.update(cx, |t, cx| t.fix_block(id, cx))),
            );
        }
        Some(bar.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_hint_pluralizes() {
        assert_eq!(secrets_hint(1), "incluye 1 secreto oculto");
        assert_eq!(secrets_hint(3), "incluye 3 secretos ocultos");
    }
}
