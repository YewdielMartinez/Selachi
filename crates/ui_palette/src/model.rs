//! The palette's state machine, free of gpui so it can be tested end to end: merged results,
//! section layout, selection and what Enter / Ctrl+Enter do. The view only paints it.

use std::sync::Arc;

use crate::form::FormState;
use crate::item::{ActionKind, Effect, ItemAction, PaletteEvent, PaletteItem};
use crate::query::Query;
use crate::source::{Cancel, PaletteSource, SourceCtx, SourceQuery};

/// The items one source returned for the current query.
#[derive(Clone, Debug)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub order: u8,
    pub weight: f32,
    pub items: Vec<PaletteItem>,
}

/// A row of the list: a header or an item.
#[derive(Clone, Debug)]
pub enum LayoutRow {
    Header(&'static str),
    Item(Box<PaletteItem>),
}

/// Merged results of the current query. Results are tagged with the query generation they were
/// computed for; late answers of an older generation are dropped.
#[derive(Debug, Default)]
pub struct Results {
    generation: u64,
    sections: Vec<Section>,
}

impl Results {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A new query starts: sections of sources that no longer apply go away at once; the others
    /// keep their rows until their fresh answer arrives (no flicker while typing).
    pub fn begin(&mut self, generation: u64, applies: impl Fn(&str) -> bool) {
        self.generation = generation;
        self.sections.retain(|s| applies(s.id));
    }

    /// Stores one source's answer (replacing its previous rows). Returns `false` and ignores it
    /// when it belongs to an older query.
    pub fn push(&mut self, generation: u64, section: Section) -> bool {
        if generation != self.generation {
            return false;
        }
        match self.sections.iter_mut().find(|s| s.id == section.id) {
            Some(slot) => *slot = section,
            None => self.sections.push(section),
        }
        true
    }

    pub fn clear(&mut self) {
        self.sections.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|s| s.items.is_empty())
    }

    pub fn item_count(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// Sections with rows, in display order: fixed [`Section::order`] while the query is empty,
    /// by best score (plus the source weight) once the user typed.
    pub fn layout(&self, query_is_empty: bool) -> Vec<LayoutRow> {
        let mut secs: Vec<(f32, &Section)> = self
            .sections
            .iter()
            .filter(|s| !s.items.is_empty())
            .map(|s| {
                let best = s.items.iter().map(|i| i.score).fold(f32::MIN, f32::max);
                (best + s.weight, s)
            })
            .collect();
        if query_is_empty {
            secs.sort_by_key(|(_, s)| s.order);
        } else {
            secs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.order.cmp(&b.1.order)));
        }
        let mut rows = Vec::new();
        for (_, s) in secs {
            rows.push(LayoutRow::Header(s.title));
            let mut items: Vec<&PaletteItem> = s.items.iter().collect();
            items.sort_by(|a, b| b.score.total_cmp(&a.score));
            rows.extend(items.into_iter().map(|i| LayoutRow::Item(Box::new(i.clone()))));
        }
        rows
    }
}

/// What confirming did.
#[derive(Debug, PartialEq)]
pub enum Confirmed {
    /// Nothing selected / nothing to do.
    Nothing,
    /// Emit `event`, record `effect` and close.
    Emit(PaletteEvent, Option<Effect>),
    /// The selected item needs a form: [`PaletteModel::form`] is now set.
    FormOpened,
    /// The form is not valid yet; [`FormState::error`] says why.
    FormInvalid,
    /// Enter on a field that is not the last one moved to the next field.
    NextField,
}

/// Everything the view paints.
#[derive(Debug, Default)]
pub struct PaletteModel {
    pub results: Results,
    pub rows: Vec<LayoutRow>,
    /// Row indices of the selectable rows (items), in order.
    selectable: Vec<usize>,
    selected: usize,
    /// The user moved the selection since the query last changed (so late results must not
    /// yank it back to the top).
    navigated: bool,
    pub form: Option<FormState>,
}

impl PaletteModel {
    /// Rebuilds [`PaletteModel::rows`] from the results and fixes the selection: stays on the same
    /// item (by key) when the user already navigated, else goes to the first item.
    pub fn relayout(&mut self, query_is_empty: bool) {
        let keep = self.selected_item().map(|i| i.key.clone());
        self.rows = self.results.layout(query_is_empty);
        self.selectable =
            self.rows.iter().enumerate().filter_map(|(i, r)| matches!(r, LayoutRow::Item(_)).then_some(i)).collect();
        self.selected = match (self.navigated, keep) {
            (true, Some(key)) => self.selectable.iter().position(|&r| self.row_key(r) == Some(key.as_str())).unwrap_or(0),
            _ => 0,
        };
    }

