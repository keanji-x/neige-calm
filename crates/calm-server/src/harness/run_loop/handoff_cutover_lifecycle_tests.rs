//! Lifecycle consumers of the production identity operation; no provider process is started.
use super::completed_commit_tests::Fixture;
use super::handoff_cutover_tests::{commit, plan};
use super::*;
use crate::db::prelude::*;
use crate::model::CardRole;

#[tokio::test]
async fn handoff_retirement_predicates_use_exact_session_lookups() {
    use sqlx::Row;
    let f = Fixture::new().await;
    // Plan the installed production predicates, including both pre-resource
    // journals. No history cardinality or wall-clock threshold is an oracle.
    for trigger in [
        "planner_handoff_owner_retired_no_regrant",
        "planner_handoff_owner_retired_no_current",
    ] {
        let sql: String = sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE name=?1")
            .bind(trigger)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
        let predicate = sql
            .split_once("WHEN ")
            .unwrap()
            .1
            .split_once("BEGIN")
            .unwrap()
            .0;
        let predicate = predicate
            .replace("OLD.id", "'unrelated-session'")
            .replace("NEW.state", "'idle'")
            .replace("NEW.mcp_token_hash", "NULL")
            .replace("NEW.session_id", "'unrelated-session'")
            .replace("OLD.session_id", "'previous-session'");
        let rows = sqlx::query(&format!("EXPLAIN QUERY PLAN SELECT {predicate}"))
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
        let details: Vec<String> = rows.iter().map(|row| row.get("detail")).collect();
        let journals: Vec<_> = details.iter().filter(|line| line.contains(" o ")).collect();
        assert_eq!(journals.len(), 4, "{trigger}: {details:#?}");
        assert!(
            journals
                .iter()
                .all(|line| line.contains("SEARCH o USING INDEX") && line.contains("<expr>=?")),
            "{trigger} must seek each V2/legacy source/target session: {details:#?}"
        );
    }
}

#[tokio::test]
async fn handoff_retirement_pre_resource_journals_fence_direct_writers() {
    use super::handoff_coordinator_tests::{entered, request, runtime, start_at};
    for legacy in [false, true] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let mut live = None;
        if legacy {
            commit(&a, plan(&a, &b).await).await.unwrap();
        } else {
            let (runtime, _) = runtime(&a, &b);
            let approval = request(&a, &b).await;
            let (_, hook, drive) = start_at(&runtime, &approval, "Committed").await;
            entered(&hook).await;
            live = Some((runtime, hook, drive));
        }
        for table in ["planner_handoff_resources", "planner_handoff_activations"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(a.repo.pool())
                .await
                .unwrap();
            assert_eq!(count, 0, "must exercise journal-only retirement");
        }
        for retired in [
            &a.harness.inner.worker_session_id,
            &b.harness.inner.worker_session_id,
        ] {
            for update in ["state='idle'", "mcp_token_hash='regrant'"] {
                let error =
                    sqlx::query(&format!("UPDATE worker_sessions SET {update} WHERE id=?1"))
                        .bind(retired)
                        .execute(a.repo.db().writer_pool())
                        .await
                        .unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("Handoff retired session requires forward recovery"),
                    "{legacy}: {error}"
                );
            }
            // Keep the role unchanged to isolate the current-pointer SQL guard.
            let error = sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                .bind(retired)
                .bind(b.harness.inner.card_id.as_str())
                .execute(a.repo.db().writer_pool())
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Handoff retired session cannot become current"),
                "{legacy}: {error}"
            );
        }
        if let Some((_runtime, hook, drive)) = live {
            hook.release.notify_one();
            drive.await.unwrap();
        }
    }
}

