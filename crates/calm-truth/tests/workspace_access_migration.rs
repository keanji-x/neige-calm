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

#[tokio::test]
async fn workspace_access_explicit_cwd_uses_its_own_resource() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let other = root.path().join("other");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(source.to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    lease(
        &mut db,
        "reader",
        source.to_str().unwrap(),
        source.to_str().unwrap(),
        "read_only",
    )
    .await
    .unwrap();
    assert!(
        calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "terminal",
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
            Some(other.to_str().unwrap()),
            None
        )
        .await
        .unwrap()
    );
    assert!(
        !calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "terminal",
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
            Some(source.to_str().unwrap()),
            None
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn workspace_access_owned_writer_lineage_can_reenter_while_readers_wait() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(root.path().to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,created_at_ms,updated_at_ms, \
        access_mode,write_root_id,holder_kind,holder_id,holder_phase) VALUES('child','terminal','track',?1,'held','terminal',1,1,'read_write','parent','terminal','terminal','running')")
        .bind(root.path().to_str().unwrap()).execute(&mut db).await.unwrap();
    assert!(
        calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "planner",
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
            Some(root.path().to_str().unwrap()),
            Some("parent")
        )
        .await
        .unwrap()
    );
    assert!(
        !calm_truth::db::sqlite::track_available(
            &mut db,
            "track",
            "reader",
            calm_types::workspace_access::WorkspaceAccess::ReadOnly
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn workspace_access_writer_reentry_never_skips_a_reader() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(root.path().to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    lease(
        &mut db,
        "reader",
        root.path().to_str().unwrap(),
        root.path().to_str().unwrap(),
        "read_only",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE workspace_leases SET write_root_id='parent' WHERE lease_id='reader'")
        .execute(&mut db)
        .await
        .unwrap();
    assert!(
        !calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "planner",
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
            Some(root.path().to_str().unwrap()),
            Some("parent")
        )
        .await
        .unwrap()
    );
}

async fn inflight(
    db: &mut SqliteConnection,
    track: &str,
    id: &str,
    kind: &str,
    cwd: Option<&str>,
    access: &str,
) {
    let context = serde_json::json!({"neige_workspace":{"access":access}}).to_string();
    sqlx::query(
        r#"
INSERT INTO tasks(id,track_id,key,kind,goal,context_json,cwd,depends_on_json,status,spawn,
created_at_ms,updated_at_ms) VALUES(?1,?2,?1,?3,'work',?4,?5,'[]','dispatched',?6,1,1)
"#,
    )
    .bind(id)
    .bind(track)
    .bind(kind)
    .bind(context)
    .bind(cwd)
    .bind(calm_types::task_recovery::TASK_IN_TRACK_ROUTE)
    .execute(db)
    .await
    .unwrap();
}
async fn resource_track(db: &mut SqliteConnection, id: &str, path: &str) {
    sqlx::query(
        r#"
INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at,workspace_path)
VALUES(?1,'area',?1,1,1,2,?2)
"#,
    )
    .bind(id)
    .bind(path)
    .execute(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn workspace_inflight_checkout_does_not_block_explicit_independent_cwd() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let other = root.path().join("other");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&other).unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(source.to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    inflight(&mut db, "track", "running", "codex", None, "read_write").await;
    assert!(
        calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "next",
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
            Some(other.to_str().unwrap()),
            None
        )
        .await
        .unwrap(),
        "an in-flight task occupies its actual checkout, not every directory of its Track"
    );
}

#[tokio::test]
async fn workspace_inflight_foreign_track_blocks_overlapping_resources_before_lease() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    std::fs::create_dir(&child).unwrap();
    for kind in ["codex", "claude", "terminal"] {
        let mut db = database(129).await;
        sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
            .bind(root.path().to_str().unwrap())
            .execute(&mut db)
            .await
            .unwrap();
        resource_track(&mut db, "other", child.to_str().unwrap()).await;
        inflight(&mut db, "track", "running", kind, None, "read_write").await;
        for access in [
            calm_types::workspace_access::WorkspaceAccess::ReadOnly,
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
        ] {
            assert!(
                !calm_truth::db::sqlite::track_available(&mut db, "other", "next", access)
                    .await
                    .unwrap(),
                "{kind} claim must fence the physical subtree before its lease is created"
            );
        }
    }
}

#[tokio::test]
async fn workspace_inflight_terminal_explicit_cwds_are_independent() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let other = root.path().join("other");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&other).unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(root.path().to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    inflight(
        &mut db,
        "track",
        "running",
        "terminal",
        Some(source.to_str().unwrap()),
        "read_write",
    )
    .await;
    assert!(
        calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "next",
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
            Some(other.to_str().unwrap()),
            None
        )
        .await
        .unwrap()
    );
    assert!(
        !calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "next",
            calm_types::workspace_access::WorkspaceAccess::ReadOnly,
            Some(source.to_str().unwrap()),
            None
        )
        .await
        .unwrap(),
        "explicit terminal cwd must fence its own resource"
    );
}

