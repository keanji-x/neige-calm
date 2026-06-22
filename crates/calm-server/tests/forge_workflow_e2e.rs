#![cfg(unix)]

mod support;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_mcp_token_set_tx, card_with_codex_create_tx, session_bind_attribution_tx,
    session_mark_wave_root_tx, session_mcp_token_set_tx, session_projection_active_for_card_tx,
    session_start_runtime_tx,
};
use calm_server::db::write_with_actor_events_typed;
use calm_server::event::{ChannelVerdict, Event, EventBus, EventScope, RatifyDecision};
use calm_server::forge_trust::trusted_forge_plugin;
use calm_server::harness::{
    HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, spawn_recovered_harness,
};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::tools::review::{TOOL_RATIFY_REQUEST, TOOL_REVIEW_ROUND};
use calm_server::mcp_server::{
    AppContext, McpServer, ToolCallIdentity, ToolRegistry, build_default_registry,
};
use calm_server::model::{
    CardRole, NewCard, NewCove, NewPlugin, NewWave, WaveLifecycle, new_id, now_ms,
};
use calm_server::operation::forge_action_adapter::{
    FORGE_ACTION_KIND, ForgeActionAdapter, ForgeActionPayload, ProbeSpec,
};
use calm_server::operation::{
    OperationCompletionBus, OperationKey, OperationOutcome, OperationResult, OperationRuntime,
    ProviderAdapter, RecoveryItem, SpawnArtifacts, SpawnCtx, SqlxOperationRepo,
};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_types::worker::WorkerSessionId;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use support::mcp::{
    connect, handshake, recv_frame, send_frame, tools_call_frame, tools_list_frame,
};
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep, timeout};
use tower::ServiceExt;

const FORGE_BIN: &str = env!("CARGO_BIN_EXE_git-forge");
const PLUGIN_ID: &str = "dev.neige.git-forge";
const COMMIT_TOOL: &str = "plugin.dev.neige.git-forge_git.commit";
const PR_LIST_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.list";
const PR_CREATE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.create";
const PR_DIFF_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.diff";
const PR_CHECKS_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.checks";
const PR_MERGE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.merge";
const ISSUE_VIEW_TOOL: &str = "plugin.dev.neige.git-forge_gh.issue.view";
const ISSUE_CLOSE_TOOL: &str = "plugin.dev.neige.git-forge_gh.issue.close";
const RECOVERY_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const SPEC_SESSION_ID: &str = "forge-workflow-spec-session";

static FORGE_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
const WORKFLOW_ID: &str = "issue-development";

struct Fixture {
    _server: Arc<McpServer>,
    plugin_host: Arc<PluginHost>,
    repo: Arc<SqlxRepo>,
    events: EventBus,
    write: WriteContext,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    review_ctx: Arc<AppContext>,
    review_registry: Arc<ToolRegistry>,
    socket_path: PathBuf,
    raw_token: String,
    thread_id: String,
    wave_id: String,
    cove_id: String,
    spec_card_id: String,
    worker_card_id: String,
    lease_id: String,
    lease_abs: PathBuf,
    wave_cwd: PathBuf,
    origin_repo: PathBuf,
    _runtime: Arc<OperationRuntime>,
    _socket_tmp: TempDir,
    _tmp: TempDir,
}

struct Caller {
    card_id: String,
    raw_token: String,
    thread_id: String,
    wave_id: String,
    lease_id: String,
    lease_abs: PathBuf,
}

#[derive(Clone, Debug)]
struct EventRow {
    id: i64,
    scope_kind: String,
    scope_wave: Option<String>,
    scope_card: Option<String>,
    payload: Value,
}

type RawEventRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

