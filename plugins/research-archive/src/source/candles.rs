//! Versioned typed daily-bar projection. This is not an HTTP/raw-wire body.
use super::{Fetched, LongbridgeSource, failed, invalid_body};
use crate::types::{ErrorClass, FailureClass};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::America::New_York;
use longbridge::Decimal;
use longbridge_proto::quote::{self, Candlestick};
use prost::Message;
use serde_json::{Value, json};

impl LongbridgeSource {
    pub(super) async fn candles(
        &self,
        query: &[(String, String)],
        may_send: &(impl Fn() -> bool + Sync),
    ) -> Option<Fetched> {
        let Some(symbol) = param(query, "symbol") else {
            return Some(invalid_body("missing_symbol"));
        };
        if !symbol.ends_with(".US") {
            return Some(invalid_body("unsupported_bar_market"));
        }
        let (Some(start), Some(end)) = (
            param(query, "start").and_then(date),
            param(query, "end").and_then(date),
        ) else {
            return Some(invalid_body("invalid_bar_range"));
        };
        let Some(url) = &self.quote_ws_url else {
            return Some(invalid_body("missing_quote_url"));
        };
        let token = match tokio::time::timeout(self.timeouts.token, self.oauth.access_token()).await
        {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => return Some(auth_failure(crate::sdk_error::oauth_kind(&e))),
            Err(_) => return Some(auth_failure("refresh_timeout")),
        };
        if !may_send() {
            return None;
        }
        let mut cache = self.quote.lock().await;
        // Move the connection OUT of the cache before awaiting anything.
        // Dropping this future then closes its socket; no queue can send later.
        let previous = cache
            .take()
            .filter(|(old, wire)| old == &token && wire.valid());
        let operation = async {
            let mut wire = match previous {
                Some((_, wire)) => wire,
                None => match super::wire::QuoteWire::open(self, url, &token, may_send).await {
                    Ok(Some(wire)) => wire,
                    Ok(None) => return None,
                    Err(fetched) => return Some(fetched),
                },
            };
            let request = quote::SecurityHistoryCandlestickRequest {
                symbol: symbol.to_owned(),
                period: 1000,
                adjust_type: 0,
                query_type: 2,
                offset_request: None,
                date_request: Some(quote::security_history_candlestick_request::DateQuery {
                    start_date: start.to_string().replace('-', ""),
                    end_date: end.to_string().replace('-', ""),
                }),
                trade_session: 0,
            };
            let raw = match wire.call(27, &request, self, may_send).await {
                Ok(Some(body)) => body,
                Ok(None) => return None,
                Err(fetched) => return Some(fetched),
            };
            let response = match quote::SecurityCandlestickResponse::decode(raw.as_slice()) {
                Ok(response) if response.symbol == symbol => response,
                _ => return Some(invalid_body("invalid_daily_bar_response")),
            };
            let body = match normalize(symbol, start, end, response.candlesticks) {
                Ok(body) => body,
                Err(code) => return Some(invalid_body(code)),
            };
            *cache = Some((token, wire));
            Some(Fetched::body(body.to_string()))
        };
        match tokio::time::timeout(self.timeouts.fetch, operation).await {
            Ok(result) => result,
            Err(_) => Some(invalid_body("quote_timeout")),
        }
    }
}

fn param<'a>(query: &'a [(String, String)], name: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}
fn date(s: &str) -> Option<time::Date> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    use chrono::Datelike;
    time::Date::from_calendar_date(
        d.year(),
        time::Month::try_from(d.month() as u8).ok()?,
        d.day() as u8,
    )
    .ok()
}
pub(super) fn auth_failure(code: &str) -> Fetched {
    Fetched {
        response: failed(
            FailureClass::AuthUnavailable,
            ErrorClass::Auth,
            code.to_owned(),
        ),
        retry_after: None,
    }
}
pub(super) fn quote_code(code: i64) -> Fetched {
    let throttled = code == 301606 || (429000..=429999).contains(&code);
    Fetched {
        response: failed(
            if throttled {
                FailureClass::Throttled
            } else {
                FailureClass::Permanent
            },
            ErrorClass::Envelope,
            code.to_string(),
        ),
        retry_after: throttled.then_some(super::RETRY_AFTER_MAX),
    }
}

