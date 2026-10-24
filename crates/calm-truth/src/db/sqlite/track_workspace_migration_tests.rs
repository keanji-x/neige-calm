//! Workspace storage fixtures and the single-writer projection.

use crate::model::{TrackWorkspace, TrackWorkspaceKind};

/// Shared real pre-workspace storage fixture, using the released historical schema.
pub(super) async fn seed_pre_workspace_area(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, created_at, updated_at)
         VALUES ('area-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(pool)
    .await
    .expect("seed area");
}

pub(super) async fn seed_pre_workspace_track(
    pool: &sqlx::SqlitePool,
    id: &str,
    cwd: &str,
    created_at: i64,
) {
    sqlx::query(
        "INSERT INTO waves (id, cove_id, title, sort, lifecycle, cwd, created_at, updated_at)
             VALUES (?1, 'area-1', 't', 0, 'draft', ?2, ?3, ?3)",
    )
    .bind(id)
    .bind(cwd)
    .bind(created_at)
    .execute(pool)
    .await
    .unwrap_or_else(|error| panic!("seed track {id}: {error}"));
}

#[tokio::test]
async fn workspace_writer_sets_kind_path_and_stamp_together() {
    let repo = super::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open repo");
    sqlx::query(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at)
         VALUES ('area-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(repo.pool())
    .await
    .expect("seed area");

    let mut tx = repo.pool().begin().await.expect("begin");
    let track = super::track_create_tx(
        &mut tx,
        crate::model::NewTrack {
            area_id: "area-1".to_string().into(),
            title: "w".into(),
            sort: None,
            cwd: "/home/kenji/neige-calm".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("create track");
    tx.commit().await.expect("commit");

    assert_eq!(track.workspace.kind, TrackWorkspaceKind::Attached);
    assert_eq!(track.workspace.path, "/home/kenji/neige-calm");
    assert_eq!(
        track.workspace.frozen_at,
        Some(track.created_at),
        "user-area tracks are minted already frozen (design D9 + D6: attached never re-points)"
    );
    assert_eq!(track.cwd_wire_alias, track.workspace.path);

    let row: (String, String, Option<i64>) = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id = ?1",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .expect("read back");
    assert_eq!(row.0, "attached");
    assert_eq!(row.1, "/home/kenji/neige-calm");
    assert_eq!(row.2, Some(track.created_at));

    // The latch lives at the bottom write, not at the route.
    let mut tx = repo.pool().begin().await.expect("begin");
    let refused = super::track_workspace::track_workspace_write_tx(
        &mut tx,
        track.id.as_str(),
        &TrackWorkspace {
            kind: TrackWorkspaceKind::Managed,
            path: "/srv/neige-workspaces/area-1/w".into(),
            frozen_at: None,
            worktree: None,
        },
    )
    .await;
    let refusal = refused.expect_err("a frozen workspace must be refused by the writer itself");
    assert!(
        refusal.to_string().contains("frozen"),
        "the refusal must say the workspace is frozen, got {refusal}"
    );
    tx.rollback().await.expect("rollback");

    // Raw SQL on purpose: the writer has no un-freeze path.
    // Registered in `tests/track_write_point_registry.rs`.
    sqlx::query("UPDATE tracks SET workspace_frozen_at = NULL WHERE id = ?1")
        .bind(track.id.as_str())
        .execute(repo.pool())
        .await
        .expect("clear the freeze stamp");

    let mut tx = repo.pool().begin().await.expect("begin");
    super::track_workspace::track_workspace_write_tx(
        &mut tx,
        track.id.as_str(),
        &TrackWorkspace {
            kind: TrackWorkspaceKind::Managed,
            path: "/srv/neige-workspaces/area-1/w".into(),
            frozen_at: None,
            worktree: None,
        },
    )
    .await
    .expect("rewrite workspace");
    tx.commit().await.expect("commit");

    let row: (String, String, Option<i64>) = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id = ?1",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .expect("read back");
    assert_eq!(row.0, "managed");
    assert_eq!(row.1, "/srv/neige-workspaces/area-1/w");
    assert_eq!(row.2, None);

    // `query_as` binds by name at runtime, so the read path is what would blow up
    // if a column list had gone stale.
    let read = crate::db::RepoRead::track_get(&repo, track.id.as_str())
        .await
        .expect("track_get")
        .expect("track exists");
    assert_eq!(read.workspace.kind, TrackWorkspaceKind::Managed);
    assert_eq!(read.workspace.path, "/srv/neige-workspaces/area-1/w");
    assert_eq!(read.workspace.frozen_at, None);
    assert_eq!(read.cwd_wire_alias, read.workspace.path);
}

/// A stale SELECT column list here would panic at runtime, not compile time.
#[tokio::test]
async fn track_update_tx_leaves_the_workspace_alone() {
    let repo = super::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open repo");
    sqlx::query(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at)
         VALUES ('area-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(repo.pool())
    .await
    .expect("seed area");

    let mut tx = repo.pool().begin().await.expect("begin");
    let track = super::track_create_tx(
        &mut tx,
        crate::model::NewTrack {
            area_id: "area-1".to_string().into(),
            title: "before".into(),
            sort: None,
            cwd: "/home/kenji/proj".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("create track");
    let patched = super::track_update_tx(
        &mut tx,
        track.id.as_str(),
        crate::model::TrackPatch {
            title: Some("after".into()),
            ..Default::default()
        },
    )
    .await
    .expect("patch track");
    tx.commit().await.expect("commit");

    assert_eq!(patched.title, "after");
    assert_eq!(patched.workspace, track.workspace);
    assert_eq!(patched.cwd_wire_alias, track.cwd_wire_alias);
}

/// Dropping `tracks.cwd` must not move the model layer's answer. Assertions
/// are on *values*: `cwd_wire_alias` is a serialization alias a typo could
/// point at the wrong string.
#[tokio::test]
async fn every_path_reader_resolves_to_the_one_stored_column() {
    let repo = super::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open repo");
    sqlx::query(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at)
         VALUES ('area-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(repo.pool())
    .await
    .expect("seed area");

    const PATH: &str = "/home/kenji/neige-calm";
    let mut tx = repo.pool().begin().await.expect("begin");
    let created = super::track_create_tx(
        &mut tx,
        crate::model::NewTrack {
            area_id: "area-1".to_string().into(),
            title: "w".into(),
            sort: None,
            cwd: PATH.into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("create track");
    tx.commit().await.expect("commit");

    let stored: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id = ?1")
        .bind(created.id.as_str())
        .fetch_one(repo.pool())
        .await
        .expect("read stored column");
    assert_eq!(stored, PATH);

    // `tracks.cwd` is gone — not shadowed, not defaulted, gone.
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('tracks')")
        .fetch_all(repo.pool())
        .await
        .expect("read table info");
    assert!(
        !columns.iter().any(|c| c == "cwd"),
        "tracks.cwd still exists; migration 0077 did not drop it. Columns: {columns:?}"
    );

    assert_eq!(created.workspace.path, PATH);
    assert_eq!(created.cwd_wire_alias, PATH);
    let read = crate::db::RepoRead::track_get(&repo, created.id.as_str())
        .await
        .expect("track_get")
        .expect("track exists");
    assert_eq!(read.workspace.path, PATH);
    assert_eq!(read.cwd_wire_alias, PATH);
    // The JSON wire shape old clients parse: still a top-level `cwd`.
    let wire = serde_json::to_value(&read).expect("serialize track");
    assert_eq!(
        wire["cwd"], PATH,
        "the `cwd` wire key must survive the column being dropped: {wire}"
    );
    assert_eq!(wire["workspace"]["path"], PATH, "{wire}");
    let back: crate::model::Track = serde_json::from_value(wire).expect("deserialize track");
    assert_eq!(back.workspace.path, PATH);
}
