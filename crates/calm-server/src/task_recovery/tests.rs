//! Every admission refusal site, driven through the real admission function
//! with a fixture that trips exactly that site, asserts the `(site, code,
//! kind, continuation)` it names. No assertion here reads the message text.
use super::admission;
use super::launch_test_support::{
    RecoveryFixture, initial_claimed_task, initial_claimed_task_among, recovered_claimed_task,
};
use super::refusal::{
    AdmissionError, RecoveryRefusal, RecoveryRefusalCode as Code, RefusalKind as Kind,
    RefusalSite as Site, SupportedContinuation as Next,
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
use std::sync::Arc;

const PLANNER: &str = calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
const CHILD_ROUTE: &str = calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;

fn declaration(declared_by: &str) -> Value {
    json!({"key":"b","kind":"terminal","command":"true","ready":true,"declared_by":declared_by})
}

/// A user-owned declaration that selects the isolated codex route: the
/// predecessor fence then consults the isolated operation's stop record.
fn isolated_declaration() -> Value {
    json!({"key":"b","kind":"codex","goal":"Write result.txt and report.","ready":true,
        "declared_by":"user","no_gate_reason":"Isolated fixture; no machine verification.",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}})
}

/// A file-delivery producer/consumer pair (the shape `prompts/planner.md`
/// documents); the consumer projects only once its producer is declared, and
/// its recovery must inherit the predecessor's frozen JSON input binding.
fn file_delivery_declaration(key: &str, delivery: Value) -> Value {
    let workspace = if delivery["role"] == "consumer" {
        "file-input"
    } else {
        "empty"
    };
    json!({"key":key,"kind":"codex","goal":"Process the declared JSON document.","ready":true,
        "declared_by":"user","no_gate_reason":"JSON syntax policy only; no business gate.",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":workspace,"file_delivery":delivery}}})
}

fn producer_declaration() -> Value {
    file_delivery_declaration(
        "produce",
        json!({"role":"producer","slot":"result","path":"result.json","policy":"json-document-v1"}),
    )
}

fn consumer_declaration() -> Value {
    file_delivery_declaration(
        "b",
        json!({"role":"consumer","producer":"produce","slot":"result","purpose":"json-input"}),
    )
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

/// An initial attempt that failed before worker preparation: admissible as-is.
/// The REST door only accepts user-owned declarations; a Planner-declared
/// fixture rewrites the CRDT authority afterwards (the same bypass
/// `task_recovery_rebuild_uses_authoritative_crdt_when_payload_cache_diverges`
/// uses) and the frozen row's author with it, so admission sees one author.
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
    failed_initial_among(&[], declaration).await
}

/// [`failed_initial_with`] after `siblings` were declared in the same report.
async fn failed_initial_among(siblings: &[Value], declaration: Value) -> Fx {
    let (repo, events, write, track_id) = open().await;
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let fixture = if siblings.is_empty() {
        initial_claimed_task(repo_dyn, events, write, &track_id, declaration).await
    } else {
        initial_claimed_task_among(repo_dyn, events, write, &track_id, siblings, declaration).await
    };
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

/// Edit the report's CRDT authority directly, leaving the derived payload
/// cache alone: the snapshot readers prefer the CRDT.
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
    sqlx::query("UPDATE cards SET body_crdt=?1 WHERE id=?2")
        .bind(doc.to_bytes())
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

/// The isolated fence, driven directly with an operation id (admission only
/// ever passes the single keyed isolated operation's id).
async fn require_stopped(fx: &Fx, op_id: &str) -> Result<(), AdmissionError> {
    let pool = fx.repo.sqlite_pool().unwrap();
    let mut tx = begin_immediate_tx(&pool).await.unwrap();
    let task = crate::db::sqlite::task_get_tx(&mut tx, &fx.fixture.task.id)
        .await
        .unwrap()
        .unwrap();
    crate::isolated_codex::recovery::require_stopped_tx(&mut tx, &task, op_id).await
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
    let track = crate::track_lifecycle::track_get_tx(&mut tx, &TrackId::from(fx.track_id.as_str()))
        .await
        .unwrap();
    let task = crate::db::sqlite::task_get_tx(&mut tx, &fx.fixture.task.id)
        .await
        .unwrap()
        .unwrap();
    // The transaction is dropped (rolled back): admission is read-only.
    admission::admit_recovery_tx(&mut tx, &track, &task, generation, &actor, true).await
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
    let track = crate::track_lifecycle::track_get_tx(&mut tx, &TrackId::from(fx.track_id.as_str()))
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
}

/// What every site names, keyed by the site. A row's fixture must trip
/// exactly that site (the returned refusal's `site` is asserted), so a
/// fixture that drifts onto a neighbouring site is red, not silently green.
const ROWS: &[(Site, Code, Kind, Next)] = &[
    (
        Site::ActorNotUserOrPlanner,
        Code::NotAuthorized,
        Kind::Forbidden,
        Next::None,
    ),
    (
        Site::PlannerSessionUnresolved,
        Code::NotAuthorized,
        Kind::Forbidden,
        Next::None,
    ),
    (
        Site::TrackNotReady,
        Code::TrackNotReady,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::ChildTaskRoute,
        Code::UnsupportedSpawn,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::PlannerOutsideAutoDeclare,
        Code::UserAuthorizationRequired,
        Kind::Forbidden,
        Next::UserRecovery,
    ),
    (
        Site::PlannerRetryLimit,
        Code::RecoveryLimitReached,
        Kind::Forbidden,
        Next::UserRecovery,
    ),
    (
        Site::ConstraintShapeInvalid,
        Code::MissingFrozenContract,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::FileDeliveryInputUnhonoured,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::FrozenContextTruncated,
        Code::MissingFrozenContract,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::FrozenContextMissing,
        Code::MissingFrozenContract,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::FrozenContextMalformed,
        Code::MissingFrozenContract,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::DeclarationMissing,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::DeclarationNotCurrent,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::DeclarationInvalid,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::ReleaseWithdrawn,
        Code::DeclarationWithdrawn,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::InnerConstraintShapeInvalid,
        Code::MissingFrozenContract,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::RouteOrAuthorChanged,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::FrozenTrackMissing,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::ContextMovedOutsideArea,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::FrozenReportMissing,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::FrozenBlockMissing,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::RootIdentityChanged,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::RootHashChanged,
        Code::ContractChanged,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::IsolatedAmbiguousOperations,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::OrdinaryWorkerPrepared,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::NewTask,
    ),
    (
        Site::VerificationEffectsWithoutWorker,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::NewTask,
    ),
    (
        Site::NotSpawnFailedWithoutStopProof,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::NewTask,
    ),
    (
        Site::OperationUncertainExternalEffects,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::NewTask,
    ),
    (
        Site::IsolatedRouteMismatch,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::IsolatedOperationNotThisExecution,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::IsolatedCompensationRecorded,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::IsolatedStopPending,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::WaitForSettlement,
    ),
    (
        Site::IsolatedOperationTerminalWithoutFailure,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::IsolatedRecordUnreadable,
        Code::PredecessorNotQuiescent,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::AllocationMissing,
        Code::RecoveryLineageMissing,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::PredecessorRowMissing,
        Code::RecoveryLineageMissing,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::ProvenanceUnsupported,
        Code::NotAuthorized,
        Kind::Conflict,
        Next::None,
    ),
    (
        Site::TrackNoLongerSchedules,
        Code::TrackNotReady,
        Kind::Conflict,
        Next::None,
    ),
];

/// Sites no fixture in this file can reach, each with its reason; the
/// set-equality assertion below subtracts exactly these from [`Site::ALL`].
/// Both sit behind `journal::load_tx` + `RunRecord::session()`, which need
/// a prepared `RunRecord` whose `PreparedEndpoint` carries the private
/// `LaunchConfig` only the dedicated-codex Controller writes; this file's
/// fixtures cannot forge one. They are driven at integration level with the
/// fake isolated backend's real receipt
/// (`isolated_codex_retry::stop_evidence_corruption_and_ambiguous_operations_refuse_retry`
/// mutates `/provider/record/stop` and the identity fields).
const UNREACHABLE: &[(Site, &str)] = &[
    (
        Site::IsolatedStopUnconfirmed,
        "needs a prepared run record whose stop state is not Quiesced; only the Controller writes \
         one (covered by isolated_codex_retry with the fake backend)",
    ),
    (
        Site::IsolatedStopIdentityMismatch,
        "needs a prepared run record with a Quiesced stop whose identity chain is then broken; \
         only the Controller writes one (covered by isolated_codex_retry with the fake backend)",
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
                    "UPDATE tracks SET lifecycle='done' WHERE id=?1",
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
                    (
                        user_owned.site,
                        user_owned.code,
                        user_owned.kind,
                        user_owned.continuation
                    ),
                    (
                        declare_and_wait.site,
                        declare_and_wait.code,
                        declare_and_wait.kind,
                        declare_and_wait.continuation
                    ),
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
        Site::FileDeliveryInputUnhonoured => {
            Box::pin(async {
                // A consumer claimed without its frozen input binding row:
                // `file_delivery::require_recovery_input_tx` refuses.
                let fx =
                    failed_initial_among(&[producer_declaration()], consumer_declaration()).await;
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
                // Neither view-consulted entry can trip this site:
                // `admit_contract_and_predecessor_tx` validates the same
                // constraint for the same Track first, and a stored allocation
                // is validated by `AllocationRow::decode` (Internal, not a
                // refusal) before `check_recovery_attempt_tx` reads it.
                // `validate_frozen_contract_tx` reaches it with the code erased
                // to `CalmError`. Drive the real function with a constraint
                // that parses but fails `validate()`.
                let fx = failed_initial(PLANNER).await;
                let pool = fx.repo.sqlite_pool().unwrap();
                let mut tx = begin_immediate_tx(&pool).await.unwrap();
                let track = crate::track_lifecycle::track_get_tx(
                    &mut tx,
                    &TrackId::from(fx.track_id.as_str()),
                )
                .await
                .unwrap();
                let constraint = calm_types::task_recovery::TaskRecoveryConstraint::V1 {
                    refs: Vec::new(),
                    spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
                    declared_by: PLANNER.into(),
                };
                refused(
                    site,
                    admission::check_constraint_tx(&mut tx, &track, "b", &constraint).await,
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
                // The shape `task_recovery_refuses_live_verifier_descendant_after_gate_exit`
                // produces: a gate ran, no worker card was ever prepared.
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
                // Admission only ever passes the keyed isolated operation's own
                // id; a settlement event naming another operation reaches the
                // fence directly.
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
                // The task failed while its operation is still running its
                // stop. (`parked` itself is CHECK-bound to a real run record;
                // `planner_observes_failure_then_settled_isolated_recovery`
                // drives that shape through the same site.)
                let fx = failed_initial_with(isolated_declaration()).await;
                insert_operation(
                    &fx,
                    crate::isolated_codex::OPERATION_KIND,
                    "spawn_succeeded",
                    Some("{}"),
                )
                .await;
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
        Site::IsolatedStopUnconfirmed | Site::IsolatedStopIdentityMismatch => {
            unreachable!("{site:?} is listed in UNREACHABLE and never driven")
        }
        Site::AllocationMissing => {
            Box::pin(async {
                // tasks(id,track_id,key) references the allocation, so an
                // existing row always has one; only an unknown attempt id
                // reaches this site.
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
            // Allocations are immutable (trigger) and the service
            // boundary only ever records User/Planner actors, so the
            // only way to this site is a successor allocation inserted
            // with a provenance admission would never have accepted.
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
                    "UPDATE tracks SET lifecycle='blocked' WHERE id=?1",
                    &[&fx.track_id],
                )
                .await;
                refused(site, check_attempt(&fx).await)
            })
            .await
        }
    }
}

/// Every row drives the real function and asserts what the site named; the
/// sites actually returned must be exactly `Site::ALL` minus `UNREACHABLE`,
/// so a site without a row (or a row that never reaches its site) is red.
#[tokio::test]
async fn every_refusal_site_names_its_code_kind_and_continuation() {
    let mut exercised = std::collections::BTreeSet::new();
    for &(site, code, kind, continuation) in ROWS {
        let refusal = Box::pin(drive(site)).await;
        assert_eq!(
            (
                refusal.site,
                refusal.code,
                refusal.kind,
                refusal.continuation
            ),
            (site, code, kind, continuation),
            "{site:?} named {refusal:?}"
        );
        assert!(
            exercised.insert(refusal.site),
            "{site:?} has more than one row"
        );
    }
    let expected: std::collections::BTreeSet<Site> = Site::ALL
        .iter()
        .copied()
        .filter(|site| {
            !UNREACHABLE
                .iter()
                .any(|(unreachable, _)| unreachable == site)
        })
        .collect();
    assert_eq!(
        exercised, expected,
        "every production refusal site needs exactly one row in ROWS, or an UNREACHABLE entry with its reason"
    );
    for (site, reason) in UNREACHABLE {
        assert!(Site::ALL.contains(site), "{site:?} is not a site");
        assert!(!reason.is_empty(), "{site:?} needs its reason");
    }
}

/// `Site::ALL` is exhaustive: the wildcard-free match below does not compile
/// once a variant exists that it does not map, and its index must be the
/// variant's position in `ALL`, so a variant added to the enum (or to the
/// match) without an `ALL` entry is red.
#[test]
fn refusal_site_all_lists_every_variant_exactly_once() {
    fn position(site: Site) -> usize {
        match site {
            Site::ActorNotUserOrPlanner => 0,
            Site::PlannerSessionUnresolved => 1,
            Site::TrackNotReady => 2,
            Site::ChildTaskRoute => 3,
            Site::PlannerOutsideAutoDeclare => 4,
            Site::PlannerRetryLimit => 5,
            Site::ConstraintShapeInvalid => 6,
            Site::FileDeliveryInputUnhonoured => 7,
            Site::FrozenContextTruncated => 8,
            Site::FrozenContextMissing => 9,
            Site::FrozenContextMalformed => 10,
            Site::DeclarationMissing => 11,
            Site::DeclarationNotCurrent => 12,
            Site::DeclarationInvalid => 13,
            Site::ReleaseWithdrawn => 14,
            Site::InnerConstraintShapeInvalid => 15,
            Site::RouteOrAuthorChanged => 16,
            Site::FrozenTrackMissing => 17,
            Site::ContextMovedOutsideArea => 18,
            Site::FrozenReportMissing => 19,
            Site::FrozenBlockMissing => 20,
            Site::RootIdentityChanged => 21,
            Site::RootHashChanged => 22,
            Site::IsolatedAmbiguousOperations => 23,
            Site::OrdinaryWorkerPrepared => 24,
            Site::VerificationEffectsWithoutWorker => 25,
            Site::NotSpawnFailedWithoutStopProof => 26,
            Site::OperationUncertainExternalEffects => 27,
            Site::IsolatedRouteMismatch => 28,
            Site::IsolatedOperationNotThisExecution => 29,
            Site::IsolatedCompensationRecorded => 30,
            Site::IsolatedStopPending => 31,
            Site::IsolatedOperationTerminalWithoutFailure => 32,
            Site::IsolatedRecordUnreadable => 33,
            Site::IsolatedStopUnconfirmed => 34,
            Site::IsolatedStopIdentityMismatch => 35,
            Site::AllocationMissing => 36,
            Site::PredecessorRowMissing => 37,
            Site::ProvenanceUnsupported => 38,
            Site::TrackNoLongerSchedules => 39,
        }
    }
    const VARIANTS: usize = 40;
    assert_eq!(Site::ALL.len(), VARIANTS);
    for (index, site) in Site::ALL.iter().enumerate() {
        assert_eq!(position(*site), index, "{site:?} is misplaced in ALL");
    }
}

/// The wire spellings are a published vocabulary: every variant is distinct
/// and the pre-existing strings are byte-identical to what `capability_code`
/// used to emit.
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
