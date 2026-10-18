//! Receipt admission through the real owner-bound managed start.
use super::*;
use calm_server::harness::QueueEntry;
use provider::control::{CorrelationKey, InputRequest, TurnEdit, TurnRequest};

fn request(client: &str) -> TurnRequest {
    TurnRequest {
        input: InputRequest {
            items: vec![provider::InputItem::text("receipt input")],
            correlation: CorrelationKey(client.into()),
        },
        selection: provider::TurnModelSelection::inherit(),
        permission: calm_server::planner_permission_mode::PlannerPermissionMode::Never,
        edit: TurnEdit::NoEdit,
    }
}
fn launches(root: &Root) -> usize {
    std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
        .count()
}
async fn receipt(stack: &Stack, worker: &str) -> (String, String, String) {
    sqlx::query_as(
        "SELECT client_id,turn_id,input_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(worker)
    .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_pipe_failure_after_prepare_retires_original_batch_without_resend() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    std::fs::write(root.path().join("scenario"), "closed-input").unwrap();
    let outcome = turn(&stack, &card, "original batch", 1).await;
    let (_, turn_id, input) = receipt(&stack, &runtime.id).await;
    assert_eq!(outcome["id"], turn_id);
    assert_eq!(outcome["status"], "failed");
    assert!(
        outcome["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outcome is unknown")
    );
    let input: Value = serde_json::from_str(&input).unwrap();
    assert_eq!(input["queue"].as_array().unwrap().len(), 1);
    assert!(
        input["input"][0]["text"]
            .as_str()
            .unwrap()
            .contains("original batch")
    );
    assert!(
        stack
            .harness(&runtime.id)
            .snapshot()
            .await
            .pending_entries()
            .is_empty()
    );
    assert!(
        requests(&root, "session/prompt").is_empty(),
        "broken pipe must precede any peer read"
    );
    let before = launches(&root);
    let (status, body) = stack.post_input(&card, "later batch").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handle = stack.harness(&runtime.id);
    tokio::time::timeout(Duration::from_secs(20), async {
        while handle.refused_issuances_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("later input must reach the durable fence");
    assert_eq!(launches(&root), before);
    assert!(requests(&root, "session/prompt").is_empty());
    let pending = stack
        .harness(&runtime.id)
        .snapshot()
        .await
        .pending_entries();
    assert_eq!(pending.len(), 1);
    assert!(format!("{:?}", pending[0]).contains("later batch"));
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_same_key_replay_checks_exact_claim_without_spawn_setup_or_prompt() {
    for state in ["sending", "unknown", "completed"] {
        let root = Root::new("unused");
        let stack = boot(&root).await;
        let (_, card) = create(&stack).await;
        let runtime = stack.runtime(&card).await;
        let harness = stack.harness(&runtime.id);
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
        let pause = calm_server::acp_planner::test_seams::pause_settlement(&runtime.id);
        let turn_id = harness
            .turn_start_for_test(request("same-key"), &claim)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
            .await
            .unwrap();
        if state != "sending" {
            pause.release.notify_one();
            stack.wait_outcomes(&card, 1).await;
        }
        let stored_state: String =
            sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
                .bind(&runtime.id)
                .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
                .await
                .unwrap();
        assert_eq!(stored_state, state);
        let (_, stored_turn, bytes) = receipt(&stack, &runtime.id).await;
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
            harness
                .turn_start_for_test(request("same-key"), &claim)
                .await
                .unwrap(),
            turn_id
        );
        let mut revised = claim.clone();
        assert!(revised[1].bump_rev_for_restore());
        let error = harness
            .turn_start_for_test(request("same-key"), &revised)
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("ACP receipt ownership or original input changed"));
        let replaced = vec![
            claim[0].clone(),
            QueueEntry::user_message("same".into(), None, vec![]),
        ];
        let error = harness
            .turn_start_for_test(request("same-key"), &replaced)
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("ACP receipt ownership or original input changed"));
        assert_eq!(
            receipt(&stack, &runtime.id).await.2,
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
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_two_owner_decoy_is_untouched_by_bound_start_interrupt_and_shutdown() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, a) = create(&stack).await;
    let (_, b) = create(&stack).await;
    let ra = stack.runtime(&a).await;
    let rb = stack.runtime(&b).await;
    let ha = stack.harness(&ra.id);
    let hb = stack.harness(&rb.id);
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let claim = vec![QueueEntry::user_message("owner input".into(), None, vec![])];
    let tb = hb
        .turn_start_for_test(request("shared-key"), &claim)
        .await
        .unwrap();
    stack.wait_phase(&rb.id, "turn_running").await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while requests(&root, "session/prompt").is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let before = serde_json::to_value(hb.snapshot().await).unwrap();
    let b_receipt = receipt(&stack, &rb.id).await;
    let b_native = stack.runtime(&b).await.session_id.unwrap();
    let b_pid = requests(&root, "session/prompt")
        .into_iter()
        .find(|r| r["params"]["sessionId"] == b_native)
        .unwrap()["_pid"]
        .as_u64()
        .unwrap();
    let ta = ha
        .turn_start_for_test(request("shared-key"), &claim)
        .await
        .unwrap();
    assert_ne!(ta, tb, "same client key is scoped to its owner");
    ha.set_state_for_test(calm_server::harness::HarnessState::turn_running(
        ta,
        std::time::Instant::now(),
    ))
    .await;
    ha.interrupt("selected owner only".into()).await.unwrap();
    ha.shutdown().await.unwrap();
    assert_eq!(receipt(&stack, &rb.id).await, b_receipt);
    assert_eq!(serde_json::to_value(hb.snapshot().await).unwrap(), before);
    assert!(
        requests(&root, "session/cancel")
            .iter()
            .all(|r| r["params"]["sessionId"] != b_native)
    );
    assert_eq!(
        requests(&root, "session/prompt")
            .iter()
            .filter(|r| r["params"]["sessionId"] == b_native)
            .count(),
        1
    );
    assert!(
        std::path::Path::new(&format!("/proc/{b_pid}")).exists(),
        "decoy process must remain alive"
    );
    stack.shutdown().await;
}
