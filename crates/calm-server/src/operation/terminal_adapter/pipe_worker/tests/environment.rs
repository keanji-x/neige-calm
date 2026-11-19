use super::*;
use std::process::Command;

fn private_child(name: &str) -> bool {
    if std::env::var_os("TERMINAL_ENV_TEST_CHILD").is_none() {
        std::fs::create_dir_all("_local/tmp").unwrap();
        let home = tempfile::tempdir_in("_local/tmp").unwrap();
        let path = std::fs::canonicalize(home.path()).unwrap();
        let name = crate::test_support::exact_test_name(module_path!(), name);
        let output = Command::new(std::env::current_exe().unwrap())
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &path)
            .env("XDG_CONFIG_HOME", path.join("config"))
            .env("XDG_DATA_HOME", path.join("data"))
            .env("TMPDIR", &path)
            .env("TERMINAL_ENV_TEST_CHILD", "1")
            .output()
            .unwrap();
        crate::test_support::assert_isolated_test_passed(&output);
        return false;
    }
    true
}

#[tokio::test]
async fn terminal_workspace_environment_is_private_and_reaches_real_child() {
    if !private_child(stringify!(
        terminal_workspace_environment_is_private_and_reaches_real_child
    )) {
        return;
    }
    let h = Harness::new("printf '%s' \"${GH_TOKEN-unset}\" > child-token").await;
    std::fs::write(
        h.root.path().join(".envrc"),
        "export GH_TOKEN=synthetic-terminal-project\n",
    )
    .unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(h.root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    h.park().await;
    h.release().await;
    h.settled().await;
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("child-token")).unwrap(),
        "synthetic-terminal-project"
    );
    let prepared = h.op().await;
    assert!(
        !serde_json::to_string(&prepared.tx_output)
            .unwrap()
            .contains("synthetic-terminal-project")
    );
    let persisted: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT intent_json,secrets_json FROM workspace_executions")
            .fetch_all(h.db.pool())
            .await
            .unwrap();
    for (intent, secrets) in persisted {
        assert!(!intent.contains("synthetic-terminal-project"));
        assert!(
            !secrets
                .unwrap_or_default()
                .contains("synthetic-terminal-project")
        );
    }
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
}

