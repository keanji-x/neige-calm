//! Track detail authority through the real session-authenticated application router.
use super::auth::{app, dev_auth_state, fresh_state, live_auth_state};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{OWNER_USER_ID, SESSION_COOKIE, SessionAuthority, SessionIdentity};
use calm_server::db::sqlite::{area_membership, begin_immediate_tx};
use calm_server::model::{NewArea, NewCard, NewOverlay, NewTrack};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const PRIVATE_CARD: &str = "private Track card body";
const PRIVATE_OVERLAY: &str = "private Track overlay body";

struct Fixture {
    app: axum::Router,
    state: calm_server::state::AppState,
    auth: calm_server::auth::AuthState,
    pool: sqlx::SqlitePool,
    area_id: String,
    alice_area_id: String,
    track_id: String,
    owner_cookie: String,
    alice_cookie: String,
}

impl Fixture {
    async fn new() -> Self {
        let state = fresh_state().await;
        let repo = state.raw_repo();
        let pool = repo.sqlite_pool().unwrap();
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        area_membership::application_user_register(&mut tx, "alice", "Owner")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let area = repo
            .area_create(
                OWNER_USER_ID,
                NewArea {
                    name: "Private Track Area".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        // Alice owns another Area. Area ownership is not installation authority
        // or membership of this Track's actual parent.
        let alice_area = repo
            .area_create(
                "alice",
                NewArea {
                    name: "Alice's Area".into(),
                    color: "#654321".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id.clone(),
                title: "Private Track".into(),
                sort: None,
                cwd: "/fixture/private-track".into(),
                template_id: None,
                plugin_scope: None,
                template_input: Some(json!({"private": "template input"})),
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        repo.card_create(NewCard {
            track_id: track.id.clone(),
            kind: "note".into(),
            sort: None,
            payload: json!({"text": PRIVATE_CARD}),
            title: Some("Private note".into()),
        })
        .await
        .unwrap();
        repo.overlay_upsert(NewOverlay {
            plugin_id: "fixture".into(),
            entity_kind: "track".into(),
            entity_id: track.id.to_string(),
            kind: "badge".into(),
            payload: json!({"text": PRIVATE_OVERLAY}),
        })
        .await
        .unwrap();
        let auth = live_auth_state("owner", "fixture-password");
        let owner = auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin,
        );
        let alice = auth.sessions.create(
            SessionIdentity {
                user_id: "alice".into(),
                display_name: "Owner".into(),
                role: "owner".into(),
            },
            SessionAuthority::PasswordLogin,
        );
        Self {
            app: app(state.clone(), auth.clone()),
            state,
            auth,
            pool,
            area_id: area.id.to_string(),
            alice_area_id: alice_area.id.to_string(),
            track_id: track.id.to_string(),
            owner_cookie: format!("{SESSION_COOKIE}={owner}"),
            alice_cookie: format!("{SESSION_COOKIE}={alice}"),
        }
    }

    async fn get(&self, cookie: &str) -> (StatusCode, Value) {
        get_detail(&self.app, &self.track_id, Some(cookie)).await
    }

    async fn add_alice(&self) {
        sqlx::query(
            "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES (?1,'alice','member',0)",
        )
        .bind(&self.area_id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn revoke_alice(&self) {
        sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id='alice'")
            .bind(&self.area_id)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn track_in(&self, area_id: &str) -> String {
        self.state
            .raw_repo()
            .track_create(NewTrack {
                area_id: area_id.into(),
                title: "Another Track".into(),
                sort: None,
                cwd: "/fixture/another-track".into(),
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
}

async fn get_detail(
    app: &axum::Router,
    track_id: &str,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::get(format!("/api/tracks/{track_id}"))
        .header("x-calm-actor", "user")
        .header("x-calm-user-id", OWNER_USER_ID);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn assert_no_private_payload(body: &Value) {
    let text = body.to_string();
    assert!(!text.contains(PRIVATE_CARD), "{body}");
    assert!(!text.contains(PRIVATE_OVERLAY), "{body}");
}

#[tokio::test]
async fn track_detail_nonmember_cannot_read_private_cards_or_overlays() {
    let fixture = Fixture::new().await;
    let (status, owner_detail) = fixture.get(&fixture.owner_cookie).await;
    assert_eq!(status, StatusCode::OK, "{owner_detail}");
    assert_eq!(owner_detail["cards"][0]["payload"]["text"], PRIVATE_CARD);
    assert_eq!(
        owner_detail["overlays"][0]["payload"]["text"],
        PRIVATE_OVERLAY
    );

    let (status, denied) = fixture.get(&fixture.alice_cookie).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a registered user with another Area and owner-looking labels is not this Area's member"
    );
    let body = denied.to_string();
    assert!(!body.contains(PRIVATE_CARD));
    assert!(!body.contains(PRIVATE_OVERLAY));
}

#[tokio::test]
async fn track_detail_nonmember_is_denied_before_private_template_input_decode() {
    let fixture = Fixture::new().await;
    sqlx::query("UPDATE tracks SET template_input=?2 WHERE id=?1")
        .bind(&fixture.track_id)
        .bind("private-malformed-template-input{")
        .execute(&fixture.pool)
        .await
        .unwrap();

    let (status, denied) = fixture.get(&fixture.alice_cookie).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "resolve actual Track-to-Area scope without decoding private Track metadata"
    );
    assert_eq!(denied["code"], "forbidden");
    assert!(
        !denied
            .to_string()
            .contains("private-malformed-template-input")
    );
    // Authorized reading retains the existing data-integrity failure. Denial
    // must not be implemented by suppressing decoding errors or repairing data.
    let (status, _) = fixture.get(&fixture.owner_cookie).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn track_detail_uses_actual_membership_and_deployment_owner_has_no_user_area_bypass() {
    let f = Fixture::new().await;
    f.add_alice().await;
    let (status, detail) = f.get(&f.alice_cookie).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["cards"][0]["payload"]["text"], PRIVATE_CARD);
    assert_eq!(detail["overlays"][0]["payload"]["text"], PRIVATE_OVERLAY);
    f.revoke_alice().await;
    let (status, detail) = f.get(&f.alice_cookie).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{detail}");
    assert_no_private_payload(&detail);

    let alice_track = f.track_in(&f.alice_area_id).await;
    assert_eq!(
        get_detail(&f.app, &alice_track, Some(&f.alice_cookie))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        get_detail(&f.app, &alice_track, Some(&f.owner_cookie))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id=?2")
        .bind(&f.area_id)
        .bind(OWNER_USER_ID)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(f.get(&f.owner_cookie).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn track_detail_preserves_system_owner_and_dev_access() {
    let f = Fixture::new().await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::post("/api/areas/system")
                .header(header::COOKIE, &f.owner_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let track = f.track_in(body["id"].as_str().unwrap()).await;
    assert_eq!(
        get_detail(&f.app, &track, Some(&f.owner_cookie)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        get_detail(&f.app, &track, Some(&f.alice_cookie)).await.0,
        StatusCode::FORBIDDEN
    );
    let dev = app(f.state.clone(), dev_auth_state());
    assert_eq!(get_detail(&dev, &f.track_id, None).await.0, StatusCode::OK);
    assert_eq!(get_detail(&dev, &track, None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn track_detail_requires_session_and_registration_before_missing_scope() {
    let f = Fixture::new().await;
    assert_eq!(
        get_detail(&f.app, &f.track_id, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get_detail(&f.app, &f.track_id, Some("calm-session=unknown"))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get_detail(&f.app, "missing", Some(&f.alice_cookie)).await.0,
        StatusCode::NOT_FOUND
    );
    let unknown = f.auth.sessions.create(
        SessionIdentity {
            user_id: "unregistered".into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let cookie = format!("{SESSION_COOKIE}={unknown}");
    assert_eq!(f.get(&cookie).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        get_detail(&f.app, "missing", Some(&cookie)).await.0,
        StatusCode::FORBIDDEN
    );
    f.auth
        .sessions
        .remove(
            f.owner_cookie
                .strip_prefix(&format!("{SESSION_COOKIE}="))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(f.get(&f.owner_cookie).await.0, StatusCode::UNAUTHORIZED);
}

async fn park_detail(
    f: &Fixture,
) -> (
    std::sync::Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<(StatusCode, Value)>,
) {
    use calm_server::routes::tracks::{
        TrackDetailReadHook, install_track_detail_read_hook_for_test,
    };
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    install_track_detail_read_hook_for_test(
        &f.track_id,
        TrackDetailReadHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let app = f.app.clone();
    let track = f.track_id.clone();
    let cookie = f.alice_cookie.clone();
    let task = tokio::spawn(async move { get_detail(&app, &track, Some(&cookie)).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .expect("detail assembled outside a transaction");
    (release, task)
}

async fn finish_detail(
    release: std::sync::Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<(StatusCode, Value)>,
) -> (StatusCode, Value) {
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn track_detail_rechecks_membership_after_response_assembly() {
    let f = Fixture::new().await;
    f.add_alice().await;
    let (release, task) = park_detail(&f).await;
    f.revoke_alice().await;
    let (status, body) = finish_detail(release, task).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_no_private_payload(&body);
}

#[tokio::test]
async fn track_detail_rechecks_existence_after_response_assembly() {
    let f = Fixture::new().await;
    f.add_alice().await;
    let (release, task) = park_detail(&f).await;
    f.state.raw_repo().track_delete(&f.track_id).await.unwrap();
    let (status, body) = finish_detail(release, task).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_no_private_payload(&body);
}

#[tokio::test]
async fn track_detail_rechecks_expected_area_even_when_both_areas_are_accessible() {
    let f = Fixture::new().await;
    f.add_alice().await;
    let (release, task) = park_detail(&f).await;
    sqlx::query("UPDATE tracks SET area_id=?2 WHERE id=?1")
        .bind(&f.track_id)
        .bind(&f.alice_area_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let (status, body) = finish_detail(release, task).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_no_private_payload(&body);
    assert_eq!(f.get(&f.alice_cookie).await.0, StatusCode::OK);
}
