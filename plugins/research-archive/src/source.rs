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
    oauth::{OAuth, OAuthBuilder, OAuthError, OAuthResult, StoredToken, TokenStorage},
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

/// The bounds of one `fetch` (see [`LongbridgeSource::fetch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Resolving the access token, a refresh included. oauth2's HTTP client
    /// has no timeout of its own. Expiry is `auth_unavailable` / `auth` /
    /// `refresh_timeout`.
    pub token: Duration,
    /// The request after its rate-limit slot, the SDK's retries included.
    /// Expiry is `transient` / `transport` / `request_timeout`.
    pub fetch: Duration,
}

impl Timeouts {
    /// Production bounds. `fetch` is above the SDK's own 30 s request
    /// timeout, so it fires only if the SDK itself hangs.
    pub const PRODUCTION: Timeouts = Timeouts {
        token: Duration::from_secs(30),
        fetch: Duration::from_secs(60),
    };
}

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
///
/// **One place resolves the token.** `fetch` asks the shared, refreshing
/// `OAuth` for the access token under [`Timeouts::token`], then sends through
/// an `HttpClient` built on a snapshot of that token which can never refresh
/// ([`snapshot_client`]). The SDK's own token resolution inside `send()`
/// (`httpclient/src/request.rs:316-331`) therefore reads the snapshot and
/// returns at once: no refresh can start under [`Timeouts::fetch`], even when
/// the token crosses the SDK's five-minute margin while the fetch waits for
/// its rate-limit slot. The snapshot client is cached per token value, so
/// connections are reused and `HttpClientConfig::from_oauth` (which also
/// runs `dotenv()`) is paid only when the token changes.
pub struct LongbridgeSource {
    oauth: OAuth,
    http_url: String,
    snapshot: Mutex<Option<(String, HttpClient)>>,
    limiter: RateLimiter,
    timeouts: Timeouts,
}

impl LongbridgeSource {
    /// `oauth` is the process's one shared, refreshing client; `http_url` is
    /// pinned, never taken from the environment.
    pub fn new(oauth: OAuth, http_url: &str, rps: Rps, timeouts: Timeouts) -> Self {
        Self {
            oauth,
            http_url: http_url.to_owned(),
            snapshot: Mutex::new(None),
            limiter: RateLimiter::new(rps),
            timeouts,
        }
    }

    /// Perform `endpoint` for `key` once, in this order:
    ///
    /// 1. **Resolve the access token**, a refresh included, within
    ///    `timeouts.token`. A failure is `auth_unavailable` / `auth` / the
    ///    OAuth error kind, or `refresh_timeout`; it takes no rate-limit slot
    ///    and sends nothing. Like every auth pause it is not a failure of the
    ///    pair, so it never counts toward the attempt cap.
    /// 2. **Take a rate-limit slot.** Callers that queued behind a refresh
    ///    (the `OAuth` lock) are paced from here, not released together.
    /// 3. **Send** through the snapshot client of that token within
    ///    `timeouts.fetch`. A token inside the SDK's margin but not yet
    ///    expired is still sent: the SDK checks expiry only to decide on a
    ///    refresh (`oauth/src/token.rs:59-66`), never before a request.
    pub async fn fetch(&self, endpoint: &Endpoint, key: &Key) -> Fetched {
        self.fetch_if(endpoint, key, || true)
            .await
            .expect("an unconditional fetch always sends")
    }

    /// [`fetch`](Self::fetch), asking `may_send` once more at the actual
    /// request start, after the rate-limit slot and right before the send.
    /// `None` when it says no: nothing was sent, and there is nothing to
    /// record. The capture runner passes its deadline check here, so no
    /// request starts at or after the deadline, however long the slot took.
    pub async fn fetch_if(
        &self,
        endpoint: &Endpoint,
        key: &Key,
        may_send: impl FnOnce() -> bool,
    ) -> Option<Fetched> {
        let auth_failure = |code: &str| Fetched {
            response: failed(
                FailureClass::AuthUnavailable,
                ErrorClass::Auth,
                code.to_owned(),
            ),
            retry_after: None,
        };
        let token = match tokio::time::timeout(self.timeouts.token, self.oauth.access_token()).await
        {
            Ok(Ok(token)) => token,
            Ok(Err(err)) => return Some(auth_failure(sdk_error::oauth_kind(&err))),
            Err(_elapsed) => return Some(auth_failure("refresh_timeout")),
        };
        let http = match self.snapshot_for(token).await {
            Ok(http) => http,
            Err(err) => return Some(auth_failure(sdk_error::oauth_kind(&err))),
        };
        self.limiter.acquire().await;
        if !may_send() {
            return None;
        }
        let method = match endpoint.method {
            catalog::Method::Get => Method::GET,
        };
        let mut request = http.request(method, endpoint.path);
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
        Some(
            match tokio::time::timeout(self.timeouts.fetch, send).await {
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
            },
        )
    }

    /// The cached snapshot client for `token`, built when the token changed.
    async fn snapshot_for(&self, token: String) -> OAuthResult<HttpClient> {
        if let Some((cached, http)) = &*self.snapshot.lock().expect("never poisoned")
            && *cached == token
        {
            return Ok(http.clone());
        }
        let http = snapshot_client(self.oauth.client_id(), &token, &self.http_url).await?;
        *self.snapshot.lock().expect("never poisoned") = Some((token, http.clone()));
        Ok(http)
    }
}

/// Token storage that holds one token and never saves.
struct Snapshot(StoredToken);

impl TokenStorage for Snapshot {
    fn load(&self, _client_id: &str) -> Option<StoredToken> {
        Some(self.0.clone())
    }

    fn save(&self, _token: &StoredToken) -> OAuthResult<()> {
        Err(OAuthError::Other("a token snapshot is never saved".into()))
    }
}

/// An `HttpClient` that sends `token` and can never refresh it.
///
/// Its `OAuth` is built from storage holding `token` with no refresh token
/// and an expiry of `u64::MAX`, so the SDK loads it without network
/// (`oauth/src/builder.rs:116-124`), never finds it expiring
/// (`token.rs:59-66`) and never refreshes (`client.rs:96-112`). The real
/// expiry is the shared `OAuth`'s concern. Unlike `OAuth::from_token`, which
/// has an empty client id (`client.rs:68-81`), this keeps the client id the
/// SDK sends as `X-Api-Key` (`request.rs:326`), so a request carries the
/// same headers as through the shared client.
async fn snapshot_client(client_id: &str, token: &str, http_url: &str) -> OAuthResult<HttpClient> {
    let oauth = OAuthBuilder::new(client_id)
        .callback_port(0)
        .token_storage(Snapshot(StoredToken {
            client_id: client_id.to_owned(),
            access_token: token.to_owned(),
            refresh_token: None,
            expires_at: u64::MAX,
        }))
        // Unreachable: the storage always yields a token that never expires.
        .build(|_| tracing::error!("a token snapshot asked for a browser login"))
        .await?;
    Ok(HttpClient::new(
        HttpClientConfig::from_oauth(oauth).http_url(http_url),
    ))
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
