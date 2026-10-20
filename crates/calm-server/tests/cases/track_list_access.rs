//! Lists exercise the production application router and trusted session identity.
use super::auth::{app, fresh_state, live_auth_state};
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use calm_server::auth::{OWNER_USER_ID, SESSION_COOKIE, SessionAuthority, SessionIdentity};
use calm_server::db::sqlite::{area_membership, begin_immediate_tx};
use calm_server::model::{NewArea, NewTrack};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    app: axum::Router,
    state: calm_server::state::AppState,
    pool: sqlx::SqlitePool,
    auth: calm_server::auth::AuthState,
    area: String,
    other: String,
    track: String,
    owner: String,
    alice: String,
}
impl Fixture {
    async fn new() -> Self {
        let state = fresh_state().await;
        let repo = state.raw_repo();
        let db = repo.sqlite_db().unwrap();
        let mut tx = begin_immediate_tx(&db).await.unwrap();
        area_membership::application_user_register(&mut tx, "alice", "Owner")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut areas = Vec::new();
        for user in [OWNER_USER_ID, "alice"] {
            areas.push(
                repo.area_create(
                    user,
                    NewArea {
                        name: user.into(),
                        color: "#123456".into(),
                        sort: None,
                    },
                )
                .await
                .unwrap()
                .id
                .to_string(),
            );
        }
        let auth = live_auth_state("owner", "fixture-password");
        let owner = cookie(&auth, OWNER_USER_ID);
        let alice = cookie(&auth, "alice");
        let mut f = Self {
            app: app(state.clone(), auth.clone()),
            state,
            pool: db.read_pool().clone(),
            auth,
            area: areas[0].clone(),
            other: areas[1].clone(),
            track: String::new(),
            owner,
            alice,
        };
        f.track = f.track_in(&f.area).await;
        f
    }
    async fn track_in(&self, area: &str) -> String {
        self.state
            .raw_repo()
            .track_create(NewTrack {
                area_id: area.into(),
                title: "private list payload".into(),
                sort: None,
                cwd: "/fixture/list".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap()
            .id
            .to_string()
    }
    fn paths(&self) -> Vec<String> {
        vec![
            format!("/api/areas/{}/tracks", self.area),
            format!("/api/tracks?area_id={}", self.area),
        ]
    }
    async fn member(&self, add: bool) {
        let sql = if add {
            "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES (?1,'alice','member',0)"
        } else {
            "DELETE FROM area_members WHERE area_id=?1 AND user_id='alice'"
        };
        sqlx::query(sql)
            .bind(&self.area)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}
fn cookie(auth: &calm_server::auth::AuthState, user: &str) -> String {
    let token = auth.sessions.create(
        SessionIdentity {
            user_id: user.into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    format!("{SESSION_COOKIE}={token}")
}
async fn get(app: &axum::Router, path: &str, cookie: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::get(path)
        .header("x-calm-actor", "user")
        .header("x-calm-user-id", OWNER_USER_ID);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn authenticated_scope_filters_global_and_denies_specified_area() {
    let f = Fixture::new().await;
    for path in f.paths() {
        assert_eq!(get(&f.app, &path, Some(&f.owner)).await.0, StatusCode::OK);
        assert_eq!(
            get(&f.app, &path, Some(&f.alice)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, body) = get(&f.app, "/api/tracks", Some(&f.alice)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn membership_includes_future_tracks_without_owner_bypass() {
    let f = Fixture::new().await;
    f.member(true).await;
    let future = f.track_in(&f.area).await;
    for path in f.paths().into_iter().chain(["/api/tracks".into()]) {
        let (status, body) = get(&f.app, &path, Some(&f.alice)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == future)
        );
    }
    let other_track = f.track_in(&f.other).await;
    let (_, body) = get(&f.app, "/api/tracks", Some(&f.owner)).await;
    assert!(
        !body
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == other_track)
    );
    assert_eq!(
        get(
            &f.app,
            &format!("/api/areas/{}/tracks", f.other),
            Some(&f.owner)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn session_registration_and_missing_scope() {
    let f = Fixture::new().await;
    let unknown = cookie(&f.auth, "unregistered");
    for path in f.paths().into_iter().chain([
        "/api/tracks".into(),
        "/api/areas/missing/tracks".into(),
        "/api/tracks?area_id=missing".into(),
    ]) {
        assert_eq!(get(&f.app, &path, None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            get(&f.app, &path, Some("calm-session=unknown")).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&f.app, &path, Some(&unknown)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for path in ["/api/areas/missing/tracks", "/api/tracks?area_id=missing"] {
        assert_eq!(
            get(&f.app, path, Some(&f.alice)).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn invisible_malformed_track_is_not_decoded() {
    let f = Fixture::new().await;
    sqlx::query("UPDATE tracks SET template_input='malformed-private{' WHERE id=?1")
        .bind(&f.track)
        .execute(&f.pool)
        .await
        .unwrap();
    let (status, body) = get(&f.app, "/api/tracks", Some(&f.alice)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, serde_json::json!([]));
    for path in f.paths() {
        assert_eq!(
            get(&f.app, &path, Some(&f.alice)).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            get(&f.app, &path, Some(&f.owner)).await.0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
    assert_eq!(
        get(&f.app, "/api/tracks", Some(&f.owner)).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn system_scope_requires_deployment_owner() {
    let f = Fixture::new().await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::post("/api/areas/system")
                .header(header::COOKIE, &f.owner)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let area: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = area["id"].as_str().unwrap();
    let track = f.track_in(id).await;
    for path in [
        format!("/api/areas/{id}/tracks"),
        format!("/api/tracks?area_id={id}"),
    ] {
        assert_eq!(get(&f.app, &path, Some(&f.owner)).await.0, StatusCode::OK);
        assert_eq!(
            get(&f.app, &path, Some(&f.alice)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for (c, visible) in [(&f.owner, true), (&f.alice, false)] {
        let (status, body) = get(&f.app, "/api/tracks", Some(c)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == track),
            visible
        );
    }
}

#[tokio::test]
async fn global_window_order_and_area_sort_remain_distinct() {
    let f = Fixture::new().await;
    f.member(true).await;
    let b = f.track_in(&f.other).await;
    let c = f.track_in(&f.area).await;
    let d = f.track_in(&f.area).await;
    for (id, created, closed, sort) in [
        (&f.track, 1_i64, Some(2_i64), 40_i64),
        (&b, 5, None, 30),
        (&c, 5, None, 20),
        (&d, 10, Some(12), 10),
    ] {
        sqlx::query("UPDATE tracks SET created_at=?2, closed_at=?3, sort=?4 WHERE id=?1")
            .bind(id)
            .bind(created)
            .bind(closed)
            .bind(sort)
            .execute(&f.pool)
            .await
            .unwrap();
    }
    let (status, body) = get(&f.app, "/api/tracks?since=4&until=8", Some(&f.alice)).await;
    assert_eq!(status, StatusCode::OK);
    let mut expected = vec![b.as_str(), c.as_str()];
    expected.sort();
    let ids: Vec<_> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, expected);
    let (_, body) = get(&f.app, &f.paths()[0], Some(&f.alice)).await;
    let ids: Vec<_> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![d.as_str(), c.as_str(), f.track.as_str()]);
    assert_eq!(
        get(&f.app, "/api/tracks?since=8&until=4", Some(&f.alice))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

async fn park(
    f: &Fixture,
    path: String,
) -> (
    std::sync::Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<(StatusCode, Value)>,
) {
    use calm_server::routes::tracks::{TrackDetailReadHook, install_track_list_read_hook_for_test};
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    install_track_list_read_hook_for_test(
        &f.area,
        TrackDetailReadHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let app = f.app.clone();
    let cookie = f.alice.clone();
    let task = tokio::spawn(async move { get(&app, &path, Some(&cookie)).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .expect("list assembled without holding a read transaction");
    (release, task)
}
async fn finish(
    parked: (
        std::sync::Arc<tokio::sync::Notify>,
        tokio::task::JoinHandle<(StatusCode, Value)>,
    ),
    expected: StatusCode,
) {
    parked.0.notify_one();
    let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(10), parked.1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, expected, "{body}");
    assert!(!body.to_string().contains("private list payload"));
}
fn path(f: &Fixture, kind: usize) -> String {
    match kind {
        0 => f.paths()[0].clone(),
        1 => f.paths()[1].clone(),
        _ => "/api/tracks".into(),
    }
}
#[tokio::test]
async fn publication_rechecks_revoked_membership() {
    for kind in 0..3 {
        let f = Fixture::new().await;
        f.member(true).await;
        let parked = park(&f, path(&f, kind)).await;
        f.member(false).await;
        finish(parked, StatusCode::FORBIDDEN).await;
    }
}
#[tokio::test]
async fn publication_rechecks_moved_track_even_between_authorized_areas() {
    for kind in 0..3 {
        let f = Fixture::new().await;
        f.member(true).await;
        let parked = park(&f, path(&f, kind)).await;
        sqlx::query("UPDATE tracks SET area_id=?2 WHERE id=?1")
            .bind(&f.track)
            .bind(&f.other)
            .execute(&f.pool)
            .await
            .unwrap();
        finish(parked, StatusCode::FORBIDDEN).await;
    }
}
#[tokio::test]
async fn publication_rechecks_deleted_track() {
    for kind in 0..3 {
        let f = Fixture::new().await;
        f.member(true).await;
        let parked = park(&f, path(&f, kind)).await;
        f.state.raw_repo().track_delete(&f.track).await.unwrap();
        finish(parked, StatusCode::NOT_FOUND).await;
    }
}
#[tokio::test]
async fn empty_publication_rechecks_original_scope() {
    for kind in 0..3 {
        let f = Fixture::new().await;
        f.member(true).await;
        let p = match kind {
            0 => {
                f.state.raw_repo().track_delete(&f.track).await.unwrap();
                path(&f, kind)
            }
            1 => format!("{}&until=0", path(&f, kind)),
            _ => "/api/tracks?until=0".into(),
        };
        let parked = park(&f, p).await;
        f.member(false).await;
        finish(parked, StatusCode::FORBIDDEN).await;
    }
}
