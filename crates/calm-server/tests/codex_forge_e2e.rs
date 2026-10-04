//! Real Codex forge E2E; feature-gated behind `codex-e2e` and self-skipping when no real Codex binary is available.

#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

#[path = "cases/codex_builtin_discovery.rs"]
mod codex_builtin_discovery;

#[path = "cases/codex_calendar_preview.rs"]
mod codex_calendar_preview;

use std::path::{Path, PathBuf};
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventScope};
use calm_server::harness::{HarnessState, Observation, PlannerHarness};
use calm_server::ids::{ActorId, TrackId};
use calm_server::mcp_server::tools::track_file::TOOL_TRACK_CAT;
use calm_server::plugin_host::Manifest;
use serde_json::{Value, json};
use support::agent_diag::panic_with_agent_diag;
use support::codex_fixture::*;
use support::event_queries::*;
use support::forge_env::FORGE_ENV_LOCK;
use support::gh_shim::{run_gh, seed_shim_issue_body, write_gh_shim};
use support::git_helpers::*;
use support::mcp::call_tool_via_socket;
use support::oracle::{
    OrderingEdge, RequiredEvent, assert_event_skeleton_superset, assert_ordering,
};
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

#[tokio::test]
async fn real_planner_agent_autonomously_plans_from_bound_template() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let goal =
        "Plan the smallest issue-development template for adding one marker file.".to_string();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            template_id: Some("issue-development".into()),
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
    assert_bound_issue_development_template_preconditions(&fx).await;

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
            template_id: Some("issue-development".into()),
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

/// The capstone's source issue number. An environment fact for the gh shim
/// (state keyed per repo selector); any number works.
const CAPSTONE_ISSUE_NUMBER: u64 = 840;

/// The real code task (P2): named-function contract so the oracle can assert
/// the diff at content-invariant level without matching stochastic text.
const CAPSTONE_ISSUE_BODY: &str = "Add `pub fn is_palindrome(s: &str) -> bool` to `src/lib.rs` \
with a `///` doc comment and a `#[test]` unit test covering a palindrome and a \
non-palindrome. Do not push; do not merge.\n";

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
            template_id: Some("issue-development".into()),
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

    let dispatcher = spawn_dispatcher_with_harness(&fx);

    let repo_gitdir = fx.track_cwd.join(".git").display().to_string();
    let goal = capstone_goal(&repo_gitdir, CAPSTONE_ISSUE_NUMBER, &fx.origin_main_initial);
    boot_planner_harness_via_start_op(&fx, goal).await;

    // Every stage gets its own ≥480s floor under one overall NEIGE_CAPSTONE_BUDGET deadline (default 3600s).
    let overall_deadline = Instant::now() + capstone_budget();
    let st = move || capstone_stage_budget().min(remaining(overall_deadline));

    // S1 — the real planner plans from the bound template.
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

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// head_sha of the latest `forge.pr.diff.read` for `pr_number` strictly BEFORE the merge.
async fn latest_reviewed_head_before_merge(fx: &Fixture, merged_id: i64, pr_number: u64) -> String {
    event_rows(&fx.repo, "forge.pr.diff.read")
        .await
        .into_iter()
        .filter(|r| r.id < merged_id && r.payload["pr_number"] == json!(pr_number))
        .max_by_key(|r| r.id)
        .and_then(|r| r.payload["head_sha"].as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| {
            panic!("no forge.pr.diff.read for pr {pr_number} precedes the merge (id {merged_id})")
        })
}

