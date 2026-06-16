use crate::db::sqlite::worker_session_from_row;
use crate::runtime_repo::{
    AgentProvider, CardId, CardRuntime, Result, RunStatus, RuntimeKind, RuntimeRepoError,
};
use calm_types::worker::{WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionState};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::HashMap;

/// Projection semantics: the card's current worker-session pointer is the winner.
pub(crate) const PROJECTABLE_RUNTIMES_FOR_CARDS_SQL: &str = r#"SELECT ws.id, ws.wave_id, ws.provider, ws.mode, ws.contract, ws.parent_session_id,
                  ws.requester_session_id, ws.state, ws.mcp_token_hash, ws.thread_id,
                  ws.agent_session_id, ws.active_turn_id, ws.terminal_run_id,
                  ws.handle_state_json, ws.liveness, ws.liveness_probed_at_ms,
                  ws.exit_code, ws.exit_interpretation, ws.spawn_op_id,
                  ws.last_activity_ms, ws.last_thread_status, ws.created_at_ms,
                  ws.updated_at_ms, ws.completed_at_ms,
                  c.id AS card_id
           FROM worker_sessions ws
           JOIN cards c ON c.session_id = ws.id
           WHERE c.id IN ({card_id_bindings})
             AND ws.state != 'superseded'
             AND EXISTS (SELECT 1 FROM runtimes r WHERE r.id = ws.id)
           ORDER BY c.id"#;

const PROJECTABLE_RUNTIMES_FOR_CARDS_BINDINGS: &str = "{card_id_bindings}";

pub(crate) const WS_BACKED_CARD_RUNTIME_SELECT: &str = r#"SELECT ws.id, ws.wave_id, ws.provider, ws.mode, ws.contract, ws.parent_session_id,
                  ws.requester_session_id, ws.state, ws.mcp_token_hash, ws.thread_id,
                  ws.agent_session_id, ws.active_turn_id, ws.terminal_run_id,
                  ws.handle_state_json, ws.liveness, ws.liveness_probed_at_ms,
                  ws.exit_code, ws.exit_interpretation, ws.spawn_op_id,
                  ws.last_activity_ms, ws.last_thread_status, ws.created_at_ms,
                  ws.updated_at_ms, ws.completed_at_ms,
                  c.id AS card_id
           FROM worker_sessions ws
           JOIN cards c ON c.session_id = ws.id"#;

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
        let runtime = card_runtime_from_ws_join_row(&row)?;
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

pub(crate) fn card_runtime_from_session(
    ws: &WorkerSession,
    card_id: String,
) -> Result<CardRuntime> {
    let kind = runtime_kind_from_session_identity(ws.provider, ws.contract)?;
    Ok(CardRuntime {
        id: ws.id.as_str().to_string(),
        card_id,
        kind,
        agent_provider: agent_provider_from_session_provider(ws.provider),
        status: run_status_from_worker_session_state(ws.state),
        terminal_run_id: ws.terminal_run_id.clone(),
        terminal_ref: None,
        thread_id: ws.thread_id.clone(),
        session_id: ws.agent_session_id.clone(),
        active_turn_id: ws.active_turn_id.clone(),
        handle_state_json: ws.handle_state_json.clone(),
        lease_owner: None,
        lease_until_ms: None,
        created_at_ms: ws.created_at_ms,
        updated_at_ms: ws.updated_at_ms,
        completed_at_ms: ws.completed_at_ms,
    })
}

pub(crate) fn card_runtime_from_ws_join_row(row: &SqliteRow) -> Result<CardRuntime> {
    let ws = worker_session_from_row(row).map_err(|err| RuntimeRepoError::Message {
        message: err.to_string(),
    })?;
    let card_id: String = row.try_get("card_id")?;
    card_runtime_from_session(&ws, card_id)
}

fn runtime_kind_from_session_identity(
    provider: WorkerProviderKind,
    contract: WorkerContract,
) -> Result<RuntimeKind> {
    match (provider, contract) {
        (WorkerProviderKind::Terminal, WorkerContract::Executor) => Ok(RuntimeKind::Terminal),
        (WorkerProviderKind::Codex, WorkerContract::Executor) => Ok(RuntimeKind::CodexCard),
        (WorkerProviderKind::Codex, WorkerContract::Planner) => Ok(RuntimeKind::SharedSpec),
        (WorkerProviderKind::Claude, WorkerContract::Executor) => Ok(RuntimeKind::ClaudeCard),
        _ => Err(RuntimeRepoError::Message {
            message: format!(
                "unmappable session identity (provider={provider:?}, contract={contract:?})"
            ),
        }),
    }
}

fn agent_provider_from_session_provider(provider: WorkerProviderKind) -> Option<AgentProvider> {
    match provider {
        WorkerProviderKind::Terminal => None,
        WorkerProviderKind::Codex => Some(AgentProvider::Codex),
        WorkerProviderKind::Claude => Some(AgentProvider::Claude),
    }
}

