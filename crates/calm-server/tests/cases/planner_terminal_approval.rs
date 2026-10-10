//! Actual adapter-to-provider requests, using the existing protocol fixture.
use super::*;

pub(super) fn start_payload(
    track: &Track,
    card_id: &str,
    profile: HarnessProfile,
    force_new_thread: bool,
    goal: Option<String>,
) -> PlannerHarnessStartOperationPayload {
    PlannerHarnessStartOperationPayload {
        actor: calm_server::ids::ActorId::User,
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(card_id.to_owned()),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.path.clone(),
        goal,
        reset_harness_items: false,
        force_new_thread,
        profile,
        create_card: None,
        opening_briefing: None,
        first_message: None,
        create_request_sha256: None,
    }
}

#[tokio::test]
async fn assistant_thread_receives_only_the_terminal_control_and_input_policy() {
    let _guard = ENV_LOCK.lock().await;
    let tmp = TempDir::new().unwrap();
    let capture = tmp.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");
    let (state, repo, roles) = state_with_live_daemon(&tmp).await;
    let track = seed_track(&repo).await;
    let card_id = new_id();
    seed_assistant_card(&repo, &roles, &track, &card_id).await;
    let payload = serde_json::to_value(start_payload(
        &track,
        &card_id,
        HarnessProfile::Assistant,
        true,
        None,
    ))
    .unwrap();
    let operation = state
        .operation_runtime
        .submit("planner-harness-start", key(), payload)
        .await
        .unwrap();
    assert!(matches!(
        wait_op(&state, &operation).await,
        OperationOutcome::Succeeded { .. }
    ));
    let rows = wait_for_requests(&capture, 2).await;
    let start = rows.iter().find(|r| r["method"] == "thread/start").unwrap();
    assert_eq!(start["params"]["approvalPolicy"], "never");
    assert!(
        start
            .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
            .is_some()
    );
    // #2492: an Assistant drives existing task workers but opens no card, so it is delegated
    // exactly the two terminal writes it may call, never `neige_terminal_open`.
    assert_eq!(
        start.pointer("/params/config/mcp_servers/neige/tools"),
        Some(&json!({
            "neige_terminal_control": {"approval_mode":"approve"},
            "neige_terminal_input": {"approval_mode":"approve"}
        })),
        "an Assistant must delegate only the Terminal writes its role may call"
    );
    assert!(
        start
            .pointer("/params/config/mcp_servers/neige/default_tools_approval_mode")
            .is_none()
    );
}