/// Capstone track goal: environment facts plus descriptor-legal planning steering; PR coordinates must flow through observations/runs.
fn capstone_goal(repo_gitdir: &str, issue_number: u64, base_sha: &str) -> String {
    format!(
        "Drive the bound issue-development template END-TO-END for issue #{issue_number}: read \
         the issue, implement, open a pull request, review it, merge, close the issue, and close \
         the track.\n\
         \n\
         Environment facts:\n\
         - The `repo` argument for EVERY gh.* forge tool call (gh.issue.view, gh.pr.create, \
         gh.pr.checks, gh.pr.diff, gh.pr.merge, gh.issue.close) is exactly `{repo_gitdir}`. \
         Embed this exact literal value in the goal of every task that must call a gh.* tool; \
         workers cannot discover it on their own.\n\
         - The track's source issue is #{issue_number}.\n\
         - Pull requests use base branch `main`; the base commit sha is `{base_sha}`.\n\
         \n\
         Planning constraints (all within the bound template):\n\
         - Attach the bound template gate (exactly its cmd) to every task you plan; do not use \
         no_gate_reason.\n\
         - A task block whose depends_on names a task that does not exist yet is written but \
         receives an unknown_dependency diagnostic and is not projected, so create report task \
         blocks in dependency order: (1) inspect-issue, then implement-change; (2) add open-pr \
         only after implement-change completes, embedding the implement worker's actual branch \
         name in its goal; (3) after open-pr completes, add review-pr, then add merge after \
         the review task block exists, embedding the literal repo, pr number, base sha and \
         head sha values in each of their goals.\n\
         - implement-change goal: implement exactly what the issue asks by editing src/lib.rs \
         in the worker's own working directory, then call the MCP tool whose name ends in \
         `git.commit` (arguments: a commit message and a non-empty idem) and note the branch \
         it reports; the worker must NOT run `git push`, must NOT open a pull request, and \
         must NOT use the shell for git; it must report the branch name in its \
         calm.task.complete result.\n\
         - open-pr goal: call gh.pr.create with repo `{repo_gitdir}`, head = the implement \
         worker's branch, base `main`, and a non-empty title and body; then call gh.pr.checks \
         for the created PR; then call calm.task.complete reporting the literal pr_number and \
         head_sha values gh.pr.create returned; the open-pr worker must NOT call gh.pr.diff \
         or gh.pr.list.\n\
         - review-pr goal: call gh.pr.diff with the embedded repo, pr, \
         base_sha and head_sha, review the returned diff against the issue requirements, and \
         report the literal verdict token `approved` or `changes_requested` in \
         calm.task.complete.\n\
         - merge goal: call gh.pr.merge with the embedded repo and pr, phase `impl`, \
         slice_id `implement-change`, and expected_head_sha equal to the head sha the review read with \
         gh.pr.diff; then call gh.issue.close for issue #{issue_number} with the same repo.\n\
         - After the merge task completes and the issue is closed, close the track with \
         calm.track.close.\n\
         - If review cannot converge, give up and close the track with the reason; do not \
         request ratification."
    )
}

/// Stage wait with the failure terminator folded in: `ratify.requested` at ANY point, or a close before the awaited one, fails fast with agent diagnostics.
async fn wait_capstone_event(
    fx: &Fixture,
    kind: &str,
    floor: i64,
    budget: Duration,
    describe: &str,
    predicate: impl Fn(&Value) -> bool,
) -> (i64, ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        if !event_payloads(&fx.repo, "ratify.requested")
            .await
            .is_empty()
        {
            panic_with_agent_diag(
                fx,
                format!(
                    "ratify.requested emitted during the steered GIVE-UP capstone (purity \
                     violation) while waiting for {kind} ({describe})"
                ),
            )
            .await;
        }
        if kind != "track.updated" && track_is_closed(fx).await {
            panic_with_agent_diag(
                fx,
                format!("the track closed (planner gave up) while waiting for {kind} ({describe})"),
            )
            .await;
        }
        let rows: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(
            "SELECT id, actor, scope_track, payload FROM events \
             WHERE kind = ?1 AND id > ?2 ORDER BY id ASC",
        )
        .bind(kind)
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("{kind} event rows after floor {floor}: {e}"));
        let hit = rows
            .into_iter()
            .find_map(|(id, actor, scope_track, payload)| {
                let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
                let payload: Value = serde_json::from_str(&payload).expect("event payload json");
                (scope_track.as_deref() == Some(fx.track_id.as_str()) && predicate(&payload))
                    .then_some((id, actor, payload))
            });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for {kind} ({describe}) after event \
                     id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

