use std::{os::unix::fs::symlink, path::Path, process::Command};

use crate::{
    db::sqlite::{
        CheckoutOccupancy, SqlxRepo, TrackWorkspacePlan, area_create_tx, checkout_occupancy,
        track_create_tx, track_workspace_write_tx, workspace_adopt_tx, workspace_adoption_snapshot,
        workspace_require_adopted,
    },
    model::{NewArea, NewTrack, RequestTheme, Track, TrackWorkspace, TrackWorkspaceKind},
};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q"]);
    git(
        repo,
        &[
            "-c",
            "user.name=Workspace",
            "-c",
            "user.email=workspace@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    );
}

async fn track(repo: &SqlxRepo, cwd: &Path, plan: TrackWorkspacePlan) -> Track {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "adopt".into(),
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
            title: "adopt".into(),
            sort: None,
            cwd: cwd.to_str().unwrap().into(),
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

async fn task(repo: &SqlxRepo, track: &Track) {
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,created_at_ms,updated_at_ms) \
         VALUES (?1,?1,'run','terminal','g','{}','running','read_write',1,1)",
    ).bind(track.id.as_str()).execute(repo.pool()).await.unwrap();
}

#[tokio::test]
async fn workspace_identity_adopts_symlink_and_subdir_into_one_admission_domain() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    std::fs::create_dir(source.join("subdir")).unwrap();
    let link = tmp.path().join("alias");
    symlink(&source, &link).unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let first = track(&repo, &link, TrackWorkspacePlan::AttachedFromCwd).await;
    let second = track(
        &repo,
        &source.join("subdir"),
        TrackWorkspacePlan::AttachedFromCwd,
    )
    .await;
    task(&repo, &first).await;
    // The production entry reconciles the already-running legacy binding before admitting second.
    super::adopt_for_task(repo.pool(), second.id.as_str())
        .await
        .unwrap();
    let refs: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT b.track_id,b.workspace_id,b.cwd FROM track_workspace_bindings b \
         JOIN workspace_resources r ON r.id=b.workspace_id ORDER BY b.track_id",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(refs[0].1, refs[1].1);
    let second_cwd = refs.iter().find(|row| row.0 == second.id.as_str()).unwrap();
    assert_eq!(second_cwd.2, "subdir");
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            second.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Busy
    );
}

#[tokio::test]
async fn workspace_identity_distinct_owned_worktrees_share_git_metadata_only() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let first = track(
        &repo,
        &source,
        TrackWorkspacePlan::AttachedWithTrackWorktree(source.clone()),
    )
    .await;
    let second = track(
        &repo,
        &source,
        TrackWorkspacePlan::AttachedWithTrackWorktree(source.clone()),
    )
    .await;
    super::super::track_worktree::ensure_track_worktree(&first)
        .await
        .unwrap();
    super::super::track_worktree::ensure_track_worktree(&second)
        .await
        .unwrap();
    super::adopt_for_task(repo.pool(), first.id.as_str())
        .await
        .unwrap();
    super::adopt_for_task(repo.pool(), second.id.as_str())
        .await
        .unwrap();
    let roots: Vec<(String, String)> = sqlx::query_as(
        "SELECT adopted_workspace_id,git_common_dir FROM workspace_resources WHERE canonical_root IS NOT NULL",
    ).fetch_all(repo.pool()).await.unwrap();
    assert_eq!(roots.len(), 2);
    assert_ne!(roots[0].0, roots[1].0);
    assert_eq!(roots[0].1, roots[1].1);
    task(&repo, &first).await;
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            second.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Free
    );
}

#[tokio::test]
async fn workspace_identity_pending_selection_refuses_and_stale_adoption_cannot_repoint() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    init(&first);
    init(&second);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let track = track(
        &repo,
        &first,
        TrackWorkspacePlan::ManagedUnder(tmp.path().into()),
    )
    .await;
    assert!(
        workspace_require_adopted(&mut repo.pool().acquire().await.unwrap(), track.id.as_str())
            .await
            .is_err()
    );
    let snapshot =
        workspace_adoption_snapshot(&mut repo.pool().acquire().await.unwrap(), track.id.as_str())
            .await
            .unwrap();
    super::super::workspace_materialize::materialize_workspace(
        &track.workspace,
        tmp.path(),
        track.id.as_str(),
    )
    .unwrap();
    let identity =
        workspace::directory::DirectoryIdentity::open(Path::new(&snapshot.selected_path), true)
            .unwrap()
            .verify_managed_owner(track.id.as_str())
            .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    track_workspace_write_tx(
        &mut tx,
        track.id.as_str(),
        &TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: second.to_str().unwrap().into(),
            frozen_at: None,
            worktree: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(
        workspace_adopt_tx(&mut tx, &snapshot, &identity)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    super::adopt_for_task(repo.pool(), track.id.as_str())
        .await
        .unwrap();
    let root: String = sqlx::query_scalar(
        "SELECT canonical_root FROM workspace_resources WHERE canonical_root IS NOT NULL",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(root, second.to_str().unwrap());
}

#[tokio::test]
async fn workspace_identity_replaced_root_is_recovery_instead_of_silent_rebind() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let track = track(&repo, &source, TrackWorkspacePlan::AttachedFromCwd).await;
    super::adopt_for_task(repo.pool(), track.id.as_str())
        .await
        .unwrap();
    std::fs::rename(&source, tmp.path().join("previous")).unwrap();
    init(&source);
    let error = super::adopt_for_task(repo.pool(), track.id.as_str())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("WorkspaceRecoveryRequired"));
}

