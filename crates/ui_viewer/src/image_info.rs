//! Pixel dimensions of PNG / GIF / BMP / JPEG files from their headers (no decoding), for the
//! header chip of the image view and the 1:1 zoom label.

/// `(width, height)` or `None` when the format is not recognised or the header is cut short.
pub fn image_dimensions(head: &[u8]) -> Option<(u32, u32)> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        // IHDR: 4 length + "IHDR" + width + height, all big endian.
        let w = u32::from_be_bytes(head.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(head.get(20..24)?.try_into().ok()?);
        return Some((w, h));
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        let w = u16::from_le_bytes(head.get(6..8)?.try_into().ok()?);
        let h = u16::from_le_bytes(head.get(8..10)?.try_into().ok()?);
        return Some((u32::from(w), u32::from(h)));
    }
    if head.starts_with(b"BM") {
        let w = i32::from_le_bytes(head.get(18..22)?.try_into().ok()?);
        let h = i32::from_le_bytes(head.get(22..26)?.try_into().ok()?);
        return Some((w.unsigned_abs(), h.unsigned_abs()));
    }
    if head.starts_with(&[0xFF, 0xD8]) {
        return jpeg_dimensions(head);
    }
    None
}

fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        // fill bytes and standalone markers
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        // SOF0..SOF15 except DHT (C4), JPG (C8) and DAC (CC)
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes(b.get(i + 5..i + 7)?.try_into().ok()?);
            let w = u16::from_be_bytes(b.get(i + 7..i + 9)?.try_into().ok()?);
            return Some((u32::from(w), u32::from(h)));
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png() {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        b.extend(256u32.to_be_bytes());
        b.extend(128u32.to_be_bytes());
        assert_eq!(image_dimensions(&b), Some((256, 128)));
        assert_eq!(image_dimensions(&b[..20]), None);
    }

    #[test]
    fn gif_and_bmp() {
        let mut g = b"GIF89a".to_vec();
        g.extend(64u16.to_le_bytes());
        g.extend(32u16.to_le_bytes());
        assert_eq!(image_dimensions(&g), Some((64, 32)));

        let mut bmp = vec![0u8; 26];
        bmp[0] = b'B';
        bmp[1] = b'M';
        bmp[18..22].copy_from_slice(&10i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-20i32).to_le_bytes());
        assert_eq!(image_dimensions(&bmp), Some((10, 20)));
    }

    #[test]
    fn jpeg_skips_to_the_frame_header() {
        let mut j = vec![0xFF, 0xD8];
        // APP0 with 14 bytes of payload
        j.extend([0xFF, 0xE0, 0x00, 0x10]);
        j.extend([0u8; 14]);
        // SOF0: len 17, precision 8, height 480, width 640
        j.extend([0xFF, 0xC0, 0x00, 0x11, 0x08]);
        j.extend(480u16.to_be_bytes());
        j.extend(640u16.to_be_bytes());
        j.extend([0u8; 10]);
        assert_eq!(image_dimensions(&j), Some((640, 480)));
    }

    #[test]
    fn unknown_data_is_none() {
        assert_eq!(image_dimensions(b"hello world, definitely not an image"), None);
        assert_eq!(image_dimensions(&[]), None);
    }
}