#[tokio::test]
async fn git_forge_workflow_registers_and_wave_create_binds() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);

    let fx = boot_fixture().await;
    let registered = wait_for_event_count(&fx.repo, "workflow.registered", 1).await;
    assert_eq!(registered[0].payload["pluginId"], PLUGIN_ID);
    assert_eq!(registered[0].payload["workflowId"], WORKFLOW_ID);

    let app = wave_router_for_fixture(&fx);
    let wave_dir = short_tempdir("wf").expect("workflow wave cwd");
    let (status, body) = post_wave(
        app.clone(),
        json!({
            "cove_id": fx.cove_id,
            "title": "bound workflow wave",
            "cwd": wave_dir.path().display().to_string(),
            "attach_folder": true,
            "workflow_id": WORKFLOW_ID,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    assert_eq!(body["workflow_id"], WORKFLOW_ID);
    let wave_id = body["id"].as_str().expect("created wave id");
    let stored: Option<String> = sqlx::query_scalar("SELECT workflow_id FROM waves WHERE id = ?1")
        .bind(wave_id)
        .fetch_one(fx.repo.pool())
        .await
        .expect("select workflow_id");
    assert_eq!(stored.as_deref(), Some(WORKFLOW_ID));

    let missing_dir = short_tempdir("wf-missing").expect("missing workflow cwd");
    let (status, _body) = post_wave(
        app.clone(),
        json!({
            "cove_id": fx.cove_id,
            "title": "missing workflow wave",
            "cwd": missing_dir.path().display().to_string(),
            "attach_folder": true,
            "workflow_id": "missing-workflow",
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    drop(trusted);
    let _untrusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", "other.plugin");
    let untrusted_dir = short_tempdir("wf-untrusted").expect("untrusted workflow cwd");
    let (status, _body) = post_wave(
        app,
        json!({
            "cove_id": fx.cove_id,
            "title": "untrusted workflow wave",
            "cwd": untrusted_dir.path().display().to_string(),
            "attach_folder": true,
            "workflow_id": WORKFLOW_ID,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn git_forge_happy_path_persists_ordered_workflow_events() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let path_dir = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(path_dir.path());
    let path_value = prepend_to_path(path_dir.path());
    let results_dir = short_tempdir("r").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let _path = EnvGuard::set("PATH", path_value);

    let fx = boot_fixture().await;
    assert_tools_are_discoverable(&fx).await;

    let repo_arg = fx.origin_repo.display().to_string();
    let base = "main";
    let head = "slice-810-e2e";

    let issue_view_resp = call_tool(
        &fx,
        9,
        ISSUE_VIEW_TOOL,
        json!({ "repo": repo_arg, "issue": 810 }),
    )
    .await;
    assert_tool_succeeded(&issue_view_resp, "gh.issue.view");
    let issue_view_op_id = op_id_from_response(&issue_view_resp);
    wait_for_operation_phase(&fx.repo, &issue_view_op_id, "succeeded").await;
    let issue_body = "# Issue 810\n\nFake issue body for issue-development ingestion.\n";
    assert_eq!(
        issue_view_resp["result"]["structuredContent"]["result"]["stdout"], issue_body,
        "gh.issue.view must return the issue body inline"
    );
    let issue_read_rows = wait_for_event_count(&fx.repo, "forge.issue.read", 1).await;
    let issue_read = issue_read_rows[0].clone();
    assert_wave_event(&issue_read, &fx.wave_id);
    assert_eq!(issue_read.payload["issue_number"], json!(810));
    let issue_artifact_path = issue_read.payload["artifact_path"]
        .as_str()
        .expect("issue read artifact_path")
        .to_string();
    assert!(
        !issue_artifact_path.is_empty(),
        "issue read artifact_path must be non-empty"
    );
    let issue_artifact =
        std::fs::read_to_string(&issue_artifact_path).expect("read issue body artifact");
    assert_eq!(
        issue_artifact, issue_body,
        "issue read artifact must contain the shim issue body"
    );

    let scan_resp = call_tool(
        &fx,
        10,
        PR_LIST_TOOL,
        json!({ "repo": repo_arg, "base": base, "head": head }),
    )
    .await;
    assert_tool_succeeded(&scan_resp, "gh.pr.list");
    let scan = wait_for_event_count(&fx.repo, "forge.scan.completed", 1).await;
    assert_wave_event(&scan[0], &fx.wave_id);
    assert_eq!(scan[0].payload["overlapping_prs"], json!([]));

    run_git(&fx.lease_abs, ["checkout", "-b", head]);
    stage_git_change(&fx.lease_abs, "feature.txt", "hello from e2e\n");
    let commit_resp = call_tool(
        &fx,
        11,
        COMMIT_TOOL,
        json!({ "message": "e2e feature", "idem": "slice-810-e2e-commit" }),
    )
    .await;
    assert_tool_succeeded(&commit_resp, "git.commit");
    let head_sha = run_git_capture(&fx.lease_abs, ["rev-parse", "HEAD"]);
    let base_sha = run_git_capture(&fx.lease_abs, ["rev-parse", "origin/main"]);
    run_git(&fx.lease_abs, ["push", "-u", "origin", head]);

    let create_resp = call_tool(
        &fx,
        12,
        PR_CREATE_TOOL,
        json!({
            "repo": repo_arg,
            "head": head,
            "base": base,
            "title": "E2E feature",
            "body": "Created by forge workflow E2E"
        }),
    )
    .await;
    assert_tool_succeeded(&create_resp, "gh.pr.create");
    let opened_rows = wait_for_event_count(&fx.repo, "forge.pr.opened", 1).await;
    let opened = opened_rows[0].clone();
    assert_wave_event(&opened, &fx.wave_id);
    assert_eq!(opened.payload["pr_number"], 1);
    assert_eq!(opened.payload["head_sha"], head_sha);
    let pr_number = opened.payload["pr_number"].as_u64().expect("pr number");

    let diff_resp = call_tool(
        &fx,
        13,
        PR_DIFF_TOOL,
        json!({
            "repo": repo_arg,
            "pr": pr_number,
            "base_sha": base_sha,
            "head_sha": head_sha
        }),
    )
    .await;
    assert_tool_succeeded(&diff_resp, "gh.pr.diff");
    assert!(
        diff_resp["result"]["structuredContent"]["result"]
            .get("stdout")
            .is_none(),
        "gh.pr.diff must not inline patch stdout"
    );
    let diff_rows = wait_for_event_count(&fx.repo, "forge.pr.diff.read", 1).await;
    let diff = diff_rows[0].clone();
    assert_wave_event(&diff, &fx.wave_id);
    assert_eq!(diff.payload["pr_number"], pr_number);
    assert_eq!(diff.payload["base_sha"], base_sha);
    assert_eq!(diff.payload["head_sha"], head_sha);
    let artifact_path = diff.payload["artifact_path"]
        .as_str()
        .expect("artifact_path")
        .to_string();
    assert!(
        !artifact_path.is_empty(),
        "diff artifact_path must be non-empty"
    );
    let artifact = std::fs::read_to_string(&artifact_path).expect("read diff artifact");
    assert!(
        artifact.contains("diff --git") && artifact.contains("feature.txt"),
        "diff artifact must contain the shim patch body: {artifact}"
    );

    let checks_resp = call_tool(
        &fx,
        14,
        PR_CHECKS_TOOL,
        json!({ "repo": repo_arg, "pr": pr_number }),
    )
    .await;
    assert_tool_succeeded(&checks_resp, "gh.pr.checks");
    let checks_rows = wait_for_event_count(&fx.repo, "forge.pr.checks", 1).await;
    let checks = checks_rows[0].clone();
    assert_wave_event(&checks, &fx.wave_id);
    assert_eq!(checks.payload["pr_number"], pr_number);
    assert_eq!(checks.payload["conclusion"], "success");

    let merge_resp = call_tool(
        &fx,
        15,
        PR_MERGE_TOOL,
        json!({
            "repo": repo_arg,
            "pr": pr_number,
            "phase": "impl",
            "slice_id": "810"
        }),
    )
    .await;
    assert_tool_succeeded(&merge_resp, "gh.pr.merge");
    let merged_rows = wait_for_event_count(&fx.repo, "forge.pr.merged", 1).await;
    let merged = merged_rows[0].clone();
    assert_wave_event(&merged, &fx.wave_id);
    assert_eq!(merged.payload["head_sha"], head_sha);
    assert_eq!(merged.payload["subject"]["pr_number"], pr_number);
    let merge_sha = merged.payload["merge_sha"]
        .as_str()
        .expect("merge_sha string");
    assert_eq!(merge_sha.len(), 40, "merge sha should be a git-shaped oid");

    let issue_resp = call_tool(
        &fx,
        16,
        ISSUE_CLOSE_TOOL,
        json!({ "repo": repo_arg, "issue": 810 }),
    )
    .await;
    assert_tool_succeeded(&issue_resp, "gh.issue.close");
    let issue_rows = wait_for_event_count(&fx.repo, "forge.issue.closed", 1).await;
    let issue_closed = issue_rows[0].clone();
    assert_wave_event(&issue_closed, &fx.wave_id);
    assert_eq!(issue_closed.payload["issue_number"], 810);

    transition_wave_to_done(&fx).await;
    let done = wait_for_event_matching(&fx.repo, "wave.lifecycle_changed", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id) && row.payload["to"] == "done"
    })
    .await;
    assert_eq!(done.payload["from"], "reviewing");
    assert_eq!(done.payload["to"], "done");

    assert!(opened.id < diff.id, "PR opened must precede diff read");
    assert!(
        checks.id < merged.id,
        "successful checks must precede merge"
    );
    assert!(
        merged.id < issue_closed.id,
        "merge must precede issue close"
    );
    assert!(
        issue_closed.id < done.id,
        "issue close must precede done lifecycle transition"
    );

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn git_forge_merge_crash_recovers_once_via_probe() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let path_dir = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(path_dir.path());
    let path_value = prepend_to_path(path_dir.path());
    let results_dir = short_tempdir("r").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let _path = EnvGuard::set("PATH", path_value);

    let fx = boot_fixture().await;
    let repo_arg = fx.origin_repo.display().to_string();
    let base = "main";
    let head = "slice-810-e2e-merge-crash";

    run_git(&fx.lease_abs, ["checkout", "-b", head]);
    stage_git_change(&fx.lease_abs, "merge-crash.txt", "merge crash e2e\n");
    let commit_resp = call_tool(
        &fx,
        20,
        COMMIT_TOOL,
        json!({ "message": "merge crash e2e", "idem": "slice-810-e2e-merge-crash-commit" }),
    )
    .await;
    assert_tool_succeeded(&commit_resp, "git.commit");
    let head_sha = run_git_capture(&fx.lease_abs, ["rev-parse", "HEAD"]);
    run_git(&fx.lease_abs, ["push", "-u", "origin", head]);

    let create_resp = call_tool(
        &fx,
        21,
        PR_CREATE_TOOL,
        json!({
            "repo": repo_arg,
            "head": head,
            "base": base,
            "title": "Merge crash E2E",
            "body": "Created by forge merge crash E2E"
        }),
    )
    .await;
    assert_tool_succeeded(&create_resp, "gh.pr.create");
    let opened_rows = wait_for_event_count(&fx.repo, "forge.pr.opened", 1).await;
    let pr_number = opened_rows[0].payload["pr_number"]
        .as_u64()
        .expect("pr number");

    let state = shim_state_dir(&fx.origin_repo);
    let block = ShimBlock::new(&state, "pr_merge");
    let merge_resp = call_tool(
        &fx,
        22,
        PR_MERGE_TOOL,
        json!({
            "repo": repo_arg,
            "pr": pr_number,
            "phase": "impl",
            "slice_id": "810"
        }),
    )
    .await;
    assert_tool_succeeded(&merge_resp, "gh.pr.merge");
    let op_id = op_id_from_response(&merge_resp);
    wait_for_counter(&state.join("pr_merge_count"), 1).await;
    wait_for_operation_phase(&fx.repo, &op_id, "parked").await;
    let result_path = operation_result_path(&fx.repo, &op_id).await;
    assert_result_files_absent(&result_path);
    assert_eq!(shim_counter(&state.join("pr_merge_count")), 1);
    assert_eq!(workspace_lease_state(&fx.repo, &fx.lease_id).await, "held");

    mark_parked_artifacts_dead(&fx.repo, &op_id).await;
    mark_workspace_lease_stale_for_boot(&fx.repo, &fx.lease_id).await;
    let recovery = boot_recovery_runtime(&fx).await;
    let plan = recovery.recover_on_boot().await.expect("recover on boot");
    assert!(
        plan.items
            .iter()
            .any(|item| matches!(item, RecoveryItem::VerifyParked { op_id: item_op_id } if item_op_id == &op_id)),
        "merge crash should recover through parked verification: {:?}",
        plan.items
    );
    recovery.apply_recovery(plan).await.expect("apply recovery");
    let result = wait_for_recovery_result(&recovery, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "merge recovery should succeed: {:?}",
        result.outcome
    );
    assert_eq!(operation_phase(&fx.repo, &op_id).await, "succeeded");

    let merged_rows = wait_for_event_count(&fx.repo, "forge.pr.merged", 1).await;
    let merged = merged_rows[0].clone();
    assert_wave_event(&merged, &fx.wave_id);
    assert_eq!(merged.payload["head_sha"], head_sha);
    assert_eq!(merged.payload["subject"]["pr_number"], pr_number);
    let merge_sha = merged.payload["merge_sha"]
        .as_str()
        .expect("merge_sha string");
    assert_eq!(merge_sha.len(), 40, "merge sha should be a git-shaped oid");
    assert_eq!(shim_counter(&state.join("pr_merge_count")), 1);
    assert_eq!(
        workspace_lease_state(&fx.repo, &fx.lease_id).await,
        "released"
    );

    block.release();
    wait_for_result_code_file(&result_path).await;
    assert_event_count_stays(&fx.repo, "forge.pr.merged", 1).await;
    assert_eq!(shim_counter(&state.join("pr_merge_count")), 1);

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn git_forge_never_ran_parked_merge_recovers_not_landed_via_probe() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let path_dir = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(path_dir.path());
    let path_value = prepend_to_path(path_dir.path());
    let results_dir = short_tempdir("r").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let _path = EnvGuard::set("PATH", path_value);

    let fx = boot_fixture().await;
    let repo_arg = fx.origin_repo.display().to_string();
    let base = "main";
    let head = "slice-810-e2e-merge-never-ran";

    run_git(&fx.lease_abs, ["checkout", "-b", head]);
    stage_git_change(
        &fx.lease_abs,
        "merge-never-ran.txt",
        "merge never ran e2e\n",
    );
    let commit_resp = call_tool(
        &fx,
        24,
        COMMIT_TOOL,
        json!({ "message": "merge never ran e2e", "idem": "slice-810-e2e-merge-never-ran-commit" }),
    )
    .await;
    assert_tool_succeeded(&commit_resp, "git.commit");
    run_git(&fx.lease_abs, ["push", "-u", "origin", head]);

    let create_resp = call_tool(
        &fx,
        25,
        PR_CREATE_TOOL,
        json!({
            "repo": repo_arg,
            "head": head,
            "base": base,
            "title": "Merge never ran E2E",
            "body": "Created by forge merge never-ran E2E"
        }),
    )
    .await;
    assert_tool_succeeded(&create_resp, "gh.pr.create");
    let opened_rows = wait_for_event_count(&fx.repo, "forge.pr.opened", 1).await;
    let pr_number = opened_rows[0].payload["pr_number"]
        .as_u64()
        .expect("pr number");
    let head_sha = opened_rows[0].payload["head_sha"]
        .as_str()
        .expect("head sha")
        .to_string();
    let state = shim_state_dir(&fx.origin_repo);
    assert!(!pr_is_merged(&state, pr_number), "PR must start open");

    let result_path = results_dir.path().join("merge-never-ran.result");
    let payload = ForgeActionPayload {
        wave_id: fx.wave_id.clone(),
        card_id: "card-1".into(),
        subject: Some(
            serde_json::from_value(json!({
                "phase": "impl",
                "slice_id": "810",
                "pr_number": pr_number
            }))
            .expect("merge subject"),
        ),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()],
        idem_key: format!("gh.pr.merge:{repo_arg}:{pr_number}"),
        event_spec: Some(
            serde_json::from_value(json!({
                "event_kind": "forge.pr.merged",
                "fields": {
                    "head_sha": { "json_field": { "path": "/headRefOid" } },
                    "merge_sha": { "json_field": { "path": "/mergeCommit/oid" } }
                }
            }))
            .expect("merge event spec"),
        ),
        context: serde_json::Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: vec![
                "sh".into(),
                "-c".into(),
                "out=$(gh pr view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; case \"$out\" in *'\"state\":\"MERGED\"'*) exit 0 ;; *) exit 1 ;; esac".into(),
                "sh".into(),
                pr_number.to_string(),
                repo_arg.clone(),
            ],
            output_probe_argv: Some(vec![
                "gh".into(),
                "pr".into(),
                "view".into(),
                pr_number.to_string(),
                "--repo".into(),
                repo_arg.clone(),
                "--json".into(),
                "headRefOid,mergeCommit".into(),
            ]),
        }),
        cwd_lease: fx.lease_abs.clone(),
        result_path: result_path.clone(),
        deadline_ms: now_ms() + 60_000,
    };
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(payload.idem_key.clone()),
        payload_hash: "merge-never-ran-semantic-hash".into(),
    };
    let op_id = fx
        ._runtime
        .submit(
            FORGE_ACTION_KIND,
            key,
            serde_json::to_value(payload).expect("payload json"),
        )
        .await
        .expect("submit parked merge");
    wait_for_operation_phase(&fx.repo, &op_id, "parked").await;
    let _sleep_guard = parked_process_group_guard(&fx.repo, &op_id).await;
    assert_result_files_absent(&result_path);
    assert!(
        !pr_is_merged(&state, pr_number),
        "parked never-ran merge must leave PR open"
    );

    mark_parked_artifacts_dead(&fx.repo, &op_id).await;
    mark_workspace_lease_stale_for_boot(&fx.repo, &fx.lease_id).await;
    let recovery = boot_recovery_runtime(&fx).await;
    let plan = recovery.recover_on_boot().await.expect("recover on boot");
    assert!(
        plan.items
            .iter()
            .any(|item| matches!(item, RecoveryItem::VerifyParked { op_id: item_op_id } if item_op_id == &op_id)),
        "never-ran merge should recover through parked verification: {:?}",
        plan.items
    );
    recovery.apply_recovery(plan).await.expect("apply recovery");
    let result = wait_for_recovery_result(&recovery, &op_id).await;
    match result.outcome {
        OperationOutcome::Failed {
            last_error_class, ..
        } => {
            assert_eq!(last_error_class.as_deref(), Some("action-not-landed"));
        }
        other => panic!("never-ran merge should fail not-landed: {other:?}"),
    }
    assert_eq!(operation_phase(&fx.repo, &op_id).await, "failed");
    assert_event_count_stays(&fx.repo, "forge.pr.merged", 0).await;
    assert!(
        !pr_is_merged(&state, pr_number),
        "not-landed recovery must not mutate PR state; head was {head_sha}"
    );

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn git_forge_issue_close_crash_recovers_once_via_verdict_probe() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let path_dir = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(path_dir.path());
    let path_value = prepend_to_path(path_dir.path());
    let results_dir = short_tempdir("r").expect("forge results tempdir");
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let _path = EnvGuard::set("PATH", path_value);

    let fx = boot_fixture().await;
    let repo_arg = fx.origin_repo.display().to_string();
    let issue_number = 810_u64;
    let state = shim_state_dir(&fx.origin_repo);
    let block = ShimBlock::new(&state, "issue_close");

    let issue_resp = call_tool(
        &fx,
        30,
        ISSUE_CLOSE_TOOL,
        json!({ "repo": repo_arg, "issue": issue_number }),
    )
    .await;
    assert_tool_succeeded(&issue_resp, "gh.issue.close");
    let op_id = op_id_from_response(&issue_resp);
    wait_for_counter(&state.join("issue_close_count"), 1).await;
    wait_for_operation_phase(&fx.repo, &op_id, "parked").await;
    let result_path = operation_result_path(&fx.repo, &op_id).await;
    assert_result_files_absent(&result_path);

    mark_parked_artifacts_dead(&fx.repo, &op_id).await;
    mark_workspace_lease_stale_for_boot(&fx.repo, &fx.lease_id).await;
    let recovery = boot_recovery_runtime(&fx).await;
    let plan = recovery.recover_on_boot().await.expect("recover on boot");
    assert!(
        plan.items
            .iter()
            .any(|item| matches!(item, RecoveryItem::VerifyParked { op_id: item_op_id } if item_op_id == &op_id)),
        "issue close crash should recover through parked verification: {:?}",
        plan.items
    );
    recovery.apply_recovery(plan).await.expect("apply recovery");
    let result = wait_for_recovery_result(&recovery, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "issue close recovery should succeed: {:?}",
        result.outcome
    );
    assert_eq!(operation_phase(&fx.repo, &op_id).await, "succeeded");

    let issue_rows = wait_for_event_count(&fx.repo, "forge.issue.closed", 1).await;
    let issue_closed = issue_rows[0].clone();
    assert_wave_event(&issue_closed, &fx.wave_id);
    assert_eq!(issue_closed.payload["issue_number"], issue_number);
    assert_eq!(shim_counter(&state.join("issue_close_count")), 1);

    block.release();
    wait_for_result_code_file(&result_path).await;
    assert_event_count_stays(&fx.repo, "forge.issue.closed", 1).await;
    assert_eq!(shim_counter(&state.join("issue_close_count")), 1);

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn dual_review_converges_then_merges() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _env = setup_forge_env();

    let fx = boot_fixture().await;
    let design_round = emit_review_round(&fx, &ReviewRoundInput::design("760")).await;
    let pr = drive_pr_to_diff(
        &fx,
        40,
        760,
        "slice-760-review-converges",
        "review-converges.txt",
        "review convergence e2e\n",
        "Review convergence E2E",
    )
    .await;
    let impl_round = emit_review_round(
        &fx,
        &ReviewRoundInput::impl_round("760", pr.pr_number, &pr.head_sha, 1, 8, true),
    )
    .await;

    let merged = merge_reviewed_pr(&fx, 44, &pr, "760").await;
    let issue_closed = close_issue(&fx, 46, &pr.repo_arg, 760).await;
    transition_wave_to_done(&fx).await;
    let done = wait_for_event_matching(&fx.repo, "wave.lifecycle_changed", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id) && row.payload["to"] == "done"
    })
    .await;

    if let Some(first_impl_dispatch) = event_rows(&fx.repo, "task.dispatched").await.first() {
        assert!(
            design_round.id < first_impl_dispatch.id,
            "design review must precede first impl dispatch"
        );
    }
    assert!(
        design_round.id < merged.id,
        "design review must precede merge"
    );
    assert!(impl_round.id < merged.id, "PR review must precede merge");
    assert_eq!(
        row_head_sha(&merged).as_deref(),
        Some(pr.head_sha.as_str()),
        "merge must use reviewed head"
    );
    assert!(merged.id < issue_closed.id, "merge must precede close");
    assert!(issue_closed.id < done.id, "close must precede done");
    assert_subject_keyed_cap_enforcement(&fx).await;

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn cap_exhausted_give_up_fails_terminal() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _env = setup_forge_env();

    let fx = boot_fixture().await;
    let pr = drive_pr_to_diff(
        &fx,
        50,
        761,
        "slice-760-review-give-up",
        "review-give-up.txt",
        "review give-up e2e\n",
        "Review give-up E2E",
    )
    .await;
    let cap_round = emit_review_round(
        &fx,
        &ReviewRoundInput::impl_round("760", pr.pr_number, &pr.head_sha, 1, 1, false),
    )
    .await;

    transition_wave_along(
        &fx,
        &[
            WaveLifecycle::Planning,
            WaveLifecycle::Dispatching,
            WaveLifecycle::Working,
            WaveLifecycle::Reviewing,
            WaveLifecycle::Failed,
        ],
        "scripted give-up after cap exhaustion",
    )
    .await;
    let failed = wait_for_event_matching(&fx.repo, "wave.lifecycle_changed", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id) && row.payload["to"] == "failed"
    })
    .await;
    assert_eq!(failed.payload["from"], "reviewing");
    assert!(cap_round.id < failed.id);
    assert!(
        event_rows(&fx.repo, "forge.pr.merged")
            .await
            .iter()
            .all(|row| row.payload["subject"]["pr_number"] != json!(pr.pr_number)),
        "give-up subject must not merge"
    );
    assert_subject_keyed_cap_enforcement(&fx).await;

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn cap_exhausted_ask_human_pauses_then_resumes() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _env = setup_forge_env();

    let fx = boot_fixture().await;
    let pr = drive_pr_to_diff(
        &fx,
        60,
        762,
        "slice-760-review-ask-human",
        "review-ask-human.txt",
        "review ask-human e2e\n",
        "Review ask-human E2E",
    )
    .await;
    transition_wave_along(
        &fx,
        &[
            WaveLifecycle::Planning,
            WaveLifecycle::Dispatching,
            WaveLifecycle::Working,
            WaveLifecycle::Reviewing,
        ],
        "ready for capped review",
    )
    .await;
    let cap_round = emit_review_round(
        &fx,
        &ReviewRoundInput::impl_round("760", pr.pr_number, &pr.head_sha, 1, 1, false),
    )
    .await;
    transition_wave_along(
        &fx,
        &[WaveLifecycle::Working],
        "ask human after capped review",
    )
    .await;
    let request = request_ratification(&fx, "cap_exhausted").await;

    let lifecycle = event_rows(&fx.repo, "wave.lifecycle_changed").await;
    let reviewing_to_working = lifecycle
        .iter()
        .find(|row| {
            row.id > cap_round.id
                && row.scope_wave.as_deref() == Some(&fx.wave_id)
                && row.payload["from"] == "reviewing"
                && row.payload["to"] == "working"
        })
        .expect("reviewing->working edge before ratify");
    let working_to_blocked = lifecycle
        .iter()
        .find(|row| {
            row.id > reviewing_to_working.id
                && row.scope_wave.as_deref() == Some(&fx.wave_id)
                && row.payload["from"] == "working"
                && row.payload["to"] == "blocked"
        })
        .expect("working->blocked edge from ratify request");
    assert!(working_to_blocked.id < request.id);
    assert!(
        lifecycle.iter().all(|row| {
            !(row.scope_wave.as_deref() == Some(&fx.wave_id)
                && row.payload["from"] == "reviewing"
                && row.payload["to"] == "blocked")
        }),
        "ASK-HUMAN must not use a direct reviewing->blocked edge"
    );
    assert!(
        event_rows(&fx.repo, "forge.pr.merged").await.is_empty(),
        "merge must be absent while latest subject round is unconverged before grant"
    );
    assert_subject_keyed_cap_enforcement(&fx).await;

    let (status, body) = post_ratify(&fx, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resolved = wait_for_event_matching(&fx.repo, "ratify.resolved", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id) && row.payload["decision"] == "grant"
    })
    .await;
    let unblocked = wait_for_event_matching(&fx.repo, "wave.lifecycle_changed", |row| {
        row.id > request.id
            && row.scope_wave.as_deref() == Some(&fx.wave_id)
            && row.payload["from"] == "blocked"
            && row.payload["to"] == "working"
    })
    .await;
    assert!(unblocked.id < resolved.id);

    transition_wave_along(
        &fx,
        &[WaveLifecycle::Reviewing],
        "resume review after grant",
    )
    .await;
    let converged = emit_review_round(
        &fx,
        &ReviewRoundInput::impl_round("760", pr.pr_number, &pr.head_sha, 2, 8, true),
    )
    .await;
    let merged = merge_reviewed_pr(&fx, 64, &pr, "760").await;
    close_issue(&fx, 66, &pr.repo_arg, 762).await;
    transition_wave_along(
        &fx,
        &[WaveLifecycle::Done],
        "done after ratified convergence",
    )
    .await;

    assert!(resolved.id < converged.id);
    assert!(converged.id < merged.id);
    assert_eq!(row_head_sha(&merged).as_deref(), Some(pr.head_sha.as_str()));
    assert_subject_keyed_cap_enforcement(&fx).await;

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn review_round_recovers_into_pending_queue() {
    let fx = boot_fixture().await;
    transition_wave_along(
        &fx,
        &[
            WaveLifecycle::Planning,
            WaveLifecycle::Dispatching,
            WaveLifecycle::Working,
        ],
        "recovery ratify setup",
    )
    .await;
    let input = ReviewRoundInput::impl_round("760", 760, "head-sha-recovery", 1, 1, false);
    emit_review_round(&fx, &input).await;
    request_ratification(&fx, "cap_exhausted").await;
    let (status, body) = post_ratify(&fx, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_for_event_matching(&fx.repo, "ratify.resolved", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id) && row.payload["decision"] == "grant"
    })
    .await;

    let runtime = fx
        .repo
        .session_projection_by_id(&SPEC_SESSION_ID.to_string())
        .await
        .expect("query spec runtime")
        .expect("spec runtime");
    let repo: Arc<dyn Repo> = fx.repo.clone();
    let daemon = SharedCodexAppServer::new_stub(repo.clone());
    let registry = HarnessRegistry::new();
    let handle = spawn_recovered_harness(
        repo,
        fx.events.clone(),
        fx.card_role_cache.clone(),
        fx.wave_cove_cache.clone(),
        daemon,
        &registry,
        runtime,
    )
    .await
    .expect("spawn recovered harness")
    .expect("recovered harness");

    let pending = wait_for_recovered_pending(&handle).await;
    assert!(
        pending.iter().any(|obs| matches!(
            obs,
            Observation::ReviewRound {
                phase,
                slice_id,
                pr_number: Some(760),
                head_sha: Some(head_sha),
                n: 1,
                cap: 1,
                converged: false,
                ..
            } if phase == "impl" && slice_id == "760" && head_sha == "head-sha-recovery"
        )),
        "review.round must recover into pending queue: {pending:?}"
    );
    assert!(
        pending.iter().any(|obs| matches!(
            obs,
            Observation::RatifyRequested { reason, .. } if reason == "cap_exhausted"
        )),
        "ratify.requested must recover into pending queue: {pending:?}"
    );
    assert!(
        pending.iter().any(|obs| matches!(
            obs,
            Observation::RatifyResolved {
                decision: RatifyDecision::Grant,
                ..
            }
        )),
        "ratify.resolved grant must recover into pending queue: {pending:?}"
    );
    assert_review_round_duplicate_noop(&fx, &input).await;

    handle.shutdown().await.expect("shutdown recovered harness");
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

