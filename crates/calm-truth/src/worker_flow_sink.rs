//! Worker-flow read-model sink (#695 PR3).
//!
//! [`WorkerFlowSink`] is calm-truth's implementation of
//! [`calm_exec::flow::WorkerFlowItemSink`]: it stamps row-context onto each
//! normalized [`WorkerFlowItem`] and appends it to the `worker_flow_items`
//! capture table.
//!
//! Like `run_loop`'s direct `harness_item_insert`, the sink writes via
//! [`RepoOutOfDomain::worker_flow_item_insert`] **directly** — these
//! out-of-domain writes deliberately emit no [`Event`](crate::event::Event)
//! and pass through no [`DecisionGate`](crate::decision_gate). The capture
//! stream is a passive read-model feed, not a domain mutation.

use std::sync::Arc;

use async_trait::async_trait;
use calm_exec::flow::{FlowRowCtx, WorkerFlowItemSink};
use calm_types::error::CoreError;
use calm_types::worker_flow::WorkerFlowItem;

use crate::db::RepoOutOfDomain;
use crate::model::now_ms;

/// Read-model writer that appends captured worker-flow items to the
/// `worker_flow_items` table.
pub struct WorkerFlowSink {
    repo: Arc<dyn RepoOutOfDomain>,
}

impl WorkerFlowSink {
    /// Wrap the out-of-domain repo handle the sink writes through.
    pub fn new(repo: Arc<dyn RepoOutOfDomain>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl WorkerFlowItemSink for WorkerFlowSink {
    async fn record(&self, ctx: &FlowRowCtx, item: WorkerFlowItem) -> Result<(), CoreError> {
        // `kind` is the serde `"type"` tag of the variant; `payload` is the
        // item's full JSON form (flattened envelope + payload).
        let value = serde_json::to_value(&item)?;
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let payload = serde_json::to_string(&item)?;

        // Direct out-of-domain insert — no Event, no gate (db/mod.rs trait
        // doc), exactly like run_loop's `harness_item_insert`.
        // Worker-flow sessions are runtime-keyed after #695 PR5.
        self.repo
            .worker_flow_item_insert(
                ctx.card_id.as_deref(),
                Some(ctx.session_id.as_str()),
                ctx.wave_id.as_deref(),
                Some(ctx.session_id.as_str()),
                kind,
                &payload,
                now_ms(),
            )
            .await
            .map_err(|e| CoreError::Internal(format!("worker_flow_item_insert: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::RepoRead;
    use crate::db::sqlite::SqlxRepo;
    use calm_types::worker::{
        LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
        WorkerSessionId, WorkerSessionState,
    };
    use calm_types::worker_flow::{
        ExecSource, ExecStatus, FileChangeKind, FileEdit, FlowEnvelope, MessageBlock, PatchStatus,
        ToolCallId, WorkerFlowItem,
    };

    const SESSION_ID: &str = "rt-sink-3";

    fn env(seq: u64, turn: u32) -> FlowEnvelope {
        FlowEnvelope {
            seq,
            turn,
            session_id: WorkerSessionId::from(SESSION_ID),
            provider: WorkerProviderKind::Codex,
            timestamp: Some(1_700_000_000),
            source_uuid: None,
            provider_extra: None,
            raw_ref: None,
        }
    }

    async fn seed_card(repo: &SqlxRepo) -> String {
        use crate::db::sqlite::session_insert_tx;
        use crate::model::{NewCard, NewCove, NewWave, RequestTheme};

        let mut tx = repo.pool().begin().await.unwrap();
        let cove = crate::db::sqlite::cove_create_tx(
            &mut tx,
            NewCove {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = crate::db::sqlite::wave_create_tx(
            &mut tx,
            NewWave {
                workflow_input: None,
                cove_id: cove.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .unwrap();
        let card = crate::db::sqlite::card_create_tx(
            &mut tx,
            NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: Some(0.0),
                payload: serde_json::json!({ "task": "x" }),
            },
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        session_insert_tx(
            &mut tx,
            WorkerSession {
                id: WorkerSessionId::from(SESSION_ID),
                wave_id: wave.id,
                provider: WorkerProviderKind::Codex,
                mode: SessionMode::Resumable,
                contract: WorkerContract::Executor,
                parent_session_id: None,
                requester_session_id: None,
                state: WorkerSessionState::Running,
                mcp_token_hash: None,
                thread_id: Some("thread-sink-3".into()),
                agent_session_id: Some("agent-sink-3".into()),
                active_turn_id: None,
                terminal_run_id: None,
                card_id: Some(card.id.clone()),
                handle_state_json: None,
                liveness: LivenessTag::Alive,
                liveness_probed_at_ms: None,
                exit_code: None,
                exit_interpretation: None,
                spawn_op_id: None,
                last_activity_ms: None,
                last_thread_status: None,
                created_at_ms: 1,
                updated_at_ms: 1,
                completed_at_ms: None,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        card.id.as_str().to_string()
    }

    #[tokio::test]
    async fn record_round_trips_kind_payload_and_order() {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let card_id = seed_card(&repo).await;
        let sink = WorkerFlowSink::new(repo.clone());

        let ctx = FlowRowCtx {
            session_id: WorkerSessionId::from(SESSION_ID),
            wave_id: Some("wave-x".to_string()),
            card_id: Some(card_id.clone()),
        };

        let items = vec![
            WorkerFlowItem::UserMessage {
                env: env(0, 1),
                content: vec![MessageBlock::Text {
                    text: "do the thing".into(),
                }],
            },
            WorkerFlowItem::CommandExecution {
                env: env(1, 1),
                call_id: Some(ToolCallId::from("c1")),
                command: "ls".into(),
                cwd: None,
                parsed_actions: vec![],
                aggregated_output: None,
                exit_code: Some(0),
                duration_ms: None,
                status: ExecStatus::Completed,
                source: ExecSource::Agent,
            },
            WorkerFlowItem::FileChange {
                env: env(2, 1),
                call_id: None,
                changes: vec![FileEdit {
                    path: "a.rs".into(),
                    kind: FileChangeKind::Add,
                    diff: None,
                }],
                status: PatchStatus::Completed,
            },
        ];
        for item in &items {
            sink.record(&ctx, item.clone()).await.unwrap();
        }

        let rows = repo
            .worker_flow_item_list_by_card(&card_id, 0, 100, false)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        // Stored in append order.
        assert_eq!(rows[0].kind, "userMessage");
        assert_eq!(rows[1].kind, "commandExecution");
        assert_eq!(rows[2].kind, "fileChange");
        assert_eq!(rows[0].card_id.as_deref(), Some(card_id.as_str()));
        assert_eq!(rows[0].runtime_id.as_deref(), Some(SESSION_ID));
        assert_eq!(rows[0].worker_session_id.as_deref(), Some(SESSION_ID));
        assert_eq!(rows[0].wave_id.as_deref(), Some("wave-x"));

        // Payload deserializes back to the original item.
        for (row, item) in rows.iter().zip(items.iter()) {
            let back: WorkerFlowItem = serde_json::from_str(&row.payload).unwrap();
            assert_eq!(&back, item);
        }
    }
}