#[tokio::test]
async fn handoff_cutover_history_remains_readonly_while_parent_track_can_be_deleted() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    a.repo
        .harness_item_insert(
            &a.harness.inner.worker_session_id,
            a.harness.inner.card_id.as_str(),
            a.harness.inner.track_id.as_str(),
            "thread-commit-wake",
            Some("retained-turn"),
            Some("history-item"),
            Some("agentMessage"),
            "item/completed",
            "{}",
            None,
        )
        .await
        .unwrap();
    let plan = plan(&a, &b).await;
    commit(&a, plan.clone()).await.unwrap();
    assert!(
        sqlx::query("UPDATE cards SET title='cannot edit history' WHERE id=?1")
            .bind(plan.scope.source_card_id.as_str())
            .execute(a.repo.db().writer_pool())
            .await
            .is_err()
    );
    // Track deletion is its existing user/kernel management contract, never history authority.
    a.repo
        .track_delete(plan.scope.track_id.as_str())
        .await
        .unwrap();
    assert!(
        a.repo
            .track_get(plan.scope.track_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        a.repo
            .card_get(plan.scope.source_card_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let event = |key: &str| Event::TrackWakeRequested {
        track_id: a.harness.inner.track_id.clone(),
        source: "handoff-fixture".into(),
        key: key.into(),
        text: key.into(),
    };
    let (floor, original) = a.event_entry(&event("queued-prefix")).await;
    a.harness
        .observe_envelope(original.observation(), floor)
        .await
        .unwrap();
    let plan = plan(&a, &b).await;
    commit(&a, plan.clone()).await.unwrap();
    let row = a
        .repo
        .session_get_by_id(&plan.scope.new_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    let snapshot = HarnessSnapshot::parse_known(row.handle_state_json.unwrap()).unwrap();
    assert_eq!(snapshot.push_watermark, floor);
    assert_eq!(snapshot.pending_entries(), vec![original]);
    let (suffix, entry) = b.event_entry(&event("suffix-after-cutover")).await;
    let observations = crate::harness::catch_up::observations_since(
        a.repo.as_ref(),
        &plan.scope.track_id,
        snapshot.push_watermark,
        Some(suffix),
    )
    .await
    .unwrap();
    assert_eq!(observations, vec![(suffix, entry.observation())]);
    assert_eq!(a.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn handoff_activation_registered_providers_keep_backend_owned_readiness() {
    for provider in crate::harness::backend::PlannerBackend::registered_providers() {
        let mut a = Fixture::new().await;
        let mut b = a.sibling(CardRole::Assistant).await;
        let mut ingress = vec![];
        // Configure each fixture before its pause. Open only the existing backend's inert
        // session wrapper: its documented contract creates no native resource before a turn.
        for f in [&mut a, &mut b] {
            let role = f
                .repo
                .card_role_get(f.harness.inner.card_id.as_str())
                .await
                .unwrap()
                .unwrap();
            let key = if role == CardRole::Planner {
                "$.planner_provider"
            } else {
                "$.assistant_provider"
            };
            sqlx::query("UPDATE cards SET payload=json_set(payload,?1,json(?2)) WHERE id=?3")
                .bind(key)
                .bind(serde_json::to_string(&provider).unwrap())
                .bind(f.harness.inner.card_id.as_str())
                .execute(f.repo.db().writer_pool())
                .await
                .unwrap();
            sqlx::query("UPDATE worker_sessions SET provider=?1 WHERE id=?2")
                .bind(calm_types::worker::WorkerProviderKind::from(&provider).as_db_str())
                .bind(&f.harness.inner.worker_session_id)
                .execute(f.repo.db().writer_pool())
                .await
                .unwrap();
            let card = f
                .repo
                .card_get(f.harness.inner.card_id.as_str())
                .await
                .unwrap()
                .unwrap();
            let binding = crate::harness::profile::PlannerBinding::from_card(&card, role).unwrap();
            let wiring =
                crate::harness::wiring::PlannerWiring::unconfigured_for_test(f.repo.clone());
            let backend = crate::harness::backend::factory::PlannerFactory::new(
                std::sync::Arc::downgrade(&f.daemon),
                &wiring.claude,
                &wiring.acp,
            )
            .open(
                f.daemon.thread_seals().clone(),
                &wiring,
                f.repo.clone(),
                crate::harness::wiring::PlannerRow {
                    binding,
                    worker_session_id: &f.harness.inner.worker_session_id,
                    card_id: f.harness.inner.card_id.as_str(),
                    track_id: f.harness.inner.track_id.as_str(),
                    prior_total_tokens: 0,
                },
            )
            .await
            .unwrap();
            let mut params = super::handoff_pause_tests::params(f, f.stored().await);
            params.backend = backend;
            let (harness, receiver) = PlannerHarness::run_unstarted_for_test(params, 8);
            f.harness = harness;
            ingress.push(receiver);
        }
        let plan = plan(&a, &b).await;
        commit(&a, plan.clone()).await.unwrap();
        let row = a
            .repo
            .session_get_by_id(&plan.scope.new_session_id.as_str().into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.provider,
            calm_types::worker::WorkerProviderKind::from(&provider)
        );
        assert_eq!(
            row.state,
            crate::session_projection_repo::WorkerSessionState::PendingActivation
        );
        assert!(
            row.thread_id.is_none()
                && row.agent_session_id.is_none()
                && row.mcp_token_hash.is_none()
        );
        assert_eq!(a.daemon.turn_start_count_for_test(), 0);
        assert_eq!(b.daemon.turn_start_count_for_test(), 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM acp_managed_sessions")
                .fetch_one(a.repo.pool())
                .await
                .unwrap(),
            0
        );
        let (runtime, registry) = super::handoff_activation_test_support::runtime(&a);
        let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
            operation_id,
            ..
        } = commit(&a, plan.clone()).await.unwrap();
        let receipt = crate::operation::handoff_activation::activate(&runtime, operation_id)
            .await
            .unwrap();
        let result = match receipt {
            crate::operation::OperationReceipt::AwaitingRetry { output, .. } => output.result,
            crate::operation::OperationReceipt::Settled(crate::operation::OperationResult {
                outcome: crate::operation::OperationOutcome::Succeeded { result },
                ..
            }) => result,
            other => panic!("expected backend-owned readiness result: {other:?}"),
        };
        let result: crate::operation::handoff_activation::ActivationResult =
            serde_json::from_value(result).unwrap();
        if crate::harness::backend::PlannerBackend::metadata(&provider).requires_codex_daemon {
            assert!(matches!(
                result,
                crate::operation::handoff_activation::ActivationResult::Activated { .. }
            ));
            registry
                .shutdown_and_remove(&plan.scope.new_session_id)
                .await
                .unwrap();
        } else {
            assert!(
                matches!(
                    result,
                    crate::operation::handoff_activation::ActivationResult::Blocked { .. }
                ),
                "unconfigured native backend must retain its owner's refusal"
            );
            let row = a
                .repo
                .session_get_by_id(&plan.scope.new_session_id.as_str().into())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                row.state,
                crate::session_projection_repo::WorkerSessionState::PendingActivation
            );
            assert!(row.thread_id.is_none() && row.mcp_token_hash.is_none());
        }
        drop(ingress);
    }
}

async fn echo_projection(f: &Fixture, client: &str) -> crate::error::Result<()> {
    on_notification(
        &f.harness.inner,
        &f.harness.inner.live_claim.writer(),
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: PlannerEventKind::Item {
                phase: ItemPhase::Completed,
                params: serde_json::json!({"turnId":"echo-turn", "item":{
                    "id":"echo-item", "clientId":client, "type":"userMessage",
                    "content":[{"type":"inputText","text":"echo"}]
                }}),
                questions: vec![],
            },
        },
    )
    .await
}

#[tokio::test]
async fn handoff_retired_echo_cannot_upgrade_target_transcript() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let id = insert_projection_row(
        &b.harness.inner,
        "thread-commit-wake",
        "late-client",
        &[super::completed_commit_tests::initial_budget_segment()],
    )
    .await
    .unwrap();
    commit(&a, plan(&a, &b).await).await.unwrap();
    let before = data_transcript(&b).await;
    let _ = echo_projection(&b, "late-client").await;
    assert_eq!(
        data_transcript(&b).await,
        before,
        "retired echo changed row {id}"
    );
}

const TRANSCRIPT_TABLE: &str = "harness_items";

async fn data_transcript(f: &Fixture) -> Vec<(String, Vec<String>)> {
    super::handoff_cutover_tests::data_image(f.repo.pool())
        .await
        .into_iter()
        .filter(|(table, _)| table == TRANSCRIPT_TABLE)
        .collect()
}

#[tokio::test]
async fn handoff_current_echo_upgrades_projection_in_place() {
    let f = Fixture::new().await;
    let id = insert_projection_row(
        &f.harness.inner,
        "thread-commit-wake",
        "current-client",
        &[super::completed_commit_tests::initial_budget_segment()],
    )
    .await
    .unwrap();
    echo_projection(&f, "current-client").await.unwrap();
    let rows = f
        .repo
        .transcript_rows_of_thread(f.harness.inner.card_id.as_str(), "thread-commit-wake")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].turn_id.as_deref(), Some("echo-turn"));
    assert_eq!(rows[0].item_uuid.as_deref(), Some("echo-item"));
    assert_eq!(
        rows[0].input_segments.as_deref(),
        Some(
            serde_json::to_string(&[super::completed_commit_tests::initial_budget_segment()])
                .unwrap()
                .as_str()
        )
    );
}

