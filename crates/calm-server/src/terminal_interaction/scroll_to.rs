//! #1710 — `observe scroll_to_text`: find a history row by its plain text
//! and capture the screen scrolled so that row is the first screen row.
use anyhow::{Result, ensure};
pub use calm_terminal_view::Occurrence;
use serde_json::{Value, json};

/// The longest `scroll_to_text` pattern in bytes: the `wait_text` bound.
pub const SCROLL_TO_TEXT_MAX_BYTES: usize = super::wait_plan::WAIT_TEXT_MAX_BYTES;

/// The history search an observation carries: a plain, case-sensitive
/// substring and which matching row to pick. Constructed only through
/// [`ScrollTo::new`], so a pattern the kernel searches for is always within
/// bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollTo {
    pattern: String,
    occurrence: Occurrence,
}
impl ScrollTo {
    /// 1..=200 bytes and no control characters (rendered rows never contain
    /// any, so such a pattern could never match).
    pub fn new(pattern: String, occurrence: Occurrence) -> Result<Self> {
        ensure!(
            !pattern.is_empty()
                && pattern.len() <= SCROLL_TO_TEXT_MAX_BYTES
                && !pattern.chars().any(char::is_control),
            "scroll_to_text: 1..{SCROLL_TO_TEXT_MAX_BYTES} bytes of printable text"
        );
        Ok(Self {
            pattern,
            occurrence,
        })
    }
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
    pub fn occurrence(&self) -> Occurrence {
        self.occurrence
    }
}

/// The offset to capture at for the found absolute row: a history row
/// becomes the first screen row (`history_rows - row`); a live-screen row
/// (`row >= history_rows`) and no match capture the live viewport (0). Never
/// above `history_rows`, which `frame` clamps to anyway.
pub(super) fn offset_for(history_rows: usize, row: Option<usize>) -> usize {
    row.map_or(0, |row| history_rows.saturating_sub(row).min(history_rows))
}

/// The `scroll_to` block of the observation: the request echoed, the verdict,
/// the match's absolute row and its row inside the returned screen
/// (`row_absolute - (history_rows - scroll_offset)`, the screen's top row
/// being `history_rows - scroll_offset`); both rows null when not found.
pub(super) fn report(
    request: &ScrollTo,
    row: Option<usize>,
    history_rows: usize,
    scroll_offset: usize,
) -> Value {
    let top = history_rows.saturating_sub(scroll_offset);
    json!({
        "pattern": request.pattern,
        "occurrence": request.occurrence,
        "status": if row.is_some() { "found" } else { "not_found" },
        "row_absolute": row,
        "row": row.and_then(|row| row.checked_sub(top)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(occurrence: Occurrence) -> ScrollTo {
        ScrollTo::new("MARK".into(), occurrence).unwrap()
    }

    /// A history row scrolls to itself; the live screen and no match stay
    /// at 0; an empty history never scrolls.
    #[test]
    fn offset_puts_a_history_row_first_and_leaves_the_live_screen_alone() {
        assert_eq!(offset_for(40, Some(30)), 10);
        assert_eq!(offset_for(40, Some(0)), 40);
        assert_eq!(offset_for(40, Some(39)), 1);
        assert_eq!(offset_for(40, Some(40)), 0, "first live row");
        assert_eq!(offset_for(40, Some(45)), 0, "live row");
        assert_eq!(offset_for(40, None), 0);
        assert_eq!(offset_for(0, Some(0)), 0);
        assert_eq!(offset_for(0, Some(3)), 0);
    }

    /// The block names the request, the verdict and both row indexes: 0 for
    /// a history row shown first, the live index for a live row, null for
    /// no match.
    #[test]
    fn report_carries_the_absolute_row_and_the_row_on_the_returned_screen() {
        assert_eq!(
            report(&request(Occurrence::Latest), Some(30), 40, 10),
            json!({"pattern":"MARK","occurrence":"latest","status":"found","row_absolute":30,"row":0})
        );
        assert_eq!(
            report(&request(Occurrence::Earliest), Some(42), 40, 0),
            json!({"pattern":"MARK","occurrence":"earliest","status":"found","row_absolute":42,"row":2})
        );
        assert_eq!(
            report(&request(Occurrence::Latest), None, 40, 0),
            json!({"pattern":"MARK","occurrence":"latest","status":"not_found","row_absolute":null,"row":null})
        );
    }

    /// The pattern bounds: empty, over 200 bytes and control characters are
    /// refused with the `scroll_to_text:` message; 200 bytes pass.
    #[test]
    fn pattern_must_be_printable_and_within_the_wait_text_bound() {
        for bad in [
            "".to_owned(),
            "x".repeat(201),
            "a\tb".to_owned(),
            "a\u{1b}[0m".to_owned(),
        ] {
            let error = ScrollTo::new(bad.clone(), Occurrence::Latest).unwrap_err();
            assert!(
                error.to_string().starts_with("scroll_to_text: "),
                "{bad:?}: {error}"
            );
        }
        let longest = ScrollTo::new("x".repeat(200), Occurrence::Earliest).unwrap();
        assert_eq!(longest.pattern().len(), 200);
        assert_eq!(longest.occurrence(), Occurrence::Earliest);
    }
}
