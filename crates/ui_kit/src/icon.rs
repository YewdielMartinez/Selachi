//! The pixel icon set: 16x16 grids of 1 px rects, tinted through `text_color`.

use gpui::{Hsla, Svg, prelude::*, px, svg};

/// One icon of the embedded set (`assets/icons/<name>.svg`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Terminal,
    Files,
    GitBranch,
    Agents,
    History,
    Settings,
    Plus,
    ChevronDown,
    ChevronRight,
    Close,
    Minimize,
    Maximize,
    Restore,
    Check,
    Alert,
    Dots,
    Layout,
    Search,
    Folder,
    Snippet,
    Task,
    Procs,
    Doctor,
    FolderOpen,
    Usage,
    Review,
    Attention,
    Send,
    Trash,
    Edit,
    /// 10 px gutter marker of a review note (the other icons are 16 px).
    Note,
}

impl Icon {
    /// Every icon, for galleries and tests.
    pub const ALL: [Icon; 31] = [
        Icon::Terminal,
        Icon::Files,
        Icon::GitBranch,
        Icon::Agents,
        Icon::History,
        Icon::Settings,
        Icon::Plus,
        Icon::ChevronDown,
        Icon::ChevronRight,
        Icon::Close,
        Icon::Minimize,
        Icon::Maximize,
        Icon::Restore,
        Icon::Check,
        Icon::Alert,
        Icon::Dots,
        Icon::Layout,
        Icon::Search,
        Icon::Folder,
        Icon::Snippet,
        Icon::Task,
        Icon::Procs,
        Icon::Doctor,
        Icon::FolderOpen,
        Icon::Usage,
        Icon::Review,
        Icon::Attention,
        Icon::Send,
        Icon::Trash,
        Icon::Edit,
        Icon::Note,
    ];

    /// File stem of the svg (`"git-branch"`).
    pub const fn name(self) -> &'static str {
        match self {
            Icon::Terminal => "terminal",
            Icon::Files => "files",
            Icon::GitBranch => "git-branch",
            Icon::Agents => "agents",
            Icon::History => "history",
            Icon::Settings => "settings",
            Icon::Plus => "plus",
            Icon::ChevronDown => "chevron-down",
            Icon::ChevronRight => "chevron-right",
            Icon::Close => "close",
            Icon::Minimize => "minimize",
            Icon::Maximize => "maximize",
            Icon::Restore => "restore",
            Icon::Check => "check",
            Icon::Alert => "alert",
            Icon::Dots => "dots",
            Icon::Layout => "layout",
            Icon::Search => "search",
            Icon::Folder => "folder",
            Icon::Snippet => "snippet",
            Icon::Task => "task",
            Icon::Procs => "procs",
            Icon::Doctor => "doctor",
            Icon::FolderOpen => "folder-open",
            Icon::Usage => "usage",
            Icon::Review => "review",
            Icon::Attention => "attention",
            Icon::Send => "send",
            Icon::Trash => "trash",
            Icon::Edit => "edit",
            Icon::Note => "note",
        }
    }

    /// Asset path served by [`crate::Assets`].
    pub fn path(self) -> String {
        format!("icons/{}.svg", self.name())
    }
}

/// An embedded icon drawn at `size` logical px in `color`.
///
/// ```ignore
/// ui_kit::icon(Icon::Terminal, 16.0, theme.colors.muted)
/// ```
pub fn icon(icon: Icon, size: f32, color: impl Into<Hsla>) -> Svg {
    svg().path(icon.path()).flex_none().size(px(size)).text_color(color)
}
