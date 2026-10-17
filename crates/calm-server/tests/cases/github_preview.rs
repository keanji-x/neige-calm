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
pub(super) struct GitHubCli {
    directory: tempfile::TempDir,
    _path: crate::support::forge_env::EnvGuard,
    _host: crate::support::forge_env::EnvGuard,
    _token: crate::support::forge_env::EnvGuard,
    _github_token: crate::support::forge_env::EnvGuard,
}

#[cfg(unix)]
impl GitHubCli {
    pub(super) fn calls(&self) -> Vec<String> {
        let trace = self.directory.path().join("gh.calls");
        match std::fs::read_to_string(trace) {
            Ok(value) => value.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read GitHub transport fixture calls: {error}"),
        }
    }
}

#[cfg(unix)]
pub(super) fn github_cli() -> GitHubCli {
    use crate::support::forge_env::EnvGuard;
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("gh");
    // Transport only; production owns authorization, parsing, environment and alias resolution.
    std::fs::write(&executable, r##"#!/bin/sh
printf '%s\n' "$*" >> "$0.calls"
[ "$1" = api ] && [ "$2" = --hostname ] && [ "$3" = github.com ] && [ "$4" = --method ] && [ "$5" = GET ] || exit 2
[ -z "$GH_HOST" ] && [ -z "$CARGO_MANIFEST_DIR" ] || exit 3
[ "$GH_TOKEN" = fixture-gh-token ] && [ "$GITHUB_TOKEN" = fixture-github-token ] || exit 4
case "$6" in
  repos/o/r/issues/1) printf '%s' '{"number":1,"title":"Issue title","state":"open","user":{"login":"author"},"labels":[{"name":"bug"}],"body":"Issue excerpt"}' ;;
  repos/o/r/issues/2) printf '%s' '{"number":2,"title":"PR alias","state":"closed","user":{"login":"author"},"labels":[],"body":null,"pull_request":{}}' ;;
  repos/o/r/pulls/2) printf '%s' '{"number":2,"title":"Merged PR","state":"closed","user":{"login":"author"},"labels":[],"body":"PR excerpt","draft":false,"merged":true,"additions":10,"deletions":2,"changed_files":1}' ;;
  *) echo 'credential-sensitive upstream details' >&2; exit 1 ;;
esac
"##).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = EnvGuard::set("PATH", directory.path());
    let host = EnvGuard::set("GH_HOST", "attacker.invalid");
    let token = EnvGuard::set("GH_TOKEN", "fixture-gh-token");
    let github_token = EnvGuard::set("GITHUB_TOKEN", "fixture-github-token");
    GitHubCli {
        directory,
        _path: path,
        _host: host,
        _token: token,
        _github_token: github_token,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn github_preview_production_route_reads_issue_and_pr_with_fixed_gh_arguments() {
    let _guard = crate::support::forge_env::FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _cli = github_cli();
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

#[cfg(unix)]
#[tokio::test]
async fn github_preview_area_owner_cannot_use_operator_credentials() {
    use calm_server::auth::{SESSION_COOKIE, SessionAuthority, SessionIdentity};
    use calm_server::db::sqlite::{area_membership, begin_immediate_tx};
    use calm_server::model::NewArea;
    let _guard = crate::support::forge_env::FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cli = github_cli();
    let state = fresh_state().await;
    let repo = state.raw_repo();
    let pool = repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    area_membership::application_user_register(&mut tx, "alice", "Owner")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    repo.area_create(
        "alice",
        NewArea {
            name: "Alice's Area".into(),
            color: "#123456".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let auth = live_auth_state("owner", "fixture-password");
    let token = auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let protected = app(state, auth);
    let response = protected
        .oneshot(
            Request::get("/api/github/preview?owner=o&repo=r&kind=issue&number=1")
                .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
                .header("x-calm-actor", "user")
                .header("x-calm-user-id", calm_server::auth::OWNER_USER_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        (response.status(), cli.calls().len()),
        (StatusCode::FORBIDDEN, 0),
        "Area ownership and owner-looking labels cannot use the operator's gh identity"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn github_preview_nonowner_is_denied_before_feature_lookup() {
    use calm_server::auth::{SESSION_COOKIE, SessionAuthority, SessionIdentity};
    let _guard = crate::support::forge_env::FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cli = github_cli();
    let auth = live_auth_state("owner", "fixture-password");
    let token = auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Alice".into(),
            role: "member".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let protected = app(fresh_state().await, auth);
    let response = protected
        .oneshot(
            Request::get("/api/github/preview?owner=o&repo=r&kind=pull&number=0")
                .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        (response.status(), cli.calls().len()),
        (StatusCode::FORBIDDEN, 0)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn github_preview_password_owner_keeps_reads_and_unauthenticated_calls_do_not_run_gh() {
    use calm_server::auth::{SESSION_COOKIE, SessionAuthority, SessionIdentity};
    let _guard = crate::support::forge_env::FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let cli = github_cli();
    let auth = live_auth_state("owner", "fixture-password");
    let token = auth.sessions.create(
        SessionIdentity::owner(&auth.config),
        SessionAuthority::PasswordLogin,
    );
    let protected = app(fresh_state().await, auth);
    let response = protected
        .clone()
        .oneshot(
            Request::get("/api/github/preview?owner=o&repo=r&kind=issue&number=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        (response.status(), cli.calls().len()),
        (StatusCode::UNAUTHORIZED, 0)
    );
    for (kind, number, title) in [
        ("issue", 1, "Issue title"),
        ("pull", 2, "Merged PR"),
        ("issue", 2, "Merged PR"),
    ] {
        let response = protected
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/github/preview?owner=o&repo=r&kind={kind}&number={number}"
                ))
                .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["title"], title);
    }
    assert_eq!(
        cli.calls().len(),
        4,
        "Issue → PR alias reads remain feature-owned"
    );
}
