//! Pane bookkeeping for the split area: how many panes a [`Splits`] layout has, which tab each
//! pane hosts, and the gutter ratios. Pure (no gpui), so it is unit tested.

use store::Splits;

/// Number of panes a layout has.
pub fn pane_count(s: Splits) -> usize {
    match s {
        Splits::Single => 1,
        Splits::TwoVertical | Splits::TwoHorizontal => 2,
        Splits::OneAndTwoStacked => 3,
        Splits::Grid2x2 => 4,
    }
}

/// Smallest share a pane may be dragged to.
pub const MIN_RATIO: f32 = 0.15;

/// Clamps a gutter ratio so no pane vanishes.
pub fn clamp_ratio(r: f32) -> f32 {
    if r.is_finite() { r.clamp(MIN_RATIO, 1.0 - MIN_RATIO) } else { 0.5 }
}

/// Ratio of the pointer inside `[origin, origin + size]`.
pub fn ratio_at(pointer: f32, origin: f32, size: f32) -> f32 {
    if size <= 0.0 { 0.5 } else { clamp_ratio((pointer - origin) / size) }
}

/// Which tab each pane hosts, and which pane has the focus. `T` is the tab id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSet<T> {
    panes: Vec<Option<T>>,
    focused: usize,
}

impl<T: Copy + PartialEq> PaneSet<T> {
    pub fn new(count: usize) -> Self {
        Self { panes: vec![None; count.max(1)], focused: 0 }
    }

    /// A pane set restored from a saved session (`focused` is clamped).
    pub fn from_panes(panes: Vec<Option<T>>, focused: usize) -> Self {
        let panes = if panes.is_empty() { vec![None] } else { panes };
        let focused = focused.min(panes.len() - 1);
        Self { panes, focused }
    }

    pub fn len(&self) -> usize {
        self.panes.len()
    }

    /// Like [`PaneSet::show`] when only the first `visible` panes are on screen (a narrow window
    /// folds the 2x2 split into two columns): a tab that sits in a hidden pane swaps places with
    /// the focused pane's tab, so it is shown and nothing is lost.
    pub fn show_within(&mut self, tab: T, visible: usize) {
        let visible = visible.clamp(1, self.panes.len());
        match self.panes.iter().position(|p| *p == Some(tab)) {
            Some(i) if i < visible => self.focused = i,
            Some(i) => {
                let f = self.focused.min(visible - 1);
                self.focused = f;
                self.panes.swap(i, f);
            },
            None => {
                self.focused = self.focused.min(visible - 1);
                self.panes[self.focused] = Some(tab);
            },
        }
    }

