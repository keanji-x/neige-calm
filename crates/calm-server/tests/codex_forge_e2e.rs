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

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{ChannelVerdict, ChannelVerdictKind, Event, EventScope, ReviewSubject};
use calm_server::harness::{HarnessState, Observation, SpecHarness};
use calm_server::ids::ActorId;
use calm_server::mcp_server::tools::wave_file::TOOL_WAVE_CAT;
use calm_server::model::{WaveLifecycle, WavePatch};
use calm_server::plugin_host::Manifest;
use calm_server::state::AppState;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use support::agent_diag::panic_with_agent_diag;
use support::codex_fixture::*;
use support::event_queries::*;
use support::forge_env::FORGE_ENV_LOCK;
use support::gh_shim::{run_gh, seed_shim_issue_body, write_gh_shim};
use support::git_helpers::*;
use support::mcp::call_tool_via_socket;
use support::oracle::{
    OrderingEdge, RequiredEvent, SubjectKey, assert_cap_extension_history,
    assert_converged_subject_has_merge, assert_event_skeleton_superset, assert_ordering,
    assert_subject_keyed_cap_enforcement,
};
use support::spec_turn::*;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const PR_CREATE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.create";
const PR_CHECKS_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.checks";
/// The d2 test's source issue. Purely an environment fact: the gh shim keeps
/// per-repo issue state keyed by number, and any number works.
const D2_ISSUE_NUMBER: u64 = 840;

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

    // Each test boots an isolated fixture/DB. This test's fixture sees NO
    // scripted `call_tool` for any `gh.*` or `git.commit` (its only direct
    // tool call is `TOOL_PLAN_UPSERT` via the spec identity inside
    // `plan_codex_task`; the d2 merge test scripts `gh.pr.create`/
    // `gh.pr.checks` only against its own separate fixture). Therefore the
    // ONLY thing that can emit `forge.pr.opened` / `forge.pr.checks` here is
    // the real worker's own MCP `tools/call`. Assert via the `events` table
    // (NOT `harness_items`, which is spec-thread-only).
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
            descriptor_gate_cmd: None,
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

#[tokio::test]
async fn real_spec_agent_autonomously_emits_design_review_round_from_descriptor() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let goal = "Plan the smallest issue-development workflow for adding one marker file, \
                then drive design-review convergence."
        .to_string();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            workflow_id: Some("issue-development".into()),
            plan_source: PlanSource::RealSpecTurn,
            issue_body: None,
            mint_report_card: false,
            require_task_gates: false,
            descriptor_gate_cmd: None,
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

    boot_spec_harness_via_start_op(&fx, goal).await;

    let (plan_actor, _plan) = wait_for_plan_updated(&fx, spec_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiSpecSession(_)),
        "plan.updated actor must be the real spec session, got {plan_actor:?}"
    );

    let harness = recover_spec_harness(&fx).await.expect("live spec harness");
    // Settle the planning turn before seeding so the accepted review.round is
    // causally a response to the injected task completions, not a planning-time
    // fabrication. R6 deliberately does not prove the spec literally read runs/:
    // the runs/ pre-check proves the verdict data is present and readable, and
    // this causal wake is sufficient for the autonomy thesis.
    wait_for_spec_turn_settled(&fx, &harness, spec_planning_budget()).await;
    seed_design_channel_complete(&fx, "review-design-a", "a").await;
    seed_design_channel_complete(&fx, "review-design-b", "b").await;
    let floor = max_event_id(&fx.repo).await;
    assert_eq!(
        count_design_review_rounds(&fx).await,
        0,
        "planning turn must not have emitted a design review.round before the seeded verdicts (id<=floor): proof-validity guard"
    );

    inject_task_completed(&harness, &task_id(&fx, "review-design-a")).await;
    inject_task_completed(&harness, &task_id(&fx, "review-design-b")).await;

    let rounds = wait_for_converged_design_review_round(&fx, floor, review_budget()).await;
    assert_real_design_review_round(&fx, &rounds).await;

    shutdown_spec_harness_if_registered(&fx).await;
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

#[tokio::test]
async fn real_spec_gives_up_at_review_cap_from_descriptor() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // Steer the cap-exhaust GIVE-UP branch (R7 design D3): GIVE-UP and
    // ASK-HUMAN are mutually exclusive terminal branches of one wave, so the
    // goal fixes coverage on this branch; branch choice is descriptor-legal
    // either way and the protocol mechanics stay autonomous.
    let goal = "Plan the smallest issue-development workflow for adding one marker file, \
                then drive design review. If design review cannot converge at the review \
                cap, give up and fail the wave; do not request ratification."
        .to_string();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            workflow_id: Some("issue-development".into()),
            plan_source: PlanSource::RealSpecTurn,
            issue_body: None,
            mint_report_card: false,
            require_task_gates: false,
            descriptor_gate_cmd: None,
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

    boot_spec_harness_via_start_op(&fx, goal).await;

    let (plan_actor, plan) = wait_for_plan_updated(&fx, spec_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiSpecSession(_)),
        "plan.updated actor must be the real spec session, got {plan_actor:?}"
    );
    // The review subject slice comes from the spec's OWN real plan so the
    // seeded prior round matches the spec's mental model (design D2).
    let slice_id = plan["changed_keys"][0]
        .as_str()
        .unwrap_or_else(|| panic!("plan.updated changed_keys[0] must be a string: {plan}"))
        .to_string();

    let harness = recover_spec_harness(&fx).await.expect("live spec harness");
    // Settle the planning turn before seeding (R6 causality guard): the
    // accepted give-up sequence must be causally a response to the injected
    // observations below, not a planning-time fabrication.
    wait_for_spec_turn_settled(&fx, &harness, spec_planning_budget()).await;

    // Pre-position the wave at `reviewing` via a raw WavePatch (precedent:
    // crates/calm-server/tests/review_ratify.rs `set_wave_lifecycle`) —
    // walking planning -> ... -> reviewing by real turns is capstone scope.
    fx.repo_dyn
        .wave_update(
            fx.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Reviewing),
                ..WavePatch::default()
            },
        )
        .await
        .expect("pre-position wave lifecycle to reviewing");

    seed_design_channel_changes_requested(&fx, "review-design-a", "a").await;
    seed_design_channel_changes_requested(&fx, "review-design-b", "b").await;
    // Seed ONE prior round at n=7/cap=8 (design D2): the kernel's monotonic
    // check reads max(n) from the event log, so the only acceptable next
    // round on this subject is n=8, and n=9 is then unreachable (n<=cap).
    // role_gate rule 2.8 makes AiSpec(spec card) the ONLY legal author for
    // review.round — KernelDispatcher (R6's seed actor) is rejected.
    seed_prior_design_review_round(&fx, &slice_id, 7, 8).await;

    let floor = max_event_id(&fx.repo).await;
    let pre_wake_rounds = actor_payload_rows(&fx.repo, "review.round").await;
    assert_eq!(
        pre_wake_rounds.len(),
        1,
        "exactly the one seeded review.round may exist pre-wake (proof-validity guard): {pre_wake_rounds:?}"
    );

    // Wake: both channels changes_requested + the prior-round state, injected
    // exactly as the prod dispatcher's `harness_observation_from_event` would
    // push them (design D5; no dispatcher runs in the spec-harness E2E).
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-a")).await;
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-b")).await;
    inject_design_review_round_observation(&harness, &fx, &slice_id, 7, 8, false).await;

    // Oracle (a): the spec's own round 8/8, non-converged, on the seeded
    // subject. The kernel guarantees n==8 is the only accepted next round.
    let (round_id, round_actor, round) =
        wait_for_design_review_round_on_subject(&fx, floor, &slice_id, review_budget()).await;
    assert!(
        matches!(round_actor, ActorId::AiSpecSession(_)),
        "cap round actor must be AiSpecSession, got {round_actor:?} for {round}"
    );
    assert_eq!(round["n"], json!(8), "cap round must be n=8: {round}");
    assert_eq!(
        round["cap"],
        json!(8),
        "cap must be descriptor-fixed 8: {round}"
    );
    assert_eq!(
        round["converged"],
        json!(false),
        "cap round must be non-converged: {round}"
    );
    assert!(
        round
            .pointer("/subject/pr_number")
            .is_none_or(Value::is_null),
        "design review.round subject must omit/null pr_number: {round}"
    );
    let channels = round["channels"]
        .as_array()
        .unwrap_or_else(|| panic!("cap round channels must be an array: {round}"));
    assert!(
        channels.len() >= 2,
        "cap round must carry at least two channels: {round}"
    );
    let roles: std::collections::BTreeSet<&str> = channels
        .iter()
        .map(|channel| {
            channel["role"]
                .as_str()
                .unwrap_or_else(|| panic!("cap round channel missing role: {channel}"))
        })
        .collect();
    assert!(
        roles.len() >= 2,
        "cap round channels must have at least two distinct roles: {round}"
    );
    assert!(
        channels
            .iter()
            .any(|channel| channel["verdict"] == json!("changes_requested")),
        "cap round must carry at least one changes_requested verdict: {round}"
    );

    // Oracle (b): the FSM's give-up edge, spec-only-legal. The *edge* is in
    // the static prompt, but the *when* (at cap, instead of ratifying) comes
    // only from the descriptor. Waiting from the cap round's event id (not
    // the original floor) pins the ordering invariant: the give-up must
    // FOLLOW the cap round, so a fail-first-record-later turn cannot pass.
    let (edge_actor, edge) = wait_for_wave_failed_edge(&fx, round_id, review_budget()).await;
    assert_eq!(
        edge["from"],
        json!("reviewing"),
        "give-up edge must leave reviewing: {edge}"
    );
    assert_eq!(edge["id"], json!(fx.wave_id.as_str()));
    assert!(
        matches!(edge_actor, ActorId::AiSpecSession(_)),
        "give-up edge actor must be AiSpecSession, got {edge_actor:?} for {edge}"
    );

    // Oracle (c): the waves row landed on the terminal lifecycle.
    let lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id = ?1")
        .bind(fx.wave_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select wave lifecycle");
    assert_eq!(lifecycle, "failed", "wave row lifecycle must be failed");

    // Oracle (d): branch purity, asserted AFTER the give-up edge (terminal
    // state + teardown follows, so no window ambiguity): the steered run
    // must neither merge nor ask for ratification.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "steered GIVE-UP run must not request ratification"
    );

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

