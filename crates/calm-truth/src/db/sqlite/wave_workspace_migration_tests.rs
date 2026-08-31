//! #1147 S1 — migration 0077 backfill (design D9) and the single-writer
//! projection it hands over to.

use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

use crate::model::{WaveWorkspace, WaveWorkspaceKind};

fn migrator_through(version: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

/// Design D9: every pre-#1147 wave becomes `attached`, pointing at its own
/// `cwd`, frozen at `created_at` — including the `$HOME` / `/tmp` / `/` rows
/// that #1131 left behind, which are deliberately backfilled rather than
/// repaired, and including the `cwd = ''` rows that migration 0018's own
/// backfill produced.
#[tokio::test]
async fn migration_0077_backfills_existing_waves_as_frozen_attached() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(76)
        .run(&pool)
        .await
        .expect("apply migrations through 0076");

    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, created_at, updated_at)
         VALUES ('cove-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed cove");
    // The kernel-owned system cove (#175) and the legacy `Today` wave that
    // `today_launchpad_ensure_tx` adopts and re-points.
    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
         VALUES ('cove-system', 'system', '#000', -1, 'system', 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed system cove");
    sqlx::query(
        "INSERT INTO waves (id, cove_id, title, sort, lifecycle, cwd, created_at, updated_at)
         VALUES ('w-today', 'cove-system', 'Today', 0, 'draft', '/home/kenji', 6000, 6000)",
    )
    .execute(&pool)
    .await
    .expect("seed legacy Today wave");

    // (id, cwd, created_at) — a real project dir, the three #1131 casualties,
    // and a pre-#250 row whose cwd never got one.
    let seeds = [
        ("w-repo", "/home/kenji/neige-calm", 1000_i64),
        ("w-home", "/home/kenji", 2000),
        ("w-tmp", "/tmp", 3000),
        ("w-root", "/", 4000),
        ("w-empty", "", 5000),
    ];
    for (id, cwd, created_at) in seeds {
        sqlx::query(
            "INSERT INTO waves (id, cove_id, title, sort, lifecycle, cwd, created_at, updated_at)
             VALUES (?1, 'cove-1', 't', 0, 'draft', ?2, ?3, ?3)",
        )
        .bind(id)
        .bind(cwd)
        .bind(created_at)
        .execute(&pool)
        .await
        .unwrap_or_else(|error| panic!("seed wave {id}: {error}"));
    }

    migrator_through(77)
        .run(&pool)
        .await
        .expect("apply migration 0077");

    for (id, cwd, created_at) in seeds {
        let row: (String, String, Option<i64>) = sqlx::query_as(
            "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id = ?1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|error| panic!("read back {id}: {error}"));
        assert_eq!(row.0, "attached", "{id}: every legacy wave is attached");
        assert_eq!(row.1, cwd, "{id}: workspace_path is backfilled from cwd");
        assert_eq!(
            row.2,
            Some(created_at),
            "{id}: legacy waves are frozen at created_at (D9) — an unfrozen \
             attached row is what a kind-blind PATCH branch would relocate"
        );
    }

    // D9's exception. The system cove's wave stays re-pointable, because
    // `today_launchpad_ensure_tx` re-points it. Freezing it here would put the
    // adopt branch in the position of either violating the latch or clearing a
    // stamp — the second is the subtler one, and it is what a blanket
    // `frozen_at = created_at` would have caused on any deployment that has a
    // legacy `Today` wave.
    let today: (String, String, Option<i64>) = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id='w-today'",
    )
    .fetch_one(&pool)
    .await
    .expect("read back w-today");
    assert_eq!(today.0, "attached");
    assert_eq!(today.1, "/home/kenji", "path still backfills from cwd");
    assert_eq!(
        today.2, None,
        "the system cove's wave must stay unfrozen (design D9 exception)"
    );

    // …and the exception is scoped: nothing outside the system cove escaped
    // the freeze.
    let unfrozen_outside_system: Vec<(String,)> = sqlx::query_as(
        "SELECT w.id FROM waves w JOIN coves c ON c.id = w.cove_id \
         WHERE w.workspace_frozen_at IS NULL AND c.kind != 'system'",
    )
    .fetch_all(&pool)
    .await
    .expect("scan unfrozen");
    assert!(
        unfrozen_outside_system.is_empty(),
        "waves outside the system cove must all be frozen, found {unfrozen_outside_system:?}"
    );
}

/// After migration 0077 there is one stored path. This pins that every way of
/// getting at it — the create return value, a fresh repo read, and the raw
/// column — yields the same bytes, and that the wire alias is computed from it
/// rather than from a second column (there is no second column).
#[tokio::test]
async fn workspace_writer_sets_kind_path_and_stamp_together() {
    let repo = super::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open repo");
    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, created_at, updated_at)
         VALUES ('cove-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(&repo.pool)
    .await
    .expect("seed cove");

    let mut tx = repo.pool.begin().await.expect("begin");
    let wave = super::wave_create_tx(
        &mut tx,
        crate::model::NewWave {
            cove_id: "cove-1".to_string().into(),
            title: "w".into(),
            sort: None,
            cwd: "/home/kenji/neige-calm".into(),
            workflow_id: None,
            plugin_scope: None,
            workflow_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::WaveWorkspacePlan::AttachedFromCwd,
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave");
    tx.commit().await.expect("commit");

    // S1: waves minted here (user coves) are attached and frozen at creation.
    // The launchpad wave is the D9 exception and is not created through this path.
    assert_eq!(wave.workspace.kind, WaveWorkspaceKind::Attached);
    assert_eq!(wave.workspace.path, "/home/kenji/neige-calm");
    assert_eq!(
        wave.workspace.frozen_at,
        Some(wave.created_at),
        "user-cove waves are minted already frozen (design D9 + D6: attached never re-points)"
    );
    // The wire alias is computed from the one stored column, not read from a
    // second one — `waves.cwd` no longer exists.
    assert_eq!(wave.cwd_wire_alias, wave.workspace.path);

    let row: (String, String, Option<i64>) = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id = ?1",
    )
    .bind(wave.id.as_str())
    .fetch_one(&repo.pool)
    .await
    .expect("read back");
    assert_eq!(row.0, "attached");
    assert_eq!(row.1, "/home/kenji/neige-calm");
    assert_eq!(row.2, Some(wave.created_at));

    // Re-pointing through the writer moves both columns together; that is the
    // whole point of it being one statement. (S1 has no route that does this;
    // the assertion pins the mechanism S3 will lean on.)
    let mut tx = repo.pool.begin().await.expect("begin");
    super::wave_workspace::wave_workspace_write_tx(
        &mut tx,
        wave.id.as_str(),
        &WaveWorkspace {
            kind: WaveWorkspaceKind::Managed,
            path: "/srv/neige-workspaces/cove-1/w".into(),
            frozen_at: None,
        },
    )
    .await
    .expect("rewrite workspace");
    tx.commit().await.expect("commit");

    let row: (String, String, Option<i64>) = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM waves WHERE id = ?1",
    )
    .bind(wave.id.as_str())
    .fetch_one(&repo.pool)
    .await
    .expect("read back");
    assert_eq!(row.0, "managed");
    assert_eq!(row.1, "/srv/neige-workspaces/cove-1/w");
    assert_eq!(row.2, None);

    // A read through the repo must surface the same thing — this is the
    // `SELECT` column-list trap (`query_as` binds by name at runtime): the
    // read path is what would blow up if a column list had gone stale.
    let read = crate::db::RepoRead::wave_get(&repo, wave.id.as_str())
        .await
        .expect("wave_get")
        .expect("wave exists");
    assert_eq!(read.workspace.kind, WaveWorkspaceKind::Managed);
    assert_eq!(read.workspace.path, "/srv/neige-workspaces/cove-1/w");
    assert_eq!(read.workspace.frozen_at, None);
    assert_eq!(read.cwd_wire_alias, read.workspace.path);
}

/// `wave_update_tx` reads the row back through `WaveRow` and rewrites it. It
/// must not touch the workspace — the freeze stamp and path survive a title
/// patch untouched. (A stale SELECT column list here would panic at runtime,
/// not compile time.)
#[tokio::test]
async fn wave_update_tx_leaves_the_workspace_alone() {
    let repo = super::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open repo");
    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, created_at, updated_at)
         VALUES ('cove-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(&repo.pool)
    .await
    .expect("seed cove");

    let mut tx = repo.pool.begin().await.expect("begin");
    let wave = super::wave_create_tx(
        &mut tx,
        crate::model::NewWave {
            cove_id: "cove-1".to_string().into(),
            title: "before".into(),
            sort: None,
            cwd: "/home/kenji/proj".into(),
            workflow_id: None,
            plugin_scope: None,
            workflow_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::WaveWorkspacePlan::AttachedFromCwd,
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave");
    let patched = super::wave_update_tx(
        &mut tx,
        wave.id.as_str(),
        crate::model::WavePatch {
            title: Some("after".into()),
            ..Default::default()
        },
    )
    .await
    .expect("patch wave");
    tx.commit().await.expect("commit");

    assert_eq!(patched.title, "after");
    assert_eq!(patched.workspace, wave.workspace);
    assert_eq!(patched.cwd_wire_alias, wave.cwd_wire_alias);
}

/// #1147 S1 — dropping `waves.cwd` must not move the **model layer's** answer.
///
/// Scope, stated precisely because an earlier version of this comment
/// overclaimed: this test lives in `calm-truth` and touches no `calm-server`
/// code. It covers the model-layer surfaces only — the stored column, the
/// `WaveRow` SELECT, `Wave::workspace`, the `cwd` serialization alias, and the
/// JSON round trip.
///
/// It does **not** cover the raw-SQL readers in `calm-server`
/// (`workspace_lease`, `task_verify_adapter`, `child_wave_adapter`), which the
/// compiler also cannot check because they name the column in a string. Those
/// are covered by `operation::workspace_lease::tests::`
/// `wave_sweep_uses_persisted_lease_paths_when_wave_cwd_is_deleted` and
/// `wave_release_sweeps_worktrees_plain_dirs_and_branches_post_commit`, both of
/// which go red if `workspace_lease/mod.rs`'s `row.try_get("workspace_path")`
/// is pointed back at `"cwd"` — that mutation is how the by-name read was
/// caught in the first place.
///
/// Compiling is not the evidence for what this test does cover:
/// `Wave::cwd_wire_alias` is a serialization alias that a typo could point at
/// the wrong string, so the assertions are on *values*.
#[tokio::test]
async fn every_path_reader_resolves_to_the_one_stored_column() {
    let repo = super::SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open repo");
    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, created_at, updated_at)
         VALUES ('cove-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(&repo.pool)
    .await
    .expect("seed cove");

    const PATH: &str = "/home/kenji/neige-calm";
    let mut tx = repo.pool.begin().await.expect("begin");
    let created = super::wave_create_tx(
        &mut tx,
        crate::model::NewWave {
            cove_id: "cove-1".to_string().into(),
            title: "w".into(),
            sort: None,
            cwd: PATH.into(),
            workflow_id: None,
            plugin_scope: None,
            workflow_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::WaveWorkspacePlan::AttachedFromCwd,
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave");
    tx.commit().await.expect("commit");

    // 1. The stored column — the single source.
    let stored: String = sqlx::query_scalar("SELECT workspace_path FROM waves WHERE id = ?1")
        .bind(created.id.as_str())
        .fetch_one(&repo.pool)
        .await
        .expect("read stored column");
    assert_eq!(stored, PATH);

    // 2. `waves.cwd` is gone — not shadowed, not defaulted, gone. If it still
    //    existed, some reader could keep answering from a stale copy.
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('waves')")
        .fetch_all(&repo.pool)
        .await
        .expect("read table info");
    assert!(
        !columns.iter().any(|c| c == "cwd"),
        "waves.cwd still exists; migration 0077 did not drop it. Columns: {columns:?}"
    );

    // 3. The create return value.
    assert_eq!(created.workspace.path, PATH);
    // 4. The wire alias on that same object. NOTE: this one is redundant given
    //    (3) — `cwd_wire_alias` is `workspace_path.clone()` inside
    //    `From<WaveRow>`, read here without going through serde, so it is
    //    near-tautological. It is kept only to document the field's existence.
    //    The assertion that actually carries weight is (6), which reads the
    //    key out of serialized JSON.
    assert_eq!(created.cwd_wire_alias, PATH);
    // 5. A fresh read through the repo — the `WaveRow` SELECT column list.
    let read = crate::db::RepoRead::wave_get(&repo, created.id.as_str())
        .await
        .expect("wave_get")
        .expect("wave exists");
    assert_eq!(read.workspace.path, PATH);
    assert_eq!(read.cwd_wire_alias, PATH);
    // 6. The JSON wire shape old clients parse: still a top-level `cwd`.
    let wire = serde_json::to_value(&read).expect("serialize wave");
    assert_eq!(
        wire["cwd"], PATH,
        "the `cwd` wire key must survive the column being dropped: {wire}"
    );
    assert_eq!(wire["workspace"]["path"], PATH, "{wire}");
    // 7. …and round-trips back, so a client echoing a wave still parses.
    let back: crate::model::Wave = serde_json::from_value(wire).expect("deserialize wave");
    assert_eq!(back.workspace.path, PATH);
}
