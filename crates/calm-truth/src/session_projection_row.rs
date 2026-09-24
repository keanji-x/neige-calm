use crate::db::sqlite::worker_session_from_row;
use crate::session_projection_repo::{
    AgentProvider, CardId, Result, WorkerSessionKind, WorkerSessionProjection,
    WorkerSessionProjectionRepoError,
};
use calm_types::worker::{WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionState};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::HashMap;

/// The one spelling of "when did this card's last turn end": the newest
/// non-`interrupted` `turn/completed` transcript row (a `failed` turn counts as
/// an ending). `c` is the enclosing statement's `cards` alias. Not
/// `worker_sessions.last_turn_completed_ms`: the feeder stamps that for stale
/// turns too. A macro so `concat!` can inline it into `const` SELECTs.
#[macro_export]
macro_rules! last_turn_completed_ms_subquery {
    () => {
        "(SELECT MAX(h.created_at_ms) FROM harness_items h \
           WHERE h.card_id = c.id AND h.method = 'turn/completed' \
             AND COALESCE(json_extract(h.params, '$.status'), '') <> 'interrupted')"
    };
}

pub const LAST_TURN_COMPLETED_MS_SUBQUERY: &str = last_turn_completed_ms_subquery!();

/// Shared by the three SELECTs so a column added to one is added to all;
/// `card_runtime_from_ws_join_row` reads every column by name.
macro_rules! ws_card_runtime_select {
    ($tail:literal) => {
        concat!(
            "SELECT ws.id, ws.track_id, ws.provider, ws.mode, ws.contract, ws.parent_session_id,\n",
            "                  ws.requester_session_id, ws.state, ws.mcp_token_hash, ws.thread_id,\n",
            "                  ws.agent_session_id, ws.active_turn_id, ws.terminal_run_id,\n",
            "                  ws.handle_state_json, ws.liveness, ws.liveness_probed_at_ms,\n",
            "                  ws.exit_code, ws.exit_interpretation, ws.spawn_op_id,\n",
            "                  ws.last_activity_ms, ws.last_thread_status, ws.created_at_ms,\n",
            "                  ws.updated_at_ms, ws.completed_at_ms,\n",
            "                  ",
            last_turn_completed_ms_subquery!(),
            " AS last_turn_completed_ms,\n",
            "                  c.id AS card_id\n",
            $tail
        )
    };
}

/// Projection semantics: the card's current worker-session pointer is the winner.
pub(crate) const PROJECTABLE_RUNTIMES_FOR_CARDS_SQL: &str = ws_card_runtime_select!(
    "           FROM worker_sessions ws
           JOIN cards c ON c.session_id = ws.id
           WHERE c.id IN ({card_id_bindings})
             AND ws.state != 'superseded'
           ORDER BY c.id"
);

const PROJECTABLE_RUNTIMES_FOR_CARDS_BINDINGS: &str = "{card_id_bindings}";

pub(crate) const WS_BACKED_CARD_RUNTIME_SELECT: &str = ws_card_runtime_select!(
    "           FROM worker_sessions ws
           JOIN cards c ON c.session_id = ws.id"
);

/// The **one** definition of "which runtime is this card's ACTIVE one"; `?1`
/// is the card id. Owns the state filter and the newest-first tie-break so no
/// second reader restates either; `calm-server` embeds it as a subquery too.
pub const ACTIVE_CARD_RUNTIME_SELECT: &str = r#"SELECT ws.id
             FROM worker_sessions ws
             JOIN cards c ON c.session_id = ws.id
            WHERE c.id = ?1
              AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
            ORDER BY ws.updated_at_ms DESC, ws.created_at_ms DESC, ws.id DESC
            LIMIT 1"#;