fn run_status_from_worker_session_state(state: WorkerSessionState) -> RunStatus {
    match state {
        WorkerSessionState::Starting => RunStatus::Starting,
        WorkerSessionState::Running => RunStatus::Running,
        WorkerSessionState::Idle => RunStatus::Idle,
        WorkerSessionState::TurnPending => RunStatus::TurnPending,
        WorkerSessionState::Failed => RunStatus::Failed,
        WorkerSessionState::Exited => RunStatus::Exited,
        WorkerSessionState::Superseded => RunStatus::Superseded,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::ids::WaveId;
    use calm_types::worker::{LivenessTag, SessionMode, WorkerSessionId};
    use serde_json::json;

    fn worker_session(
        provider: WorkerProviderKind,
        contract: WorkerContract,
        state: WorkerSessionState,
    ) -> WorkerSession {
        WorkerSession {
            id: WorkerSessionId::from("ws-1"),
            wave_id: WaveId::from("wave-1"),
            provider,
            mode: SessionMode::Resumable,
            contract,
            parent_session_id: Some(WorkerSessionId::from("parent-1")),
            requester_session_id: Some(WorkerSessionId::from("requester-1")),
            state,
            mcp_token_hash: Some("token-hash-1".into()),
            thread_id: Some("thread-1".into()),
            agent_session_id: Some("agent-session-1".into()),
            active_turn_id: Some("turn-1".into()),
            terminal_run_id: Some("terminal-run-1".into()),
            handle_state_json: Some(json!({"mode": "harness"})),
            liveness: LivenessTag::Alive,
            liveness_probed_at_ms: Some(111),
            exit_code: Some(0),
            exit_interpretation: Some("clean".into()),
            spawn_op_id: Some("op-1".into()),
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 10,
            updated_at_ms: 20,
            completed_at_ms: Some(30),
        }
    }

    fn expected_runtime(
        kind: RuntimeKind,
        agent_provider: Option<AgentProvider>,
        status: RunStatus,
    ) -> CardRuntime {
        CardRuntime {
            id: "ws-1".into(),
            card_id: "card-1".into(),
            kind,
            agent_provider,
            status,
            terminal_run_id: Some("terminal-run-1".into()),
            terminal_ref: None,
            thread_id: Some("thread-1".into()),
            session_id: Some("agent-session-1".into()),
            active_turn_id: Some("turn-1".into()),
            handle_state_json: Some(json!({"mode": "harness"})),
            lease_owner: None,
            lease_until_ms: None,
            created_at_ms: 10,
            updated_at_ms: 20,
            completed_at_ms: Some(30),
        }
    }

    #[test]
    fn card_runtime_from_session_maps_terminal() {
        let ws = worker_session(
            WorkerProviderKind::Terminal,
            WorkerContract::Executor,
            WorkerSessionState::Starting,
        );

        assert_eq!(
            card_runtime_from_session(&ws, "card-1".into()).unwrap(),
            expected_runtime(RuntimeKind::Terminal, None, RunStatus::Starting)
        );
    }

    #[test]
    fn card_runtime_from_session_maps_codex_card() {
        let ws = worker_session(
            WorkerProviderKind::Codex,
            WorkerContract::Executor,
            WorkerSessionState::Running,
        );

        assert_eq!(
            card_runtime_from_session(&ws, "card-1".into()).unwrap(),
            expected_runtime(
                RuntimeKind::CodexCard,
                Some(AgentProvider::Codex),
                RunStatus::Running
            )
        );
    }

    #[test]
    fn card_runtime_from_session_maps_shared_spec() {
        let ws = worker_session(
            WorkerProviderKind::Codex,
            WorkerContract::Planner,
            WorkerSessionState::Idle,
        );

        assert_eq!(
            card_runtime_from_session(&ws, "card-1".into()).unwrap(),
            expected_runtime(
                RuntimeKind::SharedSpec,
                Some(AgentProvider::Codex),
                RunStatus::Idle
            )
        );
    }

    #[test]
    fn card_runtime_from_session_maps_claude_card() {
        let ws = worker_session(
            WorkerProviderKind::Claude,
            WorkerContract::Executor,
            WorkerSessionState::TurnPending,
        );

        assert_eq!(
            card_runtime_from_session(&ws, "card-1".into()).unwrap(),
            expected_runtime(
                RuntimeKind::ClaudeCard,
                Some(AgentProvider::Claude),
                RunStatus::TurnPending
            )
        );
    }

    #[test]
    fn card_runtime_from_session_hard_errors_unmapped_identity() {
        for (provider, contract) in [
            (WorkerProviderKind::Codex, WorkerContract::Validator),
            (WorkerProviderKind::Terminal, WorkerContract::Planner),
        ] {
            let ws = worker_session(provider, contract, WorkerSessionState::Running);
            let err = card_runtime_from_session(&ws, "card-1".into()).unwrap_err();
            let expected = format!(
                "unmappable session identity (provider={provider:?}, contract={contract:?})"
            );

            assert!(
                matches!(
                    err,
                    RuntimeRepoError::Message { ref message } if message == &expected
                ),
                "unexpected error: {err:?}"
            );
        }
    }

    #[test]
    fn worker_session_state_round_trips_to_run_status() {
        // The reverse helper is private to db::sqlite, so this module cannot assert a true round-trip.
        let pairs = [
            (WorkerSessionState::Starting, RunStatus::Starting),
            (WorkerSessionState::Running, RunStatus::Running),
            (WorkerSessionState::Idle, RunStatus::Idle),
            (WorkerSessionState::TurnPending, RunStatus::TurnPending),
            (WorkerSessionState::Failed, RunStatus::Failed),
            (WorkerSessionState::Exited, RunStatus::Exited),
            (WorkerSessionState::Superseded, RunStatus::Superseded),
        ];

        for (session_state, run_status) in pairs {
            assert_eq!(
                run_status_from_worker_session_state(session_state),
                run_status
            );
        }
    }
}
