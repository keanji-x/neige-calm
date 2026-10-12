//! The calendar refresh through the SDK's quote connection.
//!
//! `QuoteContext::trading_days(market, begin, end)` returns
//! `MarketTradingDays { trading_days, half_trading_days }` as `time::Date`s
//! (`rust/src/quote/context.rs:1310-1344`, `rust/src/quote/types.rs:1338-1343`
//! at `longbridge/openapi@v5.2.0`). The context connects lazily on the first
//! request: an OTP over HTTP from `http_url`, then the quote WebSocket at
//! `quote_ws_url` (`rust/src/quote/core.rs:186-210`); a failed connect fails
//! the request. The context shares the capture's one `OAuth` client, and
//! every URL is passed explicitly. `dont_print_quote_packages` keeps the
//! SDK from printing the account's quote packages to stdout
//! (`core.rs:268-270`).

use std::{sync::Arc, time::Duration};

use chrono::NaiveDate;
use longbridge::{Config as SdkConfig, Language, Market, oauth::OAuth, quote::QuoteContext};

use super::TradingCalendar;
use crate::config::Config;
use crate::types::{FailureClass, Response, UnixMs};
use crate::{sdk_error, source};

/// The longest a refresh may take, connection included.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a refresh failed. It carries only the SDK error's variant name
/// ([`sdk_error::sdk_kind`]), never its text.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The SDK had no usable credentials. The job exits `AUTH_UNAVAILABLE`.
    #[error("the calendar refresh needs auth ({0})")]
    Auth(String),
    #[error("the calendar refresh failed ({0})")]
    Failed(String),
}

fn to_time(day: NaiveDate) -> time::Date {
    use chrono::Datelike;
    let month = u8::try_from(day.month())
        .ok()
        .and_then(|m| time::Month::try_from(m).ok())
        .expect("a chrono month is 1-12");
    let day_of_month = u8::try_from(day.day()).expect("a day of month is 1-31");
    time::Date::from_calendar_date(day.year(), month, day_of_month)
        .expect("a chrono date is a valid calendar date")
}

fn from_time(day: time::Date) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(
        day.year(),
        u32::from(u8::from(day.month())),
        u32::from(day.day()),
    )
}

/// The refresh failure for an SDK error. An HTTP error is an auth failure
/// exactly when the source's classification calls it `auth_unavailable`, so
/// both share one table.
pub fn refresh_error(err: longbridge::Error) -> RefreshError {
    let kind = sdk_error::sdk_kind(&err).into_owned();
    let auth = match err {
        longbridge::Error::OAuth(_) => true,
        longbridge::Error::HttpClient(http) => matches!(
            source::classify(http).response(),
            Response::Failed {
                class: FailureClass::AuthUnavailable,
                ..
            }
        ),
        _ => false,
    };
    if auth {
        RefreshError::Auth(kind)
    } else {
        RefreshError::Failed(kind)
    }
}

/// Fetch the US calendar for the refresh range at `now`.
pub async fn fetch(
    oauth: OAuth,
    config: &Config,
    now: UnixMs,
) -> Result<TradingCalendar, RefreshError> {
    let (from, to) = TradingCalendar::refresh_range(now);
    let sdk = SdkConfig::from_oauth(oauth)
        .http_url(config.http_url.clone())
        .quote_ws_url(config.quote_ws_url.clone())
        .language(Language::EN)
        .dont_print_quote_packages();
    let (context, _pushes) = QuoteContext::new(Arc::new(sdk));
    let answer = tokio::time::timeout(
        REFRESH_TIMEOUT,
        context.trading_days(Market::US, to_time(from), to_time(to)),
    )
    .await
    .map_err(|_| RefreshError::Failed("timeout".to_owned()))?;
    let days = answer.map_err(refresh_error)?;
    let convert = |days: Vec<time::Date>| -> Result<Vec<NaiveDate>, RefreshError> {
        let mut out = days
            .into_iter()
            .map(|d| from_time(d).ok_or_else(|| RefreshError::Failed("bad_date".to_owned())))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort();
        out.dedup();
        Ok(out)
    };
    let calendar = TradingCalendar {
        fetched_at: now.0,
        from,
        to,
        trading_days: convert(days.trading_days)?,
        half_days: convert(days.half_trading_days)?,
    };
    calendar
        .validate()
        .map_err(|reason| RefreshError::Failed(format!("invalid: {reason}")))?;
    Ok(calendar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_convert_both_ways() {
        for (y, m, d) in [(2026, 1, 1), (2026, 12, 31), (2028, 2, 29)] {
            let day = NaiveDate::from_ymd_opt(y, m, d).unwrap();
            assert_eq!(from_time(to_time(day)), Some(day));
        }
    }
}
