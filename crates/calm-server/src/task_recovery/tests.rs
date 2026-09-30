//! Every admission refusal, driven through the real admission function with a fixture that
//! trips exactly that refusing check.
use super::admission;
use super::launch_test_support::{RecoveryFixture, initial_claimed_task, recovered_claimed_task};
use super::refusal::{
    AdmissionError, RecoveryRefusal, RecoveryRefusalCode as Code, RefusalKind as Kind,
};
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, TaskReporter, begin_immediate_tx, task_fail_from_worker_tx};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, TrackId};
use crate::model::{NewArea, NewCard, NewTrack};
use crate::routes::theme::RequestTheme;
use crate::state::WriteContext;
use crate::track_report::{ReportDocOp, ReportEditTarget, TrackReportPayload};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const PLANNER: &str = calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;

/// One fixture per refusing check in admission, in source order; each trips exactly that check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Site {
    /// The actor is neither a User nor a Planner.
    ActorNotUserOrPlanner,
    /// The Planner session cannot be resolved to a role.
    PlannerSessionUnresolved,
    /// The Track is closed, so it does not schedule work.
    TrackNotReady,
    /// Child-task routes are not recoverable.
    ChildTaskRoute,
    /// A Planner asked for a task outside auto-declare (user-owned or
    /// declare-and-wait).
    PlannerOutsideAutoDeclare,
    /// The bounded Planner retry for this key was consumed.
    PlannerRetryLimit,
    /// The frozen constraint fails `validate` for this Track.
    ConstraintShapeInvalid,
    /// The frozen context closure was truncated at claim.
    FrozenContextTruncated,
    /// No frozen context was recorded for the failed execution.
    FrozenContextMissing,
    /// The frozen context is valid JSON but not a reference list.
    FrozenContextMalformed,
    /// No declaration in the report carries the key.
    DeclarationMissing,
    /// The declaration is duplicated, tombstoned or not ready.
    DeclarationNotCurrent,
    /// The declaration block carries validation diagnostics.
    DeclarationInvalid,
    /// The declare-and-wait release was withdrawn.
    ReleaseWithdrawn,
    /// The constraint fails `validate` at the inner check.
    InnerConstraintShapeInvalid,
    /// The declaration's route or author differs from the frozen ones.
    RouteOrAuthorChanged,
    /// A frozen context reference names a missing Track.
    FrozenTrackMissing,
    /// A frozen context reference left the authorized area.
    ContextMovedOutsideArea,
    /// A frozen context reference names a Track without a report.
    FrozenReportMissing,
    /// A frozen context reference names a missing block.
    FrozenBlockMissing,
    /// The root reference no longer names the declaration block.
    RootIdentityChanged,
    /// A frozen reference's content hash changed.
    RootHashChanged,
    /// A prepared isolated execution shares its key with other operations or
    /// verification effects.
    IsolatedAmbiguousOperations,
    /// An ordinary worker card was prepared for the key.
    OrdinaryWorkerPrepared,
    /// Verification effects exist for the key without a worker card.
    VerificationEffectsWithoutWorker,
    /// The failure is not a spawn failure and nothing proves a stop.
    NotSpawnFailedWithoutStopProof,
    /// A keyed operation has uncertain external effects.
    OperationUncertainExternalEffects,
    /// The execution is not a failed isolated-route execution.
    IsolatedRouteMismatch,
    /// The named operation is not this execution's isolated operation.
    IsolatedOperationNotThisExecution,
    /// Compensation state or spawn artifacts were recorded.
    IsolatedCompensationRecorded,
    /// The isolated operation has not reached a terminal phase.
    IsolatedStopPending,
    /// The isolated operation ended without a failed outcome.
    IsolatedOperationTerminalWithoutFailure,
    /// The operation's journal record cannot be read as a prepared run.
    IsolatedRecordUnreadable,
    /// The journal holds no quiescence proof for the terminal operation.
    IsolatedStopUnconfirmed,
    /// The recorded run's admission is not closed, or its identity chain or
    /// stop proof does not validate for this execution.
    IsolatedStopIdentityMismatch,
    /// The attempt's allocation row is missing.
    AllocationMissing,
    /// The accepted recovery's predecessor row is missing.
    PredecessorRowMissing,
    /// The accepted recovery's admitting actor is unsupported.
    ProvenanceUnsupported,
    /// The Track no longer schedules the accepted recovery.
    TrackNoLongerSchedules,
}
const CHILD_ROUTE: &str = calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;

fn declaration(declared_by: &str) -> Value {
    json!({"key":"b","kind":"terminal","command":"true","ready":true,"declared_by":declared_by})
}

fn isolated_declaration() -> Value {
    json!({"key":"b","kind":"codex","goal":"Write result.txt and report.","ready":true,
        "declared_by":"user","no_gate_reason":"Isolated fixture; no machine verification.",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}})
}

struct Fx {
    repo: Arc<SqlxRepo>,
    fixture: RecoveryFixture,
    track_id: String,
}

