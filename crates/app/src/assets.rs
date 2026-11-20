//! Embedded assets: the pixel icon set and the shark logo, served to gpui by path.

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
    // The logo is multi-tone, so it is drawn as an image (an svg would be flattened to a mask).
    // One file per mode (DESIGN.md "Logo por modo"): light art for dark/OLED, dark art for light.
    asset!("logo-dark-mode.png", "transparent-for-dark-mode/png/128x128@2x.png"),
    asset!("logo-light-mode.png", "transparent-for-light-mode/png/128x128@2x.png"),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}
