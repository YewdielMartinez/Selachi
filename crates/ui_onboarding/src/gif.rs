//! The shark GIF of the welcome step.
//!
//! * Not embedded in the binary: it is a file (`assets/onboarding/shark.gif`) resolved next to
//!   the exe, falling back to the repository's `assets` folder in dev builds.
//! * Decoded on the background executor into one gpui image per frame (BGRA, like gpui's own
//!   GIF loader), so the view can advance frames with its own timer only while the step is on
//!   screen, and release every frame (CPU copy and GPU atlas) when the step is left.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::RenderImage;
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, Frame};

/// Path of the GIF relative to an `assets` root.
pub const REL_PATH: &str = "assets/onboarding/shark.gif";

/// Delay used when a frame has none (browsers do the same for 0 and 10 ms).
const DEFAULT_DELAY: Duration = Duration::from_millis(100);
const MIN_DELAY: Duration = Duration::from_millis(20);

/// Where the shark is: next to the exe (installed), in an ancestor of the exe (cargo's
/// `target\<profile>`) or at the repository path baked in at compile time (dev builds).
pub fn resolve_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(REL_PATH));
        candidates.push(dir.join("onboarding").join("shark.gif"));
        candidates.extend(dir.ancestors().skip(1).take(4).map(|a| a.join(REL_PATH)));
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(REL_PATH),
    );
    candidates.into_iter().find(|p| p.is_file())
}

/// One decoded frame, ready to show.
#[derive(Clone)]
pub struct SharkFrame {
    pub image: Arc<RenderImage>,
    pub delay: Duration,
}

/// Decode GIF bytes into BGRA frames. Frames are used as they come (the asset is made of full
/// frames); a frame at an offset or of another size is composited onto the previous canvas.
pub fn decode(bytes: &[u8]) -> Result<Vec<SharkFrame>, String> {
    let decoder = GifDecoder::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut out: Vec<SharkFrame> = Vec::new();
    let mut canvas: Option<image::RgbaImage> = None;
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|e| e.to_string())?;
        let delay = Duration::from(frame.delay());
        let delay = if delay.is_zero() {
            DEFAULT_DELAY
        } else {
            delay.max(MIN_DELAY)
        };
        let (left, top) = (frame.left(), frame.top());
        let buffer = frame.into_buffer();
        let full = match &mut canvas {
            Some(c) if (left, top) != (0, 0) || buffer.dimensions() != c.dimensions() => {
                image::imageops::overlay(c, &buffer, i64::from(left), i64::from(top));
                c.clone()
            }
            _ => {
                canvas = Some(buffer.clone());
                buffer
            }
        };
        let mut bgra = full;
        for px in bgra.pixels_mut() {
            px.0.swap(0, 2);
        }
        out.push(SharkFrame {
            image: Arc::new(RenderImage::new(vec![Frame::new(bgra)])),
            delay,
        });
    }
    if out.is_empty() {
        return Err("el GIF no tiene fotogramas".to_string());
    }
    Ok(out)
}

/// Read and decode the GIF at `path`. Blocking: background executor only.
pub fn load(path: &Path) -> Result<Vec<SharkFrame>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_shipped_shark() {
        let path = resolve_path().expect("assets/onboarding/shark.gif must be found from the repo");
        let frames = load(&path).unwrap();
        assert_eq!(frames.len(), 30);
        assert!(frames.iter().all(|f| f.image.as_bytes(0).is_some()));
        assert!(frames.iter().all(|f| f.delay >= MIN_DELAY));
        let size = frames[0].image.size(0);
        assert_eq!((size.width.0, size.height.0), (640, 360));
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"not a gif").is_err());
    }
}
