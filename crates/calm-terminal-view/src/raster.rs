use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, ensure};
use resvg::{tiny_skia, usvg};
use rmux_core::GridAttr;

use crate::Frame;

pub struct Rasterizer {
    fonts: Arc<usvg::fontdb::Database>,
}

impl Rasterizer {
    /// Fixed OS font roots; never load terminal-supplied files or user font config.
    pub fn system() -> Result<Self> {
        let mut fonts = usvg::fontdb::Database::new();
        for root in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            if Path::new(root).is_dir() {
                fonts.load_fonts_dir(root);
            }
        }
        ensure!(
            !fonts.is_empty(),
            "terminal images require installed system fonts"
        );
        fonts.set_monospace_family("DejaVu Sans Mono");
        Ok(Self {
            fonts: Arc::new(fonts),
        })
    }

    pub fn png(&self, frame: &Frame) -> Result<Vec<u8>> {
        let native_width = u32::from(frame.cols) * 10;
        let native_height = u32::from(frame.rows) * 20;
        let scale = (2048.0 / native_width as f32)
            .min(1536.0 / native_height as f32)
            .min(1.0);
        let width = (native_width as f32 * scale).ceil() as u32;
        let height = (native_height as f32 * scale).ceil() as u32;
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {native_width} {native_height}\"><rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
            rgb(frame.background)
        );
        for (index, cell) in frame.cells.iter().enumerate() {
            let x = (index % usize::from(frame.cols)) * 10;
            let y = (index / usize::from(frame.cols)) * 20;
            let mut fg = colour(cell.foreground, frame.foreground);
            let mut bg = colour(cell.background, frame.background);
            if cell.attributes & GridAttr::REVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            if bg != frame.background {
                write!(
                    svg,
                    "<rect x=\"{x}\" y=\"{y}\" width=\"10\" height=\"20\" fill=\"{}\"/>",
                    rgb(bg)
                )?;
            }
            if cell.width == 0
                || cell.text.trim().is_empty()
                || cell.attributes & GridAttr::HIDDEN != 0
            {
                continue;
            }
            let weight = if cell.attributes & GridAttr::BRIGHT != 0 {
                "bold"
            } else {
                "normal"
            };
            let style = if cell.attributes & GridAttr::ITALICS != 0 {
                "italic"
            } else {
                "normal"
            };
            write!(
                svg,
                "<text x=\"{x}\" y=\"{}\" font-family=\"monospace\" font-size=\"16\" font-weight=\"{weight}\" font-style=\"{style}\" textLength=\"{}\" lengthAdjust=\"spacingAndGlyphs\" fill=\"{}\">{}</text>",
                y + 16,
                u32::from(cell.width) * 10,
                rgb(fg),
                escape(&cell.text)
            )?;
            if cell.attributes & GridAttr::UNDERSCORE != 0 {
                write!(
                    svg,
                    "<path d=\"M{x} {}h{}\" stroke=\"{}\"/>",
                    y + 18,
                    u32::from(cell.width) * 10,
                    rgb(fg)
                )?;
            }
            ensure!(
                svg.len() < 8 * 1024 * 1024,
                "terminal SVG exceeds rendering budget"
            );
        }
        if frame.cursor.visible
            && frame.cursor.column < u32::from(frame.cols)
            && frame.cursor.row < u32::from(frame.rows)
        {
            write!(
                svg,
                "<rect x=\"{}\" y=\"{}\" width=\"9\" height=\"19\" fill=\"none\" stroke=\"{}\"/>",
                frame.cursor.column * 10,
                frame.cursor.row * 20,
                rgb(frame.foreground)
            )?;
        }
        svg.push_str("</svg>");
        let options = usvg::Options {
            fontdb: self.fonts.clone(),
            ..Default::default()
        };
        let tree = usvg::Tree::from_str(&svg, &options)?;
        let mut pixels = tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| anyhow::anyhow!("invalid image dimensions"))?;
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixels.as_mut(),
        );
        Ok(pixels.encode_png()?)
    }
}

fn escape(text: &str) -> String {
    let mut result = String::new();
    for character in text.chars() {
        match character {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&apos;"),
            c if c.is_control() => {}
            c => result.push(c),
        }
    }
    result
}
fn rgb([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}
fn colour(value: i32, default: [u8; 3]) -> [u8; 3] {
    if value & 0x0200_0000 != 0 && value >= 0 {
        return [(value >> 16) as u8, (value >> 8) as u8, value as u8];
    }
    let index = if value & 0x0100_0000 != 0 && value >= 0 {
        value & 255
    } else if (0..8).contains(&value) {
        value
    } else if (90..98).contains(&value) {
        value - 90 + 8
    } else {
        return default;
    };
    const ANSI: [[u8; 3]; 16] = [
        [46, 52, 54],
        [204, 0, 0],
        [78, 154, 6],
        [196, 160, 0],
        [52, 101, 164],
        [117, 80, 123],
        [6, 152, 154],
        [211, 215, 207],
        [85, 87, 83],
        [239, 41, 41],
        [138, 226, 52],
        [252, 233, 79],
        [114, 159, 207],
        [173, 127, 168],
        [52, 226, 226],
        [238, 238, 236],
    ];
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let i = index - 16;
            let component = |n: i32| if n == 0 { 0 } else { (55 + n * 40) as u8 };
            [component(i / 36), component(i / 6 % 6), component(i % 6)]
        }
        _ => [((index - 232) * 10 + 8) as u8; 3],
    }
}