#[tokio::test]
async fn terminal_workspace_changed_keys_refuse_release_before_business_command() {
    if !private_child(stringify!(
        terminal_workspace_changed_keys_refuse_release_before_business_command
    )) {
        return;
    }
    let h = Harness::new("touch business-started").await;
    let rc = h.root.path().join(".envrc");
    std::fs::write(&rc, "export PROJECT_VALUE=initial\n").unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(h.root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    h.park().await;
    std::fs::write(
        &rc,
        "export PROJECT_VALUE=changed\nexport EXTRA_PROJECT_KEY=new\n",
    )
    .unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(h.root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    h.release().await;
    h.settled().await;
    assert!(!h.root.path().join("business-started").exists());
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Failed);
}

#[tokio::test]
async fn terminal_workspace_closed_track_refuses_rc_before_release() {
    if !private_child(stringify!(
        terminal_workspace_closed_track_refuses_rc_before_release
    )) {
        return;
    }
    let h = Harness::new("touch business-started").await;
    let marker = h.root.path().join("rc-evaluated");
    std::fs::write(h.root.path().join(".envrc"), "touch rc-evaluated\n").unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(h.root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    h.park().await;
    let held = pipe::record(CONFIG, &h.op().await).unwrap();
    assert_eq!(held.release, Release::Held);
    assert!(
        marker.exists(),
        "authorized preparation must evaluate the project"
    );
    std::fs::remove_file(&marker).unwrap();

    // Older kernels could close a Track while this Terminal execution remained Held.
    // The current Close API refuses Terminal tasks, so inject that durable legacy state
    // through the typed row writer to exercise the release fence, not Close admission.
    h.db.track_update(
        &h.task.track_id,
        crate::model::TrackPatch {
            closed: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        h.db.track_get(&h.task.track_id)
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_some()
    );
    assert_eq!(pipe::record(CONFIG, &h.op().await).unwrap(), held);
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);

    // Exercise the production post-park release, including owner authorization and native stop.
    h.release().await;
    let stopped = pipe::record(CONFIG, &h.op().await).unwrap();
    assert_eq!(stopped.execution_id, held.execution_id);
    assert_eq!(stopped.release, Release::Held);
    assert_eq!(
        stopped.decision,
        Decision::Stopping {
            reason: StopReason::ReleaseRefused
        }
    );
    assert_eq!(h.op().await.phase, Phase::Parked);
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);
    assert_eq!(h.execution_row().await.0, "stop");
    h.settled().await;
    assert!(
        h.execution_row().await.1.is_some(),
        "the refused Held execution must retain real settlement evidence"
    );
    assert!(!h.root.path().join("business-started").exists());
    assert!(
        !marker.exists(),
        "a revoked task owner must be rejected before evaluating project code"
    );
}

#[tokio::test]
async fn terminal_workspace_checkout_blocked_skips_rc_and_retries_same_execution() {
    if !private_child(stringify!(
        terminal_workspace_checkout_blocked_skips_rc_and_retries_same_execution
    )) {
        return;
    }
    let h = Harness::new("printf once >> business-started").await;
    let marker = h.root.path().join("rc-evaluated");
    std::fs::write(h.root.path().join(".envrc"), "touch rc-evaluated\n").unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(h.root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    h.park().await;
    std::fs::remove_file(&marker).unwrap();
    let held = pipe::record(CONFIG, &h.op().await).unwrap();
    let sibling = format!("{}:follower", h.task.track_id);
    // Reuse the existing real checkout-occupancy fixture: another current writer is active.
    h.db.db().writer().execute(sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,'follower','terminal','true','{}','running','read_write',1,1)",
    ).bind(&sibling).bind(&h.task.track_id)).await.unwrap();
    let desired = h.execution_row().await.0;
    h.release().await;
    assert_eq!(pipe::record(CONFIG, &h.op().await).unwrap(), held);
    assert_eq!(h.execution_row().await.0, desired);
    assert!(!marker.exists());
    assert!(!h.root.path().join("business-started").exists());
    h.db.db()
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='done' WHERE id=?1").bind(&sibling))
        .await
        .unwrap();
    h.release().await;
    h.settled().await;
    assert_eq!(
        pipe::record(CONFIG, &h.op().await).unwrap().execution_id,
        held.execution_id
    );
    assert!(marker.exists());
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("business-started")).unwrap(),
        "once"
    );
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
}