/// The P6 post-run oracle: skeleton superset, orderings, actor table, merge
/// idem-key shape, no-cargo gate audit, content invariant, purity.
async fn capstone_oracle(fx: &Fixture, pr_number: u64, merged_head: &str, repo_gitdir: &str) {
    // Skeleton (⊇): every required kind appears at least once; extra events,
    // extra tasks, dup idempotent rows are all tolerated.
    assert_event_skeleton_superset(
        &fx.repo,
        &[
            RequiredEvent::any("plan.updated"),
            RequiredEvent::new("task.dispatched", |r| r.payload["kind"] == json!("codex")),
            RequiredEvent::any("workspace.leased"),
            RequiredEvent::any("worker_session.started"),
            RequiredEvent::any("task.completed"),
            RequiredEvent::any("worktree.committed"),
            RequiredEvent::new("task.gate_result", |r| r.payload["passed"] == json!(true)),
            RequiredEvent::new("forge.issue.read", |r| {
                r.payload["issue_number"] == json!(CAPSTONE_ISSUE_NUMBER)
            }),
            RequiredEvent::any("forge.pr.opened"),
            RequiredEvent::new("forge.pr.checks", |r| {
                r.payload["conclusion"] == json!("success")
            }),
            RequiredEvent::any("forge.pr.diff.read"),
            RequiredEvent::new("forge.pr.merged", |r| {
                r.payload["merge_sha"].as_str().is_some_and(is_hex_sha)
            }),
            RequiredEvent::new("forge.issue.closed", |r| {
                r.payload["issue_number"] == json!(CAPSTONE_ISSUE_NUMBER)
            }),
            RequiredEvent::new("track.updated", |r| !r.payload["closed_at"].is_null()),
        ],
    )
    .await;

    assert_ordering(
        &fx.repo,
        &[
            OrderingEdge::new(
                "plan.updated",
                |_| true,
                "task.dispatched",
                |r| r.payload["kind"] == json!("codex"),
            ),
            OrderingEdge::new(
                "task.gate_result",
                |r| r.payload["passed"] == json!(true),
                "forge.pr.merged",
                |_| true,
            ),
            OrderingEdge::new(
                "forge.pr.checks",
                |r| r.payload["conclusion"] == json!("success"),
                "forge.pr.merged",
                |_| true,
            ),
            OrderingEdge::new("forge.pr.merged", |_| true, "forge.issue.closed", |_| true),
        ],
    )
    .await;

    // Actor table (event-row column, never payload).
    for (actor, payload) in actor_payload_rows(&fx.repo, "plan.updated").await {
        assert!(
            matches!(actor, ActorId::AiPlannerSession(_)),
            "plan.updated actor must be AiPlannerSession, got {actor:?} for {payload}"
        );
    }
    for kind in ["task.dispatched", "task.gate_result", "worktree.committed"] {
        for (actor, payload) in actor_payload_rows(&fx.repo, kind).await {
            assert_eq!(
                actor,
                ActorId::KernelDispatcher,
                "{kind} actor must be KernelDispatcher: {payload}"
            );
        }
    }

    // Merge idem-key shape: the plugin idem carries `:{expected_head_sha}` ONLY when it was passed. The caller card is NOT pinned: a merge-worker seat is as legal as the planner seat.
    let merge_keys = forge_action_idem_keys_containing(fx, ":gh.pr.merge:").await;
    assert!(
        !merge_keys.is_empty(),
        "expected a parked forge-action gh.pr.merge operation row"
    );
    let merge_suffix = format!(":gh.pr.merge:{repo_gitdir}:{pr_number}:{merged_head}");
    for key in &merge_keys {
        assert!(
            key.ends_with(&merge_suffix),
            "every gh.pr.merge forge-action op must carry the WITH-sha idem key \
             (expected suffix {merge_suffix}): {merge_keys:?}"
        );
    }
    let close_keys = forge_action_idem_keys_containing(fx, ":gh.issue.close:").await;
    assert!(
        !close_keys.is_empty(),
        "expected a parked forge-action gh.issue.close operation row"
    );
    let close_suffix = format!(":gh.issue.close:{repo_gitdir}:{CAPSTONE_ISSUE_NUMBER}");
    for key in &close_keys {
        assert!(
            key.ends_with(&close_suffix),
            "every gh.issue.close forge-action op must target the goal issue at the \
             steered repo selector (expected suffix {close_suffix}): {close_keys:?}"
        );
    }

    // No-cargo audit (checker pin d): the planner copied gates into task blocks;
    // no stored task gate may invoke cargo.
    let gate_jsons: Vec<Option<String>> = sqlx::query_scalar("SELECT gate_json FROM tasks")
        .fetch_all(fx.repo.pool())
        .await
        .expect("tasks.gate_json rows");
    for gate_json in gate_jsons.into_iter().flatten() {
        assert!(
            !gate_json.contains("cargo"),
            "a tasks.gate_json row invokes cargo (#863-B amplifier): {gate_json}"
        );
    }

    // Content invariant: the MERGED head's diff against the seeded base adds the issue-contract function. Worker worktrees share the clone gitdir, so no push is involved.
    let content_diff = git_stdout(
        &fx.track_cwd,
        [
            "diff",
            &format!("{}..{}", fx.origin_main_initial, merged_head),
            "--",
            "src/lib.rs",
        ],
    );
    assert!(
        !content_diff.is_empty(),
        "merged head {merged_head} must change src/lib.rs vs the seeded base"
    );
    assert!(
        content_diff
            .lines()
            .any(|l| l.starts_with('+') && l.contains("pub fn is_palindrome")),
        "merged diff must ADD `pub fn is_palindrome` to src/lib.rs:\n{content_diff}"
    );
    assert!(
        content_diff
            .lines()
            .any(|l| l.starts_with('+') && l.contains("#[test]")),
        "merged diff must ADD a #[test] to src/lib.rs:\n{content_diff}"
    );
    // The worker must not have pushed (issue contract + local-shim topology).
    let bare_main =
        git_stdout_no_cwd(["--git-dir", path_str(&fx.origin_repo), "rev-parse", "main"]);
    assert_eq!(
        bare_main, fx.origin_main_initial,
        "local bare origin main changed; nothing in the capstone may push"
    );

    // Exactly-once remote side effects (shim counters count REAL merges/
    // closes only; the shim is idempotent) at the steered repo selector.
    let shim_state = PathBuf::from(format!("{repo_gitdir}.shimstate"));
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

    // Purity: never ratified, never the injected-plan path, and the track closed.
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "steered GIVE-UP capstone must never request ratification"
    );
    assert!(
        track_is_closed(fx).await,
        "the capstone track row must be closed"
    );
    assert!(
        !fx.used_injected_plan(),
        "RealPlannerTurn must not use injected plan path"
    );
}