#[tokio::test]
async fn real_spec_requests_ratification_at_cap_and_resumes_on_grant() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // Steer the cap-exhaust ASK-HUMAN branch (R7 design D4): GIVE-UP and
    // ASK-HUMAN are mutually exclusive terminal branches of one wave, so the
    // goal fixes coverage on this branch; branch choice is descriptor-legal
    // either way and the protocol mechanics stay autonomous.
    let goal = "Plan the smallest issue-development workflow for adding one marker file, \
                then drive design review. If design review cannot converge at the review \
                cap, ask for human ratification instead of giving up; do not fail the wave."
        .to_string();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            workflow_id: Some("issue-development".into()),
            plan_source: PlanSource::RealSpecTurn,
            issue_body: None,
            mint_report_card: false,
            require_task_gates: false,
            descriptor_gate_cmd: None,
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

    boot_spec_harness_via_start_op(&fx, goal).await;

    let (plan_actor, plan) = wait_for_plan_updated(&fx, spec_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiSpecSession(_)),
        "plan.updated actor must be the real spec session, got {plan_actor:?}"
    );
    // The review subject slice comes from the spec's OWN real plan so the
    // seeded prior round matches the spec's mental model (design D2).
    let slice_id = plan["changed_keys"][0]
        .as_str()
        .unwrap_or_else(|| panic!("plan.updated changed_keys[0] must be a string: {plan}"))
        .to_string();

    let harness = recover_spec_harness(&fx).await.expect("live spec harness");
    // Settle the planning turn before seeding (R6 causality guard): the
    // accepted ASK-HUMAN sequence must be causally a response to the injected
    // observations below, not a planning-time fabrication.
    wait_for_spec_turn_settled(&fx, &harness, spec_planning_budget()).await;

    // Pre-position the wave at `reviewing` via a raw WavePatch (precedent:
    // crates/calm-server/tests/review_ratify.rs `set_wave_lifecycle`) —
    // walking planning -> ... -> reviewing by real turns is capstone scope.
    fx.repo_dyn
        .wave_update(
            fx.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Reviewing),
                ..WavePatch::default()
            },
        )
        .await
        .expect("pre-position wave lifecycle to reviewing");

    seed_design_channel_changes_requested(&fx, "review-design-a", "a").await;
    seed_design_channel_changes_requested(&fx, "review-design-b", "b").await;
    // Seed ONE prior round at n=7/cap=8 (design D2): the kernel's monotonic
    // check reads max(n) from the event log, so the only acceptable next
    // round on this subject is n=8, and n=9 is then unreachable (n<=cap).
    seed_prior_design_review_round(&fx, &slice_id, 7, 8).await;

    let floor = max_event_id(&fx.repo).await;
    let pre_wake_rounds = actor_payload_rows(&fx.repo, "review.round").await;
    assert_eq!(
        pre_wake_rounds.len(),
        1,
        "exactly the one seeded review.round may exist pre-wake (proof-validity guard): {pre_wake_rounds:?}"
    );

    // Wake: both channels changes_requested + the prior-round state, injected
    // exactly as the prod dispatcher's `harness_observation_from_event` would
    // push them (design D5; no dispatcher runs in the spec-harness E2E).
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-a")).await;
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-b")).await;
    inject_design_review_round_observation(&harness, &fx, &slice_id, 7, 8, false).await;

    // Oracle phase 1 (a): the spec's own round 8/8, non-converged, on the
    // seeded subject. The kernel guarantees n==8 is the only accepted next
    // round.
    let (round_id, round_actor, round) =
        wait_for_design_review_round_on_subject(&fx, floor, &slice_id, review_budget()).await;
    assert!(
        matches!(round_actor, ActorId::AiSpecSession(_)),
        "cap round actor must be AiSpecSession, got {round_actor:?} for {round}"
    );
    assert_eq!(round["n"], json!(8), "cap round must be n=8: {round}");
    assert_eq!(
        round["cap"],
        json!(8),
        "cap must be descriptor-fixed 8: {round}"
    );
    assert_eq!(
        round["converged"],
        json!(false),
        "cap round must be non-converged: {round}"
    );
    assert!(
        round
            .pointer("/subject/pr_number")
            .is_none_or(Value::is_null),
        "design review.round subject must omit/null pr_number: {round}"
    );
    let channels = round["channels"]
        .as_array()
        .unwrap_or_else(|| panic!("cap round channels must be an array: {round}"));
    assert!(
        channels.len() >= 2,
        "cap round must carry at least two channels: {round}"
    );
    let roles: std::collections::BTreeSet<&str> = channels
        .iter()
        .map(|channel| {
            channel["role"]
                .as_str()
                .unwrap_or_else(|| panic!("cap round channel missing role: {channel}"))
        })
        .collect();
    assert!(
        roles.len() >= 2,
        "cap round channels must have at least two distinct roles: {round}"
    );
    assert!(
        channels
            .iter()
            .any(|channel| channel["verdict"] == json!("changes_requested")),
        "cap round must carry at least one changes_requested verdict: {round}"
    );

    // Oracle phase 1 (b): the ordered ASK-HUMAN chain, every floor rising so
    // each edge must FOLLOW the cap round (a fail-first-record-later turn
    // cannot pass). `calm.ratify.request` demands lifecycle==Working, so the
    // spec must first leave `reviewing`; the tool then emits working->blocked
    // + ratify.requested in ONE tx (mcp_server/tools/review.rs), so both must
    // appear.
    let (rw_id, rw_actor, rw_edge) =
        wait_for_wave_lifecycle_edge(&fx, round_id, "reviewing", "working", ratify_budget()).await;
    assert!(
        matches!(rw_actor, ActorId::AiSpecSession(_)),
        "reviewing->working edge actor must be AiSpecSession, got {rw_actor:?} for {rw_edge}"
    );
    let (wb_id, wb_actor, wb_edge) =
        wait_for_wave_lifecycle_edge(&fx, rw_id, "working", "blocked", ratify_budget()).await;
    assert!(
        matches!(wb_actor, ActorId::AiSpecSession(_)),
        "working->blocked edge actor must be AiSpecSession, got {wb_actor:?} for {wb_edge}"
    );
    // The request is REAL and structurally unforgeable: role_gate rule 2.8
    // makes ratify.requested spec-session-only and this test never calls
    // calm.ratify.request — only the real spec's own tool call can emit it.
    let (req_id, req_actor, req) = wait_for_ratify_requested(&fx, wb_id, ratify_budget()).await;
    assert!(
        matches!(req_actor, ActorId::AiSpecSession(_)),
        "ratify.requested actor must be AiSpecSession, got {req_actor:?} for {req}"
    );
    assert!(
        req["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "ratify.requested must carry a non-empty reason: {req}"
    );
    assert_eq!(req["wave_id"], json!(fx.wave_id.as_str()));

    // Oracle phase 1 (c): parked, not merged.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);
    assert_eq!(
        wave_lifecycle_row(&fx).await,
        "blocked",
        "wave row must be blocked while awaiting ratification"
    );

    // Grant = PRODUCTION HTTP route via in-process router-oneshot (design D4;
    // precedent tests/review_ratify.rs). actor_middleware defaults an absent
    // X-Calm-Actor header to the authenticated user; the route enforces the
    // pending request and emits blocked->working + ratify.resolved{grant}
    // same-tx as ActorId::User — a log_pure_event shortcut is User-only at the
    // role gate AND would have to hand-roll the waves-row flip.
    let app = fixture_router(&fx);
    let body = serde_json::to_vec(&json!({ "decision": "grant" })).expect("grant body");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", fx.spec_card_id))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("grant request"),
        )
        .await
        .expect("grant response");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("grant response body")
        .to_bytes();
    let grant_body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "grant must succeed: {grant_body}");
    assert_eq!(grant_body["decision"], json!("grant"), "{grant_body}");

    // Same-tx grant effects: waves row flipped + ratify.resolved{grant} by the
    // human actor.
    assert_eq!(
        wave_lifecycle_row(&fx).await,
        "working",
        "grant must flip the wave row blocked->working"
    );
    let resolved_rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, actor, payload FROM events WHERE kind = 'ratify.resolved' ORDER BY id ASC",
    )
    .fetch_all(fx.repo.pool())
    .await
    .expect("ratify.resolved rows");
    assert_eq!(
        resolved_rows.len(),
        1,
        "exactly one ratify.resolved after the grant: {resolved_rows:?}"
    );
    let (resolved_id, resolved_actor, resolved) = {
        let (id, actor, payload) = &resolved_rows[0];
        let actor: ActorId = serde_json::from_str(actor).expect("event actor json");
        let payload: Value = serde_json::from_str(payload).expect("event payload json");
        (*id, actor, payload)
    };
    assert_eq!(
        resolved_actor,
        ActorId::User,
        "ratify.resolved actor must be User: {resolved}"
    );
    assert_eq!(resolved["decision"], json!("grant"), "{resolved}");
    assert_eq!(resolved["wave_id"], json!(fx.wave_id.as_str()));
    assert!(
        resolved_id > req_id,
        "grant must follow the request: resolved={resolved_id}, requested={req_id}"
    );
    // The grant's own blocked->working edge lands in the same tx, strictly
    // between the request and the resolution row.
    let grant_edges: Vec<(ActorId, Value)> =
        lifecycle_changed_rows_between(&fx, req_id, resolved_id)
            .await
            .into_iter()
            .filter(|(_, payload)| {
                payload["from"] == json!("blocked")
                    && payload["to"] == json!("working")
                    && payload["id"] == json!(fx.wave_id.as_str())
            })
            .collect();
    assert_eq!(
        grant_edges.len(),
        1,
        "grant must emit exactly one blocked->working edge in-tx: {grant_edges:?}"
    );
    assert_eq!(
        grant_edges[0].0,
        ActorId::User,
        "grant blocked->working edge actor must be User: {grant_edges:?}"
    );

    // Recovery wake: the same Observation the prod dispatcher's
    // `harness_observation_from_event` would push for ratify.resolved
    // (design D5; hard-fire).
    inject_ratify_resolved_grant(&harness, &fx).await;

    // Oracle phase 2 — PRIMARY resumption signal (independent checker's pin):
    // the real spec re-enters review, working->reviewing, after the grant.
    // Since #888 the descriptor states the wave is already back in `working`
    // after a grant and instructs a plain working->reviewing resume (the
    // historical "blocked->working->reviewing" wording produced a tolerated
    // illegal working->working attempt, now fixed); resumption is proven by
    // the working->reviewing edge alone.
    let (_resume_id, resume_actor, resume_edge) =
        wait_for_wave_lifecycle_edge(&fx, resolved_id, "working", "reviewing", ratify_budget())
            .await;
    assert!(
        matches!(resume_actor, ActorId::AiSpecSession(_)),
        "post-grant working->reviewing edge actor must be AiSpecSession, got {resume_actor:?} for {resume_edge}"
    );

    // Post-grant convergence/merge is deliberately NOT asserted here: this
    // subject is design-phase (pr_number: null — no PR exists in this
    // fixture) and no post-grant channel verdicts are injected. The #888
    // kernel cap-extension arm resolved the old cap contradiction (the next
    // round n=9/cap=10 is now kernel-legal after the grant); the extension →
    // convergence → merge finish is exercised by the R7c E2E
    // (`real_spec_extends_cap_after_grant_converges_and_merges`) on a
    // real-PR impl subject. Merge must still be absent — structurally
    // impossible without a PR in this fixture.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);

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

