use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use serde_json::Value;
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