fn normalize(
    symbol: &str,
    start: time::Date,
    end: time::Date,
    rows: Vec<Candlestick>,
) -> Result<Value, &'static str> {
    let mut by_date = std::collections::BTreeMap::new();
    for c in rows {
        let parse = |s: &str| s.parse::<Decimal>().map_err(|_| "invalid_daily_bar");
        let (open, close, high, low, turnover) = (
            parse(&c.open)?,
            parse(&c.close)?,
            parse(&c.high)?,
            parse(&c.low)?,
            parse(&c.turnover)?,
        );
        if [open, close, high, low].iter().any(|n| *n <= Decimal::ZERO)
            || low > high
            || open < low
            || open > high
            || close < low
            || close > high
            || c.volume < 0
            || turnover < Decimal::ZERO
            || c.trade_session != 0
        {
            return Err("invalid_daily_bar");
        }
        let timestamp =
            DateTime::<Utc>::from_timestamp(c.timestamp, 0).ok_or("invalid_bar_timestamp")?;
        let day = timestamp.with_timezone(&New_York).date_naive().to_string();
        if day < start.to_string() || day > end.to_string() || by_date.contains_key(&day) {
            return Err("invalid_bar_trade_date");
        }
        let value = json!({"trade_date":day,"timestamp":timestamp.to_rfc3339(),
            "open":c.open.to_string(),"close":c.close.to_string(),"high":c.high.to_string(),
            "low":c.low.to_string(),"volume":c.volume,"turnover":c.turnover.to_string()});
        by_date.insert(day, value);
    }
    // Six closes allow five session returns. Empty data remains an explicit value.
    let skip = by_date.len().saturating_sub(6);
    let bars: Vec<Value> = by_date.into_values().skip(skip).collect();
    Ok(
        json!({"format":"daily_bars_v1","symbol":symbol,"timezone":"America/New_York",
        "adjust":"none","session":"regular","start":start.to_string(),"end":end.to_string(),"bars":bars}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bar(timestamp: &str, close: &str) -> Candlestick {
        Candlestick {
            open: "10".into(),
            close: close.into(),
            high: "12".into(),
            low: "8".into(),
            volume: 20,
            turnover: "200.123456789".into(),
            trade_session: 0,
            timestamp: chrono::DateTime::parse_from_rfc3339(timestamp)
                .unwrap()
                .timestamp(),
        }
    }
    #[test]
    fn ny_trade_dates_and_decimal_strings_are_explicit() {
        for (stamp, day) in [
            ("2026-01-06T01:00:00Z", "2026-01-05"),
            ("2026-07-07T00:00:00Z", "2026-07-06"),
        ] {
            let d = date(day).unwrap();
            let body = normalize("MSFT.US", d, d, vec![bar(stamp, "10.123456789")]).unwrap();
            assert_eq!(body["bars"][0]["trade_date"], day);
            assert_eq!(body["bars"][0]["close"], "10.123456789");
            assert!(body["bars"][0].get("open_updated").is_none());
        }
    }
    #[test]
    fn malformed_decimals_and_inconsistent_ohlc_are_rejected() {
        let d = date("2026-10-20").unwrap();
        for close in ["0", "15", "malformed"] {
            assert!(normalize("MSFT.US", d, d, vec![bar("2026-10-20T20:00:00Z", close)]).is_err());
        }
    }
    #[test]
    fn permission_quota_and_throttle_codes_remain_distinct() {
        for code in [301604, 301607, 301606] {
            let fetched = quote_code(code);
            let crate::types::Response::Failed { class, error } = fetched.response() else {
                panic!("failure expected")
            };
            assert_eq!(error.code, code.to_string());
            assert_eq!(
                *class,
                if code == 301606 {
                    FailureClass::Throttled
                } else {
                    FailureClass::Permanent
                }
            );
        }
    }
}
