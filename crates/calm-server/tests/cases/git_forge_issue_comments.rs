//! Planner issue communication through the real MCP socket and operation runtime.
use super::*;
use crate::support::gh_shim::write_gh_shim;

const COMMENT: &str = "plugin.dev.neige.git-forge_gh.issue.comment";
const COMMENTS: &str = "plugin.dev.neige.git-forge_gh.issue.comments";

async fn call(fx: &Fixture, token: &str, thread: &str, id: i64, tool: &str, args: Value) -> Value {
    call_tool_as(fx, token, thread, id, tool, args).await
}

async fn wait_succeeded(fx: &Fixture, op_id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
            .bind(op_id)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
        if phase == "succeeded" {
            return;
        }
        assert!(
            !["failed", "cancelled"].contains(&phase.as_str()),
            "unexpected phase: {phase}"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "operation remained {phase}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn planner_issue_comments_are_recorded_deduplicated_and_refreshable() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let shim = short_tempdir("comments-gh").unwrap();
    write_gh_shim(shim.path());
    let _path = EnvGuard::set(
        "PATH",
        format!(
            "{}:{}",
            shim.path().display(),
            std::env::var("PATH").unwrap()
        ),
    );
    let results = short_tempdir("comments-results").unwrap();
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results.path());
    let fx = boot_fixture().await;
    let track = token_track(&fx).await;
    let (token, thread) = planner_caller(&fx, &track.track_id).await;
    let repo = track.checkout.to_string_lossy().to_string();
    let state = PathBuf::from(format!("{repo}.shimstate"));

    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &token).await;
    send_frame(&mut wr, tools_list_frame(2, &thread)).await;
    let list = recv_frame(&mut rd).await;
    for tool in [COMMENT, COMMENTS] {
        let descriptor = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == tool)
            .expect("Planner discovers issue tools");
        assert_eq!(descriptor["annotations"]["readOnlyHint"], tool == COMMENTS);
    }
    let empty = call(
        &fx,
        &token,
        &thread,
        3,
        COMMENTS,
        json!({"repo":repo,"issue":42}),
    )
    .await;
    assert_eq!(empty["result"]["isError"], false, "{empty}");
    let empty_text = empty["result"]["structuredContent"]["result"]["stdout"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(empty_text).unwrap(),
        json!([])
    );

    // Hold the external command after publication: the first reply must remain pending.
    std::fs::write(state.join("block_issue_comment"), "").unwrap();
    let args = json!({"repo":repo,"issue":42,"body":"Planning done\n\"quotes\" $(touch forbidden)","idem":"plan-1"});
    let pending = call(&fx, &token, &thread, 4, COMMENT, args.clone()).await;
    assert_eq!(pending["result"]["isError"], false, "{pending}");
    assert_eq!(pending["result"]["structuredContent"]["status"], "pending");
    let op = pending["result"]["structuredContent"]["op_id"]
        .as_str()
        .unwrap();
    let stored: (String, String, String) =
        sqlx::query_as("SELECT kind, phase, payload_json FROM operations WHERE id = ?1")
            .bind(op)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    assert_eq!(stored.0, FORGE_ACTION_KIND);
    assert_eq!(stored.1, "parked");
    let payload: Value = serde_json::from_str(&stored.2).unwrap();
    assert_eq!(payload["track_id"], track.track_id);
    assert_eq!(
        payload["cwd_lease"],
        track.worktree.to_string_lossy().as_ref()
    );
    assert!(payload["probe"].is_object());
    assert!(!track.worktree.join("forbidden").exists());
    std::fs::write(state.join("release_issue_comment"), "").unwrap();
    wait_succeeded(&fx, op).await;
    let again = call(&fx, &token, &thread, 5, COMMENT, args.clone()).await;
    assert_eq!(again["result"]["isError"], false, "{again}");
    assert_eq!(again["result"]["structuredContent"]["op_id"], op);
    assert_eq!(
        std::fs::read_to_string(state.join("issue_comment_count"))
            .unwrap()
            .trim(),
        "1"
    );

    let mut changed = args.clone();
    changed["body"] = json!("Changed body on the same logical request");
    let conflict = call(&fx, &token, &thread, 6, COMMENT, changed.clone()).await;
    assert_eq!(conflict["result"]["isError"], true, "{conflict}");
    assert_eq!(
        std::fs::read_to_string(state.join("issue_comment_count"))
            .unwrap()
            .trim(),
        "1"
    );
    changed["idem"] = json!("plan-2");
    let second = call(&fx, &token, &thread, 7, COMMENT, changed).await;
    assert_eq!(second["result"]["isError"], false, "{second}");
    wait_succeeded(
        &fx,
        second["result"]["structuredContent"]["op_id"]
            .as_str()
            .unwrap(),
    )
    .await;
    let fresh = call(
        &fx,
        &token,
        &thread,
        8,
        COMMENTS,
        json!({"repo":repo,"issue":42,"attempt":1}),
    )
    .await;
    let text = fresh["result"]["structuredContent"]["result"]["stdout"]
        .as_str()
        .unwrap();
    let discussion: Value = serde_json::from_str(text).unwrap();
    assert_eq!(discussion.as_array().unwrap().len(), 2);
    assert_eq!(discussion[0]["body"], payload["argv"][7]);
    assert_eq!(
        std::fs::read_to_string(state.join("issue_comment_count"))
            .unwrap()
            .trim(),
        "2"
    );
    let view_tool = "plugin.dev.neige.git-forge_gh.issue.view";
    let original_body = call(
        &fx,
        &token,
        &thread,
        9,
        view_tool,
        json!({"repo":repo,"issue":42}),
    )
    .await;
    assert_eq!(original_body["result"]["isError"], false);
    std::fs::write(state.join("issues/42.body"), "New requirements").unwrap();
    let fresh_body = call(
        &fx,
        &token,
        &thread,
        10,
        view_tool,
        json!({"repo":repo,"issue":42,"attempt":1}),
    )
    .await;
    assert_eq!(
        fresh_body["result"]["structuredContent"]["result"]["stdout"],
        "New requirements"
    );

    fx.plugin_host.disable(PLUGIN_ID).await.unwrap();
    let denied = call(&fx, &token, &thread, 11, COMMENT, args).await;
    assert_eq!(denied["error"]["code"], -32002, "{denied}");
    assert_eq!(
        std::fs::read_to_string(state.join("issue_comment_count"))
            .unwrap()
            .trim(),
        "2"
    );
}
