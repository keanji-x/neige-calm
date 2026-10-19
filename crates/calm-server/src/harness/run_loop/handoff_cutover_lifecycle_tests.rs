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
