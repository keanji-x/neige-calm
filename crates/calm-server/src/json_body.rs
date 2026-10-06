//! `JsonBody<T>`: the one JSON request-body extractor. It deserializes exactly as `axum::Json`
//! does, but a rejection answers the `ErrorBody` contract (`{error, code}`) with the status axum
//! gives it, instead of axum's plain-text body. `tests/cases/json_body_extractor_scan.rs` keeps
//! `axum::Json` out of every handler's arguments.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use serde::de::DeserializeOwned;

use crate::error::CalmError;

/// A JSON request body; `JsonBody(value): JsonBody<T>` in a handler's arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonBody<T>(pub T);

impl<T, S> FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = CalmError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(JsonBody(value)),
            Err(rejection) => Err(rejection_error(rejection)),
        }
    }
}

/// Every status a [`JsonBody`] rejection answers, with its OpenAPI description. The document adds
/// each one to every operation that declares a JSON request body (`openapi::DeclaredResponses`),
/// so a route annotation names a JSON body once and these follow.
pub(crate) const REJECTIONS: [(StatusCode, &str); 4] = [
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

/// A rejection keeps axum's status, so only the body changes shape: 400 unparseable JSON
/// (`bad_request`), 413 over the body limit (`payload_too_large`), 415 no JSON content type
/// (`unsupported_media_type`), 422 JSON of the wrong shape (`invalid_body`). The reason is axum's
/// own text, which names the field serde stopped at.
fn rejection_error(rejection: JsonRejection) -> CalmError {
    let reason = rejection.body_text();
    match rejection.status() {
        StatusCode::BAD_REQUEST => CalmError::BadRequest(reason),
        StatusCode::PAYLOAD_TOO_LARGE => CalmError::PayloadTooLarge(reason),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => CalmError::UnsupportedMediaType(reason),
        StatusCode::UNPROCESSABLE_ENTITY => CalmError::InvalidBody(reason),
        // `JsonRejection` is non-exhaustive; every rejection axum 0.8 has is one of the four above.
        other => CalmError::Internal(format!(
            "unexpected JSON body rejection ({other}): {reason}"
        )),
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
        let mut documented: Vec<u16> = REJECTIONS.iter().map(|(s, _)| s.as_u16()).collect();
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
