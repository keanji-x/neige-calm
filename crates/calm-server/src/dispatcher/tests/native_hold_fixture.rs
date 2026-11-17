//! A legal native Hold for the dispatcher reader contract: real session and self Card scope.
#![cfg(test)]
use super::*;
use crate::db::sqlite::{card_create_with_id_tx, session_start_runtime_tx};
use crate::model::{CardRole, NewCard, new_id, now_ms};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};

pub(super) async fn seed(repo: &crate::db::sqlite::SqlxRepo, track: &str) -> i64 {
    let track = TrackId::from(track);
    let db = repo.db().clone();
    db.writer().with_writer(move |writer| Box::pin(async move {
        let mut tx = writer.begin_immediate().await.unwrap();
        let card = card_create_with_id_tx(&mut tx, format!("planner-{track}"), NewCard {
            track_id: track.clone(), title: None, kind: "codex".into(), sort: None,
            payload: serde_json::json!({"planner_harness":true,"planner_provider":"codex","permission_mode":"ask"}),
        }, CardRole::Planner, false, &CardRoleCache::new()).await.unwrap();
        let session_id = new_id();
        session_start_runtime_tx(&mut tx, WorkerSessionInit {
            id: session_id.clone(), card_id: card.id.to_string(), kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex), status: WorkerSessionState::Idle,
            terminal_run_id: None, thread_id: Some("dispatcher-native-hold".into()), session_id: None,
            active_turn_id: None, handle_state_json: None, spawn_op_id: None, now_ms: now_ms(),
        }).await.unwrap();
        let (scope, event) = crate::ask::hold_ask_requested_tx(&mut tx, &card.id, vec![AskQuestion {
            detail: None, title: "Merge PR #7 (head abc)?".into(), options: vec!["Merge".into(), "Hold".into()],
        }]).await.unwrap();
        let id = crate::db::sqlite::append_decision_event_in_tx(&mut tx,
            &ActorId::AiPlannerSession(WorkerSessionId(session_id)), &scope, None, &event).await.unwrap();
        tx.commit().await.unwrap();
        id
    })).await.unwrap()
}
