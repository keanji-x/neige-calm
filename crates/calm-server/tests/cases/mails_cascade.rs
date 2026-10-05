//! #2130 row 11: `mails` rows follow either Track (migration 0141 on the real chain): deleting a
//! Track deletes every mail it sent or got, and a reply to a deleted mail keeps its row.

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, track_delete_tx};
use calm_server::ids::TrackId;
use calm_server::model::{NewArea, NewTrack, new_id, now_ms};
use calm_server::track_area_cache::TrackAreaCache;

async fn track(repo: &SqlxRepo, area: &calm_server::ids::AreaId, dir: &std::path::Path) -> TrackId {
    let cwd = dir.join(new_id());
    std::fs::create_dir_all(&cwd).unwrap();
    repo.track_create(NewTrack {
        template_input: None,
        area_id: area.clone(),
        title: "mail".into(),
        sort: None,
        cwd: cwd.display().to_string(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .unwrap()
    .id
}

async fn mail(repo: &SqlxRepo, from: &TrackId, to: &TrackId, reply_to: Option<&str>) -> String {
    let id = new_id();
    sqlx::query(
        "INSERT INTO mails (id, from_track_id, to_track_id, reply_to, summary, text, hop, sent_at) \
         VALUES (?1, ?2, ?3, ?4, 's', 't', 1, ?5)",
    )
    .bind(&id)
    .bind(from.as_str())
    .bind(to.as_str())
    .bind(reply_to)
    .bind(now_ms())
    .execute(repo.pool())
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn mails_rows_cascade_with_either_track() {
    let dir = tempfile::tempdir().unwrap();
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(NewArea {
            name: "mail".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let (r, n, t) = (
        track(&repo, &area.id, dir.path()).await,
        track(&repo, &area.id, dir.path()).await,
        track(&repo, &area.id, dir.path()).await,
    );
    let sent = mail(&repo, &r, &n, None).await;
    let got = mail(&repo, &n, &r, None).await;
    let kept = mail(&repo, &r, &t, Some(&sent)).await;

    let cache = TrackAreaCache::new();
    let mut tx = repo.pool().begin().await.unwrap();
    track_delete_tx(&mut tx, n.as_str(), &cache).await.unwrap();
    tx.commit().await.unwrap();

    let left: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, reply_to FROM mails ORDER BY id")
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        left,
        vec![(kept, None)],
        "sent {sent} and got {got} follow N"
    );
}
