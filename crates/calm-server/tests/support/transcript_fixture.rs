//! Transcript writers accept only the card's current session, so a fixture that writes rows
//! through them first makes its session the card's current one, as a live start does.
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