// Capstone deterministic support gates: these run WITHOUT a codex binary (no skip).

/// The seeded gate script must pass under the task-verify wrapper's EXACT conditions (`/bin/sh`, cleared env, repo cwd) and be cargo-free.
#[test]
fn capstone_gate_script_is_hermetic_and_cargo_free() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin.git");
    let clone = tmp.path().join("clone");
    seed_rust_micro_crate(&origin, &tmp.path().join("seed"));
    clone_for_track(&origin, &clone);

    let script = std::fs::read_to_string(clone.join("e2e-gate.sh")).expect("seeded gate script");
    assert!(
        !script.contains("cargo"),
        "seeded gate script must never invoke cargo:\n{script}"
    );
    assert!(!CAPSTONE_GATE_CMD.contains("cargo"));
    assert!(
        !clone.join("Cargo.toml").exists() && !clone.join("src/Cargo.toml").exists(),
        "the micro-crate must not carry a Cargo.toml (removes every cargo surface)"
    );

    let out = std::process::Command::new("/bin/sh")
        .arg("e2e-gate.sh")
        .current_dir(&clone)
        .env_clear()
        .output()
        .expect("run seeded gate script env-cleared");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "env-cleared gate script failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("test result: ok"),
        "gate script must run the seeded unit test\nstdout:\n{stdout}"
    );
}