#[tokio::test]
async fn fu4_teardown_releases_after_merge_close_and_fences_in_flight_forge_op() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _env = setup_forge_env();

    let fx = boot_fixture().await;
    let pr = drive_pr_to_diff(
        &fx,
        70,
        763,
        "slice-760-fu4",
        "fu4.txt",
        "fu4 teardown fence e2e\n",
        "FU4 teardown fence E2E",
    )
    .await;
    let state = shim_state_dir(&fx.origin_repo);
    let block = ShimBlock::new(&state, "pr_merge");

    let checks_resp = call_tool(
        &fx,
        74,
        PR_CHECKS_TOOL,
        json!({ "repo": pr.repo_arg, "pr": pr.pr_number }),
    )
    .await;
    assert_tool_succeeded(&checks_resp, "gh.pr.checks");
    let merge_resp = call_tool(
        &fx,
        75,
        PR_MERGE_TOOL,
        json!({
            "repo": pr.repo_arg.as_str(),
            "pr": pr.pr_number,
            "phase": "impl",
            "slice_id": "760",
            "expected_head_sha": pr.head_sha.as_str()
        }),
    )
    .await;
    assert_tool_succeeded(&merge_resp, "gh.pr.merge");
    let op_id = op_id_from_response(&merge_resp);
    wait_for_counter(&state.join("pr_merge_count"), 1).await;
    wait_for_operation_phase(&fx.repo, &op_id, "parked").await;

    let status = delete_wave(&fx).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(workspace_lease_state(&fx.repo, &fx.lease_id).await, "held");
    assert!(
        fx.lease_abs.exists(),
        "lease path must survive fenced teardown"
    );

    block.release();
    wait_for_operation_phase(&fx.repo, &op_id, "succeeded").await;
    let merged = wait_for_event_matching(&fx.repo, "forge.pr.merged", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id)
            && row.payload["subject"]["pr_number"] == json!(pr.pr_number)
    })
    .await;
    assert_eq!(row_head_sha(&merged).as_deref(), Some(pr.head_sha.as_str()));
    close_issue(&fx, 76, &pr.repo_arg, 763).await;

    let release_count_before_teardown = event_rows(&fx.repo, "workspace.released").await.len();
    let status = delete_wave(&fx).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let released_rows = wait_for_event_count(
        &fx.repo,
        "workspace.released",
        release_count_before_teardown + 1,
    )
    .await;
    let released = released_rows
        .iter()
        .find(|row| row.payload["lease_id"] == json!(fx.lease_id))
        .expect("teardown persisted workspace.released for worker lease");
    assert_eq!(
        released.payload["card_id"],
        json!(fx.worker_card_id.as_str()),
        "workspace.released must identify the released worker card"
    );
    assert_eq!(
        workspace_lease_state_optional(&fx.repo, &fx.lease_id).await,
        None,
        "wave delete cascades released workspace lease rows after persisting workspace.released"
    );
    assert!(
        !git_ref_exists(
            &fx.wave_cwd,
            &format!("refs/heads/neige/{}/{}", fx.wave_id, fx.worker_card_id),
        ),
        "wave teardown must remove the released worker branch"
    );
    assert!(
        !fx.lease_abs.exists(),
        "wave teardown must remove the released worker checkout"
    );

    let release_count = event_rows(&fx.repo, "workspace.released").await.len();
    let recovery = boot_recovery_runtime(&fx).await;
    let _plan = recovery.recover_on_boot().await.expect("recover on boot");
    assert_event_count_stays(&fx.repo, "workspace.released", release_count).await;
    assert_eq!(
        workspace_lease_state_optional(&fx.repo, &fx.lease_id).await,
        None,
        "boot recovery must not recreate a cascaded released lease row"
    );

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
}

