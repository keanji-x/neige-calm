//! Real session middleware -> source facade -> protected canonical transaction.
//! Today itself is created by its production lifecycle, without launching a model.
mod execution;
use super::*;
use crate::auth::{AuthConfig, AuthState, SessionAuthority, SessionIdentity};
use crate::db::prelude::*;
use crate::db::sqlite::area_membership;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::extract::Json;
use crate::model::{CardRole, NewArea, NewTrack, RequestTheme, new_id};
use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
use crate::state::RouteState;
use axum::body::Body;
use axum::extract::{FromRef, State};
use axum::http::{Request, StatusCode};
use axum::{Router, middleware, routing::post};
use calm_types::conversation::ConversationInputCheckpoint;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

#[derive(Clone)]
struct InputState {
    repo: Arc<SqlxRepo>,
    card: String,
    runtime: String,
}

async fn authenticated_accept(
    State(input): State<InputState>,
    principal: Principal,
) -> Result<Json<serde_json::Value>> {
    let accepted = crate::db::write_in_tx_typed(input.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let mut carrier =
                conversation::current_input_carrier_tx(tx, &input.card, &input.runtime).await?;
            accept_human_tx(
                tx,
                &principal,
                &mut carrier,
                HumanPayload {
                    text: "message from the authenticated principal".into(),
                    bound_attachments: vec![],
                },
            )
            .await
        })
    })
    .await?;
    Ok(Json(json!({"input":accepted.input_ref})))
}

struct Fixture {
    _temp: tempfile::TempDir,
    input: InputState,
    auth: AuthState,
    app: Router,
}

impl Fixture {
    async fn new(system: bool) -> Self {
        let (temp, repo, state) = crate::daily_planner::tests::fixture().await;
        let card = if system {
            let track = crate::daily_planner::reconcile(
                &RouteState::from_ref(&state),
                crate::daily_planner::tests::at("2026-10-17T01:00:00Z"),
            )
            .await
            .unwrap();
            let mut planner = None;
            for card in repo.cards_by_track(track.id.as_str()).await.unwrap() {
                if repo.card_role_get(card.id.as_str()).await.unwrap() == Some(CardRole::Planner) {
                    planner = Some(card.id.to_string());
                }
            }
            planner.expect("Today lifecycle creates its own Planner")
        } else {
            let area = repo
                .area_create(
                    LEGACY_OWNER_USER_ID,
                    NewArea {
                        name: "human source".into(),
                        color: "#123456".into(),
                        sort: None,
                    },
                )
                .await
                .unwrap();
            let track = repo
                .track_create(NewTrack {
                    area_id: area.id,
                    title: "human source".into(),
                    sort: None,
                    cwd: temp.path().to_string_lossy().into(),
                    template_id: None,
                    plugin_scope: None,
                    template_input: None,
                    attach_folder: false,
                    theme: RequestTheme::default_dark(),
                })
                .await
                .unwrap();
            let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
            let planner = new_id();
            crate::track_creation::initialize_track_tx(
                &mut tx,
                &track,
                (&planner, &new_id()),
                crate::track_creation::TrackInitialization::Independent {
                    planner_payload: crate::track_creation::planner_harness_card_payload(
                        &crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch()
                            .unwrap(),
                        None,
                        AgentProvider::Codex,
                    ),
                },
                repo.card_role_cache(),
                &crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            planner
        };
        let runtime = new_id();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        crate::db::sqlite::session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::shared_planner(
                runtime.clone(),
                card.clone(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                Some("fixture-owned-thread".into()),
                serde_json::to_value({
                    let mut frame =
                        calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
                    frame.header.last_thread_id = Some("fixture-owned-thread".into());
                    frame
                })
                .unwrap(),
                0,
            ),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let input = InputState {
            repo,
            card,
            runtime,
        };
        let auth = AuthState::new(AuthConfig {
            username: Some("Owner".into()),
            password: Some("fixture-credential".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let app = Router::new()
            .route("/input", post(authenticated_accept))
            .route_layer(middleware::from_fn_with_state(
                auth.clone(),
                crate::auth::require_session,
            ))
            .with_state(input.clone());
        Self {
            _temp: temp,
            input,
            auth,
            app,
        }
    }

    fn session(&self, user: &str, role: &str) -> String {
        self.auth.sessions.create(
            SessionIdentity {
                user_id: user.into(),
                display_name: user.into(),
                role: role.into(),
            },
            SessionAuthority::PasswordLogin,
        )
    }

    async fn send(&self, session: Option<&str>) -> axum::response::Response {
        let mut request = Request::post("/input")
            .header("host", "localhost")
            .header("origin", "http://localhost")
            .header("x-calm-actor", "user")
            // Neither a profile role nor this invented identity header is authority.
            .header("x-calm-user-id", LEGACY_OWNER_USER_ID);
        if let Some(session) = session {
            request = request.header(
                "cookie",
                format!("{}={session}", crate::auth::SESSION_COOKIE),
            );
        }
        self.app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn register(&self, user: &str) {
        let mut tx = begin_immediate_tx(self.input.repo.db()).await.unwrap();
        area_membership::application_user_register(&mut tx, user, user)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn message_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
            .bind(&self.input.card)
            .fetch_one(self.input.repo.pool())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn real_today_accepts_authenticated_deployment_owner_without_fabricated_membership() {
    let fx = Fixture::new(true).await;
    let session = fx.session(LEGACY_OWNER_USER_ID, "owner");
    let response = fx.send(Some(&session)).await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(fx.message_count().await, 1);
    let members: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM area_members m JOIN areas a ON a.id=m.area_id WHERE a.kind='system'",
    )
    .fetch_one(fx.input.repo.pool())
    .await
    .unwrap();
    assert_eq!(members, 0);
}

#[tokio::test]
async fn area_owner_and_owner_profile_do_not_grant_system_conversation_access() {
    let fx = Fixture::new(true).await;
    fx.register("alice").await;
    fx.input
        .repo
        .area_create(
            "alice",
            NewArea {
                name: "Alice owns this Area".into(),
                color: "#111111".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let session = fx.session("alice", "owner");
    assert_eq!(
        fx.send(Some(&session)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(fx.message_count().await, 0);
}

#[tokio::test]
async fn unregistered_identity_and_missing_session_never_fall_back_to_deployment_owner() {
    let fx = Fixture::new(true).await;
    let session = fx.session("not-registered", "owner");
    assert_eq!(
        fx.send(Some(&session)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(fx.send(None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(fx.message_count().await, 0);
}

#[tokio::test]
async fn existing_session_rechecks_current_area_membership_in_the_acceptance_transaction() {
    let fx = Fixture::new(false).await;
    fx.register("alice").await;
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    let area = conversation::conversation_area_tx(&mut tx, &fx.input.card)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(area.id.as_str())
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let session = fx.session("alice", "owner");
    assert_eq!(fx.send(Some(&session)).await.status(), StatusCode::OK);
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id='alice'")
        .bind(area.id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        fx.send(Some(&session)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(fx.message_count().await, 1);
}
