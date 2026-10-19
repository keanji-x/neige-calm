//! Legacy fixtures use actual native mint/bind and serial pause owners, not success proofs.
use super::completed_commit_tests::Fixture;
use super::handoff_coordinator_tests::{active, request, runtime};
use crate::db::prelude::*;
use crate::model::CardRole;
use crate::operation::handoff_activation as activation;

#[tokio::test]
async fn handoff_coordinator_legacy_proven_transport_damage_rejects_before_adoption_or_mint() {
    for state in [
        "resources_checkpointed",
        "blocked",
        "needs_recovery",
        "preparing",
        "unknown",
        "resource_request_recorded",
        "blocked_false",
    ] {
        for damage in ["missing", "null", "mismatch", "state_mismatch", "none"] {
            if !matches!(
                state,
                "resources_checkpointed" | "blocked" | "needs_recovery"
            ) && damage != "none"
            {
                continue;
            }
            if damage == "state_mismatch" && state != "resources_checkpointed" {
                continue;
            }
            let a = Fixture::new().await;
            let b = a.sibling(CardRole::Assistant).await;
            let plan = super::handoff_cutover_tests::plan(&a, &b).await;
            let cutover = super::handoff_activation_test_support::original(
                super::handoff_cutover_tests::commit(&a, plan)
                    .await
                    .unwrap(),
            );
            let (runtime, registry) = runtime(&a, &b);
            let start = super::handoff_activation_test_support::start_adapter(
                &a,
                a.repo.clone(),
                registry.clone(),
            );
            let child = crate::operation::handoff_legacy_fixtures_tests::proven(
                a.repo.as_ref(),
                &cutover,
                &start,
            )
            .await
            .unwrap();
            assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
            let proof: (String, Option<String>) = sqlx::query_as(
                "SELECT thread_id,token_hash FROM planner_handoff_activations WHERE cutover_operation_id=?1",
            ).bind(&cutover).fetch_one(a.repo.pool()).await.unwrap();
            let original: String =
                sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
                    .bind(&child)
                    .fetch_one(a.repo.pool())
                    .await
                    .unwrap();
            let mut output: serde_json::Value = serde_json::from_str(&original).unwrap();
            output["result"] = match state {
                "resources_checkpointed" => serde_json::json!({"state":state,"thread_id":proof.0}),
                "blocked" => {
                    serde_json::json!({"state":state,"reason":"readiness lost","resources_checkpointed":true})
                }
                "preparing" | "resource_request_recorded" => serde_json::json!({"state":state}),
                "blocked_false" => {
                    serde_json::json!({"state":"blocked","reason":"readiness lost","resources_checkpointed":false})
                }
                _ => serde_json::json!({"state":state,"reason":"installation failed"}),
            };
            match damage {
                "none" => {}
                "missing" => {
                    output["data"]
                        .as_object_mut()
                        .unwrap()
                        .remove("codex_thread_id");
                }
                "null" => output["data"]["codex_thread_id"] = serde_json::Value::Null,
                "state_mismatch" => {
                    output["result"]["thread_id"] = serde_json::json!("wrong-state-thread")
                }
                _ => output["data"]["codex_thread_id"] = serde_json::json!("wrong-thread"),
            }
            // Minimal corrupt storage boundary: the already minted immutable proof is untouched.
            sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
                .bind(output.to_string())
                .bind(&child)
                .execute(a.repo.db().writer_pool())
                .await
                .unwrap();
            let rejected = activation::activate(&runtime, cutover.clone()).await;
            if damage == "none"
                && matches!(
                    state,
                    "resources_checkpointed" | "blocked" | "needs_recovery"
                )
            {
                let result = super::handoff_activation_test_support::result(&rejected.unwrap());
                let activation::ActivationResult::Activated { session_id } = result else {
                    panic!("valid proof must forward");
                };
                assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
                assert_eq!(active(&a).await, 0);
                registry.shutdown_and_remove(&session_id).await.unwrap();
                continue;
            }
            assert!(
                rejected.is_err(),
                "{state}/{damage}: adoption must reject inconsistent evidence"
            );
            assert_eq!(
                a.daemon.started_thread_params_for_test().len(),
                1,
                "{state}/{damage}"
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM planner_handoff_adoptions")
                    .fetch_one(a.repo.pool())
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(sqlx::query_as::<_,(String,Option<String>)>("SELECT thread_id,token_hash FROM planner_handoff_activations WHERE cutover_operation_id=?1")
                .bind(&cutover).fetch_one(a.repo.pool()).await.unwrap(), proof);
        }
    }
}

#[tokio::test]
async fn handoff_coordinator_legacy_finished_requires_explicit_original_claim_settlement() {
    for landed in [false, true] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let legacy =
            crate::operation::handoff_coordinator::admit_for_test(a.repo.db(), approval.clone())
                .await
                .unwrap();
        let mut input: serde_json::Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
                .bind(&legacy)
                .fetch_one(a.repo.pool())
                .await
                .unwrap(),
        )
        .unwrap();
        input.as_object_mut().unwrap().remove("version");
        let pause = serde_json::from_value(input["source"].clone()).unwrap();
        // The fixture represents the old Finished checkpoint after a real serial absence barrier.
        // It does not claim to execute an old binary or generate a successful native proof.
        assert!(matches!(
            a.harness
                .resolve_original_handoff_pause(pause)
                .await
                .unwrap(),
            crate::harness::handoff_pause::PauseQuery::Unknown
        ));
        let output = crate::operation::TxOutput::new(
            "track",
            Some(approval.track_id.to_string()),
            serde_json::json!({"stage":"finished","source_attempted":false,"target_attempted":false,"source_receipt":null,"target_receipt":null,"source_release":null,"target_release":null,"plan":null,"committed":null,"reason":null}),
        );
        sqlx::query("UPDATE operations SET kind='handoff-coordinator',payload_json=?1,tx_output_json=?2,phase='succeeded' WHERE id=?3")
            .bind(input.to_string()).bind(serde_json::to_string(&output).unwrap()).bind(&legacy).execute(a.repo.db().writer_pool()).await.unwrap();
        if landed {
            // Emulate the pre-0177 boundary only during the real original command.
            let trigger: String = sqlx::query_scalar(
                "SELECT sql FROM sqlite_master WHERE name='planner_handoff_no_new_revoked_claim'",
            )
            .fetch_one(a.repo.pool())
            .await
            .unwrap();
            sqlx::query("DROP TRIGGER planner_handoff_no_new_revoked_claim")
                .execute(a.repo.db().writer_pool())
                .await
                .unwrap();
            a.harness
                .pause_for_handoff(serde_json::from_value(input["source"].clone()).unwrap())
                .await
                .unwrap();
            sqlx::raw_sql(&trigger)
                .execute(a.repo.db().writer_pool())
                .await
                .unwrap();
        }
        let before = super::handoff_cutover_tests::data_image(a.repo.pool()).await;
        assert!(matches!(
            crate::operation::handoff_coordinator::query(&runtime, &approval)
                .await
                .unwrap(),
            crate::operation::handoff_coordinator::CoordinatorResult::NeedsRecovery { .. }
        ));
        assert_eq!(
            super::handoff_cutover_tests::data_image(a.repo.pool()).await,
            before,
            "query is pure read"
        );
        if !landed {
            assert!(
                a.harness
                    .pause_for_handoff(serde_json::from_value(input["source"].clone()).unwrap())
                    .await
                    .is_err(),
                "legacy terminal must fence before adoption"
            );
        }
        assert!(
            crate::operation::handoff_coordinator::retry(&runtime, &approval, 1)
                .await
                .is_err()
        );
        crate::operation::handoff_coordinator::retry(&runtime, &approval, 0)
            .await
            .unwrap();
        assert_eq!(active(&a).await, 0);
        assert!(!a.stored().await.handoff_pause.is_paused());
        assert!(
            a.harness
                .pause_for_handoff(serde_json::from_value(input["source"].clone()).unwrap())
                .await
                .is_err(),
            "late original command must not recreate settled pause"
        );
        assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
        let fresh = super::handoff_pause_tests::request(&a);
        let receipt = a.harness.pause_for_handoff(fresh).await.unwrap();
        assert_eq!(
            a.harness.resume_from_handoff(receipt).await.unwrap(),
            crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
        );
        sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
            .bind(approval.track_id.as_str())
            .execute(a.repo.db().writer_pool())
            .await
            .unwrap();
        a.repo
            .track_delete(approval.track_id.as_str())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn handoff_coordinator_legacy_activated_ignored_reservation_reopens_original_generation() {
    use crate::operation::handoff_activation::ActivationResult;
    use crate::operation::{handoff_coordinator as handoff, handoff_cutover as cutover};
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("legacy.db").display()
    );
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, registry) = runtime(&a, &b);
    let approval = request(&a, &b).await;
    let legacy = handoff::admit_for_test(a.repo.db(), approval.clone())
        .await
        .unwrap();
    let mut input: serde_json::Value = serde_json::from_str(
        &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
            .bind(&legacy)
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
    )
    .unwrap();
    input.as_object_mut().unwrap().remove("version");
    let source = a
        .harness
        .pause_for_handoff(serde_json::from_value(input["source"].clone()).unwrap())
        .await
        .unwrap();
    let target = b
        .harness
        .pause_for_handoff(serde_json::from_value(input["target"].clone()).unwrap())
        .await
        .unwrap();
    // Real precheck and cutover use the original admitted scope; only the legacy journal
    // representation is a fixture. No success result is authored by this test.
    let plan = cutover::precheck(
        a.repo.db(),
        cutover::CutoverScope {
            track_id: approval.track_id.clone(),
            source_card_id: approval.source_card_id.clone(),
            source_session_id: approval.source_session_id.clone(),
            target_card_id: approval.target_card_id.clone(),
            target_session_id: approval.target_session_id.clone(),
            source_pause: source.clone(),
            target_pause: target.clone(),
            plan_id: input["plan_id"].as_str().unwrap().into(),
            new_session_id: input["new_session_id"].as_str().unwrap().into(),
            user_audit_id: approval.user_audit_id.clone(),
        },
    )
    .await
    .unwrap();
    let mut output = crate::operation::TxOutput::new(
        "track",
        Some(approval.track_id.to_string()),
        serde_json::json!({"stage":"commit_intent","source_attempted":true,"target_attempted":true,"source_receipt":source,"target_receipt":target,"source_release":null,"target_release":null,"plan":plan,"committed":null,"reason":null}),
    );
    sqlx::query("UPDATE operations SET kind='handoff-coordinator',payload_json=?1,tx_output_json=?2,phase='succeeded' WHERE id=?3")
        .bind(input.to_string()).bind(serde_json::to_string(&output).unwrap()).bind(&legacy).execute(a.repo.db().writer_pool()).await.unwrap();
    let committed = super::handoff_cutover_tests::commit(&a, plan)
        .await
        .unwrap();
    let cutover::CutoverResult::CutoverCommittedPendingActivation {
        operation_id: cutover,
        planner_session_id: session,
        ..
    } = &committed;
    output.result["stage"] = serde_json::json!("committed");
    output.result["committed"] = serde_json::to_value(&committed).unwrap();
    sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&output).unwrap())
        .bind(&legacy)
        .execute(a.repo.db().writer_pool())
        .await
        .unwrap();
    let start =
        super::handoff_activation_test_support::start_adapter(&a, a.repo.clone(), registry.clone());
    let child =
        crate::operation::handoff_legacy_fixtures_tests::proven(a.repo.as_ref(), cutover, &start)
            .await
            .unwrap();
    let before = super::handoff_cutover_tests::data_image(a.repo.pool()).await;
    assert!(matches!(
        activation::query(&runtime, cutover).await.unwrap(),
        ActivationResult::ResourcesCheckpointed { .. }
    ));
    assert_eq!(
        super::handoff_cutover_tests::data_image(a.repo.pool()).await,
        before
    );
    sqlx::query(concat!(
        "CREATE TRIGGER legacy_ignored_reservation BEFORE UPDATE OF active ON ",
        "planner_handoff_coordinators WHEN OLD.active=1 AND NEW.active=0 BEGIN SELECT ",
        "RAISE(IGNORE); END",
    ))
    .execute(a.repo.db().writer_pool())
    .await
    .unwrap();
    crate::operation::handoff_legacy_fixtures_tests::finish(a.repo.clone(), &child, &start)
        .await
        .unwrap();
    assert_eq!(
        active(&a).await,
        1,
        "old successful UPDATE execution did not settle reservation"
    );
    let old_image: (String, String) =
        sqlx::query_as("SELECT payload_json,tx_output_json FROM operations WHERE id=?1")
            .bind(&legacy)
            .fetch_one(a.repo.pool())
            .await
            .unwrap();
    let proof_image:(String,Option<String>)=sqlx::query_as("SELECT thread_id,token_hash FROM planner_handoff_activations WHERE cutover_operation_id=?1").bind(cutover).fetch_one(a.repo.pool()).await.unwrap();
    assert!(matches!(
        handoff::query(&runtime, &approval).await.unwrap(),
        handoff::CoordinatorResult::Forward {
            activation: ActivationResult::NeedsRecovery { .. },
            ..
        }
    ));
    assert!(handoff::retry(&runtime, &approval, 1).await.is_err());
    assert!(
        handoff::retry(&runtime, &approval, 0).await.is_err(),
        "ignored settlement rolls adoption back"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM planner_handoff_adoptions")
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER legacy_ignored_reservation")
        .execute(a.repo.db().writer_pool())
        .await
        .unwrap();
    drop(runtime);
    let reopened = std::sync::Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    let restored =
        super::handoff_activation_test_support::runtime_with_repo(&a, reopened, registry.clone());
    assert!(matches!(
        handoff::retry(&restored, &approval, 0).await.unwrap(),
        handoff::CoordinatorResult::Forward {
            activation: ActivationResult::Activated { .. },
            ..
        }
    ));
    assert_eq!(active(&a).await, 0);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    assert_eq!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT payload_json,tx_output_json FROM operations WHERE id=?1"
        )
        .bind(&legacy)
        .fetch_one(a.repo.pool())
        .await
        .unwrap(),
        old_image
    );
    assert_eq!(sqlx::query_as::<_,(String,Option<String>)>("SELECT thread_id,token_hash FROM planner_handoff_activations WHERE cutover_operation_id=?1").bind(cutover).fetch_one(a.repo.pool()).await.unwrap(),proof_image);
    registry.shutdown_and_remove(session).await.unwrap();
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
        .bind(approval.track_id.as_str())
        .execute(a.repo.db().writer_pool())
        .await
        .unwrap();
    a.repo
        .track_delete(approval.track_id.as_str())
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_coordinator_legacy_source_intent_expired_driver_cannot_relock_after_adoption() {
    for source in [true, false] {
        use crate::operation::handoff_coordinator as handoff;
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let admitted = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "handoff-begin-admitted",
            &approval.key,
            admitted.clone(),
        );
        let drive = {
            let runtime = runtime.clone();
            let approval = approval.clone();
            tokio::spawn(async move { handoff::begin(&runtime, approval).await })
        };
        super::handoff_coordinator_tests::entered(&admitted).await;
        let id = runtime
            .find_by_kind_and_idempotency("handoff-workflow", &approval.key)
            .await
            .unwrap()
            .unwrap()
            .id;
        let hook = super::handoff_coordinator_tests::hook(
            &id,
            if source {
                "SourceIntent"
            } else {
                "TargetIntent"
            },
        );
        admitted.release.notify_one();
        super::handoff_coordinator_tests::entered(&hook).await;
        let mut input: serde_json::Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
                .bind(&id)
                .fetch_one(a.repo.pool())
                .await
                .unwrap(),
        )
        .unwrap();
        input.as_object_mut().unwrap().remove("version");
        let source_receipt = a.stored().await.handoff_pause.receipt().cloned();
        let output = crate::operation::TxOutput::new(
            "track",
            Some(approval.track_id.to_string()),
            serde_json::json!({"stage":if source { "source_intent" } else { "target_intent" },"source_attempted":true,"target_attempted":!source,"source_receipt":source_receipt,"target_receipt":null,"source_release":null,"target_release":null,"plan":null,"committed":null,"reason":null}),
        );
        // Use the public production post-intent pause seam. This storage representation change
        // models a V1 intent, not execution of a historical binary or a second compensation algorithm.
        sqlx::query(concat!(
        "UPDATE operations SET kind='handoff-coordinator',payload_json=?1,tx_output_json=?2,",
        "lease_until_ms=0,lease_owner=NULL,phase='stuck',",
        "phase_detail_json='{\"reason\":\"Legacy handoff requires explicit adoption\",\"since\":0,",
        "\"from_phase\":\"app_server_interact\"}' WHERE id=?3",
    ))
    .bind(input.to_string())
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&id)
    .execute(a.repo.db().writer_pool())
    .await
    .unwrap();
        // A separate process-equivalent runtime serially resolves the exact request to absence.
        let (recovery, _) = self::runtime(&a, &b);
        let settling = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "handoff-coordinator-before-settlement",
            &approval.source_session_id,
            settling.clone(),
        );
        let retry = {
            let approval = approval.clone();
            tokio::spawn(async move { handoff::retry(&recovery, &approval, 0).await })
        };
        super::handoff_coordinator_tests::entered(&settling).await;
        // The actual old command arrives after serial absence but BEFORE terminal persistence.
        // Adoption must fence this interval; a terminal-only guard would be too late.
        assert_eq!(active(&a).await, 1);
        let frozen: (String, String, String) =
            sqlx::query_as("SELECT payload_json,tx_output_json,phase FROM operations WHERE id=?1")
                .bind(&id)
                .fetch_one(a.repo.pool())
                .await
                .unwrap();
        hook.release.notify_one();
        let _old_answer = drive.await.unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, String, String)>(
                "SELECT payload_json,tx_output_json,phase FROM operations WHERE id=?1"
            )
            .bind(&id)
            .fetch_one(a.repo.pool())
            .await
            .unwrap(),
            frozen,
            "old journal remains byte-for-byte frozen"
        );
        assert!(!a.stored().await.handoff_pause.is_paused());
        assert!(!b.stored().await.handoff_pause.is_paused());
        settling.release.notify_one();
        assert!(matches!(
            retry.await.unwrap().unwrap(),
            handoff::CoordinatorResult::CancelledOrRefused
        ));
        assert!(!a.stored().await.handoff_pause.is_paused());
        assert!(!b.stored().await.handoff_pause.is_paused());
        assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
        assert_eq!(active(&a).await, 0);
    }
}

