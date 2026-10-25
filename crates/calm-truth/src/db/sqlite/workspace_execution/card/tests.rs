use super::*;
use crate::db::{RepoSyncDomainRaw, sqlite::SqlxRepo};
use serde_json::json;

async fn seed(repo: &SqlxRepo, id: &str, target_type: &str, target: &str, alias: &str) {
    seed_with_owner(repo, id, target_type, target, alias, &format!("op-{id}")).await;
}

async fn seed_with_owner(
    repo: &SqlxRepo,
    id: &str,
    target_type: &str,
    target: &str,
    alias: &str,
    owner: &str,
) {
    sqlx::query("INSERT INTO operations(
        id,operation_key,kind,payload_hash,target_type,target_id,target_json,
        payload_json,tx_output_json,phase,created_at_ms,updated_at_ms)
        VALUES(?1,?1,'pipe-test','hash',?2,?3,'{}',?4,?5,'spawn_started',1,1)")
        .bind(format!("op-{id}")).bind(target_type).bind(target)
        .bind(json!({"card_id":"wanted","target_id":"wanted"}).to_string())
        .bind(json!({"data":{"terminal_launch":{"version":1,"execution_id":id,"release":"Held","decision":"Observing"}}}).to_string())
        .execute(repo.pool()).await.unwrap();
    let metadata =
        json!({"id":id,"consumer":"pipe-test","owner":owner,"track":"track","alias":alias});
    let mut tx = repo.pool().begin().await.unwrap();
    super::super::insert(&mut tx, id, "track", alias, &metadata.to_string(), "{}")
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn w_owned_card_scope_uses_operation_target_without_terminal() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    seed(
        &repo,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "card",
        "wanted",
        "pipe:bound",
    )
    .await;
    seed(
        &repo,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "card",
        "elsewhere",
        "pipe:other-card",
    )
    .await;
    seed(
        &repo,
        "cccccccccccccccccccccccccccccccc",
        "track",
        "wanted",
        "pipe:other-type",
    )
    .await;
    let mut conn = repo.pool().acquire().await.unwrap();
    let selected = executions(&mut conn, "wanted").await.unwrap();
    assert_eq!(
        selected.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
    );
    let unsettled =
        super::super::unsettled_executions(&mut conn, super::super::Scope::Card("wanted"))
            .await
            .unwrap();
    assert_eq!(unsettled, [selected[0].id.clone()]);
}

#[tokio::test]
async fn w_owned_card_scope_rejects_record_metadata_damage() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    seed(
        &repo,
        "dddddddddddddddddddddddddddddddd",
        "card",
        "wanted",
        "pipe:damaged",
    )
    .await;
    sqlx::query("UPDATE operations
        SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch.execution_id','different')
        WHERE id='op-dddddddddddddddddddddddddddddddd'")
        .execute(repo.pool()).await.unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert!(matches!(
        executions(&mut conn, "wanted").await,
        Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
}

#[tokio::test]
async fn w_owned_card_scope_unions_terminal_alias_and_keeps_settled_owners() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let track = crate::db::sqlite::workspace_lease_lookup_tests::seed_track(&repo).await;
    let card = repo
        .card_create(crate::model::NewCard {
            track_id: track.into(),
            kind: "note".into(),
            title: None,
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    let terminal = crate::db::sqlite::terminal_create_tx(
        &mut tx,
        crate::model::NewTerminal {
            card_id: card.id.clone(),
            program: "/bin/sh".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: crate::model::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let alias = super::super::terminal_alias(&terminal.id);
    // Found through both ownership and alias, once.
    seed(
        &repo,
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "card",
        card.id.as_str(),
        &alias,
    )
    .await;
    // Alias still finds a typed terminal whose owner isn't an operation targeting this card.
    let mut tx = repo.pool().begin().await.unwrap();
    super::super::settle_tx(&mut tx, "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "{}", 1)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    let owners = executions(&mut conn, card.id.as_str()).await.unwrap();
    assert_eq!(owners.len(), 1);
    assert!(owners[0].settled);
    assert!(
        super::super::unsettled_executions(&mut conn, super::super::Scope::Card(card.id.as_str()))
            .await
            .unwrap()
            .is_empty()
    );
    drop(conn);
    // Retire is a durable request, not proof that the owner/executor have finished.
    let mut tx = repo.pool().begin().await.unwrap();
    super::super::request(&mut tx, "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "retire")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        executions(&mut conn, card.id.as_str()).await.unwrap().len(),
        1
    );
    drop(conn);
    super::super::finish(repo.db(), "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
        .await
        .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert!(
        executions(&mut conn, card.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    drop(conn);
    seed(
        &repo,
        "ffffffffffffffffffffffffffffffff",
        "track",
        "another",
        &alias,
    )
    .await;
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        executions(&mut conn, card.id.as_str()).await.unwrap()[0]
            .id
            .as_str(),
        "ffffffffffffffffffffffffffffffff"
    );
}

#[tokio::test]
async fn w_owned_card_scope_accepts_typed_terminal_checkpoint() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let id = "11111111111111111111111111111111";
    seed(&repo, id, "card", "wanted", "term:typed").await;
    // terminal_execution_create persists its identity directly in output.data;
    // it does not write a Pipe owner-state record.
    sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
        .bind(json!({"data":{"execution_id":id}}).to_string())
        .bind(format!("op-{id}"))
        .execute(repo.pool())
        .await
        .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    let selected = executions(&mut conn, "wanted").await.unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id.as_str(), id);
}

#[tokio::test]
async fn w_owned_card_scope_requires_exactly_one_record() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let id = "33333333333333333333333333333333";
    seed(&repo, id, "card", "wanted", "pipe:record-count").await;
    for output in [
        None,
        Some(json!({"data":{}})),
        Some(json!({"data":{
            "execution_id":id,
            "terminal_launch":{"execution_id":id}
        }})),
    ] {
        sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
            .bind(output.map(|value| value.to_string()))
            .bind(format!("op-{id}"))
            .execute(repo.pool())
            .await
            .unwrap();
        let mut conn = repo.pool().acquire().await.unwrap();
        assert!(matches!(
            executions(&mut conn, "wanted").await,
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
    }
}

#[tokio::test]
async fn w_owned_card_scope_rejects_owner_metadata_disagreement() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    seed_with_owner(
        &repo,
        "22222222222222222222222222222222",
        "card",
        "wanted",
        "pipe:damaged-owner",
        "missing-operation",
    )
    .await;
    let mut conn = repo.pool().acquire().await.unwrap();
    assert!(matches!(
        executions(&mut conn, "wanted").await,
        Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
    ));
}
