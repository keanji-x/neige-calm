//! Protected production HTTP -> actual auth source -> share owner -> Truth.
use super::auth::{app, fresh_state, live_auth_state};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use calm_server::auth::{
    AuthState, OWNER_USER_ID, SESSION_COOKIE, SessionAuthority, SessionIdentity,
};
use calm_server::db::sqlite::{SqlxRepo, area_membership, share_links};
use calm_server::model::{NewArea, NewTrack};
use calm_server::state::AppState;
use calm_types::share_links::ShareScope;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

struct Fixture {
    state: AppState,
    auth: AuthState,
    app: Router,
    area: String,
    track: String,
    cookie: String,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_state(fresh_state().await).await
    }

    async fn with_state(state: AppState) -> Self {
        let repo = state.raw_repo();
        let area = repo
            .area_create(
                OWNER_USER_ID,
                NewArea {
                    name: "share owner".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id.clone(),
                title: "shared Track".into(),
                sort: None,
                cwd: "/fixture/share".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let auth = live_auth_state("owner", "password");
        let token = auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin,
        );
        Self {
            app: app(state.clone(), auth.clone()),
            state,
            auth,
            area: area.id.to_string(),
            track: track.id.to_string(),
            cookie: format!("{SESSION_COOKIE}={token}"),
        }
    }

    fn path(&self, track: bool) -> String {
        if track {
            format!("/api/tracks/{}/share-links", self.track)
        } else {
            format!("/api/areas/{}/share-links", self.area)
        }
    }

    async fn register(&self, user: &str) {
        let db = self.state.raw_repo().sqlite_db().unwrap().clone();
        db.writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await.unwrap();
                    area_membership::application_user_register(&mut tx, user, "Owner")
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                })
            })
            .await
            .unwrap();
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        request(&self.app, method, path, cookie, body).await
    }

    async fn create(&self, track: bool) -> Value {
        let (status, value) = self
            .request("POST", &self.path(track), Some(&self.cookie), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value
    }
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        // Header attribution cannot turn an anonymous/foreign issuer into owner.
        .header("X-Calm-Actor", "user")
        .header("X-Calm-Role", "owner");
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn owner_creates_lists_and_revokes_both_scopes_without_leaking_secrets() {
    let fx = Fixture::new().await;
    fx.register("unrelated").await;
    for track in [false, true] {
        let created = fx.create(track).await;
        let token = created["token"].as_str().unwrap();
        assert!(!token.is_empty());
        let delegation = &created["link"]["delegation"];
        assert_eq!(delegation["purpose"], "conversation");
        assert_eq!(delegation["resourcePolicy"], "current_deployment");
        assert_eq!(delegation["approverUserId"], OWNER_USER_ID);
        assert_eq!(
            delegation["scope"]["kind"],
            if track { "track" } else { "area" }
        );
        let id = delegation["id"].as_str().unwrap();
        let db = fx.state.raw_repo().sqlite_db().unwrap();
        let hash: String = sqlx::query_scalar("SELECT token_hash FROM share_links WHERE id=?1")
            .bind(id)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(hash, hex::encode(Sha256::digest(token.as_bytes())));
        assert_ne!(hash, token);
        let (status, listed) = fx
            .request("GET", &fx.path(track), Some(&fx.cookie), Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed, json!([created["link"]]));
        assert!(!listed.to_string().contains(token));
        assert!(!listed.to_string().contains(&hash));
        let path = format!("/api/share-links/{id}");
        let (status, revoked) = fx
            .request("DELETE", &path, Some(&fx.cookie), Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(revoked["revokedAt"].is_i64());
        assert_eq!(&revoked["delegation"], delegation);
        assert_eq!(
            fx.request("DELETE", &path, Some(&fx.cookie), Value::Null)
                .await,
            (StatusCode::OK, revoked.clone())
        );
        assert_eq!(
            fx.request("GET", &fx.path(track), Some(&fx.cookie), Value::Null)
                .await
                .1,
            json!([revoked])
        );
        let second = fx.create(track).await;
        assert_ne!(created["token"], second["token"]);
        assert_ne!(delegation["id"], second["link"]["delegation"]["id"]);
    }
}

#[tokio::test]
async fn real_issuer_registration_and_current_target_owner_are_required() {
    let fx = Fixture::new().await;
    let created = fx.create(true).await;
    let delete = format!(
        "/api/share-links/{}",
        created["link"]["delegation"]["id"].as_str().unwrap()
    );
    fx.register("alice").await;
    let alice = fx.auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let paired = fx.auth.sessions.create(
        SessionIdentity::owner(&fx.auth.config),
        SessionAuthority::PairedDevice,
    );
    for (cookie, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (
            Some(format!("{SESSION_COOKIE}={alice}")),
            StatusCode::FORBIDDEN,
        ),
        (
            Some(format!("{SESSION_COOKIE}={paired}")),
            StatusCode::FORBIDDEN,
        ),
    ] {
        for (method, path) in [
            ("POST", fx.path(true)),
            ("GET", fx.path(false)),
            ("DELETE", delete.clone()),
        ] {
            assert_eq!(
                fx.request(method, &path, cookie.as_deref(), json!({}))
                    .await
                    .0,
                expected
            );
        }
    }
    // Even a raw owner Principal extension cannot replace the real source.
    let raw = calm_server::routes::protected_router()
        .with_state(fx.state.clone())
        .layer(axum::Extension(calm_server::auth::Principal {
            user_id: OWNER_USER_ID.into(),
            display_name: "Owner".into(),
            role: "owner".into(),
            session_id: "forged".into(),
        }));
    assert_eq!(
        request(&raw, "POST", &fx.path(true), None, json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer()
        .execute(
            sqlx::query(
                "UPDATE area_members SET user_id='alice' WHERE area_id=?1 AND role='owner'",
            )
            .bind(&fx.area),
        )
        .await
        .unwrap();
    for (method, path) in [
        ("POST", fx.path(true)),
        ("GET", fx.path(false)),
        ("DELETE", delete),
    ] {
        assert_eq!(
            fx.request(method, &path, Some(&fx.cookie), json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    // Ownership of an Area still cannot give Alice deployment credentials.
    assert_eq!(
        fx.request(
            "POST",
            &fx.path(false),
            Some(&format!("{SESSION_COOKIE}={alice}")),
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn system_missing_targets_and_request_controlled_delegation_are_rejected() {
    let fx = Fixture::new().await;
    let (status, system) = fx
        .request("POST", "/api/areas/system", Some(&fx.cookie), json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let system_track = fx
        .state
        .raw_repo()
        .track_create(NewTrack {
            area_id: system["id"].as_str().unwrap().into(),
            title: "system".into(),
            sort: None,
            cwd: "/fixture/system".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    for (path, expected) in [
        (
            format!("/api/tracks/{}/share-links", system_track.id),
            StatusCode::FORBIDDEN,
        ),
        (
            format!("/api/areas/{}/share-links", system["id"].as_str().unwrap()),
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/areas/missing/share-links".into(),
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/tracks/missing/share-links".into(),
            StatusCode::NOT_FOUND,
        ),
    ] {
        for method in ["POST", "GET"] {
            assert_eq!(
                fx.request(method, &path, Some(&fx.cookie), json!({}))
                    .await
                    .0,
                expected
            );
        }
    }
    for body in [
        json!({"allows_input":false}),
        json!({"scope":{"kind":"area","id":"other"}}),
        json!({"purpose":"other"}),
        json!({"resourcePolicy":"frozen"}),
        json!({"approverUserId":"alice"}),
    ] {
        assert_eq!(
            fx.request("POST", &fx.path(true), Some(&fx.cookie), body)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        fx.request(
            "DELETE",
            "/api/share-links/missing",
            Some(&fx.cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // A revoked source cannot create merely because its old Principal was owner.
    let token = fx.cookie.split_once('=').unwrap().1;
    fx.auth.sessions.remove(token).unwrap();
    assert_eq!(
        fx.request("POST", &fx.path(true), Some(&fx.cookie), json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn unregistered_owner_cannot_delegate() {
    let fx = Fixture::new().await;
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer()
        .execute(sqlx::query("DELETE FROM area_members"))
        .await
        .unwrap();
    db.writer()
        .execute(sqlx::query("DELETE FROM application_users"))
        .await
        .unwrap();
    for track in [false, true] {
        for method in ["GET", "POST"] {
            assert_eq!(
                fx.request(method, &fx.path(track), Some(&fx.cookie), json!({}))
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
    }
}

#[tokio::test]
async fn database_fences_scope_policy_identity_and_monotonic_access() {
    let fx = Fixture::new().await;
    let created = fx.create(true).await;
    let id = created["link"]["delegation"]["id"].as_str().unwrap();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    for (track, area, purpose, policy) in [
        (None, None, "conversation", "current_deployment"),
        (
            Some(fx.track.as_str()),
            Some(fx.area.as_str()),
            "conversation",
            "current_deployment",
        ),
        (Some(fx.track.as_str()), None, "other", "current_deployment"),
        (Some(fx.track.as_str()), None, "conversation", "frozen"),
    ] {
        assert!(
            db.writer()
                .execute(
                    sqlx::query(concat!(
                        "INSERT INTO share_links(id,track_id,area_id,approver_user_id,",
                        "purpose,resource_policy,created_at,token_hash) ",
                        "VALUES('invalid',?1,?2,?3,?4,?5,0,printf('%064d',0))",
                    ))
                    .bind(track)
                    .bind(area)
                    .bind(OWNER_USER_ID)
                    .bind(purpose)
                    .bind(policy)
                )
                .await
                .is_err()
        );
    }
    for statement in [
        "UPDATE share_links SET purpose='other' WHERE id=?1",
        "UPDATE share_links SET resource_policy='frozen' WHERE id=?1",
        "UPDATE share_links SET track_id=NULL WHERE id=?1",
        "UPDATE share_links SET area_id=(SELECT id FROM areas LIMIT 1) WHERE id=?1",
        "UPDATE share_links SET approver_user_id='other' WHERE id=?1",
        "UPDATE share_links SET created_at=created_at+1 WHERE id=?1",
        "UPDATE share_links SET id='replacement' WHERE id=?1",
        "UPDATE share_links SET token_hash=printf('%064d',0) WHERE id=?1",
        "DELETE FROM share_links WHERE id=?1",
        "INSERT OR REPLACE INTO share_links SELECT * FROM share_links WHERE id=?1",
    ] {
        assert!(
            db.writer()
                .execute(sqlx::query(statement).bind(id))
                .await
                .is_err(),
            "{statement}"
        );
    }
    let revoked = fx
        .request(
            "DELETE",
            &format!("/api/share-links/{id}"),
            Some(&fx.cookie),
            Value::Null,
        )
        .await
        .1;
    for statement in [
        "UPDATE share_links SET revoked_at=NULL WHERE id=?1",
        "UPDATE share_links SET revoked_at=revoked_at+1 WHERE id=?1",
    ] {
        assert!(
            db.writer()
                .execute(sqlx::query(statement).bind(id))
                .await
                .is_err()
        );
    }
    assert_eq!(
        fx.request("GET", &fx.path(true), Some(&fx.cookie), Value::Null)
            .await
            .1,
        json!([revoked])
    );
}

#[tokio::test]
async fn target_deletion_uses_existing_lifecycle_cascades() {
    let fx = Fixture::new().await;
    let track = fx.create(true).await;
    let area = fx.create(false).await;
    let (status, body) = fx
        .request(
            "DELETE",
            &format!("/api/tracks/{}", fx.track),
            Some(&fx.cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let track_link = track["link"]["delegation"]["id"].as_str().unwrap();
    assert_eq!(
        fx.request(
            "DELETE",
            &format!("/api/share-links/{track_link}"),
            Some(&fx.cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fx.request("GET", &fx.path(false), Some(&fx.cookie), Value::Null)
            .await
            .1,
        json!([area["link"]])
    );
    let (status, body) = fx
        .request(
            "DELETE",
            &format!("/api/areas/{}", fx.area),
            Some(&fx.cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM share_links")
        .fetch_one(fx.state.raw_repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

fn with_repo(seed: &AppState, repo: Arc<SqlxRepo>) -> AppState {
    AppState::from_parts(
        repo,
        seed.events.clone(),
        seed.daemon.clone(),
        seed.plugin.clone(),
        seed.codex.clone(),
        None,
        None,
    )
}

#[tokio::test]
async fn upgrade_then_reopen_consumes_durable_delegation_and_revocation() {
    use crate::support::migration_replay::{open_sqlite, stage_db_at};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shares.db");
    let pool = open_sqlite(&path).await;
    stage_db_at(&pool, 214).await;
    // An existing unrelated application user survives upgrade and cannot block owner.
    sqlx::query("INSERT INTO application_users VALUES('unrelated','Owner',1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('old-area','old','#000',0,1,1)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('old-track','old-area','old',0,1,1)").execute(&pool).await.unwrap();
    let old_payload = r#"{ "text": "existing history", "legacy": {"pending": []} }"#;
    sqlx::query(concat!(
        "INSERT INTO cards(id,track_id,kind,role,sort,payload,created_at,updated_at) ",
        "VALUES('old-card','old-track','note','worker',0,?1,1,1)",
    ))
    .bind(old_payload)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let url = format!("sqlite://{}", path.display());
    let seed = fresh_state().await;
    let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    let fx = Fixture::with_state(with_repo(&seed, repo.clone())).await;
    let created = fx.create(true).await;
    let area = fx.create(false).await;
    let shared_track = format!("/api/share/{}", created["token"].as_str().unwrap());
    let shared_area = format!("/api/share/{}", area["token"].as_str().unwrap());
    assert_eq!(
        fx.request("GET", &shared_track, None, Value::Null).await.0,
        StatusCode::OK
    );
    let id = created["link"]["delegation"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let delete = format!("/api/share-links/{id}");
    let revoked = fx
        .request("DELETE", &delete, Some(&fx.cookie), Value::Null)
        .await
        .1;
    let auth = fx.auth.clone();
    let cookie = fx.cookie.clone();
    let track_path = fx.path(true);
    let area_path = fx.path(false);
    drop(fx);
    repo.db().read_pool().close().await;
    drop(repo);
    let reopened = Arc::new(SqlxRepo::open(&url).await.unwrap());
    let stored_payload: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE id='old-card'")
            .fetch_one(reopened.db().read_pool())
            .await
            .unwrap();
    assert_eq!(stored_payload, old_payload);
    let unrelated: i64 =
        sqlx::query_scalar("SELECT count(*) FROM application_users WHERE id='unrelated'")
            .fetch_one(reopened.db().read_pool())
            .await
            .unwrap();
    assert_eq!(unrelated, 1);
    let db = reopened.db().clone();
    let reload = db
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                let link = share_links::get_tx(&mut tx, &id).await.unwrap();
                assert!(matches!(link.delegation.scope, ShareScope::Track(_)));
                tx.commit().await.unwrap();
                link
            })
        })
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(reload).unwrap(), revoked);
    let app = app(with_repo(&seed, reopened), auth);
    assert_eq!(
        request(&app, "GET", &shared_track, None, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "GET", &shared_area, None, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &track_path, Some(&cookie), Value::Null).await,
        (StatusCode::OK, json!([revoked.clone()]))
    );
    assert_eq!(
        request(&app, "DELETE", &delete, Some(&cookie), Value::Null).await,
        (StatusCode::OK, revoked)
    );
    assert_eq!(
        request(&app, "GET", &area_path, Some(&cookie), Value::Null)
            .await
            .1,
        json!([area["link"]])
    );
}

#[tokio::test]
async fn management_resolves_current_track_parent_and_keeps_sibling_scopes_separate() {
    let fx = Fixture::new().await;
    let first = fx.create(true).await;
    let sibling = fx
        .state
        .raw_repo()
        .track_create(NewTrack {
            area_id: fx.area.clone().into(),
            title: "sibling".into(),
            sort: None,
            cwd: "/fixture/sibling".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let sibling_path = format!("/api/tracks/{}/share-links", sibling.id);
    assert_eq!(
        fx.request("POST", &sibling_path, Some(&fx.cookie), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        fx.request("GET", &fx.path(true), Some(&fx.cookie), Value::Null)
            .await
            .1,
        json!([first["link"]])
    );
    fx.register("alice").await;
    let foreign = fx
        .state
        .raw_repo()
        .area_create(
            "alice",
            NewArea {
                name: "foreign".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer()
        .execute(
            sqlx::query("UPDATE tracks SET area_id=?2 WHERE id=?1")
                .bind(&fx.track)
                .bind(foreign.id.as_str()),
        )
        .await
        .unwrap();
    for method in ["GET", "POST"] {
        assert_eq!(
            fx.request(method, &fx.path(true), Some(&fx.cookie), json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let id = first["link"]["delegation"]["id"].as_str().unwrap();
    assert_eq!(
        fx.request(
            "DELETE",
            &format!("/api/share-links/{id}"),
            Some(&fx.cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fx.request("GET", &sibling_path, Some(&fx.cookie), Value::Null)
            .await
            .0,
        StatusCode::OK
    );
}

#[path = "share_links/reads.rs"]
mod reads;

#[path = "share_links/events.rs"]
mod events;

#[path = "share_links/conversations.rs"]
mod conversations;

#[path = "share_links/retry.rs"]
mod retry;

#[path = "share_links/input.rs"]
mod input;
