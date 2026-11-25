//! Composer thumbnails: attached images shown at 28 px must not be decoded at full size.
//!
//! gpui's `img(path)` decodes the whole file into its global asset cache (a 4K screenshot is 33 MB
//! of pixels, kept for the life of the process, plus its atlas texture): four pasted screenshots
//! were +370 MB of private bytes. A thumbnail is decoded once, off the UI thread, and downscaled to
//! at most [`THUMB_PX`] on its long edge; the composer owns it and drops it with the attachment.

use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;

/// Long edge of a thumbnail (shown at 28 px; 64 keeps it sharp at 2x scale).
pub(crate) const THUMB_PX: u32 = 64;

/// The decoded, downscaled thumbnail of the image at `path` in gpui's pixel order (BGRA), or
/// `None` when the file cannot be decoded.
pub(crate) fn load_thumb(path: &Path) -> Option<Arc<RenderImage>> {
    let decoded = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let mut small = decoded.thumbnail(THUMB_PX, THUMB_PX).into_rgba8();
    drop(decoded);
    // gpui paints BGRA.
    for px in small.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(small)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_are_small_whatever_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        image::RgbaImage::from_fn(1600, 900, |x, y| image::Rgba([x as u8, y as u8, 7, 255]))
            .save(&path)
            .unwrap();
        let t = load_thumb(&path).expect("decodes");
        let size = t.size(0);
        assert!(size.width.0 <= THUMB_PX as i32 && size.height.0 <= THUMB_PX as i32);
        assert!(size.width.0 >= 1 && size.height.0 >= 1);
        // 16:9 stays 16:9 (within rounding).
        assert!(size.width.0 > size.height.0);
    }

    #[test]
    fn undecodable_files_give_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(load_thumb(&path).is_none());
        assert!(load_thumb(&dir.path().join("missing.png")).is_none());
    }
}