async fn boot_fixture() -> Fixture {
    let tmp = short_tempdir("w").expect("tempdir");
    let socket_tmp = socket_tempdir().expect("MCP socket tempdir");
    let socket_path = socket_tmp.path().join("mcp").join("kernel.sock");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let wave_cwd = tmp.path().join("wave-cwd");
    std::fs::create_dir_all(&wave_cwd).expect("create wave cwd");

    let origin_repo = tmp.path().join("origin.git");
    init_bare_origin(&origin_repo, &tmp.path().join("seed"));
    clone_for_wave(&origin_repo, &wave_cwd);

    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let events = EventBus::new();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());

    let cove = repo
        .cove_create(NewCove {
            name: "forge-workflow-e2e".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "forge-workflow-e2e".into(),
            sort: None,
            cwd: wave_cwd.display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    repo.seed_wave_cove_cache(&wave_cove_cache)
        .await
        .expect("seed wave/cove cache");

    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("create spec card");
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    seed_spec_runtime(&sqlx_repo, &wave.id, &spec_card.id).await;

    let caller =
        create_worker_caller(&sqlx_repo, &card_role_cache, wave.id.clone(), &wave_cwd).await;
    provision_worker_worktree(
        &wave_cwd,
        &caller.wave_id,
        &caller.card_id,
        &caller.lease_abs,
    );

    let plugin_host = boot_plugin_host(
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir.clone(),
        events.clone(),
        write.clone(),
    )
    .await;
    plugin_host.spawn(PLUGIN_ID).await.expect("spawn plugin");
    wait_for_running(&plugin_host).await;
    emit_workflow_registered_events_for_fixture(
        &repo,
        &events,
        &card_role_cache,
        &wave_cove_cache,
        &plugin_host,
    )
    .await;

    let operation_repo = Arc::new(SqlxOperationRepo::new(sqlx_repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let runtime = Arc::new(
        OperationRuntime::new(
            operation_repo.clone(),
            vec![Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>],
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events.clone(),
                completion,
            ),
        )
        .await
        .expect("operation runtime"),
    );

    let plugin_host_cell = Arc::new(OnceCell::new());
    assert!(plugin_host_cell.set(plugin_host.clone()).is_ok());
    let operation_runtime_cell = Arc::new(OnceCell::new());
    assert!(operation_runtime_cell.set(runtime.clone()).is_ok());
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let review_ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs: sqlx_repo
            .sqlite_pool()
            .map(calm_truth::wave_vcs_repo::SqlxWaveVcsRepo::shared),
        events: events.clone(),
        write: write.clone(),
        daemon_token_hash: None,
        gate_logs_dir: tmp.path().join("gate-logs"),
        plugin_host: plugin_host_cell.clone(),
        operation_runtime: operation_runtime_cell.clone(),
    });
    let mut review_registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut review_registry);
    let review_registry = Arc::new(review_registry);
    let server = McpServer::spawn(
        repo,
        events.clone(),
        write.clone(),
        socket_path.clone(),
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
        None,
        plugin_host_cell,
        operation_runtime_cell,
        tmp.path().join("gate-logs"),
    )
    .await
    .expect("spawn McpServer");

    Fixture {
        _server: server,
        plugin_host,
        repo: sqlx_repo,
        events,
        write,
        card_role_cache,
        wave_cove_cache,
        review_ctx,
        review_registry,
        socket_path,
        raw_token: caller.raw_token,
        thread_id: caller.thread_id,
        wave_id: caller.wave_id,
        cove_id: cove.id.to_string(),
        spec_card_id: spec_card.id.to_string(),
        worker_card_id: caller.card_id,
        lease_id: caller.lease_id,
        lease_abs: caller.lease_abs,
        wave_cwd,
        origin_repo,
        _runtime: runtime,
        _socket_tmp: socket_tmp,
        _tmp: tmp,
    }
}

async fn create_worker_caller(
    sqlx_repo: &Arc<SqlxRepo>,
    card_role_cache: &CardRoleCache,
    wave_id: WaveId,
    wave_cwd: &Path,
) -> Caller {
    let card_id = calm_server::model::new_id();
    let runtime_id = calm_server::model::new_id();
    let lease_abs = wave_cwd
        .join(".claude")
        .join("worktrees")
        .join(wave_id.as_str())
        .join(&card_id);
    let lease_path = lease_abs.display().to_string();

    let mut tx = sqlx_repo.pool().begin().await.expect("begin card tx");
    let (_card, _term, mcp_token) = card_with_codex_create_tx(
        &mut tx,
        card_id.clone(),
        &runtime_id,
        None,
        wave_id.clone(),
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .expect("mint codex card");
    let raw_token = match mcp_token {
        Some(token) => token,
        None => {
            let token = calm_server::mcp_server::auth::CardMcpToken::generate();
            let token_hash = calm_server::mcp_server::auth::hash_token(token.as_str());
            card_mcp_token_set_tx(&mut tx, &card_id, &token_hash)
                .await
                .expect("mint card MCP token");
            session_mcp_token_set_tx(&mut tx, &runtime_id, &token_hash)
                .await
                .expect("mint session MCP token");
            token.into_inner()
        }
    };
    let lease_id = insert_workspace_lease(&mut tx, &card_id, wave_id.as_str(), &lease_path).await;
    tx.commit().await.expect("commit card tx");

    let thread_id = format!("thread-{card_id}");
    seed_runtime_thread(sqlx_repo, card_id.as_str(), thread_id.as_str()).await;

    Caller {
        card_id,
        raw_token,
        thread_id,
        wave_id: wave_id.to_string(),
        lease_id,
        lease_abs,
    }
}

async fn seed_spec_runtime(sqlx_repo: &SqlxRepo, wave_id: &WaveId, spec_card_id: &CardId) {
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("spec-thread".into());
    let mut tx = sqlx_repo.pool().begin().await.expect("begin spec tx");
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: SPEC_SESSION_ID.to_string(),
            card_id: spec_card_id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("spec-thread".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).expect("snapshot json")),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("start spec runtime");
    session_mark_wave_root_tx(&mut tx, wave_id, &WorkerSessionId::from(SPEC_SESSION_ID))
        .await
        .expect("mark spec root session");
    tx.commit().await.expect("commit spec tx");
}

