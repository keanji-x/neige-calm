//! The real `LongbridgeSource` against the fake server, through the real
//! Store: raw bytes (I3), the request each catalog entry sends, and the
//! classification of every failure into a `Response` the Store accepts.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use longbridge::oauth::{OAuth, OAuthBuilder, OAuthResult, StoredToken, TokenStorage};
use research_archive::StoreError;
use research_archive::catalog::{Catalog, DcRegion, Endpoint, KeyKind, QueryValue};
use research_archive::source::{Fetched, LongbridgeSource, RETRY_AFTER_FALLBACK, Rps};
use research_archive::store::RunPlan;
use research_archive::types::{ErrorClass, ErrorDetail, FailureClass, Response, Status, UnixMs};

use crate::fake::{FakeServer, Reply};
use crate::support::{Fixture, at, fresh, key, plan_request, raw};

const TOKEN: &str = "ap_m_CANARY-TOKEN-0b7e";
/// A decimal longer than f64 and keys out of order.
const BODY: &str = r#"{"b":1,"a":0.12345678901234567890123}"#;

pub fn source(url: &str, oauth: OAuth) -> LongbridgeSource {
    LongbridgeSource::new(oauth, url, Rps::new(20).unwrap())
}

fn endpoint(id: &str) -> Endpoint {
    *Catalog::compiled().get(id).unwrap()
}

