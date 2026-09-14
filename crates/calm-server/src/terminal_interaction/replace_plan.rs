//! `replace` (#1677): the server derives the cursor moves, the Backspaces
//! and the text of a draft edit from the cursor row of the live frame, so the
//! Planner no longer counts characters (CJK width included) by hand. Pure
//! functions over a captured `Frame`; no lock, no client.
//!
//! The plan assumes the cursor row is the application's line buffer with one
//! character per non-padding cell, and that the application moves one
//! character per arrow key and erases one per Backspace (true for Claude
//! Code, readline and most line editors). The cursor may be hidden: Claude
//! Code keeps DECTCEM off in its draft box while still positioning the
//! cursor at the edit point, so the plan uses the position whether or not
//! the cursor is shown and reports `cursor_visible` for the Planner to
//! audit. The tool guarantees that the bytes correspond to the plan against
//! the row as captured; nothing more.
use super::actions::ACTION_BYTES_MAX;
use anyhow::{Result, ensure};
use calm_terminal_view::{Frame, InputSurface, key_bytes};
use serde_json::{Value, json};
use std::cmp::Ordering;

/// `from` is at most this many bytes.
pub const REPLACE_FROM_BYTES_MAX: usize = 200;

/// The cursor movement of a plan: `Left` or `Right`, repeated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moves {
    pub key: &'static str,
    pub repeat: usize,
}

