//! Text selection of the code view: the model lives in `ui_kit::SelectionGroup` in line / column
//! coordinates (segment = file line, offset = byte in the displayed line), so a selection survives
//! scrolling and the virtualization of the rows. Lines that are not on screen are read from the
//! document when copying.

use ui_kit::{Gap, SegmentSource};
use viewer_svc::Document;

use crate::spans::{MAX_LINE_BYTES, prepare_line};

/// The lines of a [`Document`] as the group's copy source. The text of a line is what the row
/// shows (tabs expanded, very long lines cut), so offsets agree with the painted glyphs.
pub struct DocLines {
    doc: Document,
}

impl DocLines {
    pub fn new(doc: Document) -> Self {
        Self { doc }
    }
}

impl SegmentSource for DocLines {
    fn len(&self) -> usize {
        self.doc.line_count().max(self.doc.estimated_line_count())
    }

    fn text(&self, seg: usize) -> gpui::SharedString {
        let line = self.doc.line(seg).unwrap_or_default();
        prepare_line(&line, &[], MAX_LINE_BYTES).text.into()
    }

    fn gap_before(&self, _seg: usize) -> Gap {
        Gap::Line
    }
}
