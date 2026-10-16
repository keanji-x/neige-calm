//! Production writer/admission tests for persistent resource identity.

use std::borrow::Cow;

use super::{
    CheckoutOccupancy, SqlxRepo, TrackWorkspacePlan, area_create_tx, checkout_occupancy,
    track_create_tx, track_workspace_freeze_tx, track_workspace_write_tx,
};
use crate::{
    db::RepoRead,
    model::{NewArea, NewTrack, RequestTheme, Track, TrackWorkspace, TrackWorkspaceKind},
};

async fn create(repo: &SqlxRepo, cwd: &str, plan: TrackWorkspacePlan) -> Track {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "workspace resource".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "resource".into(),
            sort: None,
            cwd: cwd.into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &plan,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    track
}

async fn resource(repo: &SqlxRepo, track: &Track) -> String {
    sqlx::query_scalar("SELECT workspace_id FROM track_workspace_bindings WHERE track_id=?1")
        .bind(track.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

async fn occupancy(repo: &SqlxRepo, track: &Track, except: &str) -> CheckoutOccupancy {
    checkout_occupancy(
        &mut repo.pool().acquire().await.unwrap(),
        track.id.as_str(),
        except,
    )
    .await
    .unwrap()
}

async fn running(repo: &SqlxRepo, track: &Track, access: &str) -> String {
    let id = format!("{}:attempt", track.id);
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,'attempt','terminal','g','{}','running',?3,1,1)",
    ).bind(&id).bind(track.id.as_str()).bind(access).execute(repo.pool()).await.unwrap();
    id
}

#[tokio::test]
async fn workspace_resource_independent_selections_preserve_existing_track_admission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let first = create(
        &repo,
        "/shared/checkout",
        TrackWorkspacePlan::AttachedFromCwd,
    )
    .await;
    let second = create(
        &repo,
        "/shared/checkout",
        TrackWorkspacePlan::AttachedFromCwd,
    )
    .await;
    assert_ne!(
        resource(&repo, &first).await,
        resource(&repo, &second).await
    );
    let reader = running(&repo, &first, "read_only").await;
    assert_eq!(
        occupancy(&repo, &first, "").await,
        CheckoutOccupancy::Readers
    );
    assert!(
        occupancy(&repo, &first, "")
            .await
            .admits(crate::model::TaskAccess::ReadOnly)
    );
    assert!(
        !occupancy(&repo, &first, "")
            .await
            .admits(crate::model::TaskAccess::ReadWrite)
    );
    assert_eq!(
        occupancy(&repo, &first, &reader).await,
        CheckoutOccupancy::Free
    );
    sqlx::query(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner, \
         created_at_ms,updated_at_ms,access_mode) VALUES \
         ('writer','writer',?1,'/shared/checkout','held','owner',1,1,'read_write')",
    )
    .bind(first.id.as_str())
    .execute(repo.pool())
    .await
    .unwrap();
    assert_eq!(occupancy(&repo, &first, "").await, CheckoutOccupancy::Busy);
    assert_eq!(
        occupancy(&repo, &second, "").await,
        CheckoutOccupancy::Free,
        "identity rollout does not activate shared admission"
    );
}

#[tokio::test]
async fn workspace_resource_owned_worktrees_do_not_share_the_source_repository_identity() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let first = create(
        &repo,
        "/source/repo",
        TrackWorkspacePlan::AttachedWithTrackWorktree("/source/repo".into()),
    )
    .await;
    let second = create(
        &repo,
        "/source/repo",
        TrackWorkspacePlan::AttachedWithTrackWorktree("/source/repo".into()),
    )
    .await;
    assert_ne!(
        resource(&repo, &first).await,
        resource(&repo, &second).await
    );
    running(&repo, &first, "read_write").await;
    assert_eq!(occupancy(&repo, &second, "").await, CheckoutOccupancy::Free);
}

