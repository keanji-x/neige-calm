use super::*;
use crate::operation::{Operation, Phase};
use execution::{Decision, Kind, Release, StopReason};

/// Drive the real adapter to its persisted handoff, leaving the observer unscheduled.
async fn park(fx: &ForgeRuntimeFixture, payload: ForgeActionPayload) -> Operation {
    let ops = &fx.runtime.repo;
    let id = fx.prepare_operation(&payload).await.id;
    let adapter = ForgeActionAdapter::new();
    let ctx = fx.runtime.reclaim_context();
    let op = ops.claim_drive_operation(&id).await.unwrap().unwrap();
    ops.set_phase(&op, Phase::SpawnStarted, None)
        .await
        .unwrap()
        .applied()
        .unwrap();
    let op = ops.claim_drive_operation(&id).await.unwrap().unwrap();
    let spawn = adapter
        .spawn_side_effect(crate::operation::required_output(&op).unwrap(), &op, ctx)
        .await
        .unwrap();
    assert!(matches!(
        spawn,
        crate::operation::SpawnOutcome::Parked { .. }
    ));
    drop(spawn);
    let latest = ops.get_operation(&id).await.unwrap().unwrap();
    ops.set_parked(&latest, payload.deadline_ms, Some(execution::KEY))
        .await
        .unwrap()
        .unwrap();
    ops.claim_parked(&id).await.unwrap().unwrap()
}

async fn latest(fx: &ForgeRuntimeFixture, op: &Operation) -> Operation {
    fx.runtime
        .repo
        .get_operation(&op.id)
        .await
        .unwrap()
        .unwrap()
}

async fn settled(fx: &ForgeRuntimeFixture, id: &workspace_runtime::execution::ExecutionId) {
    tokio::time::timeout(
        Duration::from_secs(8),
        fx.runtime
            .reclaim_context()
            .execution
            .engine()
            .unwrap()
            .wait_settled(id),
    )
    .await
    .unwrap()
    .unwrap();
}

async fn retire_target(fx: &ForgeRuntimeFixture) {
    fx.repo
        .track_update(
            &fx.track_id,
            serde_json::from_value(json!({"closed":true})).unwrap(),
        )
        .await
        .unwrap();
    let event = OwnerEvent {
        actor: ActorId::KernelDispatcher,
        scope: EventScope::Card {
            card: "card-1".into(),
            track: fx.track_id.clone().into(),
            area: "area".into(),
        },
        event: Event::WorktreeProvisioned {
            track_id: fx.track_id.clone().into(),
            card_id: "card-1".into(),
            path: fx.results.path().join("extra").to_str().unwrap().into(),
        },
    };
    fx.repo
        .db()
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                ForgeActionAdapter::new()
                    .record_owner_event_tx(&mut tx, event)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}

fn provision_payload(fx: &ForgeRuntimeFixture) -> ForgeActionPayload {
    let mut payload = action_payload(
        fx,
        "admission-provision",
        shell_probe("touch business-started; exit 7"),
        "admission-provision",
        Some(
            serde_json::from_value(json!({"event_kind":"worktree.provisioned","fields":{}}))
                .unwrap(),
        ),
    );
    // Keep the retired provision target disjoint from the still-admitted probe cwd.
    payload
        .context
        .insert("path".into(), json!(fx.results.path().join("extra")));
    payload.probe = Some(ProbeSpec {
        probe_argv: shell_probe("touch verdict-started"),
        output_probe_argv: Some(shell_probe("touch output-started; printf '{}'")),
    });
    payload
}

