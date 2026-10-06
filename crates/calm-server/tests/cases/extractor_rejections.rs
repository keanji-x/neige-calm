//! #2175: a request whose path parameters or query string do not deserialize is answered with the
//! `ErrorBody` contract (`{error, code}`), not axum's plain text, at the status axum gives it.
//! Every request goes through the production router assembly.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::auth::{AuthConfig, AuthState};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use super::auth::fresh_state;

async fn get(uri: &str) -> (StatusCode, Value) {
    let auth = AuthState::new(AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "Owner".into(),
    });
    let response = calm_server::routes::application_router(fresh_state().await, auth)
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "{uri}: the rejection is not JSON ({e}): {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, body)
}

fn assert_bad_request((status, body): (StatusCode, Value), needle: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "bad_request", "{body}");
    let error = body["error"].as_str().expect("error is a string");
    assert!(error.contains(needle), "{error}");
    assert_eq!(body.as_object().unwrap().len(), 2, "no field: {body}");
}

#[tokio::test]
async fn a_query_string_that_does_not_deserialize_answers_the_error_body() {
    assert_bad_request(get("/api/fs/readfile").await, "missing field `path`");
}

#[tokio::test]
async fn a_path_parameter_that_does_not_deserialize_answers_the_error_body() {
    assert_bad_request(get("/api/tracks/%FF").await, "UTF-8");
}
