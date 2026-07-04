//! Real Codex forge E2E for issue #760.
//!
//! Feature-gated behind `codex-e2e` and self-skipping when no real Codex
//! binary is available. The test keeps GitHub fake via a local `gh` shim, but
//! runs a real local Codex app-server and a real Codex worker against a local
//! bare git origin. The worker must write a small file on its leased worktree;
//! the kernel must then commit that leased worktree and emit
//! `worktree.committed`.

#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

use std::path::PathBuf;

use calm_server::ids::ActorId;
use serde_json::{Value, json};
use support::codex_fixture::*;
use support::event_queries::*;
use support::forge_env::FORGE_ENV_LOCK;
use support::git_helpers::*;
use support::spec_turn::*;

#[tokio::test]
async fn real_codex_worker_writes_code_on_leased_worktree() {
    // This asserts the real-worker-writes-code integration seam and the #834
    // kernel-owned deterministic commit path.
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let fx = match boot_real_codex_worker_fixture(codex_bin).await {
        Ok(fx) => fx,
        Err(reason) => {
            skip!("{reason}");
        }
    };

    let _dispatcher = spawn_dispatcher(&fx);
    let goal = forge_goal();
    plan_codex_task(&fx, TASK_KEY, &goal).await;

    let budget = e2e_budget();
    let task_id = task_id(&fx, TASK_KEY);
    let worker = wait_for_worker_success(&fx, &task_id, budget).await;
    let output = worker
        .tx_output
        .as_ref()
        .expect("codex-worker tx_output persisted");
    let worker_cwd = PathBuf::from(output_string(output, "cwd"));
    let worker_card_id = output_string(output, "card_id");

    // The codex-worker op reaches `succeeded` at turn-START (it only awaits the
    // initial TurnStarted), not worker-done. The real worker writes the file and
    // reports `task.complete` tens of seconds later, after which the kernel
    // commits and emits `worktree.committed`. So wait on that commit event (the
    // true end-to-end completion barrier) BEFORE asserting working-tree state,
    // otherwise the marker check races the async worker turn.
    assert_worker_commit_landed(&fx, &worker_cwd, &worker_card_id, budget).await;
    assert_worker_wrote_marker_file(&fx, &worker_cwd).await;

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

#[tokio::test]
async fn real_codex_worker_opens_pr_after_committing_on_leased_worktree() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let fx = match boot_real_codex_worker_fixture(codex_bin).await {
        Ok(fx) => fx,
        Err(reason) => {
            skip!("{reason}");
        }
    };

    let _dispatcher = spawn_dispatcher(&fx);
    let repo_gitdir = fx.wave_cwd.join(".git").display().to_string();
    // R-d1: this is the first test where a real worker must DISCOVER+CALL
    // annotation-less plugin forge tools (forge plugin tools are published with
    // empty schema/annotations). If the worker fails to call them, that is a
    // genuine finding (forge tool descriptors may need annotations), not to be
    // worked around by scripting the call.
    let goal = forge_pr_goal(&repo_gitdir);
    plan_codex_task(&fx, TASK_KEY, &goal).await;

    let budget = e2e_budget();
    let task_id = task_id(&fx, TASK_KEY);
    let worker = wait_for_worker_success(&fx, &task_id, budget).await;
    let output = worker
        .tx_output
        .as_ref()
        .expect("codex-worker tx_output persisted");
    let worker_cwd = PathBuf::from(output_string(output, "cwd"));
    let worker_card_id = output_string(output, "card_id");

    // This file issues NO scripted `call_tool` for any `gh.*` or `git.commit`
    // (the only direct tool call in the whole file is `TOOL_PLAN_UPSERT` via
    // the spec identity inside `plan_codex_task`). Therefore the ONLY thing
    // that can emit `forge.pr.opened` / `forge.pr.checks` is the real worker's
    // own MCP `tools/call`. Assert via the `events` table (NOT
    // `harness_items`, which is spec-thread-only).
    let (s5_id, s5) = wait_for_first_worktree_committed_event(&fx, &task_id, budget).await;
    assert_eq!(s5.actor, ActorId::KernelDispatcher);
    assert_eq!(s5.scope_kind, "card");
    assert_eq!(s5.scope_wave.as_deref(), Some(fx.wave_id.as_str()));
    assert_eq!(s5.scope_card.as_deref(), Some(worker_card_id.as_str()));
    assert_eq!(
        s5.payload["branch"],
        format!("neige/{}/{}", fx.wave_id.as_str(), worker_card_id)
    );
    let head = git_stdout(&worker_cwd, ["rev-parse", "HEAD"]);
    assert!(
        is_hex_sha(&head),
        "worker worktree HEAD should be a 40-char hex sha, got {head:?}"
    );
    assert_eq!(s5.payload["commit_sha"], head);

    let (s6_id, s6_wave, s6) = wait_for_first_forge_event(&fx, "forge.pr.opened", budget).await;
    assert_eq!(s6_wave.as_deref(), Some(fx.wave_id.as_str()));
    assert_eq!(s6["head_sha"], head);
    let pr_number = s6["pr_number"]
        .as_u64()
        .unwrap_or_else(|| panic!("forge.pr.opened missing pr_number: {s6}"));
    assert!(pr_number >= 1, "PR number must be >= 1, got {pr_number}");

    let (s7_id, s7_wave, s7) = wait_for_first_forge_event(&fx, "forge.pr.checks", budget).await;
    assert_eq!(s7_wave.as_deref(), Some(fx.wave_id.as_str()));
    assert_eq!(s7["pr_number"].as_u64(), Some(pr_number));
    assert_eq!(s7["conclusion"], "success");

    let task_completed_id = wait_for_task_completed_id(&fx, budget).await;

    // Enforce that the worker performed git.commit/gh.pr.create/gh.pr.checks
    // in-turn BEFORE calm.task.complete (construction W), preventing a
    // false-pass where the worker completes early then opens the PR afterward.
    assert!(
        s5_id < s6_id && s6_id < s7_id && s7_id < task_completed_id,
        "expected S5 < S6 < S7 < task.completed, got S5={s5_id}, S6={s6_id}, S7={s7_id}, task.completed={task_completed_id}"
    );
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);
    assert_eq!(
        event_payloads(&fx.repo, "forge.issue.closed").await.len(),
        0
    );

    let marker_at_head = git_stdout(&worker_cwd, ["show", "HEAD:FORGE_E2E.md"]);
    assert_eq!(
        marker_at_head.trim(),
        "forge-e2e-ok",
        "FORGE_E2E.md content at committed HEAD mismatch"
    );

    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

