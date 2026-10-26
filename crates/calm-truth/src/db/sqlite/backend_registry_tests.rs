use super::{
    SqlxRepo,
    backend_registry::{BackendIdentity, RegistryError, reconcile},
};

fn identity(id: &str, home: &str) -> BackendIdentity {
    BackendIdentity {
        provider: "codex".into(),
        backend_id: id.into(),
        protocol: "codex".into(),
        configured_home: home.as_bytes().to_vec(),
        canonical_home: home.as_bytes().to_vec(),
    }
}

#[tokio::test]
async fn agent_backend_registry_rejects_changed_home() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    reconcile(repo.db(), &[identity("default", "/old")], b"/old")
        .await
        .unwrap();
    assert!(matches!(
        reconcile(repo.db(), &[identity("default", "/new")], b"/old").await,
        Err(RegistryError::Changed(..))
    ));
}

#[tokio::test]
async fn agent_backend_registry_rejects_alias_protocol_removal_and_reuse() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let default = identity("default", "/default");
    let other = identity("other", "/other");
    reconcile(repo.db(), &[default.clone(), other.clone()], b"/default")
        .await
        .unwrap();
    reconcile(
        repo.db(),
        &[other.clone(), default.clone()],
        b"/ignored-after-registration",
    )
    .await
    .unwrap();
    let mut alias = other.clone();
    alias.configured_home = b"/alias".to_vec();
    assert!(matches!(
        reconcile(repo.db(), &[default.clone(), alias], b"/default").await,
        Err(RegistryError::Changed(..))
    ));
    let mut protocol = other.clone();
    protocol.protocol = "claude".into();
    assert!(matches!(
        reconcile(repo.db(), &[default.clone(), protocol], b"/default").await,
        Err(RegistryError::Changed(..))
    ));
    assert!(matches!(
        reconcile(repo.db(), std::slice::from_ref(&default), b"/default").await,
        Err(RegistryError::Removed(..))
    ));
    assert!(matches!(
        reconcile(
            repo.db(),
            &[default.clone(), other.clone(), identity("reused", "/other")],
            b"/default"
        )
        .await,
        Err(RegistryError::Reused)
    ));
    // SQL cannot erase an identity to make its id reusable.
    assert!(
        sqlx::query("DELETE FROM agent_backend_registry WHERE backend_id='other'")
            .execute(repo.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE agent_backend_registry SET canonical_home='/new' WHERE backend_id='other'"
        )
        .execute(repo.pool())
        .await
        .is_err()
    );
}

#[tokio::test]
async fn agent_backend_registry_upgrade_requires_original_default() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    assert!(matches!(
        reconcile(
            repo.db(),
            &[identity("default", "/replacement")],
            b"/original"
        )
        .await,
        Err(RegistryError::LegacyDefault)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_backend_registry")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    reconcile(repo.db(), &[identity("default", "/original")], b"/original")
        .await
        .unwrap();
}

#[tokio::test]
async fn agent_backend_upgrade_preserves_daemon_and_rejects_unknown_migration() {
    use crate::db::RepoRead;
    use sqlx::migrate::Migrator;
    use std::borrow::Cow;
    let tmp = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("upgrade.db").display()
    );
    let old = Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version < 198)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    old.run(&pool).await.unwrap();
    sqlx::query("UPDATE shared_codex_daemon SET state='running', pid=123, pgid=124, sock_path='/old.sock', codex_home_path='/old', process_start_time=999, boot_id='boot', started_at=111, updated_at=222, restart_count=3, last_error='old-error', daemon_env_signature='signature'").execute(&pool).await.unwrap();
    pool.close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    let record = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(record.pid, Some(123));
    assert_eq!(record.pgid, Some(124));
    assert!(record.paths_are_legacy_display);
    assert_eq!(record.sock_path.as_deref(), Some(b"/old.sock".as_slice()));
    assert_eq!(record.codex_home_path.as_deref(), Some(b"/old".as_slice()));
    assert_eq!(record.process_start_time, Some(999));
    assert_eq!(record.boot_id.as_deref(), Some("boot"));
    assert_eq!(record.started_at, Some(111));
    assert_eq!(record.updated_at, 222);
    assert_eq!(record.restart_count, 3);
    assert_eq!(record.last_error.as_deref(), Some("old-error"));
    assert_eq!(record.daemon_env_signature.as_deref(), Some("signature"));
    assert_eq!(record.state, crate::db::SharedDaemonState::Running);
    reconcile(
        repo.db(),
        &[identity("default", "/old"), identity("other", "/other")],
        b"/old",
    )
    .await
    .unwrap();
    assert_eq!(
        repo.shared_daemon_runtime_get("other").await.unwrap().pid,
        None
    );
    assert_eq!(
        repo.shared_daemon_runtime_get("default").await.unwrap().pid,
        Some(123)
    );
    assert!(
        super::check_no_unknown_future_migrations(repo.pool(), &old)
            .await
            .is_err()
    );
    sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9999,'future',1,X'00',0)").execute(repo.pool()).await.unwrap();
    repo.pool().close().await;
    assert!(SqlxRepo::open(&url).await.is_err());
}
