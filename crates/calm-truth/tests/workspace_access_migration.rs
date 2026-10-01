//! Durable read/read sharing and read/write exclusion at the SQL write boundary.
use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate};

async fn database(version: i64) -> SqliteConnection {
    let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    db.ensure_migrations_table().await.unwrap();
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version <= version && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    sqlx::raw_sql(
        r#"
INSERT INTO areas(id, name, color, sort, created_at, updated_at) VALUES('area', 'A', 'red', 0,
1, 2); INSERT INTO tracks(id, area_id, title, sort, created_at, updated_at) VALUES('track',
'area', 'T', 0, 1, 2);
"#,
    )
    .execute(&mut db)
    .await
    .unwrap();
    db
}

async fn lease(
    db: &mut SqliteConnection,
    id: &str,
    path: &str,
    canonical: &str,
    access: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
INSERT INTO workspace_leases(lease_id, card_id, track_id, path, state, lease_owner,
created_at_ms, updated_at_ms, base_sha, base_source, canonical_path, git_common_dir,
access_mode) VALUES(?1, ?1, 'track', ?2, 'held', ?1, 1, 1, 'sha', 'commit', ?3, '/repo/.git',
?4)
"#,
    )
    .bind(id)
    .bind(path)
    .bind(canonical)
    .bind(access)
    .execute(db)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn workspace_access_readers_share_and_writers_exclude_aliases() {
    let mut db = database(129).await;
    lease(&mut db, "reader-a", "/alias-a", "/real", "read_only")
        .await
        .unwrap();
    lease(&mut db, "reader-b", "/alias-b", "/real", "read_only")
        .await
        .unwrap();
    let error = lease(&mut db, "writer", "/alias-c", "/real", "read_write")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("workspace access conflict"));
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE access_mode='read_only'")
        .execute(&mut db)
        .await
        .unwrap();
    lease(&mut db, "writer", "/alias-c", "/real", "read_write")
        .await
        .unwrap();
    assert!(
        lease(&mut db, "late-reader", "/alias-d", "/real", "read_only")
            .await
            .unwrap_err()
            .to_string()
            .contains("workspace access conflict")
    );
}

#[tokio::test]
async fn workspace_access_mode_and_read_delivery_are_fenced() {
    let mut db = database(129).await;
    lease(&mut db, "reader", "/real", "/real", "read_only")
        .await
        .unwrap();
    for statement in [
        "UPDATE workspace_leases SET access_mode='read_write' WHERE lease_id='reader'",
        "UPDATE workspace_leases SET delivery_policy='kernel' WHERE lease_id='reader'",
    ] {
        assert!(
            sqlx::query(statement)
                .execute(&mut db)
                .await
                .unwrap_err()
                .to_string()
                .contains("workspace access is immutable")
        );
    }
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE lease_id='reader'")
        .execute(&mut db)
        .await
        .unwrap();
    lease(&mut db, "writer", "/real", "/real", "read_write")
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE workspace_leases SET state='held' WHERE lease_id='reader'")
            .execute(&mut db)
            .await
            .unwrap_err()
            .to_string()
            .contains("workspace access conflict")
    );
}

#[tokio::test]
async fn workspace_access_upgrade_keeps_historical_leases_exclusive() {
    let mut db = database(128).await;
    sqlx::query(
        r#"
INSERT INTO workspace_leases(lease_id, card_id, track_id, path, state, lease_owner,
created_at_ms, updated_at_ms) VALUES('legacy', 'card', 'track', '/real', 'held', 'op', 1, 1)
"#,
    )
    .execute(&mut db)
    .await
    .unwrap();
    let migration = MIGRATOR.iter().find(|m| m.version == 129).unwrap();
    db.apply(migration).await.unwrap();
    let access: String =
        sqlx::query_scalar("SELECT access_mode FROM workspace_leases WHERE lease_id='legacy'")
            .fetch_one(&mut db)
            .await
            .unwrap();
    assert_eq!(access, "read_write");
    assert!(
        lease(&mut db, "reader", "/real", "/real", "read_only")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn workspace_access_claim_blocks_foreign_track_writer_in_actual_checkout() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(root.path().to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) \
        VALUES('other','area','Other',1,1,2)",
    )
    .execute(&mut db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner, \
        created_at_ms,updated_at_ms,access_mode,holder_kind,holder_id,holder_phase,native_provider,write_root_id) \
        VALUES('native','writer','other',?1,'held','thread',1,1,'read_write','native','thread','running','codex','native')")
        .bind(root.path().to_str().unwrap()).execute(&mut db).await.unwrap();
    assert!(
        !calm_truth::db::sqlite::track_available(
            &mut db,
            "track",
            "reader",
            calm_types::workspace_access::WorkspaceAccess::ReadOnly
        )
        .await
        .unwrap(),
        "card ownership cannot hide a writer using another Track's physical checkout"
    );
}

#[tokio::test]
async fn workspace_access_parent_directory_writer_conflicts_with_child_reader() {
    let mut db = database(129).await;
    lease(&mut db, "reader", "/root/repo", "/root/repo", "read_only")
        .await
        .unwrap();
    assert!(
        lease(&mut db, "writer", "/root", "/root", "read_write")
            .await
            .is_err(),
        "a writable parent directory includes the reader's checkout"
    );
}
