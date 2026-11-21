//! The built-in sources. Each one is a [`crate::PaletteSource`]; an app can mix them with its own.

mod actions;
mod git;
mod history;
mod presets;
mod projects;
mod snippets;
mod tabs;
mod tasks;

pub use actions::{ActionEntry, ActionsSource};
pub use git::GitSource;
pub use history::HistorySource;
pub use presets::PresetsSource;
pub use projects::ProjectsSource;
pub use snippets::SnippetsSource;
pub use tabs::TabsSource;
pub use tasks::TasksSource;

use crate::item::PaletteItem;

/// Best `limit` items by score (stable for equal scores, so the source's own order survives).
pub(crate) fn top_n(mut items: Vec<PaletteItem>, limit: usize) -> Vec<PaletteItem> {
    items.sort_by(|a, b| b.score.total_cmp(&a.score));
    items.truncate(limit);
    items
}

/// Score that keeps the source's own order for an empty query (`index` 0 first).
pub(crate) fn ordered_score(index: usize) -> f32 {
    1.0 - (index as f32 * 1e-3).min(0.9)
}
