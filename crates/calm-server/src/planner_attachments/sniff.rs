//! Magic-number sniffing. The declared `Content-Type` is never the judgement; the extension the file is stored under, and hence the read-back `Content-Type`, comes from here.

use calm_types::planner_attachment::AttachmentFormat;

/// Bytes needed before [`sniff`] can answer for every format; WebP is the longest (`RIFF` at 0, `WEBP` at 8).
pub const SNIFF_PREFIX_BYTES: usize = 12;

/// Identify one of the four accepted formats from a file's leading bytes. No fallback branch: a format we cannot name is one codex would silently re-encode or replace with placeholder text.
pub fn sniff(prefix: &[u8]) -> Option<AttachmentFormat> {
    const PNG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
    const GIF: &[u8] = b"GIF8";
    const RIFF: &[u8] = b"RIFF";
    const WEBP: &[u8] = b"WEBP";

    if prefix.starts_with(PNG) {
        return Some(AttachmentFormat::Png);
    }
    if prefix.starts_with(JPEG) {
        return Some(AttachmentFormat::Jpeg);
    }
    if prefix.starts_with(GIF) {
        return Some(AttachmentFormat::Gif);
    }
    if prefix.starts_with(RIFF) && prefix.len() >= 12 && &prefix[8..12] == WEBP {
        return Some(AttachmentFormat::Webp);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_accepted_format_is_recognised_from_its_magic_number() {
        assert_eq!(
            sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]),
            Some(AttachmentFormat::Png)
        );
        assert_eq!(
            sniff(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(AttachmentFormat::Jpeg)
        );
        assert_eq!(sniff(b"GIF89a......"), Some(AttachmentFormat::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(AttachmentFormat::Webp));
    }

    #[test]
    fn refused_formats_have_no_fallback_branch() {
        // SVG is the one that matters: an executable document that `readfile-raw`'s extension table still serves as `image/svg+xml`.
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"), None);
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><svg/>"), None);
        assert_eq!(sniff(b"BM\x36\x00\x00\x00"), None, "bmp");
        assert_eq!(sniff(&[0x00, 0x00, 0x01, 0x00]), None, "ico");
        assert_eq!(sniff(b"%PDF-1.7"), None, "pdf");
        assert_eq!(sniff(b""), None, "empty");
        assert_eq!(sniff(b"RIFF\0\0\0\0AVI "), None, "riff container, not webp");
        assert_eq!(sniff(b"RIFF\0\0\0\0WEB"), None, "truncated webp header");
    }
}