/// Shipped git-forge `templates[]` is an id handle only; gate cmds live in report task blocks, not the descriptor.
#[test]
fn shipped_git_forge_templates_are_id_only() {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");
    let value: Value = serde_json::from_str(&raw).expect("manifest json");
    assert_eq!(
        value["templates"],
        json!([{ "id": "issue-development" }]),
        "S5 git-forge templates[] must be id-only"
    );
    let manifest = Manifest::parse(&raw).expect("production manifest parses");
    assert_eq!(manifest.templates.len(), 1);
    assert_eq!(manifest.templates[0].id, "issue-development");
}

/// gh shim `issue view --json body`: a seeded per-issue body file wins; absent
/// a seeded file the historical hardcoded fallback is byte-preserved.
#[test]
fn gh_shim_issue_view_prefers_seeded_body_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_gh_shim(tmp.path());
    let gh = tmp.path().join("gh");
    let repo = tmp.path().join("origin.git");
    let repo_arg = repo.display().to_string();
    seed_shim_issue_body(&repo, CAPSTONE_ISSUE_NUMBER, CAPSTONE_ISSUE_BODY);

    let seeded = run_gh(
        &gh,
        &[
            "issue",
            "view",
            &CAPSTONE_ISSUE_NUMBER.to_string(),
            "--repo",
            &repo_arg,
            "--json",
            "body",
            "--jq",
            ".body",
        ],
    );
    assert!(seeded.status.success());
    assert_eq!(
        String::from_utf8_lossy(&seeded.stdout),
        CAPSTONE_ISSUE_BODY,
        "seeded issue body file must be served verbatim"
    );

    let fallback = run_gh(
        &gh,
        &[
            "issue", "view", "9999", "--repo", &repo_arg, "--json", "body", "--jq", ".body",
        ],
    );
    assert!(fallback.status.success());
    assert_eq!(
        String::from_utf8_lossy(&fallback.stdout),
        "# Issue 9999\n\nFake issue body for issue-development ingestion.\n",
        "unseeded issues must keep the historical hardcoded body (behavior-preserving)"
    );
}

/// A child forked by another thread can hold a fork-inherited write fd to the shim until it execs, so a direct spawn can fail ETXTBSY; `run_gh` must retry. Linux-only: macOS does not enforce ETXTBSY.
#[cfg(target_os = "linux")]
#[test]
fn gh_shim_spawn_retries_transient_etxtbsy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_gh_shim(tmp.path());
    let gh = tmp.path().join("gh");
    let repo_arg = tmp.path().join("origin.git").display().to_string();
    let args = [
        "issue", "view", "1234", "--repo", &repo_arg, "--json", "body", "--jq", ".body",
    ];

    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(&gh)
        .expect("open write fd on gh shim");

    // Repro gate: with the write fd held, the raw exec fails ETXTBSY.
    let raw_err = std::process::Command::new(&gh)
        .args(args)
        .output()
        .expect_err("raw spawn must fail while a write fd is held");
    assert_eq!(
        raw_err.kind(),
        std::io::ErrorKind::ExecutableFileBusy,
        "raw spawn under a held write fd must fail ETXTBSY, got: {raw_err}"
    );

    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(held);
    });

    let out = run_gh(&gh, &args);
    releaser.join().expect("join fd releaser thread");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "# Issue 1234\n\nFake issue body for issue-development ingestion.\n",
        "run_gh must succeed with the expected shim output once the fd is released"
    );
}

