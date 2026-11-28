//! The camera of the graph: auto-fit, zoom around the pointer and pan. Pure math (no gpui), so it is
//! unit-tested; the view only feeds it pointer events and the measured size of the viewport.

/// Smallest zoom (the whole map of a very large session still fits at this scale).
pub const MIN_ZOOM: f32 = 0.25;
/// Largest zoom.
pub const MAX_ZOOM: f32 = 1.75;
/// How much of the content must stay inside the viewport when panning (px).
const KEEP: f32 = 64.0;
/// Space left around a node revealed by the keyboard (px).
const REVEAL_MARGIN: f32 = 24.0;

/// Where the content is: `pan` is where the content's top-left corner sits in the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cam {
    pub zoom: f32,
    pub pan: (f32, f32),
    /// Follows the content and the viewport automatically (until the user zooms or pans).
    pub fit: bool,
}

impl Default for Cam {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: (0.0, 0.0),
            fit: true,
        }
    }
}

/// The zoom at which `content` fits `view` (never above 1: small maps are not magnified).
pub fn fit_zoom(content: (f32, f32), view: (f32, f32)) -> f32 {
    if content.0 <= 0.0 || content.1 <= 0.0 || view.0 <= 0.0 || view.1 <= 0.0 {
        return 1.0;
    }
    (view.0 / content.0)
        .min(view.1 / content.1)
        .clamp(MIN_ZOOM, 1.0)
}

/// A camera that shows all of `content` in `view`: centered across, at the top.
pub fn fit(content: (f32, f32), view: (f32, f32)) -> Cam {
    let zoom = fit_zoom(content, view);
    Cam {
        zoom,
        pan: (((view.0 - content.0 * zoom) / 2.0).max(0.0), 0.0),
        fit: true,
    }
}

/// Keeps part of the content on screen.
pub fn clamp_pan(mut cam: Cam, content: (f32, f32), view: (f32, f32)) -> Cam {
    let (cw, ch) = (content.0 * cam.zoom, content.1 * cam.zoom);
    let keep_x = KEEP.min(cw).min(view.0);
    let keep_y = KEEP.min(ch).min(view.1);
    cam.pan.0 = cam.pan.0.clamp(keep_x - cw, view.0 - keep_x);
    cam.pan.1 = cam.pan.1.clamp(keep_y - ch, view.1 - keep_y);
    cam
}

