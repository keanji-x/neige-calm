use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use std::sync::Arc;

struct TerminalWorkerHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    adapter: TerminalWorkerAdapter,
    wave_id: String,
}

async fn terminal_worker_harness() -> TerminalWorkerHarness {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        repo.as_ref(),
        crate::model::NewCove {
            name: "terminal workers".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        repo.as_ref(),
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "terminal workers".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    TerminalWorkerHarness {
        adapter: TerminalWorkerAdapter::new(route_repo, CardRoleCache::new(), WaveCoveCache::new()),
        repo,
        wave_id: wave.id.to_string(),
    }
}

async fn prepare_terminal_worker(harness: &TerminalWorkerHarness, key: &str) -> TxOutput {
    let task_id = format!("{}:{key}", harness.wave_id);
    let payload = serde_json::to_value(TerminalWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        wave_id: harness.wave_id.clone(),
        idempotency_key: task_id.clone(),
        cmd: format!("printf {key}\n"),
        cwd: Some("/tmp".into()),
    })
    .unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO tasks \
         (id, wave_id, key, kind, goal, context_json, depends_on_json, status, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, 'terminal', 'test', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(&task_id)
    .bind(&harness.wave_id)
    .bind(key)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("op-{key}")),
                payload_hash: format!("hash-{key}"),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == op_id)
        .unwrap();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let output = harness
        .adapter
        .prepare_tx(&mut tx, &payload, &op)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    output
}

/// #1149 — the terminal worker card is titled after its task's plan key.
#[tokio::test]
async fn terminal_worker_prepare_titles_card_with_task_key() {
    let harness = terminal_worker_harness().await;
    let output = prepare_terminal_worker(&harness, "slice-d").await;
    let card_id = output.output_string("card_id", "test").unwrap();

    let stored: Option<String> = sqlx::query_scalar("SELECT title FROM cards WHERE id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, Some("slice-d".to_string()));

    let wire: crate::model::Card = serde_json::from_value(output.result.clone()).unwrap();
    assert_eq!(wire.title, Some("slice-d".to_string()));
    // The pre-existing payload merge must survive untouched.
    assert_eq!(
        wire.payload.get("idempotency_key").and_then(Value::as_str),
        Some(format!("{}:slice-d", harness.wave_id).as_str())
    );
    assert_eq!(
        wire.payload.get("role_request").and_then(Value::as_str),
        Some("terminal")
    );
}
