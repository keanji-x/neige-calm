use super::*;

#[tokio::test]
async fn w_terminal_worker_prepare_freezes_managed_workspace() {
    let workspace_root = tempfile::tempdir().unwrap();
    let h = Harness::with_plan(
        "exit 0",
        Some(crate::db::sqlite::TrackWorkspacePlan::ManagedUnder(
            workspace_root.path().into(),
        )),
    )
    .await;
    let before: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT t.workspace_frozen_at,b.frozen_at_ms FROM tracks t \
         JOIN track_workspace_bindings b ON b.track_id=t.id WHERE t.id=?1",
    )
    .bind(&h.task.track_id)
    .fetch_one(h.db.pool())
    .await
    .unwrap();
    assert_eq!(before, (None, None));
    h.prepare().await;
    let after: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT t.workspace_frozen_at,b.frozen_at_ms FROM tracks t \
         JOIN track_workspace_bindings b ON b.track_id=t.id WHERE t.id=?1",
    )
    .bind(&h.task.track_id)
    .fetch_one(h.db.pool())
    .await
    .unwrap();
    assert!(after.0.is_some());
    assert_eq!(after.0, after.1);
}

#[tokio::test]
async fn w_terminal_worker_child_environment_allowlist() {
    const MARKER: &str = "WT2_ENV_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["operation::terminal_adapter::pipe_worker::tests::preparation::w_terminal_worker_child_environment_allowlist", "--exact", "--nocapture", "--test-threads=1"])
            .env(MARKER, "1")
            .env("NODE_EXTRA_CA_CERTS", "/tmp/wt2-ca.pem")
            .env("WT2_UNREVIEWED_SECRET", "must-not-inherit")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let h = Harness::new("printf '%s\\n%s' \"${NODE_EXTRA_CA_CERTS-unset}\" \"${WT2_UNREVIEWED_SECRET-unset}\" > child-env").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("child-env")).unwrap(),
        "/tmp/wt2-ca.pem\nunset"
    );
}

#[tokio::test]
async fn w_terminal_worker_ambient_secret_proxy_refused_before_admit() {
    const MARKER: &str = "WT2_PROXY_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                concat!(
                    "operation::terminal_adapter::pipe_worker::tests::preparation::",
                    "w_terminal_worker_ambient_secret_proxy_refused_before_admit"
                ),
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MARKER, "1")
            .env("ALL_PROXY", "http://user:password@proxy.example")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let h = Harness::new("exit 0").await;
    let op = h.ops.claim_drive_operation(&h.id).await.unwrap().unwrap();
    assert!(matches!(
        h.adapter.prepare_external(&op.payload, &op, &h.ctx).await,
        Err(CalmError::BadRequest(_))
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(h.op().await.tx_output.is_none());
}

#[tokio::test]
async fn w_terminal_worker_ambient_values_not_persisted() {
    const NAME: &str = "operation::terminal_adapter::pipe_worker::tests::preparation::w_terminal_worker_ambient_values_not_persisted";
    const SOCK: &str = "/tmp/wt2-ambient-agent.sock";
    const CERT: &str = "/tmp/wt2-ambient-cert.pem";
    const MARKER: &str = "WT2_AMBIENT_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([NAME, "--exact", "--nocapture", "--test-threads=1"])
            .env(MARKER, "1")
            .env("SSH_AUTH_SOCK", SOCK)
            .env("SSL_CERT_FILE", CERT)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let h = Harness::new("printf '%s\\n%s' \"$SSH_AUTH_SOCK\" \"$SSL_CERT_FILE\" > ambient-values")
        .await;
    h.park().await;
    h.release().await;
    h.settled().await;
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("ambient-values")).unwrap(),
        format!("{SOCK}\n{CERT}")
    );
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT intent_json,secrets_json FROM workspace_executions")
            .fetch_one(h.db.pool())
            .await
            .unwrap();
    let record = pipe::record(CONFIG, &h.op().await).unwrap();
    let journal = h
        ._executor
        ._root
        .path()
        .join("journal")
        .join(format!("{}.execution", record.execution_id.as_str()));
    let journal = std::fs::read_to_string(journal).unwrap();
    for value in [SOCK, CERT] {
        assert!(!row.0.contains(value), "ambient value persisted in intent");
        assert!(
            !row.1.as_deref().unwrap_or("").contains(value),
            "ambient value persisted in secrets"
        );
        assert!(
            !journal.contains(value),
            "ambient value persisted in journal"
        );
    }
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
}
