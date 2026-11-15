use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn antigravity_worker_shim_accepts_the_actual_server_name_budget() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("shim-budget", "read_only")).await;
    let task = wait_task(&stack, &track, "shim-budget", TaskStatus::Running).await;
    let attachments = requests(&root, "session/new");
    let servers: Vec<_> = attachments
        .iter()
        .flat_map(|request| request["params"]["mcpServers"].as_array().unwrap())
        .collect();
    assert_eq!(servers.len(), 1);
    let server = servers[0];
    let args: Vec<_> = server["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let native = state(&stack, &task).await.native_session_id.unwrap();
    let token = std::fs::read_to_string(root.path().join(format!("{native}.test-token"))).unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(server["command"].as_str().unwrap())
            .args(&args)
            .env_clear()
            .env("NEIGE_MCP_SOCKET", stack.mcp_socket())
            .env("NEIGE_MCP_TOKEN", token)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("shim exits after stdin closes")
    .unwrap();
    // Settle the native fixture even when the regression assertions fail.
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
    assert_ne!(
        output.status.code(),
        Some(2),
        "shim rejected the worker arguments: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success());
    assert_eq!(
        args.windows(2)
            .find(|pair| pair[0] == "--max-tool-name-bytes")
            .map(|pair| pair[1]),
        Some("41")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn effort_only_uses_provider_model_and_reconfirms_declared_effort() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let mut declaration = payload("effort-only", "read_only");
    declaration["reasoning_effort"] = json!("deep");
    let (track, planner) = declare(&stack, declaration).await;
    let task = wait_task(&stack, &track, "effort-only", TaskStatus::Running).await;
    let state = state(&stack, &task).await;
    assert_eq!(state.effective_model.as_deref(), Some("fixture/model-a"));
    assert_eq!(state.effective_effort.as_deref(), Some("deep"));
    assert_eq!(
        state.selection_source.model,
        WorkerSelectionSource::ProviderDefaultReported
    );
    assert_eq!(
        state.selection_source.effort,
        WorkerSelectionSource::Explicit
    );
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreported_provider_defaults_are_not_invented() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-default-unreported").await;
    let (track, planner) = declare(&stack, payload("unreported", "read_only")).await;
    let task = wait_task(&stack, &track, "unreported", TaskStatus::Running).await;
    let state = state(&stack, &task).await;
    assert!(state.effective_model.is_none());
    assert!(state.effective_effort.is_none());
    assert_eq!(
        state.selection_source.model,
        WorkerSelectionSource::ProviderDefaultUnreported
    );
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changing_model_refreshes_effort_options_before_selection() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-model-options-change").await;
    let mut declaration = payload("changed-options", "read_only");
    declaration["model"] = json!("fixture/model-b");
    declaration["reasoning_effort"] = json!("deep");
    let (track, planner) = declare(&stack, declaration).await;
    let task = wait_task(&stack, &track, "changed-options", TaskStatus::Running).await;
    let options = requests(&root, "session/set_config_option");
    assert_eq!(options[0]["params"]["configId"], "declared-model-key");
    assert_eq!(options[1]["params"]["configId"], "declared-effort-key");
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configuration_mismatch_and_unsupported_effort_fail_before_prompt() {
    for (scenario, effort) in [
        ("worker-config-mismatch", "deep"),
        ("worker-hold", "unavailable"),
    ] {
        let root = Root::new("unused");
        let stack = boot(&root, scenario).await;
        let mut declaration = payload("invalid-config", "read_only");
        declaration["model"] = json!("fixture/model-b");
        declaration["reasoning_effort"] = json!(effort);
        let (track, _) = declare(&stack, declaration).await;
        let task = wait_task(&stack, &track, "invalid-config", TaskStatus::Failed).await;
        assert!(requests(&root, "session/prompt").is_empty());
        wait_released(&stack, &task).await;
        stack.shutdown().await;
    }
}
async fn user_request(
    stack: &Stack,
    track: &str,
    key: &str,
    action: &str,
    actor: &str,
    body: Value,
) -> (axum::http::StatusCode, Value) {
    use tower::ServiceExt;
    let response = stack
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{track}/tasks/{key}/{action}"))
                .header("content-type", "application/json")
                .header("X-Calm-Actor", actor)
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authenticated_user_messages_and_cancel_reuse_native_attempt_authority() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, _) = declare(&stack, payload("user-control", "read_only")).await;
    let task = wait_task(&stack, &track, "user-control", TaskStatus::Running).await;
    let body = json!({"attempt_id":task.id,"idempotency_key":"user-key","text":"Inspect the same frozen objective"});
    for actor in ["ai:codex", "ai:planner", "ai:anything"] {
        assert_eq!(
            user_request(&stack, &track, &task.key, "messages", actor, body.clone())
                .await
                .0,
            axum::http::StatusCode::FORBIDDEN
        );
    }
    let mut stale = body.clone();
    stale["attempt_id"] = json!("old-attempt");
    assert_eq!(
        user_request(&stack, &track, &task.key, "messages", "user", stale)
            .await
            .0,
        axum::http::StatusCode::CONFLICT
    );
    let (other, _) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    assert!(
        !user_request(&stack, &other, &task.key, "messages", "user", body.clone())
            .await
            .0
            .is_success()
    );
    let (status, first) =
        user_request(&stack, &track, &task.key, "messages", "user", body.clone()).await;
    assert!(status.is_success(), "{status}: {first}");
    assert_eq!(
        user_request(&stack, &track, &task.key, "messages", "user", body)
            .await
            .1,
        first
    );
    let native = state(&stack, &task).await.native_session_id.unwrap();
    std::fs::write(
        root.path().join(format!("worker-release-{native}")),
        "release",
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let prompts = requests(&root, "session/prompt");
            if prompts.len() >= 2 {
                let text = prompts[1]["params"]["prompt"].to_string();
                assert!(text.contains("User message"));
                assert!(text.contains(first["message_id"].as_str().unwrap()));
                assert!(!text.contains("frozen controlled worker goal"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let user: Vec<calm_types::track_fs_dto::TaskPlannerMessage> =
        calm_truth::db::sqlite::planner_messages_for_attempt(
            stack.repo().sqlite_db().unwrap().read_pool(),
            &track,
            &task.id,
        )
        .await
        .unwrap();
    assert_eq!(user.len(), 1);
    assert_eq!(
        user[0].source,
        Some(calm_types::track_fs_dto::TaskMessageSource::User)
    );
    assert!(user[0].read_at_ms.is_none());
    let cancel = json!({"attempt_id":task.id});
    assert_eq!(
        user_request(
            &stack,
            &track,
            &task.key,
            "cancel",
            "ai:other",
            cancel.clone()
        )
        .await
        .0,
        axum::http::StatusCode::FORBIDDEN
    );
    assert!(
        user_request(&stack, &track, &task.key, "cancel", "user", cancel.clone())
            .await
            .0
            .is_success()
    );
    assert!(
        user_request(&stack, &track, &task.key, "cancel", "user", cancel)
            .await
            .0
            .is_success()
    );
    assert_eq!(held(&stack, &task).await, 1);
    settle(&root, &stack, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_cancel_reply_close_ack_retains_unknown_lease_and_shared_sibling() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track_a, planner_a) = declare(&stack, payload("unknown", "read_only")).await;
    let task_a = wait_task(&stack, &track_a, "unknown", TaskStatus::Running).await;
    let (track_b, planner_b) = declare(&stack, payload("sibling", "read_only")).await;
    let task_b = wait_task(&stack, &track_b, "sibling", TaskStatus::Running).await;
    let native_a = state(&stack, &task_a).await.native_session_id.unwrap();
    std::fs::write(
        root.path().join(format!("worker-drop-cancel-{native_a}")),
        "drop",
    )
    .unwrap();
    call(
        &stack,
        identity(&stack, &planner_a, CardRole::Planner).await,
        "neige_task_cancel",
        json!({"key":task_a.key,"message":"cancel with lost result"}),
    )
    .await
    .unwrap();
    wait_file(&root, &format!("worker-cancel-{native_a}")).await;
    std::fs::write(
        root.path().join(format!("worker-settle-{native_a}")),
        "discard response",
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(25), async {
        while state(&stack, &task_a).await.turn_phase
            != calm_types::acp_worker::WorkerTurnPhase::Unknown
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("unknown after convergence timeout");
    assert_eq!(held(&stack, &task_a).await, 1);
    assert!(!state(&stack, &task_a).await.cleanup_confirmed);
    let retained = stack
        .runtime(task_a.worker_card_id.as_deref().unwrap())
        .await;
    assert!(
        stack
            .state
            .operation_runtime
            .stop_worker_turn_for_test(&retained)
            .await
            .is_err(),
        "close ACK cannot make an unconfirmed prompt stop successful"
    );
    assert_eq!(held(&stack, &task_a).await, 1);
    assert_eq!(
        stack
            .repo()
            .task_get(&task_b.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(held(&stack, &task_b).await, 1);
    let prompts = requests(&root, "session/prompt");
    assert_eq!(
        prompts
            .iter()
            .filter(|p| p["params"]["sessionId"] == native_a)
            .count(),
        1,
        "unknown never replays goal"
    );
    cancel(&root, &stack, &planner_b, &task_b).await;
    wait_released(&stack, &task_a).await;
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_sampler_unavailable_refuses_without_releasing_authority() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("unsampled", "read_only")).await;
    let task = wait_task(&stack, &track, "unsampled", TaskStatus::Running).await;
    let cwd: String =
        sqlx::query_scalar("SELECT path FROM workspace_leases WHERE card_id=?1 AND state='held'")
            .bind(task.worker_card_id.as_deref())
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    let moved = format!("{cwd}-temporarily-unavailable");
    std::fs::rename(&cwd, &moved).unwrap();
    let worker = identity(
        &stack,
        task.worker_card_id.as_deref().unwrap(),
        CardRole::Worker,
    )
    .await;
    let refused = call(
        &stack,
        worker,
        "neige_task_done",
        json!({"attempt_id":task.id}),
    )
    .await
    .unwrap_err();
    std::fs::rename(&moved, &cwd).unwrap();
    assert!(refused.contains("worker-read-only-unsampled"), "{refused}");
    assert_eq!(held(&stack, &task).await, 1);
    assert_eq!(
        stack
            .repo()
            .task_get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_principal_cannot_write_dirty_diagnostic_or_cross_attempt_report() {
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = declare(&stack, payload("identity", "read_only")).await;
    let task = wait_task(&stack, &track, "identity", TaskStatus::Running).await;
    let worker = identity(
        &stack,
        task.worker_card_id.as_deref().unwrap(),
        CardRole::Worker,
    )
    .await;
    let mut stale = worker.clone();
    stale.session_id = "revoked-session".into();
    let cwd: String =
        sqlx::query_scalar("SELECT path FROM workspace_leases WHERE card_id=?1 AND state='held'")
            .bind(task.worker_card_id.as_deref())
            .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
            .await
            .unwrap();
    let dirty = std::path::Path::new(&cwd).join("identity-dirty");
    std::fs::write(&dirty, "changed").unwrap();
    let before = stack
        .repo()
        .task_get(&task.id)
        .await
        .unwrap()
        .unwrap()
        .status_detail;
    assert!(
        call(
            &stack,
            stale,
            "neige_task_done",
            json!({"attempt_id":task.id})
        )
        .await
        .is_err()
    );
    assert_eq!(
        stack
            .repo()
            .task_get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .status_detail,
        before
    );
    std::fs::remove_file(dirty).unwrap();
    for tool in ["neige_task_accept", "neige_task_reject"] {
        assert!(
            call(&stack, worker.clone(), tool, json!({"key":task.key}))
                .await
                .is_err()
        );
    }
    assert!(
        call(
            &stack,
            worker,
            "neige_task_done",
            json!({"attempt_id":"another-attempt"})
        )
        .await
        .is_err()
    );
    assert_eq!(held(&stack, &task).await, 1);
    cancel(&root, &stack, &planner, &task).await;
    stack.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_v1_validator_invalid_result_rolls_back_then_pass_and_blocking_settle() {
    for verdict in ["pass", "blocking"] {
        let root = Root::new("unused");
        let stack = boot(&root, "worker-hold").await;
        let mut declaration = payload("review", "read_only");
        declaration["result_contract"] = json!("review-v1");
        let (track, _) = declare(&stack, declaration).await;
        let task = wait_task(&stack, &track, "review", TaskStatus::Running).await;
        let worker = identity(
            &stack,
            task.worker_card_id.as_deref().unwrap(),
            CardRole::Worker,
        )
        .await;
        assert_eq!(
            stack
                .repo()
                .session_get(&worker.session_id.clone().into())
                .await
                .unwrap()
                .unwrap()
                .contract,
            calm_types::worker::WorkerContract::Validator
        );
        assert!(call(&stack,worker.clone(),"neige_task_done",json!({"attempt_id":task.id,"result":{"verdict":"pass","summary":"invalid","blocking_findings":[{"title":"bad","evidence":"contradiction"}]}})).await.is_err());
        assert_eq!(
            stack
                .repo()
                .task_get(&task.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running
        );
        assert_eq!(held(&stack, &task).await, 1);
        assert!(!state(&stack, &task).await.release_intent);
        let findings = if verdict == "pass" {
            json!([])
        } else {
            json!([{"title":"Controlled blocker","evidence":"The acceptance criterion is unmet"}])
        };
        call(&stack,worker,"neige_task_done",json!({"attempt_id":task.id,"result":{"verdict":verdict,"summary":"Controlled review","blocking_findings":findings}})).await.unwrap();
        assert_eq!(held(&stack, &task).await, 1);
        settle(&root, &stack, &task).await;
        stack.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reboot_unsettled_and_reported_workers_never_replay_business_goal() {
    for reported in [false, true] {
        let root = Root::new("unused");
        let stack = boot(&root, "worker-hold").await;
        let (track, _) = declare(&stack, payload("restart", "read_only")).await;
        let task = wait_task(&stack, &track, "restart", TaskStatus::Running).await;
        if reported {
            let worker = identity(
                &stack,
                task.worker_card_id.as_deref().unwrap(),
                CardRole::Worker,
            )
            .await;
            call(
                &stack,
                worker,
                "neige_task_done",
                json!({"attempt_id":task.id,"result":{"preserved":true}}),
            )
            .await
            .unwrap();
        }
        assert_eq!(held(&stack, &task).await, 1);
        stack.shutdown().await;
        let recovered = boot(&root, "worker-hold").await;
        let status = if reported {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        let task = wait_task(&recovered, &track, "restart", status).await;
        wait_released(&recovered, &task).await;
        assert_eq!(
            requests(&root, "session/prompt").len(),
            1,
            "startup cannot replay the goal"
        );
        let settled = state(&recovered, &task).await;
        assert!(settled.cleanup_confirmed);
        assert!(settled.goal_submitted);
        recovered.shutdown().await;
        if reported {
            let again = boot(&root, "worker-hold").await;
            let finalized = wait_task(&again, &track, "restart", TaskStatus::Done).await;
            assert_eq!(held(&again, &finalized).await, 0);
            let reports: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='task.report_settled' AND json_extract(payload,'$.idempotency_key')=?1")
                .bind(&task.id).fetch_one(again.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
            let releases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='workspace.released' AND json_extract(payload,'$.card_id')=?1")
                .bind(task.worker_card_id.as_deref().unwrap()).fetch_one(again.repo().sqlite_db().unwrap().read_pool()).await.unwrap();
            assert_eq!(
                (reports, releases),
                (1, 1),
                "reboot cannot repeat an already finalized report or release"
            );
            assert_eq!(requests(&root, "session/prompt").len(), 1);
            again.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepared_recovery_resumes_same_native_reconfirms_defaults_and_fences_old_generation() {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    let root = Root::new("unused");
    let stack = boot(&root, "worker-hold").await;
    let (track, planner) = stack
        .create_canonical_track_with(json!({"planner_provider":"antigravity"}))
        .await;
    let before_goal = PausePoint::default();
    install_pause_for_test(
        "acp-worker-before-initial-round",
        &track,
        before_goal.clone(),
    );
    declare_on(
        &stack,
        &track,
        &planner,
        payload("prepared-restart", "read_only"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), before_goal.entered.notified())
        .await
        .unwrap();
    let task = stack
        .repo()
        .task_current_get(&track, "prepared-restart")
        .await
        .unwrap()
        .unwrap();
    let prepared = state(&stack, &task).await;
    let native = prepared.native_session_id.clone().unwrap();
    assert!(!prepared.goal_submitted);
    assert_eq!(
        prepared.turn_phase,
        calm_types::acp_worker::WorkerTurnPhase::Prepared
    );
    assert_eq!(prepared.effective_model.as_deref(), Some("fixture/model-a"));
    assert_eq!(prepared.effective_effort.as_deref(), Some("normal"));
    assert!(requests(&root, "session/prompt").is_empty());
    assert_eq!(held(&stack, &task).await, 1);
    let queued = call(
        &stack,
        identity(&stack, &planner, CardRole::Planner).await,
        "neige_terminal_input",
        json!({"attempt_id":task.id,"idempotency_key":"startup-message",
            "action":{"type":"message","text":"Inspect the durable startup message"}}),
    )
    .await
    .unwrap();
    assert_eq!(queued["transport"], "native_session");
    assert_eq!(state(&stack, &task).await.notified_through_seq, 0);
    assert!(
        requests(&root, "session/prompt").is_empty(),
        "startup notification cannot submit a second goal"
    );
    // Leave only the captured pre-send future alive. The production boot sweep must stop
    // its original process, then the original operation resumes its durable Prepared row.
    stack.shutdown().await;
    let recovered = boot(&root, "worker-resume-defaults-changed").await;
    let task = wait_task(&recovered, &track, "prepared-restart", TaskStatus::Running).await;
    let resumed = state(&recovered, &task).await;
    assert_eq!(resumed.native_session_id.as_deref(), Some(native.as_str()));
    assert_eq!(resumed.effective_model.as_deref(), Some("fixture/model-a"));
    assert_eq!(resumed.effective_effort.as_deref(), Some("normal"));
    assert_eq!(
        resumed.selection_source, prepared.selection_source,
        "reconfirmed defaults remain provider defaults"
    );
    assert_ne!(
        resumed.credential_generation,
        prepared.credential_generation
    );
    let private_generation = resumed.credential_generation.as_deref().unwrap();
    let card = task.worker_card_id.as_deref().unwrap();
    for path in [
        format!("/api/tracks/{track}/tasks/{}/attempts", task.key),
        format!("/api/tracks/{track}/files/cat?path=cards%2F{card}%2Fruntime.json"),
        format!("/api/tracks/{track}/files/cat?path=cards%2F{card}%2F.payload.json"),
        format!("/api/tracks/{track}/files/cat?path=cards%2F{card}%2Fconversation.md"),
    ] {
        let (status, public) = recovered.send("GET", &path, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{path}: {public}");
        let public = public.to_string();
        assert!(
            !public.contains(private_generation) && !public.contains("credential_generation"),
            "internal grant generation leaked through {path}"
        );
    }
    // Boot also probes the model catalog with an unprivileged, empty-MCP session.
    // Count the real Worker attachment, rather than unrelated provider sessions.
    let new_sessions = requests(&root, "session/new");
    let worker_attachment = |request: &Value| {
        request["params"]["mcpServers"]
            .as_array()
            .is_some_and(|servers| !servers.is_empty())
    };
    let ownership: Vec<_> = new_sessions
        .iter()
        .map(|request| {
            json!({"cwd":request["params"]["cwd"],"mcp_server_names":request["params"]["mcpServers"].as_array().unwrap().iter().map(|server|server["name"].clone()).collect::<Vec<_>>()})
        })
        .collect();
    eprintln!("credential-free session/new ownership: {ownership:?}");
    let new_attachments: Vec<_> = new_sessions
        .iter()
        .filter(|r| worker_attachment(r))
        .collect();
    assert_eq!(new_attachments.len(), 1);
    let server_name = new_attachments[0]["params"]["mcpServers"][0]["name"]
        .as_str()
        .unwrap();
    assert!(server_name.starts_with("neige-"));
    assert_eq!(server_name.len(), 18);
    assert!(
        server_name[6..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    // This peer advertises resume; the common setup prefers it over load.
    let resumed_attachments: Vec<_> = requests(&root, "session/resume")
        .into_iter()
        .filter(worker_attachment)
        .collect();
    assert_eq!(resumed_attachments.len(), 1);
    assert_eq!(resumed_attachments[0]["params"]["sessionId"], native);
    assert_eq!(
        resumed_attachments[0]["params"]["mcpServers"][0]["name"], server_name,
        "resuming the same worker must replace its existing MCP registration"
    );
    let for_native = |method| {
        requests(&root, method)
            .into_iter()
            .filter(|request| request["params"]["sessionId"] == native)
            .collect::<Vec<_>>()
    };
    let initial_prompts = for_native("session/prompt");
    assert_eq!(initial_prompts.len(), 1);
    let first_text = initial_prompts[0]["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap();
    assert!(first_text.contains("Perform the frozen controlled worker goal"));
    assert!(first_text.contains(queued["message_id"].as_str().unwrap()));
    tokio::time::timeout(Duration::from_secs(15), async {
        while state(&recovered, &task).await.notified_through_seq != 1 {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .unwrap();
    let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM task_planner_messages WHERE attempt_id=?1 AND read_at_ms IS NULL",
    )
    .bind(&task.id)
    .fetch_one(recovered.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap();
    assert_eq!(unread, 1, "startup notification is not a message read");
    let confirmed = for_native("session/set_config_option");
    assert_eq!(
        confirmed
            .iter()
            .map(|request| (
                request["params"]["configId"].clone(),
                request["params"]["value"].clone()
            ))
            .collect::<Vec<_>>(),
        vec![
            (json!("declared-model-key"), json!("fixture/model-a")),
            (json!("declared-effort-key"), json!("normal"))
        ]
    );
    let successor = recovered
        .runtime(task.worker_card_id.as_deref().unwrap())
        .await;
    let old_ended = PausePoint::default();
    install_pause_for_test("acp-worker-driver-error-settled", &track, old_ended.clone());
    before_goal.release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), old_ended.entered.notified())
        .await
        .unwrap();
    let after_old = recovered
        .runtime(task.worker_card_id.as_deref().unwrap())
        .await;
    assert_eq!(after_old.active_turn_id, successor.active_turn_id);
    assert_eq!(
        state(&recovered, &task).await.credential_generation,
        resumed.credential_generation
    );
    assert_eq!(
        recovered
            .repo()
            .task_get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(held(&recovered, &task).await, 1);
    assert!(for_native("session/cancel").is_empty());
    assert_eq!(
        for_native("session/prompt").len(),
        1,
        "neither recovery nor old driver may replay the business goal"
    );
    old_ended.release.notify_one();
    cancel(&root, &recovered, &planner, &task).await;
    recovered.shutdown().await;
}