pub(crate) const WS_CARD_KEYED_RUNTIME_SELECT: &str = ws_card_runtime_select!(
    "           FROM worker_sessions ws
           JOIN cards c ON c.id = ws.card_id"
);

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
) -> Result<HashMap<CardId, WorkerSessionProjection>> {
    let mut out = HashMap::new();
    for row in rows {
        let runtime = card_runtime_from_ws_join_row(&row)?;
        let card_id = runtime.card_id.clone();
        out.entry(card_id).or_insert(runtime);
    }
    Ok(out)
}

/// `last_turn_completed_ms` is a column of the projection row, not of
/// `WorkerSession`; the join row hands it in here.
pub(crate) fn card_runtime_from_session(
    ws: &WorkerSession,
    card_id: String,
    last_turn_completed_ms: Option<i64>,
) -> Result<WorkerSessionProjection> {
    let kind = runtime_kind_from_session_identity(ws.provider, ws.contract)?;
    Ok(WorkerSessionProjection {
        id: ws.id.as_str().to_string(),
        card_id,
        kind,
        agent_provider: agent_provider_from_session_provider(ws.provider),
        status: ws.state,
        terminal_run_id: ws.terminal_run_id.clone(),
        thread_id: ws.thread_id.clone(),
        session_id: ws.agent_session_id.clone(),
        active_turn_id: ws.active_turn_id.clone(),
        handle_state_json: ws.handle_state_json.clone(),
        created_at_ms: ws.created_at_ms,
        updated_at_ms: ws.updated_at_ms,
        completed_at_ms: ws.completed_at_ms,
        last_turn_completed_ms,
    })
}

pub(crate) fn card_runtime_from_ws_join_row(row: &SqliteRow) -> Result<WorkerSessionProjection> {
    let ws =
        worker_session_from_row(row).map_err(|err| WorkerSessionProjectionRepoError::Message {
            message: err.to_string(),
        })?;
    let card_id: String = row.try_get("card_id")?;
    // `try_get`, not `get`-with-default: a SELECT that forgot the column
    // must fail here, not project "no completed turn" for every card.
    let last_turn_completed_ms: Option<i64> = row.try_get("last_turn_completed_ms")?;
    card_runtime_from_session(&ws, card_id, last_turn_completed_ms)
}

