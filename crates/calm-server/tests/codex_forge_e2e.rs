//! Real Codex forge E2E; feature-gated behind `codex-e2e` and self-skipping when no real Codex binary is available.

#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

#[path = "cases/codex_builtin_discovery.rs"]
mod codex_builtin_discovery;

#[path = "cases/codex_calendar_preview.rs"]
mod codex_calendar_preview;

#[path = "codex_forge_e2e/capstone.rs"]
mod capstone;

#[path = "codex_forge_e2e/deterministic_gates.rs"]
mod deterministic_gates;

#[path = "codex_forge_e2e/scenario_support.rs"]
mod scenario_support;

use std::path::PathBuf;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::harness::Observation;
use calm_server::ids::ActorId;
use capstone::*;
use scenario_support::*;
use serde_json::{Value, json};
use support::agent_diag::panic_with_agent_diag;
use support::codex_fixture::*;
use support::event_queries::*;
use support::forge_env::FORGE_ENV_LOCK;
use support::git_helpers::*;
use support::mcp::call_tool_via_socket;
use support::planner_turn::*;
use tokio::time::{Instant, sleep};

const PR_CREATE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.create";
const PR_CHECKS_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.checks";
/// The d2 test's source issue. Purely an environment fact: the gh shim keeps
/// per-repo issue state keyed by number, and any number works.
const D2_ISSUE_NUMBER: u64 = 840;

