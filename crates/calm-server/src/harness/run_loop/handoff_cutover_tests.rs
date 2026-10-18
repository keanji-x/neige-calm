//! Exercise the actual registered transaction operation, never a copy of the SQL policy.
use super::completed_commit_tests::Fixture;
use super::handoff_pause_tests::{request, system};
use super::*;
use crate::codex_appserver::Notification;
use crate::db::prelude::*;
use crate::model::CardRole;
use crate::operation::OperationRuntime;
use crate::operation::handoff_cutover::{self, CutoverScope, ValidatedCutoverPlan};

fn runtime(f: &Fixture) -> Arc<OperationRuntime> {
    runtime_for_repo(f.repo.clone())
}

pub(super) fn runtime_for_repo(repo: Arc<crate::db::sqlite::SqlxRepo>) -> Arc<OperationRuntime> {
    let repo: Arc<dyn crate::db::Repo> = repo;
    let state = crate::state::AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(crate::state::DaemonClient::new_stub()),
        Arc::new(crate::plugin_host::PluginHost::new_full(
            Arc::new(crate::plugin_host::PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("handoff-plugins-data"),
            Vec::new(),
            EventBus::new(),
            crate::state::WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        )),
        Arc::new(crate::state::CodexClient::new_stub()),
        None,
        None,
    );
    assert!(
        state
            .operation_runtime
            .registered_adapter_kinds()
            .any(|kind| kind == "handoff-cutover")
    );
    state.operation_runtime.clone()
}

/// Data-only before-image of the actual SQLite store, including task/worker/claim/gate rows.
/// Discover columns from SQLite so later schema additions cannot silently escape the oracle.
pub(super) async fn data_image(pool: &sqlx::SqlitePool) -> Vec<(String, Vec<String>)> {
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .fetch_all(pool).await.unwrap();
    let mut image = vec![];
    for table in tables {
        let identifier = format!("\"{}\"", table.replace('"', "\"\""));
        let columns: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info({identifier})"))
                .fetch_all(pool)
                .await
                .unwrap();
        let expressions = columns
            .iter()
            .map(|column| format!("quote(\"{}\")", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(",");
        let mut rows: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT json_array({expressions}) FROM {identifier}"
        ))
        .fetch_all(pool)
        .await
        .unwrap();
        rows.sort();
        image.push((table, rows));
    }
    image
}

pub(super) async fn plan(a: &Fixture, b: &Fixture) -> ValidatedCutoverPlan {
    let source_pause = a.harness.pause_for_handoff(request(a)).await.unwrap();
    let target_pause = b.harness.pause_for_handoff(request(b)).await.unwrap();
    handoff_cutover::precheck(
        a.repo.pool(),
        CutoverScope {
            track_id: a.harness.inner.track_id.clone(),
            source_card_id: a.harness.inner.card_id.clone(),
            source_session_id: a.harness.inner.worker_session_id.clone(),
            target_card_id: b.harness.inner.card_id.clone(),
            target_session_id: b.harness.inner.worker_session_id.clone(),
            source_pause,
            target_pause,
            plan_id: crate::model::new_id(),
            new_session_id: crate::model::new_id(),
            user_audit_id: "user-approved-exact-scope".into(),
        },
    )
    .await
    .unwrap()
}

pub(super) async fn commit(
    a: &Fixture,
    plan: ValidatedCutoverPlan,
) -> Result<handoff_cutover::CutoverResult> {
    handoff_cutover::commit(&runtime(a), &a.harness.inner.card_role_cache, plan).await
}

