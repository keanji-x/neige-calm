//! One catalog request for one key, through the SDK's HTTP client, as the
//! Store's [`Response`].
//!
//! Requests use `HttpClient` with `.response::<String>()`: the SDK parses the
//! `{code, message, data}` envelope with `data` as a `RawValue` and hands its
//! bytes to `String::from_utf8` unchanged
//! (`rust/crates/httpclient/src/request.rs:80-87,116-121,452-470` at
//! `longbridge/openapi@v5.2.0`). The typed SDK methods, and even
//! `Json<serde_json::Value>`, would re-serialize numbers through `f64` and
//! reorder keys.
//!
//! The source never classifies a body: a body is [`Response::Body`], and the
//! Store decides what it is (a membership body may be incomplete). A failure
//! is [`Response::Failed`] with the class and the detail of [`classify`].
//!
//! What the SDK keeps of a failure decides the detail:
//! - an envelope with a non-zero `code` keeps `code` and `message`, but not
//!   the HTTP status (`request.rs:454-458`);
//! - a body that is not an envelope keeps the status (`request.rs:462-467`),
//!   unless the status is 200 (`request.rs:459-461`);
//! - `code: 0` with `data` absent or `null` is `UnexpectedResponse`
//!   (`request.rs:453`).
//!
//! **SDK retries bypass the limiter.** The SDK retries a non-envelope HTTP
//! 429 itself, up to five times with 0.1 s doubling backoff inside one
//! `send()` (`request.rs:475-513`). The [`RateLimiter`] paces only the
//! first start of each `fetch`; it cannot pace those retries, and nothing
//! here can turn them off. The throttle the server actually sends is the
//! envelope code `429002`, which the SDK does not retry, and probe P1 saw no
//! bare 429, so in practice every request start is paced.

use std::{num::NonZeroU32, sync::Mutex, time::Duration};

use longbridge::{
    httpclient::{DcRegion as SdkDcRegion, HttpClient, HttpClientConfig, HttpClientError, Method},
    oauth::OAuth,
};
use serde::{Serialize, Serializer, ser::SerializeMap};

use crate::catalog::{self, Endpoint, QueryValue};
use crate::sdk_error;
use crate::types::{ErrorClass, ErrorDetail, FailureClass, Key, Response};

/// Requests per second to one upstream, 1 to 20.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rps(NonZeroU32);

impl Rps {
    pub const MAX: u32 = 20;

    pub fn new(rps: u32) -> Option<Self> {
        NonZeroU32::new(rps)
            .filter(|n| n.get() <= Self::MAX)
            .map(Self)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// A token bucket of capacity one that paces request *starts*: each start is
/// at least `1 / rps` after the previous actual start, so any one-second
/// window holds at most `rps` starts. The next slot is set from the moment a
/// caller is released, not from its scheduled slot, so a late wake-up never
/// lets the following start crowd in behind it.
#[derive(Debug)]
pub struct RateLimiter {
    interval: Duration,
    next: Mutex<Option<tokio::time::Instant>>,
}

impl RateLimiter {
    pub fn new(rps: Rps) -> Self {
        Self {
            interval: Duration::from_secs(1) / rps.get(),
            next: Mutex::new(None),
        }
    }

    /// Wait until a start is allowed, and claim it.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut next = self.next.lock().expect("limiter lock is never poisoned");
                let now = tokio::time::Instant::now();
                match *next {
                    Some(at) if at > now => at,
                    _ => {
                        *next = Some(now + self.interval);
                        return;
                    }
                }
            };
            tokio::time::sleep_until(wait).await;
        }
    }
}

/// The wait after a throttle when the server names none.
pub const RETRY_AFTER_FALLBACK: Duration = Duration::from_secs(1);

/// The longest server-named wait; a longer one is clamped to it.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(60);

/// The production bound on one whole `fetch` (see [`LongbridgeSource::new`]):
/// above the SDK's 30 s request timeout, so it fires only when token
/// resolution or a token refresh hangs.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// The envelope code of the server's rate limit (probe P1).
pub const THROTTLE_CODE: i32 = 429_002;

/// One attempt's result: the Store's [`Response`], plus how long to wait
/// before retrying when it was throttled. The wait is not stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    response: Response,
    retry_after: Option<Duration>,
}

impl Fetched {
    fn body(data: String) -> Self {
        Self {
            response: Response::Body(data.into_bytes()),
            retry_after: None,
        }
    }

    pub fn response(&self) -> &Response {
        &self.response
    }

    /// Set exactly when the response is a `throttled` failure.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

/// The catalog source for Longbridge.
pub struct LongbridgeSource {
    http: HttpClient,
    limiter: RateLimiter,
    fetch_timeout: Duration,
}

impl LongbridgeSource {
    /// `oauth` is the process's one shared client; `http_url` is pinned, never
    /// taken from the environment. `fetch_timeout` bounds one whole `fetch`
    /// after its rate-limit slot: token resolution, any token refresh, the
    /// request and the SDK's own retries ([`FETCH_TIMEOUT`] in production).
    /// It expires as `transient` / `transport` / `request_timeout`.
    pub fn new(oauth: OAuth, http_url: &str, rps: Rps, fetch_timeout: Duration) -> Self {
        Self {
            http: HttpClient::new(HttpClientConfig::from_oauth(oauth).http_url(http_url)),
            limiter: RateLimiter::new(rps),
            fetch_timeout,
        }
    }