#[tokio::test]
async fn real_spec_agent_autonomously_plans_from_bound_workflow() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let goal =
        "Plan the smallest issue-development workflow for adding one marker file.".to_string();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            workflow_id: Some("issue-development".into()),
            plan_source: PlanSource::RealSpecTurn,
            issue_body: None,
            mint_report_card: false,
            require_task_gates: false,
        },
        codex_bin,
    )
    .await
    {
        Ok(fx) => fx,
        Err(reason) => {
            skip!("{reason}");
        }
    };

    boot_spec_harness_via_start_op(&fx, goal).await;

    let (actor, plan) = wait_for_plan_updated(&fx, spec_planning_budget()).await;
    assert!(
        matches!(actor, ActorId::AiSpecSession(_)),
        "plan.updated actor must be AiSpecSession, got {actor:?}"
    );
    assert!(
        plan["changed_keys"]
            .as_array()
            .is_some_and(|keys| !keys.is_empty()),
        "plan.updated changed_keys must be non-empty: {plan}",
    );
    // These preconditions prove `bound_workflow_descriptor` resolves the
    // trusted bound workflow instead of falling back to the vanilla spec prompt.
    assert_bound_issue_development_workflow_preconditions(&fx).await;

    // Superset-tolerant (design §1/§2): assert the kernel-deterministic
    // Draft->Planning companion is present exactly once; the real spec may emit
    // further lifecycle transitions (e.g. Planning->Dispatching) once it plans,
    // so we filter rather than assert the total count.
    let lifecycle = lifecycle_changed_rows(&fx.repo).await;
    let draft_to_planning: Vec<&(ActorId, Value)> = lifecycle
        .iter()
        .filter(|(_, payload)| {
            payload["from"] == json!("draft") && payload["to"] == json!("planning")
        })
        .collect();
    assert_eq!(
        draft_to_planning.len(),
        1,
        "expected exactly one wave.lifecycle_changed draft->planning, got {lifecycle:?}"
    );
    let (lifecycle_actor, lifecycle_payload) = draft_to_planning[0];
    assert_eq!(
        lifecycle_actor,
        &ActorId::Kernel,
        "draft->planning companion actor must be Kernel"
    );
    assert_eq!(lifecycle_payload["id"], json!(fx.wave_id.as_str()));
    assert!(
        !fx.used_injected_plan(),
        "RealSpecTurn must not use injected plan path"
    );

    shutdown_spec_harness_if_registered(&fx).await;
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}