#[tokio::test]
async fn forge_workspace_release_rechecks_claim_record_and_target_before_rc() {
    if isolated_forge_environment(
        concat!(
            "admission::",
            stringify!(forge_workspace_release_rechecks_claim_record_and_target_before_rc)
        ),
        &[],
    ) {
        return;
    }
    for change in ["claim", "record", "target"] {
        let fx = forge_runtime_fixture().await;
        std::fs::write(fx.cwd.path().join(".envrc"), "touch rc-evaluated\n").unwrap();
        allow_environment(fx.cwd.path());
        let op = park(&fx, provision_payload(&fx)).await;
        std::fs::remove_file(fx.cwd.path().join("rc-evaluated")).unwrap();
        let ctx = fx.runtime.reclaim_context();
        let ready = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "workspace-environment-before-ready",
            fx.cwd.path().canonicalize().unwrap().to_str().unwrap(),
            ready.clone(),
        );
        let release = execution::release(ctx, &op, Kind::Action);
        let revoke = async {
            tokio::time::timeout(Duration::from_secs(5), ready.entered.notified())
                .await
                .unwrap();
            match change {
                "claim" => {
                    fx.repo
                        .db()
                        .writer()
                        .execute(
                            sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
                                .bind(&op.id),
                        )
                        .await
                        .unwrap();
                    fx.runtime.repo.claim_parked(&op.id).await.unwrap().unwrap();
                }
                "record" => execution::stop(ctx, &op, Kind::Action, StopReason::Disposed)
                    .await
                    .unwrap(),
                "target" => retire_target(&fx).await,
                _ => unreachable!(),
            }
            ready.release.notify_one();
        };
        let (result, ()) = tokio::join!(release, revoke);
        match change {
            "claim" => assert!(matches!(result, Err(CalmError::Conflict(_)))),
            "record" => assert!(matches!(
                result,
                Err(CalmError::Db(sqlx::Error::RowNotFound))
            )),
            "target" => assert!(result.is_ok()),
            _ => unreachable!(),
        }
        let current = latest(&fx, &op).await;
        let record = execution::record(&current).unwrap();
        assert_eq!(record.release, Release::Held);
        if change == "claim" {
            assert_eq!(record.decision, Decision::Observing);
            // Cleanup uses the replacement's real claim, never the rejected original snapshot.
            execution::stop(ctx, &current, Kind::Action, StopReason::Disposed)
                .await
                .unwrap();
        } else {
            assert!(matches!(record.decision, Decision::Stopping { .. }));
        }
        settled(&fx, &record.execution_id).await;
        assert!(!fx.cwd.path().join("business-started").exists());
        assert!(
            !fx.cwd.path().join("rc-evaluated").exists(),
            "{change} invalidation must precede project evaluation"
        );
    }
}

#[tokio::test]
async fn forge_workspace_probe_release_does_not_repeat_action_provision_gate() {
    if isolated_forge_environment(
        concat!(
            "admission::",
            stringify!(forge_workspace_probe_release_does_not_repeat_action_provision_gate)
        ),
        &[],
    ) {
        return;
    }
    let fx = forge_runtime_fixture().await;
    std::fs::write(fx.cwd.path().join(".envrc"), "touch rc-evaluated\n").unwrap();
    allow_environment(fx.cwd.path());
    let op = park(&fx, provision_payload(&fx)).await;
    let ctx = fx.runtime.reclaim_context();
    execution::release(ctx, &op, Kind::Action).await.unwrap();
    let current = latest(&fx, &op).await;
    settled(&fx, &execution::record(&current).unwrap().execution_id).await;
    assert!(fx.cwd.path().join("business-started").exists());
    retire_target(&fx).await;
    for (kind, marker) in [
        (Kind::Verdict, "verdict-started"),
        (Kind::Output, "output-started"),
    ] {
        let current = latest(&fx, &op).await;
        let frozen =
            FrozenForge::from_output(crate::operation::required_output(&current).unwrap()).unwrap();
        execution::admit_probe(ctx, &current, &frozen, kind)
            .await
            .unwrap();
        let current = latest(&fx, &op).await;
        std::fs::remove_file(fx.cwd.path().join("rc-evaluated")).unwrap();
        execution::release(ctx, &current, kind).await.unwrap();
        let released = execution::record(&latest(&fx, &op).await).unwrap();
        let probe = match kind {
            Kind::Verdict => released.probes.verdict.unwrap(),
            Kind::Output => released.probes.output.unwrap(),
            Kind::Action => unreachable!(),
        };
        settled(&fx, &probe.execution_id).await;
        assert!(matches!(probe.release, Release::Released { .. }));
        assert_eq!(probe.decision, Decision::Observing);
        assert!(fx.cwd.path().join(marker).exists());
        assert!(fx.cwd.path().join("rc-evaluated").exists());
    }
}

