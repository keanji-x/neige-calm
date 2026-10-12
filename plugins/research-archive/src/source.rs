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
//! The SDK retries only non-envelope HTTP 429 responses, five times with
//! 0.1 s doubling backoff inside one `send()` (`request.rs:475-513`). Those
//! retries bypass [`RateLimiter`]. The throttle the server actually sends is
//! the envelope code `429002`, which the SDK does not retry (probe P1).

use std::{num::NonZeroU32, sync::Mutex, time::Duration};

use longbridge::{
    httpclient::{DcRegion as SdkDcRegion, HttpClient, HttpClientConfig, HttpClientError, Method},
    oauth::OAuth,
};
use serde::{Serialize, Serializer, ser::SerializeMap};

use crate::catalog::{self, Endpoint, QueryValue};
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

/// The longest server-named wait taken as is; a longer or unparsable one
/// falls back to [`RETRY_AFTER_FALLBACK`].
const RETRY_AFTER_MAX: Duration = Duration::from_secs(60);

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
}

impl LongbridgeSource {
    /// `oauth` is the process's one shared client; `http_url` is pinned, never
    /// taken from the environment.
    pub fn new(oauth: OAuth, http_url: &str, rps: Rps) -> Self {
        Self {
            http: HttpClient::new(HttpClientConfig::from_oauth(oauth).http_url(http_url)),
            limiter: RateLimiter::new(rps),
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
        let result = request
            .query_params(KeyQuery {
                key: key.as_str(),
                query: endpoint.query,
            })
            .response::<String>()
            .send()
            .await;
        match result {
            Ok(data) => Fetched::body(data),
            Err(err) => classify(err),
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
/// interval has been reached, please retry after: 0.2s" (probe P1), or
/// [`RETRY_AFTER_FALLBACK`] when it names none or an implausible one.
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
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
        .filter(|wait| !wait.is_zero() && *wait <= RETRY_AFTER_MAX)
        .unwrap_or(RETRY_AFTER_FALLBACK)
}

/// Map one SDK error to a failure. Every variant is named, so an SDK upgrade
/// that adds one fails to compile here. The error code is the envelope code
/// or HTTP status in decimal, or else the SDK variant in snake_case.
pub fn classify(err: HttpClientError) -> Fetched {
    let failed = |class, error: ErrorClass, code: String| Response::Failed {
        class,
        error: ErrorDetail { class: error, code },
    };
    let variant = |class, error, name: &str| failed(class, error, name.to_owned());
    let mut retry_after = None;
    let response = match err {
        HttpClientError::OpenApi { code, message, .. } if code == THROTTLE_CODE => {
            retry_after = Some(parse_retry_after(&message));
            failed(
                FailureClass::Throttled,
                ErrorClass::Envelope,
                code.to_string(),
            )
        }
        HttpClientError::OpenApi { code, .. } => failed(
            FailureClass::Permanent,
            ErrorClass::Envelope,
            code.to_string(),
        ),
        HttpClientError::UnexpectedHttpResponse { status, .. }
        | HttpClientError::BadStatus(status) => {
            let (class, error) = class_for_status(status.as_u16());
            if class == FailureClass::Throttled {
                retry_after = Some(RETRY_AFTER_FALLBACK);
            }
            match error {
                ErrorClass::HttpStatus => failed(class, error, status.as_u16().to_string()),
                _ => variant(class, error, "unexpected_http_response"),
            }
        }
        // A 200 whose body is not an envelope: a proxy or gateway page.
        HttpClientError::DeserializeResponseBody(_) => variant(
            FailureClass::Transient,
            ErrorClass::UnexpectedResponse,
            "deserialize_response_body",
        ),
        HttpClientError::UnexpectedResponse => variant(
            FailureClass::Permanent,
            ErrorClass::UnexpectedResponse,
            "unexpected_response",
        ),
        HttpClientError::RequestTimeout => variant(
            FailureClass::Transient,
            ErrorClass::Transport,
            "request_timeout",
        ),
        HttpClientError::Http(_) => variant(FailureClass::Transient, ErrorClass::Transport, "http"),
        HttpClientError::DcRegionRestricted { .. } => variant(
            FailureClass::RegionRestricted,
            ErrorClass::DcRegionRestricted,
            "dc_region_restricted",
        ),
        HttpClientError::OAuth(_) => {
            variant(FailureClass::AuthUnavailable, ErrorClass::Auth, "oauth")
        }
        HttpClientError::InvalidAccessToken => variant(
            FailureClass::AuthUnavailable,
            ErrorClass::Auth,
            "invalid_access_token",
        ),
        HttpClientError::InvalidApiKey => variant(
            FailureClass::AuthUnavailable,
            ErrorClass::Auth,
            "invalid_api_key",
        ),
        HttpClientError::InvalidRequestMethod => variant(
            FailureClass::Permanent,
            ErrorClass::Client,
            "invalid_request_method",
        ),
        HttpClientError::MissingEnvVar { .. } => variant(
            FailureClass::Permanent,
            ErrorClass::Client,
            "missing_env_var",
        ),
        HttpClientError::SerializeRequestBody(_) => variant(
            FailureClass::Permanent,
            ErrorClass::Client,
            "serialize_request_body",
        ),
        HttpClientError::SerializeQueryString(_) => variant(
            FailureClass::Permanent,
            ErrorClass::Client,
            "serialize_query_string",
        ),
        HttpClientError::ConnectionLimitExceeded { .. } => variant(
            FailureClass::Permanent,
            ErrorClass::Client,
            "connection_limit_exceeded",
        ),
        HttpClientError::Sse(_) => variant(FailureClass::Permanent, ErrorClass::Client, "sse"),
    };
    Fetched {
        response,
        retry_after,
    }
}

#[cfg(test)]
mod tests;
