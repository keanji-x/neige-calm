//! Real authentication and host handlers; Area ownership is not host authority.
use super::*;
use crate::auth::{AuthConfig, AuthState, SESSION_COOKIE, SessionAuthority, SessionIdentity};
use crate::db::prelude::*;
use crate::db::sqlite::{area_membership, begin_immediate_tx};
use crate::model::{NewArea, NewTrack, RequestTheme};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::process::Command as TestCommand;
use tower::ServiceExt;

fn auth(dev: bool) -> AuthState {
    AuthState::new(AuthConfig {
        username: Some("owner".into()),
        password: Some("fixture-password".into()),
        dev_autologin: dev,
        display_name: "Owner".into(),
    })
}

pub(crate) async fn app(auth: AuthState) -> (tempfile::TempDir, Router) {
    let (temp, _repo, state) = crate::daily_planner::tests::fixture().await;
    let app = super::router()
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::require_session,
        ))
        .with_state(state);
    (temp, app)
}

/// Existing direct handler/IO tests still enter with a genuinely issued identity.
pub(super) async fn owner_principal() -> Principal {
    use std::sync::{Arc, Mutex};
    async fn capture(State(output): State<Arc<Mutex<Option<Principal>>>>, principal: Principal) {
        *output.lock().unwrap() = Some(principal);
    }
    let auth = auth(false);
    let token = auth.sessions.create(
        SessionIdentity::owner(&auth.config),
        SessionAuthority::PasswordLogin,
    );
    let output = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/principal", axum::routing::get(capture))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::require_session,
        ))
        .with_state(output.clone());
    assert_eq!(
        get(&app, "/principal", Some(&token)).await.status(),
        StatusCode::OK
    );
    output.lock().unwrap().take().unwrap()
}

async fn get(app: &Router, uri: &str, token: Option<&str>) -> Response {
    let mut request = Request::get(uri)
        .header("x-calm-actor", "user")
        .header("x-calm-user-id", crate::auth::OWNER_USER_ID);
    if let Some(token) = token {
        request = request.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

struct HostFiles(tempfile::TempDir);

impl HostFiles {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        ] {
            assert!(
                TestCommand::new("git")
                    .args(["-c", "commit.gpgsign=false", "-C"])
                    .arg(temp.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(temp.path().join("note.txt"), "host fixture content").unwrap();
        std::fs::write(temp.path().join("image.png"), b"fixture image bytes").unwrap();
        Self(temp)
    }

    fn requests(&self, missing: bool) -> Vec<String> {
        let root = self.0.path();
        let path = |name: &str| {
            let path = if missing {
                root.join("absent").join(name)
            } else {
                root.join(name)
            };
            path.to_string_lossy().into_owned()
        };
        vec![
            format!("/api/fs/listdir?path={}", path(".")),
            format!("/api/fs/readfile?path={}", path("note.txt")),
            format!("/api/fs/readfile-raw?path={}", path("image.png")),
            format!("/api/fs/gitstatus?path={}", path(".")),
            format!("/api/fs/gitdiff?path={}", path("note.txt")),
        ]
    }
}

#[tokio::test]
async fn area_owner_cannot_read_host_files_or_git() {
    let files = HostFiles::new();
    let (_temp, repo, state) = crate::daily_planner::tests::fixture().await;
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    area_membership::application_user_register(&mut tx, "alice", "Owner")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let area = repo
        .area_create(
            "alice",
            NewArea {
                name: "Alice's Area".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    repo.track_create(NewTrack {
        area_id: area.id,
        title: "Alice's workspace".into(),
        sort: None,
        cwd: files.0.path().to_string_lossy().into_owned(),
        template_id: None,
        plugin_scope: None,
        template_input: None,
        attach_folder: false,
        theme: RequestTheme::default_dark(),
    })
    .await
    .unwrap();
    let auth = auth(false);
    let token = auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let app = super::router()
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::require_session,
        ))
        .with_state(state);
    for uri in files.requests(false) {
        let response = get(&app, &uri, Some(&token)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&body).contains("host fixture content"));
    }
}

#[tokio::test]
async fn nonowner_missing_host_paths_are_forbidden_before_lookup() {
    let files = HostFiles::new();
    let auth = auth(false);
    let token = auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Alice".into(),
            role: "member".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let (_state_files, app) = app(auth).await;
    for uri in files.requests(true) {
        assert_eq!(
            get(&app, &uri, Some(&token)).await.status(),
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn password_owner_and_configured_dev_keep_host_reads() {
    let files = HostFiles::new();
    for dev in [false, true] {
        let auth = auth(dev);
        let token = auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin,
        );
        let (_state_files, app) = app(auth).await;
        for uri in files.requests(false) {
            assert_eq!(
                get(&app, &uri, (!dev).then_some(token.as_str()))
                    .await
                    .status(),
                StatusCode::OK,
                "{uri}"
            );
        }
        let response = get(
            &app,
            &format!(
                "/api/fs/readfile?path={}",
                files.0.path().join("note.txt").display()
            ),
            (!dev).then_some(token.as_str()),
        )
        .await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("host fixture content"));
    }
}

#[tokio::test]
async fn host_reads_require_actual_session() {
    let files = HostFiles::new();
    let (_state_files, app) = app(auth(false)).await;
    for uri in files.requests(false) {
        assert_eq!(
            get(&app, &uri, None).await.status(),
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
    }
}
