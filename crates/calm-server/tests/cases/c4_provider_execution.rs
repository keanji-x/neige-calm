//! C4.1 uses the production owner, Engine, SessionExecutor and real supervisor.
use super::claude_planner_session_fixture::{Rig, request};
use calm_server::{error::CalmError, provider_execution as owner};
use calm_truth::db::sqlite::{provider_execution as sql, workspace_execution as executions};
use provider::control::{CompactSubmission, EditPreparation};
use std::{sync::Arc, time::Duration};
use workspace_runtime::execution::*;

async fn wait(mut ready: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("C4 progress");
}
fn script(rig: &Rig, source: &str) {
    std::fs::write(rig.bin("claude"), format!("#!/bin/sh\n{source}\n")).unwrap();
}
async fn claim(rig: &Rig) -> Option<sql::Claim> {
    sql::load(rig.repo.db().read_pool(), &rig.worker_session_id)
        .await
        .unwrap()
}
async fn check(rig: &Rig) -> EditPreparation<CalmError> {
    rig.session()
        .prepare_edit_for_test(&rig.thread)
        .await
        .unwrap()
}
async fn sweep(rig: &Rig) {
    rig.host
        .provider_execution
        .sweep(rig.repo.db(), rig.host.execution.engine())
        .await
        .unwrap();
}
async fn desired(rig: &Rig, id: &ExecutionId) -> String {
    executions::load(rig.repo.db().read_pool(), id.as_str())
        .await
        .unwrap()
        .unwrap()
        .desired
}

#[tokio::test]
async fn c4_rewind_has_one_engine_construction() {
    let rig = Rig::new("exit").await;
    assert!(matches!(
        check(&rig).await,
        EditPreparation::Prepared { .. }
    ));
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT intent_json, desired FROM workspace_executions")
            .fetch_all(rig.repo.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    let metadata: ExecutionMetadata = serde_json::from_str(&rows[0].0).unwrap();
    assert_eq!(metadata.consumer.0, "claude-planner");
    assert_eq!(metadata.program.program, std::path::Path::new("/bin/sh"));
    assert_eq!(rows[0].1, "retire");
    assert!(claim(&rig).await.is_none());
    assert_eq!(rig.read_bin("checks").unwrap().lines().count(), 1);
}

#[tokio::test]
async fn c4_rewind_refuses_unavailable_executor() {
    let rig = Rig::new("exit").await;
    let session = rig.open_session_with_environment(vec![], None).await;
    assert!(matches!(
        session.prepare_edit_for_test(&rig.thread).await,
        Err(CalmError::ExecutionUnavailable)
    ));
    assert!(claim(&rig).await.is_none());
    assert!(rig.read_bin("checks").is_none());
}

#[tokio::test]
async fn c4_rewind_credentials_never_enter_intent_or_journal() {
    let rig = Rig::new("exit").await;
    script(
        &rig,
        "printf '%s' \"$HTTP_PROXY\" > proxy-received; printf '%s' \"$HTTPS_PROXY\" > public-received; printf '%s' \"$ALL_PROXY\" > normalized-received; touch ready; while [ ! -f finish ]; do sleep 0.01; done; exit 0",
    );
    let secret = "http://private-user:private-password@proxy.invalid:8080";
    let normalized_secret = "http:///normalized-user:normalized-password@proxy.invalid:8080";
    let public = "https://public-proxy.invalid:443";
    let session = Arc::new(
        rig.open_session_with_environment(
            vec![
                ("HTTP_PROXY".into(), "http_proxy".into(), secret.into()),
                ("HTTPS_PROXY".into(), "https_proxy".into(), public.into()),
                (
                    "ALL_PROXY".into(),
                    "all_proxy".into(),
                    normalized_secret.into(),
                ),
            ],
            rig.host.execution.engine().cloned(),
        )
        .await,
    );
    let task = {
        let session = session.clone();
        let thread = rig.thread.clone();
        tokio::spawn(async move { session.prepare_edit_for_test(&thread).await })
    };
    let root = rig.dir.path().join("ws");
    wait(async || root.join("ready").exists()).await;
    assert_eq!(
        std::fs::read_to_string(root.join("proxy-received")).unwrap(),
        secret
    );
    assert_eq!(
        std::fs::read_to_string(root.join("public-received")).unwrap(),
        public
    );
    assert_eq!(
        std::fs::read_to_string(root.join("normalized-received")).unwrap(),
        normalized_secret
    );
    assert!(matches!(
        claim(&rig).await.unwrap().record().release,
        sql::Release::Released { .. }
    ));
    let intent: String = sqlx::query_scalar("SELECT intent_json FROM workspace_executions")
        .fetch_one(rig.repo.pool())
        .await
        .unwrap();
    assert!(!intent.contains("private-user"));
    assert!(!intent.contains("private-password"));
    assert!(intent.contains(public));
    assert!(!intent.contains("normalized-user"));
    assert!(!intent.contains("normalized-password"));
    rig.assert_execution_journal_excludes("private-user");
    rig.assert_execution_journal_excludes("private-password");
    rig.assert_execution_journal_excludes("normalized-user");
    rig.assert_execution_journal_excludes("normalized-password");
    std::fs::write(root.join("finish"), "").unwrap();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        EditPreparation::Prepared { .. }
    ));
}

