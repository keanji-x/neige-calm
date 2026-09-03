use super::{SqlxRepo, area_create_tx, track_create_tx};
use crate::db::RepoRead;
use crate::model::{NewArea, NewTrack, RequestTheme, now_ms};

async fn seed_track(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.expect("begin seed tx");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "workspace lease lookup".into(),
            color: "#202020".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            template_input: None,
            area_id: area.id,
            title: "workspace lease lookup".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        repo.track_area_cache(),
    )
    .await
    .expect("create track");
    tx.commit().await.expect("commit seed tx");
    track.id.to_string()
}

async fn insert_workspace_lease(
    repo: &SqlxRepo,
    lease_id: &str,
    card_id: &str,
    track_id: &str,
    path: &str,
    state: &str,
    created_at_ms: i64,
) {
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, track_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms, released_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?8, NULL)"#,
    )
    .bind(lease_id)
    .bind(card_id)
    .bind(track_id)
    .bind(path)
    .bind(state)
    .bind("workspace-lease-lookup-test")
    .bind(now_ms() + 60_000)
    .bind(created_at_ms)
    .execute(repo.pool())
    .await
    .expect("insert workspace lease");
}

#[tokio::test]
async fn workspace_lease_for_card_returns_only_held_leases() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track_id = seed_track(&repo).await;

    insert_workspace_lease(
        &repo,
        "lease-releasing-only",
        "card-releasing-only",
        &track_id,
        "/tmp/lease-releasing-only",
        "releasing",
        100,
    )
    .await;
    assert!(
        repo.workspace_lease_for_card("card-releasing-only")
            .await
            .expect("lookup releasing-only lease")
            .is_none(),
        "releasing leases must not be used as forge execution workspaces"
    );

    insert_workspace_lease(
        &repo,
        "lease-held-older",
        "card-held-with-newer-releasing",
        &track_id,
        "/tmp/lease-held-older",
        "held",
        200,
    )
    .await;
    insert_workspace_lease(
        &repo,
        "lease-releasing-newer",
        "card-held-with-newer-releasing",
        &track_id,
        "/tmp/lease-releasing-newer",
        "releasing",
        300,
    )
    .await;

    let lease = repo
        .workspace_lease_for_card("card-held-with-newer-releasing")
        .await
        .expect("lookup mixed lease states")
        .expect("held lease should resolve");
    assert_eq!(lease.lease_id, "lease-held-older");
    assert_eq!(lease.state, "held");
    assert_eq!(lease.path, "/tmp/lease-held-older");
}