    /// Keeps the focus on a visible pane (the window got narrower).
    pub fn clamp_focus(&mut self, visible: usize) {
        self.focused = self.focused.min(visible.clamp(1, self.panes.len()) - 1);
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    pub fn panes(&self) -> &[Option<T>] {
        &self.panes
    }

    pub fn focused(&self) -> usize {
        self.focused
    }

    pub fn focused_tab(&self) -> Option<T> {
        self.panes.get(self.focused).copied().flatten()
    }

    pub fn contains(&self, tab: T) -> bool {
        self.panes.contains(&Some(tab))
    }

    pub fn focus_pane(&mut self, pane: usize) {
        if pane < self.panes.len() {
            self.focused = pane;
        }
    }

    /// The layout has `count` panes now. Extra panes are dropped (their tabs stay open); new ones
    /// take a tab from `all` that is not shown yet, if any.
    pub fn resize(&mut self, count: usize, all: &[T]) {
        let count = count.max(1);
        self.panes.truncate(count);
        while self.panes.len() < count {
            let next = all.iter().copied().find(|t| !self.contains(*t));
            self.panes.push(next);
        }
        self.focused = self.focused.min(self.panes.len() - 1);
    }

    /// Select `tab`: focus the pane that already shows it, or put it in the focused pane.
    pub fn show(&mut self, tab: T) {
        if let Some(i) = self.panes.iter().position(|p| *p == Some(tab)) {
            self.focused = i;
        } else {
            self.panes[self.focused] = Some(tab);
        }
    }

    /// A new tab: it goes to the first empty pane (which gets the focus), else replaces the
    /// focused pane's tab.
    pub fn open(&mut self, tab: T) {
        match self.panes.iter().position(Option::is_none) {
            Some(i) => {
                self.panes[i] = Some(tab);
                self.focused = i;
            },
            None => self.panes[self.focused] = Some(tab),
        }
    }

    /// `tab` was closed. Its pane shows `replacement(neighbours)`: the first of `all` (the
    /// remaining tabs, in order) that is not shown elsewhere, preferring the one that followed it.
    pub fn remove(&mut self, tab: T, all: &[T], index_of_removed: usize) {
        let candidates = || {
            let after = all.iter().skip(index_of_removed);
            let before = all.iter().take(index_of_removed).rev();
            after.chain(before).copied().collect::<Vec<_>>()
        };
        for i in 0..self.panes.len() {
            if self.panes[i] == Some(tab) {
                let next = candidates().into_iter().find(|t| !self.contains(*t));
                self.panes[i] = next;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        assert_eq!(pane_count(Splits::Single), 1);
        assert_eq!(pane_count(Splits::TwoVertical), 2);
        assert_eq!(pane_count(Splits::TwoHorizontal), 2);
        assert_eq!(pane_count(Splits::OneAndTwoStacked), 3);
        assert_eq!(pane_count(Splits::Grid2x2), 4);
    }

    #[test]
    fn ratio_is_clamped() {
        assert_eq!(clamp_ratio(0.0), MIN_RATIO);
        assert_eq!(clamp_ratio(2.0), 1.0 - MIN_RATIO);
        assert_eq!(clamp_ratio(f32::NAN), 0.5);
        assert_eq!(ratio_at(150.0, 100.0, 200.0), 0.25);
        assert_eq!(ratio_at(5.0, 0.0, 0.0), 0.5);
    }

    #[test]
    fn open_fills_empty_panes_then_replaces_focused() {
        let mut p = PaneSet::<u32>::new(2);
        p.open(1);
        assert_eq!(p.panes(), &[Some(1), None]);
        p.open(2);
        assert_eq!(p.panes(), &[Some(1), Some(2)]);
        assert_eq!(p.focused(), 1);
        p.open(3);
        assert_eq!(p.panes(), &[Some(1), Some(3)]);
        assert_eq!(p.focused_tab(), Some(3));
    }

    #[test]
    fn show_focuses_an_already_visible_tab() {
        let mut p = PaneSet::<u32>::new(2);
        p.open(1);
        p.open(2);
        p.show(1);
        assert_eq!((p.focused(), p.focused_tab()), (0, Some(1)));
        p.show(9);
        assert_eq!(p.panes(), &[Some(9), Some(2)]);
    }

    #[test]
    fn resize_grows_with_unshown_tabs_and_shrinks_keeping_focus_valid() {
        let mut p = PaneSet::<u32>::new(1);
        p.open(1);
        p.resize(3, &[1, 2, 3, 4]);
        assert_eq!(p.panes(), &[Some(1), Some(2), Some(3)]);
        p.focus_pane(2);
        p.resize(1, &[1, 2, 3, 4]);
        assert_eq!(p.panes(), &[Some(1)]);
        assert_eq!(p.focused(), 0);
        p.resize(2, &[1]);
        assert_eq!(p.panes(), &[Some(1), None]);
    }

    #[test]
    fn remove_prefers_the_following_tab() {
        let mut p = PaneSet::<u32>::new(1);
        p.open(2);
        // tabs after closing 2 (index 1): [1, 3]
        p.remove(2, &[1, 3], 1);
        assert_eq!(p.focused_tab(), Some(3));
        p.remove(3, &[1], 1);
        assert_eq!(p.focused_tab(), Some(1));
        p.remove(1, &[], 0);
        assert_eq!(p.focused_tab(), None);
    }

    #[test]
    fn remove_skips_tabs_shown_in_other_panes() {
        let mut p = PaneSet::<u32>::new(2);
        p.open(1);
        p.open(2);
        p.remove(2, &[1, 3], 1);
        assert_eq!(p.panes(), &[Some(1), Some(3)]);
    }

    #[test]
    fn show_within_swaps_a_hidden_tab_into_the_visible_panes() {
        let mut p = PaneSet::<u32>::from_panes(vec![Some(1), Some(2), Some(3), Some(4)], 0);
        p.show_within(4, 2);
        assert_eq!(p.panes(), &[Some(4), Some(2), Some(3), Some(1)]);
        assert_eq!(p.focused_tab(), Some(4));
        p.show_within(2, 2);
        assert_eq!((p.focused(), p.focused_tab()), (1, Some(2)));
        // A tab that is not shown at all replaces the focused (visible) pane.
        p.show_within(9, 1);
        assert_eq!(p.panes()[0], Some(9));
    }

    #[test]
    fn from_panes_clamps_focus_and_clamp_focus_follows_visible() {
        let mut p = PaneSet::<u32>::from_panes(vec![Some(1), None], 7);
        assert_eq!(p.focused(), 1);
        p.clamp_focus(1);
        assert_eq!(p.focused(), 0);
        assert_eq!(PaneSet::<u32>::from_panes(vec![], 3).len(), 1);
    }
}
