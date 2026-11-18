//! PATCH must never turn an autonomous worker into a human conversation.
use crate::support::planner_queue_fixture::{
    Boot, boot_with, idle_snapshot, post_input_keyed, send_json,
};
use axum::http::StatusCode;
use calm_server::db::sqlite::card_create_with_id_tx;
use calm_server::db::write_in_tx_typed;
use calm_server::model::{CardRole, NewCard, new_id};
use serde_json::{Value, json};

async fn worker(boot: &Boot) -> String {
    let track_id = boot.planner_card.track_id.clone();
    let roles = boot.state.card_role_cache.clone();
    write_in_tx_typed(boot.repo.as_ref(), move |tx| Box::pin(async move {
        Ok(card_create_with_id_tx(tx, new_id(), NewCard {
            track_id, kind: "codex".into(), title: None, sort: None,
            payload: json!({"schemaVersion":1,"harness_profile":"task_worker","worker_provider":"opencode"}),
        }, CardRole::Worker, false, &roles).await?.id.to_string())
    })).await.unwrap()
}

async fn forbidden(boot: &Boot, card: &str) {
    for (method, action, body) in [
        ("POST", "input", json!({"text":"injected"})),
        (
            "PUT",
            "model",
            json!({"model":"gpt-5","reasoning_effort":null}),
        ),
        ("POST", "restart", json!({})),
        ("POST", "interrupt", json!({})),
        ("POST", "reset", json!({})),
    ] {
        let (status, response) = if action == "input" {
            post_input_keyed(boot.app.clone(), card, body, "worker-isolation-probe").await
        } else {
            send_json(
                boot.app.clone(),
                method,
                format!("/api/cards/{card}/planner/{action}"),
                "user",
                body,
            )
            .await
        };
        assert_eq!(status, StatusCode::FORBIDDEN, "{action}: {response}");
    }
}

async fn patch(boot: &Boot, card: &str, payload: Value) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "PATCH",
        format!("/api/cards/{card}"),
        "user",
        json!({"payload":payload}),
    )
    .await
}

#[tokio::test]
async fn task_worker_patch_cannot_replace_harness_identity() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card = worker(&boot).await;
    let (status, body) = patch(
        &boot,
        &card,
        json!({"schemaVersion":1,"harness_profile":"plain_chat"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    forbidden(&boot, &card).await;
}

#[tokio::test]
async fn task_worker_patch_omission_preserves_harness_identity() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card = worker(&boot).await;
    let (status, body) = patch(&boot, &card, json!({"schemaVersion":1})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["payload"]["harness_profile"], "task_worker");
    forbidden(&boot, &card).await;
}

// Simulate a pre-fix or corrupt row. Independent durable ownership must still fence it.
async fn corrupt_worker(boot: &Boot, evidence: &str) -> String {
    let card = worker(boot).await;
    boot.repo.db().writer().execute(sqlx::query(
        r#"UPDATE cards SET payload='{"schemaVersion":1,"harness_profile":"plain_chat"}' WHERE id=?1"#)
        .bind(&card)).await.unwrap();
    match evidence {
        "task" => {
            boot.repo
                .db()
                .writer()
                .execute(
                    sqlx::query(concat!(
                        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,",
                        "status,worker_card_id,created_at_ms,updated_at_ms) ",
                        "VALUES(?1,?2,'bound','agent','bound','null','[]','done',?3,1,1)"
                    ))
                    .bind(format!("{}:bound", boot.planner_card.track_id))
                    .bind(boot.planner_card.track_id.as_str())
                    .bind(&card),
                )
                .await
                .unwrap();
        }
        "operation" => {
            boot.repo.db().writer().execute(sqlx::query(
                concat!("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,",
                "target_type,target_id,target_json,payload_json,phase,created_at_ms,updated_at_ms) ",
                "VALUES(?1,?1,'agent-worker','bound','hash','card',?2,'{}',?3,'succeeded',1,1)"))
                .bind(new_id()).bind(&card).bind(json!({"actor":{"kind":"KernelDispatcher"}}).to_string())).await.unwrap();
        }
        "session" => {
            let id = card.clone();
            write_in_tx_typed(boot.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    let mut init =
                        calm_server::session_projection_repo::WorkerSessionInit::shared_planner(
                            new_id(),
                            id,
                            calm_server::session_projection_repo::AgentProvider::OpenCode,
                            calm_server::session_projection_repo::WorkerSessionState::Idle,
                            Some("worker-thread".into()),
                            json!({}),
                            1,
                        );
                    init.kind =
                        calm_server::session_projection_repo::WorkerSessionKind::ManagedWorker;
                    calm_server::db::sqlite::session_start_runtime_tx(tx, init).await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        }
        _ => unreachable!(),
    }
    card
}

#[tokio::test]
async fn task_worker_task_binding_cannot_become_plain_chat() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card = corrupt_worker(&boot, "task").await;
    forbidden(&boot, &card).await;
}
#[tokio::test]
async fn task_worker_operation_binding_cannot_become_plain_chat() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card = corrupt_worker(&boot, "operation").await;
    forbidden(&boot, &card).await;
}
#[tokio::test]
async fn task_worker_managed_session_cannot_become_plain_chat() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card = corrupt_worker(&boot, "session").await;
    forbidden(&boot, &card).await;
}