#[tokio::test]
async fn handoff_transcript_session_binding_fences_all_mutations() {
    for defect in [
        "superseded",
        "pending_activation",
        "pointer",
        "card",
        "track",
        "row_track",
    ] {
        let f = Fixture::new().await;
        let id = insert_projection_row(
            &f.harness.inner,
            "thread-commit-wake",
            "client",
            &[super::completed_commit_tests::initial_budget_segment()],
        )
        .await
        .unwrap();
        let session = &f.harness.inner.worker_session_id;
        let card = f.harness.inner.card_id.as_str();
        let track = f.harness.inner.track_id.as_str();
        match defect {
            "superseded" | "pending_activation" => {
                sqlx::query(
                    "UPDATE worker_sessions SET state=?1, \
                    thread_id=CASE WHEN ?1='pending_activation' THEN NULL ELSE thread_id END \
                    WHERE id=?2",
                )
                .bind(defect)
                .bind(session)
                .execute(f.repo.db().writer_pool())
                .await
                .unwrap();
            }
            "pointer" => {
                sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
                    .bind(card)
                    .execute(f.repo.db().writer_pool())
                    .await
                    .unwrap();
            }
            "row_track" => {
                sqlx::query(&format!(
                    "UPDATE {TRANSCRIPT_TABLE} SET track_id='another-track' WHERE id=?1"
                ))
                .bind(id)
                .execute(f.repo.db().writer_pool())
                .await
                .unwrap();
            }
            _ => {}
        }
        let caller_card = if defect == "card" {
            "another-card"
        } else {
            card
        };
        let caller_track = if defect == "track" {
            "another-track"
        } else {
            track
        };
        let before = data_transcript(&f).await;
        assert_eq!(
            f.repo
                .transcript_projection_upgrade(
                    session,
                    caller_card,
                    caller_track,
                    "client",
                    Some("turn"),
                    "item",
                    "{}"
                )
                .await
                .unwrap(),
            None,
            "{defect}"
        );
        assert_eq!(
            f.repo
                .transcript_projection_delete(session, caller_card, caller_track, "client")
                .await
                .unwrap(),
            0,
            "{defect}"
        );
        if defect != "row_track" {
            assert!(
                f.repo
                    .harness_turn_outcome_put(
                        session,
                        caller_card,
                        caller_track,
                        "thread-commit-wake",
                        "turn",
                        "{}"
                    )
                    .await
                    .is_err(),
                "{defect}"
            );
            let receipt = calm_truth::db::TranscriptReceiptItem {
                item_uuid: "receipt-item".into(),
                item_type: "agentMessage".into(),
                method: "item/completed".into(),
                params: "{}".into(),
            };
            assert!(
                calm_truth::db::RepoOutOfDomain::transcript_receipt_restore(
                    f.repo.as_ref(),
                    session,
                    caller_card,
                    caller_track,
                    "thread-commit-wake",
                    "turn",
                    &[receipt]
                )
                .await
                .is_err(),
                "{defect}"
            );
            let mut tx = crate::db::sqlite::begin_immediate_tx(f.repo.db())
                .await
                .unwrap();
            assert_eq!(
                crate::db::sqlite::transcript_delete_thread_suffix_tx(
                    &mut tx,
                    session,
                    caller_card,
                    caller_track,
                    "thread-commit-wake",
                    0,
                    id
                )
                .await
                .unwrap(),
                0,
                "{defect}"
            );
            tx.commit().await.unwrap();
        }
        assert_eq!(data_transcript(&f).await, before, "{defect}");
    }
}