#[tokio::test]
async fn workspace_unsettled_delivery_blocks_only_its_actual_resource() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let other = root.path().join("other");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&other).unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(source.to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    resource_track(&mut db, "other", source.to_str().unwrap()).await;
    lease(
        &mut db,
        "delivery-lease",
        source.to_str().unwrap(),
        source.to_str().unwrap(),
        "read_write",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE lease_id='delivery-lease'")
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query(
        r#"
INSERT INTO task_git_deliveries(delivery_id,track_id,producer_attempt_id,card_id,lease_id,
ordinal,operation_key,forge_idempotency_key,created_at_ms)
VALUES('delivery','track','producer','card','delivery-lease',0,'op-delivery','forge-delivery',1)
"#,
    )
    .execute(&mut db)
    .await
    .unwrap();
    assert!(
        calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "next",
            calm_types::workspace_access::WorkspaceAccess::ReadWrite,
            Some(other.to_str().unwrap()),
            None
        )
        .await
        .unwrap(),
        "delivery must not reserve unrelated explicit directories"
    );
    assert!(
        !calm_truth::db::sqlite::track_available(
            &mut db,
            "other",
            "reader",
            calm_types::workspace_access::WorkspaceAccess::ReadOnly
        )
        .await
        .unwrap(),
        "the pinned delivery lease fences the same physical directory across Tracks"
    );
}

#[tokio::test]
async fn workspace_delivery_admission_waits_for_handoff_then_orders_overlapping_deliveries() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(129).await;
    let path = root.path().to_str().unwrap();
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(path)
        .execute(&mut db)
        .await
        .unwrap();
    lease(&mut db, "first-lease", path, path, "read_write")
        .await
        .unwrap();
    sqlx::query(
        r#"
INSERT INTO task_git_deliveries(delivery_id,track_id,producer_attempt_id,card_id,lease_id,
ordinal,operation_key,forge_idempotency_key,created_at_ms)
VALUES('first','track','first-producer','card','first-lease',0,'first-op','first-forge',1)
"#,
    )
    .execute(&mut db)
    .await
    .unwrap();
    assert!(
        !calm_truth::db::sqlite::delivery_workspace_available(&mut db, "first")
            .await
            .unwrap(),
        "delivery may not ignore its still-held producer task lease"
    );
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE lease_id='first-lease'")
        .execute(&mut db)
        .await
        .unwrap();
    lease(&mut db, "second-lease", path, path, "read_write")
        .await
        .unwrap();
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE lease_id='second-lease'")
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query(
        r#"
INSERT INTO task_git_deliveries(delivery_id,track_id,producer_attempt_id,card_id,lease_id,
ordinal,operation_key,forge_idempotency_key,created_at_ms)
VALUES('second','track','second-producer','card','second-lease',0,'second-op','second-forge',2)
"#,
    )
    .execute(&mut db)
    .await
    .unwrap();
    assert!(
        calm_truth::db::sqlite::delivery_workspace_available(&mut db, "first")
            .await
            .unwrap(),
        "the first delivery does not wait on later work"
    );
    assert!(
        !calm_truth::db::sqlite::delivery_workspace_available(&mut db, "second")
            .await
            .unwrap(),
        "the second delivery waits on the overlapping predecessor"
    );
    sqlx::query(
        r#"
UPDATE task_git_deliveries SET settlement='failed',settled_event_id=1,failure_code='test',
failure_reason='test',retry_allowed=0,wake_reason='failed' WHERE delivery_id='first'
"#,
    )
    .execute(&mut db)
    .await
    .unwrap();
    assert!(
        calm_truth::db::sqlite::delivery_workspace_available(&mut db, "second")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn workspace_verifying_terminal_reserves_gate_cwd_not_its_finished_execution() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let gate = root.path().join("gate");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&gate).unwrap();
    let mut db = database(129).await;
    sqlx::query("UPDATE tracks SET workspace_path=?1 WHERE id='track'")
        .bind(source.to_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    inflight(
        &mut db,
        "track",
        "verifier",
        "terminal",
        Some(source.to_str().unwrap()),
        "read_write",
    )
    .await;
    sqlx::query("UPDATE tasks SET status='verifying',gate_json=?1 WHERE id='verifier'")
        .bind(serde_json::json!({"cwd":gate,"steps":[{"name":"gate","cmd":"true"}]}).to_string())
        .execute(&mut db)
        .await
        .unwrap();
    assert!(
        !calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "reader",
            calm_types::workspace_access::WorkspaceAccess::ReadOnly,
            Some(gate.to_str().unwrap()),
            None
        )
        .await
        .unwrap(),
        "a claimed verifier may act in its explicit gate directory before preparing its lease"
    );
    assert!(
        calm_truth::db::sqlite::workspace_available(
            &mut db,
            "track",
            "reader",
            calm_types::workspace_access::WorkspaceAccess::ReadOnly,
            Some(source.to_str().unwrap()),
            None
        )
        .await
        .unwrap(),
        "the finished command no longer occupies its old directory"
    );
}
