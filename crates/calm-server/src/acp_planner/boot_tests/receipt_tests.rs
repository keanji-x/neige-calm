//! Exact-key replay through the existing crate-private managed ACP start entry.
//! HTTP deduplicates sends and recovery retires receipted claims before starting;
//! neither can submit the same key with a changed identity/revision to this fence.
//! Keep this adversarial check in-crate rather than exposing a fixture start API.
#![cfg(test)]
use super::*;
use crate::acp_planner::config::AcpPlannerHost;
use crate::acp_planner::session::{AcpPlannerSession, SessionParams};
use crate::harness::{QueueEntry, backend::ordinary_start_result};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
fn requests(root: &tempfile::TempDir, method: &str) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|frame| frame["method"] == method)
        .collect()
}
fn launches(root: &tempfile::TempDir) -> usize {
    std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
        .count()
}
async fn receipt(repo: &SqlxRepo, worker: &str) -> (String, String, String) {
    sqlx::query_as(
        "SELECT client_id,turn_id,input_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(worker)
    .fetch_one(repo.pool())
    .await
    .unwrap()
}
use provider::control::{CorrelationKey, InputRequest, TurnEdit, TurnRequest};

fn request(client: &str) -> TurnRequest {
    TurnRequest {
        input: InputRequest {
            items: vec![provider::InputItem::text("receipt input")],
            correlation: CorrelationKey(client.into()),
        },
        selection: provider::TurnModelSelection::inherit(),
        permission: crate::planner_permission_mode::PlannerPermissionMode::Never,
        edit: TurnEdit::NoEdit,
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_same_key_replay_checks_exact_claim_without_spawn_setup_or_prompt() {
    for state in ["sending", "unknown", "completed"] {
        let root = tempfile::tempdir().unwrap();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let row = owner(&repo, root.path(), AgentProvider::OpenCode).await;
        let host = Arc::new(AcpPlannerHost::new(
            Some(serde_json::from_value(json!({"agents":[{
                "provider":"opencode", "command":"/usr/bin/python3",
                "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
                "env":{"ACP_FIXTURE_ROOT":root.path()},
                "expected_agent_name":"Fixture ACP", "expected_agent_version":"1"
            }]})).unwrap()), root.path(), root.path().join("unused-shim"), root.path().join("mcp.sock")
        ).unwrap());
        let session = AcpPlannerSession::open(SessionParams {
            host,
            provider: AgentProvider::OpenCode,
            repo: repo.clone(),
            seals: Arc::new(crate::thread_seals::ThreadSeals::new()),
            worker_session_id: row.worker.clone(),
            card_id: row.card.clone(),
            track_id: row.track.clone(),
            cwd: root.path().into(),
            instructions: String::new(),
        });
        let (held, _receiver) = crate::harness::held_requests::channel();
        session.mark_installed(held);
        std::fs::write(root.path().join("scenario"), "reply").unwrap();
        let claim = vec![
            QueueEntry::LegacyUser {
                text: "same".into(),
                envelope_id: None,
                message_ids: vec![],
            },
            QueueEntry::user_message("same".into(), None, vec![]),
        ];
        if state == "unknown" {
            std::fs::write(root.path().join("scenario"), "lost").unwrap();
        }
        let pause = crate::acp_planner::test_seams::pause_settlement(&row.worker);
        let turn_id = ordinary_start_result(
            session
                .start_turn(&row.thread, request("same-key"), &claim)
                .await,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
            .await
            .unwrap();
        if state != "sending" {
            pause.release.notify_one();
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let stored: String = sqlx::query_scalar(
                        "SELECT state FROM acp_submissions WHERE worker_session_id=?1",
                    )
                    .bind(&row.worker)
                    .fetch_one(repo.pool())
                    .await
                    .unwrap();
                    if stored == state {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap();
        }
        let stored_state: String =
            sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
                .bind(&row.worker)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(stored_state, state);
        let (_, stored_turn, bytes) = receipt(&repo, &row.worker).await;
        assert_eq!(stored_turn, turn_id);
        let input: Value = serde_json::from_str(&bytes).unwrap();
        // Literal independent codec oracle, including a legacy entry with no invented identity.
        let view = claim[1].user_view().unwrap();
        assert_eq!(
            input["queue"],
            json!([
                {"kind":"legacy-user","id":null,"observation":{"type":"user_message","text":"same"},"envelopeId":null,"messageIds":[],"attachments":[],"rev":null,"queuedAtMs":null},
                {"kind":"user","id":claim[1].id(),"observation":{"type":"user_message","text":"same"},"envelopeId":null,"messageIds":claim[1].message_ids(),"attachments":[],"rev":0,"queuedAtMs":view.queued_at_ms}
            ])
        );
        assert_eq!(
            input["input"],
            json!([{"type":"text","text":"receipt input"}])
        );
        assert_eq!(
            input["prompt"].as_array().unwrap().last().unwrap(),
            &json!({"type":"text","text":"receipt input"})
        );
        let before = (
            launches(&root),
            requests(&root, "initialize"),
            requests(&root, "session/new"),
            requests(&root, "session/load"),
            requests(&root, "session/prompt"),
        );
        assert_eq!(
            ordinary_start_result(
                session
                    .start_turn(&row.thread, request("same-key"), &claim)
                    .await
            )
            .unwrap(),
            turn_id
        );
        let mut revised = claim.clone();
        assert!(revised[1].bump_rev_for_restore());
        let error = session
            .start_turn(&row.thread, request("same-key"), &revised)
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("ACP receipt ownership or original input changed"));
        let replaced = vec![
            claim[0].clone(),
            QueueEntry::user_message("same".into(), None, vec![]),
        ];
        let error = session
            .start_turn(&row.thread, request("same-key"), &replaced)
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("ACP receipt ownership or original input changed"));
        assert_eq!(
            receipt(&repo, &row.worker).await.2,
            bytes,
            "replay preserves exact receipt bytes"
        );
        assert_eq!(
            (
                launches(&root),
                requests(&root, "initialize"),
                requests(&root, "session/new"),
                requests(&root, "session/load"),
                requests(&root, "session/prompt")
            ),
            before
        );
        pause.release.notify_one();
        session.shutdown().await.unwrap();
    }
}
