//! Embedded assets: the pixel icon set and the shark logo, served to gpui by path.
//!
//! Register once with `Application::new().with_assets(ui_kit::Assets)`.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

macro_rules! asset {
    ($path:literal, $file:literal) => {
        (
            $path,
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/", $file)).as_slice(),
        )
    };
}

const ASSETS: &[(&str, &[u8])] = &[
    asset!("icons/terminal.svg", "icons/terminal.svg"),
    asset!("icons/files.svg", "icons/files.svg"),
    asset!("icons/git-branch.svg", "icons/git-branch.svg"),
    asset!("icons/agents.svg", "icons/agents.svg"),
    asset!("icons/history.svg", "icons/history.svg"),
    asset!("icons/settings.svg", "icons/settings.svg"),
    asset!("icons/plus.svg", "icons/plus.svg"),
    asset!("icons/chevron-down.svg", "icons/chevron-down.svg"),
    asset!("icons/chevron-right.svg", "icons/chevron-right.svg"),
    asset!("icons/close.svg", "icons/close.svg"),
    asset!("icons/minimize.svg", "icons/minimize.svg"),
    asset!("icons/maximize.svg", "icons/maximize.svg"),
    asset!("icons/restore.svg", "icons/restore.svg"),
    asset!("icons/check.svg", "icons/check.svg"),
    asset!("icons/alert.svg", "icons/alert.svg"),
    asset!("icons/dots.svg", "icons/dots.svg"),
    asset!("icons/layout.svg", "icons/layout.svg"),
    asset!("icons/search.svg", "icons/search.svg"),
    asset!("icons/folder.svg", "icons/folder.svg"),
    asset!("icons/snippet.svg", "icons/snippet.svg"),
    asset!("icons/task.svg", "icons/task.svg"),
    asset!("icons/procs.svg", "icons/procs.svg"),
    asset!("icons/doctor.svg", "icons/doctor.svg"),
    asset!("icons/folder-open.svg", "icons/folder-open.svg"),
    asset!("icons/file-generic.svg", "icons/file-generic.svg"),
    asset!("icons/file-code.svg", "icons/file-code.svg"),
    asset!("icons/file-ts.svg", "icons/file-ts.svg"),
    asset!("icons/file-js.svg", "icons/file-js.svg"),
    asset!("icons/file-json.svg", "icons/file-json.svg"),
    asset!("icons/file-html.svg", "icons/file-html.svg"),
    asset!("icons/file-css.svg", "icons/file-css.svg"),
    asset!("icons/file-markdown.svg", "icons/file-markdown.svg"),
    asset!("icons/file-config.svg", "icons/file-config.svg"),
    asset!("icons/file-image.svg", "icons/file-image.svg"),
    asset!("icons/file-lock.svg", "icons/file-lock.svg"),
    asset!("icons/file-go.svg", "icons/file-go.svg"),
    asset!("icons/file-rust.svg", "icons/file-rust.svg"),
    asset!("icons/file-python.svg", "icons/file-python.svg"),
    asset!("icons/file-shell.svg", "icons/file-shell.svg"),
    asset!("icons/usage.svg", "icons/usage.svg"),
    asset!("icons/review.svg", "icons/review.svg"),
    asset!("icons/attention.svg", "icons/attention.svg"),
    asset!("icons/send.svg", "icons/send.svg"),
    asset!("icons/trash.svg", "icons/trash.svg"),
    asset!("icons/edit.svg", "icons/edit.svg"),
    asset!("icons/note.svg", "icons/note.svg"),
    asset!("icons/agents/claude.svg", "icons/agents/claude.svg"),
    asset!("icons/agents/codex.svg", "icons/agents/codex.svg"),
    asset!("icons/agents/gemini.svg", "icons/agents/gemini.svg"),
    asset!("icons/agents/opencode.svg", "icons/agents/opencode.svg"),
    asset!("icons/agents/cursor.svg", "icons/agents/cursor.svg"),
    asset!("icons/agents/copilot.svg", "icons/agents/copilot.svg"),
    asset!("icons/agents/windsurf.svg", "icons/agents/windsurf.svg"),
    // The logo is multi-tone, so it is drawn as an image (an svg would be flattened to a mask).
    // One file per mode (DESIGN.md "Logo por modo"): light art for dark/OLED, dark art for light.
    asset!("logo-dark-mode.png", "transparent-for-dark-mode/png/128x128@2x.png"),
    asset!("logo-light-mode.png", "transparent-for-light-mode/png/128x128@2x.png"),
];

/// The embedded asset source (icons + logos).
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS.iter().find(|(p, _)| *p == path).map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_and_logo_is_embedded() {
        for icon in crate::Icon::ALL {
            let path = icon.path();
            assert!(Assets.load(&path).unwrap().is_some(), "{path} missing");
        }
        for fi in crate::FileIcon::ALL {
            let path = fi.path();
            assert!(Assets.load(&path).unwrap().is_some(), "{path} missing");
        }
        for mark in crate::AgentMark::ALL {
            let path = mark.path();
            assert!(Assets.load(&path).unwrap().is_some(), "{path} missing");
        }
        for mode in [theme::Mode::Dark, theme::Mode::Light, theme::Mode::Oled] {
            assert!(Assets.load(crate::logo::logo_asset(mode)).unwrap().is_some());
        }
    }
}
