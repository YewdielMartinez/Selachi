//! Pestañas: jump to an open tab (from the snapshot the host passes in [`crate::OpenContext`]).

use ui_kit::Icon;

use super::{ordered_score, top_n};
use crate::item::{ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::scorer::{Matcher, Needle};
use crate::source::{PaletteSource, SourceQuery, display_path};

pub struct TabsSource;

impl PaletteSource for TabsSource {
    fn id(&self) -> &'static str {
        "tabs"
    }

    fn title(&self) -> &'static str {
        "Pestañas"
    }

    /// Tabs only appear in the unscoped view.
    fn scopes(&self) -> &'static [Scope] {
        &[]
    }

    fn order(&self) -> u8 {
        10
    }

    fn weight(&self) -> f32 {
        0.1
    }

    fn empty_limit(&self) -> usize {
        8
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let needle = Needle::new(q.text);
        let mut matcher = Matcher::new();
        let mut out = Vec::new();
        for (i, tab) in q.ctx.tabs.iter().enumerate() {
            let active = q.ctx.active_tab == Some(tab.id);
            let cwd = tab.cwd.as_ref().map(|p| display_path(&p.to_string_lossy()));
            let sub = tab.subtitle.as_ref().map_or("", |s| s.as_ref());
            let score = if needle.is_empty() {
                // The active tab goes last: jumping to it is a no-op.
                Some(ordered_score(i) - if active { 0.5 } else { 0.0 })
            } else {
                matcher
                    .score(
                        &needle,
                        &[
                            (&tab.title, 1.0),
                            (sub, 0.6),
                            (cwd.as_deref().unwrap_or(""), 0.5),
                        ],
                    )
                    .map(|s| s - if active { 0.05 } else { 0.0 })
            };
            let Some(score) = score else { continue };
            let mut item = PaletteItem::new(
                format!("tab:{}", tab.id.as_u64()),
                tab.title.clone(),
                ItemAction::emit("Ir a la pestaña", PaletteEvent::FocusTab(tab.id)),
            )
            .icon(tab.icon.icon())
            .score(score);
            item.status = tab.status;
            item.subtitle = tab.subtitle.clone().or_else(|| cwd.map(Into::into));
            if active {
                item.meta = Some("actual".into());
            }
            if item.status.is_none() && item.icon.is_none() {
                item.icon = Some(Icon::Terminal);
            }
            out.push(item);
        }
        top_n(out, q.limit)
    }
}
