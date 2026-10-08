//! Worker-flow read-model sink: commits each complete source record and checkpoint.
//! Writes go through the out-of-domain repo directly — no Event, no decision gate.

use std::sync::Arc;

use async_trait::async_trait;
use calm_exec::flow::{CaptureBatch, CaptureOutcome, FlowRowCtx, WorkerFlowItemSink};
use calm_types::error::CoreError;

use crate::db::RepoOutOfDomain;
use crate::db::worker_flow_capture::{CaptureItem, WorkerFlowCapture};

/// Serializes whole source records before the repository commits their checkpoint.
pub struct WorkerFlowSink {
    repo: Arc<dyn RepoOutOfDomain>,
}

impl WorkerFlowSink {
    pub fn new(repo: Arc<dyn RepoOutOfDomain>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl WorkerFlowItemSink for WorkerFlowSink {
    async fn capture_batch(
        &self,
        ctx: &FlowRowCtx,
        batch: &CaptureBatch,
    ) -> Result<CaptureOutcome, CoreError> {
        if ctx.card_id.as_deref() != Some(batch.card_id.as_str()) {
            return Err(CoreError::Internal(
                "capture batch card differs from row context".into(),
            ));
        }
        // Serialize the whole record before starting a transaction. No partial encoding writes.
        let items = batch
            .items
            .iter()
            .map(|item| {
                let value = serde_json::to_value(item)?;
                let kind = value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| CoreError::Internal("worker flow item has no type".into()))?
                    .to_owned();
                Ok(CaptureItem {
                    kind,
                    payload: serde_json::to_string(item)?,
                })
            })
            .collect::<Result<Vec<_>, CoreError>>()?;
        let capture = WorkerFlowCapture {
            card_id: batch.card_id.clone(),
            source_kind: batch.source_kind.clone(),
            session_id: ctx.session_id.clone(),
            track_id: ctx.track_id.clone(),
            expected: batch.expected.clone(),
            next: batch.next.clone(),
            items,
        };
        self.repo
            .worker_flow_capture_commit(&capture)
            .await
            .map_err(|e| match e {
                crate::TruthError::Db(ref db) if crate::db::sqlite::is_sqlite_busy(db) => {
                    CoreError::ServiceUnavailable(format!("worker_flow_capture_commit: {e}"))
                }
                _ => CoreError::Internal(format!("worker_flow_capture_commit: {e}")),
            })
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
        use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};

        let mut tx = repo.pool().begin().await.unwrap();
        let area = crate::db::sqlite::area_create_tx(
            &mut tx,
            NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let track = crate::db::sqlite::track_create_tx(
            &mut tx,
            NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
            None,
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        let card = crate::db::sqlite::card_create_tx(
            &mut tx,
            NewCard {
                track_id: track.id.clone(),
                title: None,
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
                track_id: track.id,
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
            track_id: Some("track-x".to_string()),
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
        let batch = CaptureBatch {
            card_id: card_id.clone(),
            source_kind: "test".into(),
            expected: calm_exec::flow::CaptureCheckpoint::Missing,
            next: calm_exec::flow::CapturePosition {
                source_path: "test.jsonl".into(),
                record_index: 1,
                byte_offset: 10,
                last_source_uuid: None,
                last_line_hash: Some("hash".into()),
            },
            items: items.clone(),
        };
        assert!(matches!(
            sink.capture_batch(&ctx, &batch).await.unwrap(),
            CaptureOutcome::Applied(_)
        ));

        let rows = repo
            .worker_flow_item_list_by_card(&card_id, 0, 100, false)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, "userMessage");
        assert_eq!(rows[1].kind, "commandExecution");
        assert_eq!(rows[2].kind, "fileChange");
        assert_eq!(rows[0].card_id.as_deref(), Some(card_id.as_str()));
        assert_eq!(rows[0].captured_session_id.as_deref(), Some(SESSION_ID));
        assert_eq!(rows[0].worker_session_id.as_deref(), Some(SESSION_ID));
        assert_eq!(rows[0].track_id.as_deref(), Some("track-x"));

        for (row, item) in rows.iter().zip(items.iter()) {
            let back: WorkerFlowItem = serde_json::from_str(&row.payload).unwrap();
            assert_eq!(&back, item);
        }
    }
}