/// A store with day 1 planned for `AAPL.US` and `.SPX.US`.
async fn planned() -> (Fixture, RunPlan) {
    let f = fresh().await;
    let plan = f
        .store
        .plan_run(&plan_request(1, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    (f, plan)
}

/// The stored `(status, error_class, error_code)` of the latest attempt.
async fn stored_row(f: &Fixture, ep: &str, k: &str) -> (String, Option<String>, Option<String>) {
    let mut conn = raw(&f.path, false).await;
    sqlx::query_as(
        "SELECT status, error_class, error_code FROM observation
         WHERE key = ? AND endpoint_id = ? ORDER BY attempt DESC LIMIT 1",
    )
    .bind(k)
    .bind(ep)
    .fetch_one(&mut conn)
    .await
    .unwrap()
}

fn failed(class: FailureClass, error: ErrorClass, code: &str) -> Response {
    Response::Failed {
        class,
        error: ErrorDetail {
            class: error,
            code: code.into(),
        },
    }
}

/// I3: the stored bytes are exactly the envelope's `data`: a decimal longer
/// than f64 keeps every digit and unsorted keys keep their order, through
/// `record_observation` and `read_at`.
#[tokio::test]
async fn i3_data_bytes_are_stored_and_read_back_exactly() {
    let server = FakeServer::start(|_| Reply::data(BODY)).await;
    let (f, plan) = planned().await;
    let valuation = endpoint("valuation");
    let fetched = source(&server.url, OAuth::from_token(TOKEN))
        .fetch(&valuation, &key("AAPL.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &Response::Body(BODY.as_bytes().to_vec())
    );
    assert_eq!(fetched.retry_after(), None);

    let recorded = f
        .store
        .record_observation(
            plan.run_id,
            &key("AAPL.US"),
            valuation.id,
            at(1, 10),
            fetched.response(),
        )
        .await
        .unwrap();
    assert_eq!(recorded.status, Status::Ok);
    let read = f
        .store
        .read_at(&key("AAPL.US"), valuation.id, at(1, 10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.body, BODY.as_bytes());
    assert_eq!(read.fetched_at, at(1, 10));
}

#[tokio::test]
async fn every_catalog_entry_sends_its_path_query_and_headers() {
    let server = FakeServer::start(|_| Reply::data("{}")).await;
    let source = source(&server.url, OAuth::from_token(TOKEN));
    let catalog = Catalog::compiled();
    let mut expected = Vec::new();
    for e in catalog.endpoints() {
        let k = match e.key_kind {
            KeyKind::Index => ".SPX.US",
            KeyKind::Security => "AAPL.US",
        };
        let fetched = source.fetch(e, &key(k)).await;
        assert_eq!(
            fetched.response(),
            &Response::Body(b"{}".to_vec()),
            "{}",
            e.id
        );
        let query: Vec<String> = e
            .query
            .iter()
            .map(|(name, value)| match value {
                QueryValue::Key => format!("{name}={k}"),
                QueryValue::Fixed(v) => format!("{name}={v}"),
            })
            .collect();
        expected.push((e.path.to_owned(), query.join("&")));
    }
    let seen = server.seen();
    let sent: Vec<_> = seen
        .iter()
        .map(|s| (s.path.clone(), s.query.clone().unwrap()))
        .collect();
    assert_eq!(sent, expected);
    assert_eq!(
        sent[1],
        (
            "/v1/quote/forecast-eps".to_owned(),
            "symbol=AAPL.US".to_owned()
        )
    );
    for s in &seen {
        assert_eq!(s.method, "GET");
        assert_eq!(s.header("accept-language"), Some("en"));
        assert_eq!(s.header("authorization"), Some(&*format!("Bearer {TOKEN}")));
        assert_eq!(s.header("x-dc-region"), Some("ap"));
    }
}

/// One classification case: what the fake returns, and the failure the
/// source must hand the Store.
struct Case {
    status: u16,
    body: &'static str,
    expected: Response,
    retry_after: Option<Duration>,
}

fn case(
    status: u16,
    body: &'static str,
    class: FailureClass,
    error: ErrorClass,
    code: &str,
) -> Case {
    Case {
        status,
        body,
        expected: failed(class, error, code),
        retry_after: (class == FailureClass::Throttled).then_some(RETRY_AFTER_FALLBACK),
    }
}

/// Every HTTP-shaped row of the classification table, each recorded through
/// the real Store with the class and detail it stores.
#[tokio::test]
async fn http_failures_are_classified_into_stored_failures() {
    use ErrorClass as E;
    use FailureClass as F;
    let throttle = r#"{"code":429002,"message":"rate limit of 1-second interval has been reached, please retry after: 0.4s"}"#;
    let cases = [
        Case {
            retry_after: Some(Duration::from_millis(400)),
            ..case(200, throttle, F::Throttled, E::Envelope, "429002")
        },
        case(
            200,
            r#"{"code":429002,"message":"slow down"}"#,
            F::Throttled,
            E::Envelope,
            "429002",
        ),
        case(
            200,
            r#"{"code":2601400,"message":"internal server error","data":null}"#,
            F::Permanent,
            E::Envelope,
            "2601400",
        ),
        // The SDK drops the HTTP status of an envelope: the code decides.
        case(
            500,
            r#"{"code":500100,"message":"busy"}"#,
            F::Permanent,
            E::Envelope,
            "500100",
        ),
        case(
            200,
            r#"{"code":0,"message":"","data":null}"#,
            F::Permanent,
            E::UnexpectedResponse,
            "unexpected_response",
        ),
        case(
            200,
            r#"{"code":0,"message":""}"#,
            F::Permanent,
            E::UnexpectedResponse,
            "unexpected_response",
        ),
        case(
            200,
            "<html>proxy</html>",
            F::Transient,
            E::UnexpectedResponse,
            "deserialize_response_body",
        ),
        case(
            202,
            "<html>accepted</html>",
            F::Transient,
            E::UnexpectedResponse,
            "unexpected_http_response",
        ),
        case(500, "<html>oops</html>", F::Transient, E::HttpStatus, "500"),
        case(
            503,
            "<html>gateway</html>",
            F::Transient,
            E::HttpStatus,
            "503",
        ),
        case(
            401,
            "<html>who</html>",
            F::AuthUnavailable,
            E::HttpStatus,
            "401",
        ),
        case(
            403,
            "<html>no</html>",
            F::AuthUnavailable,
            E::HttpStatus,
            "403",
        ),
        case(404, "<html>none</html>", F::Permanent, E::HttpStatus, "404"),
    ];
    let (f, plan) = planned().await;
    let consensus = endpoint("consensus");
    for (i, c) in cases.into_iter().enumerate() {
        let (status, body) = (c.status, c.body);
        let server = FakeServer::start(move |_| Reply::new(status, body)).await;
        let fetched = source(&server.url, OAuth::from_token(TOKEN))
            .fetch(&consensus, &key("AAPL.US"))
            .await;
        assert_eq!(fetched.response(), &c.expected, "{status} {body}");
        assert_eq!(fetched.retry_after(), c.retry_after, "{status} {body}");
        assert_eq!(server.seen().len(), 1, "{status} {body} is not retried");
        assert_stored(&f, &plan, &consensus, at(1, 10 + i as i64), &fetched).await;
    }
}

/// Records `fetched` and checks the stored row repeats its class and detail.
async fn assert_stored(f: &Fixture, plan: &RunPlan, e: &Endpoint, t: UnixMs, fetched: &Fetched) {
    let recorded = f
        .store
        .record_observation(plan.run_id, &key("AAPL.US"), e.id, t, fetched.response())
        .await
        .unwrap();
    let Response::Failed { class, error } = fetched.response() else {
        panic!("expected a failure");
    };
    assert_eq!(recorded.status, Status::Failed(*class));
    assert_eq!(
        stored_row(f, e.id.as_str(), "AAPL.US").await,
        (
            recorded.status.as_str().to_owned(),
            Some(error.class.as_str().to_owned()),
            Some(error.code.clone())
        )
    );
}

/// A non-envelope 429 is retried five times inside the SDK, then throttled.
#[tokio::test]
async fn a_bare_429_is_retried_by_the_sdk_then_throttled() {
    let server = FakeServer::start(|_| Reply::new(429, "slow down")).await;
    let (f, plan) = planned().await;
    let forecast = endpoint("forecast_eps");
    let fetched = source(&server.url, OAuth::from_token(TOKEN))
        .fetch(&forecast, &key("AAPL.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &failed(FailureClass::Throttled, ErrorClass::HttpStatus, "429")
    );
    assert_eq!(fetched.retry_after(), Some(RETRY_AFTER_FALLBACK));
    assert_eq!(server.seen().len(), 6);
    assert_stored(&f, &plan, &forecast, at(1, 10), &fetched).await;
}

/// The SDK's 30 s request timeout, on a paused clock.
#[tokio::test(start_paused = true)]
async fn a_hanging_server_is_a_transport_timeout() {
    let server = FakeServer::start(|_| Reply::data("{}").after(Duration::from_secs(3600))).await;
    let fetched = source(&server.url, OAuth::from_token(TOKEN))
        .fetch(&endpoint("ratings"), &key("AAPL.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &failed(
            FailureClass::Transient,
            ErrorClass::Transport,
            "request_timeout"
        )
    );
}

#[tokio::test]
async fn a_refused_connection_is_a_transport_failure() {
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let (f, plan) = planned().await;
    let ratings = endpoint("ratings");
    let fetched = source(&format!("http://{closed}"), OAuth::from_token(TOKEN))
        .fetch(&ratings, &key("AAPL.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &failed(FailureClass::Transient, ErrorClass::Transport, "http")
    );
    assert_stored(&f, &plan, &ratings, at(1, 10), &fetched).await;
}

/// A token that is not a valid header value fails locally.
#[tokio::test]
async fn an_unusable_access_token_is_auth_unavailable() {
    let server = FakeServer::start(|_| Reply::data("{}")).await;
    let (f, plan) = planned().await;
    let ratings = endpoint("ratings");
    let fetched = source(&server.url, OAuth::from_token("ap_bad\ntoken"))
        .fetch(&ratings, &key("AAPL.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &failed(
            FailureClass::AuthUnavailable,
            ErrorClass::Auth,
            "invalid_access_token"
        )
    );
    assert!(server.seen().is_empty());
    assert_stored(&f, &plan, &ratings, at(1, 10), &fetched).await;
}

/// The DC guard fails locally. No catalog endpoint is restricted, so the
/// Store refuses the class for every compiled endpoint.
#[tokio::test]
async fn a_dc_restricted_endpoint_is_region_restricted() {
    let server = FakeServer::start(|_| Reply::data("{}")).await;
    let us_only = Endpoint {
        dc_restrict: Some(DcRegion::Us),
        ..endpoint("consensus")
    };
    let fetched = source(&server.url, OAuth::from_token(TOKEN))
        .fetch(&us_only, &key("AAPL.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &failed(
            FailureClass::RegionRestricted,
            ErrorClass::DcRegionRestricted,
            "dc_region_restricted"
        )
    );
    assert!(server.seen().is_empty());
    let (f, plan) = planned().await;
    let err = f
        .store
        .record_observation(
            plan.run_id,
            &key("AAPL.US"),
            us_only.id,
            at(1, 10),
            fetched.response(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::ErrorClassForEndpoint { .. }),
        "{err}"
    );
}

/// The source hands a membership body over as is; the Store files an
/// incomplete one as `permanent` / `incomplete_membership`.
#[tokio::test]
async fn the_store_not_the_source_judges_a_membership_body() {
    let partial = r#"{"total":501,"stocks":[{"symbol":"AAPL.US"}]}"#;
    let server = FakeServer::start(move |_| Reply::data(partial)).await;
    let (f, plan) = planned().await;
    let constituents = endpoint("index_constituents");
    let fetched = source(&server.url, OAuth::from_token(TOKEN))
        .fetch(&constituents, &key(".SPX.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &Response::Body(partial.as_bytes().to_vec())
    );
    let recorded = f
        .store
        .record_observation(
            plan.run_id,
            &key(".SPX.US"),
            constituents.id,
            at(1, 10),
            fetched.response(),
        )
        .await
        .unwrap();
    assert_eq!(recorded.status, Status::Failed(FailureClass::Permanent));
    assert_eq!(
        stored_row(&f, "index_constituents", ".SPX.US").await,
        (
            "permanent".to_owned(),
            Some("incomplete_membership".to_owned()),
            Some("count_mismatch".to_owned())
        )
    );
}

/// Token storage held in memory. A save would mean a refresh succeeded,
/// which no test allows.
struct Memory(StoredToken);

impl TokenStorage for Memory {
    fn load(&self, _client_id: &str) -> Option<StoredToken> {
        Some(self.0.clone())
    }

    fn save(&self, _token: &StoredToken) -> OAuthResult<()> {
        panic!("no token may be saved");
    }
}

/// An SDK OAuth failure: the token comes within the SDK's five-minute
/// refresh margin after the client was built, and there is no refresh
/// token. The SDK fails the refresh with `NoRefreshToken` before building
/// any request to the OAuth host (`oauth/src/client.rs:181-185`).
///
/// Hermetic even when the build itself is slow: a build that finds the
/// token already expiring falls back to the browser flow, which first binds
/// the callback port (`client.rs:125-131`); that port is held here, so the
/// build fails instead of reaching any host.
#[tokio::test]
async fn a_failed_token_refresh_is_auth_unavailable() {
    let server = FakeServer::start(|_| Reply::data("{}")).await;
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token = StoredToken {
        client_id: "test-client".into(),
        access_token: TOKEN.into(),
        refresh_token: None,
        expires_at: now + 302,
    };
    let oauth = OAuthBuilder::new("test-client")
        .callback_port(occupied.local_addr().unwrap().port())
        .token_storage(Memory(token))
        .build(|_| panic!("no browser flow"))
        .await
        .expect("the token is valid for more than five minutes at build");
    tokio::time::sleep(Duration::from_secs(4)).await;

    let (f, plan) = planned().await;
    let ratings = endpoint("ratings");
    let fetched = source(&server.url, oauth)
        .fetch(&ratings, &key("AAPL.US"))
        .await;
    assert_eq!(
        fetched.response(),
        &failed(FailureClass::AuthUnavailable, ErrorClass::Auth, "oauth")
    );
    assert!(server.seen().is_empty());
    assert_stored(&f, &plan, &ratings, at(1, 10), &fetched).await;
}
