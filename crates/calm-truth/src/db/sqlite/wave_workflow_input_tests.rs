use super::{SqlxRepo, cove_create_tx, wave_create_tx};
use crate::db::RepoRead;
use crate::model::{NewCove, NewWave, RequestTheme};
use serde_json::json;

/// #891 — `workflow_input` INSERT → SELECT round-trip: the JSON blob
/// persists verbatim (TEXT column, `#[sqlx(json(nullable))]` decode) and
/// a `None` input stays `None`.
#[tokio::test]
async fn wave_create_round_trips_workflow_input() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let mut tx = repo.pool().begin().await.expect("begin tx");
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "workflow input round trip".into(),
            color: "#202020".into(),
            sort: None,
        },
    )
    .await
    .expect("create cove");
    let input = json!({
        "issue_url": "https://github.com/o/r/issues/891",
        "issue_number": 891,
        "merge_policy": "hold-for-ratify"
    });
    let with_input = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id.clone(),
            title: "with input".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: Some("issue-development".into()),
            workflow_input: Some(input.clone()),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave with input");
    assert_eq!(with_input.workflow_input.as_ref(), Some(&input));
    let without_input = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id,
            title: "without input".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave without input");
    tx.commit().await.expect("commit tx");

    let stored = repo
        .wave_get(with_input.id.as_str())
        .await
        .expect("get wave")
        .expect("wave exists");
    assert_eq!(stored.workflow_input.as_ref(), Some(&input));

    let stored_none = repo
        .wave_get(without_input.id.as_str())
        .await
        .expect("get wave")
        .expect("wave exists");
    assert_eq!(stored_none.workflow_input, None);
}
