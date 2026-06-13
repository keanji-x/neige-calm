use crate::runtime_repo::{
    AgentProvider, CardId, CardRuntime, Result, RunStatus, RuntimeKind, RuntimeRepoError,
};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::HashMap;

/// Projection semantics: include terminal-state rows so last-known identity surfaces, exclude 'superseded' so the replacement active row is preferred.
pub(crate) const PROJECTABLE_RUNTIMES_FOR_CARDS_SQL: &str = r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE status != 'superseded'
             AND card_id IN ({card_id_bindings})
           ORDER BY card_id ASC,
             CASE
                 WHEN status IN ('starting', 'running', 'idle', 'turn_pending') THEN 0
                 ELSE 1
             END ASC,
             updated_at_ms DESC, created_at_ms DESC, id DESC"#;

const PROJECTABLE_RUNTIMES_FOR_CARDS_BINDINGS: &str = "{card_id_bindings}";

pub(crate) fn projectable_runtimes_for_cards_query<'a>(
    card_ids: &'a [CardId],
) -> QueryBuilder<'a, Sqlite> {
    let (query_prefix, query_suffix) = PROJECTABLE_RUNTIMES_FOR_CARDS_SQL
        .split_once(PROJECTABLE_RUNTIMES_FOR_CARDS_BINDINGS)
        .expect("projectable runtime cards query must contain bindings marker");
    let mut query = QueryBuilder::<Sqlite>::new(query_prefix);
    let mut separated = query.separated(", ");
    for card_id in card_ids {
        separated.push_bind(card_id);
    }
    separated.push_unseparated(query_suffix);
    query
}

pub(crate) fn projectable_runtimes_for_cards_from_rows(
    rows: impl IntoIterator<Item = SqliteRow>,
) -> Result<HashMap<CardId, CardRuntime>> {
    let mut out = HashMap::new();
    for row in rows {
        let runtime = card_runtime_from_row(&row)?;
        let card_id = runtime.card_id.clone();
        out.entry(card_id).or_insert(runtime);
    }
    Ok(out)
}

pub(crate) fn card_runtime_from_row(row: &SqliteRow) -> Result<CardRuntime> {
    let kind = runtime_kind_from_db(row.try_get::<String, _>("kind")?.as_str())?;
    let agent_provider = row
        .try_get::<Option<String>, _>("agent_provider")?
        .as_deref()
        .map(agent_provider_from_db)
        .transpose()?;
    let status = run_status_from_db(row.try_get::<String, _>("status")?.as_str())?;
    let handle_state_json = row
        .try_get::<Option<String>, _>("handle_state_json")?
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;

    Ok(CardRuntime {
        id: row.try_get("id")?,
        card_id: row.try_get("card_id")?,
        kind,
        agent_provider,
        status,
        terminal_run_id: row.try_get("terminal_run_id")?,
        terminal_ref: None,
        thread_id: row.try_get("thread_id")?,
        session_id: row.try_get("session_id")?,
        active_turn_id: row.try_get("active_turn_id")?,
        handle_state_json,
        lease_owner: row.try_get("lease_owner")?,
        lease_until_ms: row.try_get("lease_until_ms")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        completed_at_ms: row.try_get("completed_at_ms")?,
    })
}

pub(crate) fn run_status_from_db(value: &str) -> Result<RunStatus> {
    match value {
        "starting" => Ok(RunStatus::Starting),
        "running" => Ok(RunStatus::Running),
        "idle" => Ok(RunStatus::Idle),
        "turn_pending" => Ok(RunStatus::TurnPending),
        "failed" => Ok(RunStatus::Failed),
        "exited" => Ok(RunStatus::Exited),
        "superseded" => Ok(RunStatus::Superseded),
        other => Err(RuntimeRepoError::Message {
            message: format!("unknown runtime status {other:?}"),
        }),
    }
}

fn runtime_kind_from_db(value: &str) -> Result<RuntimeKind> {
    match value {
        "terminal" => Ok(RuntimeKind::Terminal),
        "codex" => Ok(RuntimeKind::CodexCard),
        "claude" => Ok(RuntimeKind::ClaudeCard),
        "shared-spec" => Ok(RuntimeKind::SharedSpec),
        other => Err(RuntimeRepoError::Message {
            message: format!("unknown runtime kind {other:?}"),
        }),
    }
}

fn agent_provider_from_db(value: &str) -> Result<AgentProvider> {
    match value {
        "codex" => Ok(AgentProvider::Codex),
        "claude" => Ok(AgentProvider::Claude),
        other => Err(RuntimeRepoError::Message {
            message: format!("unknown runtime agent provider {other:?}"),
        }),
    }
}