// #840 slice (d2) — S11/S12: the real spec session, woken only by injected
// observations, obeys merge fence F4 (call gh.pr.merge for a subject ONLY
// when that subject's latest review.round has converged:true, passing
// expected_head_sha equal to that round's head_sha) and then closes the
// source issue, merge-before-close.
//
// Seat caveat (design pin): the seat was steered by goal text — the
// production descriptor's merge step is kind:codex, so a real production run
// may DISPATCH a worker to execute the merge instead of the spec calling
// gh.pr.merge itself; the worker-executed merge topology stays open for
// slice (e)/capstone. The goal ALSO restates F4's WHEN trigger ("once the
// impl review round reports converged, execute the merge step"), so for the
// WHEN half of F4, descriptor-obedience and goal-obedience are
// indistinguishable here. The genuinely unsteered F4 autonomy content is the
// expected_head_sha selection: the goal never mentions any sha, so choosing
// the converged round's head_sha (observed, not given) is the spec's own.
//
// Seat proof, construction W (d1 precedent) — LOAD-BEARING: no scripted call
// to `gh.pr.merge` or `gh.issue.close` exists in this file beyond this
// comment — scripted setup stops at `gh.pr.create`/`gh.pr.checks` (against
// this test's isolated fixture only). The only possible emitter of
// `forge.pr.merged` / `forge.issue.closed` is therefore the real spec
// session's own MCP `tools/call`. The oracle's forge-action op idem-key
// checks CORROBORATE only on the worker-seat axis (the embedded caller card
// id excludes other-card callers); they cannot discriminate
// scripted-vs-autonomous, because scripted setup calls use the same spec
// thread → same spec card → a hypothetical scripted merge would produce a
// byte-identical key.
#[tokio::test]
async fn real_spec_agent_autonomously_merges_pr_and_closes_issue_from_descriptor() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // Goal carries environment facts only (repo gitdir + issue number,
    // forge_pr_goal precedent) plus descriptor-legal branch steering. The
    // pr_number and head_sha must reach the spec ONLY via observations —
    // never as pre-chewed tool args. The goal needs the fixture's origin
    // path, which only exists post-boot, so it flows through the
    // spec-harness start op (the spec's actual WaveGoal source,
    // `initial_snapshot_with_goal`); `FixtureSpec.goal` only mirrors into
    // the card payload `prompt`, which the spec-harness path never reads.
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: None,
            workflow_id: Some("issue-development".into()),
            plan_source: PlanSource::RealSpecTurn,
            issue_body: None,
            mint_report_card: false,
            require_task_gates: false,
            descriptor_gate_cmd: None,
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

    boot_spec_harness_via_start_op(&fx, goal).await;

    let (plan_actor, plan) = wait_for_plan_updated(&fx, spec_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiSpecSession(_)),
        "plan.updated actor must be the real spec session, got {plan_actor:?}"
    );
    // The review subject slice comes from the spec's OWN real plan so the
    // seeded converged round matches the spec's mental model (R7a precedent).
    let slice_id = plan["changed_keys"][0]
        .as_str()
        .unwrap_or_else(|| panic!("plan.updated changed_keys[0] must be a string: {plan}"))
        .to_string();

    let harness = recover_spec_harness(&fx).await.expect("live spec harness");
    // Settle the planning turn before setup/seeding (R6 causality guard): the
    // accepted merge+close must be causally a response to the injected
    // observations below, not a planning-time fabrication.
    wait_for_spec_turn_settled(&fx, &harness, spec_planning_budget()).await;

    // Pre-position the wave at `reviewing` via a raw WavePatch (R7a
    // precedent) — walking the FSM by real turns is capstone scope.
    fx.repo_dyn
        .wave_update(
            fx.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Reviewing),
                ..WavePatch::default()
            },
        )
        .await
        .expect("pre-position wave lifecycle to reviewing");

    // Scripted REAL PR setup (setup, not the proof): branch + commit in the
    // wave cwd with raw git, push to the bare origin, then scripted MCP
    // tools/call of gh.pr.create + gh.pr.checks through the daemon socket
    // (identity = the live spec thread) so GENUINE forge.pr.opened /
    // forge.pr.checks events back the injected observations — the events go
    // through the real plugin lowering, not fabricated shim state.
    let branch = "neige-d2-impl-slice";
    run_git(&fx.wave_cwd, ["checkout", "-B", branch, "origin/main"]);
    stage_git_change(&fx.wave_cwd, "FORGE_E2E_D2.md", "forge-e2e-d2\n");
    run_git(&fx.wave_cwd, ["commit", "-m", "d2 scripted impl commit"]);
    let head_sha = run_git_capture(&fx.wave_cwd, ["rev-parse", "HEAD"]);
    assert!(
        is_hex_sha(&head_sha),
        "scripted branch tip should be a 40-char hex sha, got {head_sha:?}"
    );
    run_git(&fx.wave_cwd, ["push", "-u", "origin", branch]);
    run_git(&fx.wave_cwd, ["checkout", "main"]);

    let spec_thread_id = spec_session_thread_id(&fx).await;
    let create_resp = call_tool_via_socket(
        &fx.socket_path,
        &fx.daemon_token,
        &spec_thread_id,
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
    let (opened_id, _, opened) = wait_for_wave_forge_event(
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
        &spec_thread_id,
        202,
        PR_CHECKS_TOOL,
        json!({ "repo": repo_arg, "pr": pr_number }),
    )
    .await;
    assert_forge_tool_accepted(&checks_resp, "gh.pr.checks");
    let (checks_id, _, _checks) = wait_for_wave_forge_event(
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
    // pre-check each) so runs/ shows implement/open-pr/review-a/review-b done.
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
    seed_completed_task_pair(
        &fx,
        "review-pr-a",
        json!({ "summary": "approved", "verdict": "approved", "channel": "a" }),
        "approved",
    )
    .await;
    seed_completed_task_pair(
        &fx,
        "review-pr-b",
        json!({ "summary": "approved", "verdict": "approved", "channel": "b" }),
        "approved",
    )
    .await;

    // Seed ONE converged typed impl review.round carrying the REAL branch tip
    // (push precedes seeding so the round carries the real tip sha). Actor
    // MUST be AiSpec(spec card) + wave scope: role_gate rule 2.8 makes
    // review.round spec-only, and the seeded AiSpec row stays
    // actor-distinguishable from anything the real AiSpecSession emits.
    seed_converged_impl_review_round(&fx, &slice_id, pr_number, &head_sha).await;
    let round_id = latest_event_id_of_kind(&fx, "review.round").await;

    let floor = max_event_id(&fx.repo).await;
    // Proof-validity guards: nothing merged yet, and exactly the one seeded
    // round exists — a planning-time merge or fabricated round would poison
    // the F4 evidence.
    assert_eq!(
        event_payloads(&fx.repo, "forge.pr.merged").await.len(),
        0,
        "proof-validity guard: no forge.pr.merged may exist pre-wake"
    );
    let pre_wake_rounds = actor_payload_rows(&fx.repo, "review.round").await;
    assert_eq!(
        pre_wake_rounds.len(),
        1,
        "exactly the one seeded review.round may exist pre-wake (proof-validity guard): {pre_wake_rounds:?}"
    );

    // Wake: inject exactly what the prod dispatcher's
    // `harness_observation_from_event` would push for the rows that exist
    // (dispatcher.rs shapes; no dispatcher runs in the spec-harness E2E).
    // The converged ReviewRound observation is the F4 trigger and the spec's
    // ONLY channel for pr_number/head_sha selection (rounds have no runs/
    // projection).
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
    for (key, chan) in [("review-pr-a", "a"), ("review-pr-b", "b")] {
        inject_observation(
            &harness,
            Observation::TaskCompleted {
                idempotency_key: task_id(&fx, key),
                result: json!({ "summary": "approved", "verdict": "approved", "channel": chan }),
            },
        )
        .await;
    }
    inject_observation(
        &harness,
        Observation::ForgePrOpened {
            wave_id: fx.wave_id.clone(),
            pr_number,
        },
    )
    .await;
    inject_observation(
        &harness,
        Observation::ForgePrChecks {
            wave_id: fx.wave_id.clone(),
            pr_number,
            conclusion: "success".into(),
        },
    )
    .await;
    inject_observation(
        &harness,
        Observation::ReviewRound {
            wave_id: fx.wave_id.clone(),
            phase: "impl".into(),
            slice_id: slice_id.clone(),
            pr_number: Some(pr_number),
            head_sha: Some(head_sha.clone()),
            n: 1,
            cap: 8,
            converged: true,
        },
    )
    .await;

    // Oracle (a): the S11 merge event. All forge.* events are appended by the
    // kernel's forge-action observer as ActorId::KernelDispatcher
    // (forge_action_adapter.rs `complete_forge_op_succeeded`) — event actor
    // therefore CANNOT attribute the seat; the load-bearing attribution is
    // construction W (no scripted merge/close call in this file), with the
    // oracle (b) op idem-key check corroborating on the worker-seat axis
    // only (it pins the caller card, not scripted-vs-autonomous).
    let (merged_id, merged_actor, merged) = wait_for_wave_forge_event(
        &fx,
        "forge.pr.merged",
        floor,
        review_budget(),
        "spec-initiated merge",
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
        "merged head must equal the seeded round head_sha == real branch tip: {merged}"
    );
    let merge_sha = merged["merge_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("forge.pr.merged missing merge_sha: {merged}"));
    assert!(
        is_hex_sha(merge_sha),
        "merge_sha should be a git-shaped oid: {merged}"
    );
    assert_eq!(
        merged["subject"]["phase"],
        json!("impl"),
        "merged subject phase: {merged}"
    );
    assert_eq!(
        merged["subject"]["slice_id"],
        json!(slice_id),
        "merged subject slice: {merged}"
    );
    assert_eq!(
        merged["subject"]["pr_number"],
        json!(pr_number),
        "merged subject pr: {merged}"
    );

    // Oracle (b) — the F4 proof: the spec must have passed expected_head_sha.
    // The forge-action op idempotency key is
    // `{plugin}:{wave}:{caller card}:{plugin idem}` (transport.rs
    // `submit_forge_action`), and the plugin idem is the WITH-sha shape
    // `gh.pr.merge:{repo}:{pr}:{expected_head_sha}` only when
    // expected_head_sha was passed (plugins/git-forge/main.rs
    // `lower_gh_pr_merge`) — an omitted-sha merge produces
    // `gh.pr.merge:{repo}:{pr}` and MUST fail this assert. The embedded card
    // id corroborates the seat on the worker-exclusion axis ONLY: it proves
    // the caller was the spec card, but scripted setup calls use the same
    // spec thread (`spec_session_thread_id`) → same card → a scripted merge
    // would produce a byte-identical key, so construction W (file-level
    // comment above the test) stays the sole scripted-vs-autonomous
    // discriminator.
    let expected_merge_key = format!(
        "{PLUGIN_ID}:{}:{}:gh.pr.merge:{}:{}:{}",
        fx.wave_id.as_str(),
        fx.spec_card_id.as_str(),
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
            "every gh.pr.merge forge-action op must carry the with-sha idempotency key (F4): {merge_keys:?}"
        );
    }

    // Oracle (c) — ordering (oracle-only; no kernel check ties gh.pr.merge to
    // review.round): the converged round precedes the merge (F4), and the
    // checks event precedes the merge (attribute to setup ordering, not spec
    // autonomy — S7 is d1's theorem).
    assert!(
        round_id < merged_id,
        "converged review.round (id={round_id}) must precede forge.pr.merged (id={merged_id})"
    );
    assert!(
        checks_id < merged_id,
        "forge.pr.checks (id={checks_id}) must precede forge.pr.merged (id={merged_id})"
    );

    // Oracle (d): S12 — the issue close FOLLOWS the merge (#840 §4 invariant
    // 5, oracle-only), on the right issue; the op idem key corroborates the
    // caller card (worker-seat exclusion; construction W carries the
    // scripted-vs-autonomous axis).
    let (closed_id, closed_actor, closed) = wait_for_wave_forge_event(
        &fx,
        "forge.issue.closed",
        floor,
        review_budget(),
        "spec-initiated issue close",
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
        fx.wave_id.as_str(),
        fx.spec_card_id.as_str(),
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
            "every gh.issue.close forge-action op must target the goal issue from the spec seat: {close_keys:?}"
        );
    }

    // Exactly-once events (a spec retry with the same args dedups on the
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

    // Oracle (e): shim counters — the remote side effect happened exactly
    // once (the shim is idempotent and counts real merges/closes only).
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

    // Oracle (f): purity. Happy path needs no ratification grant; extra
    // lifecycle transitions (e.g. reviewing->done) are tolerated, but the
    // wave must not have failed; the plan must be the spec's own.
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "happy-path merge run must not request ratification"
    );
    let lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id = ?1")
        .bind(fx.wave_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select wave lifecycle");
    assert_ne!(lifecycle, "failed", "wave must not fail on the happy path");
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

// #888 R7c — post-grant cap extension to the F4 finish. The real spec,
// cap-exhausted on a real-PR impl subject (seeded prior round n=7/cap=8, its
// own cap round n=8/cap=8 non-converged), walks the full ASK-HUMAN chain; a
// human grant (production HTTP route) then authorizes the descriptor's
// "previous cap plus exactly 2" window, and the spec's SINGLE post-grant
// round is both the extension AND the convergence (n=9, cap=10,
// converged:true on the real branch tip — approved verdicts are injected
// post-grant, before the spec's next round). Kernel acceptance of that
// n=9/cap=10 row IS the E2E proof of the #888 extension arm and of
// descriptor satisfiability; the merge then lands through the spec's own
// gh.pr.merge (F4). In-window multi-round continuation after an extension is
// unit-tested (review_ratify.rs t2), deliberately not E2E'd here.
//
// Composition = R7b's ratify flow ⊕ the d2 merge test's PR scaffolding. The
// d2 seat caveat + construction W carry over verbatim: no scripted call to
// `gh.pr.merge` exists in this file — scripted setup stops at
// `gh.pr.create`/`gh.pr.checks` against this test's isolated fixture, so the
// only possible emitter of `forge.pr.merged` is the real spec session's own
// MCP `tools/call`. No dependency on #863: the spec executes gh.pr.merge
// through its own MCP socket exactly as the merged d2 test does.
#[tokio::test]
async fn real_spec_extends_cap_after_grant_converges_and_merges() {
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
            workflow_id: Some("issue-development".into()),
            plan_source: PlanSource::RealSpecTurn,
            issue_body: None,
            mint_report_card: false,
            require_task_gates: false,
            descriptor_gate_cmd: None,
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
    let goal = extension_merge_goal(&repo_arg);

    boot_spec_harness_via_start_op(&fx, goal).await;

    let (plan_actor, plan) = wait_for_plan_updated(&fx, spec_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiSpecSession(_)),
        "plan.updated actor must be the real spec session, got {plan_actor:?}"
    );
    // The review subject slice comes from the spec's OWN real plan so the
    // seeded prior round matches the spec's mental model (R7a/d2 precedent).
    let slice_id = plan["changed_keys"][0]
        .as_str()
        .unwrap_or_else(|| panic!("plan.updated changed_keys[0] must be a string: {plan}"))
        .to_string();

    let harness = recover_spec_harness(&fx).await.expect("live spec harness");
    // Settle the planning turn before setup/seeding (R6 causality guard).
    wait_for_spec_turn_settled(&fx, &harness, spec_planning_budget()).await;

    // Pre-position the wave at `reviewing` (R7a/R7b precedent).
    fx.repo_dyn
        .wave_update(
            fx.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Reviewing),
                ..WavePatch::default()
            },
        )
        .await
        .expect("pre-position wave lifecycle to reviewing");

    // Scripted REAL PR setup (d2 precedent; setup, not the proof): genuine
    // forge.pr.opened/checks events back the injected observations.
    let branch = "neige-r7c-impl-slice";
    run_git(&fx.wave_cwd, ["checkout", "-B", branch, "origin/main"]);
    stage_git_change(&fx.wave_cwd, "FORGE_E2E_R7C.md", "forge-e2e-r7c\n");
    run_git(&fx.wave_cwd, ["commit", "-m", "r7c scripted impl commit"]);
    let head_sha = run_git_capture(&fx.wave_cwd, ["rev-parse", "HEAD"]);
    assert!(
        is_hex_sha(&head_sha),
        "scripted branch tip should be a 40-char hex sha, got {head_sha:?}"
    );
    run_git(&fx.wave_cwd, ["push", "-u", "origin", branch]);
    run_git(&fx.wave_cwd, ["checkout", "main"]);

    let spec_thread_id = spec_session_thread_id(&fx).await;
    let create_resp = call_tool_via_socket(
        &fx.socket_path,
        &fx.daemon_token,
        &spec_thread_id,
        211,
        PR_CREATE_TOOL,
        json!({
            "repo": repo_arg,
            "head": branch,
            "base": "main",
            "title": "r7c scripted impl PR",
            "body": "Scripted setup PR for the #888 R7c cap-extension E2E"
        }),
    )
    .await;
    assert_forge_tool_accepted(&create_resp, "gh.pr.create");
    let (opened_id, _, opened) = wait_for_wave_forge_event(
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
        &spec_thread_id,
        212,
        PR_CHECKS_TOOL,
        json!({ "repo": repo_arg, "pr": pr_number }),
    )
    .await;
    assert_forge_tool_accepted(&checks_resp, "gh.pr.checks");
    let (_checks_id, _, _checks) = wait_for_wave_forge_event(
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

    // Seed both PR review channels changes_requested (runs/ pre-check each) —
    // the pre-grant window is genuinely non-approving.
    for (key, chan) in [("review-pr-a", "a"), ("review-pr-b", "b")] {
        seed_completed_task_pair(
            &fx,
            key,
            json!({
                "summary": "changes_requested",
                "verdict": "changes_requested",
                "channel": chan,
            }),
            "changes_requested",
        )
        .await;
    }

    // Seed ONE prior impl round n=7/cap=8 carrying the REAL branch tip +
    // pr_number: the kernel then accepts only n=8 next on this subject.
    seed_prior_impl_review_round(&fx, &slice_id, pr_number, &head_sha, 7, 8).await;

    let floor = max_event_id(&fx.repo).await;
    let pre_wake_rounds = actor_payload_rows(&fx.repo, "review.round").await;
    assert_eq!(
        pre_wake_rounds.len(),
        1,
        "exactly the one seeded review.round may exist pre-wake (proof-validity guard): {pre_wake_rounds:?}"
    );
    assert_eq!(
        event_payloads(&fx.repo, "forge.pr.merged").await.len(),
        0,
        "proof-validity guard: no forge.pr.merged may exist pre-wake"
    );

    // Wake: dispatcher-shaped observations only (design D5; no dispatcher
    // runs in the spec-harness E2E).
    inject_task_changes_requested(&harness, &task_id(&fx, "review-pr-a")).await;
    inject_task_changes_requested(&harness, &task_id(&fx, "review-pr-b")).await;
    inject_observation(
        &harness,
        Observation::ForgePrOpened {
            wave_id: fx.wave_id.clone(),
            pr_number,
        },
    )
    .await;
    inject_observation(
        &harness,
        Observation::ForgePrChecks {
            wave_id: fx.wave_id.clone(),
            pr_number,
            conclusion: "success".into(),
        },
    )
    .await;
    inject_observation(
        &harness,
        Observation::ReviewRound {
            wave_id: fx.wave_id.clone(),
            phase: "impl".into(),
            slice_id: slice_id.clone(),
            pr_number: Some(pr_number),
            head_sha: Some(head_sha.clone()),
            n: 7,
            cap: 8,
            converged: false,
        },
    )
    .await;

    // Phase 1 (a) — the spec's own cap round n=8/cap=8, non-converged, on
    // the seeded impl subject (R7b phase-1 oracle, impl-subject variant).
    let (cap_round_id, cap_round_actor, cap_round) =
        wait_for_impl_review_round_on_subject(&fx, floor, &slice_id, pr_number, review_budget())
            .await;
    assert!(
        matches!(cap_round_actor, ActorId::AiSpecSession(_)),
        "cap round actor must be AiSpecSession, got {cap_round_actor:?} for {cap_round}"
    );
    assert_eq!(
        cap_round["n"],
        json!(8),
        "cap round must be n=8: {cap_round}"
    );
    assert_eq!(
        cap_round["cap"],
        json!(8),
        "cap must be descriptor-fixed 8 in the first window: {cap_round}"
    );
    assert_eq!(
        cap_round["converged"],
        json!(false),
        "cap round must be non-converged: {cap_round}"
    );
    assert!(
        cap_round["channels"]
            .as_array()
            .is_some_and(|channels| channels.len() >= 2
                && channels
                    .iter()
                    .any(|channel| channel["verdict"] == json!("changes_requested"))),
        "cap round must carry >=2 channels with >=1 changes_requested: {cap_round}"
    );

    // Phase 1 (b) — the ordered ASK-HUMAN chain (rising floors, R7b oracle).
    let (rw_id, rw_actor, rw_edge) =
        wait_for_wave_lifecycle_edge(&fx, cap_round_id, "reviewing", "working", ratify_budget())
            .await;
    assert!(
        matches!(rw_actor, ActorId::AiSpecSession(_)),
        "reviewing->working edge actor must be AiSpecSession, got {rw_actor:?} for {rw_edge}"
    );
    let (wb_id, wb_actor, wb_edge) =
        wait_for_wave_lifecycle_edge(&fx, rw_id, "working", "blocked", ratify_budget()).await;
    assert!(
        matches!(wb_actor, ActorId::AiSpecSession(_)),
        "working->blocked edge actor must be AiSpecSession, got {wb_actor:?} for {wb_edge}"
    );
    let (req_id, req_actor, req) = wait_for_ratify_requested(&fx, wb_id, ratify_budget()).await;
    assert!(
        matches!(req_actor, ActorId::AiSpecSession(_)),
        "ratify.requested actor must be AiSpecSession, got {req_actor:?} for {req}"
    );

    // Phase 1 (c) — parked, not merged.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);
    assert_eq!(
        wave_lifecycle_row(&fx).await,
        "blocked",
        "wave row must be blocked while awaiting ratification"
    );

    // Grant = PRODUCTION HTTP route via in-process router-oneshot (R7b
    // precedent): blocked->working + ratify.resolved{grant}, both User.
    let app = fixture_router(&fx);
    let body = serde_json::to_vec(&json!({ "decision": "grant" })).expect("grant body");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", fx.spec_card_id))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("grant request"),
        )
        .await
        .expect("grant response");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("grant response body")
        .to_bytes();
    let grant_body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "grant must succeed: {grant_body}");
    assert_eq!(
        wave_lifecycle_row(&fx).await,
        "working",
        "grant must flip the wave row blocked->working"
    );
    let resolved_rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, actor, payload FROM events WHERE kind = 'ratify.resolved' ORDER BY id ASC",
    )
    .fetch_all(fx.repo.pool())
    .await
    .expect("ratify.resolved rows");
    assert_eq!(
        resolved_rows.len(),
        1,
        "exactly one ratify.resolved after the grant: {resolved_rows:?}"
    );
    let (resolved_id, resolved_actor, resolved) = {
        let (id, actor, payload) = &resolved_rows[0];
        let actor: ActorId = serde_json::from_str(actor).expect("event actor json");
        let payload: Value = serde_json::from_str(payload).expect("event payload json");
        (*id, actor, payload)
    };
    assert_eq!(
        resolved_actor,
        ActorId::User,
        "ratify.resolved actor must be User: {resolved}"
    );
    assert_eq!(resolved["decision"], json!("grant"), "{resolved}");
    let grant_edges: Vec<(ActorId, Value)> =
        lifecycle_changed_rows_between(&fx, req_id, resolved_id)
            .await
            .into_iter()
            .filter(|(_, payload)| {
                payload["from"] == json!("blocked")
                    && payload["to"] == json!("working")
                    && payload["id"] == json!(fx.wave_id.as_str())
            })
            .collect();
    assert_eq!(
        grant_edges.len(),
        1,
        "grant must emit exactly one blocked->working edge in-tx: {grant_edges:?}"
    );
    assert_eq!(
        grant_edges[0].0,
        ActorId::User,
        "grant blocked->working edge actor must be User: {grant_edges:?}"
    );

    // Recovery wake + post-grant APPROVED verdicts for both channels,
    // injected BEFORE the spec's next round so its single post-grant round is
    // both the extension and the convergence (design C1 resolution;
    // merge-test observation shapes).
    inject_ratify_resolved_grant(&harness, &fx).await;
    for (key, chan) in [("review-pr-a", "a"), ("review-pr-b", "b")] {
        inject_observation(
            &harness,
            Observation::TaskCompleted {
                idempotency_key: task_id(&fx, key),
                result: json!({ "summary": "approved", "verdict": "approved", "channel": chan }),
            },
        )
        .await;
    }

    // Oracle 1 — THE extension round: n=9 (= old cap + 1), cap=10 (= old cap
    // + 2), converged, both channels approved, real branch tip, spec-authored.
    // Kernel acceptance of this row is the E2E proof of the #888 arm.
    let (ext_round_id, ext_round_actor, ext_round) = wait_for_impl_review_round_on_subject(
        &fx,
        resolved_id,
        &slice_id,
        pr_number,
        review_budget(),
    )
    .await;
    assert!(
        matches!(ext_round_actor, ActorId::AiSpecSession(_)),
        "extension round actor must be AiSpecSession, got {ext_round_actor:?} for {ext_round}"
    );
    assert_eq!(
        ext_round["n"],
        json!(9),
        "extension round must be n=9 (= cap_old + 1): {ext_round}"
    );
    assert_eq!(
        ext_round["cap"],
        json!(10),
        "extension round must carry cap=10 (= cap_old + 2): {ext_round}"
    );
    assert_eq!(
        ext_round["converged"],
        json!(true),
        "extension round must be converged: {ext_round}"
    );
    assert_eq!(
        ext_round["head_sha"],
        json!(head_sha),
        "extension round must carry the real branch tip: {ext_round}"
    );
    assert!(
        ext_round["channels"]
            .as_array()
            .is_some_and(|channels| channels.len() >= 2
                && channels
                    .iter()
                    .all(|channel| channel["verdict"] == json!("approved"))),
        "extension round channels must all be approved: {ext_round}"
    );

    // Oracle 2 — the merge follows the extension round (F4 linkage),
    // kernel-appended, head-matched, on the full impl subject.
    let (merged_id, merged_actor, merged) = wait_for_wave_forge_event(
        &fx,
        "forge.pr.merged",
        ext_round_id,
        review_budget(),
        "post-extension merge",
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
        "merged head must equal the extension round's head_sha (F4): {merged}"
    );
    assert_eq!(merged["subject"]["phase"], json!("impl"), "{merged}");
    assert_eq!(merged["subject"]["slice_id"], json!(slice_id), "{merged}");
    assert_eq!(merged["subject"]["pr_number"], json!(pr_number), "{merged}");
    assert_eq!(
        event_payloads(&fx.repo, "forge.pr.merged").await.len(),
        1,
        "exactly one forge.pr.merged event"
    );

    // Oracle 3 — F4 op idem key: every gh.pr.merge forge-action row carries
    // the WITH-sha shape from the spec seat (d2 oracle (b)).
    let expected_merge_key = format!(
        "{PLUGIN_ID}:{}:{}:gh.pr.merge:{}:{}:{}",
        fx.wave_id.as_str(),
        fx.spec_card_id.as_str(),
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
            "every gh.pr.merge forge-action op must carry the with-sha idempotency key (F4): {merge_keys:?}"
        );
    }

    // Oracle 1 (exactly-once half) — exactly ONE post-grant review.round on
    // the impl subject: the extension round itself.
    let post_grant_rounds = event_rows(&fx.repo, "review.round")
        .await
        .into_iter()
        .filter(|row| {
            row.id > resolved_id && {
                let subject = &row.payload["subject"];
                subject["phase"] == json!("impl")
                    && subject["slice_id"] == json!(slice_id)
                    && subject["pr_number"] == json!(pr_number)
            }
        })
        .count();
    assert_eq!(
        post_grant_rounds, 1,
        "exactly one post-grant review.round on the impl subject (the extension round)"
    );

    // Oracle 4 — ordering by row id: cap round < ratify.requested <
    // ratify.resolved{grant} < extension round < forge.pr.merged. (Oracle 5,
    // actor shapes, is asserted at each wait above.)
    assert!(
        cap_round_id < req_id
            && req_id < resolved_id
            && resolved_id < ext_round_id
            && ext_round_id < merged_id,
        "ordering violated: cap_round={cap_round_id}, requested={req_id}, \
         resolved={resolved_id}, extension={ext_round_id}, merged={merged_id}"
    );

    // Oracle 6 — full-history INV-CAP-EXT validation by adjacent pairs:
    // exactly ONE extension on the impl subject, zero on every other subject.
    let extensions = assert_cap_extension_history(&fx.repo, fx.wave_id.as_str()).await;
    let impl_subject = SubjectKey {
        phase: "impl".into(),
        slice_id: slice_id.clone(),
        pr_number: Some(pr_number),
    };
    assert_eq!(
        extensions.get(&impl_subject).copied(),
        Some(1),
        "exactly one cap extension on the impl subject: {extensions:?}"
    );
    for (key, count) in &extensions {
        if key != &impl_subject {
            assert_eq!(
                *count, 0,
                "no cap extension may exist on any other subject: {key:?}"
            );
        }
    }

    // Oracle 7 — the plan was the spec's own.
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

