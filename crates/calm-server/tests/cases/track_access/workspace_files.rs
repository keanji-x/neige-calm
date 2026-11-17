//! Workspace bytes obey the same admission and publication rules as Track detail.
use super::*;
use std::sync::Arc;
use std::time::Duration;

const PRIVATE_FILE: &str = "private workspace file contents";
const FILES: [(&str, &str); 3] = [
    ("readfile", "evidence.log"),
    ("readfile-raw", "figure.png"),
    ("readfile-raw", "page.html"),
];

async fn fixture() -> (tempfile::TempDir, Fixture) {
    let workspace = tempfile::tempdir().unwrap();
    for (_, path) in FILES {
        std::fs::write(workspace.path().join(path), PRIVATE_FILE).unwrap();
    }
    let fixture = Fixture::in_workspace(workspace.path()).await;
    (workspace, fixture)
}

async fn get_file(
    app: &axum::Router,
    track: &str,
    endpoint: &str,
    path: &str,
    cookie: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut request = Request::get(format!(
        "/api/tracks/{track}/workspace/{endpoint}?path={path}"
    ));
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, body.to_vec())
}

fn assert_rejected(status: StatusCode, body: &[u8], expected: StatusCode) {
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(body));
    assert!(!String::from_utf8_lossy(body).contains(PRIVATE_FILE));
    let error: Value = serde_json::from_slice(body).unwrap();
    let code = match expected {
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::NOT_FOUND => "not_found",
        _ => panic!("unexpected rejection {expected}"),
    };
    assert_eq!(error["code"], code);
}

#[tokio::test]
async fn workspace_files_require_membership_before_io() {
    let (_workspace, f) = fixture().await;
    for (endpoint, existing) in FILES {
        for path in [existing, "absent/does-not-exist"] {
            let (status, body) =
                get_file(&f.app, &f.track_id, endpoint, path, Some(&f.alice_cookie)).await;
            assert_rejected(status, &body, StatusCode::FORBIDDEN);
        }
    }
}

#[tokio::test]
async fn workspace_files_allow_members_and_require_a_session() {
    let (_workspace, f) = fixture().await;
    f.add_alice().await;
    for (endpoint, path) in FILES {
        for cookie in [&f.owner_cookie, &f.alice_cookie] {
            let (status, body) = get_file(&f.app, &f.track_id, endpoint, path, Some(cookie)).await;
            assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
            if endpoint == "readfile" {
                let file: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(file["text"], PRIVATE_FILE);
            } else {
                assert_eq!(body, PRIVATE_FILE.as_bytes());
            }
        }
        let (status, body) = get_file(&f.app, &f.track_id, endpoint, path, None).await;
        assert_rejected(status, &body, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn workspace_files_reject_unregistered_users() {
    let (_workspace, f) = fixture().await;
    let token = f.auth.sessions.create(
        SessionIdentity {
            user_id: "unregistered".into(),
            display_name: "Unregistered user".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let cookie = format!("{SESSION_COOKIE}={token}");
    for (endpoint, path) in FILES {
        for track in [f.track_id.as_str(), "missing-track"] {
            let (status, body) = get_file(&f.app, track, endpoint, path, Some(&cookie)).await;
            assert_rejected(status, &body, StatusCode::FORBIDDEN);
        }
    }
}

async fn park_file(
    f: &Fixture,
    endpoint: &'static str,
    path: &'static str,
) -> (
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<(StatusCode, Vec<u8>)>,
) {
    use calm_server::routes::tracks::{
        TrackDetailReadHook, install_track_detail_read_hook_for_test,
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    install_track_detail_read_hook_for_test(
        &f.track_id,
        TrackDetailReadHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let (app, track, cookie) = (f.app.clone(), f.track_id.clone(), f.alice_cookie.clone());
    let mut task =
        tokio::spawn(async move { get_file(&app, &track, endpoint, path, Some(&cookie)).await });
    tokio::select! {
        _ = entered.notified() => {}
        result = &mut task => panic!("file response bypassed publication fence: {:?}", result.unwrap().0),
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("file publication did not reach the fence"),
    }
    (release, task)
}

async fn finish_file(
    release: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<(StatusCode, Vec<u8>)>,
    expected: StatusCode,
) {
    release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert_rejected(status, &body, expected);
}

#[tokio::test]
async fn workspace_files_recheck_membership_before_publication() {
    let (_workspace, f) = fixture().await;
    for (endpoint, path) in FILES {
        f.add_alice().await;
        let (release, task) = park_file(&f, endpoint, path).await;
        f.revoke_alice().await;
        finish_file(release, task, StatusCode::FORBIDDEN).await;
    }
}

#[tokio::test]
async fn workspace_files_recheck_area_before_publication() {
    for (endpoint, path) in FILES {
        let (_workspace, f) = fixture().await;
        f.add_alice().await;
        let (release, task) = park_file(&f, endpoint, path).await;
        f.state
            .raw_repo()
            .sqlite_db()
            .unwrap()
            .writer()
            .execute(
                sqlx::query("UPDATE tracks SET area_id=?2 WHERE id=?1")
                    .bind(&f.track_id)
                    .bind(&f.alice_area_id),
            )
            .await
            .unwrap();
        finish_file(release, task, StatusCode::FORBIDDEN).await;
    }
}

#[tokio::test]
async fn workspace_files_recheck_existence_before_publication() {
    for (endpoint, path) in FILES {
        let (_workspace, f) = fixture().await;
        f.add_alice().await;
        let (release, task) = park_file(&f, endpoint, path).await;
        f.state.raw_repo().track_delete(&f.track_id).await.unwrap();
        finish_file(release, task, StatusCode::NOT_FOUND).await;
    }
}