/// Seed a pipeline task pair whose completion carries `result`, then fail fast unless the runs/ projection surfaces `expected_summary`.
async fn seed_completed_task_pair(fx: &Fixture, key: &str, result: Value, expected_summary: &str) {
    let verdict = expected_summary;
    let task_id = task_id(fx, key);
    let track_scope = EventScope::Track {
        track: fx.track_id.clone(),
        area: fx.area_id.clone(),
    };
    let dispatch_message = format!("[codex-forge-e2e] seed task {key}");
    calm_server::db::write_with_actor_events_typed::<(), _>(
        fx.repo.as_ref(),
        None,
        &fx.events,
        &fx.write,
        {
            let task_id = task_id.clone();
            let dispatch_message = dispatch_message.clone();
            move |_tx| {
                let task_id = task_id.clone();
                let track_scope = track_scope.clone();
                let dispatch_message = dispatch_message.clone();
                Box::pin(async move {
                    Ok((
                        (),
                        vec![
                            (
                                ActorId::KernelDispatcher,
                                track_scope.clone(),
                                Event::TaskDispatched {
                                    idempotency_key: task_id.clone(),
                                    kind: "codex".into(),
                                    agent_message: Some(dispatch_message),
                                },
                            ),
                            (
                                ActorId::KernelDispatcher,
                                track_scope,
                                Event::TaskContextFrozen {
                                    track_id: TrackId::default(),
                                    task_key: String::new(),
                                    idempotency_key: String::new(),
                                    task_id,
                                    refs: vec![],
                                    doc_revs: Default::default(),
                                    truncated: false,
                                },
                            ),
                        ],
                    ))
                })
            }
        },
    )
    .await
    .expect("log seeded dispatch + context freeze batch");

    // The fixture shortcut mints no real worker session, so the completion is authored as KernelDispatcher; card scope alone routes it to the completed bucket.
    let card_scope = EventScope::Card {
        card: fx.planner_card_id.clone(),
        track: fx.track_id.clone(),
        area: fx.area_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            card_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.track_area_cache,
            Event::TaskCompleted {
                idempotency_key: task_id.clone(),
                result,
                artifacts: Vec::new(),
                agent_message: Some(format!("[codex-forge-e2e] task {key} -> {verdict}")),
            },
        )
        .await
        .expect("log seeded task.completed");

    let handler = fx
        .registry
        .lookup(TOOL_TRACK_CAT)
        .expect("track cat registered");
    let json_path = format!("runs/{task_id}.json");
    let json_read = handler(
        fx.ctx.clone(),
        planner_identity(fx),
        json!({ "path": json_path }),
    )
    .await
    .map(calm_server::mcp_server::result::ToolResult::into_structured)
    .map_err(|e| format!("{e:?}"));
    let mut json_diag = String::new();
    if let Ok(value) = &json_read {
        let content = value
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match serde_json::from_str::<Value>(content) {
            Ok(run) => {
                let result = run.pointer("/events/completed/payload/result");
                match result {
                    Some(Value::Object(result)) => match result.get("summary") {
                        Some(Value::String(summary)) if summary == verdict => return,
                        Some(summary) => {
                            json_diag = format!(
                                "completed result summary was not exact {verdict}: {summary}; result={}",
                                Value::Object(result.clone())
                            );
                        }
                        None => {
                            json_diag = format!(
                                "completed result missing summary: {}",
                                Value::Object(result.clone())
                            );
                        }
                    },
                    Some(result) => {
                        json_diag = format!("completed result was not an object: {result}");
                    }
                    None => {
                        json_diag = "<missing completed result>".into();
                    }
                }
            }
            Err(err) => {
                json_diag = format!("invalid json content: {err}; content={content}");
            }
        }
    }

    let md_path = format!("runs/{task_id}.md");
    let md_read = handler(
        fx.ctx.clone(),
        planner_identity(fx),
        json!({ "path": md_path }),
    )
    .await
    .map(calm_server::mcp_server::result::ToolResult::into_structured)
    .map_err(|e| format!("{e:?}"));
    if let Ok(value) = &md_read {
        let content = value
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if content.lines().any(|line| line == verdict) {
            return;
        }
    }

    panic!(
        "seeded task run {task_id} did not expose exact {verdict} summary in runs projection; \
         json_result={}; json_read={:?}; md_read={:?}",
        if json_diag.is_empty() {
            "<unread>".to_string()
        } else {
            json_diag
        },
        json_read,
        md_read
    );
}

async fn recover_planner_harness(fx: &Fixture) -> Option<PlannerHarness> {
    let runtime = fx
        .repo
        .session_projection_active_for_card(&fx.planner_card_id.to_string())
        .await
        .ok()
        .flatten()?;
    fx.harness.get(&runtime.id)
}

