//! Track detail authority through the real session-authenticated application router.
use super::auth::{app, fresh_state, live_auth_state};
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
    pool: sqlx::SqlitePool,
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
        repo.area_create(
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
                area_id: area.id,
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
            app: app(state, auth),
            pool,
            track_id: track.id.to_string(),
            owner_cookie: format!("{SESSION_COOKIE}={owner}"),
            alice_cookie: format!("{SESSION_COOKIE}={alice}"),
        }
    }

    async fn get(&self, cookie: &str) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::get(format!("/api/tracks/{}", self.track_id))
                    .header(header::COOKIE, cookie)
                    .header("x-calm-actor", "user")
                    .header("x-calm-user-id", OWNER_USER_ID)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

#[tokio::test]
async fn track_detail_nonmember_cannot_read_private_cards_or_overlays() {
    let fixture = Fixture::new().await;
    let (status, owner_detail) = fixture.get(&fixture.owner_cookie).await;
    assert_eq!(status, StatusCode::OK, "{owner_detail}");
    assert_eq!(owner_detail["cards"][0]["payload"]["text"], PRIVATE_CARD);
    assert_eq!(owner_detail["overlays"][0]["payload"]["text"], PRIVATE_OVERLAY);

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
    assert!(!denied.to_string().contains("private-malformed-template-input"));
    // Authorized reading retains the existing data-integrity failure. Denial
    // must not be implemented by suppressing decoding errors or repairing data.
    let (status, _) = fixture.get(&fixture.owner_cookie).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}
