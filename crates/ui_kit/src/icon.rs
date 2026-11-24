//! The pixel icon set: 16x16 grids of 1 px rects, tinted through `text_color`.

use gpui::{Hsla, Svg, Transformation, prelude::*, px, radians, svg};

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

/// A clockwise quarter-turn rotation applied in code, so `chevron-up` / `chevron-left` come from
/// `chevron-down` / `chevron-right` without new SVG files (the PLAN section 16 contract: no new
/// SVGs). The rotation is around the icon center and affects painting only, not layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    None,
    Deg90,
    Deg180,
    Deg270,
}

impl Rotation {
    /// Clockwise angle in degrees.
    pub const fn degrees(self) -> f32 {
        match self {
            Rotation::None => 0.0,
            Rotation::Deg90 => 90.0,
            Rotation::Deg180 => 180.0,
            Rotation::Deg270 => 270.0,
        }
    }

    /// Applies the rotation to an svg element (a no-op for [`Rotation::None`]).
    pub fn apply(self, svg: Svg) -> Svg {
        match self {
            Rotation::None => svg,
            r => svg.with_transformation(Transformation::rotate(radians(r.degrees().to_radians()))),
        }
    }
}

impl Icon {
    /// This icon turned by `rotation` (see [`Rotation`]).
    pub const fn rotate(self, rotation: Rotation) -> RotatedIcon {
        RotatedIcon {
            icon: self,
            rotation,
        }
    }
}

/// An [`Icon`] plus a [`Rotation`]: `Icon::ChevronDown.rotate(Rotation::Deg180)` is chevron-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotatedIcon {
    pub icon: Icon,
    pub rotation: Rotation,
}

impl RotatedIcon {
    /// Draws it at `size` logical px in `color`, like [`icon`].
    pub fn svg(self, size: f32, color: impl Into<Hsla>) -> Svg {
        icon_rotated(self.icon, self.rotation, size, color)
    }
}

/// [`icon`] turned by `rotation`.
pub fn icon_rotated(icon_: Icon, rotation: Rotation, size: f32, color: impl Into<Hsla>) -> Svg {
    rotation.apply(icon(icon_, size, color))
}

/// An embedded icon drawn at `size` logical px in `color`.
///
/// ```ignore
/// ui_kit::icon(Icon::Terminal, 16.0, theme.colors.muted)
/// ```
pub fn icon(icon: Icon, size: f32, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(icon.path())
        .flex_none()
        .size(px(size))
        .text_color(color)
}
