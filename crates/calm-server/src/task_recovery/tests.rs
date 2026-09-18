//! Every admission refusal site, driven through the real admission function
//! with a fixture that trips exactly that site, asserts its typed code. No
//! assertion here reads the message text.
use super::admission;
use super::launch_test_support::{RecoveryFixture, initial_claimed_task, recovered_claimed_task};
use super::refusal::{AdmissionError, RecoveryRefusalCode as Code, RefusalKind as Kind};
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

fn declaration(declared_by: &str) -> Value {
    json!({"key":"b","kind":"terminal","command":"true","ready":true,"declared_by":declared_by})
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
    let (repo, events, write, track_id) = open().await;
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let fixture =
        initial_claimed_task(repo_dyn, events, write, &track_id, declaration("user")).await;
    let pool = repo.sqlite_pool().unwrap();
    if declared_by != "user" {
        let block_id = fixture.block_id().to_string();
        edit_report_crdt(&pool, &track_id, |doc| {
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
            .bind(&fixture.task.id)
            .execute(&pool)
            .await
            .unwrap();
    }
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

async fn insert_operation(fx: &Fx, kind: &str, phase: &str, tx_output: Option<&str>) {
    let pool = fx.repo.sqlite_pool().unwrap();
    sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_id,target_json,payload_json,phase,tx_output_json,created_at_ms,updated_at_ms) VALUES(?1,?1,?2,?3,'h','track',?4,'{}','{}',?5,?6,1,1)")
        .bind(format!("op-{kind}-{phase}"))
        .bind(kind)
        .bind(&fx.fixture.task.id)
        .bind(&fx.track_id)
        .bind(phase)
        .bind(tx_output)
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

fn refused<T: std::fmt::Debug>(site: &str, outcome: Result<T, AdmissionError>) -> (Code, Kind) {
    match outcome {
        Err(AdmissionError::Refused(refusal)) => (refusal.code, refusal.kind),
        other => panic!("{site}: expected a typed refusal, got {other:?}"),
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

#[tokio::test]
async fn every_refusal_site_names_its_typed_code() {
    let planner = || ActorId::AiPlanner("planner".into());
    let user = || ActorId::User;
    let mut seen = Vec::new();
    for site in [
        "authorize_tx: actor is not a User or Planner",
        "authorize_tx: Planner session cannot be resolved",
        "recovery_policy: track lifecycle does not schedule",
        "recovery_policy: child-task route",
        "recovery_policy: Planner asks for a user-owned declaration",
        "recovery_policy: Planner asks under declare-and-wait",
        "recovery_policy: Planner retry limit consumed",
        "claim_constraint_tx: frozen context truncated",
        "claim_constraint_tx: no frozen context",
        "claim_constraint_tx: malformed frozen context",
        "admit_recovery_tx: constraint shape invalid (no root reference)",
        "declaration_tx: declaration missing for key",
        "declaration_tx: declaration not ready",
        "declaration_tx: declaration has validation errors",
        "declaration_tx: declare-and-wait release withdrawn",
        "check_constraint_tx: route or author changed",
        "check_constraint_tx: frozen context track missing",
        "check_constraint_tx: context moved outside its area",
        "check_constraint_tx: frozen context report missing",
        "check_constraint_tx: frozen context block missing",
        "check_constraint_tx: root declaration identity changed",
        "check_constraint_tx: root hash changed",
        "require_recoverable_predecessor_tx: isolated with ambiguous verification effects",
        "isolated_codex::recovery::require_stopped_tx: no confirmed namespace stop",
        "require_recoverable_predecessor_tx: ordinary worker was prepared",
        "require_recoverable_predecessor_tx: failure was not a spawn failure",
        "require_recoverable_predecessor_tx: operation has uncertain external effects",
        "check_recovery_attempt_tx: allocation missing",
        "check_recovery_attempt_tx: predecessor row missing",
        "check_recovery_attempt_tx: accepted actor provenance unsupported",
        "check_recovery_attempt_tx: track no longer schedules",
    ] {
        let (code, kind) = match site {
            "authorize_tx: actor is not a User or Planner" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    refused(site, authorize(&fx, ActorId::KernelDispatcher).await)
                })
                .await
            }
            "authorize_tx: Planner session cannot be resolved" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    refused(
                        site,
                        authorize(&fx, ActorId::AiPlannerSession("missing-session".into())).await,
                    )
                })
                .await
            }
            "recovery_policy: track lifecycle does not schedule" => {
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
            "recovery_policy: child-task route" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    sql(
                        &fx,
                        "UPDATE tasks SET spawn='sub-wave' WHERE id=?1",
                        &[&fx.fixture.task.id],
                    )
                    .await;
                    refused(site, admit(&fx, user(), 1).await)
                })
                .await
            }
            "recovery_policy: Planner asks for a user-owned declaration" => {
                Box::pin(async {
                    let fx = failed_initial("user").await;
                    refused(site, admit(&fx, planner(), 1).await)
                })
                .await
            }
            "recovery_policy: Planner asks under declare-and-wait" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    sql(
                        &fx,
                        "UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1",
                        &[&fx.track_id],
                    )
                    .await;
                    refused(site, admit(&fx, planner(), 1).await)
                })
                .await
            }
            "recovery_policy: Planner retry limit consumed" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    refused(site, admit(&fx, planner(), 2).await)
                })
                .await
            }
            "claim_constraint_tx: frozen context truncated" => {
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
            "claim_constraint_tx: no frozen context" => {
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
            "claim_constraint_tx: malformed frozen context" => {
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
            "admit_recovery_tx: constraint shape invalid (no root reference)" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    set_frozen_refs(&fx, Vec::new()).await;
                    refused(site, admit(&fx, user(), 1).await)
                })
                .await
            }
            "declaration_tx: declaration missing for key" => {
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
            "declaration_tx: declaration not ready" => {
                Box::pin(async {
                    let fx = failed_initial("user").await;
                    fx.fixture.withdraw().await;
                    refused(site, admit(&fx, user(), 1).await)
                })
                .await
            }
            "declaration_tx: declaration has validation errors" => {
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
            "declaration_tx: declare-and-wait release withdrawn" => {
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
            "check_constraint_tx: route or author changed" => {
                Box::pin(async {
                    let fx = failed_initial("user").await;
                    sql(
                        &fx,
                        "UPDATE tasks SET declared_by='spec' WHERE id=?1",
                        &[&fx.fixture.task.id],
                    )
                    .await;
                    refused(site, admit(&fx, user(), 1).await)
                })
                .await
            }
            "check_constraint_tx: frozen context track missing" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    let mut refs = frozen_refs(&fx).await;
                    refs.push(foreign_ref("no-such-track", "b_missing"));
                    set_frozen_refs(&fx, refs).await;
                    refused(site, admit(&fx, user(), 1).await)
                })
                .await
            }
            "check_constraint_tx: context moved outside its area" => {
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
            "check_constraint_tx: frozen context report missing" => {
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
            "check_constraint_tx: frozen context block missing" => {
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
            "check_constraint_tx: root declaration identity changed" => {
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
            "check_constraint_tx: root hash changed" => {
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
            "require_recoverable_predecessor_tx: isolated with ambiguous verification effects" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
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
            "isolated_codex::recovery::require_stopped_tx: no confirmed namespace stop" => {
                Box::pin(async {
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
            "require_recoverable_predecessor_tx: ordinary worker was prepared" => {
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
            "require_recoverable_predecessor_tx: failure was not a spawn failure" => {
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
            "require_recoverable_predecessor_tx: operation has uncertain external effects" => {
                Box::pin(async {
                    let fx = failed_initial(PLANNER).await;
                    insert_operation(&fx, "terminal-worker", "spawn_started", None).await;
                    refused(site, admit(&fx, user(), 1).await)
                })
                .await
            }
            "check_recovery_attempt_tx: allocation missing" => {
                Box::pin(async {
                    // tasks(id,track_id,key) references the allocation, so an
                    // existing row always has one; only an unknown attempt id
                    // reaches this site.
                    let fx = recovered().await;
                    refused(site, check_attempt_for(&fx, "no-such-attempt").await)
                })
                .await
            }
            "check_recovery_attempt_tx: predecessor row missing" => {
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
            "check_recovery_attempt_tx: accepted actor provenance unsupported" => {
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
            "check_recovery_attempt_tx: track no longer schedules" => {
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
            other => unreachable!("unlisted site {other}"),
        };
        seen.push((site, code, kind));
    }
    let expected: Vec<(&str, Code, Kind)> = vec![
        (
            "authorize_tx: actor is not a User or Planner",
            Code::NotAuthorized,
            Kind::Forbidden,
        ),
        (
            "authorize_tx: Planner session cannot be resolved",
            Code::NotAuthorized,
            Kind::Forbidden,
        ),
        (
            "recovery_policy: track lifecycle does not schedule",
            Code::TrackNotReady,
            Kind::Conflict,
        ),
        (
            "recovery_policy: child-task route",
            Code::UnsupportedSpawn,
            Kind::Conflict,
        ),
        (
            "recovery_policy: Planner asks for a user-owned declaration",
            Code::UserAuthorizationRequired,
            Kind::Forbidden,
        ),
        (
            "recovery_policy: Planner asks under declare-and-wait",
            Code::UserAuthorizationRequired,
            Kind::Forbidden,
        ),
        (
            "recovery_policy: Planner retry limit consumed",
            Code::RecoveryLimitReached,
            Kind::Forbidden,
        ),
        (
            "claim_constraint_tx: frozen context truncated",
            Code::MissingFrozenContract,
            Kind::Conflict,
        ),
        (
            "claim_constraint_tx: no frozen context",
            Code::MissingFrozenContract,
            Kind::Conflict,
        ),
        (
            "claim_constraint_tx: malformed frozen context",
            Code::MissingFrozenContract,
            Kind::Conflict,
        ),
        (
            "admit_recovery_tx: constraint shape invalid (no root reference)",
            Code::MissingFrozenContract,
            Kind::Conflict,
        ),
        (
            "declaration_tx: declaration missing for key",
            Code::DeclarationWithdrawn,
            Kind::Conflict,
        ),
        (
            "declaration_tx: declaration not ready",
            Code::DeclarationWithdrawn,
            Kind::Conflict,
        ),
        (
            "declaration_tx: declaration has validation errors",
            Code::DeclarationWithdrawn,
            Kind::Conflict,
        ),
        (
            "declaration_tx: declare-and-wait release withdrawn",
            Code::DeclarationWithdrawn,
            Kind::Conflict,
        ),
        (
            "check_constraint_tx: route or author changed",
            Code::ContractChanged,
            Kind::Conflict,
        ),
        (
            "check_constraint_tx: frozen context track missing",
            Code::ContractChanged,
            Kind::Conflict,
        ),
        (
            "check_constraint_tx: context moved outside its area",
            Code::ContractChanged,
            Kind::Conflict,
        ),
        (
            "check_constraint_tx: frozen context report missing",
            Code::ContractChanged,
            Kind::Conflict,
        ),
        (
            "check_constraint_tx: frozen context block missing",
            Code::ContractChanged,
            Kind::Conflict,
        ),
        (
            "check_constraint_tx: root declaration identity changed",
            Code::ContractChanged,
            Kind::Conflict,
        ),
        (
            "check_constraint_tx: root hash changed",
            Code::ContractChanged,
            Kind::Conflict,
        ),
        (
            "require_recoverable_predecessor_tx: isolated with ambiguous verification effects",
            Code::PredecessorNotQuiescent,
            Kind::Conflict,
        ),
        (
            "isolated_codex::recovery::require_stopped_tx: no confirmed namespace stop",
            Code::PredecessorNotQuiescent,
            Kind::Conflict,
        ),
        (
            "require_recoverable_predecessor_tx: ordinary worker was prepared",
            Code::PredecessorNotQuiescent,
            Kind::Conflict,
        ),
        (
            "require_recoverable_predecessor_tx: failure was not a spawn failure",
            Code::PredecessorNotQuiescent,
            Kind::Conflict,
        ),
        (
            "require_recoverable_predecessor_tx: operation has uncertain external effects",
            Code::PredecessorNotQuiescent,
            Kind::Conflict,
        ),
        (
            "check_recovery_attempt_tx: allocation missing",
            Code::RecoveryLineageMissing,
            Kind::Conflict,
        ),
        (
            "check_recovery_attempt_tx: predecessor row missing",
            Code::RecoveryLineageMissing,
            Kind::Conflict,
        ),
        (
            "check_recovery_attempt_tx: accepted actor provenance unsupported",
            Code::NotAuthorized,
            Kind::Conflict,
        ),
        (
            "check_recovery_attempt_tx: track no longer schedules",
            Code::TrackNotReady,
            Kind::Conflict,
        ),
    ];
    assert_eq!(seen, expected);
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