    /// Perform `endpoint` for `key` once, after waiting for a rate-limit slot.
    pub async fn fetch(&self, endpoint: &Endpoint, key: &Key) -> Fetched {
        self.limiter.acquire().await;
        let method = match endpoint.method {
            catalog::Method::Get => Method::GET,
        };
        let mut request = self.http.request(method, endpoint.path);
        for (name, value) in endpoint.headers {
            request = request.header(*name, *value);
        }
        if let Some(region) = endpoint.dc_restrict {
            request = request.dc_restrict(match region {
                catalog::DcRegion::Us => SdkDcRegion::Us,
            });
        }
        let send = request
            .query_params(KeyQuery {
                key: key.as_str(),
                query: endpoint.query,
            })
            .response::<String>()
            .send();
        // The SDK's own 30 s timeout starts only after the access token is
        // resolved, and a token refresh (oauth2's reqwest client) has none.
        // Dropping the future on expiry also drops the `OAuth` lock guard
        // the refresh holds.
        match tokio::time::timeout(self.fetch_timeout, send).await {
            Ok(Ok(data)) => Fetched::body(data),
            Ok(Err(err)) => classify(err),
            Err(_elapsed) => Fetched {
                response: failed(
                    FailureClass::Transient,
                    ErrorClass::Transport,
                    "request_timeout".to_owned(),
                ),
                retry_after: None,
            },
        }
    }
}

/// The endpoint's query pairs in catalog order, the key in its slot.
struct KeyQuery<'a> {
    key: &'a str,
    query: &'a [(&'a str, QueryValue)],
}

impl Serialize for KeyQuery<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.query.len()))?;
        for (name, value) in self.query {
            match value {
                QueryValue::Key => map.serialize_entry(name, self.key)?,
                QueryValue::Fixed(v) => map.serialize_entry(name, v)?,
            }
        }
        map.end()
    }
}

/// The class of a non-envelope HTTP status. A non-envelope 1xx-3xx is a
/// response that is not an envelope, like a non-envelope 200.
fn class_for_status(status: u16) -> (FailureClass, ErrorClass) {
    match status {
        429 => (FailureClass::Throttled, ErrorClass::HttpStatus),
        401 | 403 => (FailureClass::AuthUnavailable, ErrorClass::HttpStatus),
        408 | 500..=599 => (FailureClass::Transient, ErrorClass::HttpStatus),
        400..=499 => (FailureClass::Permanent, ErrorClass::HttpStatus),
        _ => (FailureClass::Transient, ErrorClass::UnexpectedResponse),
    }
}

/// The wait a throttle message names, as in "rate limit of 1-second
/// interval has been reached, please retry after: 0.2s" (probe P1). A wait
/// over [`RETRY_AFTER_MAX`] is clamped to it; a message that names no
/// positive wait gives [`RETRY_AFTER_FALLBACK`].
pub fn parse_retry_after(message: &str) -> Duration {
    const MARKER: &str = "retry after:";
    let lower = message.to_ascii_lowercase();
    lower
        .rfind(MARKER)
        .map(|at| lower[at + MARKER.len()..].trim_start())
        .and_then(|rest| {
            let number: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            rest[number.len()..].starts_with('s').then_some(number)
        })
        .and_then(|number| number.parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs > 0.0)
        .map(|secs| Duration::from_secs_f64(secs.min(RETRY_AFTER_MAX.as_secs_f64())))
        .unwrap_or(RETRY_AFTER_FALLBACK)
}

/// A failure response.
fn failed(class: FailureClass, error: ErrorClass, code: String) -> Response {
    Response::Failed {
        class,
        error: ErrorDetail { class: error, code },
    }
}

/// Map one SDK error to a failure. The error code is the envelope code or
/// the HTTP status in decimal, or else the SDK variant
/// ([`sdk_error::http_kind`]). No error text is kept.
pub fn classify(err: HttpClientError) -> Fetched {
    use ErrorClass as E;
    use FailureClass as F;
    let mut retry_after = None;
    let response = match &err {
        HttpClientError::OpenApi { code, message, .. } => {
            let class = if *code == THROTTLE_CODE {
                retry_after = Some(parse_retry_after(message));
                F::Throttled
            } else {
                F::Permanent
            };
            failed(class, E::Envelope, code.to_string())
        }
        HttpClientError::UnexpectedHttpResponse { status, .. }
        | HttpClientError::BadStatus(status) => {
            let (class, error) = class_for_status(status.as_u16());
            if class == F::Throttled {
                retry_after = Some(RETRY_AFTER_FALLBACK);
            }
            let code = match error {
                E::HttpStatus => status.as_u16().to_string(),
                _ => sdk_error::http_kind(&err).to_owned(),
            };
            failed(class, error, code)
        }
        other => {
            let (class, error) = match other {
                // A 200 whose body is not an envelope: a proxy or gateway page.
                HttpClientError::DeserializeResponseBody(_) => {
                    (F::Transient, E::UnexpectedResponse)
                }
                HttpClientError::UnexpectedResponse => (F::Permanent, E::UnexpectedResponse),
                HttpClientError::RequestTimeout | HttpClientError::Http(_) => {
                    (F::Transient, E::Transport)
                }
                HttpClientError::DcRegionRestricted { .. } => {
                    (F::RegionRestricted, E::DcRegionRestricted)
                }
                HttpClientError::OAuth(_)
                | HttpClientError::InvalidAccessToken
                | HttpClientError::InvalidApiKey => (F::AuthUnavailable, E::Auth),
                _ => (F::Permanent, E::Client),
            };
            failed(class, error, sdk_error::http_kind(other).to_owned())
        }
    };
    Fetched {
        response,
        retry_after,
    }
}

#[cfg(test)]
mod tests;