#[tokio::test]
async fn real_codex_worker_writes_code_on_leased_worktree() {
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

    // The codex-worker op reaches `succeeded` at turn-START, so wait on `worktree.committed` before asserting working-tree state.
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
    let repo_gitdir = fx.track_cwd.join(".git").display().to_string();
    // The worker must DISCOVER and CALL the annotation-less forge tools itself; a failure here is a genuine finding, not to be scripted around.
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

    // Nothing in this fixture scripts `gh.*` or `git.commit`, so only the real worker's own `tools/call` can emit `forge.pr.opened` / `forge.pr.checks`.
    let (s5_id, s5) = wait_for_first_worktree_committed_event(&fx, &task_id, budget).await;
    assert_eq!(s5.actor, ActorId::KernelDispatcher);
    assert_eq!(s5.scope_kind, "card");
    assert_eq!(s5.scope_track.as_deref(), Some(fx.track_id.as_str()));
    assert_eq!(s5.scope_card.as_deref(), Some(worker_card_id.as_str()));
    assert_eq!(
        s5.payload["branch"],
        format!("neige/track-{}", fx.track_id.as_str())
    );
    let head = git_stdout(&worker_cwd, ["rev-parse", "HEAD"]);
    assert!(
        is_hex_sha(&head),
        "worker worktree HEAD should be a 40-char hex sha, got {head:?}"
    );
    assert_eq!(s5.payload["commit_sha"], head);

    let (s6_id, s6_track, s6) = wait_for_first_forge_event(&fx, "forge.pr.opened", budget).await;
    assert_eq!(s6_track.as_deref(), Some(fx.track_id.as_str()));
    assert_eq!(s6["head_sha"], head);
    let pr_number = s6["pr_number"]
        .as_u64()
        .unwrap_or_else(|| panic!("forge.pr.opened missing pr_number: {s6}"));
    assert!(pr_number >= 1, "PR number must be >= 1, got {pr_number}");

    let (s7_id, s7_track, s7) = wait_for_first_forge_event(&fx, "forge.pr.checks", budget).await;
    assert_eq!(s7_track.as_deref(), Some(fx.track_id.as_str()));
    assert_eq!(s7["pr_number"].as_u64(), Some(pr_number));
    assert_eq!(s7["conclusion"], "success");

    let task_completed_id = wait_for_task_completed_id(&fx, budget).await;

    // The worker must commit/open/check in-turn BEFORE `calm.task.complete`.
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

/// The plan test's source issue; the gh shim serves it from the seeded body.
const PLAN_ISSUE_NUMBER: u64 = 840;
const PLAN_ISSUE_BODY: &str = "Add a file `FORGE_E2E.md` at the repository root whose only line is \
`forge-e2e-ok`.\n";

#[tokio::test]
async fn real_planner_agent_autonomously_plans_from_bound_template() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: None,
            bound_issue: Some(PLAN_ISSUE_NUMBER),
            plan_source: PlanSource::RealPlannerTurn,
            issue_body: Some(FixtureIssue {
                number: PLAN_ISSUE_NUMBER,
                body: PLAN_ISSUE_BODY.into(),
            }),
            require_task_gates: false,
            repo_seed: RepoSeed::ReadmeOnly,
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
    assert_planner_prompt_binds_issue_development(&fx, PLAN_ISSUE_NUMBER).await;

    let repo_arg = fx.origin_repo.display().to_string();
    let goal = format!(
        "Plan the bound issue-development template's work for issue #{PLAN_ISSUE_NUMBER}. \
         {}",
        gh_selector_fact(&repo_arg)
    );
    boot_planner_harness_via_start_op(&fx, goal).await;

    let (actor, plan) = wait_for_plan_updated(&fx, planner_planning_budget()).await;
    assert!(
        matches!(actor, ActorId::AiPlannerSession(_)),
        "plan.updated actor must be AiPlannerSession, got {actor:?}"
    );
    assert!(
        plan["changed_keys"]
            .as_array()
            .is_some_and(|keys| !keys.is_empty()),
        "plan.updated changed_keys must be non-empty: {plan}",
    );
    assert!(
        !fx.used_injected_plan(),
        "RealPlannerTurn must not use injected plan path"
    );

    shutdown_planner_harness_if_registered(&fx).await;
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

// The only possible emitter of `forge.pr.merged` / `forge.issue.closed` is the real planner's own `tools/call`: scripted setup stops at `gh.pr.create`/`gh.pr.checks`.
// The op idem-key checks pin the caller card only; scripted setup uses the same planner thread, so they cannot discriminate scripted-vs-autonomous.
#[tokio::test]
async fn real_planner_agent_autonomously_merges_pr_and_closes_issue_from_descriptor() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // The goal carries environment facts only; pr_number and head_sha must reach the planner ONLY via observations. It flows through the planner-harness start op because `FixtureSpec.goal` is never read by that path.
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: None,
            bound_issue: Some(D2_ISSUE_NUMBER),
            plan_source: PlanSource::RealPlannerTurn,
            issue_body: None,
            require_task_gates: false,
            repo_seed: RepoSeed::ReadmeOnly,
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
    assert_planner_prompt_binds_issue_development(&fx, D2_ISSUE_NUMBER).await;
    let repo_arg = fx.origin_repo.display().to_string();
    let goal = merge_close_goal(&repo_arg, D2_ISSUE_NUMBER);

    boot_planner_harness_via_start_op(&fx, goal).await;

    let (plan_actor, _plan) = wait_for_plan_updated(&fx, planner_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiPlannerSession(_)),
        "plan.updated actor must be the real planner session, got {plan_actor:?}"
    );

    let harness = recover_planner_harness(&fx)
        .await
        .expect("live planner harness");
    // Settle the planning turn before setup/seeding so the merge+close is causally a response to the injected observations.
    wait_for_planner_turn_settled(&fx, &harness, planner_planning_budget()).await;

    // Scripted REAL PR setup (setup, not the proof): raw git branch + push, then scripted `gh.pr.create` + `gh.pr.checks` through the daemon socket so genuine events back the injected observations.
    let branch = "neige-d2-impl-slice";
    run_git(&fx.track_cwd, ["checkout", "-B", branch, "origin/main"]);
    stage_git_change(&fx.track_cwd, "FORGE_E2E_D2.md", "forge-e2e-d2\n");
    run_git(&fx.track_cwd, ["commit", "-m", "d2 scripted impl commit"]);
    let head_sha = run_git_capture(&fx.track_cwd, ["rev-parse", "HEAD"]);
    assert!(
        is_hex_sha(&head_sha),
        "scripted branch tip should be a 40-char hex sha, got {head_sha:?}"
    );
    run_git(&fx.track_cwd, ["push", "-u", "origin", branch]);
    run_git(&fx.track_cwd, ["checkout", "main"]);

    let planner_thread_id = planner_session_thread_id(&fx).await;
    let create_resp = call_tool_via_socket(
        &fx.socket_path,
        &fx.daemon_token,
        &planner_thread_id,
        201,
        PR_CREATE_TOOL,
        json!({
            "repo": repo_arg,
            "head": branch,
            "base": "main",
            "title": "d2 scripted impl PR",
            "body": "Scripted setup PR for the #840 d2 merge E2E"
        }),
    )
    .await;
    assert_forge_tool_accepted(&create_resp, "gh.pr.create");
    let (opened_id, _, opened) = wait_for_track_forge_event(
        &fx,
        "forge.pr.opened",
        0,
        review_budget(),
        "scripted setup PR",
        |payload| payload["head_sha"] == json!(head_sha),
    )
    .await;
    let pr_number = opened["pr_number"]
        .as_u64()
        .unwrap_or_else(|| panic!("forge.pr.opened missing pr_number: {opened}"));

    let checks_resp = call_tool_via_socket(
        &fx.socket_path,
        &fx.daemon_token,
        &planner_thread_id,
        202,
        PR_CHECKS_TOOL,
        json!({ "repo": repo_arg, "pr": pr_number }),
    )
    .await;
    assert_forge_tool_accepted(&checks_resp, "gh.pr.checks");
    let (checks_id, _, _checks) = wait_for_track_forge_event(
        &fx,
        "forge.pr.checks",
        opened_id,
        review_budget(),
        "scripted setup checks",
        |payload| {
            payload["pr_number"] == json!(pr_number) && payload["conclusion"] == json!("success")
        },
    )
    .await;

    // Seed the completed pipeline (dispatched + completed pairs, runs/
    // pre-check each) so runs/ shows implement/open-pr/review-pr done.
    seed_completed_task_pair(
        &fx,
        "implement-change",
        json!({ "summary": "completed" }),
        "completed",
    )
    .await;
    seed_completed_task_pair(
        &fx,
        "open-pr",
        json!({ "summary": "completed" }),
        "completed",
    )
    .await;
    // The review result reports the REAL branch tip it read; it is the planner's ONLY channel for head_sha.
    let review_result =
        json!({ "summary": "approved", "verdict": "approved", "head_sha": head_sha });
    seed_completed_task_pair(&fx, "review-pr", review_result.clone(), "approved").await;
    let review_completed_id = latest_event_id_of_kind(&fx, "task.completed").await;

    let floor = max_event_id(&fx.repo).await;
    // Proof-validity guard: nothing merged yet.
    assert_eq!(
        event_payloads(&fx.repo, "forge.pr.merged").await.len(),
        0,
        "proof-validity guard: no forge.pr.merged may exist pre-wake"
    );

    // Wake: inject what the prod dispatcher would push.
    for key in ["implement-change", "open-pr"] {
        inject_observation(
            &harness,
            Observation::TaskCompleted {
                idempotency_key: task_id(&fx, key),
                result: json!({ "summary": "completed" }),
            },
        )
        .await;
    }
    inject_observation(
        &harness,
        Observation::TaskCompleted {
            idempotency_key: task_id(&fx, "review-pr"),
            result: review_result,
        },
    )
    .await;
    inject_observation(
        &harness,
        Observation::ForgePrOpened {
            track_id: fx.track_id.clone(),
            pr_number,
        },
    )
    .await;
    inject_observation(
        &harness,
        Observation::ForgePrChecks {
            track_id: fx.track_id.clone(),
            pr_number,
            conclusion: "success".into(),
        },
    )
    .await;

    // Oracle (a): the merge event. All forge.* events are appended by the kernel as KernelDispatcher, so the event actor cannot attribute the seat.
    let (merged_id, merged_actor, merged) = wait_for_track_forge_event(
        &fx,
        "forge.pr.merged",
        floor,
        review_budget(),
        "planner-initiated merge",
        |_| true,
    )
    .await;
    assert_eq!(
        merged_actor,
        ActorId::KernelDispatcher,
        "forge.pr.merged is kernel-appended: {merged}"
    );
    assert_eq!(
        merged["head_sha"],
        json!(head_sha),
        "merged head must equal the reviewed head_sha == real branch tip: {merged}"
    );
    let merge_sha = merged["merge_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("forge.pr.merged missing merge_sha: {merged}"));
    assert!(
        is_hex_sha(merge_sha),
        "merge_sha should be a git-shaped oid: {merged}"
    );
    assert_eq!(
        merged["subject"]["pr_number"],
        json!(pr_number),
        "merged subject pr: {merged}"
    );

    // Oracle (b) — the head fence: the plugin idem is `gh.pr.merge:{repo}:{pr}:{expected_head_sha}` only when expected_head_sha was passed; an omitted-sha merge MUST fail this.
    let expected_merge_key = format!(
        "{PLUGIN_ID}:{}:{}:gh.pr.merge:{}:{}:{}",
        fx.track_id.as_str(),
        fx.planner_card_id.as_str(),
        repo_arg,
        pr_number,
        head_sha
    );
    let merge_keys = forge_action_idem_keys_containing(&fx, ":gh.pr.merge:").await;
    assert!(
        !merge_keys.is_empty(),
        "expected a parked forge-action gh.pr.merge operation row"
    );
    for key in &merge_keys {
        assert_eq!(
            key, &expected_merge_key,
            "every gh.pr.merge forge-action op must carry the with-sha idempotency key: {merge_keys:?}"
        );
    }

    // Oracle (c) — ordering: the approving review and the checks event precede the merge.
    assert!(
        review_completed_id < merged_id,
        "review task.completed (id={review_completed_id}) must precede forge.pr.merged (id={merged_id})"
    );
    assert!(
        checks_id < merged_id,
        "forge.pr.checks (id={checks_id}) must precede forge.pr.merged (id={merged_id})"
    );

    // Oracle (d): the issue close FOLLOWS the merge, on the right issue.
    let (closed_id, closed_actor, closed) = wait_for_track_forge_event(
        &fx,
        "forge.issue.closed",
        floor,
        review_budget(),
        "planner-initiated issue close",
        |_| true,
    )
    .await;
    assert_eq!(
        closed_actor,
        ActorId::KernelDispatcher,
        "forge.issue.closed is kernel-appended: {closed}"
    );
    assert!(
        merged_id < closed_id,
        "forge.pr.merged (id={merged_id}) must precede forge.issue.closed (id={closed_id})"
    );
    assert_eq!(
        closed["issue_number"],
        json!(D2_ISSUE_NUMBER),
        "closed issue must match the goal's issue number: {closed}"
    );
    let expected_close_key = format!(
        "{PLUGIN_ID}:{}:{}:gh.issue.close:{}:{}",
        fx.track_id.as_str(),
        fx.planner_card_id.as_str(),
        repo_arg,
        D2_ISSUE_NUMBER
    );
    let close_keys = forge_action_idem_keys_containing(&fx, ":gh.issue.close:").await;
    assert!(
        !close_keys.is_empty(),
        "expected a parked forge-action gh.issue.close operation row"
    );
    for key in &close_keys {
        assert_eq!(
            key, &expected_close_key,
            "every gh.issue.close forge-action op must target the goal issue from the planner seat: {close_keys:?}"
        );
    }

    // Exactly-once events (a planner retry with the same args dedups on the
    // parked idempotent op; a differently-keyed retry already failed above).
    assert_eq!(
        event_payloads(&fx.repo, "forge.pr.merged").await.len(),
        1,
        "exactly one forge.pr.merged event"
    );
    assert_eq!(
        event_payloads(&fx.repo, "forge.issue.closed").await.len(),
        1,
        "exactly one forge.issue.closed event"
    );

    // Oracle (e): shim counters — the remote side effect happened exactly once.
    let shim_state = PathBuf::from(format!("{repo_arg}.shimstate"));
    assert_eq!(
        shim_counter(&shim_state.join("pr_merge_count")),
        1,
        "gh shim must record exactly one real merge"
    );
    assert_eq!(
        shim_counter(&shim_state.join("issue_close_count")),
        1,
        "gh shim must record exactly one real issue close"
    );

    // Oracle (f): purity — no ratification grant, the plan must be the planner's own.
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "happy-path merge run must not request ratification"
    );
    assert!(
        !fx.used_injected_plan(),
        "RealPlannerTurn must not use injected plan path"
    );

    shutdown_planner_harness_if_registered(&fx).await;
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

