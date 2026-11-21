//! What the viewer panels tell the host. The host decides what to do (open a tab, spawn the editor,
//! start a grammar download): the panels never reach outside themselves.

use std::path::PathBuf;

/// Emitted (through gpui's `EventEmitter`) by [`crate::FileTreePanel`], [`crate::FileViewer`] and
/// [`crate::AgentTouchesView`]. The providers also accept an `on_event` callback so the host does not
/// need to downcast the built `AnyView`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerEvent {
    /// A file was activated in the tree: open it in a viewer tab.
    OpenFile { path: PathBuf },
    /// A file was activated in the "what did the agent touch" list: open it in diff mode against
    /// `base` (a checkpoint commit) or HEAD when `None`.
    OpenDiff { path: PathBuf, base: Option<String> },
    /// "Abrir en editor": hand the file to VS Code / `$EDITOR`. `line` is 1-based.
    OpenInEditor { path: PathBuf, line: Option<usize> },
    /// "Instala el plugin de ...": the host downloads the grammar plugin `id` (see PLAN section 18).
    InstallGrammar { id: String },
}
