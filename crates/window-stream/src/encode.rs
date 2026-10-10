//! The version 1 encoder: one JPEG per frame.

use jpeg_encoder::{ColorType, Encoder};

use crate::frame::{Codec, EncodeError, EncodedFrame, Frame, FrameEncoder};

/// JPEG quality used when the caller has no reason to pick another.
pub const DEFAULT_JPEG_QUALITY: u8 = 75;

/// Encodes every frame as one baseline JPEG (4:2:0 chroma, quality 1–100).
#[derive(Clone)]
pub struct JpegEncoder {
    quality: u8,
    /// Tightly packed copy of the frame, used only when its stride has padding.
    packed: Vec<u8>,
}

impl JpegEncoder {
    /// `quality` is clamped to 1..=100.
    pub fn new(quality: u8) -> Self {
        Self {
            quality: quality.clamp(1, 100),
            packed: Vec::new(),
        }
    }

    pub fn quality(&self) -> u8 {
        self.quality
    }
}

impl FrameEncoder for JpegEncoder {
    fn codec(&self) -> Codec {
        Codec::Jpeg
    }

    fn encode(&mut self, frame: &Frame) -> Result<EncodedFrame, EncodeError> {
        let (width, height) = frame.size;
        let too_large = |side: u32| u16::try_from(side).map_err(|_| side);
        let (w, h) = match (too_large(width), too_large(height)) {
            (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
            _ => {
                return Err(EncodeError::BadFrame(format!(
                    "size {width}x{height} is outside 1..=65535"
                )));
            }
        };
        let row = width as usize * 4;
        let stride = frame.stride as usize;
        let needed = stride * (height as usize - 1) + row;
        if stride < row || frame.xrgb8888.len() < needed {
            return Err(EncodeError::BadFrame(format!(
                "stride {stride} and {} bytes do not hold {width}x{height} XRGB8888",
                frame.xrgb8888.len()
            )));
        }
        // XRGB8888 is little-endian 0xXXRRGGBB, so its bytes are B, G, R, X:
        // jpeg-encoder's BGRA layout with the fourth byte ignored.
        let pixels: &[u8] = if stride == row {
            &frame.xrgb8888[..row * height as usize]
        } else {
            self.packed.clear();
            for y in 0..height as usize {
                self.packed
                    .extend_from_slice(&frame.xrgb8888[y * stride..y * stride + row]);
            }
            &self.packed
        };
        let mut data = Vec::with_capacity(row * height as usize / 16);
        Encoder::new(&mut data, self.quality)
            .encode(pixels, w, h, ColorType::Bgra)
            .map_err(|e| EncodeError::Encoder(e.to_string()))?;
        Ok(EncodedFrame {
            keyframe: true,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::frame::Rect;

    const RED: [u8; 4] = [0x00, 0x00, 0xff, 0x00];
    const BLUE: [u8; 4] = [0xff, 0x00, 0x00, 0x00];

    /// Left half red, right half blue, rows padded to `stride` with green garbage.
    fn split_frame(width: u32, height: u32, stride: u32) -> Frame {
        let mut pixels = vec![0u8; (stride * height) as usize];
        for y in 0..height {
            for x in 0..stride / 4 {
                let px = match x {
                    x if x >= width => [0x00, 0xff, 0x00, 0x00],
                    x if x < width / 2 => RED,
                    _ => BLUE,
                };
                let at = (y * stride + x * 4) as usize;
                pixels[at..at + 4].copy_from_slice(&px);
            }
        }
        Frame {
            size: (width, height),
            stride,
            xrgb8888: Arc::from(pixels),
            damage: vec![Rect {
                x: 0,
                y: 0,
                width,
                height,
            }],
        }
    }

    fn decode(jpeg: &[u8]) -> (Vec<u8>, (u16, u16)) {
        let mut decoder = jpeg_decoder::Decoder::new(jpeg);
        let rgb = decoder.decode().expect("valid JPEG");
        let info = decoder.info().expect("info");
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        (rgb, (info.width, info.height))
    }

    fn rgb_at(rgb: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * width + x) * 3) as usize;
        [rgb[at], rgb[at + 1], rgb[at + 2]]
    }

    #[test]
    fn xrgb8888_channels_land_in_the_right_place() {
        for stride in [64 * 4, 64 * 4 + 32] {
            let frame = split_frame(64, 32, stride);
            let encoded = JpegEncoder::new(90).encode(&frame).unwrap();
            assert!(encoded.keyframe);
            let (rgb, size) = decode(&encoded.data);
            assert_eq!(size, (64, 32));
            let [r, g, b] = rgb_at(&rgb, 64, 8, 16);
            assert!(r > 200 && g < 60 && b < 60, "left is red: {r} {g} {b}");
            let [r, g, b] = rgb_at(&rgb, 64, 56, 16);
            assert!(r < 60 && g < 60 && b > 200, "right is blue: {r} {g} {b}");
        }
    }

    #[test]
    fn malformed_frames_are_refused() {
        let mut encoder = JpegEncoder::new(75);
        let mut short = split_frame(8, 8, 32);
        short.xrgb8888 = Arc::from(vec![0u8; 31 * 8]);
        assert!(matches!(
            encoder.encode(&short),
            Err(EncodeError::BadFrame(_))
        ));
        let mut narrow = split_frame(8, 8, 32);
        narrow.stride = 28;
        assert!(matches!(
            encoder.encode(&narrow),
            Err(EncodeError::BadFrame(_))
        ));
        let mut empty = split_frame(8, 8, 32);
        empty.size = (0, 8);
        assert!(matches!(
            encoder.encode(&empty),
            Err(EncodeError::BadFrame(_))
        ));
    }

    #[test]
    fn quality_is_clamped() {
        assert_eq!(JpegEncoder::new(0).quality(), 1);
        assert_eq!(JpegEncoder::new(255).quality(), 100);
    }
}
