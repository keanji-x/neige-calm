#![cfg(test)]
//! Real source middleware -> actual shared Track allocation -> canonical DB
//! acceptance. Fixture preparation supplies data; it does not copy the policy.
use super::*;
use crate::auth::{AuthConfig, AuthState, SessionAuthority, SessionIdentity};
use crate::db::prelude::*;
use crate::db::sqlite::conversation::*;
use crate::db::sqlite::{SqlxRepo, area_membership, begin_immediate_tx};
use crate::model::{NewArea, RequestTheme};
use crate::routes::tracks::CreateTrackRequest;
use crate::session_projection_repo::AgentProvider;
use crate::state::AppState;
use axum::extract::FromRef;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;

mod binding_tests;
mod plan_tests;

const FIRST: &str = "original accepted human body, never a start envelope";

struct Fixture {
    _temp: tempfile::TempDir,
    repo: Arc<SqlxRepo>,
    state: AppState,
    route: RouteState,
    area: String,
    auth: AuthState,
    session: String,
    source: HumanInputSource,
    root: std::path::PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let (temp, repo, state) = crate::daily_planner::tests::fixture().await;
        let area = repo
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "Human first input".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap()
            .id
            .to_string();
        let auth = AuthState::new(AuthConfig {
            username: Some("Owner".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let session = auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin,
        );
        let source = actual_source(&auth, &session).await;
        let root = temp.path().join("never-materialized-root");
        let mut route = RouteState::from_ref(&state);
        route.workspace_root = root.clone();
        Self {
            _temp: temp,
            repo,
            state,
            route,
            area,
            auth,
            session,
            source,
            root,
        }
    }

    async fn accept(&self, key: &str, text: &str) -> Result<HumanTrackCreationResult> {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", key.parse().unwrap());
        accept_dormant_human_track(
            self.route.clone(),
            self.source.clone(),
            headers,
            self.request(text),
            crate::routes::planner_model::CatalogAdvice::default(),
        )
        .await
    }

    fn request(&self, text: &str) -> CreateTrackRequest {
        CreateTrackRequest {
            admin: false,
            area_id: self.area.clone().into(),
            title: "new blank Track".into(),
            sort: None,
            cwd: None,
            isolation: None,
            template_id: None,
            recipe_id: None,
            template_input: None,
            attach_folder: false,
            allow_cross_area_cwd: None,
            theme: RequestTheme::default_dark(),
            fork_report_from: None,
            first_message: Some(text.into()),
            model: None,
            reasoning_effort: None,
            planner_provider: AgentProvider::Codex,
        }
    }

    async fn counts(&self) -> (i64, i64, i64, i64, i64, i64) {
        sqlx::query_as("SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM cards),
            (SELECT count(*) FROM worker_sessions),(SELECT count(*) FROM conversation_messages),
            (SELECT count(*) FROM conversation_human_track_creations),(SELECT count(*) FROM operations)")
            .fetch_one(self.repo.pool()).await.unwrap()
    }
}

async fn actual_source(auth: &AuthState, token: &str) -> HumanInputSource {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("{}={token}", crate::auth::SESSION_COOKIE)
            .parse()
            .unwrap(),
    );
    let (status, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    source.unwrap()
}

fn ids(ack: &HumanTrackCreationAck) -> (&str, &str, &str, &str, &MessageRevisionRef) {
    let HumanTrackCreationAck::V1 {
        track_id,
        planner_card_id,
        report_card_id,
        origin_runtime_id,
        first_message,
        ..
    } = ack;
    (
        track_id,
        planner_card_id,
        report_card_id,
        origin_runtime_id,
        first_message,
    )
}

fn assert_replayed(result: HumanTrackCreationResult, expected: &HumanTrackCreationAck) {
    assert_eq!(&result.ack, expected);
}

#[tokio::test]
async fn actual_allocation_accepts_once_without_native_or_workspace_effects() {
    let fx = Fixture::new().await;
    let result = fx.accept("fresh", FIRST).await.unwrap();
    let ack = result.ack;
    let (track, card, _report, runtime, message) = ids(&ack);
    let count = fx.counts().await;
    assert_eq!(count, (1, 2, 1, 1, 1, 0));
    assert!(!fx.root.exists());
    assert!(!fx.state.shared_codex_appserver.is_running());
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    let formal: CanonicalHarnessSnapshot =
        serde_json::from_value(carrier.snapshot().clone()).unwrap();
    assert_eq!(
        formal.header.phase,
        crate::harness::snapshot::HarnessPhaseTag::Idle
    );
    assert!(formal.header.last_thread_id.is_none());
    assert_eq!(formal.canonical_inputs.pending.len(), 1);
    assert_eq!(
        formal.canonical_inputs.pending[0].input_ref(),
        AcceptedInputRef::Human {
            message: message.clone()
        }
    );
    assert_eq!(formal.canonical_inputs.next_queue_position, 1);
    assert!(formal.canonical_inputs.active.is_none());
    assert!(formal.canonical_inputs.attempts.is_empty());
    let revision = revision_tx(&mut tx, message).await.unwrap();
    assert!(matches!(revision.payload,PrivateHistoryPayload::Present {value} if value.text==FIRST));
    let stored = human_track_creation_get_tx(&mut tx, &fx.area, "fresh")
        .await
        .unwrap()
        .unwrap();
    let bodyfree = serde_json::to_string(&stored.acknowledgement).unwrap();
    assert!(!bodyfree.contains(FIRST));
    assert!(
        !serde_json::to_string(carrier.snapshot())
            .unwrap()
            .contains(FIRST)
    );
    let resource = track_create_idempotency_get_tx(&mut tx, &fx.area, "fresh")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resource.track_id, track);
    tx.commit().await.unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_replayed(fx.accept("fresh", FIRST).await.unwrap(), &ack);
    assert_eq!(fx.counts().await, count);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        events
    );
    assert!(!fx.root.exists());
}