fn wave_router_for_fixture(fx: &Fixture) -> axum::Router {
    let repo: Arc<dyn Repo> = fx.repo.clone();
    let state = AppState::from_parts(
        repo,
        fx.events.clone(),
        Arc::new(DaemonClient::new_stub()),
        fx.plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(fx.card_role_cache.clone()),
        Some(fx.wave_cove_cache.clone()),
    );
    calm_server::routes::waves::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

fn app_router_for_fixture(fx: &Fixture) -> axum::Router {
    let repo: Arc<dyn Repo> = fx.repo.clone();
    let state = AppState::from_parts(
        repo,
        fx.events.clone(),
        Arc::new(DaemonClient::new_stub()),
        fx.plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(fx.card_role_cache.clone()),
        Some(fx.wave_cove_cache.clone()),
    );
    calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn post_wave(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/waves")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn emit_workflow_registered_events_for_fixture(
    repo: &Arc<dyn Repo>,
    events: &EventBus,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &calm_server::wave_cove_cache::WaveCoveCache,
    plugin_host: &Arc<PluginHost>,
) {
    let running_plugin_ids = plugin_host.running_plugin_ids().await;
    for manifest in plugin_host.registry().list() {
        let plugin_id = manifest.id.clone();
        if !running_plugin_ids.contains(&plugin_id) || !trusted_forge_plugin(&plugin_id) {
            continue;
        }
        for workflow in manifest.workflows {
            repo.log_pure_event(
                ActorId::Kernel,
                EventScope::System,
                None,
                events,
                card_role_cache,
                wave_cove_cache,
                Event::WorkflowRegistered {
                    plugin_id: plugin_id.clone(),
                    workflow_id: workflow.id,
                },
            )
            .await
            .expect("log workflow.registered");
        }
    }
}

async fn boot_plugin_host(
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    plugins_data_dir: PathBuf,
    events: EventBus,
    write: WriteContext,
) -> Arc<PluginHost> {
    let install_dir = plugins_dir.join(PLUGIN_ID);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
    std::fs::create_dir_all(&plugins_data_dir).expect("create plugin data dir");
    std::os::unix::fs::symlink(Path::new(FORGE_BIN), bin_dir.join("git-forge"))
        .expect("symlink git-forge plugin");

    let manifest = read_manifest();
    let manifest_json = manifest.to_json();
    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    repo.plugin_install(NewPlugin {
        id: PLUGIN_ID.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: manifest_json,
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");

    Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo,
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
        write,
    ))
}

async fn insert_workspace_lease(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    wave_id: &str,
    path: &str,
) -> String {
    let now = now_ms();
    let lease_id = calm_server::model::new_id();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, wave_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, NULL, ?7, ?7)"#,
    )
    .bind(&lease_id)
    .bind(card_id)
    .bind(wave_id)
    .bind(path)
    .bind("test-lease-owner")
    .bind(now + 60_000)
    .bind(now)
    .execute(&mut **tx)
    .await
    .expect("insert workspace lease");
    lease_id
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) -> String {
    let mut tx = repo.pool().begin().await.expect("begin runtime tx");
    let runtime_id = if let Some(runtime) = session_projection_active_for_card_tx(&mut tx, card_id)
        .await
        .expect("active runtime lookup")
    {
        let runtime_id = runtime.id.clone();
        session_bind_attribution_tx(
            &mut tx,
            &runtime_id,
            ThreadAttribution {
                runtime_id: runtime_id.clone(),
                provider: AgentProvider::Codex,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
            },
        )
        .await
        .expect("bind thread attribution");
        runtime_id
    } else {
        let runtime = session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: calm_server::model::new_id(),
                card_id: card_id.to_string(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .expect("start runtime");
        runtime.id
    };
    tx.commit().await.expect("commit runtime tx");
    runtime_id
}

async fn wait_for_running(host: &Arc<PluginHost>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = host.status(PLUGIN_ID).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn assert_tools_are_discoverable(fx: &Fixture) {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(&mut wr, tools_list_frame(2, &fx.thread_id)).await;
    let list = recv_frame(&mut rd).await;
    assert!(list.get("error").is_none(), "tools/list errored: {list:#?}");
    let names = list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        COMMIT_TOOL,
        PR_LIST_TOOL,
        PR_CREATE_TOOL,
        PR_DIFF_TOOL,
        PR_CHECKS_TOOL,
        PR_MERGE_TOOL,
        ISSUE_VIEW_TOOL,
        ISSUE_CLOSE_TOOL,
    ] {
        assert!(
            names.contains(&expected),
            "git-forge plugin tool missing from discovery: {expected}; got {names:?}"
        );
    }
}

async fn call_tool(fx: &Fixture, id: i64, name: &str, args: Value) -> Value {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(&mut wr, tools_call_frame(id, name, &fx.thread_id, args)).await;
    recv_frame(&mut rd).await
}

fn spec_identity(fx: &Fixture) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: fx.spec_card_id.clone(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: SPEC_SESSION_ID.to_string(),
        wave_id: Some(fx.wave_id.clone()),
        cove_id: fx.cove_id.clone(),
        thread_id: "spec-thread".into(),
    }
}

async fn call_review_tool(
    fx: &Fixture,
    name: &str,
    args: Value,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    let handler = fx
        .review_registry
        .lookup(name)
        .unwrap_or_else(|| panic!("review tool not registered: {name}"));
    handler(fx.review_ctx.clone(), spec_identity(fx), args).await
}

fn approved_channels() -> Vec<ChannelVerdict> {
    vec![
        ChannelVerdict {
            role: "reviewer-a".into(),
            verdict: "approved".into(),
        },
        ChannelVerdict {
            role: "reviewer-b".into(),
            verdict: "approved".into(),
        },
    ]
}

fn changes_requested_channels() -> Vec<ChannelVerdict> {
    vec![
        ChannelVerdict {
            role: "reviewer-a".into(),
            verdict: "changes_requested".into(),
        },
        ChannelVerdict {
            role: "reviewer-b".into(),
            verdict: "approved".into(),
        },
    ]
}

#[derive(Clone, Debug)]
struct ReviewRoundInput {
    phase: String,
    slice_id: String,
    pr_number: Option<u64>,
    head_sha: Option<String>,
    n: u32,
    cap: u32,
    converged: bool,
    channels: Vec<ChannelVerdict>,
    root_cause: Option<String>,
}

impl ReviewRoundInput {
    fn design(slice_id: &str) -> Self {
        Self {
            phase: "design".into(),
            slice_id: slice_id.into(),
            pr_number: None,
            head_sha: None,
            n: 1,
            cap: 8,
            converged: true,
            channels: approved_channels(),
            root_cause: None,
        }
    }

    fn impl_round(
        slice_id: &str,
        pr_number: u64,
        head_sha: &str,
        n: u32,
        cap: u32,
        converged: bool,
    ) -> Self {
        Self {
            phase: "impl".into(),
            slice_id: slice_id.into(),
            pr_number: Some(pr_number),
            head_sha: Some(head_sha.into()),
            n,
            cap,
            converged,
            channels: if converged {
                approved_channels()
            } else {
                changes_requested_channels()
            },
            root_cause: (!converged).then(|| "scripted review did not converge".into()),
        }
    }

    fn args(&self) -> Value {
        let mut subject = json!({
            "phase": self.phase.as_str(),
            "slice_id": self.slice_id.as_str(),
        });
        if let Some(pr_number) = self.pr_number {
            subject["pr_number"] = json!(pr_number);
        }
        let mut args = json!({
            "subject": subject,
            "n": self.n,
            "cap": self.cap,
            "converged": self.converged,
            "channels": self.channels.clone(),
        });
        if let Some(head_sha) = &self.head_sha {
            args["head_sha"] = json!(head_sha);
        }
        if let Some(root_cause) = &self.root_cause {
            args["root_cause"] = json!(root_cause);
        }
        args
    }
}

async fn emit_review_round(fx: &Fixture, input: &ReviewRoundInput) -> EventRow {
    let before = event_rows(&fx.repo, "review.round").await.len();
    let resp = call_review_tool(fx, TOOL_REVIEW_ROUND, input.args())
        .await
        .expect("calm.review.round succeeds");
    assert_eq!(resp["ok"], true, "review.round response: {resp}");
    assert_eq!(
        resp["emitted"], true,
        "review.round should append in this helper: {resp}"
    );
    let rows = wait_for_event_count(&fx.repo, "review.round", before + 1).await;
    rows.last().expect("new review.round").clone()
}

async fn assert_review_round_duplicate_noop(fx: &Fixture, input: &ReviewRoundInput) {
    let before = event_rows(&fx.repo, "review.round").await.len();
    let resp = call_review_tool(fx, TOOL_REVIEW_ROUND, input.args())
        .await
        .expect("duplicate review.round succeeds as no-op");
    assert_eq!(resp["ok"], true, "duplicate review.round response: {resp}");
    assert_eq!(
        resp["emitted"], false,
        "duplicate review.round should be an idempotent no-op: {resp}"
    );
    assert_event_count_stays(&fx.repo, "review.round", before).await;
}

async fn request_ratification(fx: &Fixture, reason: &str) -> EventRow {
    let before = event_rows(&fx.repo, "ratify.requested").await.len();
    let resp = call_review_tool(fx, TOOL_RATIFY_REQUEST, json!({ "reason": reason }))
        .await
        .expect("calm.ratify.request succeeds");
    assert_eq!(resp["ok"], true, "ratify.request response: {resp}");
    let rows = wait_for_event_count(&fx.repo, "ratify.requested", before + 1).await;
    rows.last().expect("new ratify.requested").clone()
}

async fn post_ratify(fx: &Fixture, decision: &str) -> (StatusCode, Value) {
    let body = serde_json::to_vec(&json!({
        "decision": decision,
        "message": format!("human says {decision}")
    }))
    .expect("ratify body json");
    let resp = app_router_for_fixture(fx)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", fx.spec_card_id))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn delete_wave(fx: &Fixture) -> StatusCode {
    app_router_for_fixture(fx)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/waves/{}", fx.wave_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn transition_wave_along(fx: &Fixture, targets: &[WaveLifecycle], message: &str) {
    let wave_id = WaveId::from(fx.wave_id.clone());
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: CoveId::from(fx.cove_id.clone()),
    };
    let actor = ActorId::AiSpec(CardId::from(fx.spec_card_id.clone()));
    let targets = targets.to_vec();
    let message = message.to_string();
    write_with_actor_events_typed::<(), _>(
        fx.repo.as_ref(),
        None,
        &fx.events,
        &fx.write,
        move |tx| {
            let wave_id = wave_id.clone();
            let scope = scope.clone();
            let actor = actor.clone();
            let targets = targets.clone();
            let message = message.clone();
            Box::pin(async move {
                let mut events = Vec::new();
                for target in targets {
                    let lifecycle_events =
                        calm_server::wave_lifecycle::apply_requested_transition_in_tx(
                            tx,
                            &wave_id,
                            target,
                            &actor,
                            message.clone(),
                        )
                        .await?
                        .unwrap_or_else(|| {
                            panic!("expected lifecycle transition to {target:?} to persist")
                        });
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), scope.clone(), event)),
                    );
                }
                Ok(((), events))
            })
        },
    )
    .await
    .expect("transition wave lifecycle");
}

