//! The completeness rule for an `index_constituents` body (#2575, "Run
//! phases" step 2). One pure function, used by the Store (to refuse an
//! incomplete `ok` and to read members at the freeze) and by the source
//! classifier (to file such a body as `permanent`, `incomplete_membership`).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::types::Key;

/// Why a constituents body is not a membership observation. `code()` is the
/// `observation.error_code` of a row filed as `incomplete_membership`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IncompleteMembership {
    #[error("body is not a JSON object")]
    NotJson,
    #[error("`total` is missing or not a non-negative integer")]
    NoTotal,
    #[error("`stocks` is missing or not an array of objects with a string `symbol`")]
    NoStocks,
    #[error("a `stocks[].symbol` is not a valid key: {0:?}")]
    BadSymbol(String),
    #[error("`total` is 0")]
    ZeroTotal,
    #[error("total {total}, {stocks} stocks, {distinct} distinct symbols")]
    CountMismatch {
        total: u64,
        stocks: usize,
        distinct: usize,
    },
}

impl IncompleteMembership {
    pub fn code(&self) -> &'static str {
        match self {
            IncompleteMembership::NotJson => "not_json",
            IncompleteMembership::NoTotal => "no_total",
            IncompleteMembership::NoStocks => "no_stocks",
            IncompleteMembership::BadSymbol(_) => "bad_symbol",
            IncompleteMembership::ZeroTotal => "zero_total",
            IncompleteMembership::CountMismatch { .. } => "count_mismatch",
        }
    }
}

/// The members of an `index_constituents` body, sorted, when the body is a
/// complete membership: `total > 0` (a number or a numeric string), and the
/// number of distinct `stocks[].symbol` equals both `total` and the length
/// of `stocks`. A mistyped index (`total = 0`) or a truncated page (`total`
/// above the `limit=1000` the catalog asks for, or above what came back) is
/// never a membership.
pub fn constituents_members(body: &[u8]) -> Result<Vec<Key>, IncompleteMembership> {
    let value: Value = serde_json::from_slice(body).map_err(|_| IncompleteMembership::NotJson)?;
    let object = value.as_object().ok_or(IncompleteMembership::NotJson)?;
    let total = match object.get("total") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.parse::<u64>().ok(),
        _ => None,
    }
    .ok_or(IncompleteMembership::NoTotal)?;
    let stocks = object
        .get("stocks")
        .and_then(Value::as_array)
        .ok_or(IncompleteMembership::NoStocks)?;
    let mut members = BTreeSet::new();
    for stock in stocks {
        let symbol = stock
            .get("symbol")
            .and_then(Value::as_str)
            .ok_or(IncompleteMembership::NoStocks)?;
        let key =
            Key::new(symbol).map_err(|_| IncompleteMembership::BadSymbol(symbol.to_owned()))?;
        members.insert(key);
    }
    if total == 0 {
        return Err(IncompleteMembership::ZeroTotal);
    }
    let complete =
        u64::try_from(members.len()).is_ok_and(|n| n == total) && members.len() == stocks.len();
    if !complete {
        return Err(IncompleteMembership::CountMismatch {
            total,
            stocks: stocks.len(),
            distinct: members.len(),
        });
    }
    Ok(members.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(body: &str) -> Result<Vec<String>, &'static str> {
        constituents_members(body.as_bytes())
            .map(|m| m.iter().map(|k| k.to_string()).collect())
            .map_err(|e| e.code())
    }

    #[test]
    fn complete_bodies_give_sorted_members() {
        let body =
            r#"{"total":2,"fall_num":1,"stocks":[{"symbol":"MSFT.US"},{"symbol":"AAPL.US"}]}"#;
        assert_eq!(members(body), Ok(vec!["AAPL.US".into(), "MSFT.US".into()]));
        let body = r#"{"total":"1","stocks":[{"symbol":"AAPL.US","name":"Apple"}]}"#;
        assert_eq!(members(body), Ok(vec!["AAPL.US".into()]));
    }

    #[test]
    fn incomplete_bodies_are_refused_with_their_code() {
        for (body, code) in [
            ("", "not_json"),
            ("[]", "not_json"),
            (r#"{"stocks":[]}"#, "no_total"),
            (r#"{"total":-1,"stocks":[]}"#, "no_total"),
            (r#"{"total":"x","stocks":[]}"#, "no_total"),
            (r#"{"total":1}"#, "no_stocks"),
            (r#"{"total":1,"stocks":[{"name":"x"}]}"#, "no_stocks"),
            (r#"{"total":1,"stocks":[{"symbol":"A B"}]}"#, "bad_symbol"),
            // A mistyped index symbol.
            (r#"{"total":0,"stocks":[]}"#, "zero_total"),
            // The unpaged default: 20 of 501.
            (
                r#"{"total":3,"stocks":[{"symbol":"A.US"},{"symbol":"B.US"}]}"#,
                "count_mismatch",
            ),
            // Inconsistent pages: a duplicate fills the count.
            (
                r#"{"total":2,"stocks":[{"symbol":"A.US"},{"symbol":"A.US"}]}"#,
                "count_mismatch",
            ),
            (
                r#"{"total":2,"stocks":[{"symbol":"A.US"},{"symbol":"A.US"},{"symbol":"B.US"}]}"#,
                "count_mismatch",
            ),
        ] {
            assert_eq!(members(body), Err(code), "{body}");
        }
    }
}
