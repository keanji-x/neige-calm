//! Magic-number sniffing. The declared `Content-Type` is never the judgement.
//!
//! A header is whatever the client typed; the bytes are what codex will try to
//! decode. When they disagree the bytes win, and the extension the file is
//! stored under — hence the `Content-Type` a later read-back sends — comes from
//! here. That is also what makes an SVG uploaded as `image/png` harmless: it is
//! refused unless it actually begins with PNG's magic number, and if it does,
//! it is served as `image/png` with `nosniff` and a sandbox CSP, so no browser
//! is ever invited to run it as markup.

use calm_types::planner_attachment::AttachmentFormat;

/// Bytes needed before [`sniff`] can answer for every format. WebP is the
/// longest: `RIFF` at 0, `WEBP` at 8.
pub const SNIFF_PREFIX_BYTES: usize = 12;

/// Identify one of the four accepted formats from a file's leading bytes.
///
/// `None` means "not one of the four" and is the answer for SVG, BMP, ICO,
/// PDF, a truncated file, and anything else. There is no fallback branch: a
/// format we cannot name is a format codex would silently re-encode or replace
/// with placeholder text, and neither is visible to the person who uploaded it.
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
        // SVG is the one that matters: it is an executable document, and
        // `readfile-raw`'s extension table still serves `image/svg+xml`. The
        // narrowing is on this upload path, not on that reader.
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