async fn open() -> (Arc<SqlxRepo>, EventBus, WriteContext, String) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = crate::db::RepoSyncDomainRaw::area_create(
        repo.as_ref(),
        NewArea {
            name: "recovery refusals".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = crate::db::RepoSyncDomainRaw::track_create(
        repo.as_ref(),
        NewTrack {
            template_input: None,
            area_id: area.id,
            title: "recovery refusals".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let write = WriteContext::new(
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    );
    (repo, EventBus::new(), write, track.id.to_string())
}

/// An initial attempt that failed before worker preparation. The REST door only accepts
/// user-owned declarations, so a Planner-declared fixture rewrites the CRDT authority afterwards.
async fn failed_initial(declared_by: &str) -> Fx {
    let fx = failed_initial_with(declaration("user")).await;
    if declared_by != "user" {
        let pool = fx.repo.sqlite_pool().unwrap();
        let block_id = fx.fixture.block_id().to_string();
        edit_report_crdt(&pool, &fx.track_id, |doc| {
            doc.upsert_block(
                Some(&block_id),
                "task",
                &calm_types::report_blocks::render_fence("task", &declaration(declared_by)),
            )
            .unwrap();
        })
        .await;
        sqlx::query("UPDATE tasks SET declared_by=?1 WHERE id=?2")
            .bind(declared_by)
            .bind(&fx.fixture.task.id)
            .execute(&pool)
            .await
            .unwrap();
    }
    fx
}

/// A user-owned initial attempt under `declaration`, claimed through the
/// production route and failed before worker preparation.
async fn failed_initial_with(declaration: Value) -> Fx {
    let (repo, events, write, track_id) = open().await;
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let fixture = initial_claimed_task(repo_dyn, events, write, &track_id, declaration).await;
    let pool = repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert_eq!(
        task_fail_from_worker_tx(
            &mut tx,
            &fixture.task.id,
            &track_id,
            TaskReporter::Kernel,
            "spawn-failed: controlled preparation failure",
            2,
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    Fx {
        repo,
        fixture,
        track_id,
    }
}

/// Edit the report's CRDT directly, writing its projection to `payload` in the same UPDATE as
/// the report writer does.
async fn edit_report_crdt(
    pool: &sqlx::SqlitePool,
    track_id: &str,
    edit: impl FnOnce(&mut crate::track_report_doc::ReportDoc),
) {
    let (card_id, bytes): (String, Vec<u8>) =
        sqlx::query_as("SELECT id, body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(track_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let mut doc = crate::track_report_doc::ReportDoc::from_bytes(&bytes).unwrap();
    edit(&mut doc);
    sqlx::query(
        "UPDATE cards SET body_crdt=?1,payload=json_set(payload,'$.body',?2,'$.blocks',json(?3)) \
         WHERE id=?4",
    )
    .bind(doc.to_bytes())
    .bind(doc.project().unwrap().1)
    .bind(serde_json::to_string(&doc.blocks_snapshot().unwrap()).unwrap())
    .bind(&card_id)
    .execute(pool)
    .await
    .unwrap();
}

/// A recovered (generation 2) attempt, claimed: `check_recovery_attempt_tx` passes as-is.
async fn recovered() -> Fx {
    let (repo, events, write, track_id) = open().await;
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let fixture =
        recovered_claimed_task(repo_dyn, events, write, &track_id, declaration("user")).await;
    Fx {
        repo,
        fixture,
        track_id,
    }
}

async fn sql(fx: &Fx, statement: &str, binds: &[&str]) {
    let pool = fx.repo.sqlite_pool().unwrap();
    let mut query = sqlx::query(statement);
    for bind in binds {
        query = query.bind(bind.to_string());
    }
    query
        .execute(&pool)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

/// Inserts a keyed operation row `op-{kind}-{phase}` and returns its id.
async fn insert_operation(fx: &Fx, kind: &str, phase: &str, tx_output: Option<&str>) -> String {
    insert_operation_with(fx, kind, phase, tx_output, None).await
}

async fn insert_operation_with(
    fx: &Fx,
    kind: &str,
    phase: &str,
    tx_output: Option<&str>,
    compensation_state: Option<&str>,
) -> String {
    let pool = fx.repo.sqlite_pool().unwrap();
    let id = format!("op-{kind}-{phase}");
    sqlx::query(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,\
         target_type,target_id,target_json,payload_json,phase,tx_output_json,\
         compensation_state,created_at_ms,updated_at_ms) VALUES(?1,?1,?2,?3,'h','track',\
         ?4,'{}','{}',?5,?6,?7,1,1)",
    )
    .bind(&id)
    .bind(kind)
    .bind(&fx.fixture.task.id)
    .bind(&fx.track_id)
    .bind(phase)
    .bind(tx_output)
    .bind(compensation_state)
    .execute(&pool)
    .await
    .unwrap();
    id
}

async fn require_stopped(fx: &Fx, op_id: &str) -> Result<(), AdmissionError> {
    let pool = fx.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let task = crate::db::sqlite::task_get_tx(&mut tx, &fx.fixture.task.id)
        .await
        .unwrap()
        .unwrap();
    crate::isolated_codex::recovery::require_stopped_tx(&mut tx, &task, op_id).await
}

const ISOLATED_OP_ID: &str = "op-isolated-prepared";
const ISOLATED_CARD_ID: &str = "card-isolated";

/// The receipt the dedicated-codex Controller writes for a prepared isolated execution
/// whose namespace stop it proved, as JSON.
fn isolated_receipt(fx: &Fx) -> Value {
    use crate::dedicated_codex::{DedicatedIdentity, DedicatedRequest};
    let task_id = fx.fixture.task.id.clone();
    let request = DedicatedRequest {
        identity: DedicatedIdentity {
            run_id: ISOLATED_OP_ID.into(),
            attempt_id: task_id.clone(),
            card_id: ISOLATED_CARD_ID.into(),
            session_id: "session-isolated".into(),
        },
        workspace: "/workspaces/isolated".into(),
        developer_instructions: "fixture".into(),
    };
    // The digest is over the typed request's own serialization, as the Controller computes it.
    let request_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&request).unwrap())
    );
    let request = serde_json::to_value(&request).unwrap();
    let boundary = json!({
        "run_id": ISOLATED_OP_ID, "attempt_id": task_id, "config_digest": "0".repeat(64),
        "init": {"pid": 1, "start_time": 1, "boot_id": "boot", "namespace_inode": 1},
    });
    json!({
        "version": "isolated-run-v1",
        "request": request,
        "track_id": fx.track_id,
        "native_token": "token",
        "admission": "closed",
        "provider": {"state": "prepared", "record": {
            "endpoint": {
                "version": 2,
                "request": request,
                "home": {
                    "version": 1, "run_id": ISOLATED_OP_ID, "request_digest": request_digest,
                    "root": "/private/run", "home": "/private/run/home",
                    "control": "/private/run/control", "socket": "/private/run/socket",
                    "mcp_source_socket": "/private/mcp.sock", "mcp_device": 1, "mcp_inode": 1,
                    "policy_digest": "policy", "authentication_digest": "auth",
                },
                "boundary": boundary,
                "launch": {
                    "attempt_id": task_id, "network": "isolated",
                    "workspace": "/workspaces/isolated", "program": "/bin/true", "args": [],
                    "environment": {}, "mounts": [],
                },
            },
            "phase": {"TurnActive": {
                "thread_id": "thread", "turn_id": "turn", "request_key": "key",
                "prompt_digest": "digest",
            }},
            "stop": {"Quiesced": {
                "handle": boundary, "observed_at_ms": 1, "method": "init_absent",
            }},
        }},
    })
}

/// The keyed isolated operation row (`ISOLATED_OP_ID`) as `prepare_tx_and_advance` leaves it;
/// `parked` rows carry the timestamps the phase CHECK requires.
async fn insert_prepared_isolated_operation(fx: &Fx, phase: &str, receipt: &Value) {
    let payload = crate::isolated_codex::WorkerPayload {
        version: crate::isolated_codex::WorkerVersion::V1,
        actor: ActorId::KernelDispatcher,
        track_id: fx.track_id.clone(),
        task_id: fx.fixture.task.id.clone(),
        idempotency_key: fx.fixture.task.id.clone(),
    };
    let output = json!({
        "target_type": "card", "target_id": ISOLATED_CARD_ID, "result": {},
        "data": {"isolated_execution": receipt},
    });
    let parked = (phase == "parked").then_some(1_i64);
    let pool = fx.repo.sqlite_pool().unwrap();
    sqlx::query(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,\
         target_type,target_id,target_json,payload_json,phase,tx_output_json,\
         parked_at_ms,parked_deadline_ms,created_at_ms,updated_at_ms) VALUES(?1,?1,?2,?3,'h',\
         'card',?4,'{}',?5,?6,?7,?8,?8,1,1)",
    )
    .bind(ISOLATED_OP_ID)
    .bind(crate::isolated_codex::OPERATION_KIND)
    .bind(&fx.fixture.task.id)
    .bind(ISOLATED_CARD_ID)
    .bind(serde_json::to_string(&payload).unwrap())
    .bind(phase)
    .bind(output.to_string())
    .bind(parked)
    .execute(&pool)
    .await
    .unwrap();
}

async fn frozen_refs(fx: &Fx) -> Vec<Value> {
    let pool = fx.repo.sqlite_pool().unwrap();
    let json: String = sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id=?1")
        .bind(&fx.fixture.task.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

async fn set_frozen_refs(fx: &Fx, refs: Vec<Value>) {
    sql(
        fx,
        "UPDATE tasks SET claim_context_json=?1 WHERE id=?2",
        &[&Value::Array(refs).to_string(), &fx.fixture.task.id],
    )
    .await;
}

fn foreign_ref(track_id: &str, block_id: &str) -> Value {
    json!({"track_id": track_id, "block_id": block_id, "rev": 0, "hash": "0".repeat(64), "is_root": false})
}

/// A second track (optionally in another area) with a report holding one prose block.
async fn other_track(fx: &Fx, other_area: bool) -> (String, String) {
    let area_id = if other_area {
        crate::db::RepoSyncDomainRaw::area_create(
            fx.repo.as_ref(),
            NewArea {
                name: "elsewhere".into(),
                color: "#202020".into(),
                sort: None,
            },
        )
        .await
        .unwrap()
        .id
    } else {
        fx.repo
            .track_get(&fx.track_id)
            .await
            .unwrap()
            .unwrap()
            .area_id
    };
    let track = crate::db::RepoSyncDomainRaw::track_create(
        fx.repo.as_ref(),
        NewTrack {
            template_input: None,
            area_id,
            title: "reference source".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    fx.repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    // A fresh report card has no CRDT layout yet: its first block goes
    // through the REST door, as the claimed-task fixture does.
    let target = ReportEditTarget::resolve(fx.repo.as_ref(), track.id.as_str())
        .await
        .unwrap();
    let (_, block) = crate::track_report::write::rest_user_block_op(
        fx.repo.as_ref(),
        &fx.fixture.events,
        &fx.fixture.write,
        target,
        ReportDocOp::UpsertBlock {
            id: None,
            kind: "prose".into(),
            content: "Frozen input".into(),
            if_rev: None,
            if_doc_rev: Some(0),
            position: None,
        },
    )
    .await
    .unwrap();
    (track.id.to_string(), block.unwrap().id)
}

async fn prose_block(fx: &Fx, track_id: &str) -> String {
    let mut id = None;
    edit_report_crdt(&fx.repo.sqlite_pool().unwrap(), track_id, |doc| {
        id = Some(doc.upsert_block(None, "prose", "Frozen input").unwrap().0);
    })
    .await;
    id.unwrap()
}

async fn admit(
    fx: &Fx,
    actor: ActorId,
    generation: i64,
) -> Result<calm_types::task_recovery::TaskRecoveryConstraint, AdmissionError> {
    let pool = fx.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let track = crate::db::sqlite::track_get_tx(&mut tx, &TrackId::from(fx.track_id.as_str()))
        .await
        .unwrap();
    let task = crate::db::sqlite::task_get_tx(&mut tx, &fx.fixture.task.id)
        .await
        .unwrap()
        .unwrap();
    // The transaction is dropped (rolled back): admission is read-only.
    admission::admit_recovery_tx(&mut tx, &track, &task, generation, &actor).await
}

async fn fail_current(fx: &Fx) {
    let pool = fx.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    assert_eq!(
        task_fail_from_worker_tx(
            &mut tx,
            &fx.fixture.task.id,
            &fx.track_id,
            TaskReporter::Kernel,
            "spawn-failed: controlled preparation failure",
            4,
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
}

async fn check_attempt(fx: &Fx) -> Result<(), AdmissionError> {
    check_attempt_for(fx, &fx.fixture.task.id).await
}

async fn check_attempt_for(fx: &Fx, attempt_id: &str) -> Result<(), AdmissionError> {
    let pool = fx.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    admission::check_recovery_attempt_tx(&mut tx, attempt_id).await
}

async fn authorize(fx: &Fx, actor: ActorId) -> Result<(), AdmissionError> {
    let pool = fx.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let track = crate::db::sqlite::track_get_tx(&mut tx, &TrackId::from(fx.track_id.as_str()))
        .await
        .unwrap();
    let event = Event::PlanUpdated {
        track_id: track.id.clone(),
        changed_keys: vec!["b".into()],
        agent_message: None,
    };
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    admission::authorize_tx(&mut tx, &actor, &scope, &event).await
}

fn refused<T: std::fmt::Debug>(site: Site, outcome: Result<T, AdmissionError>) -> RecoveryRefusal {
    match outcome {
        Err(AdmissionError::Refused(refusal)) => refusal,
        other => panic!("{site:?}: expected a typed refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn admissible_baselines_pass_so_each_case_trips_exactly_one_site() {
    let fx = failed_initial(PLANNER).await;
    admit(&fx, ActorId::User, 1)
        .await
        .expect("failed-before-preparation is admissible");
    admit(&fx, ActorId::AiPlanner("planner".into()), 1)
        .await
        .expect("a Planner may recover its own auto-declare task once");
    let fx = recovered().await;
    check_attempt(&fx)
        .await
        .expect("a freshly recovered claimed attempt passes its recheck");
    let fx = failed_initial_with(isolated_declaration()).await;
    insert_prepared_isolated_operation(&fx, "failed", &isolated_receipt(&fx)).await;
    admit(&fx, ActorId::User, 1)
        .await
        .expect("a failed isolated execution with a consistent, closed stop proof is admissible");
}

/// Recovery on a closed track refuses with the one reopen sentence; reopening admits it again.
#[tokio::test]
async fn recovery_refuses_on_a_closed_track() {
    let fx = failed_initial(PLANNER).await;
    sql(
        &fx,
        "UPDATE tracks SET closed_at=1 WHERE id=?1",
        &[&fx.track_id],
    )
    .await;
    let refusal = refused(Site::TrackNotReady, admit(&fx, ActorId::User, 1).await);
    assert_eq!(refusal.code, Code::TrackNotReady, "{refusal:?}");
    assert_eq!(refusal.reason, "track is closed; reopen it first");

    sql(
        &fx,
        "UPDATE tracks SET closed_at=NULL WHERE id=?1",
        &[&fx.track_id],
    )
    .await;
    admit(&fx, ActorId::User, 1)
        .await
        .expect("a reopened track admits the recovery");
}

/// What every refusing check names, keyed by its fixture.
const ROWS: &[(Site, Code, Kind)] = &[
    (
        Site::ActorNotUserOrPlanner,
        Code::NotAuthorized,
        Kind::Forbidden,
    ),
    (
        Site::PlannerSessionUnresolved,
        Code::NotAuthorized,
        Kind::Forbidden,
    ),
    (Site::TrackNotReady, Code::TrackNotReady, Kind::Conflict),
    (Site::ChildTaskRoute, Code::UnsupportedSpawn, Kind::Conflict),
    (
        Site::PlannerOutsideAutoDeclare,
        Code::UserAuthorizationRequired,
        Kind::Forbidden,
    ),
    (
        Site::PlannerRetryLimit,
        Code::RecoveryLimitReached,
        Kind::Forbidden,
    ),
    (
        Site::ConstraintShapeInvalid,
        Code::MissingFrozenContract,
        Kind::Conflict,
    ),
    (
        Site::FrozenContextTruncated,
        Code::MissingFrozenContract,
        Kind::Conflict,
    ),
    (
        Site::FrozenContextMissing,
        Code::MissingFrozenContract,
        Kind::Conflict,
    ),
    (
        Site::FrozenContextMalformed,
        Code::MissingFrozenContract,
        Kind::Conflict,
    ),
    (
        Site::DeclarationMissing,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
    ),
    (
        Site::DeclarationNotCurrent,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
    ),
    (
        Site::DeclarationInvalid,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
    ),
    (
        Site::ReleaseWithdrawn,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
    ),
    (
        Site::InnerConstraintShapeInvalid,
        Code::MissingFrozenContract,
        Kind::Conflict,
    ),
    (
        Site::RouteOrAuthorChanged,
        Code::ContractChanged,
        Kind::Conflict,
    ),
    (
        Site::FrozenTrackMissing,
        Code::ContractChanged,
        Kind::Conflict,
    ),
    (
        Site::ContextMovedOutsideArea,
        Code::ContractChanged,
        Kind::Conflict,
    ),
    (
        Site::FrozenReportMissing,
        Code::ContractChanged,
        Kind::Conflict,
    ),
    (
        Site::FrozenBlockMissing,
        Code::ContractChanged,
        Kind::Conflict,
    ),
    (
        Site::RootIdentityChanged,
        Code::ContractChanged,
        Kind::Conflict,
    ),
    (Site::RootHashChanged, Code::ContractChanged, Kind::Conflict),
    (
        Site::IsolatedAmbiguousOperations,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::OrdinaryWorkerPrepared,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::VerificationEffectsWithoutWorker,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::NotSpawnFailedWithoutStopProof,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::OperationUncertainExternalEffects,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedRouteMismatch,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedOperationNotThisExecution,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedCompensationRecorded,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedStopPending,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedOperationTerminalWithoutFailure,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedRecordUnreadable,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedStopUnconfirmed,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::IsolatedStopIdentityMismatch,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
    ),
    (
        Site::AllocationMissing,
        Code::RecoveryLineageMissing,
        Kind::Conflict,
    ),
    (
        Site::PredecessorRowMissing,
        Code::RecoveryLineageMissing,
        Kind::Conflict,
    ),
    (
        Site::ProvenanceUnsupported,
        Code::NotAuthorized,
        Kind::Conflict,
    ),
    (
        Site::TrackNoLongerSchedules,
        Code::TrackNotReady,
        Kind::Conflict,
    ),
];

/// Drives the real function with a fixture that trips exactly `site`.
async fn drive(site: Site) -> RecoveryRefusal {
    let planner = || ActorId::AiPlanner("planner".into());
    let user = || ActorId::User;
    match site {
        Site::ActorNotUserOrPlanner => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                refused(site, authorize(&fx, ActorId::KernelDispatcher).await)
            })
            .await
        }
        Site::PlannerSessionUnresolved => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                refused(
                    site,
                    authorize(&fx, ActorId::AiPlannerSession("missing-session".into())).await,
                )
            })
            .await
        }
        Site::TrackNotReady => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tracks SET closed_at=1 WHERE id=?1",
                    &[&fx.track_id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::ChildTaskRoute => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tasks SET spawn=?1 WHERE id=?2",
                    &[CHILD_ROUTE, &fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::PlannerOutsideAutoDeclare => {
            Box::pin(async {
                // One site, two conditions of its `||`: both fixtures must
                // agree on what it names.
                let fx = failed_initial("user").await;
                let user_owned = refused(site, admit(&fx, planner(), 1).await);
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1",
                    &[&fx.track_id],
                )
                .await;
                let declare_and_wait = refused(site, admit(&fx, planner(), 1).await);
                assert_eq!(
                    (user_owned.code, user_owned.kind),
                    (declare_and_wait.code, declare_and_wait.kind),
                    "{site:?}"
                );
                user_owned
            })
            .await
        }
        Site::PlannerRetryLimit => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                refused(site, admit(&fx, planner(), 2).await)
            })
            .await
        }
        Site::ConstraintShapeInvalid => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                set_frozen_refs(&fx, Vec::new()).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::FrozenContextTruncated => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tasks SET context_closure_truncated=1 WHERE id=?1",
                    &[&fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::FrozenContextMissing => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tasks SET claim_context_json=NULL WHERE id=?1",
                    &[&fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::FrozenContextMalformed => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    // The column CHECKs json_valid; malformed here means
                    // valid JSON that is not a reference list.
                    "UPDATE tasks SET claim_context_json='{}' WHERE id=?1",
                    &[&fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::DeclarationMissing => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                let block_id = fx.fixture.block_id().to_string();
                edit_report_crdt(&fx.repo.sqlite_pool().unwrap(), &fx.track_id, |doc| {
                    doc.delete_block(&block_id).unwrap();
                })
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::DeclarationNotCurrent => {
            Box::pin(async {
                let fx = failed_initial("user").await;
                fx.fixture.withdraw().await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::DeclarationInvalid => {
            Box::pin(async {
                let fx = failed_initial("user").await;
                // The REST door refuses an invalid payload; only the CRDT
                // authority can hold one (an older document, a merge).
                let mut invalid = fx.fixture.declaration();
                invalid["goal"] = json!("a terminal task must not carry a goal");
                let block_id = fx.fixture.block_id().to_string();
                edit_report_crdt(&fx.repo.sqlite_pool().unwrap(), &fx.track_id, |doc| {
                    doc.upsert_block(
                        Some(&block_id),
                        "task",
                        &calm_types::report_blocks::render_fence("task", &invalid),
                    )
                    .unwrap();
                })
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::ReleaseWithdrawn => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1",
                    &[&fx.track_id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::InnerConstraintShapeInvalid => {
            Box::pin(async {
                // Neither view-consulted entry can trip this site; drive the real function with a
                // constraint that parses but fails `validate()`.
                let fx = failed_initial(PLANNER).await;
                let pool = fx.repo.sqlite_pool().unwrap();
                let mut tx = begin_immediate_tx(&pool).await.unwrap();
                let track =
                    crate::db::sqlite::track_get_tx(&mut tx, &TrackId::from(fx.track_id.as_str()))
                        .await
                        .unwrap();
                let constraint = calm_types::task_recovery::TaskRecoveryConstraint::V1 {
                    refs: Vec::new(),
                    spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
                    declared_by: PLANNER.into(),
                };
                let (_, blocks) =
                    crate::track_report::report_blocks_snapshot_tx(&mut tx, fx.track_id.as_str())
                        .await
                        .unwrap();
                refused(
                    site,
                    admission::check_constraint_tx(&mut tx, &track, &blocks, "b", &constraint)
                        .await,
                )
            })
            .await
        }
        Site::RouteOrAuthorChanged => {
            Box::pin(async {
                let fx = failed_initial("user").await;
                sql(
                    &fx,
                    "UPDATE tasks SET declared_by=?1 WHERE id=?2",
                    &[PLANNER, &fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::FrozenTrackMissing => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                let mut refs = frozen_refs(&fx).await;
                refs.push(foreign_ref("no-such-track", "b_missing"));
                set_frozen_refs(&fx, refs).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::ContextMovedOutsideArea => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                let (track, block) = other_track(&fx, true).await;
                let mut refs = frozen_refs(&fx).await;
                refs.push(foreign_ref(&track, &block));
                set_frozen_refs(&fx, refs).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::FrozenReportMissing => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                let (track, block) = other_track(&fx, false).await;
                sql(
                    &fx,
                    "DELETE FROM cards WHERE track_id=?1 AND kind='track-report'",
                    &[&track],
                )
                .await;
                let mut refs = frozen_refs(&fx).await;
                refs.push(foreign_ref(&track, &block));
                set_frozen_refs(&fx, refs).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::FrozenBlockMissing => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                let (track, _) = other_track(&fx, false).await;
                let mut refs = frozen_refs(&fx).await;
                refs.push(foreign_ref(&track, "b_missing"));
                set_frozen_refs(&fx, refs).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::RootIdentityChanged => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                let other = prose_block(&fx, &fx.track_id).await;
                let mut refs = frozen_refs(&fx).await;
                let root = refs
                    .iter_mut()
                    .find(|reference| reference["is_root"] == true)
                    .unwrap();
                root["block_id"] = json!(other);
                set_frozen_refs(&fx, refs).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::RootHashChanged => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                let mut refs = frozen_refs(&fx).await;
                let root = refs
                    .iter_mut()
                    .find(|reference| reference["is_root"] == true)
                    .unwrap();
                root["hash"] = json!("f".repeat(64));
                set_frozen_refs(&fx, refs).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedAmbiguousOperations => {
            Box::pin(async {
                let fx = failed_initial_with(isolated_declaration()).await;
                insert_operation(
                    &fx,
                    crate::isolated_codex::OPERATION_KIND,
                    "failed",
                    Some("{}"),
                )
                .await;
                sql(
                    &fx,
                    "UPDATE tasks SET gate_attempt=1 WHERE id=?1",
                    &[&fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::OrdinaryWorkerPrepared => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tasks SET worker_card_id='card-prepared' WHERE id=?1",
                    &[&fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::VerificationEffectsWithoutWorker => {
            Box::pin(async {
                // A gate ran, no worker card was ever prepared.
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tasks SET gate_attempt=1 WHERE id=?1",
                    &[&fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::NotSpawnFailedWithoutStopProof => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                sql(
                    &fx,
                    "UPDATE tasks SET status_detail='worker-timeout' WHERE id=?1",
                    &[&fx.fixture.task.id],
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::OperationUncertainExternalEffects => {
            Box::pin(async {
                let fx = failed_initial(PLANNER).await;
                insert_operation(&fx, "terminal-worker", "spawn_started", None).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedRouteMismatch => {
            Box::pin(async {
                // An isolated operation with a preparation receipt on a key
                // whose frozen contract is an ordinary terminal task.
                let fx = failed_initial(PLANNER).await;
                insert_operation(
                    &fx,
                    crate::isolated_codex::OPERATION_KIND,
                    "failed",
                    Some("{}"),
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedOperationNotThisExecution => {
            Box::pin(async {
                // A settlement event naming another operation reaches the fence directly.
                let fx = failed_initial_with(isolated_declaration()).await;
                insert_operation(
                    &fx,
                    crate::isolated_codex::OPERATION_KIND,
                    "failed",
                    Some("{}"),
                )
                .await;
                refused(site, require_stopped(&fx, "op-of-another-execution").await)
            })
            .await
        }
        Site::IsolatedCompensationRecorded => {
            Box::pin(async {
                let fx = failed_initial_with(isolated_declaration()).await;
                insert_operation_with(
                    &fx,
                    crate::isolated_codex::OPERATION_KIND,
                    "failed",
                    Some("{}"),
                    Some("{}"),
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedStopPending => {
            Box::pin(async {
                // The Controller already checkpointed the matching `Quiesced` proof, but the
                // parked-completion transaction has not run: only the phase is unmet.
                let fx = failed_initial_with(isolated_declaration()).await;
                insert_prepared_isolated_operation(&fx, "parked", &isolated_receipt(&fx)).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedOperationTerminalWithoutFailure => {
            Box::pin(async {
                let fx = failed_initial_with(isolated_declaration()).await;
                insert_operation(
                    &fx,
                    crate::isolated_codex::OPERATION_KIND,
                    "succeeded",
                    Some("{}"),
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedRecordUnreadable => {
            Box::pin(async {
                // A failed isolated operation whose receipt is not a run record.
                let fx = failed_initial_with(isolated_declaration()).await;
                insert_operation(
                    &fx,
                    crate::isolated_codex::OPERATION_KIND,
                    "failed",
                    Some("{}"),
                )
                .await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedStopUnconfirmed => {
            Box::pin(async {
                // The retained checkpoint is the stop request, never its proof.
                let fx = failed_initial_with(isolated_declaration()).await;
                let mut receipt = isolated_receipt(&fx);
                *receipt.pointer_mut("/provider/record/stop").unwrap() = json!("Requested");
                insert_prepared_isolated_operation(&fx, "failed", &receipt).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::IsolatedStopIdentityMismatch => {
            Box::pin(async {
                // Every identity field unchanged and the proof recorded; only the admission never closed.
                let fx = failed_initial_with(isolated_declaration()).await;
                let mut receipt = isolated_receipt(&fx);
                *receipt.pointer_mut("/admission").unwrap() = json!("open");
                insert_prepared_isolated_operation(&fx, "failed", &receipt).await;
                refused(site, admit(&fx, user(), 1).await)
            })
            .await
        }
        Site::AllocationMissing => {
            Box::pin(async {
                // tasks(id,track_id,key) references the allocation, so only an unknown attempt id reaches this site.
                let fx = recovered().await;
                refused(site, check_attempt_for(&fx, "no-such-attempt").await)
            })
            .await
        }
        Site::PredecessorRowMissing => {
            Box::pin(async {
                let fx = recovered().await;
                sql(
                    &fx,
                    "DELETE FROM tasks WHERE track_id=?1 AND key='b' AND id<>?2",
                    &[&fx.track_id, &fx.fixture.task.id],
                )
                .await;
                refused(site, check_attempt(&fx).await)
            })
            .await
        }
        Site::ProvenanceUnsupported => {
            Box::pin(async {
            // Allocations are immutable and the service boundary only records User/Planner actors.
            let fx = recovered().await;
            // The insert trigger requires a current failed predecessor.
            fail_current(&fx).await;
            sql(
                &fx,
                "INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms) \
                 SELECT 'kernel-provenance', track_id, key, 3, \
                   json_set(origin_json,'$.previous_attempt_id',attempt_id,'$.idempotency_key','kernel-provenance', \
                     '$.request_fingerprint','kernel-provenance','$.actor',json('{\"kind\":\"Kernel\"}')), \
                   created_at_ms \
                 FROM task_attempt_allocations WHERE attempt_id=?1",
                &[&fx.fixture.task.id],
            )
            .await;
            refused(site, check_attempt_for(&fx, "kernel-provenance").await)
            })
            .await
        }
        Site::TrackNoLongerSchedules => {
            Box::pin(async {
                let fx = recovered().await;
                sql(
                    &fx,
                    "UPDATE tracks SET closed_at=1 WHERE id=?1",
                    &[&fx.track_id],
                )
                .await;
                refused(site, check_attempt(&fx).await)
            })
            .await
        }
    }
}

#[tokio::test]
async fn every_refusal_names_its_code_and_kind() {
    let sites: std::collections::BTreeSet<Site> = ROWS.iter().map(|(site, ..)| *site).collect();
    assert_eq!(sites.len(), ROWS.len(), "one row per fixture");
    assert_eq!(ROWS.len(), 39, "every refusing check has a row");
    for &(site, code, kind) in ROWS {
        let refusal = Box::pin(drive(site)).await;
        assert_eq!(
            (refusal.code, refusal.kind),
            (code, kind),
            "{site:?} named {refusal:?}"
        );
        assert!(!refusal.reason.is_empty(), "{site:?} needs its sentence");
    }
}

#[tokio::test]
async fn isolated_stop_sentences_name_the_condition_their_branch_tests() {
    let pending = Box::pin(drive(Site::IsolatedStopPending)).await;
    assert_eq!(
        pending.reason,
        "predecessor isolated operation is in phase parked, not failed; the settlement that \
         records its stop has not completed, so recovery re-opens once the kernel settles it \
         and delivers the settlement notice"
    );
    let mismatch = Box::pin(drive(Site::IsolatedStopIdentityMismatch)).await;
    assert_eq!(
        mismatch.reason,
        "predecessor isolated execution's recorded run has admission state open, not closed; \
         same-key recovery is permanently unavailable"
    );
    let fx = failed_initial_with(isolated_declaration()).await;
    let mut receipt = isolated_receipt(&fx);
    *receipt
        .pointer_mut("/provider/record/stop/Quiesced/observed_at_ms")
        .unwrap() = json!(0);
    insert_prepared_isolated_operation(&fx, "failed", &receipt).await;
    let stop_proof = refused(
        Site::IsolatedStopIdentityMismatch,
        admit(&fx, ActorId::User, 1).await,
    );
    assert_eq!(
        stop_proof.reason,
        "predecessor isolated execution's recorded run has an identity chain or stop proof that \
         does not validate for this execution; same-key recovery is permanently unavailable"
    );
}

/// The wire spellings are a published vocabulary: distinct and byte-stable.
#[test]
fn refusal_code_spellings_are_distinct_and_stable() {
    let all = [
        Code::NotAuthorized,
        Code::UserAuthorizationRequired,
        Code::RecoveryLimitReached,
        Code::TrackNotReady,
        Code::UnsupportedSpawn,
        Code::PredecessorNotQuiescent,
        Code::DeclarationWithdrawn,
        Code::MissingFrozenContract,
        Code::ContractChanged,
        Code::RecoveryLineageMissing,
    ];
    let spellings: std::collections::BTreeSet<&str> =
        all.iter().map(|code| code.as_str()).collect();
    assert_eq!(spellings.len(), all.len());
    for legacy in [
        "recovery_limit_reached",
        "user_authorization_required",
        "track_not_ready",
        "unsupported_spawn",
        "predecessor_not_quiescent",
        "declaration_withdrawn",
        "missing_frozen_contract",
        "contract_changed",
    ] {
        assert!(spellings.contains(legacy), "{legacy} left the vocabulary");
    }
}
