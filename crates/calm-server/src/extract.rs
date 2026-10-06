//! axum's `Json`, `Path` and `Query`, wrapped. `JsonBody<T>`, `Path<T>` and `Query<T>` are the
//! request extractors handlers take: each deserializes exactly as axum's own does, but a rejection
//! answers the `ErrorBody` contract (`{error, code}`) with the status axum gives it, instead of
//! axum's plain-text body. `Json<T>` is the JSON response body: it answers exactly as axum's does
//! and is not an extractor, so no handler argument can take it.
//!
//! This is the one file that may name axum's three. `tests/cases/request_rejections.rs` proves on
//! every documented route that a rejection answers an `ErrorBody`; `tests/cases/extractor_scan.rs`
//! catches the spellings a contributor would naturally write elsewhere in the crate's source; and
//! `crates/calm-server/clippy.toml` makes them disallowed types outside the impls here.
//!
//! `Path` and `Query` keep axum's names on purpose: utoipa's axum integration recognises those
//! extractors by name, and infers a handler's documented parameters from them.

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::CalmError;

/// A JSON request body; `JsonBody(value): JsonBody<T>` in a handler's arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonBody<T>(pub T);

// The one place axum's `Json` is named as a request extractor: `clippy.toml` disallows it elsewhere.
#[allow(clippy::disallowed_types)]
impl<T, S> FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = CalmError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(JsonBody(value)),
            Err(rejection) => Err(rejection_error(rejection.status(), rejection.body_text())),
        }
    }
}

/// A JSON response body; `Json(value)` from a handler. Serialized and answered exactly as
/// `axum::Json` answers, with `Content-Type: application/json`. It implements no `FromRequest`, so
/// it cannot extract a request body: that is [`JsonBody`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

#[allow(clippy::disallowed_types)]
impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Every status a [`JsonBody`] rejection answers, with its OpenAPI description. The document adds
/// each one to every operation that declares a JSON request body (`openapi::DeclaredResponses`),
/// so a route annotation names a JSON body once and these follow.
pub(crate) const JSON_BODY_REJECTIONS: [(StatusCode, &str); 4] = [
    (
        StatusCode::BAD_REQUEST,
        "`bad_request`: the body is not parseable JSON.",
    ),
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        "`payload_too_large`: the body is over the request size limit.",
    ),
    (
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "`unsupported_media_type`: the request does not say `Content-Type: application/json`.",
    ),
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        "`invalid_body`: the JSON is not the body's shape (a field missing or of the wrong type).",
    ),
];

/// The status a [`Path`] or [`Query`] rejection answers for a value that does not parse, with its
/// OpenAPI description. The document adds it to every operation that declares a path or query
/// parameter (`openapi::DeclaredResponses`). Their other rejection, a 500 for a route whose
/// parameters do not fit its handler, is a server fault and is not documented.
pub(crate) const PARAM_REJECTION: (StatusCode, &str) = (
    StatusCode::BAD_REQUEST,
    "`bad_request`: a path or query parameter does not parse.",
);

/// The path parameters; `Path(id): Path<String>` in a handler's arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct Path<T>(pub T);

// The one place axum's `Path` may be named: `clippy.toml` disallows it everywhere else.
#[allow(clippy::disallowed_types)]
impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = CalmError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(path) => Ok(Path(path.0)),
            Err(rejection) => Err(rejection_error(rejection.status(), rejection.body_text())),
        }
    }
}

/// The query string; `Query(params): Query<T>` in a handler's arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

// The one place axum's `Query` may be named: `clippy.toml` disallows it everywhere else.
#[allow(clippy::disallowed_types)]
impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = CalmError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(query) => Ok(Query(query.0)),
            Err(rejection) => Err(rejection_error(rejection.status(), rejection.body_text())),
        }
    }
}

/// A rejection keeps axum's status, so only the body changes shape. A JSON body: 400 unparseable
/// JSON (`bad_request`), 413 over the body limit (`payload_too_large`), 415 no JSON content type
/// (`unsupported_media_type`), 422 JSON of the wrong shape (`invalid_body`). Path parameters and the
/// query string: 400 a value that does not parse (`bad_request`), or 500 (`internal`) for a route
/// whose parameters do not fit its handler, a server fault. The reason is axum's own text, which
/// names the field serde stopped at.
fn rejection_error(status: StatusCode, reason: String) -> CalmError {
    match status {
        StatusCode::BAD_REQUEST => CalmError::BadRequest(reason),
        StatusCode::PAYLOAD_TOO_LARGE => CalmError::PayloadTooLarge(reason),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => CalmError::UnsupportedMediaType(reason),
        StatusCode::UNPROCESSABLE_ENTITY => CalmError::InvalidBody(reason),
        StatusCode::INTERNAL_SERVER_ERROR => CalmError::Internal(reason),
        // The rejection types are non-exhaustive; every rejection axum 0.8 has is one of the above.
        other => CalmError::Internal(format!("unexpected request rejection ({other}): {reason}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::DefaultBodyLimit;
    use axum::http::header::CONTENT_TYPE;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use serde::Deserialize;
    use tower::ServiceExt;

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Body1 {
        theme: String,
    }

    async fn handler(JsonBody(_): JsonBody<Body1>) -> StatusCode {
        StatusCode::NO_CONTENT
    }

    async fn answer(
        content_type: Option<&str>,
        body: &'static str,
    ) -> (StatusCode, serde_json::Value) {
        let app = axum::Router::new()
            .route("/", post(handler))
            .layer(DefaultBodyLimit::max(64));
        let mut request = Request::builder().method("POST").uri("/");
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        let response = app
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap()
            .into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "rejection body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, body)
    }

    /// Each rejection axum's `Json` gives keeps its status and answers `{error, code}` and nothing else.
    #[tokio::test]
    async fn every_rejection_answers_the_error_body_with_axums_status() {
        let json = Some("application/json");
        let long = Box::leak(format!("{{\"theme\":\"{}\"}}", "x".repeat(128)).into_boxed_str());
        let cases: [(Option<&str>, &'static str, StatusCode, &str, &str); 4] = [
            (
                json,
                "{",
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Failed to parse the request body as JSON",
            ),
            (
                json,
                "{}",
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_body",
                "missing field `theme`",
            ),
            (
                None,
                "{\"theme\":\"dark\"}",
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "Content-Type: application/json",
            ),
            (
                json,
                long,
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Failed to buffer the request body",
            ),
        ];
        let mut answered: Vec<u16> = cases.iter().map(|case| case.2.as_u16()).collect();
        let mut documented: Vec<u16> = JSON_BODY_REJECTIONS
            .iter()
            .map(|(s, _)| s.as_u16())
            .collect();
        answered.sort_unstable();
        documented.sort_unstable();
        assert_eq!(
            answered, documented,
            "the documented rejections are exactly the ones answered"
        );
        for (content_type, body, status, code, needle) in cases {
            let (actual, answer) = answer(content_type, body).await;
            assert_eq!(actual, status, "{code}: {answer}");
            assert_eq!(answer["code"], code, "{answer}");
            let error = answer["error"].as_str().expect("error is a string");
            assert!(error.contains(needle), "{code}: {error}");
            assert_eq!(
                answer.as_object().unwrap().len(),
                2,
                "{code}: no field: {answer}"
            );
        }
        let (ok, _) = answer(json, "{\"theme\":\"dark\"}").await;
        assert_eq!(ok, StatusCode::NO_CONTENT);
    }
}