#[tokio::test]
async fn original_ack_replays_after_claim_without_readopting_a_body() {
    let fx = Fixture::new().await;
    let ack = fx.accept("original", FIRST).await.unwrap().ack;
    let (_, card, _, runtime, _) = ids(&ack);
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    let plan =
        human_plan_for_pending_prefix_tx(&mut tx, &carrier, std::num::NonZeroU32::MIN, vec![])
            .await
            .unwrap();
    claim_initial_submission_tx(
        &mut tx,
        &mut carrier,
        plan,
        &ConversationExecutionProvenance::LegacySingleUserDeployment {
            deployment_owner_user_id: LEGACY_OWNER_USER_ID.into(),
            provider: AgentProvider::Codex,
            profile: HarnessProfile::Planner,
        },
    )
    .await
    .unwrap();
    let expected = carrier.checkpoint().clone();
    tx.commit().await.unwrap();
    assert_replayed(fx.accept("original", FIRST).await.unwrap(), &ack);
    assert!(matches!(
        fx.accept("original", "different body").await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    assert_eq!(carrier.checkpoint(), &expected);
    assert!(carrier.checkpoint().pending.is_empty());
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn original_ack_survives_dormant_carrier_transfer_and_origin_retirement() {
    let fx = Fixture::new().await;
    let ack = fx.accept("retired-origin", FIRST).await.unwrap().ack;
    let (_, card, _, origin, _) = ids(&ack);
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let old = current_input_carrier_tx(&mut tx, card, origin)
        .await
        .unwrap();
    let snapshot = old.snapshot().clone();
    let checkpoint = old.checkpoint().clone();
    let replacement = crate::model::new_id();
    // Exercise the existing DB carrier owners with the exact reference-only
    // checkpoint. This is not a new startup or native lifecycle entry.
    crate::db::sqlite::session_supersede_and_start_tx(
        &mut tx,
        &origin.to_owned(),
        WorkerSessionInit::shared_planner(
            replacement.clone(),
            card.to_owned(),
            AgentProvider::Codex,
            WorkerSessionState::Idle,
            None,
            snapshot,
            crate::model::now_ms(),
        ),
    )
    .await
    .unwrap();
    crate::db::sqlite::session_delete_tx(&mut tx, &origin.to_owned())
        .await
        .unwrap();
    let current = current_input_carrier_tx(&mut tx, card, &replacement)
        .await
        .unwrap();
    assert_eq!(current.checkpoint(), &checkpoint);
    tx.commit().await.unwrap();
    let before = fx.counts().await;
    assert_replayed(fx.accept("retired-origin", FIRST).await.unwrap(), &ack);
    assert_eq!(fx.counts().await, before);
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let current = current_input_carrier_tx(&mut tx, card, &replacement)
        .await
        .unwrap();
    assert_eq!(current.checkpoint(), &checkpoint);
    assert_eq!(checkpoint.pending.len(), 1);
    assert_eq!(checkpoint.next_queue_position, 1);
    let origin_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM worker_sessions WHERE id=?1)")
            .bind(origin)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    assert!(!origin_exists);
    tx.commit().await.unwrap();
    assert!(!fx.root.exists());
    assert!(!fx.state.shared_codex_appserver.is_running());
}

#[tokio::test]
async fn replay_rechecks_current_session_source_and_membership() {
    let fx = Fixture::new().await;
    fx.accept("owned", FIRST).await.unwrap();
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    area_membership::application_user_register(&mut tx, "alice", "Owner")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(&fx.area)
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let token = fx.auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let alice = actual_source(&fx.auth, &token).await;
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "owned".parse().unwrap());
    assert!(matches!(
        accept_dormant_human_track(
            fx.route.clone(),
            alice,
            headers,
            fx.request(FIRST),
            crate::routes::planner_model::CatalogAdvice::default()
        )
        .await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    sqlx::query("UPDATE area_members SET role='member' WHERE area_id=?1 AND user_id=?2")
        .bind(&fx.area)
        .bind(LEGACY_OWNER_USER_ID)
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE area_members SET role='owner' WHERE area_id=?1 AND user_id='alice'")
        .bind(&fx.area)
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id=?2")
        .bind(&fx.area)
        .bind(LEGACY_OWNER_USER_ID)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        fx.accept("owned", FIRST).await,
        Err(CalmError::Forbidden(_))
    ));
    assert!(matches!(
        fx.accept("new", FIRST).await,
        Err(CalmError::Forbidden(_))
    ));
    fx.auth.sessions.remove(&fx.session).unwrap();
    assert!(matches!(
        fx.accept("owned", FIRST).await,
        Err(CalmError::Unauthorized)
    ));
    assert!(matches!(
        fx.accept("new", FIRST).await,
        Err(CalmError::Unauthorized)
    ));
    assert_eq!(fx.counts().await, (1, 2, 1, 1, 1, 0));
}

#[tokio::test]
async fn deleted_original_allocation_never_reuses_its_acceptance_key() {
    let fx = Fixture::new().await;
    let ack = fx.accept("deleted", FIRST).await.unwrap().ack;
    let (track, _, _, _, _) = ids(&ack);
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    crate::db::sqlite::track_delete_tx(&mut tx, track, fx.repo.track_area_cache())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        fx.accept("deleted", FIRST).await,
        Err(CalmError::IdempotencyKeyExhausted(_))
    ));
    let row: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_human_track_creations")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(row, 1);
}