#[derive(Clone, Debug)]
struct ForgePrRun {
    repo_arg: String,
    pr_number: u64,
    head_sha: String,
}

async fn drive_pr_to_diff(
    fx: &Fixture,
    id_base: i64,
    issue_number: u64,
    head: &str,
    filename: &str,
    contents: &str,
    title: &str,
) -> ForgePrRun {
    let repo_arg = fx.origin_repo.display().to_string();
    let base = "main";
    let issue_view_resp = call_tool(
        fx,
        id_base,
        ISSUE_VIEW_TOOL,
        json!({ "repo": repo_arg, "issue": issue_number }),
    )
    .await;
    assert_tool_succeeded(&issue_view_resp, "gh.issue.view");

    run_git(&fx.lease_abs, ["checkout", "-B", head, "origin/main"]);
    stage_git_change(&fx.lease_abs, filename, contents);
    let commit_resp = call_tool(
        fx,
        id_base + 1,
        COMMIT_TOOL,
        json!({ "message": title, "idem": format!("{head}-commit") }),
    )
    .await;
    assert_tool_succeeded(&commit_resp, "git.commit");
    let head_sha = run_git_capture(&fx.lease_abs, ["rev-parse", "HEAD"]);
    let base_sha = run_git_capture(&fx.lease_abs, ["rev-parse", "origin/main"]);
    run_git(&fx.lease_abs, ["push", "-u", "origin", head, "--force"]);

    let create_resp = call_tool(
        fx,
        id_base + 2,
        PR_CREATE_TOOL,
        json!({
            "repo": repo_arg,
            "head": head,
            "base": base,
            "title": title,
            "body": "Created by forge workflow review E2E"
        }),
    )
    .await;
    assert_tool_succeeded(&create_resp, "gh.pr.create");
    let opened = wait_for_event_matching(&fx.repo, "forge.pr.opened", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id) && row.payload["head_sha"] == json!(head_sha)
    })
    .await;
    let pr_number = opened.payload["pr_number"].as_u64().expect("pr number");

    let diff_resp = call_tool(
        fx,
        id_base + 3,
        PR_DIFF_TOOL,
        json!({
            "repo": repo_arg,
            "pr": pr_number,
            "base_sha": base_sha,
            "head_sha": head_sha
        }),
    )
    .await;
    assert_tool_succeeded(&diff_resp, "gh.pr.diff");
    let diff = wait_for_event_matching(&fx.repo, "forge.pr.diff.read", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id)
            && row.payload["pr_number"] == json!(pr_number)
            && row.payload["head_sha"] == json!(head_sha)
    })
    .await;
    assert_eq!(diff.payload["base_sha"], base_sha);

    ForgePrRun {
        repo_arg,
        pr_number,
        head_sha,
    }
}

async fn merge_reviewed_pr(
    fx: &Fixture,
    id_base: i64,
    pr: &ForgePrRun,
    slice_id: &str,
) -> EventRow {
    let checks_resp = call_tool(
        fx,
        id_base,
        PR_CHECKS_TOOL,
        json!({ "repo": pr.repo_arg, "pr": pr.pr_number }),
    )
    .await;
    assert_tool_succeeded(&checks_resp, "gh.pr.checks");

    let merge_resp = call_tool(
        fx,
        id_base + 1,
        PR_MERGE_TOOL,
        json!({
            "repo": pr.repo_arg,
            "pr": pr.pr_number,
            "phase": "impl",
            "slice_id": slice_id,
            "expected_head_sha": pr.head_sha
        }),
    )
    .await;
    assert_tool_succeeded(&merge_resp, "gh.pr.merge");
    let merged = wait_for_event_matching(&fx.repo, "forge.pr.merged", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id)
            && row.payload["subject"]["pr_number"] == json!(pr.pr_number)
    })
    .await;
    assert_eq!(merged.payload["head_sha"], pr.head_sha);
    merged
}

