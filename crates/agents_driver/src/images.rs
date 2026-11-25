//! Image attachment extraction for agent prompts.
//!
//! `chat_core::compose_prompt` appends attached file paths under a standard header:
//! ```text
//! Archivos adjuntos (léelos con tus herramientas):
//! - C:\path\to\image.png
//! ```
//! Drivers call [`extract_attached_images`] to turn any existing image files in that list
//! into native image content blocks (ACP `image`, Claude `image` base64, Codex `localImage`).

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Maximum image file size we inline into a JSON-RPC / stream-json message (20 MiB).
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Header `chat_core::runtime::compose_prompt` writes before the `- <path>` bullet list (it uses
/// this constant, so the two never drift).
pub const ATTACHMENTS_HEADER: &str = "Archivos adjuntos (léelos con tus herramientas):";

/// A local image file ready to send as a protocol-native image content block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachment {
    pub path: PathBuf,
    pub mime_type: &'static str,
    pub base64: String,
}

/// MIME type for a supported image path extension, or `None` for non-images.
pub fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// Extracts and base64-encodes any existing image files listed in the prompt's attachments section.
///
/// Missing, unreadable, non-image, or oversized (`> MAX_IMAGE_BYTES`) files are skipped quietly
/// (their path remains in the text block as a fallback).
pub fn extract_attached_images(prompt: &str) -> Vec<ImageAttachment> {
    let Some(pos) = prompt.rfind(ATTACHMENTS_HEADER) else {
        return Vec::new();
    };
    let rest = &prompt[pos + ATTACHMENTS_HEADER.len()..];
    let mut out = Vec::new();
    for line in rest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(raw) = trimmed.strip_prefix("- ") else {
            break;
        };
        let path = PathBuf::from(raw.trim());
        let Some(mime_type) = image_mime(&path) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_IMAGE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        out.push(ImageAttachment {
            path,
            mime_type,
            base64: BASE64.encode(&bytes),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_mime_recognizes_common_formats_case_insensitively() {
        assert_eq!(image_mime(Path::new("a.png")), Some("image/png"));
        assert_eq!(image_mime(Path::new("A.PNG")), Some("image/png"));
        assert_eq!(image_mime(Path::new("photo.jpg")), Some("image/jpeg"));
        assert_eq!(image_mime(Path::new("photo.JPEG")), Some("image/jpeg"));
        assert_eq!(image_mime(Path::new("anim.gif")), Some("image/gif"));
        assert_eq!(image_mime(Path::new("pic.webp")), Some("image/webp"));
        assert_eq!(image_mime(Path::new("shot.bmp")), Some("image/bmp"));
        assert_eq!(image_mime(Path::new("main.rs")), None);
        assert_eq!(image_mime(Path::new("README")), None);
    }

    #[test]
    fn extract_attached_images_reads_only_existing_images() {
        let dir = std::env::temp_dir().join(format!(
            "selachi-img-test-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let png = dir.join("shot.png");
        let jpg = dir.join("pic.jpg");
        let rs = dir.join("main.rs");
        let missing = dir.join("missing.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n1234").unwrap();
        std::fs::write(&jpg, b"\xFF\xD8\xFF\xE0abcd").unwrap();
        std::fs::write(&rs, b"fn main() {}").unwrap();

        let prompt = format!(
            "Describe estas imágenes\n\n{ATTACHMENTS_HEADER}\n- {}\n- {}\n- {}\n- {}\n",
            png.display(),
            rs.display(),
            missing.display(),
            jpg.display(),
        );
        let imgs = extract_attached_images(&prompt);
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].path, png);
        assert_eq!(imgs[0].mime_type, "image/png");
        assert_eq!(imgs[0].base64, BASE64.encode(b"\x89PNG\r\n\x1a\n1234"));
        assert_eq!(imgs[1].path, jpg);
        assert_eq!(imgs[1].mime_type, "image/jpeg");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn acp_claude_and_codex_emit_image_content_blocks() {
        let dir = std::env::temp_dir().join(format!(
            "selachi-driver-img-test-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let png = dir.join("clip.png");
        let raw = b"\x89PNG\r\n\x1a\nhello";
        std::fs::write(&png, raw).unwrap();
        let prompt = format!(
            "Mira esto\n\n{ATTACHMENTS_HEADER}\n- {}\n",
            png.display()
        );

        // Claude stream-json user message
        let claude_msg = crate::claude::user_message_json(&prompt);
        let content = claude_msg["message"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], BASE64.encode(raw));

        // Codex app-server input
        let codex_in = crate::codex::CodexSession::input_for_test(&prompt);
        let items = codex_in.as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "text");
        assert_eq!(items[1]["type"], "localImage");
        assert_eq!(items[1]["path"], png.to_string_lossy().as_ref());

        // ACP session with promptCapabilities.image = true
        let mut acp = crate::acp::AcpSession::new(&crate::ThreadConfig::new(&dir));
        let _ = acp.start();
        let _ = acp.on_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "promptCapabilities": { "image": true }
                }
            }
        }));
        let _ = acp.on_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "sessionId": "s1" }
        }));
        let step = acp.user_message(&prompt);
        let blocks = step.outgoing[0]["params"]["prompt"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["mimeType"], "image/png");
        assert_eq!(blocks[1]["data"], BASE64.encode(raw));

        // ACP session without promptCapabilities.image: text only
        let mut acp_no_img = crate::acp::AcpSession::new(&crate::ThreadConfig::new(&dir));
        let _ = acp_no_img.start();
        let _ = acp_no_img.on_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": { "promptCapabilities": {} }
            }
        }));
        let _ = acp_no_img.on_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "sessionId": "s2" }
        }));
        let step_no_img = acp_no_img.user_message(&prompt);
        let blocks_no_img = step_no_img.outgoing[0]["params"]["prompt"].as_array().unwrap();
        assert_eq!(blocks_no_img.len(), 1);
        assert_eq!(blocks_no_img[0]["type"], "text");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
