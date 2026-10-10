//! `window_cat`'s screenshot: watch the window first so it draws at the
//! watched rate (an unwatched window draws once a second and would show a
//! page up to a second old), let it settle, then capture and encode a PNG.

use std::time::{Duration, Instant};

use base64::Engine;
use compositor::{Compositor, Frame, WindowId};

/// Longest wait for the window to draw after it is watched.
pub const SETTLE_MAX: Duration = Duration::from_millis(800);
/// The window counts as settled once it has drawn and then been quiet this long.
pub const SETTLE_QUIET: Duration = Duration::from_millis(150);

pub struct Shot {
    pub size: (u32, u32),
    pub base64_png: String,
}

/// Watches `id`, waits for it to draw and settle, captures it and encodes
/// the capture. Blocking: run it off the async workers.
pub fn fresh_png(compositor: &Compositor, id: WindowId) -> compositor::Result<Shot> {
    let frame = settled_capture(compositor, id)?;
    Ok(Shot {
        size: frame.size,
        base64_png: base64::engine::general_purpose::STANDARD.encode(png(&frame)),
    })
}

fn settled_capture(compositor: &Compositor, id: WindowId) -> compositor::Result<Frame> {
    let watch = compositor.watch(id)?;
    let deadline = Instant::now() + SETTLE_MAX;
    // The watch starts with a capture of what the window shows now; only a
    // frame after it is fresh.
    watch.try_take()?;
    let mut drew = false;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait = if drew {
            SETTLE_QUIET.min(deadline - now)
        } else {
            deadline - now
        };
        match watch.take_timeout(wait)? {
            Some(_) => drew = true,
            None if drew => break,
            None => {}
        }
    }
    let frame = compositor.capture(id)?;
    drop(watch);
    Ok(frame)
}

/// Encodes an XRGB8888 frame (bytes B, G, R, X) as an 8-bit RGB PNG.
pub fn png(frame: &Frame) -> Vec<u8> {
    let (width, height) = frame.size;
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height as usize {
        let row = &frame.xrgb8888[y * frame.stride as usize..][..width as usize * 4];
        for [b, g, r, _] in row.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[*r, *g, *b]);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder
        .write_header()
        .expect("a PNG header for a valid size writes to memory");
    writer
        .write_image_data(&rgb)
        .expect("the image data matches the header");
    drop(writer);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn png_turns_xrgb_rows_with_padding_into_rgb() {
        // 2x2, stride 12 (4 bytes of padding per row); pixels as 0x00RRGGBB.
        let pixels: [[u32; 2]; 2] = [[0x00ff0000, 0x0000ff00], [0x000000ff, 0x00123456]];
        let mut bytes = Vec::new();
        for row in pixels {
            for px in row {
                bytes.extend_from_slice(&px.to_le_bytes());
            }
            bytes.extend_from_slice(&[0xee; 4]);
        }
        let frame = Frame {
            size: (2, 2),
            stride: 12,
            xrgb8888: Arc::from(bytes),
            damage: Vec::new(),
        };
        let decoder = png::Decoder::new(std::io::Cursor::new(png(&frame)));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(
            &buf[..info.buffer_size()],
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 0x12, 0x34, 0x56]
        );
    }
}
