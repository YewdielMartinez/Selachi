//! Runtime pixel icons (16x16 grids of 1 px rects, tinted through `text_color`), served by path.
//!
//! `ui_kit::Assets` only knows the shared icon set, so these live here. Register
//! [`Assets`] instead of `ui_kit::Assets` (it forwards everything else to it), or call
//! [`load`] from the app's own composite `AssetSource`.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use proc_svc::RuntimeKind;

macro_rules! asset {
    ($path:literal) => {
        (
            $path,
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $path)).as_slice(),
        )
    };
}

/// Every asset of this crate, by the path the views ask for.
pub const ASSETS: &[(&str, &[u8])] = &[
    asset!("runtime/node.svg"),
    asset!("runtime/dotnet.svg"),
    asset!("runtime/python.svg"),
    asset!("runtime/rust.svg"),
    asset!("runtime/java.svg"),
    asset!("runtime/docker.svg"),
    asset!("runtime/bun.svg"),
    asset!("runtime/deno.svg"),
    asset!("runtime/go.svg"),
    asset!("runtime/shell.svg"),
    asset!("runtime/other.svg"),
    asset!("ui/arrow-up-right.svg"),
];

/// Path of the "open in browser" arrow.
pub const ARROW_UP_RIGHT: &str = "ui/arrow-up-right.svg";

/// Asset path of the pixel icon of a runtime.
pub const fn runtime_icon_path(kind: RuntimeKind) -> &'static str {
    match kind {
        RuntimeKind::Node => "runtime/node.svg",
        RuntimeKind::Dotnet => "runtime/dotnet.svg",
        RuntimeKind::Python => "runtime/python.svg",
        RuntimeKind::Rust => "runtime/rust.svg",
        RuntimeKind::Java => "runtime/java.svg",
        RuntimeKind::Docker => "runtime/docker.svg",
        RuntimeKind::Bun => "runtime/bun.svg",
        RuntimeKind::Deno => "runtime/deno.svg",
        RuntimeKind::Go => "runtime/go.svg",
        RuntimeKind::Shell => "runtime/shell.svg",
        RuntimeKind::Other => "runtime/other.svg",
    }
}

/// The bytes of one of this crate's assets.
pub fn load(path: &str) -> Option<&'static [u8]> {
    ASSETS
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, data)| *data)
}

/// `ui_kit::Assets` plus the runtime icons. Use it as the app's asset source.
///
/// ```ignore
/// Application::new().with_assets(ui_procs::Assets).run(|cx| { /* ... */ });
/// ```
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match load(path) {
            Some(data) => Ok(Some(Cow::Borrowed(data))),
            None => ui_kit::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = ui_kit::Assets.list(path)?;
        out.extend(
            ASSETS
                .iter()
                .filter(|(p, _)| p.starts_with(path))
                .map(|(p, _)| SharedString::from(*p)),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_runtime_has_an_icon_and_ui_kit_still_resolves() {
        for kind in [
            RuntimeKind::Node,
            RuntimeKind::Dotnet,
            RuntimeKind::Python,
            RuntimeKind::Rust,
            RuntimeKind::Java,
            RuntimeKind::Docker,
            RuntimeKind::Bun,
            RuntimeKind::Deno,
            RuntimeKind::Go,
            RuntimeKind::Shell,
            RuntimeKind::Other,
        ] {
            assert!(load(runtime_icon_path(kind)).is_some(), "{kind:?}");
        }
        assert!(Assets.load("icons/terminal.svg").unwrap().is_some());
        assert!(Assets.load("runtime/node.svg").unwrap().is_some());
        assert!(Assets.load("nope.svg").unwrap().is_none());
    }
}