/// Zooms by `factor` keeping the content point under `anchor` (viewport coordinates) in place.
pub fn zoom_at(cam: Cam, factor: f32, anchor: (f32, f32), content: (f32, f32), view: (f32, f32)) -> Cam {
    let zoom = (cam.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let at = ((anchor.0 - cam.pan.0) / cam.zoom, (anchor.1 - cam.pan.1) / cam.zoom);
    clamp_pan(
        Cam {
            zoom,
            pan: (anchor.0 - at.0 * zoom, anchor.1 - at.1 * zoom),
            fit: false,
        },
        content,
        view,
    )
}

/// Moves the content by `(dx, dy)` screen pixels.
pub fn pan_by(cam: Cam, dx: f32, dy: f32, content: (f32, f32), view: (f32, f32)) -> Cam {
    clamp_pan(
        Cam {
            pan: (cam.pan.0 + dx, cam.pan.1 + dy),
            fit: false,
            ..cam
        },
        content,
        view,
    )
}

/// Scrolls the least needed to show `rect` (`x, y, w, h` in content coordinates).
pub fn reveal(cam: Cam, rect: (f32, f32, f32, f32), content: (f32, f32), view: (f32, f32)) -> Cam {
    let z = cam.zoom;
    let (x0, y0) = (cam.pan.0 + rect.0 * z, cam.pan.1 + rect.1 * z);
    let (x1, y1) = (x0 + rect.2 * z, y0 + rect.3 * z);
    let shift = |lo: f32, hi: f32, size: f32| -> f32 {
        if hi - lo + 2.0 * REVEAL_MARGIN >= size {
            // does not fit with margins: align its start
            REVEAL_MARGIN - lo
        } else if lo < REVEAL_MARGIN {
            REVEAL_MARGIN - lo
        } else if hi > size - REVEAL_MARGIN {
            size - REVEAL_MARGIN - hi
        } else {
            0.0
        }
    };
    let (dx, dy) = (shift(x0, x1, view.0), shift(y0, y1, view.1));
    if dx == 0.0 && dy == 0.0 {
        return cam;
    }
    pan_by(cam, dx, dy, content, view)
}

/// Zoom factor of a wheel movement of `dy` pixels (up zooms in).
pub fn wheel_factor(dy: f32) -> f32 {
    (-dy * 0.0015).exp().clamp(0.5, 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f32, f32) = (400.0, 300.0);

    #[test]
    fn small_maps_are_not_magnified_and_big_ones_shrink_to_fit() {
        assert_eq!(fit_zoom((200.0, 100.0), VIEW), 1.0);
        let z = fit_zoom((800.0, 300.0), VIEW);
        assert!((z - 0.5).abs() < 1e-6);
        let z = fit_zoom((400.0, 1200.0), VIEW);
        assert!((z - 0.25).abs() < 1e-6, "height limits: {z}");
        assert_eq!(fit_zoom((100_000.0, 100_000.0), VIEW), MIN_ZOOM);
        assert_eq!(fit_zoom((0.0, 0.0), VIEW), 1.0);
    }

    #[test]
    fn fit_centers_across_and_starts_at_the_top() {
        let c = fit((200.0, 100.0), VIEW);
        assert!(c.fit);
        assert_eq!(c.pan, (100.0, 0.0));
        let c = fit((800.0, 300.0), VIEW);
        assert_eq!((c.zoom, c.pan), (0.5, (0.0, 0.0)));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let content = (800.0, 600.0);
        let c0 = Cam {
            zoom: 1.0,
            pan: (-50.0, -20.0),
            fit: true,
        };
        let anchor = (150.0, 120.0);
        let before = ((anchor.0 - c0.pan.0) / c0.zoom, (anchor.1 - c0.pan.1) / c0.zoom);
        let c1 = zoom_at(c0, 1.5, anchor, content, VIEW);
        let after = ((anchor.0 - c1.pan.0) / c1.zoom, (anchor.1 - c1.pan.1) / c1.zoom);
        assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3);
        assert!(!c1.fit, "a manual zoom stops the auto-fit");
        assert!((c1.zoom - 1.5).abs() < 1e-6);
    }

    #[test]
    fn zoom_is_clamped() {
        let c = Cam::default();
        let up = zoom_at(c, 100.0, (0.0, 0.0), (800.0, 600.0), VIEW);
        assert_eq!(up.zoom, MAX_ZOOM);
        let down = zoom_at(c, 0.0001, (0.0, 0.0), (800.0, 600.0), VIEW);
        assert_eq!(down.zoom, MIN_ZOOM);
    }

    #[test]
    fn panning_cannot_lose_the_content() {
        let content = (800.0, 600.0);
        let c = pan_by(Cam::default(), 10_000.0, 10_000.0, content, VIEW);
        assert!(c.pan.0 <= VIEW.0 - KEEP && c.pan.1 <= VIEW.1 - KEEP);
        let c = pan_by(Cam::default(), -10_000.0, -10_000.0, content, VIEW);
        assert!(c.pan.0 >= KEEP - content.0 && c.pan.1 >= KEEP - content.1);
        assert!(!c.fit);
    }

    #[test]
    fn reveal_scrolls_only_when_the_node_is_off_screen() {
        let content = (1000.0, 800.0);
        let c = Cam {
            fit: false,
            ..Cam::default()
        };
        // visible: nothing moves
        assert_eq!(reveal(c, (50.0, 50.0, 100.0, 40.0), content, VIEW), c);
        // far to the right and down: pan so it ends inside with a margin
        let r = reveal(c, (900.0, 700.0, 100.0, 40.0), content, VIEW);
        assert!(r.pan.0 + 900.0 >= 0.0 && r.pan.0 + 1000.0 <= VIEW.0, "{:?}", r.pan);
        assert!(r.pan.1 + 740.0 <= VIEW.1);
    }

    #[test]
    fn the_wheel_zooms_in_when_scrolled_up() {
        assert!(wheel_factor(-40.0) > 1.0);
        assert!(wheel_factor(40.0) < 1.0);
        assert_eq!(wheel_factor(0.0), 1.0);
        assert!(wheel_factor(-100_000.0) <= 2.0);
    }
}
