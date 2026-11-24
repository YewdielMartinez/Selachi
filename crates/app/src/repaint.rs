//! Debug counters of how many times each view of the chrome rendered (docs/auditoria-gpui.md H4).
//!
//! gpui re-runs `render` of a view when it was notified (and of its ancestors, which are dirty
//! with it); a view shown as a *cached* child is skipped otherwise. These counters make that
//! visible: with `RUST_LOG=selachi=trace` every render logs its running total, and the tests read
//! them to prove that a sidebar hover repaints the sidebar and not the rest of the chrome.
//! Release builds compile the counters out. They are per thread (the UI thread), so parallel
//! tests do not see each other.

/// The views of the chrome that are counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Shell,
    Sidebar,
    TitleBar,
}

#[cfg(any(debug_assertions, test))]
mod imp {
    use super::View;
    use std::cell::Cell;

    thread_local! {
        static COUNTS: [Cell<u64>; 3] = const { [Cell::new(0), Cell::new(0), Cell::new(0)] };
    }

    fn slot(view: View) -> usize {
        match view {
            View::Shell => 0,
            View::Sidebar => 1,
            View::TitleBar => 2,
        }
    }

    pub fn bump(view: View) {
        let n = COUNTS.with(|c| {
            let cell = &c[slot(view)];
            cell.set(cell.get() + 1);
            cell.get()
        });
        log::trace!("render {view:?} #{n}");
    }

    #[cfg(test)]
    pub fn count(view: View) -> u64 {
        COUNTS.with(|c| c[slot(view)].get())
    }
}

/// Counts one render of `view`.
#[cfg(any(debug_assertions, test))]
pub fn bump(view: View) {
    imp::bump(view);
}

#[cfg(not(any(debug_assertions, test)))]
pub fn bump(_view: View) {}

/// Renders of `view` so far on this thread.
#[cfg(test)]
pub fn count(view: View) -> u64 {
    imp::count(view)
}
