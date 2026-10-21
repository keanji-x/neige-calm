//! Tick regressions sharing the real typed supervisor and Terminal projection fixture.
use super::*;
use clap::Parser;
use std::sync::atomic::Ordering::SeqCst;

async fn fixture() -> (
    Fixture,
    Arc<calm_server::test_seams::ExecutionRecoveryFaults>,
) {
    let mut f = Fixture::new().await;
    let key = f._temp.path().join("kernel-key.json");
    std::fs::write(&key, serde_json::to_vec(&f.secret).unwrap()).unwrap();
    let mut cfg = calm_server::config::Config::parse_from(["calm-server"]);
    cfg.kernel_client_key_file = Some(key);
    cfg.proc_supervisor_sock = Some(f._socket.path().join("control.sock"));
    cfg.execution_recovery_interval = Duration::from_millis(100);
    let faults = cfg.execution_recovery_faults.clone();
    f.engine = calm_server::execution_boot::recover(&cfg, f.repo.db().clone())
        .await
        .unwrap()
        .unwrap();
    (f, faults)
}
async fn admit(f: &Fixture, script: &str) -> (ExecutionId, Terminal) {
    let (_, terminal) = f.terminal().await;
    let mut intent = exec::test_support::intent(f.root.clone());
    intent.metadata.consumer = calm_server::operation::terminal_execution_create::CONSUMER.into();
    intent.metadata.alias = ProcessAlias(sql::terminal_alias(&terminal.id));
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = vec!["-c".into(), script.into()];
    let id = intent.metadata.id.clone();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let ticket = f.engine.admit(&mut tx, intent).await.unwrap();
    tx.commit().await.unwrap();
    f.engine.drive(ticket.committed());
    (id, terminal)
}
async fn settled(f: &Fixture, id: &ExecutionId) -> bool {
    sqlx::query_scalar(
        "SELECT settlement_json IS NOT NULL FROM workspace_executions WHERE execution_id=?",
    )
    .bind(id.as_str())
    .fetch_one(f.repo.pool())
    .await
    .unwrap()
}
#[tokio::test]
async fn c4_recovery_tick_redrives_typed_terminal_after_store_error() {
    let (f, faults) = fixture().await;
    faults.arm("spawned");
    let flag = f.root.join("exit");
    let (id, terminal) = admit(&f, "until [ -f exit ]; do sleep 0.01; done; exit 7").await;
    wait(|| async { faults.failures.load(SeqCst) == 1 }).await;
    // This Store::spawned seam returns StoreError; a tick may already clear Engine::error.
    std::fs::write(flag, "").unwrap();
    wait(|| settled(&f, &id)).await;
    assert_eq!(
        f.repo
            .terminal_get(&terminal.id)
            .await
            .unwrap()
            .unwrap()
            .exit_code,
        Some(7)
    );
}
#[tokio::test]
async fn c4_recovery_tick_is_idempotent_with_live_slots() {
    let (f, faults) = fixture().await;
    let (id, _) = admit(
        &f,
        "echo once >> launches; until [ -f exit ]; do sleep 0.01; done",
    )
    .await;
    wait(|| async { f.root.join("launches").exists() }).await;
    let before = faults.pending_calls.load(SeqCst);
    wait(|| async { faults.pending_calls.load(SeqCst) >= before + 2 }).await;
    assert!(!settled(&f, &id).await);
    assert_eq!(
        std::fs::read_to_string(f.root.join("launches")).unwrap(),
        "once\n"
    );
    std::fs::write(f.root.join("exit"), "").unwrap();
    wait(|| settled(&f, &id)).await;
}
#[tokio::test]
async fn c4_recovery_tick_retries_after_pending_read_error() {
    let (f, faults) = fixture().await;
    // Drop the initial ticket after COMMIT: only recovery may discover this row.
    let (_, terminal) = f.terminal().await;
    let mut intent = exec::test_support::intent(f.root.clone());
    intent.metadata.consumer = calm_server::operation::terminal_execution_create::CONSUMER.into();
    intent.metadata.alias = ProcessAlias(sql::terminal_alias(&terminal.id));
    intent.metadata.program.program = "/bin/true".into();
    let id = intent.metadata.id.clone();
    faults.arm("pending");
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _lost = f.engine.admit(&mut tx, intent).await.unwrap();
    tx.commit().await.unwrap();
    wait(|| async { faults.failures.load(SeqCst) == 1 }).await;
    wait(|| settled(&f, &id)).await;
    assert!(faults.pending_calls.load(SeqCst) >= 3);
}
