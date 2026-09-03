use super::{SqlxRepo, area_create_tx, wave_create_tx};
use crate::db::RepoRead;
use crate::model::{NewArea, NewWave, RequestTheme};
use serde_json::json;

/// #891 — `template_input` INSERT → SELECT round-trip: the JSON blob
/// persists verbatim (TEXT column, `#[sqlx(json(nullable))]` decode) and
/// a `None` input stays `None`.
#[tokio::test]
async fn wave_create_round_trips_template_input() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let mut tx = repo.pool().begin().await.expect("begin tx");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "template input round trip".into(),
            color: "#202020".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    let input = json!({
        "issue_url": "https://github.com/o/r/issues/891",
        "issue_number": 891,
        "merge_policy": "hold-for-ratify"
    });
    let with_input = wave_create_tx(
        &mut tx,
        NewWave {
            area_id: area.id.clone(),
            title: "with input".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: Some("issue-development".into()),
            plugin_scope: None,
            template_input: Some(input.clone()),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        Some("area-chat"),
        &crate::db::sqlite::WaveWorkspacePlan::AttachedFromCwd,
        repo.wave_area_cache(),
    )
    .await
    .expect("create wave with input");
    assert_eq!(with_input.template_input.as_ref(), Some(&input));
    let without_input = wave_create_tx(
        &mut tx,
        NewWave {
            area_id: area.id,
            title: "without input".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::WaveWorkspacePlan::AttachedFromCwd,
        repo.wave_area_cache(),
    )
    .await
    .expect("create wave without input");
    tx.commit().await.expect("commit tx");

    let stored = repo
        .wave_get(with_input.id.as_str())
        .await
        .expect("get wave")
        .expect("wave exists");
    assert_eq!(stored.template_input.as_ref(), Some(&input));
    assert_eq!(stored.purpose.as_deref(), Some("area-chat"));

    let stored_none = repo
        .wave_get(without_input.id.as_str())
        .await
        .expect("get wave")
        .expect("wave exists");
    assert_eq!(stored_none.template_input, None);
}
