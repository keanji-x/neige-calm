use crate::runtime_repo::{
    AgentProvider, CardRuntime, Result, RunStatus, RuntimeKind, RuntimeRepoError,
};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

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