async fn track_is_closed(fx: &Fixture) -> bool {
    sqlx::query_scalar("SELECT closed_at IS NOT NULL FROM tracks WHERE id = ?1")
        .bind(fx.track_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select track closed_at")
}

async fn wait_for_planner_turn_settled(fx: &Fixture, h: &PlannerHarness, budget: Duration) {
    let deadline = Instant::now() + budget;
    let mut last_state = h.state_for_test().await;
    let mut last_pending = h.pending_len_for_test().await;
    loop {
        if matches!(
            last_state,
            HarnessState::Idle | HarnessState::TurnCompleted { .. }
        ) && last_pending == 0
        {
            return;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for planner harness turn to settle; \
                     last_state={last_state:?}; last_pending_len={last_pending}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(100)).await;
        last_state = h.state_for_test().await;
        last_pending = h.pending_len_for_test().await;
    }
}

async fn max_event_id(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events")
        .fetch_one(repo.pool())
        .await
        .expect("select max event id")
}

fn merge_close_goal(repo_gitdir: &str, issue_number: u64) -> String {
    format!(
        "Drive the tail of the issue-development template for issue #{issue_number}. \
         Environment facts: the `repo` argument for every gh.* MCP forge tool is exactly \
         `{repo_gitdir}`; the track's source issue is #{issue_number}. Implementation, the \
         pull request, and its review are already complete for this track; their results \
         arrive as observations. Once the review approves the pull request, execute the \
         merge step yourself with the MCP forge tools (gh.pr.merge, then gh.issue.close \
         for issue #{issue_number}); do not dispatch further tasks."
    )
}

/// The live planner session's bound codex thread id, the identity for scripted daemon-socket `tools/call`s.
async fn planner_session_thread_id(fx: &Fixture) -> String {
    fx.repo
        .session_projection_active_for_card(&fx.planner_card_id.to_string())
        .await
        .expect("active planner session lookup")
        .expect("live planner session for planner card")
        .thread_id
        .expect("planner session bound to a codex thread")
}

fn assert_forge_tool_accepted(resp: &Value, label: &str) {
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

/// First `kind` event on the fixture track with id > `floor` matching `predicate`; superset-tolerant. Returns the event id.
async fn wait_for_track_forge_event(
    fx: &Fixture,
    kind: &str,
    floor: i64,
    budget: Duration,
    describe: &str,
    predicate: impl Fn(&Value) -> bool,
) -> (i64, ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(
            "SELECT id, actor, scope_track, payload FROM events \
             WHERE kind = ?1 AND id > ?2 ORDER BY id ASC",
        )
        .bind(kind)
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("{kind} event rows after floor {floor}: {e}"));
        let hit = rows
            .into_iter()
            .find_map(|(id, actor, scope_track, payload)| {
                let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
                let payload: Value = serde_json::from_str(&payload).expect("event payload json");
                (scope_track.as_deref() == Some(fx.track_id.as_str()) && predicate(&payload))
                    .then_some((id, actor, payload))
            });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for {kind} ({describe}) after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn latest_event_id_of_kind(fx: &Fixture, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("max {kind} event id: {e}"))
}

/// All forge-action op idempotency keys containing `needle`, oldest first; the shape `{plugin}:{track}:{caller card}:{plugin idem}` pins both the seat and the plugin idem.
async fn forge_action_idem_keys_containing(fx: &Fixture, needle: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT idempotency_key FROM operations \
         WHERE kind = 'forge-action' AND idempotency_key LIKE '%' || ?1 || '%' \
         ORDER BY created_at_ms ASC",
    )
    .bind(needle)
    .fetch_all(fx.repo.pool())
    .await
    .expect("forge-action idempotency keys")
}

fn shim_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

#[cfg(feature = "fixtures")]
async fn inject_observation(h: &PlannerHarness, obs: Observation) {
    h.observe_for_test(obs, None).await;
}

#[cfg(not(feature = "fixtures"))]
async fn inject_observation(_h: &PlannerHarness, _obs: Observation) {
    panic!("inject_observation requires the fixtures feature");
}

fn review_budget() -> Duration {
    std::env::var("NEIGE_PLANNER_REVIEW_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        // Doubled vs planner_planning_budget: the review wait includes the planner's autonomous runs/ read round-trip.
        .unwrap_or_else(|| Duration::from_secs(480))
}