#[tokio::test]
async fn c4_rewind_no_proxy_credentials_never_enter_intent_or_journal() {
    let rig = Rig::new("exit").await;
    script(
        &rig,
        "printf '%s' \"$NO_PROXY\" > upper-received; printf '%s' \"$no_proxy\" > lower-received; touch ready; while [ ! -f finish ]; do sleep 0.01; done; exit 0",
    );
    let secret = "http://exclusion-user:exclusion-password@proxy.invalid:8080";
    let list = format!("localhost,.example.invalid,{secret},127.0.0.1");
    for (upper, lower) in [(secret, list.as_str()), ("localhost,.example.invalid", "*")] {
        let session = rig
            .open_session_with_environment(
                vec![
                    ("NO_PROXY".into(), "no_proxy".into(), "old.invalid".into()),
                    ("NO_PROXY".into(), "NO_PROXY".into(), upper.into()),
                    ("no_proxy".into(), "no_proxy".into(), lower.into()),
                ],
                rig.host.execution.engine().cloned(),
            )
            .await;
        let thread = rig.thread.clone();
        let task = tokio::spawn(async move { session.prepare_edit_for_test(&thread).await });
        let root = rig.dir.path().join("ws");
        wait(async || root.join("ready").exists()).await;
        assert_eq!(
            std::fs::read_to_string(root.join("upper-received")).unwrap(),
            upper
        );
        assert_eq!(
            std::fs::read_to_string(root.join("lower-received")).unwrap(),
            lower
        );
        rig.assert_execution_journal_excludes("exclusion-user");
        rig.assert_execution_journal_excludes("exclusion-password");
        std::fs::write(root.join("finish"), "").unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            EditPreparation::Prepared { .. }
        ));
        std::fs::remove_file(root.join("ready")).unwrap();
        std::fs::remove_file(root.join("finish")).unwrap();
    }
    let intents: Vec<String> = sqlx::query_scalar("SELECT intent_json FROM workspace_executions")
        .fetch_all(rig.repo.pool())
        .await
        .unwrap();
    assert_eq!(intents.len(), 2);
    for intent in &intents {
        assert!(!intent.contains("exclusion-user"));
        assert!(!intent.contains("exclusion-password"));
        assert!(!intent.contains("old.invalid"));
    }
    let env = intents
        .iter()
        .flat_map(|intent| {
            let metadata: ExecutionMetadata = serde_json::from_str(intent).unwrap();
            metadata.program.environment
        })
        .collect::<Vec<_>>();
    assert!(env.contains(&("NO_PROXY".into(), "localhost,.example.invalid".into())));
    assert!(env.contains(&("no_proxy".into(), "*".into())));
}

