//! Real Pipe observations must wake on execution changes without idle writer churn.
use super::*;

async fn observer_done(h: &Harness, observer: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(8), observer)
        .await
        .expect("execution changes complete the observer without recovery sweeps")
        .unwrap();
    assert_eq!(h.op().await.phase, Phase::Succeeded);
}

#[tokio::test]
async fn quiet_pipe_observer_and_sweeps_do_not_claim() {
    let h = Harness::new(
        "i=0; until [ -e finish ] || [ \"$i\" -ge 200 ]; do sleep .05; i=$((i+1)); done",
    )
    .await;
    h.park().await;
    h.release().await;
    let db = h.db.db();
    db.writer()
        .execute(sqlx::query("CREATE TABLE idle_claims (op_id TEXT)"))
        .await
        .unwrap();
    db.writer()
        .execute(sqlx::query(
            "CREATE TRIGGER observe_idle_claims AFTER UPDATE OF lease_owner ON operations \
         WHEN NEW.lease_owner IS NOT NULL BEGIN INSERT INTO idle_claims VALUES (NEW.id); END",
        ))
        .await
        .unwrap();
    let record = pipe::record(CONFIG, &h.op().await).unwrap();
    let observer = tokio::spawn(h.adapter.clone().observe_pipe(h.ctx.clone(), h.id.clone()));
    for _ in 0..3 {
        h.ctx.execution.drive([record.execution_id.clone()]);
        h.runtime.sweep_parked().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let writes: i64 = sqlx::query_scalar("SELECT count(*) FROM idle_claims WHERE op_id=?1")
        .bind(&h.id)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    std::fs::write(h.root.path().join("finish"), b"").unwrap();
    observer_done(&h, observer).await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
    assert_eq!(
        writes, 0,
        "stable Pipe execution and steady sweeps must not churn claims"
    );
}

#[tokio::test]
async fn held_pipe_observer_releases_and_completes_without_sweeps() {
    let h = Harness::new("printf observed-tail").await;
    h.park().await;
    let observer = tokio::spawn(h.adapter.clone().observe_pipe(h.ctx.clone(), h.id.clone()));
    observer_done(&h, observer).await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
    assert!(
        h.event_output().await["result"]["pty_output"]
            .as_str()
            .unwrap()
            .contains("observed-tail")
    );
}

#[tokio::test]
async fn releasing_pipe_observer_stops_without_resending() {
    let h = Harness::new("printf forbidden > ran").await;
    h.park().await;
    let op = h.ops.claim_parked(&h.id).await.unwrap().unwrap();
    let old = pipe::record(CONFIG, &op).unwrap();
    let mut new = old.clone();
    new.release = Release::Releasing {
        authorized_at_ms: crate::model::now_ms(),
    };
    pipe::transition(
        CONFIG,
        &h.ctx,
        &op,
        &old,
        &new,
        Some(&Authority(&h.task.id)),
    )
    .await
    .unwrap();
    h.clear_claim().await;
    let observer = tokio::spawn(h.adapter.clone().observe_pipe(h.ctx.clone(), h.id.clone()));
    observer_done(&h, observer).await;
    assert!(!h.root.path().join("ran").exists());
    assert_eq!(h.status().await, crate::model::TaskStatus::Failed);
}

#[tokio::test]
async fn pipe_observer_retries_contended_claim_without_new_execution_event() {
    let h = Harness::new("exit 0").await;
    h.park().await;
    h.db.db()
        .writer()
        .execute(
            sqlx::query(
                "UPDATE operations SET lease_owner='another-owner',lease_until_ms=?2 WHERE id=?1",
            )
            .bind(&h.id)
            .bind(crate::model::now_ms() + 150),
        )
        .await
        .unwrap();
    let observer = tokio::spawn(h.adapter.clone().observe_pipe(h.ctx.clone(), h.id.clone()));
    observer_done(&h, observer).await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
}

#[tokio::test]
async fn terminal_pipe_observer_exits_with_offline_executor() {
    let h = Harness::new("exit 0").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    h.complete().await;
    let socket = h._executor._socket.path().join("control.sock");
    let offline = socket.with_extension("offline");
    std::fs::rename(&socket, &offline).unwrap();
    let record = pipe::record(CONFIG, &h.op().await).unwrap();
    assert!(
        h.ctx
            .execution
            .engine()
            .unwrap()
            .output(&record.execution_id, None)
            .await
            .is_err()
    );
    let exited = tokio::time::timeout(
        Duration::from_millis(2500),
        h.adapter.clone().observe_pipe(h.ctx.clone(), h.id.clone()),
    )
    .await;
    std::fs::rename(offline, socket).unwrap();
    assert!(
        exited.is_ok(),
        "a durable terminal operation does not wait for transport recovery"
    );
}
