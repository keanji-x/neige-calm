//! The `POST /planner/input` key bindings on the real schema: exclusive per key, and gone with
//! their card.

use super::{
    PlannerInputBinding, SqlxRepo, area_create_tx, card_create_tx, card_delete_tx,
    planner_input_bind_tx, planner_input_binding_get, track_create_tx,
};
use crate::card_role_cache::CardRoleCache;
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
use serde_json::json;

async fn cards(repo: &SqlxRepo, role_cache: &CardRoleCache, n: usize) -> Vec<String> {
    let mut tx = repo.pool().begin().await.expect("begin");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "planner input keys".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("area");
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "planner input keys".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("track");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let card = card_create_tx(
            &mut tx,
            NewCard {
                track_id: track.id.clone(),
                kind: "note".into(),
                sort: None,
                payload: json!({ "i": i }),
                title: Some(format!("card {i}")),
            },
            role_cache,
        )
        .await
        .expect("card");
        ids.push(card.id.to_string());
    }
    tx.commit().await.expect("commit");
    ids
}

fn binding(n: usize) -> PlannerInputBinding {
    PlannerInputBinding {
        payload_hash: format!("hash-{n}"),
        worker_session_id: "runtime".into(),
        entry_id: Some(format!("entry-{n}")),
    }
}

async fn bind(repo: &SqlxRepo, card: &str, key: &str, value: &PlannerInputBinding) {
    let mut tx = repo.pool().begin().await.expect("begin");
    planner_input_bind_tx(&mut tx, card, key, value)
        .await
        .expect("bind");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn a_second_binding_for_one_key_fails_its_transaction() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let role_cache = CardRoleCache::new();
    let ids = cards(&repo, &role_cache, 1).await;
    bind(&repo, &ids[0], "key", &binding(0)).await;
    let mut tx = repo.pool().begin().await.expect("begin");
    assert!(
        planner_input_bind_tx(&mut tx, &ids[0], "key", &binding(1))
            .await
            .is_err()
    );
    drop(tx);
    assert_eq!(
        planner_input_binding_get(repo.pool(), &ids[0], "key")
            .await
            .expect("read"),
        Some(binding(0)),
    );
}

#[tokio::test]
async fn deleting_the_card_deletes_its_bindings() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let role_cache = CardRoleCache::new();
    let ids = cards(&repo, &role_cache, 1).await;
    bind(&repo, &ids[0], "key", &binding(0)).await;
    let mut tx = repo.pool().begin().await.expect("begin");
    card_delete_tx(&mut tx, &ids[0], &role_cache)
        .await
        .expect("delete card");
    tx.commit().await.expect("commit");
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM planner_input_idempotency")
        .fetch_one(repo.pool())
        .await
        .expect("count");
    assert_eq!(left, 0);
}