/// R7c goal (#888): environment facts (repo selector) + descriptor-legal
/// steering of the ASK-HUMAN branch (R7b precedent) + the restated merge
/// trigger (d2 merge-test precedent, seat caveat carried over). The `+2`
/// cap-extension rule itself is deliberately NOT restated: obeying it
/// post-grant must come from the bound workflow descriptor.
fn extension_merge_goal(repo_gitdir: &str) -> String {
    format!(
        "Drive the tail of the issue-development workflow. Environment facts: the `repo` \
         argument for every gh.* MCP forge tool is exactly `{repo_gitdir}`. Implementation, \
         the pull request, and both PR review channels are already complete for this wave; \
         their results arrive as observations. If the impl review cannot converge at the \
         review cap, ask for human ratification instead of giving up; do not fail the wave. \
         Once the impl review round for the pull request reports converged, execute the \
         merge step yourself with the MCP forge tools (gh.pr.merge); do not dispatch \
         further tasks."
    )
}

/// Seed ONE prior non-converged typed impl `review.round` at `n`/`cap`
/// carrying the REAL branch tip + pr_number (#888 R7c; shape =
/// `seed_prior_design_review_round` × the impl subject/idem-key shape of
/// `seed_converged_impl_review_round`). Actor MUST be
/// `ActorId::AiSpec(spec card)` with `EventScope::Wave`: role_gate rule 2.8
/// makes review.round spec-only, and the seeded AiSpec row stays
/// actor-distinguishable from the real spec's AiSpecSession rows.
async fn seed_prior_impl_review_round(
    fx: &Fixture,
    slice_id: &str,
    pr_number: u64,
    head_sha: &str,
    n: u32,
    cap: u32,
) {
    let wave_scope = EventScope::Wave {
        wave: fx.wave_id.clone(),
        cove: fx.cove_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::AiSpec(fx.spec_card_id.clone()),
            wave_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.wave_cove_cache,
            Event::ReviewRound {
                wave_id: fx.wave_id.clone(),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: slice_id.into(),
                    pr_number: Some(pr_number),
                },
                head_sha: Some(head_sha.to_string()),
                n,
                cap,
                converged: false,
                channels: vec![
                    ChannelVerdict {
                        role: "pr-correctness".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    },
                    ChannelVerdict {
                        role: "pr-failure-path".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    },
                ],
                root_cause: None,
                // Canonical shape from `review_round_idempotency_key`
                // (mcp_server/tools/review.rs): PR subjects carry the pr
                // number in the pr slot.
                idempotency_key: format!(
                    "review.round:{}:impl:{}:{}:{}",
                    fx.wave_id.as_str(),
                    slice_id,
                    pr_number,
                    n
                ),
            },
        )
        .await
        .expect("log seeded prior impl review.round");
}