#[tokio::test]
async fn terminal_workspace_source_read_failure_retries_same_held_execution() {
    if !private_child(stringify!(
        terminal_workspace_source_read_failure_retries_same_held_execution
    )) {
        return;
    }
    let h = Harness::new("printf once >> business-started").await;
    std::fs::write(h.root.path().join(".envrc"), "").unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(h.root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    h.park().await;
    let op = h.ops.claim_parked(&h.id).await.unwrap().unwrap();
    let held = pipe::record(CONFIG, &op).unwrap();
    let before = h.execution_row().await;
    let db = h.db.db();
    let operation_db = h.ctx.operation_repo.sqlite_db();
    // The only schema changed is this Harness's private database, shared with its real operation.
    assert!(std::ptr::eq(db.read_pool(), operation_db.read_pool()));
    let fault_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='terminal_release_unavailable_cards')")
        .fetch_one(db.read_pool()).await.unwrap();
    assert!(!fault_exists);
    db.writer()
        .execute(sqlx::query(
            "ALTER TABLE cards RENAME TO terminal_release_unavailable_cards",
        ))
        .await
        .unwrap();
    let result = h.adapter.act_pipe(&op, &h.ctx).await;
    // Always restore the real schema before a RED assertion or executor cleanup.
    db.writer()
        .execute(sqlx::query(
            "ALTER TABLE terminal_release_unavailable_cards RENAME TO cards",
        ))
        .await
        .unwrap();
    assert!(
        matches!(result, Err(CalmError::Db(_))),
        "a source dependency failure before sending must remain retryable"
    );
    assert_eq!(pipe::record(CONFIG, &h.op().await).unwrap(), held);
    assert_eq!(h.execution_row().await.0, before.0);
    assert_eq!(h.op().await.phase, Phase::Parked);
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);
    assert!(!h.root.path().join("business-started").exists());

    h.clear_claim().await;
    h.release().await;
    h.settled().await;
    let released = pipe::record(CONFIG, &h.op().await).unwrap();
    assert_eq!(released.execution_id, held.execution_id);
    assert_eq!(released.decision, Decision::Observing);
    assert!(matches!(released.release, Release::Released { .. }));
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("business-started")).unwrap(),
        "once"
    );
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn terminal_workspace_changed_selection_before_ready_retries_without_rc() {
    if !private_child(stringify!(
        terminal_workspace_changed_selection_before_ready_retries_without_rc
    )) {
        return;
    }
    let workspace_root = tempfile::tempdir().unwrap();
    let h = Harness::with_plan(
        "touch business-started",
        Some(crate::db::sqlite::TrackWorkspacePlan::ManagedUnder(
            workspace_root.path().into(),
        )),
    )
    .await;
    let before = h.db.track_get(&h.task.track_id).await.unwrap().unwrap();
    assert_eq!(before.workspace.frozen_at, None);
    workspace_runtime::selection::materialize_workspace(
        &before.workspace,
        workspace_root.path(),
        before.id.as_str(),
    )
    .unwrap();
    crate::test_support::init_fixture_git_repo(std::path::Path::new(&before.workspace.path));
    let op = h.ops.claim_drive_operation(&h.id).await.unwrap().unwrap();
    let selection = h
        .db
        .db()
        .writer()
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                let selection = super::super::environment::Selection::capture(&mut tx, &op.payload)
                    .await
                    .unwrap();
                tx.rollback().await.unwrap();
                selection
            })
        })
        .await
        .unwrap();
    let cwd = PathBuf::from(&selection.cwd);
    let source = crate::workspace_service::environment::source_for_track(
        h.db.as_ref(),
        &selection.track_id,
        &cwd,
    )
    .await
    .unwrap()
    .canonicalize()
    .unwrap();
    let marker = source.join("rc-evaluated");
    std::fs::write(source.join(".envrc"), "touch rc-evaluated\n").unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(&source)
            .output()
            .unwrap()
            .status
            .success()
    );
    let ready = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "workspace-environment-before-ready",
        source.to_str().unwrap(),
        ready.clone(),
    );
    let prepare = h.adapter.prepare_external(&op.payload, &op, &h.ctx);
    let change = async {
        tokio::time::timeout(Duration::from_secs(5), ready.entered.notified())
            .await
            .unwrap();
        // Freeze through the workspace owner while preparation's collected Selection is pending.
        h.db.db()
            .writer()
            .with_writer(|session| {
                Box::pin(async {
                    let mut tx = session.begin_immediate().await.unwrap();
                    assert!(
                        calm_truth::db::sqlite::track_workspace_freeze_tx(
                            &mut tx,
                            &h.task.track_id,
                            crate::model::now_ms(),
                        )
                        .await
                        .unwrap()
                    );
                    tx.commit().await.unwrap();
                })
            })
            .await
            .unwrap();
        let after = h.db.track_get(&h.task.track_id).await.unwrap().unwrap();
        assert!(after.workspace.frozen_at.is_some());
        assert_ne!(after.workspace, before.workspace);
        ready.release.notify_one();
    };
    let (prepared, ()) = tokio::join!(prepare, change);
    let prepared = prepared.unwrap();
    assert!(matches!(
        h.ops
            .prepare_tx_and_advance(&op, &h.adapter, prepared)
            .await
            .unwrap(),
        crate::operation::PrepareAdvance::Stale
    ));
    assert!(!marker.exists());
    assert!(h.op().await.tx_output.is_none());
    assert_eq!(h.op().await.phase, Phase::Pending);
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);
    // The unchanged current Selection can be collected and committed on the original claim.
    let prepared = h
        .adapter
        .prepare_external(&op.payload, &op, &h.ctx)
        .await
        .unwrap();
    assert!(
        h.ops
            .prepare_tx_and_advance(&op, &h.adapter, prepared)
            .await
            .unwrap()
            .committed()
            .is_some()
    );
    assert!(marker.exists());
    assert!(!cwd.join("business-started").exists());
}
