use calm_truth::TruthError;
use calm_truth::db::{
    DaemonRecovery, RecoveryId, RecoveryWrite, RepoOutOfDomain, RepoRead, SharedCodexDaemonUpdate,
    SharedDaemonState, sqlite::SqlxRepo,
};

fn update(recovery: RecoveryWrite, pid: Option<i32>) -> SharedCodexDaemonUpdate {
    SharedCodexDaemonUpdate {
        recovery,
        state: SharedDaemonState::Failed,
        pid,
        pgid: pid,
        paths_are_legacy_display: false,
        sock_path: Some(b"/owned/socket".to_vec()),
        codex_home_path: Some(b"/owned/home".to_vec()),
        process_start_time: None,
        boot_id: None,
        started_at: None,
        last_error: None,
        increment_restart_count: false,
        daemon_env_signature: None,
    }
}
fn assert_conflict(result: Result<(), TruthError>) {
    assert!(matches!(
        result,
        Err(TruthError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
}

#[tokio::test]
async fn stale_scope_settlement_cannot_clear_new_scope() {
    let root = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("scope.db").display()
    );
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        repo.shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        DaemonRecovery::LegacyUntracked
    );
    let a = RecoveryId::fresh();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Begin { id: a }, None))
        .await
        .unwrap();
    assert!(
        repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Publish { id: a }, None))
            .await
            .is_err()
    );
    repo.shared_daemon_runtime_set(
        "default",
        update(RecoveryWrite::Publish { id: a }, Some(101)),
    )
    .await
    .unwrap();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Settle { id: a }, None))
        .await
        .unwrap();
    let b = RecoveryId::fresh();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Begin { id: b }, None))
        .await
        .unwrap();
    repo.shared_daemon_runtime_set(
        "default",
        update(RecoveryWrite::Publish { id: b }, Some(202)),
    )
    .await
    .unwrap();
    repo.db().close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_conflict(
        repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Settle { id: a }, None))
            .await,
    );
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(RecoveryWrite::Publish { id: a }, Some(101)),
        )
        .await,
    );
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(RecoveryWrite::Publish { id: b }, Some(303)),
        )
        .await,
    );
    assert_conflict(
        repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Legacy, None))
            .await,
    );
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(
                RecoveryWrite::Begin {
                    id: RecoveryId::fresh(),
                },
                None,
            ),
        )
        .await,
    );
    let saved = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(saved.recovery, DaemonRecovery::Recorded { id: b });
    assert_eq!(saved.pid, Some(202));
}

#[tokio::test]
async fn pending_and_failed_publication_survive_real_database_reopen() {
    let root = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("pending.db").display()
    );
    let repo = SqlxRepo::open(&url).await.unwrap();
    let id = RecoveryId::fresh();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Begin { id }, None))
        .await
        .unwrap();
    repo.db().writer().execute(sqlx::query("CREATE TRIGGER fail_publish BEFORE UPDATE ON codex_backend_daemons WHEN NEW.recovery_phase='recorded' BEGIN SELECT RAISE(ABORT,'publication refused'); END")).await.unwrap();
    for _ in 0..2 {
        assert!(
            repo.shared_daemon_runtime_set(
                "default",
                update(RecoveryWrite::Publish { id }, Some(202))
            )
            .await
            .is_err()
        );
    }
    repo.db().close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        repo.shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        DaemonRecovery::Pending { id }
    );
    assert_conflict(
        repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Legacy, None))
            .await,
    );
    repo.db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER fail_publish"))
        .await
        .unwrap();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Publish { id }, Some(202)))
        .await
        .unwrap();
    repo.db().writer().execute(sqlx::query("CREATE TRIGGER fail_settle BEFORE UPDATE ON codex_backend_daemons WHEN NEW.recovery_phase='clear' BEGIN SELECT RAISE(ABORT,'settlement refused'); END")).await.unwrap();
    assert!(
        repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Settle { id }, None))
            .await
            .is_err()
    );
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(
                RecoveryWrite::Begin {
                    id: RecoveryId::fresh(),
                },
                None,
            ),
        )
        .await,
    );
    assert_eq!(
        repo.shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        DaemonRecovery::Recorded { id }
    );
}