/// First post-floor `review.round` on the seeded impl subject (phase, slice
/// AND pr_number — the kernel keys review history by the FULL subject, so a
/// pr-less round is a different stream and must not be returned). Returns the
/// event id so callers can pin ordering invariants against it.
async fn wait_for_impl_review_round_on_subject(
    fx: &Fixture,
    floor: i64,
    slice_id: &str,
    pr_number: u64,
    budget: Duration,
) -> (i64, ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, actor, payload FROM events \
             WHERE kind = 'review.round' AND id > ?1 ORDER BY id ASC",
        )
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("review.round event rows after floor {floor}: {e}"));
        let hit = rows.into_iter().find_map(|(id, actor, payload)| {
            let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
            let payload: Value = serde_json::from_str(&payload).expect("event payload json");
            let on_subject = {
                let subject = &payload["subject"];
                subject["phase"] == json!("impl")
                    && subject["slice_id"] == json!(slice_id)
                    && subject["pr_number"] == json!(pr_number)
            };
            on_subject.then_some((id, actor, payload))
        });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for post-floor impl review.round \
                     on slice {slice_id} pr {pr_number} after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

// ======================= #840 CAPSTONE (S0–S13) =========================
//
// One REAL run of the full issue→PR→merge→close backbone. The fixture serves
// a real (non-toy) code task from a shim-served issue body, and everything
// between the goal and the oracle is production machinery: real spec turns
// via the `spec-harness-start` op, a LIVE kernel dispatcher (scheduler
// claims, workspace lease + git worktree, real codex workers for
// inspect / design-review ×2 / implement / open-pr / review-pr ×2 / merge,
// kernel commit, env-cleared gate run), and spec wake-ups EXCLUSIVELY via the
// dispatcher's `harness_observation_from_event` push path (the dispatcher is
// spawned with the FIXTURE's HarnessRegistry — `spawn_dispatcher_with_harness`
// — so pushes reach the harness the start op registered). ZERO injected
// observations, ZERO seeded task/review rows, ZERO WavePatch lifecycle
// pre-positioning — R6/R7/d2 injected only because no dispatcher ran there.
//
// Shimmed (the sanctioned #840 §5 set ONLY): the GitHub remote (gh shim), CI
// conclusion (statusCheckRollup hardwired "success"), the fixture issue body
// (shim-served seeded file), and human ratification — avoided entirely: the
// goal steers the failure branch to GIVE-UP, and ANY `ratify.requested` fails
// the test (purity, asserted continuously in every stage wait and post-run).
//
// Gate honesty (checker pin a): the task-verify runner resolves its cwd as
// `gate.cwd → task.cwd → waves.cwd` (task_verify_adapter.rs §6.4) and nothing
// ever writes `task.cwd` back, so the patched `sh ./e2e-gate.sh` gate
// compiles/runs the SEEDED `src/lib.rs` in the wave clone — NOT the worker's
// branch content. The gate therefore proves the pipeline edge (commit →
// verifying → task.gate_result{passed} → merge fence ordering); the CONTENT
// proof lives in the oracle's diff invariant: the merged head's diff against
// the seeded base must add `is_palindrome` to `src/lib.rs`.
//
// No-cargo discipline (P1 + checker pin d): the descriptor gate cmd is
// test-patched BEFORE `Manifest::parse` (fixture `descriptor_gate_cmd`) with
// a boot guard that no registered gate cmd contains `cargo`; repo seeding
// preflights rustc under the gate wrapper's exact env-cleared conditions; and
// the post-run oracle asserts no `tasks.gate_json` row contains `cargo`. A
// bare `cargo test` gate dispatched from inside this suite is the #863-B
// recursive-suite amplifier.
//
// Run discipline (#863-D): the REAL capstone run happens ONLY inside the #863
// isolation wrapper, setsid-detached, never harness-tracked, never on the
// shared production box (memories `project_e2e_ingest_kills_production`,
// `feedback_real_codex_e2e_crashes_harness`). In deterministic contexts this
// test self-skips (no NEIGE_CODEX_BIN).
//
// Report card: minted by the fixture (`mint_report_card: true`); route parity
// is a documented carve-out — the report card is setup, not proof.
//
// Steering vs autonomy: the wave goal carries environment facts (repo
// selector = the CLONE gitdir, issue number, base sha — `forge_pr_goal`
// precedent) plus descriptor-legal planning steering (deferred-task batches:
// plan.upsert rule 3 REJECTS deps naming nonexistent tasks, so review-pr-a/b
// AND merge are added together in ONE batch once the PR coordinates exist —
// checker pin b; open-pr is likewise deferred until the implement branch is
// known, killing the goal-rewrite dispatch race). Round counts, fix loops,
// reviewer wording, plan shape, and extra events are all tolerated; the
// invariants live in the oracle.

/// The capstone's source issue number. An environment fact for the gh shim
/// (state keyed per repo selector); any number works.
const CAPSTONE_ISSUE_NUMBER: u64 = 840;

/// The real code task (P2): named-function contract so the oracle can assert
/// the diff at content-invariant level without matching stochastic text.
const CAPSTONE_ISSUE_BODY: &str = "Add `pub fn is_palindrome(s: &str) -> bool` to `src/lib.rs` \
with a `///` doc comment and a `#[test]` unit test covering a palindrome and a \
non-palindrome. Do not push; do not merge.\n";