/// One derived edit against the cursor row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacePlan {
    /// The cursor row the plan was derived from.
    pub row: u32,
    /// The cursor's character index on that row.
    pub cursor_index: usize,
    /// Whether the cursor was shown (DECTCEM) when the plan was derived; a
    /// hidden cursor is positioned all the same.
    pub cursor_visible: bool,
    /// Moves that bring the cursor to the end of `from`; `None` when it is
    /// there already.
    pub moves: Option<Moves>,
    /// Characters erased: `from.chars().count()`.
    pub erased: usize,
    /// The text inserted in their place (may be empty).
    pub inserted: String,
}
/// The cursor row as the plan reads it.
struct CursorRow {
    /// The row's characters: padding cells skipped, blank cells kept.
    chars: Vec<char>,
    /// `(first character index, character count)` of every non-padding
    /// cell, in column order.
    cells: Vec<(usize, usize)>,
    /// The cursor's character index when it sits on a cell boundary (`None`
    /// inside a wide cell or off the row).
    cursor_index: Option<usize>,
    /// One past the last non-blank cell's characters (the draft's end as
    /// the screen shows it).
    draft_end: usize,
    /// The cursor is in the last column and that cell is not blank: after a
    /// write into the last column the terminal parks the cursor there with
    /// a pending wrap, one cell left of where the application has it.
    pending_wrap: bool,
}
impl ReplacePlan {
    fn cursor_row(frame: &Frame) -> Result<CursorRow> {
        let cols = usize::from(frame.cols);
        let row = frame.cursor.row as usize;
        ensure!(
            row < usize::from(frame.rows) && cols > 0,
            "replace: the cursor row is outside the viewport"
        );
        let cells = frame
            .cells
            .get(row * cols..(row + 1) * cols)
            .ok_or_else(|| anyhow::anyhow!("replace: the frame has no cells for the cursor row"))?;
        let column = frame.cursor.column as usize;
        let mut chars = Vec::new();
        let mut spans = Vec::new();
        let mut cursor_index = None;
        let mut draft_end = 0;
        for (index, cell) in cells.iter().enumerate() {
            // A continuation cell of a wide glyph: width 0, text " ".
            if cell.width == 0 {
                continue;
            }
            if index == column {
                cursor_index = Some(chars.len());
            }
            let count = cell.text.chars().count();
            spans.push((chars.len(), count));
            chars.extend(cell.text.chars());
            if !cell.text.trim().is_empty() {
                draft_end = chars.len();
            }
        }
        let pending_wrap = column + 1 == cols
            && cells
                .last()
                .is_some_and(|cell| !cell.text.trim().is_empty());
        Ok(CursorRow {
            chars,
            cells: spans,
            cursor_index,
            draft_end,
            pending_wrap,
        })
    }
    /// Derive the plan for `from` → `to` on the cursor row of `frame`. Every
    /// refusal is an error: cursor row outside the viewport, cursor inside a
    /// wide cell or off the row, cursor parked in the last column (pending
    /// wrap), `from` absent or ambiguous (overlapping occurrences count),
    /// `from` reaching into the blank run past the draft, or a cell holding
    /// several scalars (a combining sequence, an emoji) inside the match or
    /// the movement span — the application moves and erases per cell, the
    /// plan counts scalars, and the two agree only when every cell touched
    /// holds one.
    pub fn derive(frame: &Frame, from: &str, to: &str) -> Result<Self> {
        let CursorRow {
            chars,
            cells,
            cursor_index,
            draft_end,
            pending_wrap,
        } = Self::cursor_row(frame)?;
        let cursor_index = cursor_index.ok_or_else(|| {
            anyhow::anyhow!("replace: the cursor is inside a wide cell or off the row")
        })?;
        ensure!(
            !pending_wrap,
            "replace: the cursor sits in the last column (pending wrap); edit with a sequence"
        );
        let needle: Vec<char> = from.chars().collect();
        let starts: Vec<usize> = if chars.len() < needle.len() {
            Vec::new()
        } else {
            (0..=chars.len() - needle.len())
                .filter(|&start| chars[start..start + needle.len()] == needle[..])
                .collect()
        };
        ensure!(
            !starts.is_empty(),
            "replace: {from:?} is not on the cursor row; edit with a sequence"
        );
        ensure!(
            starts.len() == 1,
            "replace: {from:?} occurs {} times on the cursor row; edit with a sequence",
            starts.len()
        );
        let end = starts[0] + needle.len();
        ensure!(
            end <= cursor_index || end <= draft_end,
            "replace: from extends past the draft; drop its trailing spaces"
        );
        // Every cell inside the match or between its end and the cursor
        // must hold exactly one scalar (this also covers a match that cuts
        // through a cell's text).
        let (low, high) = (starts[0].min(cursor_index), end.max(cursor_index));
        ensure!(
            cells
                .iter()
                .all(|&(start, count)| count == 1 || start >= high || start + count <= low),
            "replace: a cell holds a combining sequence or emoji; edit with a sequence"
        );
        let moves = match end.cmp(&cursor_index) {
            Ordering::Less => Some(Moves {
                key: "Left",
                repeat: cursor_index - end,
            }),
            Ordering::Greater => Some(Moves {
                key: "Right",
                repeat: end - cursor_index,
            }),
            Ordering::Equal => None,
        };
        Ok(Self {
            row: frame.cursor.row,
            cursor_index,
            cursor_visible: frame.cursor.visible,
            moves,
            erased: needle.len(),
            inserted: to.to_owned(),
        })
    }
    /// The bytes of the plan in one ordered write: the moves, then one
    /// Backspace per erased character, then the inserted text, with the same
    /// key encoding as `sequence` (arrows respect DECCKM).
    pub fn bytes(&self, surface: &InputSurface) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        if let Some(moves) = self.moves {
            bytes.extend(key_bytes(moves.key, surface.modes)?.repeat(moves.repeat));
        }
        bytes.extend(key_bytes("Backspace", surface.modes)?.repeat(self.erased));
        bytes.extend(self.inserted.as_bytes());
        ensure!(
            bytes.len() <= ACTION_BYTES_MAX,
            "replace exceeds {ACTION_BYTES_MAX} encoded bytes"
        );
        Ok(bytes)
    }
    /// The `replace` block of every write receipt.
    pub fn to_json(&self) -> Value {
        json!({"row":self.row,"cursor_index":self.cursor_index,"cursor_visible":self.cursor_visible,
            "moves":self.moves.map(|moves| json!({"key":moves.key,"repeat":moves.repeat})),
            "erased":self.erased,"inserted":self.inserted})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_terminal_view::TerminalView;

    fn frame(bytes: &[u8]) -> Frame {
        let mut view = TerminalView::new(20, 4, [220; 3], [20; 3]).unwrap();
        view.feed(bytes);
        view.frame(0).unwrap()
    }
    fn plan(bytes: &[u8], from: &str, to: &str) -> Result<ReplacePlan> {
        ReplacePlan::derive(&frame(bytes), from, to)
    }
    fn moves(key: &'static str, repeat: usize) -> Option<Moves> {
        Some(Moves { key, repeat })
    }

    /// Cursor at the end of the draft: Left moves; at Home: Right moves;
    /// right after `from`: no moves. Bytes are moves, Backspaces, text.
    #[test]
    fn plan_moves_left_right_or_not_at_all_and_encodes_one_write() {
        let surface = frame(b"").input_surface();
        let end = plan(b"> 7200 + 11 done", "11", "19").unwrap();
        assert_eq!(
            end,
            ReplacePlan {
                row: 0,
                cursor_index: 16,
                cursor_visible: true,
                moves: moves("Left", 5),
                erased: 2,
                inserted: "19".into()
            }
        );
        assert_eq!(
            end.bytes(&surface).unwrap(),
            b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x7f\x7f19".to_vec()
        );
        let home = plan(b"> 7200 + 11 done\x1b[1G", "11", "19").unwrap();
        assert_eq!(home.cursor_index, 0);
        assert_eq!(home.moves, moves("Right", 11));
        assert_eq!(
            home.bytes(&surface).unwrap(),
            [b"\x1b[C".repeat(11), b"\x7f\x7f19".to_vec()].concat()
        );
        let exact = plan(b"> 7200 + 11", "11", "19").unwrap();
        assert_eq!(exact.cursor_index, 11);
        assert_eq!(exact.moves, None);
        assert_eq!(exact.bytes(&surface).unwrap(), b"\x7f\x7f19".to_vec());
        assert_eq!(
            exact.to_json(),
            json!({"row":0,"cursor_index":11,"cursor_visible":true,"moves":null,"erased":2,"inserted":"19"})
        );
        assert_eq!(end.to_json()["moves"], json!({"key":"Left","repeat":5}));
        // Deleting: an empty `to` writes only the Backspaces.
        let delete = plan(b"> 7200 + 11", " + 11", "").unwrap();
        assert_eq!(delete.erased, 5);
        assert_eq!(delete.inserted, "");
        assert_eq!(delete.bytes(&surface).unwrap(), b"\x7f".repeat(5));
        // DECCKM: arrows are encoded like a sequence step would be.
        let app = frame(b"\x1b[?1h> 7200 + 11 x").input_surface();
        assert_eq!(
            plan(b"> 7200 + 11 x", "11", "19")
                .unwrap()
                .bytes(&app)
                .unwrap(),
            b"\x1bOD\x1bOD\x7f\x7f19".to_vec()
        );
        // The cursor row is the one the cursor is on, not the first row.
        let second = plan(b"first\r\n> 11", "11", "19").unwrap();
        assert_eq!(second.row, 1);
        assert_eq!(second.cursor_index, 4);
        assert_eq!(second.moves, None);
    }

    /// Wide glyphs: one character per non-padding cell, so the moves count
    /// characters, not columns (松果 occupies four columns and two
    /// characters); the cursor may not sit inside a wide cell.
    #[test]
    fn wide_cells_count_one_character_and_refuse_a_cursor_inside_them() {
        let surface = frame(b"").input_surface();
        // "11 松果" = 7 columns, 5 characters; cursor at the end.
        let cjk = plan("11 松果".as_bytes(), "11", "19").unwrap();
        assert_eq!(cjk.cursor_index, 5);
        assert_eq!(cjk.moves, moves("Left", 3), "3 characters, not 5 columns");
        let swap = plan("> 松果 apple".as_bytes(), "松果", "苹果").unwrap();
        assert_eq!(swap.cursor_index, 10);
        assert_eq!(swap.moves, moves("Left", 6));
        assert_eq!(swap.erased, 2);
        assert_eq!(
            swap.bytes(&surface).unwrap(),
            [
                b"\x1b[D".repeat(6),
                b"\x7f\x7f".to_vec(),
                "苹果".as_bytes().to_vec()
            ]
            .concat()
        );
        // Cursor inside the wide glyph 松 (column 3 is its padding cell).
        let inside = plan("> 松果\x1b[4G".as_bytes(), "果", "子");
        assert!(
            inside
                .unwrap_err()
                .to_string()
                .contains("inside a wide cell"),
        );
        // Cursor on the boundary before 果 (column 4): Right 1 to its end.
        let boundary = plan("> 松果\x1b[5G".as_bytes(), "果", "子").unwrap();
        assert_eq!(boundary.cursor_index, 3);
        assert_eq!(boundary.moves, moves("Right", 1));
        // The padding cell itself never enters the row string.
        let f = frame("> 松".as_bytes());
        assert_eq!(f.cells[3].width, 0);
        assert_eq!(f.cells[3].text, " ");
        assert_eq!(ReplacePlan::cursor_row(&f).unwrap().chars.len(), 19);
    }

    /// Blank cells count (a blank is a character of the buffer). A combining
    /// sequence is one cell with several scalars: the application moves and
    /// erases per cell while the plan counts scalars, so such a cell inside
    /// the match or between the match and the cursor is refused (review r1
    /// A: on readline `> 11 café` + Left×6/Backspace×2 gave `191 café`, and
    /// `from "é"` ate the `f`); cells outside that span do not matter.
    #[test]
    fn blank_cells_count_and_multi_scalar_cells_in_the_span_are_refused() {
        let blanks = plan(b"a b", "a b", "ab").unwrap();
        assert_eq!(blanks.cursor_index, 3);
        assert_eq!(blanks.erased, 3);
        // 17 trailing blanks hold 16 overlapping "  ": ambiguous.
        let blank_pairs = plan(b"a b", "  ", "").unwrap_err();
        assert!(
            blank_pairs.to_string().contains("occurs 16 times"),
            "{blank_pairs}"
        );
        // "e" + U+0301 combine into one cell of width 1 and two scalars.
        let f = frame("> 11 caf\u{65}\u{301}".as_bytes());
        assert_eq!(f.cells[8].text, "e\u{301}");
        assert_eq!(f.cells[8].width, 1);
        let row = ReplacePlan::cursor_row(&f).unwrap();
        assert_eq!(row.chars.len(), 21);
        assert_eq!(
            row.cursor_index,
            Some(10),
            "cursor column 9 is character 10"
        );
        assert_eq!(row.cells[8], (8, 2));
        for (from, why) in [
            ("11", "é between the match and the cursor"),
            ("e\u{301}", "é in the match"),
        ] {
            let refused = ReplacePlan::derive(&f, from, "x").unwrap_err();
            assert!(
                refused
                    .to_string()
                    .contains("a cell holds a combining sequence or emoji"),
                "{why}: {refused}"
            );
        }
        let cut = ReplacePlan::derive(&f, "cafe", "tea").unwrap_err();
        assert!(cut.to_string().contains("combining sequence"), "{cut}");
        // The multi-scalar cell before both the match and the cursor does
        // not enter the moves: cells and scalars agree on the span.
        let f = frame("> caf\u{65}\u{301} 11".as_bytes());
        let after = ReplacePlan::derive(&f, "11", "19").unwrap();
        assert_eq!(after.cursor_index, 10);
        assert_eq!(after.moves, None);
        assert_eq!(after.erased, 2);
        let f = frame("caf\u{65}\u{301} au lait".as_bytes());
        let tail = ReplacePlan::derive(&f, "au", "de").unwrap();
        assert_eq!(tail.moves, moves("Left", 5), "five cells, five scalars");
    }

    /// Review r1 B: after a write into the last column the terminal parks
    /// the cursor there with a pending wrap, one cell left of where the
    /// application has it; the plan cannot see the flag and refuses the
    /// shape. A cursor in the last column over a blank cell is not that.
    #[test]
    fn a_cursor_parked_in_the_last_column_over_text_is_refused() {
        let filled = frame(b"> 7200 + 11 done xyz");
        assert_eq!(
            (filled.cursor.column, filled.cursor.row),
            (19, 0),
            "{:?}",
            filled.cursor
        );
        let refused = ReplacePlan::derive(&filled, "11", "19").unwrap_err();
        assert!(refused.to_string().contains("pending wrap"), "{refused}");
        let moved = frame(b"> 7200 + 11\x1b[20G");
        assert_eq!(moved.cursor.column, 19);
        let plan = ReplacePlan::derive(&moved, "11", "19").unwrap();
        assert_eq!(plan.cursor_index, 19);
        assert_eq!(plan.moves, moves("Left", 8));
    }

    /// Review r1 D: a `from` that reaches into the blank run past the draft
    /// would move Right past the buffer's end; refused unless the cursor
    /// itself is past that point (the blanks are then typed text).
    #[test]
    fn from_extending_past_the_draft_is_refused() {
        let past = plan(b"> 7200 + 11", "11 ", "19").unwrap_err();
        assert!(
            past.to_string().contains("extends past the draft"),
            "{past}"
        );
        let past = plan(b"> 7200 + 11\x1b[9G", "11 ", "19").unwrap_err();
        assert!(
            past.to_string().contains("extends past the draft"),
            "{past}"
        );
        // Two typed trailing spaces, cursor after them: the space is real.
        let typed = plan(b"> 7200 + 11  ", "11 ", "19").unwrap();
        assert_eq!(typed.cursor_index, 13);
        assert_eq!(typed.moves, moves("Left", 1));
        assert_eq!(typed.erased, 3);
    }

    /// A hidden cursor (DECTCEM off, as in Claude Code's draft box) is
    /// positioned all the same: the plan uses it and reports the fact.
    #[test]
    fn hidden_cursor_is_used_by_position_and_reported() {
        let hidden = plan(b"> 7200 + 11 done\x1b[?25l", "11", "19").unwrap();
        assert!(!hidden.cursor_visible);
        assert_eq!(hidden.cursor_index, 16);
        assert_eq!(hidden.moves, moves("Left", 5));
        assert_eq!(hidden.to_json()["cursor_visible"], false);
        let shown = plan(b"> 7200 + 11 done", "11", "19").unwrap();
        assert!(shown.cursor_visible);
        assert_eq!(shown.to_json()["cursor_visible"], true);
        assert_eq!(
            ReplacePlan {
                cursor_visible: true,
                ..hidden.clone()
            },
            shown,
            "visibility is the only difference"
        );
    }

    /// Absent, ambiguous (overlapping occurrences included), another row,
    /// off the row, and the size bound.
    #[test]
    fn refusals_absent_ambiguous_other_row_off_row_and_size() {
        let absent = plan(b"> 7200 + 11", "12", "19").unwrap_err();
        assert!(
            absent.to_string().contains("is not on the cursor row"),
            "{absent}"
        );
        let twice = plan(b"> 11 + 11", "11", "19").unwrap_err();
        assert!(twice.to_string().contains("occurs 2 times"), "{twice}");
        let overlapping = plan(b"> aaa", "aa", "b").unwrap_err();
        assert!(
            overlapping.to_string().contains("occurs 2 times"),
            "aaa holds two aa: {overlapping}"
        );
        assert!(plan(b"> aaa", "aaa", "b").is_ok());
        // `from` is on the row above the cursor row.
        let above = plan(b"> 11\r\n> 12", "11", "19").unwrap_err();
        assert!(
            above.to_string().contains("is not on the cursor row"),
            "{above}"
        );
        let mut off = frame(b"> 11");
        off.cursor.row = 4;
        assert!(
            ReplacePlan::derive(&off, "11", "19")
                .unwrap_err()
                .to_string()
                .contains("outside the viewport")
        );
        let mut past = frame(b"> 11");
        past.cursor.column = 20;
        assert!(
            ReplacePlan::derive(&past, "11", "19")
                .unwrap_err()
                .to_string()
                .contains("off the row")
        );
        let surface = frame(b"").input_surface();
        let big = plan(b"> 11", "11", &"x".repeat(ACTION_BYTES_MAX - 1)).unwrap();
        assert!(big.bytes(&surface).is_err(), "two Backspaces push it over");
        let fits = plan(b"> 11", "11", &"x".repeat(ACTION_BYTES_MAX - 2)).unwrap();
        assert_eq!(fits.bytes(&surface).unwrap().len(), ACTION_BYTES_MAX);
    }
}
