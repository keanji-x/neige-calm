//! Row-hash and cursor comparison behind `allow_output_below_cursor` (#1666):
//! an opt-in text-and-presentation drift heuristic, not target equality. Pure
//! functions over captured frames; no lock, no client.
use calm_terminal_view::{Cursor, Frame};
use serde_json::{Value, json};
use std::hash::{DefaultHasher, Hash, Hasher};

/// The cursor facts an observation keeps for the comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorSnapshot {
    pub row: u32,
    pub column: u32,
    pub visible: bool,
}
impl From<&Cursor> for CursorSnapshot {
    fn from(cursor: &Cursor) -> Self {
        Self {
            row: cursor.row,
            column: cursor.column,
            visible: cursor.visible,
        }
    }
}

/// One 64-bit hash per rendered row over the row's cells: glyphs AND
/// presentation (width, attributes, colours), so a highlight change counts
/// as a change. Rows are `frame.rows` slices of `frame.cells`.
pub fn row_hashes(frame: &Frame) -> Vec<u64> {
    let cols = usize::from(frame.cols).max(1);
    frame
        .cells
        .chunks(cols)
        .map(|row| {
            let mut hasher = DefaultHasher::new();
            for cell in row {
                cell.text.hash(&mut hasher);
                cell.width.hash(&mut hasher);
                cell.attributes.hash(&mut hasher);
                cell.foreground.hash(&mut hasher);
                cell.background.hash(&mut hasher);
            }
            hasher.finish()
        })
        .collect()
}

/// Rows indices are reported in the stale result and the drift receipt; the
/// receipt lists at most this many.
pub const ROWS_LISTED_MAX: usize = 16;

