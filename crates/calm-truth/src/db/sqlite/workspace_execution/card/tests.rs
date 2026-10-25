use super::*;
use crate::db::{Repo, sqlite::SqlxRepo};
use serde_json::json;

async fn seed(repo: &SqlxRepo, id: &str, target_type: &str, target: &str, alias: &str) {
    sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_id,target_json,payload_json,tx_output_json,phase,created_at_ms,updated_at_ms) VALUES(?1,?1,'pipe-test','hash',?2,?3,'{}',?4,?5,'spawn_started',1,1)")
        .bind(format!("op-{id}")).bind(target_type).bind(target)
        .bind(json!({"card_id":"wanted","target_id":"wanted"}).to_string())
        .bind(json!({"data":{"terminal_launch":{"version":1,"execution_id":id,"release":"Held","decision":"Observing"}}}).to_string())
        .execute(repo.pool()).await.unwrap();
    let metadata = json!({"id":id,"consumer":"pipe-test","owner":format!("op-{id}"),"track":"track","alias":alias});
    let mut tx = repo.pool().begin().await.unwrap();
    super::super::insert(&mut tx, id, "track", alias, &metadata.to_string(), "{}").await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn w_owned_card_scope_uses_operation_target_without_terminal() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    seed(&repo, "bound", "card", "wanted", "pipe:bound").await;
    seed(&repo, "other-card", "card", "elsewhere", "pipe:other-card").await;
    seed(&repo, "other-type", "track", "wanted", "pipe:other-type").await;
    let mut conn = repo.pool().acquire().await.unwrap();
    let selected = executions(&mut conn, "wanted").await.unwrap();
    assert_eq!(selected.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["bound"]);
    let unsettled = super::super::unsettled_executions(&mut conn, super::super::Scope::Card("wanted")).await.unwrap();
    assert_eq!(unsettled, [selected[0].id.clone()]);
}

#[tokio::test]
async fn w_owned_card_scope_rejects_record_metadata_damage() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    seed(&repo, "damaged", "card", "wanted", "pipe:damaged").await;
    sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch.execution_id','different') WHERE id='op-damaged'")
        .execute(repo.pool()).await.unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert!(matches!(executions(&mut conn, "wanted").await, Err(CalmError::Conflict(_))));
}

#[tokio::test]
async fn w_owned_card_scope_unions_terminal_alias_and_keeps_settled_owners() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let track = crate::db::sqlite::workspace_lease_lookup_tests::seed_track(&repo).await;
    let card = repo.card_create(crate::model::NewCard {
        track_id: track.into(), kind:"note".into(), title:None, sort:None, payload:json!({}),
    }).await.unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    let terminal = crate::db::sqlite::terminal_create_tx(&mut tx, crate::model::NewTerminal {
        card_id:card.id.clone(), program:"/bin/sh".into(), cwd:"/tmp".into(), env:json!({}), theme:crate::model::RequestTheme::default_dark(),
    }).await.unwrap();
    tx.commit().await.unwrap();
    let alias = super::super::terminal_alias(&terminal.id);
    // Found through both ownership and alias, once.
    seed(&repo, "both", "card", card.id.as_str(), &alias).await;
    // Alias still finds a typed terminal whose owner isn't an operation targeting this card.
    let mut tx = repo.pool().begin().await.unwrap();
    super::super::settle_tx(&mut tx, "both", "{}", 1).await.unwrap();
    tx.commit().await.unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    let owners = executions(&mut conn, card.id.as_str()).await.unwrap();
    assert_eq!(owners.len(), 1);
    assert!(owners[0].settled);
    assert!(super::super::unsettled_executions(&mut conn, super::super::Scope::Card(card.id.as_str())).await.unwrap().is_empty());
    drop(conn);
    // Retire is a durable request, not proof that the owner/executor have finished.
    let mut tx = repo.pool().begin().await.unwrap();
    super::super::request(&mut tx, "both", "retire").await.unwrap();
    tx.commit().await.unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(executions(&mut conn, card.id.as_str()).await.unwrap().len(), 1);
    drop(conn);
    super::super::finish(repo.db(), "both").await.unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert!(executions(&mut conn, card.id.as_str()).await.unwrap().is_empty());
    drop(conn);
    seed(&repo, "alias-only", "track", "another", &alias).await;
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(executions(&mut conn, card.id.as_str()).await.unwrap()[0].id.as_str(), "alias-only");
}
