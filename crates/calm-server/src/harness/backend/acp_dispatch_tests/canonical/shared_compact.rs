//! The retained maintenance operation is independent of the previous canonical input receipt.
use super::*;
use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};
use crate::harness::native_admission::NativePromptAdmission;
use provider::control::CompactSubmission;

#[tokio::test]
async fn canonical_antigravity_compact_operation_is_not_resent() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let receipt = fx.receipt_bytes(&fx.submission).await;
    std::fs::write(fx.dir.path().join("scenario"), "shared-compact-success").unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    let request = || CompactRequest {
        selection: TurnModelSelection::inherit(),
    };
    assert!(matches!(
        fx.backend
            .compact_canonical(
                target.clone(),
                request(),
                &admission,
                &fx.backend.handoff_for_test().await
            )
            .await
            .unwrap(),
        CompactSubmission::Submitted { .. }
    ));
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if matches!(
                fx.backend.observe_maintenance(&target).await.unwrap(),
                Some(MaintenanceObservation::Turn {
                    settled: Some(true),
                    ..
                })
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        fx.backend
            .compact_canonical(
                target,
                request(),
                &admission,
                &fx.backend.handoff_for_test().await
            )
            .await
            .unwrap(),
        CompactSubmission::Unconfirmed { .. }
    ));
    assert_eq!(fx.prompts(), 2);
    assert_eq!(fx.receipt_bytes(&fx.submission).await, receipt);
    assert!(!fx.requests().iter().any(|r| r["method"] == "session/close"));
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_compact_unresolved_execution_refuses_before_write() {
    use calm_truth::db::sqlite::provider_execution as claims;
    use workspace_runtime::execution::{Store, test_support};
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let mut intent = test_support::intent(fx.dir.path().to_owned());
    intent.metadata.consumer = "acp-planner".into();
    intent.metadata.alias = workspace_runtime::execution::ProcessAlias(format!(
        "planner:{}:{}",
        fx.owner.worker,
        intent.metadata.id.as_str(),
    ));
    let store = crate::execution_store::SqliteExecutionStore::new(fx.owner.repo.db().clone())
        .with_projection(
            "acp-planner".into(),
            Arc::new(crate::provider_execution::Projection),
        );
    let worker = fx.owner.worker.clone();
    fx.owner
        .repo
        .db()
        .writer()
        .with_writer(move |session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                store.insert(&mut tx, &intent).await.unwrap();
                assert!(matches!(
                    claims::admit(
                        &mut tx,
                        &worker,
                        intent.metadata.id.as_str().into(),
                        crate::provider_execution::Purpose::RewindCheck,
                        claims::InitialRelease::Held
                    )
                    .await
                    .unwrap(),
                    claims::Cas::Applied(_)
                ));
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    let result = fx
        .backend
        .compact_canonical(
            target,
            CompactRequest {
                selection: TurnModelSelection::inherit(),
            },
            &admission,
            &fx.backend.handoff_for_test().await,
        )
        .await
        .unwrap();
    assert!(matches!(result, CompactSubmission::NotSubmitted { .. }));
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_compact_cancel_cannot_confirm_or_resend() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    std::fs::write(fx.dir.path().join("scenario"), "shared-compact-hold").unwrap();
    let target = MaintenanceTarget {
        card_id: fx.owner.card.clone(),
        track_id: fx.owner.track.clone(),
        worker_session_id: fx.owner.worker.clone(),
        thread_id: fx.owner.thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
    assert!(matches!(
        fx.backend
            .compact_canonical(
                target.clone(),
                CompactRequest {
                    selection: TurnModelSelection::inherit()
                },
                &admission,
                &fx.backend.handoff_for_test().await
            )
            .await
            .unwrap(),
        CompactSubmission::Submitted { .. }
    ));
    let Some(MaintenanceObservation::Turn { turn_id, .. }) =
        fx.backend.observe_maintenance(&target).await.unwrap()
    else {
        panic!("real retained maintenance must supply its turn");
    };
    assert!(matches!(
        fx.session.turn_interrupt(&fx.owner.thread, &turn_id).await,
        provider::control::InterruptOutcome::LocalSignalDelivered
    ));
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if matches!(
                fx.backend.observe_maintenance(&target).await.unwrap(),
                Some(MaintenanceObservation::Turn {
                    settled: Some(false),
                    ..
                })
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        fx.backend
            .compact_canonical(
                target,
                CompactRequest {
                    selection: TurnModelSelection::inherit()
                },
                &admission,
                &fx.backend.handoff_for_test().await
            )
            .await
            .unwrap(),
        CompactSubmission::Unconfirmed { .. }
    ));
    assert_eq!(fx.prompts(), 2);
    let executions: i64 = sqlx::query_scalar(concat!(
        "SELECT count(*) FROM workspace_executions WHERE ",
        "json_extract(intent_json,'$.consumer')='acp-planner' ",
        "AND desired='retire' AND settled_at_ms IS NOT NULL",
    ))
    .fetch_one(fx.owner.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        executions, 2,
        "the turn and canceled maintenance must both stop through Engine"
    );
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert!(
        token.is_none(),
        "canceled maintenance cannot retain tool authority"
    );
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_compact_waits_for_its_delayed_catalog() {
    for scenario in [
        "agy-menu-late",
        "agy-menu-empty",
        "agy-menu-wrong",
        "agy-menu-timeout",
    ] {
        let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await;
        fx.dispatch().await.unwrap();
        fx.wait_end().await;
        let receipt = fx.receipt_bytes(&fx.submission).await;
        std::fs::write(fx.dir.path().join("scenario"), scenario).unwrap();
        let target = MaintenanceTarget {
            card_id: fx.owner.card.clone(),
            track_id: fx.owner.track.clone(),
            worker_session_id: fx.owner.worker.clone(),
            thread_id: fx.owner.thread.clone(),
            operation_id: crate::model::new_id(),
        };
        let admission = NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }));
        let handoff = fx.backend.handoff_for_test().await;
        let compact = fx.backend.compact_canonical(
            target.clone(),
            CompactRequest {
                selection: TurnModelSelection::inherit(),
            },
            &admission,
            &handoff,
        );
        tokio::pin!(compact);
        tokio::select! {
            result = &mut compact => panic!("maintenance returned before the native catalog: {result:?}"),
            ready = tokio::time::timeout(Duration::from_secs(20), async {
                while !fx.dir.path().join("catalog-ready").exists() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }) => ready.unwrap(),
        }
        assert_eq!(fx.prompts(), 1);
        std::fs::write(fx.dir.path().join("release-catalog"), "").unwrap();
        let result = tokio::time::timeout(Duration::from_secs(20), compact)
            .await
            .unwrap()
            .unwrap();
        if scenario == "agy-menu-late" {
            assert!(matches!(result, CompactSubmission::Submitted { .. }));
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if matches!(
                        fx.backend.observe_maintenance(&target).await.unwrap(),
                        Some(MaintenanceObservation::Turn {
                            settled: Some(true),
                            ..
                        })
                    ) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(fx.prompts(), 2);
        } else {
            assert!(
                matches!(result, CompactSubmission::NotSubmitted { .. }),
                "{scenario}: {result:?}"
            );
            assert_eq!(fx.prompts(), 1);
        }
        assert_eq!(fx.receipt_bytes(&fx.submission).await, receipt);
        fx.session.shutdown().await.unwrap();
    }
}