#[tokio::test]
async fn handoff_coordinator_legacy_release_health_survives_adoption() {
    use crate::harness::handoff_pause::{PauseState, ReleaseOutcome};
    use crate::operation::handoff_coordinator as handoff;
    for recovered in [false, true] {
        let mut a = Fixture::new().await;
        a.harness = crate::harness::PlannerHarness::run(super::handoff_pause_tests::params(
            &a,
            a.stored().await,
        ));
        let b = a.sibling(CardRole::Assistant).await;
        let approval = request(&a, &b).await;
        let id = handoff::admit_for_test(a.repo.db(), approval.clone())
            .await
            .unwrap();
        let mut input: serde_json::Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
                .bind(&id)
                .fetch_one(a.repo.pool())
                .await
                .unwrap(),
        )
        .unwrap();
        input.as_object_mut().unwrap().remove("version");
        let receipt = a
            .harness
            .pause_for_handoff(serde_json::from_value(input["source"].clone()).unwrap())
            .await
            .unwrap();
        if recovered {
            super::handoff_pause_fault_tests::restart(&mut a).await;
        } else {
            super::handoff_pause_fault_tests::error(&a).await;
        }
        let outcome = a
            .harness
            .resume_from_handoff(receipt.clone())
            .await
            .unwrap();
        assert!(!matches!(outcome, ReleaseOutcome::HealthyReleased));
        let output = crate::operation::TxOutput::new(
            "track",
            Some(approval.track_id.to_string()),
            serde_json::json!({"stage":"released_needs_recovery","source_attempted":true,"target_attempted":false,"source_receipt":receipt,"target_receipt":null,"source_release":outcome,"target_release":null,"plan":null,"committed":null,"reason":"original nonhealthy release"}),
        );
        sqlx::query("UPDATE operations SET kind='handoff-coordinator',payload_json=?1,tx_output_json=?2,phase='succeeded' WHERE id=?3")
            .bind(input.to_string()).bind(serde_json::to_string(&output).unwrap()).bind(&id).execute(a.repo.db().writer_pool()).await.unwrap();
        let (runtime, _) = runtime(&a, &b);
        assert!(matches!(
            handoff::retry(&runtime, &approval, 0).await.unwrap(),
            handoff::CoordinatorResult::CancelledOrRefused
        ));
        assert_eq!(active(&a).await, 0);
        assert!(
            matches!(a.stored().await.handoff_pause,PauseState::Released{outcome:actual,..} if actual == outcome)
        );
        assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
        a.harness.shutdown().await.unwrap();
    }
}
