use super::*;
use calm_provider::provider::{
    CodexDaemonProbe, CodexLivenessFacts, LastTurnFacts, ThreadStatusLite, TurnStatusLite,
};
use std::sync::atomic::{AtomicU8, Ordering};

struct StopProbe(AtomicU8);
#[async_trait::async_trait]
impl CodexDaemonProbe for StopProbe {
    fn is_running(&self) -> bool {
        true
    }
    fn active_turn_id_for_thread(&self, _: &str) -> Option<String> {
        None
    }
    fn daemon_connected_at_ms(&self) -> i64 {
        0
    }
    async fn read_liveness_facts(&self, _: &str) -> Option<CodexLivenessFacts> {
        Some(CodexLivenessFacts {
            loaded: true,
            status: ThreadStatusLite::Idle,
            last_turn: Some(LastTurnFacts {
                status: TurnStatusLite::Completed,
                completed_at: Some(1),
            }),
        })
    }
    async fn background_terminals_stopped(&self, _: &str) -> Option<bool> {
        match self.0.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(false),
            _ => Some(true),
        }
    }
}

#[tokio::test]
async fn read_settlement_keeps_unknown_or_live_background_terminals() {
    let concrete = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let repo: Arc<dyn Repo> = concrete.clone();
    let area = repo
        .area_create(crate::model::NewArea {
            name: "read-stop".into(),
            color: "#101010".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            template_input: None,
            area_id: area.id,
            title: "read-stop".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let pool = concrete.pool();
    let mut task = super::super::tests::task("read-stop", TaskStatus::Done, &[], 0);
    task.id = format!("{}:read-stop", track.id);
    task.track_id = track.id.to_string();
    task.worker_card_id = Some("reader-card".into());
    let mut tx = pool.begin().await.unwrap();
    crate::test_support::insert_task_tx(&mut tx, &task)
        .await
        .unwrap();
    sqlx::query(
        r#"
        INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,
            target_json,payload_json,phase,created_at_ms,updated_at_ms)
        VALUES('reader-op','reader-op','codex-worker',?1,'hash','card','{}','{}','succeeded',1,1)
    "#,
    )
    .bind(&task.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,card_id,spawn_op_id,
            thread_id,created_at_ms,updated_at_ms)
        VALUES('reader-session',?1,'codex','resumable','executor','running','reader-card',
            'reader-op','reader-thread',1,1)
    "#,
    )
    .bind(track.id.as_str())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,
            access_mode,created_at_ms,updated_at_ms)
        VALUES('reader-lease','reader-card',?1,'/tmp','held','reader-op','read_only',1,1)
    "#,
    )
    .bind(track.id.as_str())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let probe = Arc::new(StopProbe(AtomicU8::new(0)));
    let scheduler = Scheduler::new(
        repo,
        EventBus::new(),
        WriteContext::new(
            concrete.card_role_cache().clone(),
            concrete.track_area_cache().clone(),
        ),
        Weak::<OperationRuntime>::new(),
        Arc::new(Semaphore::new(1)),
        std::env::temp_dir(),
        crate::scheduler::WorkerIdleWake::new(
            probe.clone(),
            Duration::ZERO,
            Duration::from_secs(1),
        ),
    );
    for background in [0, 1] {
        probe.0.store(background, Ordering::SeqCst);
        scheduler.settle_read_guards().await;
        let stopped: Option<i64> = sqlx::query_scalar(
            "SELECT read_stop_confirmed_at_ms FROM workspace_leases WHERE lease_id='reader-lease'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            stopped, None,
            "unknown/live background must retain the reader fence"
        );
    }
    probe.0.store(2, Ordering::SeqCst);
    scheduler.settle_read_guards().await;
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id='reader-lease'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        state, "released",
        "terminal turn and empty background roster release the reader"
    );
    let deliveries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_git_deliveries")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        deliveries, 0,
        "reader settlement never commits checkout contents"
    );
}