// CAPSTONE: one REAL run of the full issue→PR→merge→close backbone with a LIVE dispatcher — zero injected observations, zero seeded rows; ANY `ratify.requested` fails the test.
// Real runs happen ONLY inside the isolation wrapper, never on the shared production box; without NEIGE_CODEX_BIN this self-skips.

#[tokio::test]
async fn real_planner_drives_issue_to_close_capstone() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: None,
            bound_issue: Some(CAPSTONE_ISSUE_NUMBER),
            plan_source: PlanSource::RealPlannerTurn,
            issue_body: Some(FixtureIssue {
                number: CAPSTONE_ISSUE_NUMBER,
                body: CAPSTONE_ISSUE_BODY.into(),
            }),
            require_task_gates: true,
            repo_seed: RepoSeed::RustMicroCrate,
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

    assert_planner_prompt_binds_issue_development(&fx, CAPSTONE_ISSUE_NUMBER).await;
    let dispatcher = spawn_dispatcher_with_harness(&fx);

    let repo_gitdir = fx.track_cwd.join(".git").display().to_string();
    let goal = capstone_goal(&repo_gitdir, CAPSTONE_ISSUE_NUMBER, &fx.origin_main_initial);
    boot_planner_harness_via_start_op(&fx, goal).await;

    // Every stage gets its own ≥480s floor under one overall NEIGE_CAPSTONE_BUDGET deadline (default 3600s).
    let overall_deadline = Instant::now() + capstone_budget();
    let st = move || capstone_stage_budget().min(remaining(overall_deadline));

    // S1 — the real planner plans.
    let (plan_actor, _plan) = wait_for_plan_updated(
        &fx,
        planner_planning_budget().min(remaining(overall_deadline)),
    )
    .await;
    assert!(
        matches!(plan_actor, ActorId::AiPlannerSession(_)),
        "plan.updated actor must be the real planner session, got {plan_actor:?}"
    );

    // S0 — the inspect worker reads the SEEDED issue body through the real
    // plugin lowering (forge.issue.read + artifact).
    let (_issue_read_id, issue_read_actor, issue_read) = wait_capstone_event(
        &fx,
        "forge.issue.read",
        0,
        st(),
        "inspect worker reads the source issue",
        |p| p["issue_number"] == json!(CAPSTONE_ISSUE_NUMBER),
    )
    .await;
    assert_eq!(
        issue_read_actor,
        ActorId::KernelDispatcher,
        "forge events are kernel-appended: {issue_read}"
    );
    let issue_artifact_path = issue_read["artifact_path"]
        .as_str()
        .unwrap_or_else(|| panic!("forge.issue.read missing artifact_path: {issue_read}"));
    let issue_artifact = std::fs::read_to_string(issue_artifact_path)
        .unwrap_or_else(|e| panic!("read issue artifact {issue_artifact_path}: {e}"));
    assert_eq!(
        issue_artifact.trim(),
        CAPSTONE_ISSUE_BODY.trim(),
        "issue read artifact must carry the shim-seeded fixture body (S0)"
    );

    // S5 — the kernel commits the implement worker's leased worktree.
    let (commit_id, commit_actor, committed) = wait_capstone_event(
        &fx,
        "worktree.committed",
        0,
        st(),
        "kernel commits the implement worker's worktree",
        |_| true,
    )
    .await;
    assert_eq!(
        commit_actor,
        ActorId::KernelDispatcher,
        "worktree.committed is kernel-emitted: {committed}"
    );

    // S5.5 — the env-cleared rustc gate runs and passes, strictly after the
    // commit (verifying → task.gate_result on the implement task).
    let (_gate_id, gate_actor, gate) = wait_capstone_event(
        &fx,
        "task.gate_result",
        commit_id,
        st(),
        "post-commit task-verify gate passes",
        |p| p["passed"] == json!(true),
    )
    .await;
    assert_eq!(
        gate_actor,
        ActorId::KernelDispatcher,
        "task.gate_result is kernel-emitted: {gate}"
    );

    // S6 — a real open-pr worker opens the PR against the local shim.
    let (opened_id, opened_actor, opened) = wait_capstone_event(
        &fx,
        "forge.pr.opened",
        commit_id,
        st(),
        "open-pr worker opens the pull request",
        |_| true,
    )
    .await;
    assert_eq!(opened_actor, ActorId::KernelDispatcher, "{opened}");
    let pr_number = opened["pr_number"]
        .as_u64()
        .unwrap_or_else(|| panic!("forge.pr.opened missing pr_number: {opened}"));
    let opened_head = opened["head_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("forge.pr.opened missing head_sha: {opened}"))
        .to_string();
    assert!(is_hex_sha(&opened_head), "{opened}");

    // S7 — CI conclusion read (shim-hardwired success).
    let (_checks_id, _, _checks) = wait_capstone_event(
        &fx,
        "forge.pr.checks",
        opened_id,
        st(),
        "PR checks read success",
        |p| p["pr_number"] == json!(pr_number) && p["conclusion"] == json!("success"),
    )
    .await;

    // S8 — a real reviewer worker reads the merge-base diff via gh.pr.diff; zero `forge.pr.diff.read` fails the run. The event is card-anonymous, so exact reviewer attribution is tolerated.
    let (diff_id, diff_actor, diff_read) = wait_capstone_event(
        &fx,
        "forge.pr.diff.read",
        opened_id,
        st(),
        "reviewer worker reads the real PR diff",
        |p| p["pr_number"] == json!(pr_number),
    )
    .await;
    assert_eq!(diff_actor, ActorId::KernelDispatcher, "{diff_read}");

    // S11 — merge after the review read the diff.
    let (merged_id, merged_actor, merged) = wait_capstone_event(
        &fx,
        "forge.pr.merged",
        diff_id,
        st(),
        "PR merged after review",
        |p| p["subject"]["pr_number"] == json!(pr_number),
    )
    .await;
    assert_eq!(merged_actor, ActorId::KernelDispatcher, "{merged}");
    let merged_head = merged["head_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("forge.pr.merged missing head_sha: {merged}"))
        .to_string();
    assert!(is_hex_sha(&merged_head), "{merged}");
    // The merge head is the head the last pre-merge review read: a fix → re-review run legitimately merges on the newer head.
    let reviewed_head = latest_reviewed_head_before_merge(&fx, merged_id, pr_number).await;
    assert_eq!(
        merged_head, reviewed_head,
        "merge head must equal the head the last review read with gh.pr.diff: {merged}"
    );
    let merge_sha = merged["merge_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("forge.pr.merged missing merge_sha: {merged}"));
    assert!(is_hex_sha(merge_sha), "{merged}");

    // S12 — issue closed strictly after the merge.
    let (_closed_id, closed_actor, closed) = wait_capstone_event(
        &fx,
        "forge.issue.closed",
        merged_id,
        st(),
        "source issue closed after merge",
        |p| p["issue_number"] == json!(CAPSTONE_ISSUE_NUMBER),
    )
    .await;
    assert_eq!(closed_actor, ActorId::KernelDispatcher, "{closed}");

    // S13 — the planner closes the track.
    let (_done_id, done_actor, done_close) = wait_capstone_event(
        &fx,
        "track.updated",
        merged_id,
        st(),
        "planner closes the track",
        |p| !p["closed_at"].is_null() && p["id"] == json!(fx.track_id.as_str()),
    )
    .await;
    assert!(
        matches!(done_actor, ActorId::AiPlannerSession(_)),
        "the close actor must be AiPlannerSession, got {done_actor:?} for {done_close}"
    );

    // Post-run oracle.
    capstone_oracle(&fx, pr_number, &merged_head, &repo_gitdir).await;

    // Teardown: dispatcher handle first, then harness, plugin, codex. Panic paths skip this and leak the shared appserver pgid; the isolation wrapper reaps the session process group.
    drop(dispatcher);
    shutdown_planner_harness_if_registered(&fx).await;
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}