#[tokio::test]
async fn failed_acceptance_rolls_back_the_actual_allocation_and_events() {
    let fx = Fixture::new().await;
    let before = fx.counts().await;
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_human_acceptance BEFORE INSERT ON conversation_human_track_creations
        BEGIN SELECT RAISE(ABORT,'fixture interrupted acceptance'); END",
    )
    .execute(fx.repo.pool())
    .await
    .unwrap();
    assert!(fx.accept("rollback", FIRST).await.is_err());
    assert_eq!(fx.counts().await, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        events
    );
    assert!(!fx.root.exists());
}

#[tokio::test]
async fn separate_database_owners_race_to_the_same_original_ack() {
    let mut fx = Fixture::new().await;
    let file = fx._temp.path().join("race.sqlite");
    let url = format!("sqlite://{}", file.display());
    let a = Arc::new(SqlxRepo::open(&url).await.unwrap());
    let mut tx = begin_immediate_tx(a.db()).await.unwrap();
    area_membership::application_user_register(&mut tx, LEGACY_OWNER_USER_ID, "Owner")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    fx.area = a
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "shared database".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap()
        .id
        .to_string();
    let b = Arc::new(SqlxRepo::open(&url).await.unwrap());
    assert!(!Arc::ptr_eq(&a, &b));
    assert_eq!(a.database_id(), b.database_id());
    assert!(a.area_get(&fx.area).await.unwrap().is_some());
    assert!(b.area_get(&fx.area).await.unwrap().is_some());
    let mut route_a = fx.route.clone();
    route_a.repo = a.clone();
    route_a.database_id = a.database_id();
    route_a.write =
        crate::state::WriteContext::new(a.card_role_cache().clone(), a.track_area_cache().clone());
    let mut route_b = fx.route.clone();
    route_b.repo = b.clone();
    route_b.database_id = b.database_id();
    route_b.write =
        crate::state::WriteContext::new(b.card_role_cache().clone(), b.track_area_cache().clone());
    route_b.events = crate::event::EventBus::new();
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "same-race".parse().unwrap());
    let (left, right) = tokio::join!(
        accept_dormant_human_track(
            route_a,
            fx.source.clone(),
            headers.clone(),
            fx.request(FIRST),
            crate::routes::planner_model::CatalogAdvice::default()
        ),
        accept_dormant_human_track(
            route_b,
            fx.source.clone(),
            headers,
            fx.request(FIRST),
            crate::routes::planner_model::CatalogAdvice::default()
        ),
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!(left.ack, right.ack);
    let count:(i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM cards),
        (SELECT count(*) FROM worker_sessions),(SELECT count(*) FROM conversation_messages),
        (SELECT count(*) FROM conversation_human_track_creations),(SELECT count(*) FROM operations)")
        .fetch_one(a.pool()).await.unwrap();
    assert_eq!(count, (1, 2, 1, 1, 1, 0));
}

