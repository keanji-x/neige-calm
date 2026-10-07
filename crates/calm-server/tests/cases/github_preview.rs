//! GitHub summaries through the real owner-authenticated router and bounded CLI read.
use super::auth::{app, dev_auth_state, fresh_state, live_auth_state};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn github_preview_requires_session_and_rejects_invalid_references() {
    let uri = "/api/github/preview?owner=o&repo=r&kind=pull&number=0";
    let state = fresh_state().await;
    let protected = app(state, live_auth_state("owner", "password"));
    let response = protected
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let state = fresh_state().await;
    let admitted = app(state, dev_auth_state());
    for uri in [
        uri,
        "/api/github/preview?owner=o%2Fr&repo=r&kind=pull&number=1",
        "/api/github/preview?owner=o&repo=r&kind=other&number=1",
    ] {
        let response = admitted
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json",
            "{uri}"
        );
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(value["error"].is_string(), "{uri}");
        assert!(value["code"].is_string(), "{uri}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn github_preview_production_route_reads_issue_and_pr_with_fixed_gh_arguments() {
    use crate::support::forge_env::EnvGuard;
    use std::os::unix::fs::PermissionsExt;
    let _guard = crate::support::forge_env::FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("gh");
    // Transport fixture only: production owns parsing, environment, command, and Issue → PR resolution.
    std::fs::write(&executable, r##"#!/bin/sh
[ "$1" = api ] && [ "$2" = --hostname ] && [ "$3" = github.com ] && [ "$4" = --method ] && [ "$5" = GET ] || exit 2
[ -z "$GH_HOST" ] && [ -z "$CARGO_MANIFEST_DIR" ] || exit 3
case "$6" in
  repos/o/r/issues/1) printf '%s' '{"number":1,"title":"Issue title","state":"open","user":{"login":"author"},"labels":[{"name":"bug"}],"body":"Issue excerpt"}' ;;
  repos/o/r/issues/2) printf '%s' '{"number":2,"title":"PR alias","state":"closed","user":{"login":"author"},"labels":[],"body":null,"pull_request":{}}' ;;
  repos/o/r/pulls/2) printf '%s' '{"number":2,"title":"Merged PR","state":"closed","user":{"login":"author"},"labels":[],"body":"PR excerpt","draft":false,"merged":true,"additions":10,"deletions":2,"changed_files":1}' ;;
  *) echo 'credential-sensitive upstream details' >&2; exit 1 ;;
esac
"##).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _path = EnvGuard::set("PATH", dir.path());
    let _host = EnvGuard::set("GH_HOST", "attacker.invalid");
    let admitted = app(fresh_state().await, dev_auth_state());
    for (kind, number, title, state) in [
        ("issue", 1, "Issue title", "open"),
        ("pull", 2, "Merged PR", "merged"),
        ("issue", 2, "Merged PR", "merged"),
    ] {
        let uri = format!("/api/github/preview?owner=o&repo=r&kind={kind}&number={number}");
        let response = admitted
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["title"], title);
        assert_eq!(value["state"], state);
        if number == 2 {
            assert_eq!(value["kind"], "pull");
            assert_eq!(value["changes"]["additions"], 10);
        }
    }
    let response = admitted
        .oneshot(
            Request::builder()
                .uri("/api/github/preview?owner=o&repo=r&kind=issue&number=3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&body).contains("credential-sensitive"));
}
