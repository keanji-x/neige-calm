use super::*;
use axum::routing::get;
use tower::ServiceExt;

fn auth() -> AuthState {
    AuthState::new(AuthConfig {
        username: Some("deployment-owner".into()),
        password: Some("fixture-password".into()),
        dev_autologin: false,
        display_name: "Deployment owner".into(),
    })
}

fn identity(user_id: &str, name: &str) -> SessionIdentity {
    SessionIdentity {
        user_id: user_id.into(),
        display_name: name.into(),
        role: "member".into(),
    }
}

async fn echo_principal(principal: Principal) -> Json<WhoamiBody> {
    Json(WhoamiBody::from(&principal))
}

/// Only the generic auth surface is exposed: synthetic identities never enter
/// the application router, whose resource authorization is a later slice.
fn app(auth: AuthState) -> Router {
    let http = Router::new()
        .route("/principal", get(echo_principal))
        .layer(axum::middleware::from_fn(crate::actor::actor_middleware))
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            require_session,
        ));
    let ws = Router::new()
        .route("/ws-principal", get(echo_principal))
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            require_session_ws,
        ));
    http.merge(ws).merge(session_router().with_state(auth))
}

async fn assert_identity(app: &Router, token: &str, identity: &SessionIdentity) {
    for path in ["/principal", "/ws-principal", "/api/auth/whoami"] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
                    .header("X-Calm-Actor", "ai:codex")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["userId"], identity.user_id, "{path}");
        assert_eq!(body["displayName"], identity.display_name, "{path}");
        assert_eq!(body["role"], identity.role, "{path}");
        assert_eq!(body["sessionId"], token, "{path}");
    }
}

#[tokio::test]
async fn session_identity_survives_all_generic_gates_and_config_changes() {
    let mut auth = auth();
    let jian = identity("jian", "Jian");
    let alice = identity("alice", "Alice");
    let jian_token = auth
        .sessions
        .create(jian.clone(), SessionAuthority::PasswordLogin);
    let alice_token = auth
        .sessions
        .create(alice.clone(), SessionAuthority::PairedDevice);
    Arc::make_mut(&mut auth.config).display_name = "Renamed deployment owner".into();
    let app = app(auth);
    assert_identity(&app, &jian_token, &jian).await;
    assert_identity(&app, &alice_token, &alice).await;
}

#[derive(Debug)]
struct Backend(Session);

impl SessionBackend for Backend {
    fn get(&self, token: &str) -> Option<Session> {
        (token == self.0.session_id).then(|| self.0.clone())
    }

    fn remove(&self, _token: &str) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn session_identity_from_feature_backend_survives_all_generic_gates() {
    let auth = auth();
    let identity = identity("alice", "Alice from backend");
    let token = "backend-credential";
    auth.sessions.set_backend(Arc::new(Backend(Session {
        session_id: token.into(),
        identity: identity.clone(),
        authority: SessionAuthority::PairedDevice,
    })));
    assert_identity(&app(auth), token, &identity).await;
}

#[tokio::test]
async fn declared_actor_does_not_authenticate_any_generic_gate() {
    let app = app(auth());
    for path in ["/principal", "/ws-principal", "/api/auth/whoami"] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .header("X-Calm-Actor", "user")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[test]
fn deployment_owner_depends_only_on_authenticated_user_id() {
    let mut principal = Principal::from_identity(
        SessionIdentity {
            user_id: OWNER_USER_ID.into(),
            display_name: "Another display name".into(),
            role: "member".into(),
        },
        "fixture-session".into(),
    );
    assert!(principal.is_deployment_owner());
    principal.user_id = "alice".into();
    principal.display_name = DEFAULT_DISPLAY_NAME.into();
    principal.role = OWNER_ROLE.into();
    assert!(!principal.is_deployment_owner());
}

#[tokio::test]
async fn owner_profile_startup_preserves_stable_identity_and_does_not_repair_membership() {
    use crate::db::prelude::*;
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let area = repo
        .area_create(
            OWNER_USER_ID,
            crate::model::NewArea {
                name: "Existing".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    sqlx::query("DELETE FROM area_members WHERE area_id=?1")
        .bind(area.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let created: i64 = sqlx::query_scalar("SELECT created_at FROM application_users WHERE id=?1")
        .bind(OWNER_USER_ID)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    use clap::Parser;
    let cfg = crate::config::Config::try_parse_from([
        "calm-server",
        "--auth-username",
        "Renamed profile",
    ])
    .unwrap();
    initialize_owner_profile(repo.db(), &cfg).await.unwrap();
    let profile: (String, String, i64) =
        sqlx::query_as("SELECT id,display_name,created_at FROM application_users")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        profile,
        (OWNER_USER_ID.into(), "Renamed profile".into(), created)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM area_members")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
}