/// What differs between the observed screen and the live one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenDiff {
    pub cursor_moved: bool,
    pub cursor_visible: bool,
    /// The live cursor row is within `0..rows` of the live frame.
    pub cursor_in_range: bool,
    pub row_count_changed: bool,
    pub rows_changed_at_or_above_cursor: usize,
    pub rows_changed_below_cursor: Vec<usize>,
    pub rows_changed_total: usize,
}
impl ScreenDiff {
    /// Compare the observation (`saved`) with the live frame (`live`). Rows
    /// are paired by index; when the counts differ the diff says so and
    /// every unpaired row counts as changed on the side that has it. Row
    /// classes are relative to the LIVE cursor row.
    pub fn compare(
        saved: CursorSnapshot,
        saved_rows: &[u64],
        live: CursorSnapshot,
        live_rows: &[u64],
    ) -> Self {
        let rows = saved_rows.len().max(live_rows.len());
        let cursor_row = live.row as usize;
        let mut at_or_above = 0;
        let mut below = Vec::new();
        for index in 0..rows {
            if saved_rows.get(index) == live_rows.get(index) {
                continue;
            }
            if index <= cursor_row {
                at_or_above += 1;
            } else {
                below.push(index);
            }
        }
        Self {
            cursor_moved: saved != live,
            cursor_visible: live.visible,
            cursor_in_range: cursor_row < live_rows.len(),
            row_count_changed: saved_rows.len() != live_rows.len(),
            rows_changed_at_or_above_cursor: at_or_above,
            rows_changed_total: at_or_above + below.len(),
            rows_changed_below_cursor: below,
        }
    }
    /// The tolerance admits the write only when the cursor is visible,
    /// within the viewport and identical, the row count is unchanged and
    /// every row at or above the cursor hashes identically: only rows
    /// strictly below the cursor differ (possibly none: a revision can move
    /// without a textual or presentational change).
    pub fn only_below_cursor(&self) -> bool {
        !self.cursor_moved
            && self.cursor_visible
            && self.cursor_in_range
            && !self.row_count_changed
            && self.rows_changed_at_or_above_cursor == 0
    }
    /// The `screen_diff` block of a stale result: counts only, so the Planner
    /// sees at once whether `allow_output_below_cursor` would admit a resend.
    pub fn to_json(&self, observed_revision: u64, current_revision: u64) -> Value {
        json!({"compared":{"observed_revision":observed_revision,"current_revision":current_revision},
            "cursor":{"moved":self.cursor_moved,"visible":self.cursor_visible},
            "rows_changed_total":self.rows_changed_total,
            "rows_changed_at_or_above_cursor":self.rows_changed_at_or_above_cursor,
            "rows_changed_below_cursor":self.rows_changed_below_cursor.len()})
    }
    /// The fields the tolerance adds to `observation_drift` when it admitted
    /// the write: the first [`ROWS_LISTED_MAX`] changed row indices.
    pub fn tolerance_json(&self) -> Value {
        let listed: Vec<usize> = self
            .rows_changed_below_cursor
            .iter()
            .copied()
            .take(ROWS_LISTED_MAX)
            .collect();
        json!({"tolerance":"below_cursor","rows_changed_below_cursor":listed,
            "rows_changed_total":self.rows_changed_total,
            "truncated":self.rows_changed_below_cursor.len() > ROWS_LISTED_MAX})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_terminal_view::TerminalView;

    fn frame(bytes: &[u8]) -> Frame {
        let mut view = TerminalView::new(20, 6, [220; 3], [20; 3]).unwrap();
        view.feed(bytes);
        view.frame(0).unwrap()
    }
    fn cursor(row: u32, column: u32, visible: bool) -> CursorSnapshot {
        CursorSnapshot {
            row,
            column,
            visible,
        }
    }

    /// Hashes cover glyphs and presentation: the same text with a different
    /// attribute or colour on one cell hashes differently on that row only,
    /// and a cursor move alone changes no row.
    #[test]
    fn row_hashes_cover_glyphs_and_presentation_per_row() {
        let plain = frame(b"draft\r\nhint line");
        let hashes = row_hashes(&plain);
        assert_eq!(hashes.len(), 6);
        assert_eq!(hashes, row_hashes(&frame(b"draft\r\nhint line")));
        let bold = row_hashes(&frame(b"\x1b[1mdraft\x1b[0m\r\nhint line"));
        assert_ne!(bold[0], hashes[0], "bold is a presentation change");
        assert_eq!(bold[1..], hashes[1..]);
        let coloured = row_hashes(&frame(b"draft\r\n\x1b[31mhint line\x1b[0m"));
        assert_eq!(coloured[0], hashes[0]);
        assert_ne!(coloured[1], hashes[1], "colour is a presentation change");
        let moved = row_hashes(&frame(b"draft\r\nhint line\x1b[1;1H"));
        assert_eq!(moved, hashes, "a cursor move changes no row");
        let text = row_hashes(&frame(b"draft\r\nhint lime"));
        assert_ne!(text[1], hashes[1]);
        assert_eq!(text[0], hashes[0]);
        assert_eq!(
            CursorSnapshot::from(&plain.cursor),
            cursor(1, 9, true),
            "{:?}",
            plain.cursor
        );
    }

    /// The admission table: only rows strictly below an unmoved, visible,
    /// in-range cursor may differ.
    #[test]
    fn only_below_cursor_admits_exactly_the_hint_line_case() {
        let saved = [1, 2, 3, 4, 5];
        let at = cursor(1, 4, true);
        let diff = |live: CursorSnapshot, rows: &[u64]| ScreenDiff::compare(at, &saved, live, rows);
        // The hint line (row 3) refreshed below the cursor on row 1.
        let hint = diff(at, &[1, 2, 3, 9, 5]);
        assert!(hint.only_below_cursor(), "{hint:?}");
        assert_eq!(hint.rows_changed_below_cursor, vec![3]);
        assert_eq!(hint.rows_changed_total, 1);
        assert_eq!(hint.rows_changed_at_or_above_cursor, 0);
        // Nothing changed at all (the revision moved without a diff).
        let same = diff(at, &saved);
        assert!(same.only_below_cursor());
        assert_eq!(same.rows_changed_total, 0);
        // The cursor row itself changed: at or above.
        let cursor_row = diff(at, &[1, 9, 3, 4, 5]);
        assert!(!cursor_row.only_below_cursor());
        assert_eq!(cursor_row.rows_changed_at_or_above_cursor, 1);
        assert!(cursor_row.rows_changed_below_cursor.is_empty());
        // A row above the cursor changed.
        let above = diff(at, &[9, 2, 3, 4, 5]);
        assert!(!above.only_below_cursor());
        assert_eq!(above.rows_changed_at_or_above_cursor, 1);
        // Both sides: counts land in their classes.
        let both = diff(at, &[9, 2, 3, 9, 9]);
        assert!(!both.only_below_cursor());
        assert_eq!(both.rows_changed_at_or_above_cursor, 1);
        assert_eq!(both.rows_changed_below_cursor, vec![3, 4]);
        assert_eq!(both.rows_changed_total, 3);
        // Cursor moved (column), hidden, or out of range: refused even with
        // identical rows.
        let moved = diff(cursor(1, 5, true), &saved);
        assert!(moved.cursor_moved && !moved.only_below_cursor());
        let down = diff(cursor(2, 4, true), &saved);
        assert!(down.cursor_moved && !down.only_below_cursor());
        let hidden = ScreenDiff::compare(cursor(1, 4, false), &saved, cursor(1, 4, false), &saved);
        assert!(!hidden.cursor_visible && !hidden.only_below_cursor());
        let out = ScreenDiff::compare(cursor(7, 0, true), &saved, cursor(7, 0, true), &saved);
        assert!(!out.cursor_in_range && !out.only_below_cursor());
        // Row count changed: refused, and unpaired rows count as changed.
        let shrunk = diff(at, &[1, 2, 3]);
        assert!(shrunk.row_count_changed && !shrunk.only_below_cursor());
        assert_eq!(shrunk.rows_changed_below_cursor, vec![3, 4]);
        let grown = diff(at, &[1, 2, 3, 4, 5, 6]);
        assert!(grown.row_count_changed && !grown.only_below_cursor());
        assert_eq!(grown.rows_changed_below_cursor, vec![5]);
    }

    #[test]
    fn json_shapes_report_counts_and_a_bounded_index_list() {
        let saved: Vec<u64> = (0..40).collect();
        let mut live = saved.clone();
        for row in live.iter_mut().skip(3) {
            *row += 100;
        }
        let at = cursor(2, 0, true);
        let diff = ScreenDiff::compare(at, &saved, at, &live);
        assert!(diff.only_below_cursor());
        assert_eq!(
            diff.to_json(7, 9),
            json!({"compared":{"observed_revision":7,"current_revision":9},
                "cursor":{"moved":false,"visible":true},
                "rows_changed_total":37,"rows_changed_at_or_above_cursor":0,"rows_changed_below_cursor":37})
        );
        let tolerance = diff.tolerance_json();
        assert_eq!(tolerance["tolerance"], "below_cursor");
        assert_eq!(
            tolerance["rows_changed_below_cursor"],
            json!((3..19).collect::<Vec<usize>>())
        );
        assert_eq!(tolerance["rows_changed_total"], 37);
        assert_eq!(tolerance["truncated"], true);
        let few = ScreenDiff::compare(
            at,
            &saved,
            at,
            &[&saved[..5], &[99, 98][..], &saved[7..]].concat(),
        );
        assert_eq!(
            few.tolerance_json(),
            json!({"tolerance":"below_cursor","rows_changed_below_cursor":[5,6],"rows_changed_total":2,"truncated":false})
        );
        let stale = ScreenDiff::compare(at, &saved, cursor(3, 1, false), &live);
        assert_eq!(
            stale.to_json(1, 2)["cursor"],
            json!({"moved":true,"visible":false})
        );
        assert_eq!(stale.to_json(1, 2)["rows_changed_at_or_above_cursor"], 1);
        assert_eq!(stale.to_json(1, 2)["rows_changed_below_cursor"], 36);
    }
}