#[tokio::test]
async fn c4_rewind_verdict_waits_for_settlement() {
    let rig = Rig::new("exit").await;
    script(
        &rig,
        "setsid sh -c 'echo escaped > descendant; sleep 0.3; echo done > drained' </dev/null >/dev/null 2>/dev/null &\nwhile [ ! -f descendant ]; do sleep 0.01; done\nexit 0",
    );
    let result = check(&rig).await;
    assert!(
        matches!(result, EditPreparation::Prepared { .. }),
        "{result:?}"
    );
    let rows: Vec<(Option<String>, String)> =
        sqlx::query_as("SELECT settlement_json, desired FROM workspace_executions")
            .fetch_all(rig.repo.pool())
            .await
            .unwrap();
    assert!(
        rows[0].0.is_some(),
        "leader exit alone never authorizes the verdict"
    );
    assert_eq!(rows[0].1, "retire");
}

#[tokio::test]
async fn c4_rewind_timeout_stops_through_engine() {
    let rig = Rig::new("exit").await;
    script(&rig, "touch ready; exec sleep 120");
    let before = std::time::Instant::now();
    let result = check(&rig).await;
    assert!(matches!(
        result,
        EditPreparation::Failed {
            error: CalmError::ServiceUnavailable(_)
        }
    ));
    assert!(before.elapsed() >= Duration::from_secs(30));
    assert!(claim(&rig).await.is_none());
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT desired, settlement_json FROM workspace_executions")
            .fetch_one(rig.repo.pool())
            .await
            .unwrap();
    assert_eq!(row.0, "retire");
    assert!(row.1.is_some());
}