#[tokio::test]
async fn handoff_current_failed_owner_can_settle_and_delete_projection() {
    let f = Fixture::new().await;
    insert_projection_row(
        &f.harness.inner,
        "thread-commit-wake",
        "client",
        &[super::completed_commit_tests::initial_budget_segment()],
    )
    .await
    .unwrap();
    let session = &f.harness.inner.worker_session_id;
    let card = f.harness.inner.card_id.as_str();
    let track = f.harness.inner.track_id.as_str();
    sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1")
        .bind(session)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    assert_eq!(
        f.repo
            .transcript_projection_delete(session, card, track, "client")
            .await
            .unwrap(),
        1
    );
    let id = f
        .repo
        .harness_turn_outcome_put(session, card, track, "thread-commit-wake", "turn", "{}")
        .await
        .unwrap();
    assert_eq!(
        f.repo
            .harness_turn_outcome_put(session, card, track, "thread-commit-wake", "turn", "{}")
            .await
            .unwrap(),
        id
    );
}

#[tokio::test]
async fn handoff_current_successor_can_upgrade_and_remove_predecessor_projections() {
    use crate::db::sqlite::session_supersede_and_start_tx;
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let f = Fixture::new().await;
    let inner = &f.harness.inner;
    let id = insert_projection_row(
        inner,
        "thread-commit-wake",
        "echo-client",
        &[super::completed_commit_tests::initial_budget_segment()],
    )
    .await
    .unwrap();
    insert_projection_row(
        inner,
        "thread-commit-wake",
        "delete-client",
        &[super::completed_commit_tests::initial_budget_segment()],
    )
    .await
    .unwrap();
    let successor = crate::model::new_id();
    let mut tx = crate::db::sqlite::begin_immediate_tx(f.repo.db())
        .await
        .unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &inner.worker_session_id,
        WorkerSessionInit {
            id: successor.clone(),
            card_id: inner.card_id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread-commit-wake".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: crate::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        f.repo
            .transcript_projection_upgrade(
                &successor,
                inner.card_id.as_str(),
                inner.track_id.as_str(),
                "echo-client",
                Some("turn"),
                "echo",
                "{}"
            )
            .await
            .unwrap(),
        Some(id)
    );
    assert_eq!(
        f.repo
            .transcript_projection_delete(
                &successor,
                inner.card_id.as_str(),
                inner.track_id.as_str(),
                "delete-client"
            )
            .await
            .unwrap(),
        1
    );
    let rows = f
        .repo
        .transcript_rows_of_thread(inner.card_id.as_str(), "thread-commit-wake")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].item_uuid.as_deref(), Some("echo"));
}
