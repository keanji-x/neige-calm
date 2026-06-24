use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::ids::ActorId;
use calm_server::operation::TxOutput;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

#[derive(Clone, Debug)]
pub struct EventRow {
    pub id: i64,
    pub scope_kind: String,
    pub scope_wave: Option<String>,
    pub scope_card: Option<String>,
    pub payload: Value,
}

type RawEventRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

pub async fn event_rows(repo: &SqlxRepo, kind: &str) -> Vec<EventRow> {
    let rows: Vec<RawEventRow> = sqlx::query_as(
        "SELECT id, scope_kind, scope_cove, scope_wave, scope_card, payload \
             FROM events WHERE kind = ?1 ORDER BY id ASC",
    )
    .bind(kind)
    .fetch_all(repo.pool())
    .await
    .expect("event rows");
    rows.into_iter()
        .map(
            |(id, scope_kind, _scope_cove, scope_wave, scope_card, payload)| EventRow {
                id,
                scope_kind,
                scope_wave,
                scope_card,
                payload: serde_json::from_str(&payload).expect("event payload json"),
            },
        )
        .collect()
}

pub async fn wait_for_event_count(repo: &SqlxRepo, kind: &str, expected: usize) -> Vec<EventRow> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = event_rows(repo, kind).await;
        if rows.len() == expected {
            return rows;
        }
        if Instant::now() > deadline {
            panic!("expected {expected} `{kind}` events, got {}", rows.len());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

pub async fn operation_for_idem(
    repo: &SqlxRepo,
    kind: &str,
    idempotency_key: &str,
) -> Option<OperationRow> {
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, phase, tx_output_json, last_error \
           FROM operations \
          WHERE kind = ?1 AND idempotency_key = ?2 \
          ORDER BY created_at_ms DESC \
          LIMIT 1",
    )
    .bind(kind)
    .bind(idempotency_key)
    .fetch_optional(repo.pool())
    .await
    .expect("operation row query");
    row.map(operation_row_from_tuple)
}

pub fn operation_row_from_tuple(
    (id, phase, tx_output_json, last_error): (String, String, Option<String>, Option<String>),
) -> OperationRow {
    OperationRow {
        id,
        phase,
        tx_output: tx_output_json
            .as_deref()
            .map(|raw| serde_json::from_str(raw).expect("tx_output json")),
        last_error,
    }
}

#[derive(Debug)]
pub struct OperationRow {
    pub id: String,
    pub phase: String,
    pub tx_output: Option<TxOutput>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct CommittedEventRow {
    pub actor: ActorId,
    pub scope_kind: String,
    pub scope_wave: Option<String>,
    pub scope_card: Option<String>,
    pub payload: Value,
}

type RawCommittedEventRow = (String, String, Option<String>, Option<String>, String);
pub type RawCommittedEventRowWithId = (i64, String, String, Option<String>, Option<String>, String);

pub async fn task_failed_reason(repo: &SqlxRepo, task_id: &str) -> Option<String> {
    let rows = event_payloads(repo, "task.failed").await;
    rows.into_iter()
        .find(|payload| payload["idempotency_key"] == json!(task_id))
        .and_then(|payload| payload["reason"].as_str().map(ToOwned::to_owned))
}

pub async fn event_payloads(repo: &SqlxRepo, kind: &str) -> Vec<Value> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT payload FROM events WHERE kind = ?1 ORDER BY id ASC")
            .bind(kind)
            .fetch_all(repo.pool())
            .await
            .expect("event payload rows");
    rows.into_iter()
        .map(|(payload,)| serde_json::from_str(&payload).expect("event payload json"))
        .collect()
}

pub async fn committed_event_rows(repo: &SqlxRepo) -> Vec<CommittedEventRow> {
    let rows: Vec<RawCommittedEventRow> = sqlx::query_as(
        "SELECT actor, scope_kind, scope_wave, scope_card, payload \
         FROM events WHERE kind = 'worktree.committed' ORDER BY id ASC",
    )
    .fetch_all(repo.pool())
    .await
    .expect("worktree.committed event rows");
    rows.into_iter()
        .map(
            |(actor, scope_kind, scope_wave, scope_card, payload)| CommittedEventRow {
                actor: serde_json::from_str(&actor).expect("event actor json"),
                scope_kind,
                scope_wave,
                scope_card,
                payload: serde_json::from_str(&payload).expect("event payload json"),
            },
        )
        .collect()
}
