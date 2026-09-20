//! A model-side terminal client projection. It owns no PTY or application lifecycle.
#![forbid(unsafe_code)]

use anyhow::{Result, ensure};
use rmux_core::{COLOUR_DEFAULT, ScreenLineView, TerminalScreen, input::mode};
use rmux_proto::TerminalSize;
use serde::Serialize;

mod raster;
pub use raster::Rasterizer;

pub const MAX_COLS: u16 = 512;
pub const MAX_ROWS: u16 = 256;
const MAX_FRAME_TEXT: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Cell {
    pub text: String,
    pub width: u8,
    pub attributes: u16,
    pub foreground: i32,
    pub background: i32,
}

impl Cell {
    /// What rmux renders for a cell nothing was written to.
    fn blank() -> Self {
        Self {
            text: " ".into(),
            width: 1,
            attributes: 0,
            foreground: COLOUR_DEFAULT,
            background: COLOUR_DEFAULT,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Cursor {
    pub column: u32,
    pub row: u32,
    pub visible: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    pub alternate: bool,
    pub scroll_offset: usize,
    pub history_rows: usize,
    pub modes: u32,
    pub text: Vec<String>,
    pub cells: Vec<Cell>,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
}

/// Only the facts needed to encode an action; cached observations need not retain rendered cells or text.
#[derive(Clone, Copy, Debug)]
pub struct InputSurface {
    pub cols: u16,
    pub rows: u16,
    pub modes: u32,
    /// The alternate screen is tracked by the saved grid, not by a mode bit,
    /// so `modes` alone cannot tell a menu from the shell underneath it.
    pub alternate: bool,
    pub scroll_offset: usize,
}
/// Which matching row `TerminalView::find_text` returns; serialised as `latest` / `earliest` (the wire vocabulary).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Occurrence {
    /// The bottom-most matching row (the most recent output).
    Latest,
    /// The top-most matching row.
    Earliest,
}

/// The plain text of one row exactly as `Frame::text` carries it: padding cells skipped, unstored cells blank, trailing blanks trimmed.
fn row_text(line: &ScreenLineView, cols: u16) -> String {
    let mut plain = String::new();
    for column in 0..u32::from(cols) {
        match line.cell(column) {
            Some(cell) if cell.is_padding() => {}
            Some(cell) => plain.push_str(cell.text()),
            None => plain.push(' '),
        }
    }
    plain.trim_end().to_owned()
}

impl Frame {
    pub fn input_surface(&self) -> InputSurface {
        InputSurface {
            cols: self.cols,
            rows: self.rows,
            modes: self.modes,
            alternate: self.alternate,
            scroll_offset: self.scroll_offset,
        }
    }
}

pub struct TerminalView {
    terminal: TerminalScreen,
    foreground: [u8; 3],
    background: [u8; 3],
}

impl TerminalView {
    pub fn new(cols: u16, rows: u16, foreground: [u8; 3], background: [u8; 3]) -> Result<Self> {
        ensure!(
            (1..=MAX_COLS).contains(&cols) && (1..=MAX_ROWS).contains(&rows),
            "unsupported terminal geometry"
        );
        let mut terminal = TerminalScreen::new(TerminalSize { cols, rows }, 2000);
        terminal.set_input_buffer_limit(1024 * 1024);
        Ok(Self {
            terminal,
            foreground,
            background,
        })
    }

    pub fn colors(&mut self, foreground: Option<[u8; 3]>, background: Option<[u8; 3]>) {
        if let Some(color) = foreground {
            self.foreground = color;
        }
        if let Some(color) = background {
            self.background = color;
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        ensure!(
            (1..=MAX_COLS).contains(&cols) && (1..=MAX_ROWS).contains(&rows),
            "unsupported terminal geometry"
        );
        self.terminal.resize(TerminalSize { cols, rows });
        Ok(())
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.feed(bytes);
        // The existing server render plane is the terminal-query responder.
        // A second observing client must never inject duplicate replies.
        self.terminal.take_replies();
        self.terminal.take_terminal_passthrough();
    }

    /// The scrollback depth in rows: the absolute index of the live viewport's first row.
    pub fn history_rows(&self) -> usize {
        self.terminal.screen().history_size()
    }

    /// The absolute row index (`0..history + rows`) of the latest or earliest row whose plain text contains `pattern`
    /// (case-sensitive substring, no wrapping reassembly). In the alternate screen only the live rows are searched, mirroring `frame`.
    pub fn find_text(&self, pattern: &str, occurrence: Occurrence) -> Option<usize> {
        let screen = self.terminal.screen();
        let size = screen.size();
        let history = screen.history_size();
        let first = if screen.is_alternate() { history } else { 0 };
        let matches = |row: usize| {
            screen
                .absolute_line_view(row)
                .is_some_and(|line| row_text(&line, size.cols).contains(pattern))
        };
        let rows = first..history + usize::from(size.rows);
        match occurrence {
            Occurrence::Latest => rows.rev().find(|row| matches(*row)),
            Occurrence::Earliest => rows.into_iter().find(|row| matches(*row)),
        }
    }

    pub fn frame(&self, requested_offset: usize) -> Result<Frame> {
        let screen = self.terminal.screen();
        let size = screen.size();
        let history = screen.history_size();
        let offset = if screen.is_alternate() {
            0
        } else {
            requested_offset.min(history)
        };
        let mut cells = Vec::with_capacity(usize::from(size.cols) * usize::from(size.rows));
        let mut text = Vec::with_capacity(usize::from(size.rows));
        let mut bytes = 0usize;
        for row in 0..usize::from(size.rows) {
            let line = screen
                .absolute_line_view(history - offset + row)
                .ok_or_else(|| anyhow::anyhow!("missing terminal row"))?;
            text.push(row_text(&line, size.cols));
            for column in 0..usize::from(size.cols) {
                // rmux stores an attributed line (any SGR) to its written extent and pads only plain lines, so a history
                // row can be narrower than the viewport; its missing cells are blank cells, not a failed frame.
                let cell = match line.cell(column as u32) {
                    Some(cell) => Cell {
                        text: cell.text().into(),
                        width: cell.width(),
                        attributes: cell.attr(),
                        foreground: cell.fg(),
                        background: cell.bg(),
                    },
                    None => Cell::blank(),
                };
                bytes = bytes.saturating_add(cell.text.len());
                ensure!(
                    bytes <= MAX_FRAME_TEXT,
                    "terminal frame text exceeds capture limit"
                );
                cells.push(cell);
            }
        }
        let (column, row) = screen.cursor_position();
        Ok(Frame {
            cols: size.cols,
            rows: size.rows,
            cursor: Cursor {
                column,
                row,
                visible: offset == 0 && screen.mode() & mode::MODE_CURSOR != 0,
            },
            alternate: screen.is_alternate(),
            scroll_offset: offset,
            history_rows: history,
            modes: screen.mode(),
            text,
            cells,
            foreground: self.foreground,
            background: self.background,
        })
    }
}

/// Encode a small, explicit terminal key vocabulary. Arrow keys respect DECCKM.
pub fn key_bytes(key: &str, modes: u32) -> Result<Vec<u8>> {
    let arrow = if modes & mode::MODE_KCURSOR != 0 {
        "\x1bO"
    } else {
        "\x1b["
    };
    let bytes = match key {
        "Enter" => b"\r".to_vec(),
        "Escape" => vec![27],
        "Tab" => vec![9],
        "Backspace" => vec![127],
        "Ctrl+C" => vec![3],
        "Ctrl+D" => vec![4],
        "Ctrl+J" => vec![10],
        "Ctrl+U" => vec![21],
        "Ctrl+L" => vec![12],
        "Up" => format!("{arrow}A").into_bytes(),
        "Down" => format!("{arrow}B").into_bytes(),
        "Right" => format!("{arrow}C").into_bytes(),
        "Left" => format!("{arrow}D").into_bytes(),
        "Home" => format!("{arrow}H").into_bytes(),
        "End" => format!("{arrow}F").into_bytes(),
        "PageUp" => b"\x1b[5~".to_vec(),
        "PageDown" => b"\x1b[6~".to_vec(),
        "Delete" => b"\x1b[3~".to_vec(),
        _ => anyhow::bail!("unsupported terminal key"),
    };
    Ok(bytes)
}

pub fn click_bytes(column: u16, row: u16, frame: &InputSurface) -> Result<Vec<u8>> {
    ensure!(
        frame.scroll_offset == 0 && column < frame.cols && row < frame.rows,
        "click outside live viewport"
    );
    let mouse = mode::MODE_MOUSE_STANDARD | mode::MODE_MOUSE_BUTTON | mode::MODE_MOUSE_ALL;
    ensure!(
        frame.modes & mouse != 0,
        "application has not enabled terminal mouse input"
    );
    ensure!(
        frame.modes & mode::MODE_MOUSE_SGR != 0,
        "application does not support SGR mouse input"
    );
    Ok(format!(
        "\x1b[<0;{};{}M\x1b[<0;{};{}m",
        column + 1,
        row + 1,
        column + 1,
        row + 1
    )
    .into_bytes())
}
