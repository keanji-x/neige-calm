//! Transcript fixtures establish the same current session identity as a live writer.
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};

pub async fn start(repo: &SqlxRepo, card: &str, session: &str) {
    let kind = match repo.card_role_get(card).await.unwrap().unwrap() {
        calm_server::model::CardRole::Planner => WorkerSessionKind::SharedPlanner,
        calm_server::model::CardRole::Assistant => WorkerSessionKind::ManagedAssistant,
        _ => WorkerSessionKind::CodexCard,
    };
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session.into(),
            card_id: card.into(),
            kind,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// Fail transcript INSERTs while keeping the runtime projection's transcript reads available.
pub async fn fail_inserts(repo: &SqlxRepo, fail: bool) {
    let sql = if fail {
        concat!(
            "CREATE TRIGGER fail_transcript_insert BEFORE INSERT ON harness_items ",
            "BEGIN SELECT RAISE(ABORT, 'injected transcript storage failure'); END"
        )
    } else {
        "DROP TRIGGER fail_transcript_insert"
    };
    sqlx::query(sql).execute(repo.pool()).await.unwrap();
}