async fn close_issue(fx: &Fixture, id: i64, repo_arg: &str, issue_number: u64) -> EventRow {
    let issue_resp = call_tool(
        fx,
        id,
        ISSUE_CLOSE_TOOL,
        json!({ "repo": repo_arg, "issue": issue_number }),
    )
    .await;
    assert_tool_succeeded(&issue_resp, "gh.issue.close");
    wait_for_event_matching(&fx.repo, "forge.issue.closed", |row| {
        row.scope_wave.as_deref() == Some(&fx.wave_id)
            && row.payload["issue_number"] == json!(issue_number)
    })
    .await
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SubjectKey {
    phase: String,
    slice_id: String,
    pr_number: Option<u64>,
}

impl SubjectKey {
    fn from_subject_payload(subject: &Value) -> Self {
        Self {
            phase: subject["phase"]
                .as_str()
                .expect("subject.phase")
                .to_string(),
            slice_id: subject["slice_id"]
                .as_str()
                .expect("subject.slice_id")
                .to_string(),
            pr_number: subject.get("pr_number").and_then(Value::as_u64),
        }
    }
}

fn review_subject_key(row: &EventRow) -> SubjectKey {
    SubjectKey::from_subject_payload(&row.payload["subject"])
}

fn review_round_n(row: &EventRow) -> u32 {
    row.payload["n"].as_u64().expect("review n") as u32
}

fn review_round_converged(row: &EventRow) -> bool {
    row.payload["converged"]
        .as_bool()
        .expect("review converged")
}

fn merge_matches_subject(row: &EventRow, key: &SubjectKey) -> bool {
    SubjectKey::from_subject_payload(&row.payload["subject"]) == *key
}

fn row_head_sha(row: &EventRow) -> Option<String> {
    row.payload
        .get("head_sha")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

async fn assert_subject_keyed_cap_enforcement(fx: &Fixture) {
    let rounds = event_rows(&fx.repo, "review.round").await;
    let merges = event_rows(&fx.repo, "forge.pr.merged").await;
    let issue_closed = event_rows(&fx.repo, "forge.issue.closed").await;
    let lifecycle = event_rows(&fx.repo, "wave.lifecycle_changed").await;
    let ratify_resolved = event_rows(&fx.repo, "ratify.resolved").await;

    let mut max_round_by_subject: HashMap<SubjectKey, EventRow> = HashMap::new();
    for round in &rounds {
        let key = review_subject_key(round);
        let replace = max_round_by_subject
            .get(&key)
            .map(|existing| review_round_n(round) > review_round_n(existing))
            .unwrap_or(true);
        if replace {
            max_round_by_subject.insert(key, round.clone());
        }
    }

    for (key, max_round) in max_round_by_subject {
        if review_round_converged(&max_round) {
            if key.pr_number.is_some() {
                let expected = row_head_sha(&max_round).expect("converged PR review head_sha");
                for merge in merges.iter().filter(|row| merge_matches_subject(row, &key)) {
                    assert_eq!(
                        row_head_sha(merge).as_deref(),
                        Some(expected.as_str()),
                        "merge head must match latest max-n converged review for {key:?}"
                    );
                }
            }
            continue;
        }

        let later_grant = ratify_resolved.iter().find(|row| {
            row.id > max_round.id
                && row.scope_wave.as_deref() == Some(&fx.wave_id)
                && row.payload["decision"] == "grant"
        });
        let later_converged = later_grant.and_then(|grant| {
            rounds
                .iter()
                .filter(|row| {
                    row.id > grant.id
                        && review_subject_key(row) == key
                        && review_round_converged(row)
                })
                .max_by_key(|row| review_round_n(row))
        });

        if let Some(converged) = later_converged {
            let expected = row_head_sha(converged).expect("later converged review head_sha");
            for merge in merges.iter().filter(|row| merge_matches_subject(row, &key)) {
                assert_eq!(
                    row_head_sha(merge).as_deref(),
                    Some(expected.as_str()),
                    "post-ratify merge head must match intervening converged review for {key:?}"
                );
            }
            continue;
        }

        assert!(
            !merges
                .iter()
                .any(|row| row.id > max_round.id && merge_matches_subject(row, &key)),
            "unconverged max-n subject {key:?} must not merge later"
        );
        assert!(
            !issue_closed.iter().any(|row| row.id > max_round.id),
            "unconverged max-n subject {key:?} must not close an issue later"
        );
        assert!(
            !lifecycle.iter().any(|row| {
                row.id > max_round.id
                    && row.scope_wave.as_deref() == Some(&fx.wave_id)
                    && row.payload["to"] == "done"
            }),
            "unconverged max-n subject {key:?} must not reach done later"
        );
    }
}

async fn wait_for_recovered_pending(
    handle: &calm_server::harness::SpecHarness,
) -> Vec<Observation> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pending = handle.pending_queue_for_test().await;
        if pending
            .iter()
            .any(|obs| matches!(obs, Observation::ReviewRound { .. }))
            && pending
                .iter()
                .any(|obs| matches!(obs, Observation::RatifyRequested { .. }))
            && pending
                .iter()
                .any(|obs| matches!(obs, Observation::RatifyResolved { .. }))
        {
            return pending;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for recovered pending queue, last={pending:?}");
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn boot_recovery_runtime(fx: &Fixture) -> Arc<OperationRuntime> {
    let operation_repo = Arc::new(SqlxOperationRepo::new(fx.repo.pool().clone()));
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn RouteRepo> = fx.repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    Arc::new(
        OperationRuntime::new(
            operation_repo.clone(),
            vec![Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>],
            fx.events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo,
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                fx.events.clone(),
                completion,
            ),
        )
        .await
        .expect("boot recovery operation runtime"),
    )
}

fn op_id_from_response(resp: &Value) -> String {
    resp["result"]["structuredContent"]["op_id"]
        .as_str()
        .expect("MCP response op_id")
        .to_string()
}

async fn operation_phase(repo: &SqlxRepo, op_id: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_one(repo.pool())
        .await
        .expect("operation phase")
}

async fn wait_for_recovery_result(recovery: &OperationRuntime, op_id: &str) -> OperationResult {
    let owned_op_id = op_id.to_owned();
    timeout(RECOVERY_WAIT_TIMEOUT, recovery.wait(&owned_op_id))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for recovered operation {op_id} within 5s"))
        .expect("recovered op result")
}

async fn wait_for_operation_phase(repo: &SqlxRepo, op_id: &str, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let phase = operation_phase(repo, op_id).await;
        if phase == expected {
            return;
        }
        if Instant::now() > deadline {
            panic!("expected operation {op_id} phase `{expected}`, got `{phase}`");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn operation_result_path(repo: &SqlxRepo, op_id: &str) -> PathBuf {
    let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_one(repo.pool())
        .await
        .expect("operation tx_output_json");
    let output: Value = serde_json::from_str(&raw).expect("tx_output_json parses");
    PathBuf::from(
        output["data"]["result_path"]
            .as_str()
            .expect("forge result_path in tx_output"),
    )
}

async fn parked_process_group_guard(repo: &SqlxRepo, op_id: &str) -> ProcessGroupGuard {
    let raw: String =
        sqlx::query_scalar("SELECT spawn_artifacts_json FROM operations WHERE id = ?1")
            .bind(op_id)
            .fetch_one(repo.pool())
            .await
            .expect("operation spawn_artifacts_json");
    let artifacts: SpawnArtifacts = serde_json::from_str(&raw).expect("spawn artifacts json");
    ProcessGroupGuard {
        pgid: artifacts.pgid,
    }
}

struct ProcessGroupGuard {
    pgid: i32,
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.pgid > 0 {
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
    }
}

fn assert_result_files_absent(result_path: &Path) {
    let code = path_with_suffix(result_path, ".code");
    let stdout = path_with_suffix(result_path, ".stdout");
    assert!(
        !code.exists(),
        "forge result code file should not be complete before recovery: {}",
        code.display()
    );
    assert!(
        !stdout.exists(),
        "forge result stdout file should not be complete before recovery: {}",
        stdout.display()
    );
}

async fn wait_for_result_code_file(result_path: &Path) {
    let code = path_with_suffix(result_path, ".code");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if code.exists() {
            return;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out waiting for released forge result code file {}",
                code.display()
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

async fn mark_parked_artifacts_dead(repo: &SqlxRepo, op_id: &str) {
    let artifacts = SpawnArtifacts {
        pid: -424_242,
        pgid: -424_242,
        start_time: 0,
        boot_id: "dead-boot-for-e2e".into(),
        log_path: None,
        extra: json!({ "source": "forge_workflow_e2e_crash" }),
    };
    sqlx::query(
        r#"UPDATE operations
           SET spawn_artifacts_json = ?1,
               lease_owner = NULL,
               lease_until_ms = NULL,
               updated_at_ms = ?2
           WHERE id = ?3
             AND phase = 'parked'"#,
    )
    .bind(serde_json::to_string(&artifacts).expect("spawn artifacts json"))
    .bind(now_ms())
    .bind(op_id)
    .execute(repo.pool())
    .await
    .expect("mark parked artifacts dead");
}

async fn mark_workspace_lease_stale_for_boot(repo: &SqlxRepo, lease_id: &str) {
    sqlx::query(
        r#"UPDATE workspace_leases
           SET boot_id = 'stale-boot-for-forge-e2e',
               updated_at_ms = ?1
           WHERE lease_id = ?2
             AND state = 'held'"#,
    )
    .bind(now_ms())
    .bind(lease_id)
    .execute(repo.pool())
    .await
    .expect("mark workspace lease stale");
}

async fn workspace_lease_state(repo: &SqlxRepo, lease_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_one(repo.pool())
        .await
        .expect("workspace lease state")
}

async fn workspace_lease_state_optional(repo: &SqlxRepo, lease_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_optional(repo.pool())
        .await
        .expect("workspace lease state optional")
}

fn assert_tool_succeeded(resp: &Value, label: &str) {
    assert!(
        resp.get("error").is_none(),
        "{label} returned JSON-RPC error: {resp:#?}"
    );
    assert_eq!(
        resp["result"]["isError"], false,
        "{label} returned MCP tool error: {resp:#?}"
    );
    assert!(
        resp["result"]["structuredContent"]["op_id"]
            .as_str()
            .is_some(),
        "{label} response must carry op_id: {resp:#?}"
    );
}

async fn event_rows(repo: &SqlxRepo, kind: &str) -> Vec<EventRow> {
    let rows: Vec<RawEventRow> = sqlx::query_as(
        "SELECT id, scope_kind, scope_cove, scope_wave, scope_card, payload \
             FROM events WHERE kind = ?1 ORDER BY id ASC",
    )
    .bind(kind)
    .fetch_all(repo.pool())
    .await
    .expect("event rows");
    rows.into_iter()
        .map(
            |(id, scope_kind, _scope_cove, scope_wave, scope_card, payload)| EventRow {
                id,
                scope_kind,
                scope_wave,
                scope_card,
                payload: serde_json::from_str(&payload).expect("event payload json"),
            },
        )
        .collect()
}

async fn wait_for_event_count(repo: &SqlxRepo, kind: &str, expected: usize) -> Vec<EventRow> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = event_rows(repo, kind).await;
        if rows.len() == expected {
            return rows;
        }
        if Instant::now() > deadline {
            panic!("expected {expected} `{kind}` events, got {}", rows.len());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn assert_event_count_stays(repo: &SqlxRepo, kind: &str, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let rows = event_rows(repo, kind).await;
        assert_eq!(
            rows.len(),
            expected,
            "`{kind}` event count changed unexpectedly: {rows:#?}"
        );
        if Instant::now() > deadline {
            return;
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_event_matching(
    repo: &SqlxRepo,
    kind: &str,
    predicate: impl Fn(&EventRow) -> bool,
) -> EventRow {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = event_rows(repo, kind).await;
        if let Some(row) = rows.iter().find(|row| predicate(row)) {
            return row.clone();
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for matching `{kind}` event; rows: {rows:#?}");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn assert_wave_event(row: &EventRow, wave_id: &str) {
    assert_eq!(row.scope_kind, "wave");
    assert_eq!(row.scope_wave.as_deref(), Some(wave_id));
    assert!(row.scope_card.is_none());
    assert_eq!(row.payload["wave_id"], wave_id);
}

async fn transition_wave_to_done(fx: &Fixture) {
    let wave_id = WaveId::from(fx.wave_id.clone());
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: CoveId::from(fx.cove_id.clone()),
    };
    let actor = ActorId::Kernel;
    write_with_actor_events_typed::<(), _>(
        fx.repo.as_ref(),
        None,
        &fx.events,
        &fx.write,
        move |tx| {
            let wave_id = wave_id.clone();
            let scope = scope.clone();
            let actor = actor.clone();
            Box::pin(async move {
                let mut events = Vec::new();
                for (target, message) in [
                    (WaveLifecycle::Planning, "e2e planning"),
                    (WaveLifecycle::Dispatching, "e2e dispatching"),
                    (WaveLifecycle::Working, "e2e working"),
                    (WaveLifecycle::Reviewing, "e2e reviewing"),
                    (WaveLifecycle::Done, "e2e done"),
                ] {
                    if let Some(lifecycle_events) =
                        calm_server::wave_lifecycle::apply_requested_transition_in_tx(
                            tx,
                            &wave_id,
                            target,
                            &actor,
                            message.to_string(),
                        )
                        .await?
                    {
                        events.extend(
                            lifecycle_events
                                .into_iter()
                                .map(|event| (actor.clone(), scope.clone(), event)),
                        );
                    }
                }
                Ok(((), events))
            })
        },
    )
    .await
    .expect("transition wave to done");
}

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/git-forge/manifest.json")
}

fn read_manifest() -> Manifest {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");
    Manifest::parse(&raw).expect("git-forge manifest parses")
}

fn init_bare_origin(origin: &Path, seed: &Path) {
    run_git_no_cwd(["init", "--bare", path_str(origin)]);
    std::fs::create_dir_all(seed).expect("create seed repo");
    run_git(seed, ["init"]);
    run_git(
        seed,
        ["config", "user.email", "forge-workflow@example.test"],
    );
    run_git(seed, ["config", "user.name", "Forge Workflow Test"]);
    run_git(seed, ["branch", "-M", "main"]);
    std::fs::write(seed.join("README.md"), "initial\n").expect("write README");
    run_git(seed, ["add", "README.md"]);
    run_git(seed, ["commit", "-m", "initial"]);
    run_git(seed, ["remote", "add", "origin", path_str(origin)]);
    run_git(seed, ["push", "-u", "origin", "main"]);
    run_git_no_cwd([
        "--git-dir",
        path_str(origin),
        "symbolic-ref",
        "HEAD",
        "refs/heads/main",
    ]);
}

fn clone_for_wave(origin: &Path, target: &Path) {
    run_git_no_cwd(["clone", path_str(origin), path_str(target)]);
    configure_repo_identity(target);
}

fn provision_worker_worktree(repo: &Path, wave_id: &str, card_id: &str, target: &Path) {
    ensure_worktree_root_excluded(repo);
    let parent = target.parent().expect("worker worktree target parent");
    std::fs::create_dir_all(parent).expect("create worker worktree parent");
    let branch = format!("neige/{wave_id}/{card_id}");
    run_git(
        repo,
        ["worktree", "add", "-b", branch.as_str(), path_str(target)],
    );
    configure_repo_identity(target);
}

fn ensure_worktree_root_excluded(repo: &Path) {
    use std::io::Write as _;

    const WORKTREE_EXCLUDE: &str = ".claude/worktrees/";
    let exclude = run_git_capture(repo, ["rev-parse", "--git-path", "info/exclude"]);
    let exclude = repo.join(exclude);
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == WORKTREE_EXCLUDE) {
        return;
    }
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent).expect("create git exclude parent");
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude)
        .expect("open git exclude");
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file).expect("separate git exclude entries");
    }
    writeln!(file, "{WORKTREE_EXCLUDE}").expect("write worktree exclude");
}

fn configure_repo_identity(repo: &Path) {
    run_git(
        repo,
        ["config", "user.email", "forge-workflow@example.test"],
    );
    run_git(repo, ["config", "user.name", "Forge Workflow Test"]);
}

fn stage_git_change(repo: &Path, name: &str, contents: &str) {
    std::fs::write(repo.join(name), contents).expect("write git change");
    run_git(repo, ["add", name]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    run_git_inner(Some(repo), args);
}

fn run_git_no_cwd<const N: usize>(args: [&str; N]) {
    run_git_inner(None, args);
}

fn run_git_capture<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    let output = run_git_output(Some(repo), args);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git_ref_exists(repo: &Path, ref_name: &str) -> bool {
    run_git_output(Some(repo), ["show-ref", "--verify", "--quiet", ref_name])
        .status
        .success()
}

fn run_git_inner<const N: usize>(repo: Option<&Path>, args: [&str; N]) {
    let output = run_git_output(repo, args);
    assert!(
        output.status.success(),
        "git failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_output<const N: usize>(repo: Option<&Path>, args: [&str; N]) -> std::process::Output {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(repo) = repo {
        cmd.current_dir(repo);
    }
    cmd.output().expect("run git")
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("test paths are utf-8")
}

fn write_gh_shim(dir: &Path) {
    let path = dir.join("gh");
    std::fs::write(&path, GH_SHIM).expect("write gh shim");
    let mut perms = std::fs::metadata(&path)
        .expect("gh shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod gh shim");
}

fn shim_state_dir(repo: &Path) -> PathBuf {
    PathBuf::from(format!("{}.shimstate", repo.display()))
}

fn pr_is_merged(state: &Path, pr_number: u64) -> bool {
    std::fs::read_to_string(state.join("prs").join(pr_number.to_string()).join("merged"))
        .map(|raw| raw.trim() == "true")
        .unwrap_or(false)
}

struct ShimBlock {
    release_path: PathBuf,
}

impl ShimBlock {
    fn new(state: &Path, verb: &str) -> Self {
        std::fs::create_dir_all(state).expect("create shim state dir");
        let block_path = state.join(format!("block_{verb}"));
        let release_path = state.join(format!("release_{verb}"));
        let _ = std::fs::remove_file(&release_path);
        std::fs::write(&block_path, "").expect("write shim block sentinel");
        Self { release_path }
    }

    fn release(&self) {
        std::fs::write(&self.release_path, "").expect("write shim release sentinel");
    }
}

impl Drop for ShimBlock {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release_path, "");
    }
}

fn shim_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

async fn wait_for_counter(path: &Path, expected: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count = shim_counter(path);
        if count == expected {
            return;
        }
        if Instant::now() > deadline {
            panic!(
                "expected shim counter {} to reach {expected}, got {count}",
                path.display()
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn prepend_to_path(dir: &Path) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut value = OsString::from(dir.as_os_str());
    value.push(OsStr::new(":"));
    value.push(current);
    value
}

struct ForgeTestEnv {
    _path_dir: TempDir,
    _results_dir: TempDir,
    _trusted: EnvGuard,
    _results: EnvGuard,
    _path: EnvGuard,
}

fn setup_forge_env() -> ForgeTestEnv {
    let path_dir = short_tempdir("p").expect("gh shim PATH tempdir");
    write_gh_shim(path_dir.path());
    let path_value = prepend_to_path(path_dir.path());
    let results_dir = short_tempdir("r").expect("forge results tempdir");
    let trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results_dir.path());
    let path = EnvGuard::set("PATH", path_value);
    ForgeTestEnv {
        _path_dir: path_dir,
        _results_dir: results_dir,
        _trusted: trusted,
        _results: results,
        _path: path,
    }
}

fn short_tempdir(prefix: &str) -> std::io::Result<TempDir> {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("fwe");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix(prefix).tempdir_in(base)
}

fn socket_tempdir() -> std::io::Result<TempDir> {
    let base = std::env::temp_dir().join("fwe-s");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new().prefix("s").tempdir_in(base)
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(previous) => unsafe { std::env::set_var(self.key, previous) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

const GH_SHIM: &str = r#"#!/bin/sh
# Hermetic gh shim for forge_workflow_e2e.
# State is derived only from --repo so the kernel's env-cleared subprocess can
# replay probes without test-only variables. The merge command is idempotent:
# repeated merges for the same PR return the original recorded merge oid.

get_arg() {
  wanted=$1
  shift
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "$wanted" ]; then
      shift
      if [ "$#" -gt 0 ]; then
        printf '%s\n' "$1"
        return 0
      fi
      return 1
    fi
    shift
  done
  return 1
}

state_dir_for() {
  printf '%s.shimstate\n' "$1"
}

ensure_state() {
  repo=$1
  state=$(state_dir_for "$repo")
  mkdir -p "$state/prs" "$state/issues"
  printf '%s\n' "$state"
}

inc_counter() {
  file=$1
  if [ -f "$file" ]; then
    count=$(cat "$file")
  else
    count=0
  fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$file"
}

block_if_requested() {
  state=$1
  verb=$2
  block="$state/block_$verb"
  release="$state/release_$verb"
  [ -f "$block" ] || return 0
  i=0
  while [ "$i" -lt 200 ]; do
    [ -f "$release" ] && return 0
    # CI uses GNU coreutils; keep a real delay if fractional sleep is unavailable.
    sleep 0.1 2>/dev/null || sleep 1
    i=$((i + 1))
  done
  return 0
}

find_pr() {
  selector=$1
  state=$2
  for pr_dir in "$state"/prs/*; do
    [ -d "$pr_dir" ] || continue
    number=$(cat "$pr_dir/number")
    head=$(cat "$pr_dir/head")
    if [ "$selector" = "$number" ] || [ "$selector" = "$head" ]; then
      printf '%s\n' "$pr_dir"
      return 0
    fi
  done
  return 1
}

find_pr_by_head() {
  wanted_head=$1
  state=$2
  for pr_dir in "$state"/prs/*; do
    [ -d "$pr_dir" ] || continue
    head=$(cat "$pr_dir/head")
    if [ "$wanted_head" = "$head" ]; then
      printf '%s\n' "$pr_dir"
      return 0
    fi
  done
  return 1
}

print_pr_json() {
  pr_dir=$1
  number=$(cat "$pr_dir/number")
  head_sha=$(cat "$pr_dir/headRefOid")
  printf '{"number":%s,"headRefOid":"%s"}\n' "$number" "$head_sha"
}

[ "$#" -ge 2 ] || {
  echo "unsupported gh invocation" >&2
  exit 2
}

area=$1
verb=$2
shift 2

case "$area:$verb" in
  pr:list)
    repo=$(get_arg --repo "$@") || exit 2
    base=$(get_arg --base "$@" || true)
    head=$(get_arg --head "$@" || true)
    state=$(ensure_state "$repo")
    printf '['
    sep=
    for pr_dir in "$state"/prs/*; do
      [ -d "$pr_dir" ] || continue
      merged=$(cat "$pr_dir/merged")
      pr_base=$(cat "$pr_dir/base")
      pr_head=$(cat "$pr_dir/head")
      if [ "$merged" = "true" ]; then
        continue
      fi
      if [ -n "$base" ] && [ "$base" != "$pr_base" ]; then
        continue
      fi
      if [ -n "$head" ] && [ "$head" != "$pr_head" ]; then
        continue
      fi
      number=$(cat "$pr_dir/number")
      printf '%s%s' "$sep" "$number"
      sep=,
    done
    printf ']\n'
    ;;
  pr:create)
    repo=$(get_arg --repo "$@") || exit 2
    head=$(get_arg --head "$@") || exit 2
    base=$(get_arg --base "$@") || exit 2
    state=$(ensure_state "$repo")
    if pr_dir=$(find_pr_by_head "$head" "$state"); then
      print_pr_json "$pr_dir"
      exit 0
    fi
    next_file="$state/next_pr"
    if [ -f "$next_file" ]; then
      number=$(cat "$next_file")
    else
      number=1
    fi
    next=$((number + 1))
    printf '%s\n' "$next" > "$next_file"
    head_sha=$(git --git-dir "$repo" rev-parse "$head")
    pr_dir="$state/prs/$number"
    mkdir -p "$pr_dir"
    printf '%s\n' "$number" > "$pr_dir/number"
    printf '%s\n' "$head" > "$pr_dir/head"
    printf '%s\n' "$base" > "$pr_dir/base"
    printf '%s\n' "$head_sha" > "$pr_dir/headRefOid"
    printf 'false\n' > "$pr_dir/merged"
    print_pr_json "$pr_dir"
    ;;
  pr:diff)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    base=$(cat "$pr_dir/base")
    head=$(cat "$pr_dir/head")
    patch_file="$state/diff.$$"
    if git --git-dir "$repo" diff --patch "$base...$head" > "$patch_file" && [ -s "$patch_file" ]; then
      cat "$patch_file"
    else
      printf 'diff --git a/feature.txt b/feature.txt\n'
      printf 'new file mode 100644\n'
      printf '--- /dev/null\n'
      printf '+++ b/feature.txt\n'
      printf '@@ -0,0 +1 @@\n'
      printf '+hello from e2e\n'
    fi
    rm -f "$patch_file"
    ;;
  pr:view)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    json_fields=$(get_arg --json "$@" || true)
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    number=$(cat "$pr_dir/number")
    head_sha=$(cat "$pr_dir/headRefOid")
    merged=$(cat "$pr_dir/merged")
    case "$json_fields" in
      state)
        if [ "$merged" = "true" ]; then
          printf '{"state":"MERGED"}\n'
        else
          printf '{"state":"OPEN"}\n'
        fi
        ;;
      number,headRefOid)
        printf '{"number":%s,"headRefOid":"%s"}\n' "$number" "$head_sha"
        ;;
      headRefOid,mergeCommit)
        if [ "$merged" = "true" ]; then
          merge_sha=$(cat "$pr_dir/merge_sha")
          printf '{"headRefOid":"%s","mergeCommit":{"oid":"%s"}}\n' "$head_sha" "$merge_sha"
        else
          printf '{"headRefOid":"%s","mergeCommit":null}\n' "$head_sha"
        fi
        ;;
      statusCheckRollup)
        printf '{"conclusion":"success"}\n'
        ;;
      *)
        echo "unsupported gh pr view --json $json_fields" >&2
        exit 2
        ;;
    esac
    ;;
  pr:merge)
    [ "$#" -ge 1 ] || exit 2
    selector=$1
    repo=$(get_arg --repo "$@") || exit 2
    expected_head=$(get_arg --match-head-commit "$@" || true)
    state=$(ensure_state "$repo")
    pr_dir=$(find_pr "$selector" "$state") || exit 1
    head_sha=$(cat "$pr_dir/headRefOid")
    if [ -n "$expected_head" ] && [ "$expected_head" != "$head_sha" ]; then
      echo "head commit did not match" >&2
      exit 1
    fi
    if [ "$(cat "$pr_dir/merged")" = "true" ]; then
      merge_sha=$(cat "$pr_dir/merge_sha")
    else
      number=$(cat "$pr_dir/number")
      merge_sha=$(printf '%s' "merge:$number:$head_sha" | git hash-object --stdin)
      printf '%s\n' "$merge_sha" > "$pr_dir/merge_sha"
      printf 'true\n' > "$pr_dir/merged"
      inc_counter "$state/pr_merge_count"
      block_if_requested "$state" pr_merge
    fi
    printf '{"headRefOid":"%s","mergeCommit":{"oid":"%s"}}\n' "$head_sha" "$merge_sha"
    ;;
  issue:view)
    [ "$#" -ge 1 ] || exit 2
    issue=$1
    repo=$(get_arg --repo "$@") || exit 2
    json_fields=$(get_arg --json "$@" || true)
    jq_expr=$(get_arg --jq "$@" || true)
    state=$(ensure_state "$repo")
    issue_state=OPEN
    if [ -f "$state/issues/$issue.closed" ]; then
      issue_state=CLOSED
    fi
    case "$json_fields:$jq_expr" in
      state:*)
        printf '{"state":"%s"}\n' "$issue_state"
        ;;
      body:.body)
        printf '# Issue %s\n\nFake issue body for issue-development ingestion.\n' "$issue"
        ;;
      *)
        echo "unsupported gh issue view --json $json_fields --jq $jq_expr" >&2
        exit 2
        ;;
    esac
    ;;
  issue:close)
    [ "$#" -ge 1 ] || exit 2
    issue=$1
    repo=$(get_arg --repo "$@") || exit 2
    state=$(ensure_state "$repo")
    if [ ! -f "$state/issues/$issue.closed" ]; then
      printf 'closed\n' > "$state/issues/$issue.closed"
      inc_counter "$state/issue_close_count"
      block_if_requested "$state" issue_close
    fi
    printf 'closed issue %s\n' "$issue"
    ;;
  *)
    echo "unsupported gh invocation: $area $verb" >&2
    exit 2
    ;;
esac
"#;
