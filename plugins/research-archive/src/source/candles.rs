//! Versioned typed daily-bar projection. This is not an HTTP/raw-wire body.
use super::{Fetched, LongbridgeSource, failed, invalid_body, snapshot_oauth};
use crate::types::{ErrorClass, FailureClass};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::America::New_York;
use longbridge::{
    Decimal,
    quote::{AdjustType, Candlestick, Period, QuoteContext, TradeSession, TradeSessions},
};
use serde_json::{Value, json};
use std::sync::Arc;

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
        let context = {
            let mut cached = self.quote.lock().await;
            if !cached.as_ref().is_some_and(|(t, _)| t == &token) {
                let oauth = match snapshot_oauth(self.oauth.client_id(), &token).await {
                    Ok(o) => o,
                    Err(e) => return Some(auth_failure(crate::sdk_error::oauth_kind(&e))),
                };
                let config = longbridge::Config::from_oauth(oauth)
                    .http_url(&self.http_url)
                    .quote_ws_url(url)
                    .language(longbridge::Language::EN)
                    .dont_print_quote_packages();
                let (context, _pushes) = QuoteContext::new(Arc::new(config));
                // OTP bootstrap is an upstream HTTP start too. Pace it once,
                // under this initialization lock, before accepting the context.
                self.limiter.acquire().await;
                if !may_send() {
                    return None;
                }
                match tokio::time::timeout(self.timeouts.fetch, context.quote_level()).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => return Some(classify_quote(err)),
                    Err(_) => return Some(invalid_body("quote_connect_timeout")),
                }
                *cached = Some((token, context));
            }
            cached.as_ref().expect("quote initialized").1.clone()
        };
        if !may_send() {
            return None;
        }
        self.limiter.acquire().await;
        if !may_send() {
            return None;
        }
        let result = tokio::time::timeout(
            self.timeouts.fetch,
            context.history_candlesticks_by_date(
                symbol,
                Period::Day,
                AdjustType::NoAdjust,
                Some(start),
                Some(end),
                TradeSessions::Intraday,
            ),
        )
        .await;
        Some(match result {
            Ok(Ok(rows)) => match normalize(symbol, start, end, rows) {
                Ok(body) => Fetched::body(body.to_string()),
                Err(code) => invalid_body(code),
            },
            Ok(Err(err)) => classify_quote(err),
            Err(_) => invalid_body("quote_timeout"),
        })
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
fn auth_failure(code: &str) -> Fetched {
    Fetched {
        response: failed(
            FailureClass::AuthUnavailable,
            ErrorClass::Auth,
            code.to_owned(),
        ),
        retry_after: None,
    }
}
fn classify_quote(err: longbridge::Error) -> Fetched {
    if let longbridge::Error::HttpClient(http) = err {
        return super::classify(http);
    }
    if matches!(err, longbridge::Error::OAuth(_)) {
        return auth_failure("quote_oauth");
    }
    if let Some(code) = err.openapi_error_code() {
        return quote_code(code);
    }
    Fetched {
        response: failed(
            FailureClass::Transient,
            ErrorClass::Transport,
            crate::sdk_error::sdk_kind(&err).into_owned(),
        ),
        retry_after: None,
    }
}
fn quote_code(code: i64) -> Fetched {
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
        if [c.open, c.close, c.high, c.low]
            .iter()
            .any(|n| *n <= Decimal::ZERO)
            || c.low > c.high
            || c.open < c.low
            || c.open > c.high
            || c.close < c.low
            || c.close > c.high
            || c.volume < 0
            || c.turnover < Decimal::ZERO
            || c.trade_session != TradeSession::Intraday
        {
            return Err("invalid_daily_bar");
        }
        let timestamp = DateTime::<Utc>::from_timestamp(c.timestamp.unix_timestamp(), 0)
            .ok_or("invalid_bar_timestamp")?;
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
        serde_json::from_value(json!({"timestamp":timestamp,"open":"10","close":close,
            "high":"12","low":"8","volume":20,"turnover":"200.123456789",
            "trade_session":"Intraday","open_updated":true}))
        .unwrap()
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
    fn sdk_zero_fallback_and_inconsistent_ohlc_are_rejected() {
        let d = date("2026-10-20").unwrap();
        for close in ["0", "15"] {
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