#[tokio::test]
async fn a_shared_area_member_can_accept_content_without_an_execution_grant() {
    let fx = Fixture::new().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    area_membership::application_user_register(&mut tx, "alice", "Owner")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(&fx.area)
    .execute(&mut **tx)
    .await
    .unwrap();
    sqlx::query("UPDATE areas SET conversation_history_contract='shared_history' WHERE id=?1")
        .bind(&fx.area)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let token = fx.auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Owner".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let source = actual_source(&fx.auth, &token).await;
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "member-content".parse().unwrap());
    let ack = accept_dormant_human_track(
        fx.route.clone(),
        source.clone(),
        headers,
        fx.request(FIRST),
        crate::routes::planner_model::CatalogAdvice::default(),
    )
    .await
    .unwrap()
    .ack;
    let (_, card, _, runtime, message) = ids(&ack);
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let stored = message_tx(&mut tx, &message.message_id).await.unwrap();
    assert_eq!(
        stored.author,
        MessageAuthor::AuthenticatedHuman {
            registered_user_id: "alice".into()
        }
    );
    let mut carrier = current_input_carrier_tx(&mut tx, card, runtime)
        .await
        .unwrap();
    let plan = SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: SubmissionRenderInput::Human {
                message: message.clone(),
                author: FrozenHumanAttribution {
                    registered_user_id: "alice".into(),
                    display_name: "Owner".into(),
                },
            },
        }],
    };
    assert!(matches!(
        crate::conversation_inputs::claim_initial_tx(&mut tx, &source, &mut carrier, plan).await,
        Err(CalmError::Forbidden(_))
    ));
    assert!(carrier.checkpoint().active.is_none());
    assert_eq!(carrier.checkpoint().pending.len(), 1);
    tx.commit().await.unwrap();
    assert!(!fx.state.shared_codex_appserver.is_running());
}