#[tokio::test]
async fn workspace_identity_adopts_legacy_active_lease_without_rewriting_source_proof() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    std::fs::create_dir(source.join("subdir")).unwrap();
    let alias = tmp.path().join("alias");
    symlink(&source, &alias).unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let old = track(&repo, &alias, TrackWorkspacePlan::AttachedFromCwd).await;
    let new = track(
        &repo,
        &source.join("subdir"),
        TrackWorkspacePlan::AttachedFromCwd,
    )
    .await;
    let sha = workspace::base::resolve_head_base(&source).unwrap();
    let common = workspace::base::lease_git_common_dir(&source).unwrap();
    sqlx::query(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,created_at_ms,updated_at_ms, \
         base_sha,base_source,canonical_path,git_common_dir,delivery_policy,access_mode) \
         VALUES ('legacy','card',?1,?2,'held','legacy-owner',1,1,?3,'commit',?4,?5,'kernel','read_write')",
    ).bind(old.id.as_str()).bind(alias.to_str().unwrap()).bind(&sha)
        .bind(source.to_str().unwrap()).bind(common.to_str().unwrap()).execute(repo.pool()).await.unwrap();
    super::adopt_for_task(repo.pool(), new.id.as_str())
        .await
        .unwrap();
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            new.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Busy
    );
    let proof: (String, String, String) = sqlx::query_as(
        "SELECT base_sha,canonical_path,git_common_dir FROM workspace_leases WHERE lease_id='legacy'",
    ).fetch_one(repo.pool()).await.unwrap();
    assert_eq!(
        proof,
        (
            sha,
            source.to_str().unwrap().into(),
            common.to_str().unwrap().into()
        )
    );
    sqlx::query("UPDATE workspace_leases SET state='released' WHERE lease_id='legacy'")
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO task_git_deliveries(delivery_id,track_id,producer_attempt_id,card_id,lease_id,ordinal,operation_key,forge_idempotency_key,created_at_ms) \
         VALUES ('delivery',?1,'producer','card','legacy',1,'delivery-op','delivery-key',1)",
    ).bind(old.id.as_str()).execute(repo.pool()).await.unwrap();
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            new.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Busy
    );
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            new.id.as_str(),
            "producer"
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Free
    );
}

#[tokio::test]
async fn workspace_identity_borrowed_alias_cannot_own_the_physical_release_descriptor() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let owner = track(
        &repo,
        tmp.path(),
        TrackWorkspacePlan::ManagedUnder(tmp.path().into()),
    )
    .await;
    super::super::workspace_materialize::materialize_workspace(
        &owner.workspace,
        tmp.path(),
        owner.id.as_str(),
    )
    .unwrap();
    let borrowed = track(
        &repo,
        Path::new(&owner.workspace.path),
        TrackWorkspacePlan::AttachedFromCwd,
    )
    .await;
    super::adopt_for_task(repo.pool(), borrowed.id.as_str())
        .await
        .unwrap();
    let before: Option<String> = sqlx::query_scalar(
        "SELECT physical_owner_track_id FROM workspace_resources WHERE canonical_root IS NOT NULL",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        before, None,
        "borrowed registration supplies no destruction ownership"
    );
    super::adopt_for_task(repo.pool(), owner.id.as_str())
        .await
        .unwrap();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT workspace_id,selected_path FROM track_workspace_bindings")
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        rows[0].0, rows[1].0,
        "one canonical lifecycle and future delivered-tip owner"
    );
    let physical_owner: String = sqlx::query_scalar(
        "SELECT physical_owner_track_id FROM workspace_resources WHERE canonical_root IS NOT NULL",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(physical_owner, owner.id.as_str());
    sqlx::query("DELETE FROM tracks WHERE id=?1")
        .bind(owner.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let retained_owner: String = sqlx::query_scalar(
        "SELECT physical_owner_track_id FROM workspace_resources WHERE canonical_root IS NOT NULL",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        retained_owner, physical_owner,
        "the release descriptor is independent of domain deletion"
    );
}
