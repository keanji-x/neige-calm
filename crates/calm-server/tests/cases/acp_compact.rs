use super::*;

async fn create(stack: &Stack) -> (String, String) {
    // Clear the legacy stack helper's historical selectors for canonical OpenCode creation.
    stack
        .create_claude_track_with(json!({
            "planner_provider":"opencode", "template_id":null, "attach_folder":false
        }))
        .await
}

async fn wait_phase(stack: &Stack, worker: &str, phase: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let row = stack
                .repo()
                .session_projection_by_id(worker)
                .await
                .unwrap()
                .unwrap();
            if row
                .handle_state_json
                .as_ref()
                .is_some_and(|frame| frame["phase"] == phase)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("persisted canonical phase");
}
async fn turn(stack: &Stack, card: &str, text: &str, n: usize) -> Value {
    let (status, body) = stack.post_input(card, text).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcome = stack.wait_outcomes(card, n).await[n - 1].clone();
    wait_phase(stack, &stack.runtime(card).await.id, "turn_completed").await;
    outcome
}

async fn compact(stack: &Stack, card: &str) -> (StatusCode, Value) {
    stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/compact"),
            Some(json!({})),
        )
        .await
}
fn compact_prompts(root: &Root) -> Vec<Value> {
    requests(root, "session/prompt")
        .into_iter()
        .filter(|r| r["params"]["prompt"] == json!([{"type":"text","text":"/compact"}]))
        .collect()
}
async fn human_state(stack: &Stack, card: &str) -> Vec<(String, i64, String, String)> {
    sqlx::query_as(concat!(
        "SELECT m.id,m.current_revision,r.payload_json,r.body_sha256 ",
        "FROM conversation_messages m JOIN conversation_message_revisions r ",
        "ON r.message_id=m.id WHERE m.card_id=?1 ORDER BY m.id,r.revision",
    ))
    .bind(card)
    .fetch_all(stack.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap()
}
async fn snapshot(stack: &Stack, card: &str) -> Value {
    stack.runtime(card).await.handle_state_json.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_success_preserves_inputs_and_settles_once() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    turn(&stack, &card, "first", 1).await;
    let before = snapshot(&stack, &card).await;
    let humans = human_state(&stack, &card).await;
    assert_eq!(humans.len(), 1);
    let rows =
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await;
    let runtime = stack.runtime(&card).await;
    let cleanup = calm_server::acp_planner::test_seams::pause_after_fence(&runtime.id);
    std::fs::write(root.path().join("scenario"), "compact-success").unwrap();
    // The maintenance request must cross C4.3's release boundary before ACP
    // initialize, then retain that same claim until cleanup settles.
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    use calm_truth::db::sqlite::provider_execution as claims;
    let release = PausePoint::default();
    install_pause_for_test("c4-release-authorized", &runtime.id, release.clone());
    let initializes = requests(&root, "initialize").len();
    let (reply, admitted) = tokio::join!(compact(&stack, &card), async {
        tokio::time::timeout(Duration::from_secs(20), release.entered.notified())
            .await
            .unwrap();
        let pool = stack.repo().sqlite_db().unwrap();
        let claim = claims::load(pool.read_pool(), &runtime.id)
            .await
            .unwrap()
            .unwrap();
        let observed = requests(&root, "initialize").len();
        release.release.notify_one();
        assert_eq!(
            observed, initializes,
            "no maintenance ACP byte before release"
        );
        assert!(matches!(
            claim.record().release,
            claims::Release::Releasing { .. }
        ));
        claim
    });
    assert!(matches!(
        admitted.record().purpose,
        claims::Purpose::Compaction { .. }
    ));
    let (status, body) = reply;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::timeout(Duration::from_secs(20), cleanup.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        snapshot(&stack, &card).await["compact_outcome"]["status"],
        "started"
    );
    let pool = stack.repo().sqlite_db().unwrap();
    let held = claims::load(pool.read_pool(), &runtime.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(held.record().execution_id, admitted.record().execution_id);
    cleanup.release.notify_one();
    wait_phase(&stack, &runtime.id, "turn_completed").await;
    assert!(
        claims::load(pool.read_pool(), &runtime.id)
            .await
            .unwrap()
            .is_none()
    );
    let (desired, settled): (String, Option<i64>) = sqlx::query_as(
        "SELECT desired,settled_at_ms FROM workspace_executions WHERE execution_id=?1",
    )
    .bind(&admitted.record().execution_id)
    .fetch_one(pool.read_pool())
    .await
    .unwrap();
    assert_eq!(desired, "retire");
    assert!(
        settled.is_some(),
        "maintenance must settle the Engine process"
    );
    let after = snapshot(&stack, &card).await;
    assert_eq!(after["compact_outcome"]["status"], "completed", "{after}");
    assert_eq!(after["last_turn_id"], before["last_turn_id"]);
    assert_eq!(human_state(&stack, &card).await, humans);
    assert_eq!(
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await,
        rows
    );
    assert_eq!(compact_prompts(&root).len(), 1);
    assert_eq!(stack.outcomes(&card).await.len(), 1);
    for request in requests(&root, "initialize")
        .into_iter()
        .filter(|r| r["_pid"] == compact_prompts(&root)[0]["_pid"])
    {
        assert_eq!(
            request["params"]["clientCapabilities"],
            json!({"session":{"compaction":{}}})
        );
    }
    let pool = stack.repo().sqlite_db().unwrap();
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
    assert!(token.is_none());
    turn(&stack, &card, "after compaction", 2).await;
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_failure_cancel_loss_and_other_ids_never_complete_or_resend() {
    for scenario in [
        "compact-failed",
        "compact-cancelled",
        "compact-lost",
        "compact-death",
        "compact-other-id",
        "compact-no-response",
    ] {
        let root = Root::new("unused");
        let stack = boot(&root).await;
        let (_, card) = create(&stack).await;
        turn(&stack, &card, "first", 1).await;
        std::fs::write(root.path().join("scenario"), scenario).unwrap();
        let (status, body) = compact(&stack, &card).await;
        assert_eq!(status, StatusCode::OK, "{scenario}: {body}");
        let runtime = stack.runtime(&card).await;
        wait_phase(&stack, &runtime.id, "wedged").await;
        assert_eq!(
            snapshot(&stack, &card).await["compact_outcome"]["status"],
            "unconfirmed",
            "{scenario}"
        );
        let (status, _) = compact(&stack, &card).await;
        assert!(
            !status.is_success(),
            "{scenario}: cannot resubmit held maintenance"
        );
        assert_eq!(compact_prompts(&root).len(), 1, "{scenario}");
        stack.shutdown().await;
        let stack = boot(&root).await;
        let runtime = stack.runtime(&card).await;
        // Authenticated input wakes the retained canonical carrier, still held by maintenance.
        let (status, body) = stack.post_input(&card, "later pending input").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        wait_phase(&stack, &runtime.id, "wedged").await;
        assert_eq!(
            compact_prompts(&root).len(),
            1,
            "{scenario}: restart cannot resend"
        );
        assert_ne!(
            snapshot(&stack, &card).await["compact_outcome"]["status"],
            "completed"
        );
        assert_eq!(runtime.id, stack.runtime(&card).await.id);
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_requires_declared_command() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    turn(&stack, &card, "first", 1).await;
    std::fs::write(root.path().join("scenario"), "compact-undeclared").unwrap();
    assert!(!compact(&stack, &card).await.0.is_success());
    assert!(compact_prompts(&root).is_empty());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_assistant_and_grok_capability_contract() {
    use axum::http::Request;
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, _) = create(&stack).await;
    let (status, row) = stack
        .send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", "compact-assistant"),
            Some(json!({"provider":"opencode","text":"first assistant input"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    stack.wait_outcomes(card, 1).await;
    let runtime = stack.runtime(card).await;
    wait_phase(&stack, &runtime.id, "turn_completed").await;
    std::fs::write(root.path().join("scenario"), "compact-success").unwrap();
    let (status, body) = compact(&stack, card).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_phase(&stack, &runtime.id, "turn_completed").await;
    assert_eq!(
        snapshot(&stack, card).await["compact_outcome"]["status"],
        "completed"
    );
    assert_eq!(compact_prompts(&root).len(), 1);
    stack.shutdown().await;

    let root = Root::new("unused");
    let stack = boot_as(&root, "grok", json!({"ACP_FIXTURE_IDENTITY":"meta","ACP_FIXTURE_VERSION":"1.0.62","ACP_FIXTURE_AUTH_METHODS":"cached_token","ACP_FIXTURE_VENDOR":"xai"}), "1.0.62").await;
    let (_, card) = stack
        .create_claude_track_with(json!({"planner_provider":"grok","title":"Grok unchanged"}))
        .await;
    turn(&stack, &card, "first", 1).await;
    assert!(!compact(&stack, &card).await.0.is_success());
    assert!(compact_prompts(&root).is_empty());
    assert!(!requests(&root, "initialize").is_empty());
    for request in requests(&root, "initialize") {
        assert_eq!(request["params"]["clientCapabilities"], json!({}));
    }
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_cleanup_failure_cannot_settle_completed() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    turn(&stack, &card, "first", 1).await;
    let runtime = stack.runtime(&card).await;
    let cleanup = calm_server::acp_planner::test_seams::pause_after_fence(&runtime.id);
    std::fs::write(root.path().join("scenario"), "compact-success").unwrap();
    assert_eq!(compact(&stack, &card).await.0, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(20), cleanup.entered.notified())
        .await
        .unwrap();
    let pool = stack.repo().sqlite_db().unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_compact_revoke BEFORE UPDATE OF mcp_token_hash ",
        "ON worker_sessions WHEN NEW.mcp_token_hash IS NULL ",
        "BEGIN SELECT RAISE(FAIL,'fixture revoke failure'); END",
    ))
    .execute(pool.writer_pool())
    .await
    .unwrap();
    cleanup.release.notify_one();
    wait_phase(&stack, &runtime.id, "wedged").await;
    assert_eq!(
        snapshot(&stack, &card).await["compact_outcome"]["status"],
        "unconfirmed"
    );
    assert_eq!(compact_prompts(&root).len(), 1);
    sqlx::query("DROP TRIGGER refuse_compact_revoke")
        .execute(pool.writer_pool())
        .await
        .unwrap();
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_lost_driver_recovers_only_after_settlement_without_input_end() {
    use calm_server::provider_execution as execution;
    use calm_truth::db::sqlite::provider_execution as claims;
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    turn(&stack, &card, "first", 1).await;
    let humans = human_state(&stack, &card).await;
    let runtime = stack.runtime(&card).await;
    let session = stack.harness(&runtime.id).acp_session_for_test().unwrap();
    let cleanup = calm_server::acp_planner::test_seams::pause_after_fence(&runtime.id);
    std::fs::write(root.path().join("scenario"), "compact-success").unwrap();
    assert_eq!(compact(&stack, &card).await.0, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(20), cleanup.entered.notified())
        .await
        .unwrap();
    let pool = stack.repo().sqlite_db().unwrap();
    let admitted = claims::load(pool.read_pool(), &runtime.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        admitted.record().purpose,
        claims::Purpose::Compaction { .. }
    ));
    let id = execution::execution_id(admitted.record()).unwrap();
    assert!(
        session
            .host()
            .provider_execution
            .finish(
                &pool,
                session.host().execution.engine(),
                &runtime.id,
                &admitted,
                None
            )
            .await
            .is_err(),
        "live process cannot complete"
    );
    session.abort_driver_for_test().await;
    cleanup.release.notify_one();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            session
                .host()
                .provider_execution
                .sweep(&pool, session.host().execution.engine())
                .await
                .unwrap();
            if claims::load(pool.read_pool(), &runtime.id)
                .await
                .unwrap()
                .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (desired, settled): (String, Option<i64>) = sqlx::query_as(
        "SELECT desired,settled_at_ms FROM workspace_executions WHERE execution_id=?",
    )
    .bind(id.as_str())
    .fetch_one(pool.read_pool())
    .await
    .unwrap();
    assert_eq!(desired, "retire");
    assert!(settled.is_some());
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
            .bind(&runtime.id)
            .fetch_one(pool.read_pool())
            .await
            .unwrap();
    assert!(token.is_none());
    wait_phase(&stack, &runtime.id, "wedged").await;
    assert_eq!(
        snapshot(&stack, &card).await["compact_outcome"]["status"],
        "unconfirmed"
    );
    assert_eq!(stack.outcomes(&card).await.len(), 1);
    assert_eq!(human_state(&stack, &card).await, humans);
    assert_eq!(compact_prompts(&root).len(), 1);
    assert!(!compact(&stack, &card).await.0.is_success());
    assert_eq!(compact_prompts(&root).len(), 1);
    stack.shutdown().await;
}

async fn review_unknown(scenario: &str) {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    turn(&stack, &card, "first", 1).await;
    let humans = human_state(&stack, &card).await;
    std::fs::write(root.path().join("scenario"), scenario).unwrap();
    assert_eq!(compact(&stack, &card).await.0, StatusCode::OK);
    let runtime = stack.runtime(&card).await;
    let after = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let s = snapshot(&stack, &card).await;
            if s["compact_outcome"]["status"] != "started" {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(compact_prompts(&root).len(), 1);
    assert_eq!(human_state(&stack, &card).await, humans);
    assert_eq!(stack.outcomes(&card).await.len(), 1);
    let token: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    assert!(token.is_none(), "credential was not revoked");
    stack.shutdown().await;
    assert_eq!(
        after["compact_outcome"]["status"], "unconfirmed",
        "{scenario}: {after}"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_review_auto_start_races_manual() {
    review_unknown("compact-review-auto-first").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_compact_review_replayed_settled_id_restarts() {
    review_unknown("compact-review-replayed-id").await;
}
