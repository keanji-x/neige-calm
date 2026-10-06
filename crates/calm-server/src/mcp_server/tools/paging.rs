//! One page of a kernel list tool that pages (agent-commands §5): rows in cursor order up to a fixed
//! count, ending early at a byte budget, so no row is ever dropped. `next_cursor` is the key of the
//! page's last row while rows remain, and `null` on the last page (the workspace and mail lists'
//! convention: the cursor is the last row's sort key, an opaque string).

use serde_json::Value;

use crate::mcp_server::framing::RpcError;

/// The most row bytes one page carries.
pub(crate) const PAGE_BYTES: usize = 32 * 1024;

pub(crate) struct Page {
    rows: Vec<Value>,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
    last_key: Option<String>,
    more: bool,
}

impl Page {
    pub(crate) fn new(max_rows: usize) -> Self {
        Self::with_budget(max_rows, PAGE_BYTES)
    }

    pub(crate) fn with_budget(max_rows: usize, max_bytes: usize) -> Self {
        Self {
            rows: Vec::new(),
            bytes: 0,
            max_rows,
            max_bytes,
            last_key: None,
            more: false,
        }
    }

    /// Offers the next row in cursor order, keyed by the cursor that resumes after it. `false` when
    /// the page is full: that row and every later one belong to the next page. A page always takes
    /// its first row, so a row over the budget alone still pages.
    pub(crate) fn push(&mut self, key: String, row: Value) -> bool {
        let bytes = serde_json::to_vec(&row).map_or(usize::MAX, |encoded| encoded.len() + 1);
        let full =
            self.rows.len() >= self.max_rows || self.bytes.saturating_add(bytes) > self.max_bytes;
        if full && !self.rows.is_empty() {
            self.more = true;
            return false;
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.rows.push(row);
        self.last_key = Some(key);
        true
    }

    /// The rows and `next_cursor`; `rest` says the source holds rows past the last one offered.
    pub(crate) fn finish(self, rest: bool) -> (Vec<Value>, Value) {
        let next_cursor = match self.last_key {
            Some(key) if self.more || rest => Value::String(key),
            _ => Value::Null,
        };
        (self.rows, next_cursor)
    }
}

/// The optional `cursor` argument, a string.
pub(crate) fn cursor_arg<'a>(args: &'a Value, tool: &str) -> Result<Option<&'a str>, RpcError> {
    match args.get("cursor") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cursor)) => Ok(Some(cursor.as_str())),
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `cursor` must be a string, the previous result's next_cursor"
        ))),
    }
}

/// The refusal of a cursor that names no row of the listing: one this tool did not mint, or one
/// whose row is gone since (a deleted track, an edited report, a pruned commit).
pub(crate) fn foreign_cursor(tool: &str, cursor: &str) -> RpcError {
    RpcError::invalid_params(format!(
        "{tool}: `cursor` `{cursor}` names no row of this listing (not a next_cursor it returned, \
         or its row is gone); start again without cursor"
    ))
}

#[cfg(test)]
mod tests {
    use super::Page;
    use serde_json::{Value, json};

    fn page_through(rows: &[Value], max_rows: usize, max_bytes: usize) -> Vec<Vec<Value>> {
        let mut pages = Vec::new();
        let mut start = 0;
        loop {
            let mut page = Page::with_budget(max_rows, max_bytes);
            for (index, row) in rows.iter().enumerate().skip(start) {
                if !page.push(index.to_string(), row.clone()) {
                    break;
                }
            }
            let (taken, next) = page.finish(false);
            start += taken.len();
            pages.push(taken);
            match next {
                Value::String(key) => assert_eq!(key, (start - 1).to_string()),
                Value::Null => break,
                other => panic!("next_cursor {other}"),
            }
        }
        pages
    }

    #[test]
    fn a_page_ends_at_its_row_count_or_byte_budget_and_never_drops_a_row() {
        let rows: Vec<Value> = (0..7)
            .map(|i| json!({ "n": i, "pad": "x".repeat(20) }))
            .collect();
        let row_bytes = serde_json::to_vec(&rows[0]).unwrap().len() + 1;
        let by_count = page_through(&rows, 3, usize::MAX);
        assert_eq!(by_count.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 1]);
        let by_bytes = page_through(&rows, 50, 2 * row_bytes + 1);
        assert_eq!(
            by_bytes.iter().map(Vec::len).collect::<Vec<_>>(),
            [2, 2, 2, 1]
        );
        assert_eq!(by_bytes.concat(), rows, "every row once, in order");
        let oversized = page_through(&rows, 50, 1);
        assert_eq!(
            oversized.concat(),
            rows,
            "a row over the budget alone still pages"
        );
    }
}