#[tokio::test]
async fn legacy_claim_preserves_facts_and_rejects_a_changed_snapshot() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Legacy, Some(101)))
        .await
        .unwrap();
    let id = RecoveryId::fresh();
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(RecoveryWrite::ClaimLegacy { id }, Some(202)),
        )
        .await,
    );
    repo.shared_daemon_runtime_set(
        "default",
        update(RecoveryWrite::ClaimLegacy { id }, Some(101)),
    )
    .await
    .unwrap();
    assert_eq!(
        repo.shared_daemon_runtime_get("default").await.unwrap().pid,
        Some(101)
    );
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(
                RecoveryWrite::ClaimLegacy {
                    id: RecoveryId::fresh(),
                },
                Some(101),
            ),
        )
        .await,
    );
}

#[tokio::test]
async fn migration_preserves_legacy_facts_and_codec_rejects_invalid_scope_id() {
    use sqlx::migrate::Migrator;
    use std::borrow::Cow;
    let root = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("legacy.db").display()
    );
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    let old = Migrator {
        migrations: Cow::Owned(
            calm_truth::MIGRATOR
                .iter()
                .filter(|m| m.version < 215)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    sqlx::query("UPDATE codex_backend_daemons SET pid=101,pgid=202,boot_id='original',sock_path=X'FF' WHERE backend_id='default'").execute(&pool).await.unwrap();
    pool.close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    let saved = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(saved.recovery, DaemonRecovery::LegacyUntracked);
    assert_eq!((saved.pid, saved.pgid), (Some(101), Some(202)));
    assert_eq!(saved.sock_path, Some(vec![255]));
    assert_eq!(saved.boot_id.as_deref(), Some("original"));
    repo.db().writer().execute(sqlx::query("UPDATE codex_backend_daemons SET recovery_phase='recorded',recovery_id='invalid' WHERE backend_id='default'")).await.unwrap();
    assert!(matches!(
        repo.shared_daemon_runtime_get("default").await,
        Err(TruthError::Internal(_))
    ));
    assert!(repo.db().writer().execute(sqlx::query("UPDATE codex_backend_daemons SET recovery_phase='clear',recovery_id=NULL WHERE backend_id='default'")).await.is_err());
}

#[tokio::test]
async fn reserved_cancel_is_pending_only_and_same_id() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let a = RecoveryId::fresh();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Begin { id: a }, None))
        .await
        .unwrap();
    assert_conflict(
        repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Settle { id: a }, None))
            .await,
    );
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(
                RecoveryWrite::CancelReserved {
                    id: RecoveryId::fresh(),
                },
                None,
            ),
        )
        .await,
    );
    repo.shared_daemon_runtime_set(
        "default",
        update(RecoveryWrite::CancelReserved { id: a }, None),
    )
    .await
    .unwrap();
    let b = RecoveryId::fresh();
    repo.shared_daemon_runtime_set("default", update(RecoveryWrite::Begin { id: b }, None))
        .await
        .unwrap();
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(RecoveryWrite::CancelReserved { id: a }, None),
        )
        .await,
    );
    assert_eq!(
        repo.shared_daemon_runtime_get("default")
            .await
            .unwrap()
            .recovery,
        DaemonRecovery::Pending { id: b }
    );
    repo.shared_daemon_runtime_set(
        "default",
        update(RecoveryWrite::Publish { id: b }, Some(202)),
    )
    .await
    .unwrap();
    assert_conflict(
        repo.shared_daemon_runtime_set(
            "default",
            update(RecoveryWrite::CancelReserved { id: b }, None),
        )
        .await,
    );
    assert_eq!(
        repo.shared_daemon_runtime_get("default").await.unwrap().pid,
        Some(202)
    );
}
