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
    Map,
    FolderPlus,
    FolderMinus,
    Stop,
    Copy,
    Archive,
    Star,
    StarFilled,
    Appearance,
    Keyboard,
    Plugin,
    Info,
    Delegate,
    Update,
    SidebarLeft,
    Node,
    ArrowUpRight,
    Stage,
    Unstage,
    ArrowUp,
    ArrowDown,
    /// 10 px gutter marker of a review note (the other icons are 16 px).
    Note,
}

impl Icon {
    /// Every icon, for galleries and tests.
    pub const ALL: [Icon; 52] = [
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
        Icon::Map,
        Icon::FolderPlus,
        Icon::FolderMinus,
        Icon::Stop,
        Icon::Copy,
        Icon::Archive,
        Icon::Star,
        Icon::StarFilled,
        Icon::Appearance,
        Icon::Keyboard,
        Icon::Plugin,
        Icon::Info,
        Icon::Delegate,
        Icon::Update,
        Icon::SidebarLeft,
        Icon::Node,
        Icon::ArrowUpRight,
        Icon::Stage,
        Icon::Unstage,
        Icon::ArrowUp,
        Icon::ArrowDown,
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
            Icon::Map => "map",
            Icon::FolderPlus => "folder-plus",
            Icon::FolderMinus => "folder-minus",
            Icon::Stop => "stop",
            Icon::Copy => "copy",
            Icon::Archive => "archive",
            Icon::Star => "star",
            Icon::StarFilled => "star-filled",
            Icon::Appearance => "appearance",
            Icon::Keyboard => "keyboard",
            Icon::Plugin => "plugin",
            Icon::Info => "info",
            Icon::Delegate => "delegate",
            Icon::Update => "update",
            Icon::SidebarLeft => "sidebar-left",
            Icon::Node => "node",
            Icon::ArrowUpRight => "arrow-up-right",
            Icon::Stage => "stage",
            Icon::Unstage => "unstage",
            Icon::ArrowUp => "arrow-up",
            Icon::ArrowDown => "arrow-down",
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

#[cfg(test)]
mod svg_rules {
    use super::*;
    use std::path::PathBuf;

    fn icons_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons")
    }

    /// Value of `key="..."` inside a tag.
    fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
        let pat = format!(" {key}=\"");
        let start = tag.find(&pat)? + pat.len();
        Some(&tag[start..start + tag[start..].find('"')?])
    }

    /// Icons drawn on a 10 x 10 grid (window controls and the review note marker).
    const SMALL: [&str; 5] = ["close", "minimize", "maximize", "restore", "note"];
    /// Older files that predate the `shape-rendering` rule (their rects are on the grid anyway).
    const NO_CRISP_ATTR: [&str; 14] = [
        "agents",
        "attention",
        "doctor",
        "edit",
        "folder-open",
        "note",
        "procs",
        "review",
        "send",
        "snippet",
        "task",
        "trash",
        "usage",
        "file-*",
    ];

    fn check(name: &str, svg: &str) {
        let mut tags = svg.split('<').skip(1).map(|t| t.split('>').next().unwrap());
        let root = tags.next().unwrap();
        assert!(
            root.starts_with("svg "),
            "{name}: first element must be <svg>"
        );
        let want = if SMALL.contains(&name) { 10 } else { 16 };
        assert_eq!(
            attr(root, "viewBox"),
            Some(format!("0 0 {want} {want}").as_str()),
            "{name}: viewBox"
        );
        assert_eq!(
            attr(root, "fill"),
            Some("#fff"),
            "{name}: fill must be white"
        );
        let legacy = NO_CRISP_ATTR.contains(&name)
            || (name.starts_with("file-") && NO_CRISP_ATTR.contains(&"file-*"));
        if !legacy {
            assert_eq!(
                attr(root, "shape-rendering"),
                Some("crispEdges"),
                "{name}: shape-rendering"
            );
        }
        let mut rects = 0;
        for tag in tags {
            if tag.trim() == "/svg" {
                continue;
            }
            assert!(
                tag.starts_with("rect "),
                "{name}: only <rect> children, got <{tag}>"
            );
            rects += 1;
            let n = |k: &str| -> i32 {
                attr(tag, k)
                    .unwrap_or_else(|| panic!("{name}: rect without {k}"))
                    .parse()
                    .unwrap_or_else(|_| panic!("{name}: {k} is not an integer in <{tag}>"))
            };
            let (x, y, w, h) = (n("x"), n("y"), n("width"), n("height"));
            assert!(w > 0 && h > 0, "{name}: empty rect <{tag}>");
            assert!(
                x >= 0 && y >= 0 && x + w <= want && y + h <= want,
                "{name}: rect outside 0..{want}: <{tag}>"
            );
            for banned in ["stroke", "opacity", "transform", " rx=", " ry="] {
                assert!(!tag.contains(banned), "{name}: `{banned}` is not allowed");
            }
        }
        assert!(rects > 0, "{name}: no rects");
        for banned in ["<path", "<circle", "<g", "gradient", "<polygon", "<line"] {
            assert!(!svg.contains(banned), "{name}: `{banned}` is not allowed");
        }
    }

    #[test]
    fn every_icon_variant_has_a_svg_file() {
        let mut seen = std::collections::HashSet::new();
        for icon in Icon::ALL {
            assert!(
                seen.insert(icon.name()),
                "duplicate icon name {}",
                icon.name()
            );
            let file = icons_dir().join(format!("{}.svg", icon.name()));
            assert!(file.is_file(), "{} missing", file.display());
        }
    }

    #[test]
    fn every_icon_svg_follows_the_pixel_rules() {
        let mut count = 0;
        for entry in std::fs::read_dir(icons_dir()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "svg") {
                continue;
            }
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            check(&name, &std::fs::read_to_string(&path).unwrap());
            count += 1;
        }
        assert!(count >= Icon::ALL.len(), "only {count} svg files found");
    }
}