#[tokio::test]
async fn workspace_resource_writer_repoints_atomically_and_freezes_both_projections() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let track = create(
        &repo,
        "",
        TrackWorkspacePlan::ManagedUnder("/managed".into()),
    )
    .await;
    let original = resource(&repo, &track).await;
    let next = TrackWorkspace {
        kind: TrackWorkspaceKind::Attached,
        path: "/next/root".into(),
        frozen_at: None,
        worktree: None,
    };
    let mut tx = repo.pool().begin().await.unwrap();
    track_workspace_write_tx(&mut tx, track.id.as_str(), &next)
        .await
        .unwrap();
    let rebound: String =
        sqlx::query_scalar("SELECT workspace_id FROM track_workspace_bindings WHERE track_id=?1")
            .bind(track.id.as_str())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_ne!(rebound, original);
    tx.rollback().await.unwrap();
    assert_eq!(resource(&repo, &track).await, original);
    let mut tx = repo.pool().begin().await.unwrap();
    track_workspace_write_tx(&mut tx, track.id.as_str(), &next)
        .await
        .unwrap();
    assert!(
        track_workspace_freeze_tx(&mut tx, track.id.as_str(), 42)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    let (version, frozen): (i64, Option<i64>) = sqlx::query_as(
        "SELECT selection_version,frozen_at_ms FROM track_workspace_bindings WHERE track_id=?1",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!((version, frozen), (2, Some(42)));
    let current = resource(&repo, &track).await;
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(
        track_workspace_write_tx(&mut tx, track.id.as_str(), &track.workspace)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(resource(&repo, &track).await, current);
    assert_eq!(
        repo.track_get(track.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .workspace
            .frozen_at,
        Some(42)
    );
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_resources WHERE id=?1")
        .bind(&original)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        retained, 1,
        "repoint preserves the former resource's identity"
    );
}

#[tokio::test]
async fn workspace_resource_identity_survives_area_deletion_and_missing_binding_refuses() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let track = create(&repo, "/borrowed/root", TrackWorkspacePlan::AttachedFromCwd).await;
    let id = resource(&repo, &track).await;
    sqlx::query("DELETE FROM areas WHERE id=?1")
        .bind(track.area_id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_resources WHERE id=?1")
        .bind(&id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    let refusal = super::workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        track.id.as_str(),
    )
    .await
    .expect_err("a deleted binding cannot be adopted");
    assert!(refusal.to_string().contains("WorkspaceRecoveryRequired"));
}

#[tokio::test]
async fn workspace_resource_migration_preserves_original_selection_and_payload_bytes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let before_workspace = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 76)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before_workspace.run(&pool).await.unwrap();
    super::track_workspace_migration_tests::seed_pre_workspace_area(&pool).await;
    super::track_workspace_migration_tests::seed_pre_workspace_track(
        &pool,
        "legacy",
        "/original/checkout",
        7,
    )
    .await;
    let previous = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 165)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    previous.run(&pool).await.unwrap();
    // Use the actual operation table; formatting and hashes must be preserved byte-for-byte.
    let payload = "{ \"cwd\" : \"/original/checkout\", \"arg\" : 1 }";
    sqlx::query(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_json,payload_hash,target_type,target_json,phase,created_at_ms,updated_at_ms) \
         VALUES ('op','test:legacy:key','test','key',?1,'original-hash','track','{\"track_id\":\"legacy\"}','pending',1,1)",
    ).bind(payload).execute(&pool).await.unwrap();
    crate::MIGRATOR.run(&pool).await.unwrap();
    let (path, frozen, state): (String, Option<i64>, i64) = sqlx::query_as(
        "SELECT r.root_path,b.frozen_at_ms,r.observation_version FROM workspace_resources r \
         JOIN track_workspace_bindings b ON b.workspace_id=r.id WHERE b.track_id='legacy'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (path, frozen, state),
        ("/original/checkout".into(), Some(7), 0)
    );
    let original: (String, String) =
        sqlx::query_as("SELECT payload_json,payload_hash FROM operations WHERE id='op'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(original, (payload.into(), "original-hash".into()));
}

#[tokio::test]
async fn workspace_resource_ddl_accepts_only_complete_observation_shapes() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    for owned in [false, true] {
        for version in [0_i64, 1] {
            for mask in 0..64 {
                let id = format!("{:032x}", 1 + i64::from(owned) * 128 + version * 64 + mask);
                let owner = (mask & 32 != 0).then_some("source");
                let result = sqlx::query("INSERT INTO workspace_resources(id,source_track_id,root_path,ownership,observation_version,created_at_ms,observed_root,observed_cwd,directory_device,directory_inode,git_common_dir,physical_owner_track_id) VALUES (?1,'source','/row',?2,?3,1,?4,?5,?6,?7,?8,?9)")
                    .bind(&id).bind(if owned {"owned"} else {"borrowed"}).bind(version)
                    .bind((mask & 1 != 0).then_some("/row")).bind((mask & 2 != 0).then_some("."))
                    .bind((mask & 4 != 0).then_some(1_i64)).bind((mask & 8 != 0).then_some(1_i64))
                    .bind((mask & 16 != 0).then_some("/row/.git")).bind(owner)
                    .execute(repo.pool()).await;
                let allowed = (version == 0 && mask == 0)
                    || (version == 1 && mask == if owned { 63 } else { 31 });
                assert_eq!(
                    result.is_ok(),
                    allowed,
                    "observation shape owned={owned} version={version} mask={mask:06b}: {result:?}"
                );
            }
        }
    }
}