#[tokio::test]
async fn c4_dropped_rewind_request_is_stopped_by_sweep() {
    let rig = Rig::new("exit").await;
    script(&rig, "touch ready; exec sleep 120");
    let task = {
        let session = rig.session().clone();
        let thread = rig.thread.clone();
        tokio::spawn(async move { session.prepare_edit_for_test(&thread).await })
    };
    wait(async || rig.dir.path().join("ws/ready").exists()).await;
    let old = claim(&rig).await.unwrap();
    let id = owner::execution_id(old.record()).unwrap();
    task.abort();
    let _ = task.await;
    sweep(&rig).await;
    wait(async || {
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    sweep(&rig).await;
    assert!(claim(&rig).await.is_none());
    assert_eq!(desired(&rig, &id).await, "retire");
    assert!(rig.instructions_files().is_empty());
}

#[tokio::test]
async fn c4_canceled_rewind_blocks_native_turn_and_compaction_until_settled() {
    let rig = Rig::new("exit").await;
    script(
        &rig,
        r#"if [ "$1" = --version ]; then echo 2.1.280; exit 0; fi
touch ready; exec sleep 120"#,
    );
    let task = {
        let session = rig.session().clone();
        let thread = rig.thread.clone();
        tokio::spawn(async move { session.prepare_edit_for_test(&thread).await })
    };
    wait(async || rig.dir.path().join("ws/ready").exists()).await;
    // Keep settlement unavailable until after both production admission paths run.
    sqlx::query(
        "CREATE TRIGGER c4_delay_settlement
         BEFORE UPDATE OF settled_at_ms ON workspace_executions
         BEGIN SELECT RAISE(FAIL, 'c4 settlement paused'); END",
    )
    .execute(rig.repo.pool())
    .await
    .unwrap();
    let old = claim(&rig).await.unwrap();
    let id = owner::execution_id(old.record()).unwrap();
    task.abort();
    let _ = task.await;
    assert!(
        rig.session()
            .turn_start(
                &rig.thread,
                request(
                    rig.text("must not mint while the old claim is retained"),
                    &provider::TurnModelSelection::default(),
                    calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                    &uuid::Uuid::new_v4().to_string()
                )
            )
            .await
            .is_err()
    );
    assert!(matches!(
        rig.session()
            .compact_start(&rig.thread, &provider::TurnModelSelection::default())
            .await,
        CompactSubmission::NotSubmitted { .. }
    ));
    assert_eq!(
        rig.mcp_token_hash().await,
        None,
        "admission refusal precedes credential mint"
    );
    assert!(
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("DROP TRIGGER c4_delay_settlement")
        .execute(rig.repo.pool())
        .await
        .unwrap();
    wait(async || {
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    sweep(&rig).await;
    assert!(claim(&rig).await.is_none());
}

#[tokio::test]
async fn c4_repeated_rewind_launches_with_distinct_execution_aliases() {
    let rig = Rig::new("exit").await;
    for _ in 0..2 {
        assert!(matches!(
            check(&rig).await,
            EditPreparation::Prepared { .. }
        ));
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT execution_id, alias FROM workspace_executions ORDER BY execution_id",
    )
    .fetch_all(rig.repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].0, rows[1].0);
    assert_ne!(rows[0].1, rows[1].1);
    assert_eq!(rig.read_bin("checks").unwrap().lines().count(), 2);
}

async fn admit(rig: &Rig) -> (sql::Claim, owner::DriverGuard) {
    let mut intent = test_support::intent(rig.dir.path().join("ws"));
    intent.metadata.consumer = "claude-planner".into();
    intent.metadata.alias = ProcessAlias(owner::sweep::alias(
        &rig.worker_session_id,
        intent.metadata.id.as_str(),
    ));
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = vec!["-c".into(), "exec sleep 120".into()];
    rig.host
        .provider_execution
        .admit(
            rig.repo.db(),
            rig.host.execution.engine().unwrap(),
            &rig.worker_session_id,
            intent,
            owner::Purpose::RewindCheck,
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn c4_provider_admission_is_exclusive_per_session() {
    let rig = Rig::new("exit").await;
    let (old, guard) = admit(&rig).await;
    let mut intent = test_support::intent(rig.dir.path().join("ws"));
    intent.metadata.consumer = "claude-planner".into();
    let next_id = intent.metadata.id.clone();
    intent.metadata.alias = ProcessAlias(owner::sweep::alias(
        &rig.worker_session_id,
        next_id.as_str(),
    ));
    assert!(
        rig.host
            .provider_execution
            .admit(
                rig.repo.db(),
                rig.host.execution.engine().unwrap(),
                &rig.worker_session_id,
                intent,
                owner::Purpose::RewindCheck
            )
            .await
            .is_err()
    );
    assert!(
        executions::load(rig.repo.db().read_pool(), next_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(claim(&rig).await.unwrap(), old);
    drop(guard);
    sweep(&rig).await;
}

#[tokio::test]
async fn c4_inventory_err_never_drives_stop_retire_or_clear() {
    let rig = Rig::new("exit").await;
    let (old, guard) = admit(&rig).await;
    let id = owner::execution_id(old.record()).unwrap();
    drop(guard);
    sqlx::query("DROP TRIGGER workspace_executions_immutable")
        .execute(rig.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE workspace_executions SET intent_json='{}' WHERE execution_id=?")
        .bind(id.as_str())
        .execute(rig.repo.pool())
        .await
        .unwrap();
    sweep(&rig).await;
    assert_eq!(desired(&rig, &id).await, "run");
    assert_eq!(claim(&rig).await.unwrap(), old);
}

#[tokio::test]
async fn c4_orphan_execution_without_record_is_retired() {
    let rig = Rig::new("exit").await;
    let (old, guard) = admit(&rig).await;
    let id = owner::execution_id(old.record()).unwrap();
    drop(guard);
    // An owner deletion's record loss, with a still-classified consumer execution.
    sqlx::query("UPDATE worker_sessions SET provider_execution_json=NULL WHERE id=?")
        .bind(&rig.worker_session_id)
        .execute(rig.repo.pool())
        .await
        .unwrap();
    sweep(&rig).await;
    assert_eq!(desired(&rig, &id).await, "stop");
    wait(async || {
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    sweep(&rig).await;
    assert_eq!(desired(&rig, &id).await, "retire");
}

struct CompletionProbe {
    pool: calm_server::db::sqlite::SqliteDb,
    fail: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl owner::ProviderExecutionOwner for CompletionProbe {
    fn consumer(&self) -> &'static str {
        "claude-planner"
    }
    async fn complete(
        &self,
        session: &str,
        record: &owner::Record,
        evidence: owner::CompletionEvidence,
        _: Option<owner::KnownEnd>,
    ) -> calm_server::error::Result<()> {
        assert!(matches!(evidence, owner::CompletionEvidence::Settled(_)));
        assert_eq!(
            sql::load(self.pool.read_pool(), session)
                .await?
                .unwrap()
                .record()
                .execution_id,
            record.execution_id,
            "owner record must still exist during complete"
        );
        assert_ne!(
            executions::load(self.pool.read_pool(), &record.execution_id)
                .await?
                .unwrap()
                .desired,
            "retire"
        );
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CalmError::Conflict("owner completion paused".into()));
        }
        Ok(())
    }
}
#[tokio::test]
async fn c4_retire_only_after_settlement_and_owner_completion() {
    let rig = Rig::new("exit").await;
    let (old, guard) = admit(&rig).await;
    let id = owner::execution_id(old.record()).unwrap();
    let probe = Arc::new(CompletionProbe {
        pool: rig.repo.db().clone(),
        fail: true.into(),
        calls: 0.into(),
    });
    let port = owner::ProviderExecution {
        owner: probe.clone(),
        drivers: rig.host.provider_execution.drivers.clone(),
    };
    assert!(
        port.finish(
            rig.repo.db(),
            rig.host.execution.engine(),
            &rig.worker_session_id,
            &old,
            None
        )
        .await
        .is_err()
    );
    assert_eq!(probe.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    port.stop(
        rig.repo.db(),
        rig.host.execution.engine(),
        &rig.worker_session_id,
        &old,
        owner::StopReason::Finished,
    )
    .await
    .unwrap();
    wait(async || {
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    drop(guard);
    port.sweep(rig.repo.db(), rig.host.execution.engine())
        .await
        .unwrap();
    assert!(claim(&rig).await.is_some());
    assert_eq!(desired(&rig, &id).await, "stop");
    probe.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    port.sweep(rig.repo.db(), rig.host.execution.engine())
        .await
        .unwrap();
    assert_eq!(probe.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(claim(&rig).await.is_none());
    assert_eq!(desired(&rig, &id).await, "retire");
}

#[tokio::test]
async fn c4_boot_stops_recorded_provider_execution() {
    let rig = Rig::new("exit").await;
    let (old, guard) = admit(&rig).await;
    let id = owner::execution_id(old.record()).unwrap();
    calm_server::claude_planner::lifecycle::boot(rig.repo.as_ref(), &rig.host)
        .await
        .unwrap();
    let stopped = claim(&rig).await.unwrap();
    assert!(matches!(
        stopped.record().decision,
        owner::Decision::Stopping {
            reason: owner::StopReason::KernelRestarted,
            ..
        }
    ));
    assert_eq!(desired(&rig, &id).await, "stop");
    assert!(
        rig.host
            .provider_execution
            .admission_gate(
                rig.repo.db(),
                rig.host.execution.engine(),
                &rig.worker_session_id
            )
            .await
            .is_err()
    );
    // Keep the process-local guard until this assertion, so a fast recovery tick
    // cannot legitimately inline-complete the record before the refusal check.
    drop(guard);
    wait(async || {
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    sweep(&rig).await;
    assert!(claim(&rig).await.is_none());
}

#[tokio::test]
async fn c4_planner_lost_post_commit_drive_recovers_by_tick() {
    let rig = Rig::new("exit").await;
    let (old, guard) = admit(&rig).await;
    let id = owner::execution_id(old.record()).unwrap();
    wait(async || {
        matches!(
            rig.host.execution.engine().unwrap().output(&id, None).await,
            Ok(ExecutionReply::Observation {
                state: ExecutionState::Spawned,
                ..
            })
        )
    })
    .await;
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "c4-stop-committed",
        id.as_str(),
        pause.clone(),
    );
    let port = rig.host.provider_execution.clone();
    let pool = rig.repo.db().clone();
    let engine = rig.host.execution.engine().unwrap().clone();
    let session = rig.worker_session_id.clone();
    let task = tokio::spawn(async move {
        port.stop(
            &pool,
            Some(&engine),
            &session,
            &old,
            owner::StopReason::Finished,
        )
        .await
    });
    pause.entered.notified().await;
    task.abort();
    let _ = task.await;
    drop(guard);
    wait(async || {
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    sweep(&rig).await;
    wait(async || {
        executions::load(rig.repo.db().read_pool(), id.as_str())
            .await
            .unwrap()
            .unwrap()
            .secrets_json
            .is_none()
    })
    .await;
    assert!(claim(&rig).await.is_none());
}
#[tokio::test]
async fn c4_planner_committed_retire_recovers_by_tick() {
    let rig = Rig::new("exit").await;
    let (old, guard) = admit(&rig).await;
    let id = owner::execution_id(old.record()).unwrap();
    rig.host
        .provider_execution
        .stop(
            rig.repo.db(),
            rig.host.execution.engine(),
            &rig.worker_session_id,
            &old,
            owner::StopReason::Finished,
        )
        .await
        .unwrap();
    wait(async || {
        owner::settlement(rig.repo.db(), &id)
            .await
            .unwrap()
            .is_some()
    })
    .await;
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "c4-retire-committed",
        id.as_str(),
        pause.clone(),
    );
    let port = rig.host.provider_execution.clone();
    let pool = rig.repo.db().clone();
    let engine = rig.host.execution.engine().unwrap().clone();
    let session = rig.worker_session_id.clone();
    let task = tokio::spawn(async move {
        port.finish(&pool, Some(&engine), &session, &old, None)
            .await
    });
    pause.entered.notified().await;
    task.abort();
    let _ = task.await;
    drop(guard);
    assert!(claim(&rig).await.is_none());
    wait(async || {
        executions::load(rig.repo.db().read_pool(), id.as_str())
            .await
            .unwrap()
            .unwrap()
            .secrets_json
            .is_none()
    })
    .await;
}

#[tokio::test]
async fn c4_release_authorization_precedes_first_byte() {
    let rig = Rig::new("exit").await;
    script(&rig, "touch ready; exit 0");
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "c4-release-authorized",
        &rig.worker_session_id,
        pause.clone(),
    );
    let task = {
        let session = rig.session().clone();
        let thread = rig.thread.clone();
        tokio::spawn(async move { session.prepare_edit_for_test(&thread).await })
    };
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        claim(&rig).await.unwrap().record().release,
        sql::Release::Releasing { .. }
    ));
    assert!(!rig.dir.path().join("ws/ready").exists());
    pause.release.notify_one();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        EditPreparation::Prepared { .. }
    ));
}

#[tokio::test]
async fn c4_release_continues_partial_payload_writes() {
    let rig = Rig::new("exit").await;
    script(&rig, "printf '%s' \"$HTTP_PROXY\" > received; exit 0");
    let proxy = format!("http://{}:password@proxy.invalid", "u".repeat(12000));
    let session = rig
        .open_session_with_environment(
            vec![("HTTP_PROXY".into(), "HTTP_PROXY".into(), proxy.clone())],
            rig.host.execution.engine().cloned(),
        )
        .await;
    assert!(matches!(
        session.prepare_edit_for_test(&rig.thread).await.unwrap(),
        EditPreparation::Prepared { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(rig.dir.path().join("ws/received")).unwrap(),
        proxy
    );
}

#[tokio::test]
async fn c4_rewind_proxy_override_order_is_preserved() {
    let rig = Rig::new("exit").await;
    script(&rig, "printf '%s' \"$HTTP_PROXY\" > received; exit 0");
    for (first, last) in [
        ("http://user:secret@proxy.invalid", "http://public.invalid"),
        ("http://public.invalid", "http://user:secret@proxy.invalid"),
    ] {
        let session = rig
            .open_session_with_environment(
                vec![
                    ("HTTP_PROXY".into(), "http_proxy".into(), first.into()),
                    ("HTTP_PROXY".into(), "http_proxy".into(), last.into()),
                ],
                rig.host.execution.engine().cloned(),
            )
            .await;
        assert!(matches!(
            session.prepare_edit_for_test(&rig.thread).await.unwrap(),
            EditPreparation::Prepared { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(rig.dir.path().join("ws/received")).unwrap(),
            last
        );
    }
}
