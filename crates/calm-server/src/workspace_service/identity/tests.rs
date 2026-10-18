use std::{os::unix::fs::symlink, path::Path, process::Command};

use crate::{
    db::sqlite::{
        CheckoutOccupancy, SqlxRepo, TrackWorkspacePlan, area_create_tx, checkout_occupancy,
        track_create_tx, track_workspace_write_tx, workspace_observation_snapshot,
        workspace_observe_tx,
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

pub(super) fn init(repo: &Path) {
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

pub(super) async fn track(repo: &SqlxRepo, cwd: &Path, plan: TrackWorkspacePlan) -> Track {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        calm_types::organization::LEGACY_OWNER_USER_ID,
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

pub(super) async fn task(repo: &SqlxRepo, track: &Track) {
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,created_at_ms,updated_at_ms) \
         VALUES (?1,?1,'run','terminal','g','{}','running','read_write',1,1)",
    ).bind(track.id.as_str()).execute(repo.pool()).await.unwrap();
}

pub(super) async fn observed_root(repo: &SqlxRepo, track: &Track) -> Option<String> {
    sqlx::query_scalar(
        "SELECT r.observed_root FROM workspace_resources r JOIN track_workspace_bindings b \
         ON b.workspace_id=r.id WHERE b.track_id=?1",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn workspace_identity_observes_aliases_without_claiming_shared_object_identity() {
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
    // Each selection owns its observation; shared physical authority is not enabled.
    super::observe_for_task(repo.db(), first.id.as_str())
        .await
        .unwrap();
    super::observe_for_task(repo.db(), second.id.as_str())
        .await
        .unwrap();
    let refs: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT b.track_id,b.workspace_id,r.observed_cwd FROM track_workspace_bindings b \
         JOIN workspace_resources r ON r.id=b.workspace_id ORDER BY b.track_id",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_ne!(refs[0].1, refs[1].1);
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
        CheckoutOccupancy::Free
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
    super::observe_for_task(repo.db(), first.id.as_str())
        .await
        .unwrap();
    super::observe_for_task(repo.db(), second.id.as_str())
        .await
        .unwrap();
    let roots: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,git_common_dir FROM workspace_resources WHERE observed_root IS NOT NULL",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
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
async fn workspace_identity_registration_cas_refuses_stale_selection() {
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
    assert!(observed_root(&repo, &track).await.is_none());
    let snapshot = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        track.id.as_str(),
    )
    .await
    .unwrap();
    workspace_runtime::selection::materialize_workspace(
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
        workspace_observe_tx(&mut tx, &snapshot, &identity)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    super::observe_for_task(repo.db(), track.id.as_str())
        .await
        .unwrap();
    let root: String = sqlx::query_scalar(
        "SELECT observed_root FROM workspace_resources WHERE observed_root IS NOT NULL",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(root, second.to_str().unwrap());
}

#[tokio::test]
async fn workspace_identity_observation_refresh_is_not_cross_lifetime_replacement_proof() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("repo");
    init(&source);
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let current = track(&repo, &source, TrackWorkspacePlan::AttachedFromCwd).await;
    super::observe_for_task(repo.db(), current.id.as_str())
        .await
        .unwrap();
    let before = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    std::fs::rename(&source, tmp.path().join("previous")).unwrap();
    init(&source);
    super::observe_for_task(repo.db(), current.id.as_str())
        .await
        .unwrap();
    let after = workspace_observation_snapshot(
        &mut repo.pool().acquire().await.unwrap(),
        current.id.as_str(),
    )
    .await
    .unwrap();
    assert_eq!(
        before.workspace_id, after.workspace_id,
        "the ID names a logical selection, not a directory incarnation"
    );
    let crate::db::sqlite::DirectoryObservation::Observed(before) = before.state else {
        panic!("observed before replacement");
    };
    let crate::db::sqlite::DirectoryObservation::Observed(after) = after.state else {
        panic!("observed after replacement");
    };
    assert!(after.version > before.version);
    assert_ne!(
        before.inode, after.inode,
        "renamed old directory remains allocated"
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        after.inode,
        i64::try_from(std::fs::metadata(&source).unwrap().ino()).unwrap()
    );
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
    super::observe_for_task(repo.db(), new.id.as_str())
        .await
        .unwrap();
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            old.id.as_str(),
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
            old.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Busy
    );
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            old.id.as_str(),
            "producer"
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Free
    );
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            new.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Free,
        "an old delivery remains occupied in its original admission scope"
    );
}

#[tokio::test]
async fn workspace_identity_borrowed_observation_grants_no_physical_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let owner = track(
        &repo,
        tmp.path(),
        TrackWorkspacePlan::ManagedUnder(tmp.path().into()),
    )
    .await;
    workspace_runtime::selection::materialize_workspace(
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
    super::observe_for_task(repo.db(), borrowed.id.as_str())
        .await
        .unwrap();
    let before: Option<String> = sqlx::query_scalar(
        "SELECT physical_owner_track_id FROM workspace_resources WHERE source_track_id=?1 AND observed_root IS NOT NULL",
    )
    .bind(borrowed.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        before, None,
        "borrowed observation supplies no destruction ownership"
    );
    super::observe_for_task(repo.db(), owner.id.as_str())
        .await
        .unwrap();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT workspace_id,selected_path FROM track_workspace_bindings")
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert_ne!(
        rows[0].0, rows[1].0,
        "independent logical selections grant no shared lifecycle authority"
    );
    let physical_owner: String = sqlx::query_scalar(
        "SELECT physical_owner_track_id FROM workspace_resources WHERE source_track_id=?1 AND observed_root IS NOT NULL",
    )
    .bind(owner.id.as_str())
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
        "SELECT physical_owner_track_id FROM workspace_resources WHERE source_track_id=?1 AND observed_root IS NOT NULL",
    )
    .bind(owner.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        retained_owner, physical_owner,
        "owner observation audit is retained independently of domain deletion"
    );
}

#[tokio::test]
async fn workspace_identity_unrelated_legacy_terminal_directory_does_not_block_checkout_admission()
{
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    init(&source);
    let external = tmp.path().join("plain-terminal");
    std::fs::create_dir(&external).unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let old = track(&repo, &external, TrackWorkspacePlan::AttachedFromCwd).await;
    task(&repo, &old).await;
    let incoming = track(&repo, &source, TrackWorkspacePlan::AttachedFromCwd).await;
    super::observe_for_task(repo.db(), incoming.id.as_str())
        .await
        .expect("a legitimate non-Git terminal in another track is not checkout occupancy");
    assert_eq!(
        observed_root(&repo, &incoming).await.as_deref(),
        source.to_str()
    );
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            old.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Busy,
        "the old task's original admission fact remains occupied"
    );
}