fn runtime_kind_from_session_identity(
    provider: WorkerProviderKind,
    contract: WorkerContract,
) -> Result<WorkerSessionKind> {
    match (provider, contract) {
        (WorkerProviderKind::Terminal, WorkerContract::Executor) => Ok(WorkerSessionKind::Terminal),
        (WorkerProviderKind::Codex, WorkerContract::Executor) => Ok(WorkerSessionKind::CodexCard),
        (WorkerProviderKind::Codex | WorkerProviderKind::Claude, WorkerContract::Planner) => {
            Ok(WorkerSessionKind::SharedPlanner)
        }
        (WorkerProviderKind::Claude, WorkerContract::Executor) => Ok(WorkerSessionKind::ClaudeCard),
        _ => Err(WorkerSessionProjectionRepoError::Message {
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

pub(crate) fn run_status_from_db(value: &str) -> Result<WorkerSessionState> {
    match value {
        "starting" => Ok(WorkerSessionState::Starting),
        "running" => Ok(WorkerSessionState::Running),
        "idle" => Ok(WorkerSessionState::Idle),
        "turn_pending" => Ok(WorkerSessionState::TurnPending),
        "failed" => Ok(WorkerSessionState::Failed),
        "exited" => Ok(WorkerSessionState::Exited),
        "superseded" => Ok(WorkerSessionState::Superseded),
        other => Err(WorkerSessionProjectionRepoError::Message {
            message: format!("unknown runtime status {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::ids::{CardId, TrackId};
    use calm_types::worker::{LivenessTag, SessionMode, WorkerSessionId};
    use serde_json::json;

    fn worker_session(
        provider: WorkerProviderKind,
        contract: WorkerContract,
        state: WorkerSessionState,
    ) -> WorkerSession {
        WorkerSession {
            id: WorkerSessionId::from("ws-1"),
            track_id: TrackId::from("track-1"),
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
            card_id: Some(CardId("card-1".into())),
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
        kind: WorkerSessionKind,
        agent_provider: Option<AgentProvider>,
        status: WorkerSessionState,
    ) -> WorkerSessionProjection {
        WorkerSessionProjection {
            id: "ws-1".into(),
            card_id: "card-1".into(),
            kind,
            agent_provider,
            status,
            terminal_run_id: Some("terminal-run-1".into()),
            thread_id: Some("thread-1".into()),
            session_id: Some("agent-session-1".into()),
            active_turn_id: Some("turn-1".into()),
            handle_state_json: Some(json!({"mode": "harness"})),
            created_at_ms: 10,
            updated_at_ms: 20,
            completed_at_ms: Some(30),
            last_turn_completed_ms: Some(40),
        }
    }

    #[test]
    fn every_projection_select_carries_the_last_turn_completed_column() {
        for sql in [
            PROJECTABLE_RUNTIMES_FOR_CARDS_SQL,
            WS_BACKED_CARD_RUNTIME_SELECT,
            WS_CARD_KEYED_RUNTIME_SELECT,
        ] {
            assert!(sql.contains(LAST_TURN_COMPLETED_MS_SUBQUERY), "{sql}");
            assert!(sql.contains(" AS last_turn_completed_ms,"), "{sql}");
            assert!(sql.contains("c.id AS card_id"), "{sql}");
        }
        assert!(LAST_TURN_COMPLETED_MS_SUBQUERY.contains("<> 'interrupted'"));
    }

    #[test]
    fn card_runtime_from_session_maps_terminal() {
        let ws = worker_session(
            WorkerProviderKind::Terminal,
            WorkerContract::Executor,
            WorkerSessionState::Starting,
        );

        assert_eq!(
            card_runtime_from_session(&ws, "card-1".into(), Some(40)).unwrap(),
            expected_runtime(
                WorkerSessionKind::Terminal,
                None,
                WorkerSessionState::Starting
            )
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
            card_runtime_from_session(&ws, "card-1".into(), Some(40)).unwrap(),
            expected_runtime(
                WorkerSessionKind::CodexCard,
                Some(AgentProvider::Codex),
                WorkerSessionState::Running
            )
        );
    }

    #[test]
    fn card_runtime_from_session_maps_shared_planner() {
        let ws = worker_session(
            WorkerProviderKind::Codex,
            WorkerContract::Planner,
            WorkerSessionState::Idle,
        );

        assert_eq!(
            card_runtime_from_session(&ws, "card-1".into(), Some(40)).unwrap(),
            expected_runtime(
                WorkerSessionKind::SharedPlanner,
                Some(AgentProvider::Codex),
                WorkerSessionState::Idle
            )
        );
    }

    #[test]
    fn card_runtime_from_session_maps_claude_planner_to_shared_planner() {
        let ws = worker_session(
            WorkerProviderKind::Claude,
            WorkerContract::Planner,
            WorkerSessionState::Idle,
        );

        assert_eq!(
            card_runtime_from_session(&ws, "card-1".into(), Some(40)).unwrap(),
            expected_runtime(
                WorkerSessionKind::SharedPlanner,
                Some(AgentProvider::Claude),
                WorkerSessionState::Idle
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
            card_runtime_from_session(&ws, "card-1".into(), Some(40)).unwrap(),
            expected_runtime(
                WorkerSessionKind::ClaudeCard,
                Some(AgentProvider::Claude),
                WorkerSessionState::TurnPending
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
            let err = card_runtime_from_session(&ws, "card-1".into(), None).unwrap_err();
            let expected = format!(
                "unmappable session identity (provider={provider:?}, contract={contract:?})"
            );

            assert!(
                matches!(
                    err,
                    WorkerSessionProjectionRepoError::Message { ref message } if message == &expected
                ),
                "unexpected error: {err:?}"
            );
        }
    }
}