    fn row_key(&self, row: usize) -> Option<&str> {
        match self.rows.get(row)? {
            LayoutRow::Item(i) => Some(&i.key),
            LayoutRow::Header(_) => None,
        }
    }

    /// The query text changed: the next relayout may reset the selection.
    pub fn query_changed(&mut self) {
        self.navigated = false;
    }

    pub fn selected_row(&self) -> Option<usize> {
        self.selectable.get(self.selected).copied()
    }

    pub fn selected_item(&self) -> Option<&PaletteItem> {
        match self.rows.get(self.selected_row()?)? {
            LayoutRow::Item(i) => Some(i),
            LayoutRow::Header(_) => None,
        }
    }

    pub fn selectable_len(&self) -> usize {
        self.selectable.len()
    }

    /// Selection by position among the selectable rows (a mouse click).
    pub fn select_at_row(&mut self, row: usize) {
        if let Some(p) = self.selectable.iter().position(|&r| r == row) {
            self.selected = p;
            self.navigated = true;
        }
    }

    /// Moves the selection by `delta` items, wrapping around.
    pub fn move_selection(&mut self, delta: isize) {
        let n = self.selectable.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.navigated = true;
    }

    /// Like [`PaletteModel::move_selection`] but stops at the ends (PageUp / PageDown).
    pub fn page_selection(&mut self, delta: isize) {
        let n = self.selectable.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, n - 1) as usize;
        self.navigated = true;
    }

    /// Enter (`alt = false`) or Ctrl+Enter on the selected item, or on the open form.
    pub fn confirm(&mut self, alt: bool) -> Confirmed {
        if let Some(form) = &mut self.form {
            if !alt && !form.is_last() && form.spec.labels().1.is_some() && form.spec.params.len() > 1 {
                // Enter walks through the fields; the last one submits.
                form.move_active(1);
                return Confirmed::NextField;
            }
            return match form.submit(alt) {
                Some((event, effect)) => Confirmed::Emit(event, effect),
                None => Confirmed::FormInvalid,
            };
        }
        let Some(item) = self.selected_item() else { return Confirmed::Nothing };
        let action: ItemAction = match (&item.alt, alt) {
            (Some(a), true) => a.clone(),
            _ => item.primary.clone(),
        };
        match action.kind {
            ActionKind::Emit(event) => Confirmed::Emit(event, action.effect),
            ActionKind::Form(spec) => {
                self.form = Some(FormState::new(spec));
                Confirmed::FormOpened
            }
        }
    }

    pub fn close_form(&mut self) {
        self.form = None;
    }

    /// Resets everything (the palette closed).
    pub fn clear(&mut self) {
        *self = PaletteModel::default();
    }

    /// Footer verbs for the selected item or the open form: `(Enter, Ctrl+Enter)`.
    pub fn hints(&self) -> (Option<&'static str>, Option<&'static str>) {
        if let Some(form) = &self.form {
            let (a, b) = form.spec.labels();
            return (Some(a), b);
        }
        match self.selected_item() {
            Some(i) => (Some(i.primary.label), i.alt.as_ref().map(|a| a.label)),
            None => (None, None),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Running sources without gpui (tests, benches, and the view's own planning)
// ---------------------------------------------------------------------------------------------

/// The sources a query is routed to, with the row budget of each.
pub fn plan(sources: &[Arc<dyn PaletteSource>], query: &Query) -> Vec<(Arc<dyn PaletteSource>, usize)> {
    let empty = query.is_empty();
    sources
        .iter()
        .filter(|s| s.applies(query.scope))
        .map(|s| (s.clone(), s.limit_for(query.scope, empty)))
        .filter(|(_, limit)| *limit > 0)
        .collect()
}

/// Runs one source and wraps its rows as a [`Section`].
pub fn run_source(
    source: &dyn PaletteSource,
    limit: usize,
    query: &Query,
    ctx: &SourceCtx,
    cancel: &Cancel,
) -> Section {
    let items = source.query(&SourceQuery { text: &query.text, scope: query.scope, limit, ctx, cancel });
    Section { id: source.id(), title: source.title(), order: source.order(), weight: source.weight(), items }
}

/// Runs every applicable source in sequence and returns the merged model (blocking; the view runs
/// them concurrently on the background executor instead).
pub fn run_blocking(sources: &[Arc<dyn PaletteSource>], raw: &str, ctx: &SourceCtx) -> PaletteModel {
    let query = Query::parse(raw);
    let cancel = Cancel::new();
    let mut model = PaletteModel::default();
    let planned = plan(sources, &query);
    model.results.begin(1, |id| planned.iter().any(|(s, _)| s.id() == id));
    for (s, limit) in &planned {
        let section = run_source(s.as_ref(), *limit, &query, ctx, &cancel);
        model.results.push(1, section);
    }
    model.relayout(query.is_empty());
    model
}