#[tokio::test]
async fn historical_resource_binding_cannot_be_adopted_as_first_human_acceptance() {
    let fx = Fixture::new().await;
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "historical".parse().unwrap());
    let plan = HumanTrackCreationPlan::prepare(
        fx.request(FIRST),
        &headers,
        fx.route.workspace_root.clone(),
    )
    .unwrap();
    let (p, options, _) = plan.into_allocation().unwrap();
    let write = fx.route.write.clone();
    let templates = fx.route.templates;
    let teardown_targets = fx.route.teardown_targets.clone();
    let admission_root = fx.route.workspace_root.clone();
    crate::db::write_with_actor_events_typed(
        fx.route.repo.as_ref(),
        None,
        &fx.route.events,
        &fx.route.write,
        move |tx| {
            Box::pin(async move {
                super::super::super::allocation::allocate_track_structure_tx(
                    tx,
                    ActorId::User,
                    p,
                    options,
                    (crate::model::new_id(), crate::model::new_id()),
                    super::super::super::allocation::TrackAllocationOwners {
                        write,
                        templates,
                        teardown_targets,
                        admission_root,
                    },
                    Arc::new(OnceLock::new()),
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    let before = fx.counts().await;
    assert!(matches!(
        fx.accept("historical", FIRST).await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert_eq!(fx.counts().await, before);
    assert_eq!(before, (1, 2, 0, 0, 0, 0));
}

#[tokio::test]
async fn a_historical_retry_slot_without_a_binding_is_not_a_v2_vacant_key() {
    use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    let fx = Fixture::new().await;
    let base = derive_track_create_operation_key(&fx.area, "old-retry");
    let key = crate::routes::conversations_shared::operation_key_attempts(&base)
        .nth(1)
        .unwrap();
    let payload = serde_json::json!({"fixture":"historical start remains undriven"});
    SqlxOperationRepo::new(fx.repo.db().clone())
        .insert_operation(
            crate::routes::conversations_shared::PLANNER_HARNESS_START,
            OperationKey {
                operation_key: key.clone(),
                idempotency_key: Some(key),
                payload_hash: stable_payload_hash(&payload).unwrap(),
            },
            payload,
        )
        .await
        .unwrap();
    assert!(matches!(
        fx.accept("old-retry", FIRST).await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert_eq!(fx.counts().await, (0, 0, 0, 0, 0, 1));
}

#[tokio::test]
async fn original_create_shape_and_original_first_text_remain_separate_fingerprints() {
    let fx = Fixture::new().await;
    fx.accept("frozen", FIRST).await.unwrap();
    let mut request = fx.request(FIRST);
    request.title = "different original shape".into();
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "frozen".parse().unwrap());
    assert!(matches!(
        accept_dormant_human_track(
            fx.route.clone(),
            fx.source.clone(),
            headers,
            request,
            crate::routes::planner_model::CatalogAdvice::default()
        )
        .await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert!(matches!(
        fx.accept("frozen", "a different first input").await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert_eq!(fx.counts().await, (1, 2, 1, 1, 1, 0));
}

#[tokio::test]
async fn system_target_is_not_reclassified_as_human_creation() {
    let fx = Fixture::new().await;
    let system = crate::daily_planner::reconcile(
        &fx.route,
        crate::daily_planner::tests::at("2026-10-17T01:00:00Z"),
    )
    .await
    .unwrap();
    let before = fx.counts().await;
    let mut request = fx.request(FIRST);
    request.area_id = system.area_id.clone();
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "system".parse().unwrap());
    assert!(matches!(
        accept_dormant_human_track(
            fx.route.clone(),
            fx.source.clone(),
            headers,
            request,
            crate::routes::planner_model::CatalogAdvice::default()
        )
        .await,
        Err(CalmError::Forbidden(_))
    ));
    assert_eq!(fx.counts().await, before);
}

#[tokio::test]
async fn canonical_accepted_input_title_uses_its_human_body_without_native_readiness() {
    let fx = Fixture::new().await;
    let accepted = fx
        .accept("canonical-title", "User says:\nActual human text")
        .await
        .unwrap();
    let (_, card, _, _, _) = ids(&accepted.ack);
    let titles = crate::conversation_metadata::titles(fx.repo.as_ref(), &[card.to_owned()])
        .await
        .unwrap();
    assert_eq!(titles.get(card), Some(&Some("User says:".to_owned())));
    assert!(!fx.state.shared_codex_appserver.is_running());
}