#[tokio::test]
async fn handoff_cutover_transfers_complete_queue_and_revokes_both_old_sessions() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    for (f, token) in [(&a, "source-token"), (&b, "target-token")] {
        sqlx::query("UPDATE worker_sessions SET mcp_token_hash=?1 WHERE id=?2")
            .bind(token)
            .bind(&f.harness.inner.worker_session_id)
            .execute(a.repo.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO card_mcp_tokens(card_id,hashed_token,created_at) VALUES(?1,?2,0)")
            .bind(f.harness.inner.card_id.as_str())
            .bind(token)
            .execute(a.repo.pool())
            .await
            .unwrap();
    }
    *a.harness.inner.last_report_body_sha256.lock().await = Some("retained-report-dedupe".into());
    let context = serde_json::to_value(crate::template_context::TemplateContext::new(
        "method".into(),
        "retained instructions".into(),
    ))
    .unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.template_context',json(?1)) WHERE id=?2",
    )
    .bind(serde_json::to_string(&context).unwrap())
    .bind(a.harness.inner.card_id.as_str())
    .execute(a.repo.pool())
    .await
    .unwrap();
    sqlx::query(concat!(
        "UPDATE cards SET payload=json_set(payload,'$.model','chosen-model',",
        "'$.model_ever_set',json('true'),'$.reasoning_effort','high',",
        "'$.reasoning_effort_ever_set',json('true')) WHERE id=?1",
    ))
    .bind(b.harness.inner.card_id.as_str())
    .execute(a.repo.pool())
    .await
    .unwrap();
    system(&a, 4).await;
    system(&a, 9).await;
    let item_id = b
        .repo
        .harness_item_insert(
            &b.harness.inner.worker_session_id,
            b.harness.inner.card_id.as_str(),
            b.harness.inner.track_id.as_str(),
            "thread-commit-wake",
            Some("old-turn"),
            Some("retained-item"),
            Some("agentMessage"),
            "item/completed",
            "{}",
            None,
        )
        .await
        .unwrap();
    let transcript_before: String = sqlx::query_scalar("SELECT json_object('id',id,'params',params,'item',item_uuid,'worker',worker_session_id) FROM harness_items WHERE id=?1")
        .bind(item_id).fetch_one(a.repo.pool()).await.unwrap();
    let worker = a.sibling(CardRole::Worker).await;
    sqlx::query(
        "UPDATE worker_sessions SET state='running',active_turn_id='busy-worker-turn' WHERE id=?1",
    )
    .bind(&worker.harness.inner.worker_session_id)
    .execute(a.repo.pool())
    .await
    .unwrap();
    let worker_before = worker
        .repo
        .session_get_by_id(&worker.harness.inner.worker_session_id.as_str().into())
        .await
        .unwrap();
    let plan = plan(&a, &b).await;
    let pending = a.stored().await.pending_entries();
    let result = commit(&a, plan.clone()).await.unwrap();
    assert_eq!(commit(&a, plan.clone()).await.unwrap(), result);
    let new = a
        .repo
        .session_get_by_id(&plan.scope.new_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    let fresh = HarnessSnapshot::parse_known(new.handle_state_json.unwrap()).unwrap();
    assert_eq!(fresh.pending_entries(), pending);
    assert_eq!(fresh.push_watermark, 9);
    assert_eq!(
        fresh.last_report_body_sha256.as_deref(),
        Some("retained-report-dedupe")
    );
    let card = a
        .repo
        .card_get(plan.scope.target_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card.payload["template_context"], context);
    assert_eq!(card.payload["model"], "chosen-model");
    assert_eq!(card.payload["reasoning_effort"], "high");
    assert_eq!(card.payload["model_ever_set"], true);
    assert_eq!(card.payload["reasoning_effort_ever_set"], true);
    assert_eq!(card.payload["permission_mode"], "never");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT root_session_id FROM tracks WHERE id=?1")
            .bind(plan.scope.track_id.as_str())
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
        plan.scope.new_session_id
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT session_id FROM cards WHERE id=?1")
            .bind(plan.scope.target_card_id.as_str())
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
        plan.scope.new_session_id
    );
    let history = a
        .repo
        .card_get(plan.scope.source_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(history.payload.get("planner_harness").is_none());
    assert!(history.payload.get("planner_history").is_some());
    assert!(
        crate::harness::profile::PlannerBinding::from_card(&history, CardRole::PlannerHistory)
            .is_none()
    );
    // A phase-2 replay carries its original Planner profile, sampled before retirement.
    assert!(
        crate::harness::profile::PlannerBinding::from_card(&history, CardRole::Planner).is_none()
    );
    assert!(
        sqlx::query("UPDATE cards SET title='late admitted mutation' WHERE id=?1")
            .bind(history.id.as_str())
            .execute(a.repo.pool())
            .await
            .is_err()
    );
    assert_eq!(
        fresh.handoff_pause,
        crate::harness::handoff_pause::PauseState::Unpaused
    );
    assert_eq!(
        new.state,
        crate::session_projection_repo::WorkerSessionState::PendingActivation
    );
    assert!(!new.state.is_active_authority());
    assert!(new.thread_id.is_none());
    assert!(new.agent_session_id.is_none());
    assert!(new.mcp_token_hash.is_none());
    assert_eq!(new.card_id, Some(plan.scope.target_card_id.clone()));
    assert_eq!(new.contract, calm_types::worker::WorkerContract::Planner);
    assert_eq!(
        a.repo
            .card_role_get(a.harness.inner.card_id.as_str())
            .await
            .unwrap(),
        Some(CardRole::PlannerHistory)
    );
    assert_eq!(
        a.repo
            .card_role_get(b.harness.inner.card_id.as_str())
            .await
            .unwrap(),
        Some(CardRole::Planner)
    );
    assert_eq!(sqlx::query_scalar::<_,String>("SELECT json_object('id',id,'params',params,'item',item_uuid,'worker',worker_session_id) FROM harness_items WHERE id=?1")
        .bind(item_id).fetch_one(a.repo.pool()).await.unwrap(), transcript_before);
    assert_eq!(
        worker
            .repo
            .session_get_by_id(&worker.harness.inner.worker_session_id.as_str().into())
            .await
            .unwrap(),
        worker_before
    );
    for f in [&a, &b] {
        let row = f
            .repo
            .session_get_by_id(&f.harness.inner.worker_session_id.as_str().into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.state,
            crate::session_projection_repo::WorkerSessionState::Superseded
        );
        assert!(row.mcp_token_hash.is_none());
        assert!(
            HarnessSnapshot::parse_known(row.handle_state_json.unwrap())
                .unwrap()
                .pending_entries()
                .is_empty()
        );
        let query = crate::db::handoff_pause::query_handoff_pause(
            f.repo.as_ref(),
            if f.harness.inner.card_id == plan.scope.source_card_id {
                plan.scope.source_pause.request().clone()
            } else {
                plan.scope.target_pause.request().clone()
            },
        )
        .await
        .unwrap();
        let crate::harness::handoff_pause::PauseQuery::Recorded(
            crate::harness::handoff_pause::PauseState::ConsumedByHandoff {
                plan_id: consumed_plan,
                operation_id: consumed_op,
                new_card_id,
                new_session_id,
                ..
            },
        ) = query
        else {
            panic!("original receipt must retain its handoff disposition")
        };
        let handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
            operation_id: committed_op,
            ..
        } = &result;
        assert_eq!(&consumed_op, committed_op);
        assert_eq!(consumed_plan, plan.scope.plan_id);
        assert_eq!(new_card_id, plan.scope.target_card_id);
        assert_eq!(new_session_id, plan.scope.new_session_id);
        assert!(
            f.repo
                .harness_item_insert(
                    &f.harness.inner.worker_session_id,
                    f.harness.inner.card_id.as_str(),
                    f.harness.inner.track_id.as_str(),
                    "thread-commit-wake",
                    Some("late-turn"),
                    Some("pre-admitted-item"),
                    Some("agentMessage"),
                    "item/completed",
                    "{}",
                    None
                )
                .await
                .is_err()
        );
        assert!(
            f.harness
                .observe_user_message_durable(
                    "old admitted".into(),
                    vec![],
                    SendKey::unique_for_test()
                )
                .await
                .is_err()
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM card_mcp_tokens")
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
        0
    );
    for token in ["source-token", "target-token"] {
        assert!(
            crate::db::sqlite::session_get_by_active_token_hash(a.repo.pool(), token)
                .await
                .unwrap()
                .is_none()
        );
    }
    a.issue().await;
    b.issue().await;
    assert_eq!(a.daemon.turn_start_count_for_test(), 0);
    assert_eq!(b.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn handoff_cutover_revalidates_fault_and_claim_without_any_writes() {
    for defect in [
        "fault",
        "target_fault",
        "claim",
        "released",
        "recovered",
        "config",
        "target_user",
        "source_user",
        "target_system",
        "acp_unknown",
        "closed",
        "current",
        "provider",
    ] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let mut plan = plan(&a, &b).await;
        match defect {
            "target_fault" => {
                handoff_pause::record_backend_fault(
                    &b.harness.inner,
                    crate::harness::handoff_pause::BackendFault::ThreadSystemError,
                )
                .await
                .unwrap();
            }
            "closed" => {
                sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
                    .bind(plan.scope.track_id.as_str())
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            "current" => {
                sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
                    .bind(plan.scope.target_card_id.as_str())
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            "provider" => {
                sqlx::query("UPDATE cards SET payload=json_set(payload,'$.assistant_provider','claude') WHERE id=?1").bind(plan.scope.target_card_id.as_str()).execute(a.repo.pool()).await.unwrap();
            }
            "acp_unknown" => {
                sqlx::query(concat!(
                    "INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,",
                    "native_session_id,input_json,state,created_at_ms) ",
                    "VALUES(?1,'c','t','th','native','{}','unknown',0)",
                ))
                .bind(&plan.scope.source_session_id)
                .execute(a.repo.pool())
                .await
                .unwrap();
            }
            "target_system" | "source_user" => {
                let (f, entries) = if defect == "target_system" {
                    (
                        &b,
                        vec![
                            QueueEntry::system(
                                Observation::TrackGoal {
                                    text: "cannot discard".into(),
                                },
                                Some(3),
                            )
                            .unwrap(),
                        ],
                    )
                } else {
                    (
                        &a,
                        vec![QueueEntry::user_message(
                            "cannot discard".into(),
                            None,
                            vec![],
                        )],
                    )
                };
                let mut snapshot = f.stored().await;
                snapshot.set_pending_entries(entries);
                sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
                    .bind(serde_json::to_string(&snapshot).unwrap())
                    .bind(&f.harness.inner.worker_session_id)
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            "fault" => {
                handoff_pause::record_backend_fault(
                    &a.harness.inner,
                    crate::harness::handoff_pause::BackendFault::ThreadSystemError,
                )
                .await
                .unwrap();
            }
            "claim" => {
                plan.scope.source_pause =
                    crate::harness::handoff_pause::PauseReceipt::from_request(request(&a));
            }
            "released" => {
                assert_eq!(
                    a.harness
                        .resume_from_handoff(plan.scope.source_pause.clone())
                        .await
                        .unwrap(),
                    crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
                );
            }
            "recovered" => {
                let mut snapshot = a.stored().await;
                snapshot.handoff_pause.unconfirm_recovered_health();
                sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
                    .bind(serde_json::to_string(&snapshot).unwrap())
                    .bind(&plan.scope.source_session_id)
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            "config" => {
                sqlx::query(
                    "UPDATE cards SET payload=json_set(payload,'$.model','changed') WHERE id=?1",
                )
                .bind(plan.scope.target_card_id.as_str())
                .execute(a.repo.pool())
                .await
                .unwrap();
            }
            "target_user" => {
                let mut snapshot = b.stored().await;
                snapshot.set_pending_entries(vec![QueueEntry::user_message(
                    "never lose".into(),
                    None,
                    vec![],
                )]);
                sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
                    .bind(serde_json::to_string(&snapshot).unwrap())
                    .bind(&plan.scope.target_session_id)
                    .execute(a.repo.pool())
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = data_image(a.repo.pool()).await;
        assert!(commit(&a, plan).await.is_err(), "{defect}");
        assert_eq!(data_image(a.repo.pool()).await, before, "{defect}");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM operations WHERE kind='handoff-cutover'"
            )
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn handoff_cutover_rollback_every_mutation_stage() {
    for trigger in [
        "BEFORE UPDATE OF handle_state_json ON worker_sessions",
        "BEFORE UPDATE OF state ON worker_sessions",
        "BEFORE UPDATE OF mcp_token_hash ON worker_sessions",
        "BEFORE UPDATE OF role ON cards WHEN NEW.role='planner_history'",
        "BEFORE UPDATE OF role ON cards WHEN NEW.role='planner'",
        "BEFORE INSERT ON worker_sessions",
        "BEFORE UPDATE OF root_session_id ON tracks",
        "BEFORE UPDATE OF session_id ON cards",
        "BEFORE UPDATE OF phase ON operations WHEN NEW.phase='succeeded'",
    ] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let plan = plan(&a, &b).await;
        let source = a.stored().await;
        let target = b.stored().await;
        sqlx::query(&format!("CREATE TRIGGER fail_cutover {trigger} BEGIN SELECT RAISE(ABORT,'cutover rollback probe'); END"))
            .execute(a.repo.pool()).await.unwrap();
        let before = data_image(a.repo.pool()).await;
        assert!(commit(&a, plan.clone()).await.is_err(), "{trigger}");
        assert_eq!(data_image(a.repo.pool()).await, before, "{trigger}");
        assert_eq!(a.stored().await, source, "{trigger}");
        assert_eq!(b.stored().await, target, "{trigger}");
        assert_eq!(
            a.harness
                .inner
                .card_role_cache
                .get(&plan.scope.source_card_id),
            Some(CardRole::Planner)
        );
        assert_eq!(
            a.harness
                .inner
                .card_role_cache
                .get(&plan.scope.target_card_id),
            Some(CardRole::Assistant)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM operations WHERE kind='handoff-cutover'"
            )
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
            0
        );
        sqlx::query("DROP TRIGGER fail_cutover")
            .execute(a.repo.pool())
            .await
            .unwrap();
        commit(&a, plan).await.unwrap();
    }
}

#[tokio::test]
async fn handoff_cutover_concurrent_same_key_and_changed_scope_replay() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let (left, right) = tokio::join!(commit(&a, plan.clone()), commit(&a, plan.clone()));
    assert_eq!(left.unwrap(), right.unwrap());
    let mut changed = plan;
    changed.scope.new_session_id = crate::model::new_id();
    assert!(commit(&a, changed).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM operations WHERE kind='handoff-cutover'"
        )
        .fetch_one(a.repo.pool())
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn handoff_cutover_lost_answer_reopens_original_operation_without_recutting() {
    let temp = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        temp.path().join("handoff.sqlite").display()
    );
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let scope = plan.scope.clone();
    let (answer, receiver) = tokio::sync::oneshot::channel();
    drop(receiver); // Reply is already lost while the accepted SQL owner continues.
    let expected = answer
        .send(commit(&a, plan.clone()).await.unwrap())
        .unwrap_err();
    drop(b);
    drop(a);
    let repo = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    let cache = crate::card_role_cache::CardRoleCache::new();
    let replay = handoff_cutover::commit(&runtime_for_repo(repo.clone()), &cache, plan)
        .await
        .unwrap();
    assert_eq!(replay, expected);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM operations WHERE kind='handoff-cutover'"
        )
        .fetch_one(repo.pool())
        .await
        .unwrap(),
        1
    );
    for receipt in [&scope.source_pause, &scope.target_pause] {
        assert!(matches!(
            crate::db::handoff_pause::query_handoff_pause(repo.as_ref(), receipt.request().clone())
                .await
                .unwrap(),
            crate::harness::handoff_pause::PauseQuery::Recorded(
                crate::harness::handoff_pause::PauseState::ConsumedByHandoff { .. }
            )
        ));
    }
}

#[tokio::test]
async fn handoff_cutover_auth_hold_after_pause_commits_only_inert_identity() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    // Exercise the real authentication owner via its public notification seam.
    a.daemon
        .emit_notification_for_test(Notification::TurnStarted {
            thread_id: "other-conversation".into(),
            turn: serde_json::json!({"id":"auth-failure"}),
        });
    a.daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: "other-conversation".into(), turn: serde_json::json!({"id":"auth-failure","status":"failed","error":{"codexErrorInfo":"unauthorized"}}),
    });
    let hold = a
        .daemon
        .authentication_hold()
        .expect("real auth owner holds issuance");
    let result = commit(&a, plan.clone()).await.unwrap();
    assert!(matches!(
        result,
        handoff_cutover::CutoverResult::CutoverCommittedPendingActivation { .. }
    ));
    assert_eq!(a.daemon.authentication_hold(), Some(hold));
    let card = a
        .repo
        .card_get(plan.scope.target_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        crate::harness::profile::checked_card_binding(a.repo.as_ref(), &card, CardRole::Planner)
            .await
            .is_err()
    );
    let runtime = a
        .repo
        .session_projection_by_id(&plan.scope.new_session_id)
        .await
        .unwrap()
        .unwrap();
    let registry = crate::harness::HarnessRegistry::new();
    let outcome = crate::harness::spawn_recovered_harness(
        a.repo.clone(),
        a.harness.inner.events.clone(),
        a.harness.inner.card_role_cache.clone(),
        a.harness.inner.track_area_cache.clone(),
        a.daemon.clone(),
        a.daemon.thread_seals().clone(),
        &crate::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(a.repo.clone()),
        &registry,
        &crate::per_card_lock::new_keyed_locks(),
        runtime,
        crate::harness::ClaimMode::Replace,
    )
    .await
    .unwrap();
    assert!(outcome.installed().is_none());
    assert!(
        sqlx::query("UPDATE worker_sessions SET state='starting' WHERE id=?1")
            .bind(&plan.scope.new_session_id)
            .execute(a.repo.pool())
            .await
            .is_err()
    );
    assert_eq!(a.daemon.turn_start_count_for_test(), 0);
    let row = a
        .repo
        .session_get_by_id(&plan.scope.new_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert!(row.thread_id.is_none() && row.mcp_token_hash.is_none());
}

#[tokio::test]
async fn handoff_cutover_competing_operation_has_one_identity_winner() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let first = plan(&a, &b).await;
    let mut other = first.clone();
    other.scope.plan_id = crate::model::new_id();
    other.scope.new_session_id = crate::model::new_id();
    let (left, right) = tokio::join!(commit(&a, first), commit(&a, other));
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM worker_sessions WHERE state='pending_activation'"
        )
        .fetch_one(a.repo.pool())
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM operations WHERE kind='handoff-cutover'"
        )
        .fetch_one(a.repo.pool())
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn handoff_activation_registered_operation_accepts_only_original_committed_cutover() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let plan = plan(&a, &b).await;
    let result = commit(&a, plan).await.unwrap();
    let handoff_cutover::CutoverResult::CutoverCommittedPendingActivation { operation_id, .. } =
        result;
    let runtime = runtime(&a);
    let payload = serde_json::json!({"cutover_operation_id":operation_id});
    let key = crate::operation::OperationKey {
        operation_key: operation_id.clone(),
        idempotency_key: Some(operation_id),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    assert!(
        runtime
            .submit("handoff-activation", key, payload)
            .await
            .is_ok(),
        "the registered internal operation must accept its original persisted cutover"
    );
}