#[tokio::test]
async fn real_spec_drives_issue_to_close_capstone() {
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
            workflow_id: Some("issue-development".into()),
            plan_source: PlanSource::RealSpecTurn,
            issue_body: Some(FixtureIssue {
                number: CAPSTONE_ISSUE_NUMBER,
                body: CAPSTONE_ISSUE_BODY.into(),
            }),
            mint_report_card: true,
            require_task_gates: true,
            descriptor_gate_cmd: Some(CAPSTONE_GATE_CMD.into()),
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

    // P4: dispatcher permits 4 so the design/PR reviewer pairs can run in
    // parallel — but the SCHEDULER also enforces the per-wave task budget
    // (kernel default 1). Raise it to match; codex tasks lease disjoint
    // worktrees, and the seeded gate script pid-suffixes its output binary so
    // concurrent gate runs in the shared waves.cwd cannot collide.
    fx.repo_dyn
        .wave_update(
            fx.wave_id.as_str(),
            WavePatch {
                task_budget: Some(Some(4)),
                ..WavePatch::default()
            },
        )
        .await
        .expect("raise wave task budget for parallel reviewer pairs");

    let dispatcher = spawn_dispatcher_with_harness(&fx);

    let repo_gitdir = fx.wave_cwd.join(".git").display().to_string();
    let goal = capstone_goal(&repo_gitdir, CAPSTONE_ISSUE_NUMBER, &fx.origin_main_initial);
    boot_spec_harness_via_start_op(&fx, goal).await;

    // P7 bounding: every stage gets its own ≥480s floor (checker pin c —
    // substantial workers run for minutes each per d1), all under one overall
    // NEIGE_CAPSTONE_BUDGET deadline (default 3600s). A stage whose budget is
    // exhausted panics through `panic_with_agent_diag` (#866 rollout dumps).
    let overall_deadline = Instant::now() + capstone_budget();
    let st = move || capstone_stage_budget().min(remaining(overall_deadline));

    // S1 — the real spec plans from the bound workflow.
    let (plan_actor, _plan) =
        wait_for_plan_updated(&fx, spec_planning_budget().min(remaining(overall_deadline))).await;
    assert!(
        matches!(plan_actor, ActorId::AiSpecSession(_)),
        "plan.updated actor must be the real spec session, got {plan_actor:?}"
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

    // S2/S9 (design phase) — the spec records a design review round after the
    // real design reviewer workers complete. Convergence/round count is
    // tolerated here; presence is skeleton-required.
    let (_design_round_id, design_round_actor, design_round) = wait_capstone_event(
        &fx,
        "review.round",
        0,
        st(),
        "spec records a design review round",
        |p| p["subject"]["phase"] == json!("design"),
    )
    .await;
    assert!(
        matches!(design_round_actor, ActorId::AiSpecSession(_)),
        "design review.round actor must be AiSpecSession, got {design_round_actor:?} for {design_round}"
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

    // S8 — THE capstone-critical seam: a real reviewer worker reads the real
    // merge-base diff via gh.pr.diff (fixture verdicts satisfy S2/S9/S10, NOT
    // S8; zero forge.pr.diff.read fails the run). Seat tightening (review
    // channel A, item 3): this diff.read PRECEDES the converged impl round it
    // feeds (the round wait floors on diff_id), the open-pr goal FORBIDS
    // gh.pr.diff, the implement worker finishes before the PR exists, and the
    // merge worker dispatches only after the round — leaving the reviewer
    // workers (or the spec itself, an equally-real unscripted read) as the
    // only possible emitters. The event is card-anonymous, so exact reviewer
    // attribution stays tolerated (construction W: nothing scripted calls
    // gh.pr.diff in this file).
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

    // S9/S10 — the spec records the converged impl review round for this PR.
    // The manifest spec_instructions mark subject.pr_number OPTIONAL for
    // review rounds and the goal never pins it, so a real spec omitting it
    // emits a perfectly legal round — tolerate absent/null, require equality
    // only when present (review channel A, item 1).
    let (round_id, round_actor, round) = wait_capstone_event(
        &fx,
        "review.round",
        diff_id,
        st(),
        "spec records the converged impl review round",
        |p| {
            p["subject"]["phase"] == json!("impl")
                && p["converged"] == json!(true)
                && subject_pr_absent_or_matches(&p["subject"], pr_number)
        },
    )
    .await;
    assert!(
        matches!(round_actor, ActorId::AiSpecSession(_)),
        "impl review.round actor must be AiSpecSession, got {round_actor:?} for {round}"
    );
    let round_slice = round["subject"]["slice_id"]
        .as_str()
        .unwrap_or_else(|| panic!("impl review.round missing subject.slice_id: {round}"))
        .to_string();

    // S11 — merge, fenced on the converged round (F4).
    let (merged_id, merged_actor, merged) = wait_capstone_event(
        &fx,
        "forge.pr.merged",
        round_id,
        st(),
        "PR merged after review convergence",
        |p| p["subject"]["pr_number"] == json!(pr_number),
    )
    .await;
    assert_eq!(merged_actor, ActorId::KernelDispatcher, "{merged}");
    let merged_head = merged["head_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("forge.pr.merged missing head_sha: {merged}"))
        .to_string();
    assert!(is_hex_sha(&merged_head), "{merged}");
    // F4 direct assert, bound to the LATEST (max-n) pre-merge round on the
    // subject — NOT the first converged one the stage wait matched: a
    // converge → late-fix → re-converge run merges on the newer head, which
    // is legitimate and must pass (review channel A, item 2; consistent with
    // `assert_subject_keyed_cap_enforcement` / 6a semantics).
    let fence_round = latest_impl_round_before_merge(&fx, merged_id, &round_slice, pr_number).await;
    assert_eq!(
        fence_round["converged"],
        json!(true),
        "latest pre-merge impl round must be converged (F4): {fence_round}"
    );
    let fence_head = fence_round["head_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("fence round missing head_sha (F4): {fence_round}"));
    assert_eq!(
        merged_head, fence_head,
        "merge head must equal the LATEST converged round's head_sha (F4): {merged}"
    );
    let subject = SubjectKey::from_subject_payload(&fence_round["subject"]);
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

    // S13 — the spec drives the wave lifecycle to done.
    let (_done_id, done_actor, done_edge) = wait_capstone_event(
        &fx,
        "wave.lifecycle_changed",
        merged_id,
        st(),
        "spec transitions the wave to done",
        |p| p["to"] == json!("done") && p["id"] == json!(fx.wave_id.as_str()),
    )
    .await;
    assert!(
        matches!(done_actor, ActorId::AiSpecSession(_)),
        "→done lifecycle edge actor must be AiSpecSession, got {done_actor:?} for {done_edge}"
    );

    // ------------------------- post-run oracle -------------------------
    capstone_oracle(&fx, pr_number, &merged_head, &subject, &repo_gitdir).await;

    // Teardown per P4: dispatcher handle first, then harness, plugin, codex.
    // Panic paths (incl. every stage-wait timeout) do NOT run this teardown
    // and leak the shared appserver pgid — the suite-wide pre-existing
    // pattern, acceptable because real runs happen only inside the #863
    // isolation wrapper, which reaps the whole session process group.
    drop(dispatcher);
    shutdown_spec_harness_if_registered(&fx).await;
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Review-round subject tolerance (review channel A, item 1): the descriptor
/// marks `subject.pr_number` optional, so absent/null is legal; when present
/// it must match the capstone PR.
fn subject_pr_absent_or_matches(subject: &Value, pr_number: u64) -> bool {
    match subject.get("pr_number") {
        None => true,
        Some(Value::Null) => true,
        Some(v) => *v == json!(pr_number),
    }
}

/// The F4 fence round: the max-n impl `review.round` on `slice_id`
/// (pr_number absent-or-matching) with event id strictly BEFORE the merge —
/// i.e. the round the merge's expected_head_sha must have been built on.
/// Latest-n, not first-converged: a converge → late-fix → re-converge run
/// legitimately merges on the newer head (review channel A, item 2).
async fn latest_impl_round_before_merge(
    fx: &Fixture,
    merged_id: i64,
    slice_id: &str,
    pr_number: u64,
) -> Value {
    let rounds = event_rows(&fx.repo, "review.round").await;
    rounds
        .into_iter()
        .filter(|r| r.id < merged_id)
        .filter(|r| {
            let subject = &r.payload["subject"];
            subject["phase"] == json!("impl")
                && subject["slice_id"] == json!(slice_id)
                && subject_pr_absent_or_matches(subject, pr_number)
        })
        .max_by_key(|r| r.payload["n"].as_u64().unwrap_or(0))
        .map(|r| r.payload)
        .unwrap_or_else(|| {
            panic!("no impl review.round on slice {slice_id} precedes the merge (id {merged_id})")
        })
}

/// Wave goal for the capstone: environment facts (forge_pr_goal precedent —
/// repo selector, issue number, base sha are facts only the fixture knows)
/// plus descriptor-legal planning steering (deferred batches per plan.upsert
/// rule 3 + P5's dispatch-race analysis; GIVE-UP failure terminator per P3).
/// The PR coordinates themselves must flow through observations/runs — they
/// do not exist when this goal is written.
fn capstone_goal(repo_gitdir: &str, issue_number: u64, base_sha: &str) -> String {
    format!(
        "Drive the bound issue-development workflow END-TO-END for issue #{issue_number}: read \
         the issue, converge design review, implement, open a pull request, converge PR review, \
         merge, close the issue, and move the wave lifecycle to done.\n\
         \n\
         Environment facts:\n\
         - The `repo` argument for EVERY gh.* forge tool call (gh.issue.view, gh.pr.create, \
         gh.pr.checks, gh.pr.diff, gh.pr.merge, gh.issue.close) is exactly `{repo_gitdir}`. \
         Embed this exact literal value in the goal of every task that must call a gh.* tool; \
         workers cannot discover it on their own.\n\
         - The wave's source issue is #{issue_number}.\n\
         - Pull requests use base branch `main`; the base commit sha is `{base_sha}`.\n\
         \n\
         Planning constraints (all within the bound workflow):\n\
         - Attach the bound workflow gate (exactly its cmd) to every task you plan; do not use \
         no_gate_reason.\n\
         - The kernel REJECTS any task whose depends_on names a task that does not exist yet, \
         so plan in three batches: (1) inspect-issue, review-design-a, review-design-b and \
         implement-change first; (2) add open-pr only after implement-change completes, \
         embedding the implement worker's actual branch name in its goal; (3) add review-pr-a, \
         review-pr-b AND merge together in ONE calm.plan.upsert batch only after open-pr \
         completes, embedding the literal repo, pr number, base sha, head sha and reviewed \
         slice_id values in each of their goals.\n\
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
         - review-pr-a / review-pr-b goals: call gh.pr.diff with the embedded repo, pr, \
         base_sha and head_sha, review the returned diff against the issue requirements, and \
         report the literal verdict token `approved` or `changes_requested` in \
         calm.task.complete.\n\
         - merge goal: call gh.pr.merge with the embedded repo and pr, phase `impl`, the \
         reviewed slice_id, and expected_head_sha equal to the head sha of the converged impl \
         review round; then call gh.issue.close for issue #{issue_number} with the same repo.\n\
         - After the merge task completes and the issue is closed, transition the wave \
         lifecycle to done.\n\
         - If a review subject cannot converge at the review cap, give up and fail the wave; \
         do not request ratification."
    )
}

/// Floor-based capstone stage wait with the failure terminator folded in:
/// `ratify.requested` at ANY point is a purity violation (the goal steers
/// GIVE-UP), and a wave that lands `failed` is a legitimate agent outcome but
/// a capstone FAILURE — both fail fast with full agent diagnostics instead of
/// burning the stage budget.
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
        if wave_lifecycle_row(fx).await == "failed" {
            panic_with_agent_diag(
                fx,
                format!(
                    "wave lifecycle landed `failed` (spec gave up — terminal) while waiting \
                     for {kind} ({describe})"
                ),
            )
            .await;
        }
        let rows: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(
            "SELECT id, actor, scope_wave, payload FROM events \
             WHERE kind = ?1 AND id > ?2 ORDER BY id ASC",
        )
        .bind(kind)
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("{kind} event rows after floor {floor}: {e}"));
        let hit = rows
            .into_iter()
            .find_map(|(id, actor, scope_wave, payload)| {
                let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
                let payload: Value = serde_json::from_str(&payload).expect("event payload json");
                (scope_wave.as_deref() == Some(fx.wave_id.as_str()) && predicate(&payload))
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
/// fence, F4 idem-key shape, no-cargo gate audit, content invariant, purity.
async fn capstone_oracle(
    fx: &Fixture,
    pr_number: u64,
    merged_head: &str,
    subject: &SubjectKey,
    repo_gitdir: &str,
) {
    // Skeleton (⊇): every required kind appears at least once; extra events,
    // extra tasks, dup idempotent rows are all tolerated.
    assert_event_skeleton_superset(
        &fx.repo,
        &[
            RequiredEvent::any("workflow.registered"),
            RequiredEvent::any("plan.updated"),
            RequiredEvent::new("task.dispatched", |r| r.payload["kind"] == json!("codex")),
            RequiredEvent::any("workspace.leased"),
            RequiredEvent::any("worktree.provisioned"),
            RequiredEvent::any("runtime.started"),
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
            RequiredEvent::new("review.round", |r| {
                r.payload["subject"]["phase"] == json!("design")
            }),
            RequiredEvent::new("review.round", |r| {
                r.payload["subject"]["phase"] == json!("impl")
                    && r.payload["converged"] == json!(true)
            }),
            RequiredEvent::new("forge.pr.merged", |r| {
                r.payload["merge_sha"].as_str().is_some_and(is_hex_sha)
            }),
            RequiredEvent::new("forge.issue.closed", |r| {
                r.payload["issue_number"] == json!(CAPSTONE_ISSUE_NUMBER)
            }),
            RequiredEvent::new("wave.lifecycle_changed", |r| {
                r.payload["to"] == json!("done")
            }),
        ],
    )
    .await;

    // Orderings 1, 4, 5 (first-matching happens-before). Ordering 2 (latest
    // converged design round < first impl-task dispatch) is deliberately NOT
    // asserted: the scheduler never reads review state (scheduler.rs
    // compute_ready — deps-Done only) and the plan shape is
    // invariant-tolerated, so the edge is soft/unidentifiable by design.
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
    // Ordering 3 (kernel-forced, HARD, per card): worktree.provisioned
    // precedes runtime.started for every card that has both.
    assert_provisioned_before_runtime_started_per_card(fx).await;

    // Fence 6: subject-keyed cap enforcement + 6a existence (converged
    // subject must actually have a head-matching merge). 6a keys merges by
    // the FULL subject; when the spec legally omitted pr_number from its
    // round subjects (item-1 tolerance) the round subject can never equal
    // the merge subject (which always carries pr_number from the tool args),
    // so 6a is replaced by the direct latest-fence assert already made
    // in-line (merged head == latest pre-merge converged round head).
    assert_subject_keyed_cap_enforcement(&fx.repo, fx.wave_id.as_str()).await;
    if subject.pr_number.is_some() {
        assert_converged_subject_has_merge(&fx.repo, subject).await;
    }

    // Actor table (event-row column, never payload).
    for (actor, payload) in actor_payload_rows(&fx.repo, "plan.updated").await {
        assert!(
            matches!(actor, ActorId::AiSpecSession(_)),
            "plan.updated actor must be AiSpecSession, got {actor:?} for {payload}"
        );
    }
    for (actor, payload) in actor_payload_rows(&fx.repo, "review.round").await {
        assert!(
            matches!(actor, ActorId::AiSpecSession(_)),
            "review.round actor must be AiSpecSession, got {actor:?} for {payload}"
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

    // F4 idem-key shape (d2 helper): the forge-action op idempotency key is
    // `{plugin}:{wave}:{caller card}:{plugin idem}` and the plugin idem is
    // `gh.pr.merge:{repo}:{pr}:{expected_head_sha}` ONLY when
    // expected_head_sha was passed — an omitted-sha merge produces
    // `gh.pr.merge:{repo}:{pr}` and fails here. The caller card is NOT pinned
    // (the descriptor merge task is kind:codex, so a merge-worker seat is as
    // legal as the spec seat); the with-sha suffix is the load-bearing F4
    // content.
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
             (F4, expected suffix {merge_suffix}): {merge_keys:?}"
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

    // No-cargo audit (checker pin d): the spec copied gates into plan.upsert;
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

    // Content invariant (P2 + pin a): the MERGED head's diff against the
    // seeded base touches src/lib.rs and adds the issue-contract function +
    // a test. The PR head commit lives in the wave clone's object db (worker
    // worktrees are `git -C {waves.cwd} worktree add`, so branches/objects
    // share the clone gitdir; no push involved).
    let content_diff = git_stdout(
        &fx.wave_cwd,
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

    // Purity: never ratified, never failed, never the injected-plan path.
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "steered GIVE-UP capstone must never request ratification"
    );
    assert_eq!(
        wave_lifecycle_row(fx).await,
        "done",
        "capstone wave row must land done"
    );
    assert!(
        !fx.used_injected_plan(),
        "RealSpecTurn must not use injected plan path"
    );
}

/// Ordering 3 (kernel-forced): for every card that has BOTH events, the first
/// `worktree.provisioned` precedes the first `runtime.started` (the spec card
/// has runtime.started but no worktree — vacuously skipped).
async fn assert_provisioned_before_runtime_started_per_card(fx: &Fixture) {
    let provisioned = event_rows(&fx.repo, "worktree.provisioned").await;
    let started = event_rows(&fx.repo, "runtime.started").await;
    let mut checked = 0usize;
    for p in &provisioned {
        let card_id = p.payload["card_id"]
            .as_str()
            .unwrap_or_else(|| panic!("worktree.provisioned missing card_id: {}", p.payload));
        let first_provisioned = provisioned
            .iter()
            .filter(|r| r.payload["card_id"] == json!(card_id))
            .map(|r| r.id)
            .min()
            .expect("at least this row");
        if let Some(first_started) = started
            .iter()
            .filter(|r| r.payload["card_id"] == json!(card_id))
            .map(|r| r.id)
            .min()
        {
            assert!(
                first_provisioned < first_started,
                "card {card_id}: worktree.provisioned (id {first_provisioned}) must precede \
                 runtime.started (id {first_started})"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "ordering-3 check matched no card with both worktree.provisioned and runtime.started"
    );
}

// --------------- #840 capstone deterministic support gates ---------------
// These run WITHOUT a codex binary (no skip!) so `cargo test --features
// codex-e2e --test codex_forge_e2e` deterministically gates the capstone's
// support seams: the hermetic gate script, the descriptor gate patch + cargo
// guard, and the gh shim's seeded-issue-body branch.

/// The seeded gate script must pass under the task-verify wrapper's EXACT
/// conditions (`/bin/sh`, cleared env, repo cwd) and must be cargo-free with
/// no Cargo.toml anywhere in the seeded repo (P1/P2 + pin d).
#[test]
fn capstone_gate_script_is_hermetic_and_cargo_free() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin.git");
    let clone = tmp.path().join("clone");
    seed_rust_micro_crate(&origin, &tmp.path().join("seed"));
    clone_for_wave(&origin, &clone);

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

/// The descriptor patch replaces the production `cargo test` gate before
/// `Manifest::parse`, the boot guard rejects cargo gate cmds, and the
/// UNPATCHED manifest still carries the cargo gate (i.e. the guard is live,
/// not vacuous).
#[test]
fn descriptor_gate_patch_removes_cargo_and_guard_is_live() {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");

    let patched = patch_manifest_gate_cmd(&raw, CAPSTONE_GATE_CMD);
    let manifest = Manifest::parse(&patched).expect("patched manifest parses");
    assert_no_cargo_gate_cmds(&manifest);
    let mut steps = 0usize;
    for workflow in &manifest.workflows {
        for gate in &workflow.gates {
            for step in &gate.steps {
                assert_eq!(step.cmd, CAPSTONE_GATE_CMD);
                steps += 1;
            }
        }
    }
    assert!(steps > 0, "patched manifest must still carry gate steps");

    // Everything except gate cmds stays value-identical (P1: plan_template,
    // spec_instructions, cap, tools untouched).
    let mut expected: Value = serde_json::from_str(&raw).expect("manifest json");
    for workflow in expected["workflows"]
        .as_array_mut()
        .expect("workflows array")
    {
        if let Some(gates) = workflow.get_mut("gates").and_then(Value::as_array_mut) {
            for gate in gates {
                if let Some(steps) = gate.get_mut("steps").and_then(Value::as_array_mut) {
                    for step in steps {
                        step["cmd"] = json!(CAPSTONE_GATE_CMD);
                    }
                }
            }
        }
    }
    let patched_value: Value = serde_json::from_str(&patched).expect("patched json");
    assert_eq!(
        patched_value, expected,
        "gate-cmd patch must not disturb anything else in the manifest"
    );

    let unpatched = Manifest::parse(&raw).expect("production manifest parses");
    let has_cargo_gate = unpatched.workflows.iter().any(|w| {
        w.gates
            .iter()
            .any(|g| g.steps.iter().any(|s| s.cmd.contains("cargo")))
    });
    assert!(
        has_cargo_gate,
        "production manifest gate baseline moved (no cargo gate found); \
         re-evaluate the capstone patch + #863-B posture"
    );
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

/// #900 regression: in a multi-threaded test process, a child forked by
/// another thread can hold a fork-inherited write fd to the freshly written
/// shim's inode until it execs, so a direct spawn can fail ETXTBSY. Model
/// that race deterministically with a held write fd: the raw spawn must fail
/// with ExecutableFileBusy, and `run_gh` must retry until the fd is released
/// and then succeed. Linux-only: the repro relies on the kernel enforcing
/// ETXTBSY at execve of a file open for writing, which macOS does not.
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

async fn seed_design_channel_complete(fx: &Fixture, key: &str, chan: &str) {
    seed_design_channel_verdict(fx, key, chan, "approved").await;
}

async fn seed_design_channel_changes_requested(fx: &Fixture, key: &str, chan: &str) {
    seed_design_channel_verdict(fx, key, chan, "changes_requested").await;
}

/// Seed a design review-channel task pair (dispatched + completed) carrying
/// `verdict`, then fail fast unless the runs/ projection surfaces that exact
/// verdict — the real spec reads runs/ to learn the channel outcomes.
async fn seed_design_channel_verdict(fx: &Fixture, key: &str, chan: &str, verdict: &str) {
    seed_completed_task_pair(
        fx,
        key,
        json!({
            "summary": verdict,
            "verdict": verdict,
            "channel": chan,
        }),
        verdict,
    )
    .await
}

/// Seed a pipeline task pair (dispatched + completed) whose completion carries
/// `result`, then fail fast unless the runs/ projection surfaces the exact
/// `expected_summary` — the real spec reads runs/ to learn task outcomes.
/// (Generalized from R6's design-channel helper for #840 d2.)
async fn seed_completed_task_pair(fx: &Fixture, key: &str, result: Value, expected_summary: &str) {
    let verdict = expected_summary;
    let task_id = task_id(fx, key);
    let wave_scope = EventScope::Wave {
        wave: fx.wave_id.clone(),
        cove: fx.cove_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            wave_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.wave_cove_cache,
            Event::TaskDispatched {
                idempotency_key: task_id.clone(),
                kind: "codex".into(),
                agent_message: Some(format!("[codex-forge-e2e] seed task {key}")),
            },
        )
        .await
        .expect("log seeded task.dispatched");

    // The seeded fixture shortcut does not mint a real worker session, so
    // author the completion as KernelDispatcher (gate-unrestricted per
    // role_gate rule 5). Card scope alone routes it to the completed bucket
    // (is_spec_verdict_event is false for non-Wave scope), so runs/ surfaces
    // the summary the real spec reads.
    let card_scope = EventScope::Card {
        card: fx.spec_card_id.clone(),
        wave: fx.wave_id.clone(),
        cove: fx.cove_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            card_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.wave_cove_cache,
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
        .lookup(TOOL_WAVE_CAT)
        .expect("wave cat registered");
    let json_path = format!("runs/{task_id}.json");
    let json_read = handler(
        fx.ctx.clone(),
        spec_identity(fx),
        json!({ "path": json_path }),
    )
    .await
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
        spec_identity(fx),
        json!({ "path": md_path }),
    )
    .await
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

async fn recover_spec_harness(fx: &Fixture) -> Option<SpecHarness> {
    let runtime = fx
        .repo
        .session_projection_active_for_card(&fx.spec_card_id.to_string())
        .await
        .ok()
        .flatten()?;
    fx.harness.get(&runtime.id)
}

#[cfg(feature = "fixtures")]
async fn inject_task_completed(h: &SpecHarness, idem_key: &str) {
    h.observe_for_test(
        Observation::TaskCompleted {
            idempotency_key: idem_key.into(),
            result: json!({ "summary": "approved" }),
        },
        None,
    )
    .await;
}

#[cfg(not(feature = "fixtures"))]
async fn inject_task_completed(_h: &SpecHarness, _idem_key: &str) {
    panic!("inject_task_completed requires the fixtures feature");
}

#[cfg(feature = "fixtures")]
async fn inject_task_changes_requested(h: &SpecHarness, idem_key: &str) {
    h.observe_for_test(
        Observation::TaskCompleted {
            idempotency_key: idem_key.into(),
            result: json!({ "summary": "changes_requested" }),
        },
        None,
    )
    .await;
}

#[cfg(not(feature = "fixtures"))]
async fn inject_task_changes_requested(_h: &SpecHarness, _idem_key: &str) {
    panic!("inject_task_changes_requested requires the fixtures feature");
}

/// Inject the same `Observation::ReviewRound` the prod dispatcher's
/// `harness_observation_from_event` would push for the seeded prior round —
/// the spec cannot learn prior rounds from runs//wave-fs (no review.round
/// projection exists), so this observation turn text is its ONLY channel for
/// round state, exactly as in production.
#[cfg(feature = "fixtures")]
async fn inject_design_review_round_observation(
    h: &SpecHarness,
    fx: &Fixture,
    slice_id: &str,
    n: u32,
    cap: u32,
    converged: bool,
) {
    h.observe_for_test(
        Observation::ReviewRound {
            wave_id: fx.wave_id.clone(),
            phase: "design".into(),
            slice_id: slice_id.into(),
            pr_number: None,
            head_sha: None,
            n,
            cap,
            converged,
        },
        None,
    )
    .await;
}

#[cfg(not(feature = "fixtures"))]
async fn inject_design_review_round_observation(
    _h: &SpecHarness,
    _fx: &Fixture,
    _slice_id: &str,
    _n: u32,
    _cap: u32,
    _converged: bool,
) {
    panic!("inject_design_review_round_observation requires the fixtures feature");
}

/// Seed one prior non-converged design review.round as typed
/// `Event::ReviewRound` (typed-seeding precedent:
/// crates/calm-truth/src/wave_vcs.rs review/ratify batch test). Actor MUST be
/// `ActorId::AiSpec(spec card)` with `EventScope::Wave`: role_gate rule 2.8
/// makes review.round spec-only, and the seeded AiSpec rows stay
/// actor-distinguishable from the real spec's AiSpecSession rows.
async fn seed_prior_design_review_round(fx: &Fixture, slice_id: &str, n: u32, cap: u32) {
    let wave_scope = EventScope::Wave {
        wave: fx.wave_id.clone(),
        cove: fx.cove_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::AiSpec(fx.spec_card_id.clone()),
            wave_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.wave_cove_cache,
            Event::ReviewRound {
                wave_id: fx.wave_id.clone(),
                subject: ReviewSubject {
                    phase: "design".into(),
                    slice_id: slice_id.into(),
                    pr_number: None,
                },
                head_sha: None,
                n,
                cap,
                converged: false,
                channels: vec![
                    ChannelVerdict {
                        role: "design-a".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    },
                    ChannelVerdict {
                        role: "design-b".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    },
                ],
                root_cause: None,
                // Canonical shape from `review_round_idempotency_key`
                // (mcp_server/tools/review.rs): design subjects use the
                // literal "design" in the pr slot.
                idempotency_key: format!(
                    "review.round:{}:design:{}:design:{}",
                    fx.wave_id.as_str(),
                    slice_id,
                    n
                ),
            },
        )
        .await
        .expect("log seeded prior review.round");
}

/// First post-floor `review.round` on the seeded design subject (phase,
/// slice AND null/absent pr_number — a hypothetical `{design, S, Some(pr)}`
/// subject is a different review stream and must not be returned). Rounds on
/// other subjects are tolerated (the kernel keeps them internally monotone);
/// on the seeded subject the kernel only accepts n=8 next, so the first hit
/// IS the cap round. Returns the event id so callers can pin ordering
/// invariants against it.
async fn wait_for_design_review_round_on_subject(
    fx: &Fixture,
    floor: i64,
    slice_id: &str,
    budget: Duration,
) -> (i64, ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, actor, payload FROM events \
             WHERE kind = 'review.round' AND id > ?1 ORDER BY id ASC",
        )
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("review.round event rows after floor {floor}: {e}"));
        let hit = rows.into_iter().find_map(|(id, actor, payload)| {
            let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
            let payload: Value = serde_json::from_str(&payload).expect("event payload json");
            let on_subject = {
                let subject = &payload["subject"];
                subject["phase"] == json!("design")
                    && subject["slice_id"] == json!(slice_id)
                    && subject.get("pr_number").is_none_or(Value::is_null)
            };
            on_subject.then_some((id, actor, payload))
        });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for post-floor design review.round \
                     on slice {slice_id} after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

/// First post-floor `wave.lifecycle_changed` landing on `failed` for the
/// fixture wave. Other lifecycle transitions are tolerated.
async fn wait_for_wave_failed_edge(fx: &Fixture, floor: i64, budget: Duration) -> (ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, actor, payload FROM events \
             WHERE kind = 'wave.lifecycle_changed' AND id > ?1 ORDER BY id ASC",
        )
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("wave.lifecycle_changed rows after floor {floor}: {e}"));
        let hit = rows.into_iter().find_map(|(_, actor, payload)| {
            let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
            let payload: Value = serde_json::from_str(&payload).expect("event payload json");
            (payload["to"] == json!("failed") && payload["id"] == json!(fx.wave_id.as_str()))
                .then_some((actor, payload))
        });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for wave.lifecycle_changed to=failed \
                     after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

/// First post-floor `wave.lifecycle_changed` matching `from -> to` for the
/// fixture wave. Other lifecycle transitions and other waves are tolerated.
/// Returns the event id so callers can chain rising-floor ordering
/// invariants.
async fn wait_for_wave_lifecycle_edge(
    fx: &Fixture,
    floor: i64,
    from: &str,
    to: &str,
    budget: Duration,
) -> (i64, ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let hit = lifecycle_changed_rows_after(fx, floor)
            .await
            .into_iter()
            .find(|(_, _, payload)| {
                payload["from"] == json!(from)
                    && payload["to"] == json!(to)
                    && payload["id"] == json!(fx.wave_id.as_str())
            });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for wave.lifecycle_changed \
                     {from}->{to} after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn lifecycle_changed_rows_after(fx: &Fixture, floor: i64) -> Vec<(i64, ActorId, Value)> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, actor, payload FROM events \
         WHERE kind = 'wave.lifecycle_changed' AND id > ?1 ORDER BY id ASC",
    )
    .bind(floor)
    .fetch_all(fx.repo.pool())
    .await
    .unwrap_or_else(|e| panic!("wave.lifecycle_changed rows after floor {floor}: {e}"));
    rows.into_iter()
        .map(|(id, actor, payload)| {
            (
                id,
                serde_json::from_str(&actor).expect("event actor json"),
                serde_json::from_str(&payload).expect("event payload json"),
            )
        })
        .collect()
}

/// `wave.lifecycle_changed` rows strictly inside the `(after, before)` event
/// id window — used to pin the grant's same-tx blocked->working edge between
/// the request and the resolution rows.
async fn lifecycle_changed_rows_between(
    fx: &Fixture,
    after: i64,
    before: i64,
) -> Vec<(ActorId, Value)> {
    lifecycle_changed_rows_after(fx, after)
        .await
        .into_iter()
        .filter(|(id, _, _)| *id < before)
        .map(|(_, actor, payload)| (actor, payload))
        .collect()
}

/// First post-floor `ratify.requested`. Never emitted by this test: role_gate
/// rule 2.8 makes it spec-session-only, so an observed row proves the real
/// spec's own `calm.ratify.request` tool call.
async fn wait_for_ratify_requested(
    fx: &Fixture,
    floor: i64,
    budget: Duration,
) -> (i64, ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, actor, payload FROM events \
             WHERE kind = 'ratify.requested' AND id > ?1 ORDER BY id ASC",
        )
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("ratify.requested rows after floor {floor}: {e}"));
        if let Some((id, actor, payload)) = rows.into_iter().next() {
            let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
            let payload: Value = serde_json::from_str(&payload).expect("event payload json");
            return (id, actor, payload);
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for ratify.requested after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn wave_lifecycle_row(fx: &Fixture) -> String {
    sqlx::query_scalar("SELECT lifecycle FROM waves WHERE id = ?1")
        .bind(fx.wave_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select wave lifecycle")
}

/// The production HTTP grant seam (design D4): the real `routes::router()`
/// behind `actor_middleware`, over the fixture's LIVE parts (repo, event bus,
/// role/wave-cove caches), driven in-process via `tower::ServiceExt::oneshot`
/// — exact precedent tests/review_ratify.rs.
fn fixture_router(fx: &Fixture) -> axum::Router {
    let state = AppState::from_parts(
        fx.repo_dyn.clone(),
        fx.events.clone(),
        fx.daemon.clone(),
        fx.plugin_host.clone(),
        fx.codex.clone(),
        Some(fx.cache.clone()),
        Some(fx.wave_cove_cache.clone()),
    );
    calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

/// Inject the same `Observation::RatifyResolved` the prod dispatcher's
/// `harness_observation_from_event` would push for the grant's
/// ratify.resolved event (hard-fire).
#[cfg(feature = "fixtures")]
async fn inject_ratify_resolved_grant(h: &SpecHarness, fx: &Fixture) {
    h.observe_for_test(
        Observation::RatifyResolved {
            wave_id: fx.wave_id.clone(),
            decision: calm_server::event::RatifyDecision::Grant,
        },
        None,
    )
    .await;
}

#[cfg(not(feature = "fixtures"))]
async fn inject_ratify_resolved_grant(_h: &SpecHarness, _fx: &Fixture) {
    panic!("inject_ratify_resolved_grant requires the fixtures feature");
}

async fn wait_for_spec_turn_settled(fx: &Fixture, h: &SpecHarness, budget: Duration) {
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
                    "timed out after {budget:?} waiting for spec harness turn to settle; \
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

async fn count_design_review_rounds(fx: &Fixture) -> usize {
    actor_payload_rows(&fx.repo, "review.round")
        .await
        .into_iter()
        .filter(|(_, payload)| is_design_review_round(payload))
        .count()
}

fn is_design_review_round(payload: &Value) -> bool {
    payload.pointer("/subject/phase").and_then(Value::as_str) == Some("design")
}

async fn wait_for_converged_design_review_round(
    fx: &Fixture,
    floor: i64,
    budget: Duration,
) -> Vec<(ActorId, Value)> {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, actor, payload FROM events \
             WHERE kind = 'review.round' AND id > ?1 ORDER BY id ASC",
        )
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("review.round event rows after floor {floor}: {e}"));
        let design: Vec<(i64, ActorId, Value)> = rows
            .into_iter()
            .map(|(id, actor, payload)| {
                (
                    id,
                    serde_json::from_str(&actor).expect("event actor json"),
                    serde_json::from_str(&payload).expect("event payload json"),
                )
            })
            .filter(|(_, _, payload)| is_design_review_round(payload))
            .collect();
        if design
            .last()
            .is_some_and(|(_, _, payload)| payload["converged"] == json!(true))
        {
            return design
                .into_iter()
                .map(|(_, actor, payload)| (actor, payload))
                .collect();
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for converged design review.round after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn assert_real_design_review_round(fx: &Fixture, rounds: &[(ActorId, Value)]) {
    fn null_or_absent(value: &Value, key: &str) -> bool {
        value.get(key).is_none() || value[key].is_null()
    }

    fn required_str<'a>(value: &'a Value, key: &str, context: &str) -> &'a str {
        value[key]
            .as_str()
            .unwrap_or_else(|| panic!("{context} missing string {key}: {value}"))
    }

    fn required_u64(value: &Value, key: &str, context: &str) -> u64 {
        value[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{context} missing unsigned integer {key}: {value}"))
    }

    fn assert_channels(payload: &Value) {
        let channels = payload["channels"]
            .as_array()
            .unwrap_or_else(|| panic!("review.round channels must be an array: {payload}"));
        assert!(
            channels.len() >= 2,
            "review.round must carry at least two channels: {payload}"
        );
        let roles: std::collections::BTreeSet<&str> = channels
            .iter()
            .map(|channel| {
                channel["role"]
                    .as_str()
                    .unwrap_or_else(|| panic!("review.round channel missing role: {channel}"))
            })
            .collect();
        assert!(
            roles.len() >= 2,
            "review.round channels must have at least two distinct roles: {payload}"
        );
    }

    assert!(
        !rounds.is_empty(),
        "expected at least one design review.round"
    );
    assert!(
        !fx.used_injected_plan(),
        "RealSpecTurn must not use injected plan path"
    );

    let mut by_subject: std::collections::BTreeMap<(String, String, Option<u64>), Vec<&Value>> =
        std::collections::BTreeMap::new();
    for (actor, payload) in rounds {
        assert!(
            matches!(actor, ActorId::AiSpecSession(_)),
            "review.round actor must be AiSpecSession, got {actor:?} for {payload}"
        );
        assert_eq!(
            payload["cap"],
            json!(8),
            "design review.round cap must be descriptor-fixed 8: {payload}"
        );
        assert!(
            null_or_absent(payload, "head_sha"),
            "design review.round must omit/null head_sha: {payload}"
        );

        let subject = &payload["subject"];
        assert_eq!(
            subject["phase"],
            json!("design"),
            "oracle received non-design review.round: {payload}"
        );
        assert!(
            null_or_absent(subject, "pr_number"),
            "design review.round subject must omit/null pr_number: {payload}"
        );
        let slice_id = required_str(subject, "slice_id", "review.round subject");
        assert!(
            !slice_id.is_empty(),
            "design review.round subject.slice_id must be non-empty: {payload}"
        );
        assert_channels(payload);

        by_subject
            .entry(("design".to_string(), slice_id.to_string(), None))
            .or_default()
            .push(payload);
    }

    for (subject, subject_rounds) in &by_subject {
        for (expected_n, payload) in (1_u64..).zip(subject_rounds.iter()) {
            let n = required_u64(payload, "n", "review.round");
            assert_eq!(
                n, expected_n,
                "design review.round n must be monotonic for {subject:?}: {subject_rounds:?}"
            );
        }

        let latest = subject_rounds
            .last()
            .expect("subject group has at least one review.round");
        assert_eq!(
            latest["converged"],
            json!(true),
            "latest design review.round must be converged: {latest}"
        );
        let channels = latest["channels"].as_array().unwrap_or_else(|| {
            panic!("latest design review.round channels must be an array: {latest}")
        });
        assert!(
            channels
                .iter()
                .all(|channel| channel["verdict"] == json!("approved")),
            "latest design review.round channel verdicts must all be literal approved: {latest}"
        );
    }
}

fn merge_close_goal(repo_gitdir: &str, issue_number: u64) -> String {
    format!(
        "Drive the tail of the issue-development workflow for issue #{issue_number}. \
         Environment facts: the `repo` argument for every gh.* MCP forge tool is exactly \
         `{repo_gitdir}`; the wave's source issue is #{issue_number}. Implementation, the \
         pull request, and both PR review channels are already complete for this wave; \
         their results arrive as observations. Once the impl review round for the pull \
         request reports converged, execute the merge step yourself with the MCP forge \
         tools (gh.pr.merge, then gh.issue.close for issue #{issue_number}); do not \
         dispatch further tasks."
    )
}

/// The live spec session's bound codex thread id — the identity handle for
/// scripted daemon-socket `tools/call`s (identical wire to the real spec's
/// own calls, so scripted setup events go through the real plugin lowering).
async fn spec_session_thread_id(fx: &Fixture) -> String {
    fx.repo
        .session_projection_active_for_card(&fx.spec_card_id.to_string())
        .await
        .expect("active spec session lookup")
        .expect("live spec session for spec card")
        .thread_id
        .expect("spec session bound to a codex thread")
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

/// First `kind` event on the fixture wave with id > `floor` matching
/// `predicate`; superset-tolerant (other events/subjects are skipped, not
/// failed). Returns the event id so callers can pin ordering invariants.
async fn wait_for_wave_forge_event(
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
            "SELECT id, actor, scope_wave, payload FROM events \
             WHERE kind = ?1 AND id > ?2 ORDER BY id ASC",
        )
        .bind(kind)
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("{kind} event rows after floor {floor}: {e}"));
        let hit = rows
            .into_iter()
            .find_map(|(id, actor, scope_wave, payload)| {
                let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
                let payload: Value = serde_json::from_str(&payload).expect("event payload json");
                (scope_wave.as_deref() == Some(fx.wave_id.as_str()) && predicate(&payload))
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

/// Seed the ONE converged typed impl review.round (d2 design D2): actor MUST
/// be `ActorId::AiSpec(spec card)` with `EventScope::Wave` — role_gate rule
/// 2.8 makes review.round spec-only, and the seeded AiSpec row stays
/// actor-distinguishable from the real spec's AiSpecSession rows. Phase
/// literal is "impl" (forge_workflow_e2e `impl_round` precedent).
async fn seed_converged_impl_review_round(
    fx: &Fixture,
    slice_id: &str,
    pr_number: u64,
    head_sha: &str,
) {
    let wave_scope = EventScope::Wave {
        wave: fx.wave_id.clone(),
        cove: fx.cove_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::AiSpec(fx.spec_card_id.clone()),
            wave_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.wave_cove_cache,
            Event::ReviewRound {
                wave_id: fx.wave_id.clone(),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: slice_id.into(),
                    pr_number: Some(pr_number),
                },
                head_sha: Some(head_sha.to_string()),
                n: 1,
                cap: 8,
                converged: true,
                channels: vec![
                    ChannelVerdict {
                        role: "pr-correctness".into(),
                        verdict: ChannelVerdictKind::Approved,
                    },
                    ChannelVerdict {
                        role: "pr-failure-path".into(),
                        verdict: ChannelVerdictKind::Approved,
                    },
                ],
                root_cause: None,
                // Canonical shape from `review_round_idempotency_key`
                // (mcp_server/tools/review.rs): PR subjects carry the pr
                // number in the pr slot.
                idempotency_key: format!(
                    "review.round:{}:impl:{}:{}:1",
                    fx.wave_id.as_str(),
                    slice_id,
                    pr_number
                ),
            },
        )
        .await
        .expect("log seeded converged impl review.round");
}

async fn latest_event_id_of_kind(fx: &Fixture, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("max {kind} event id: {e}"))
}

/// All forge-action operation idempotency keys containing `needle`, oldest
/// first. The key shape is `{plugin}:{wave}:{caller card}:{plugin idem}`
/// (mcp_server/transport.rs `submit_forge_action`), so it pins BOTH the
/// caller seat and the plugin-level idem (incl. F4's expected_head_sha).
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
async fn inject_observation(h: &SpecHarness, obs: Observation) {
    h.observe_for_test(obs, None).await;
}

#[cfg(not(feature = "fixtures"))]
async fn inject_observation(_h: &SpecHarness, _obs: Observation) {
    panic!("inject_observation requires the fixtures feature");
}

fn review_budget() -> Duration {
    std::env::var("NEIGE_SPEC_REVIEW_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        // Default doubled vs spec_planning_budget — the review wait includes the spec's autonomous runs/ read round-trip and 1/5 stability runs exceeded 240s (design §6 headroom).
        .unwrap_or_else(|| Duration::from_secs(480))
}

/// Budget for the ASK-HUMAN request-wait and the post-grant resume-wait
/// (R7 design D6): each spans a full real spec turn, so it gets the same
/// headroom as `review_budget`.
fn ratify_budget() -> Duration {
    std::env::var("NEIGE_SPEC_RATIFY_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(480))
}