#[tokio::test]
async fn forge_workspace_release_retries_same_held_execution_after_settings_read_failure() {
    if isolated_forge_environment(
        concat!(
            "admission::",
            stringify!(
                forge_workspace_release_retries_same_held_execution_after_settings_read_failure
            )
        ),
        &[],
    ) {
        return;
    }
    release_retries_after_dependency_read_failure(
        "ALTER TABLE settings RENAME TO forge_release_unavailable_dependency",
        "ALTER TABLE forge_release_unavailable_dependency RENAME TO settings",
    )
    .await;
}

#[tokio::test]
async fn forge_workspace_release_retries_same_held_execution_after_source_read_failure() {
    if isolated_forge_environment(
        concat!(
            "admission::",
            stringify!(
                forge_workspace_release_retries_same_held_execution_after_source_read_failure
            )
        ),
        &[],
    ) {
        return;
    }
    release_retries_after_dependency_read_failure(
        "ALTER TABLE cards RENAME TO forge_release_unavailable_dependency",
        "ALTER TABLE forge_release_unavailable_dependency RENAME TO cards",
    )
    .await;
}

async fn release_retries_after_dependency_read_failure(rename: &str, restore: &str) {
    let fx = forge_runtime_fixture().await;
    std::fs::write(fx.cwd.path().join(".envrc"), "").unwrap();
    allow_environment(fx.cwd.path());
    let op = park(
        &fx,
        no_event_payload(
            &fx,
            "settings-read-retry",
            shell_probe("printf once >> business-started"),
            "settings-read-retry",
        ),
    )
    .await;
    let ctx = fx.runtime.reclaim_context();
    let db = fx.repo.db();
    let operation_db = ctx.operation_repo.sqlite_db();
    // This schema fault belongs exclusively to this in-memory fixture and its real operation.
    assert!(std::ptr::eq(db.read_pool(), operation_db.read_pool()));
    let held = execution::record(&op).unwrap();
    let desired: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?1")
            .bind(held.execution_id.as_str())
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    let fault_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='forge_release_unavailable_dependency')")
        .fetch_one(db.read_pool()).await.unwrap();
    assert!(!fault_exists);
    db.writer().execute(sqlx::query(rename)).await.unwrap();
    let result = execution::release(ctx, &op, Kind::Action).await;
    // Restore before asserting the RED result, so failure cannot poison executor cleanup.
    db.writer().execute(sqlx::query(restore)).await.unwrap();
    assert!(
        matches!(result, Err(CalmError::Db(_))),
        "a transient Neige dependency read must remain retryable"
    );
    let current = latest(&fx, &op).await;
    assert_eq!(execution::record(&current).unwrap(), held);
    let after_desired: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?1")
            .bind(held.execution_id.as_str())
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    assert_eq!(after_desired, desired);
    assert!(!fx.cwd.path().join("business-started").exists());

    execution::release(ctx, &current, Kind::Action)
        .await
        .unwrap();
    settled(&fx, &held.execution_id).await;
    let released = execution::record(&latest(&fx, &op).await).unwrap();
    assert_eq!(released.execution_id, held.execution_id);
    assert_eq!(released.decision, Decision::Observing);
    assert!(matches!(released.release, Release::Released { .. }));
    assert_eq!(
        std::fs::read_to_string(fx.cwd.path().join("business-started")).unwrap(),
        "once"
    );
}
