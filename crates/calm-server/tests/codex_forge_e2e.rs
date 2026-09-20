//! Real Codex forge E2E; feature-gated behind `codex-e2e` and self-skipping when no real Codex binary is available.

#![cfg(all(unix, feature = "codex-e2e"))]

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{ChannelVerdict, ChannelVerdictKind, Event, EventScope, ReviewSubject};
use calm_server::harness::{HarnessState, Observation, PlannerHarness};
use calm_server::ids::{ActorId, TrackId};
use calm_server::mcp_server::tools::track_file::TOOL_TRACK_CAT;
use calm_server::model::{TrackLifecycle, TrackPatch};
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
use support::planner_turn::*;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const PR_CREATE_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.create";
const PR_CHECKS_TOOL: &str = "plugin.dev.neige.git-forge_gh.pr.checks";
/// The review-subject `slice_id` is agent-chosen and drifts within a run, so the goal pins it; must not collide with any plan task key or be a file path.
const STEERED_REVIEW_SLICE: &str = "marker-slice";
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
        format!("neige/{}/{}", fx.track_id.as_str(), worker_card_id)
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

    // Superset-tolerant: the real planner may emit further lifecycle transitions, so filter rather than count.
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
        "expected exactly one track.lifecycle_changed draft->planning, got {lifecycle:?}"
    );
    let (lifecycle_actor, lifecycle_payload) = draft_to_planning[0];
    assert_eq!(
        lifecycle_actor,
        &ActorId::Kernel,
        "draft->planning companion actor must be Kernel"
    );
    assert_eq!(lifecycle_payload["id"], json!(fx.track_id.as_str()));
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

#[tokio::test]
async fn real_planner_agent_autonomously_emits_design_review_round_from_descriptor() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let goal = "Plan the smallest issue-development template for adding one marker file, \
                then drive design-review convergence."
        .to_string();
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

    let (plan_actor, _plan) = wait_for_plan_updated(&fx, planner_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiPlannerSession(_)),
        "plan.updated actor must be the real planner session, got {plan_actor:?}"
    );

    let harness = recover_planner_harness(&fx)
        .await
        .expect("live planner harness");
    // Settle the planning turn before seeding so the review.round is causally a response to the injected completions.
    wait_for_planner_turn_settled(&fx, &harness, planner_planning_budget()).await;
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

    shutdown_planner_harness_if_registered(&fx).await;
    fx.plugin_host
        .stop(PLUGIN_ID)
        .await
        .expect("stop git-forge plugin");
    shutdown_shared_codex(&fx.shared).await;
}

#[tokio::test]
async fn real_planner_gives_up_at_review_cap_from_descriptor() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // Steer the cap-exhaust GIVE-UP branch: GIVE-UP and ASK-HUMAN are mutually exclusive terminal branches of one track.
    let goal = format!(
        "Plan the smallest issue-development template for adding one marker file, \
                then drive design review. If design review cannot converge at the review \
                cap, give up and fail the track; do not request ratification. For every \
                calm.review.round you record for the design phase of this track, set \
                subject.slice_id to exactly the literal string `{STEERED_REVIEW_SLICE}` \
                (that exact value, verbatim — no prefix, suffix, phase qualifier, or \
                derived variant)."
    );
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

    let (plan_actor, _plan) = wait_for_plan_updated(&fx, planner_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiPlannerSession(_)),
        "plan.updated actor must be the real planner session, got {plan_actor:?}"
    );
    // `changed_keys[0]` is the first plan TASK key, not the review SUBJECT slug — different namespaces.
    let slice_id = STEERED_REVIEW_SLICE.to_string();

    let harness = recover_planner_harness(&fx)
        .await
        .expect("live planner harness");
    // Settle the planning turn before seeding so the give-up is causally a response to the injected observations.
    wait_for_planner_turn_settled(&fx, &harness, planner_planning_budget()).await;

    // Pre-position the track at `reviewing` via a raw TrackPatch; walking there by real turns is capstone scope.
    fx.repo_dyn
        .track_update(
            fx.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Reviewing),
                ..TrackPatch::default()
            },
        )
        .await
        .expect("pre-position track lifecycle to reviewing");

    seed_design_channel_changes_requested(&fx, "review-design-a", "a").await;
    seed_design_channel_changes_requested(&fx, "review-design-b", "b").await;
    // Seed ONE prior round ALREADY AT the cap (n=8/cap=8): no further round is kernel-legal, so the agent must escalate directly (seeding n=7 deadlocks this dispatcher-less harness).
    seed_prior_design_review_round(&fx, &slice_id, 8, 8).await;

    let floor = max_event_id(&fx.repo).await;
    let pre_wake_rounds = actor_payload_rows(&fx.repo, "review.round").await;
    assert_eq!(
        pre_wake_rounds.len(),
        1,
        "exactly the one seeded review.round may exist pre-wake (proof-validity guard): {pre_wake_rounds:?}"
    );

    // Wake: inject exactly what the prod dispatcher's `harness_observation_from_event` would push (no dispatcher runs here).
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-a")).await;
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-b")).await;
    inject_design_review_round_observation(&harness, &fx, &slice_id, 8, 8, false).await;

    // Oracle (a): the FSM's give-up edge; the *when* (escalate at the cap instead of ratifying) comes only from the descriptor.
    let (edge_actor, edge) = wait_for_track_failed_edge(&fx, floor, review_budget()).await;
    assert_eq!(
        edge["from"],
        json!("reviewing"),
        "give-up edge must leave reviewing: {edge}"
    );
    assert_eq!(edge["id"], json!(fx.track_id.as_str()));
    assert!(
        matches!(edge_actor, ActorId::AiPlannerSession(_)),
        "give-up edge actor must be AiPlannerSession, got {edge_actor:?} for {edge}"
    );

    // Oracle (b): the tracks row landed on the terminal lifecycle.
    let lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM tracks WHERE id = ?1")
        .bind(fx.track_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select track lifecycle");
    assert_eq!(lifecycle, "failed", "track row lifecycle must be failed");

    // Oracle (c): branch purity — the steered run must neither merge nor ask for ratification.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "steered GIVE-UP run must not request ratification"
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

#[tokio::test]
async fn real_planner_requests_ratification_at_cap_and_resumes_on_grant() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };

    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // Steer the cap-exhaust ASK-HUMAN branch: GIVE-UP and ASK-HUMAN are mutually exclusive terminal branches of one track.
    let goal = format!(
        "Plan the smallest issue-development template for adding one marker file, \
                then drive design review. If design review cannot converge at the review \
                cap, ask for human ratification instead of giving up; do not fail the track. \
                For every calm.review.round you record for the design phase of this track, \
                set subject.slice_id to exactly the literal string \
                `{STEERED_REVIEW_SLICE}` (that exact value, verbatim — no prefix, suffix, \
                phase qualifier, or derived variant)."
    );
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

    let (plan_actor, _plan) = wait_for_plan_updated(&fx, planner_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiPlannerSession(_)),
        "plan.updated actor must be the real planner session, got {plan_actor:?}"
    );
    // `changed_keys[0]` is the first plan TASK key, not the review SUBJECT slug — different namespaces.
    let slice_id = STEERED_REVIEW_SLICE.to_string();

    let harness = recover_planner_harness(&fx)
        .await
        .expect("live planner harness");
    // Settle the planning turn before seeding so the ASK-HUMAN sequence is causally a response to the injected observations.
    wait_for_planner_turn_settled(&fx, &harness, planner_planning_budget()).await;

    // Pre-position the track at `reviewing` via a raw TrackPatch; walking there by real turns is capstone scope.
    fx.repo_dyn
        .track_update(
            fx.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Reviewing),
                ..TrackPatch::default()
            },
        )
        .await
        .expect("pre-position track lifecycle to reviewing");

    seed_design_channel_changes_requested(&fx, "review-design-a", "a").await;
    seed_design_channel_changes_requested(&fx, "review-design-b", "b").await;
    // Seed ONE prior round ALREADY AT the cap (n=8/cap=8): no further round is kernel-legal, so the agent must escalate directly (seeding n=7 deadlocks this dispatcher-less harness).
    seed_prior_design_review_round(&fx, &slice_id, 8, 8).await;

    let floor = max_event_id(&fx.repo).await;
    let pre_wake_rounds = actor_payload_rows(&fx.repo, "review.round").await;
    assert_eq!(
        pre_wake_rounds.len(),
        1,
        "exactly the one seeded review.round may exist pre-wake (proof-validity guard): {pre_wake_rounds:?}"
    );

    // Wake: inject exactly what the prod dispatcher's `harness_observation_from_event` would push (no dispatcher runs here).
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-a")).await;
    inject_task_changes_requested(&harness, &task_id(&fx, "review-design-b")).await;
    inject_design_review_round_observation(&harness, &fx, &slice_id, 8, 8, false).await;

    // Oracle phase 1 (a): the ordered ASK-HUMAN chain. `calm.ratify.request` demands lifecycle==Working and emits working->blocked + ratify.requested in ONE tx, so both must appear.
    let (rw_id, rw_actor, rw_edge) =
        wait_for_track_lifecycle_edge(&fx, floor, "reviewing", "working", ratify_budget()).await;
    assert!(
        matches!(rw_actor, ActorId::AiPlannerSession(_)),
        "reviewing->working edge actor must be AiPlannerSession, got {rw_actor:?} for {rw_edge}"
    );
    let (wb_id, wb_actor, wb_edge) =
        wait_for_track_lifecycle_edge(&fx, rw_id, "working", "blocked", ratify_budget()).await;
    assert!(
        matches!(wb_actor, ActorId::AiPlannerSession(_)),
        "working->blocked edge actor must be AiPlannerSession, got {wb_actor:?} for {wb_edge}"
    );
    // The request is structurally unforgeable: role_gate makes ratify.requested planner-session-only and this test never calls `calm.ratify.request`.
    let (req_id, req_actor, req) = wait_for_ratify_requested(&fx, wb_id, ratify_budget()).await;
    assert!(
        matches!(req_actor, ActorId::AiPlannerSession(_)),
        "ratify.requested actor must be AiPlannerSession, got {req_actor:?} for {req}"
    );
    assert!(
        req["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "ratify.requested must carry a non-empty reason: {req}"
    );
    assert_eq!(req["track_id"], json!(fx.track_id.as_str()));

    // Oracle phase 1 (b): parked, not merged.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);
    assert_eq!(
        track_lifecycle_row(&fx).await,
        "blocked",
        "track row must be blocked while awaiting ratification"
    );

    // Grant through the PRODUCTION HTTP route (in-process oneshot): a `log_pure_event` shortcut is User-only at the role gate and would have to hand-roll the tracks-row flip.
    let app = fixture_router(&fx);
    let body = serde_json::to_vec(&json!({ "decision": "grant" })).expect("grant body");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", fx.planner_card_id))
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

    // Same-tx grant effects: tracks row flipped + ratify.resolved{grant} by the
    // human actor.
    assert_eq!(
        track_lifecycle_row(&fx).await,
        "working",
        "grant must flip the track row blocked->working"
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
    assert_eq!(resolved["track_id"], json!(fx.track_id.as_str()));
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
                    && payload["id"] == json!(fx.track_id.as_str())
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

    // Recovery wake: the same Observation the prod dispatcher would push for ratify.resolved.
    inject_ratify_resolved_grant(&harness, &fx).await;

    // Oracle phase 2 — resumption: the real planner re-enters review (working->reviewing) after the grant.
    let (_resume_id, resume_actor, resume_edge) =
        wait_for_track_lifecycle_edge(&fx, resolved_id, "working", "reviewing", ratify_budget())
            .await;
    assert!(
        matches!(resume_actor, ActorId::AiPlannerSession(_)),
        "post-grant working->reviewing edge actor must be AiPlannerSession, got {resume_actor:?} for {resume_edge}"
    );

    // Post-grant convergence/merge is deliberately NOT asserted: this subject is design-phase with no PR, and no post-grant verdicts are injected.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);

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
    // `changed_keys[0]` is the first plan TASK key, not the review SUBJECT slug — different namespaces.
    let slice_id = STEERED_REVIEW_SLICE.to_string();

    let harness = recover_planner_harness(&fx)
        .await
        .expect("live planner harness");
    // Settle the planning turn before setup/seeding so the merge+close is causally a response to the injected observations.
    wait_for_planner_turn_settled(&fx, &harness, planner_planning_budget()).await;

    // Pre-position the track at `reviewing` via a raw TrackPatch.
    fx.repo_dyn
        .track_update(
            fx.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Reviewing),
                ..TrackPatch::default()
            },
        )
        .await
        .expect("pre-position track lifecycle to reviewing");

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

    // Seed ONE converged impl review.round carrying the REAL branch tip; the actor MUST be AiPlanner(planner card) because role_gate makes review.round planner-only.
    seed_converged_impl_review_round(&fx, &slice_id, pr_number, &head_sha).await;
    let round_id = latest_event_id_of_kind(&fx, "review.round").await;

    let floor = max_event_id(&fx.repo).await;
    // Proof-validity guards: nothing merged yet, and exactly the one seeded round exists.
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

    // Wake: inject what the prod dispatcher would push; the converged ReviewRound observation is the planner's ONLY channel for pr_number/head_sha.
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
    inject_observation(
        &harness,
        Observation::ReviewRound {
            track_id: fx.track_id.clone(),
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

    // Oracle (b) — the F4 proof: the plugin idem is `gh.pr.merge:{repo}:{pr}:{expected_head_sha}` only when expected_head_sha was passed; an omitted-sha merge MUST fail this.
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
            "every gh.pr.merge forge-action op must carry the with-sha idempotency key (F4): {merge_keys:?}"
        );
    }

    // Oracle (c) — ordering: the converged round and the checks event precede the merge.
    assert!(
        round_id < merged_id,
        "converged review.round (id={round_id}) must precede forge.pr.merged (id={merged_id})"
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

    // Oracle (f): purity — no ratification grant, the track must not have failed, the plan must be the planner's own.
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "happy-path merge run must not request ratification"
    );
    let lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM tracks WHERE id = ?1")
        .bind(fx.track_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select track lifecycle");
    assert_ne!(lifecycle, "failed", "track must not fail on the happy path");
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

// Post-grant cap extension to the F4 finish: the planner's SINGLE post-grant round is both the extension (n=9, cap=10) and the convergence; kernel acceptance of that row is the proof.
// No scripted `gh.pr.merge` exists in this file, so the only possible emitter of `forge.pr.merged` is the real planner's own `tools/call`.
#[tokio::test]
async fn real_planner_extends_cap_after_grant_converges_and_merges() {
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
    let goal = extension_merge_goal(&repo_arg);

    boot_planner_harness_via_start_op(&fx, goal).await;

    let (plan_actor, _plan) = wait_for_plan_updated(&fx, planner_planning_budget()).await;
    assert!(
        matches!(plan_actor, ActorId::AiPlannerSession(_)),
        "plan.updated actor must be the real planner session, got {plan_actor:?}"
    );
    // `changed_keys[0]` is the first plan TASK key, not the review SUBJECT slug — different namespaces.
    let slice_id = STEERED_REVIEW_SLICE.to_string();

    let harness = recover_planner_harness(&fx)
        .await
        .expect("live planner harness");
    // Settle the planning turn before setup/seeding.
    wait_for_planner_turn_settled(&fx, &harness, planner_planning_budget()).await;

    // Pre-position the track at `reviewing`.
    fx.repo_dyn
        .track_update(
            fx.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Reviewing),
                ..TrackPatch::default()
            },
        )
        .await
        .expect("pre-position track lifecycle to reviewing");

    // Scripted REAL PR setup (setup, not the proof).
    let branch = "neige-r7c-impl-slice";
    run_git(&fx.track_cwd, ["checkout", "-B", branch, "origin/main"]);
    stage_git_change(&fx.track_cwd, "FORGE_E2E_R7C.md", "forge-e2e-r7c\n");
    run_git(&fx.track_cwd, ["commit", "-m", "r7c scripted impl commit"]);
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
        212,
        PR_CHECKS_TOOL,
        json!({ "repo": repo_arg, "pr": pr_number }),
    )
    .await;
    assert_forge_tool_accepted(&checks_resp, "gh.pr.checks");
    let (_checks_id, _, _checks) = wait_for_track_forge_event(
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

    // Seed ONE prior impl round ALREADY AT the cap (n=8/cap=8) carrying the REAL tip: no further round is legal pre-grant, and the +2 extension makes n=9/cap=10 the single legal round after.
    seed_prior_impl_review_round(&fx, &slice_id, pr_number, &head_sha, 8, 8).await;

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

    // Wake: dispatcher-shaped observations only (no dispatcher runs here).
    inject_task_changes_requested(&harness, &task_id(&fx, "review-pr-a")).await;
    inject_task_changes_requested(&harness, &task_id(&fx, "review-pr-b")).await;
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
    inject_observation(
        &harness,
        Observation::ReviewRound {
            track_id: fx.track_id.clone(),
            phase: "impl".into(),
            slice_id: slice_id.clone(),
            pr_number: Some(pr_number),
            head_sha: Some(head_sha.clone()),
            n: 8,
            cap: 8,
            converged: false,
        },
    )
    .await;

    // Phase 1 (a) — the ordered ASK-HUMAN chain, waited from the pre-wake `floor`.
    let (rw_id, rw_actor, rw_edge) =
        wait_for_track_lifecycle_edge(&fx, floor, "reviewing", "working", ratify_budget()).await;
    assert!(
        matches!(rw_actor, ActorId::AiPlannerSession(_)),
        "reviewing->working edge actor must be AiPlannerSession, got {rw_actor:?} for {rw_edge}"
    );
    let (wb_id, wb_actor, wb_edge) =
        wait_for_track_lifecycle_edge(&fx, rw_id, "working", "blocked", ratify_budget()).await;
    assert!(
        matches!(wb_actor, ActorId::AiPlannerSession(_)),
        "working->blocked edge actor must be AiPlannerSession, got {wb_actor:?} for {wb_edge}"
    );
    let (req_id, req_actor, req) = wait_for_ratify_requested(&fx, wb_id, ratify_budget()).await;
    assert!(
        matches!(req_actor, ActorId::AiPlannerSession(_)),
        "ratify.requested actor must be AiPlannerSession, got {req_actor:?} for {req}"
    );

    // Phase 1 (c) — parked, not merged.
    assert_eq!(event_payloads(&fx.repo, "forge.pr.merged").await.len(), 0);
    assert_eq!(
        track_lifecycle_row(&fx).await,
        "blocked",
        "track row must be blocked while awaiting ratification"
    );

    // Grant through the PRODUCTION HTTP route: blocked->working + ratify.resolved{grant}, both User.
    let app = fixture_router(&fx);
    let body = serde_json::to_vec(&json!({ "decision": "grant" })).expect("grant body");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", fx.planner_card_id))
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
        track_lifecycle_row(&fx).await,
        "working",
        "grant must flip the track row blocked->working"
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
                    && payload["id"] == json!(fx.track_id.as_str())
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

    // Recovery wake + post-grant APPROVED verdicts, injected BEFORE the planner's next round so that round is both the extension and the convergence.
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

    // Oracle 1 — THE extension round: n=9 (= old cap + 1), cap=10 (= old cap + 2), converged, planner-authored.
    let (ext_round_id, ext_round_actor, ext_round) = wait_for_impl_review_round_on_subject(
        &fx,
        resolved_id,
        &slice_id,
        pr_number,
        review_budget(),
    )
    .await;
    assert!(
        matches!(ext_round_actor, ActorId::AiPlannerSession(_)),
        "extension round actor must be AiPlannerSession, got {ext_round_actor:?} for {ext_round}"
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
    let (merged_id, merged_actor, merged) = wait_for_track_forge_event(
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
    // the WITH-sha shape from the planner seat (d2 oracle (b)).
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

    // Oracle 4 — ordering by row id: ratify.requested < ratify.resolved{grant} < extension round < forge.pr.merged.
    assert!(
        req_id < resolved_id && resolved_id < ext_round_id && ext_round_id < merged_id,
        "ordering violated: requested={req_id}, resolved={resolved_id}, \
         extension={ext_round_id}, merged={merged_id}"
    );

    // Oracle 6 — exactly ONE cap extension on the impl subject, zero on every other subject.
    let extensions = assert_cap_extension_history(&fx.repo, fx.track_id.as_str()).await;
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

    // Oracle 7 — the plan was the planner's own.
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

/// Environment facts plus descriptor-legal ASK-HUMAN steering; the `+2` cap-extension rule is deliberately NOT restated and must come from the descriptor.
fn extension_merge_goal(repo_gitdir: &str) -> String {
    format!(
        "Drive the tail of the issue-development template. Environment facts: the `repo` \
         argument for every gh.* MCP forge tool is exactly `{repo_gitdir}`. Implementation, \
         the pull request, and both PR review channels are already complete for this track; \
         their results arrive as observations. If the impl review cannot converge at the \
         review cap, ask for human ratification instead of giving up; do not fail the track. \
         Once the impl review round for the pull request reports converged, execute the \
         merge step yourself with the MCP forge tools (gh.pr.merge); do not dispatch \
         further tasks. For every calm.review.round you record for the impl phase of this \
         track, set subject.slice_id to exactly the literal string \
         `{STEERED_REVIEW_SLICE}` (that exact value, verbatim — no prefix, suffix, phase \
         qualifier, or derived variant)."
    )
}

/// Seed ONE prior non-converged impl `review.round` at `n`/`cap` carrying the REAL tip; actor MUST be `AiPlanner(planner card)` with `EventScope::Track` (role_gate makes review.round planner-only).
async fn seed_prior_impl_review_round(
    fx: &Fixture,
    slice_id: &str,
    pr_number: u64,
    head_sha: &str,
    n: u32,
    cap: u32,
) {
    let track_scope = EventScope::Track {
        track: fx.track_id.clone(),
        area: fx.area_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::AiPlanner(fx.planner_card_id.clone()),
            track_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.track_area_cache,
            Event::ReviewRound {
                track_id: fx.track_id.clone(),
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
                // Canonical shape from `review_round_idempotency_key`: PR subjects carry the pr number in the pr slot.
                idempotency_key: format!(
                    "review.round:{}:impl:{}:{}:{}",
                    fx.track_id.as_str(),
                    slice_id,
                    pr_number,
                    n
                ),
            },
        )
        .await
        .expect("log seeded prior impl review.round");
}

/// Timeout diagnostic for review-round subjects observed after a floor.
async fn review_round_subjects_after(fx: &Fixture, floor: i64) -> Vec<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT payload FROM events \
         WHERE kind = 'review.round' AND id > ?1 ORDER BY id ASC",
    )
    .bind(floor)
    .fetch_all(fx.repo.pool())
    .await
    .unwrap_or_else(|e| panic!("review.round diagnostic rows after floor {floor}: {e}"));
    let mut subjects: Vec<String> = rows
        .into_iter()
        .map(|payload| {
            let payload: Value = serde_json::from_str(&payload).expect("event payload json");
            format!(
                "{}/{}",
                payload["subject"]["phase"].as_str().unwrap_or("<missing>"),
                payload["subject"]["slice_id"]
                    .as_str()
                    .unwrap_or("<missing>")
            )
        })
        .collect();
    subjects.sort();
    subjects.dedup();
    subjects
}

/// First post-floor `review.round` on the FULL impl subject (a pr-less round is a different stream); returns the event id.
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
            let subjects = review_round_subjects_after(fx, floor).await;
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for post-floor impl review.round \
                     on slice {slice_id} pr {pr_number} after event id {floor}; \
                     review.round subjects observed after floor: {subjects:?}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

// CAPSTONE: one REAL run of the full issue→PR→merge→close backbone with a LIVE dispatcher — zero injected observations, zero seeded rows, zero lifecycle pre-positioning; ANY `ratify.requested` fails the test.
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

    // Dispatcher permits 4 so reviewer pairs run in parallel; the scheduler also enforces the per-track task budget (default 1), so raise it to match.
    fx.repo_dyn
        .track_update(
            fx.track_id.as_str(),
            TrackPatch {
                task_budget: Some(Some(4)),
                ..TrackPatch::default()
            },
        )
        .await
        .expect("raise track task budget for parallel reviewer pairs");

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

    // S2/S9 — the planner records a design review round after the real reviewer workers complete; round count is tolerated.
    let (_design_round_id, design_round_actor, design_round) = wait_capstone_event(
        &fx,
        "review.round",
        0,
        st(),
        "planner records a design review round",
        |p| p["subject"]["phase"] == json!("design"),
    )
    .await;
    assert!(
        matches!(design_round_actor, ActorId::AiPlannerSession(_)),
        "design review.round actor must be AiPlannerSession, got {design_round_actor:?} for {design_round}"
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

    // S9/S10 — the converged impl review round. `subject.pr_number` is OPTIONAL per the manifest, so tolerate absent/null and require equality only when present.
    let (round_id, round_actor, round) = wait_capstone_event(
        &fx,
        "review.round",
        diff_id,
        st(),
        "planner records the converged impl review round",
        |p| {
            p["subject"]["phase"] == json!("impl")
                && p["converged"] == json!(true)
                && subject_pr_absent_or_matches(&p["subject"], pr_number)
        },
    )
    .await;
    assert!(
        matches!(round_actor, ActorId::AiPlannerSession(_)),
        "impl review.round actor must be AiPlannerSession, got {round_actor:?} for {round}"
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
    // F4 direct assert against the LATEST (max-n) pre-merge round: a converge → late-fix → re-converge run legitimately merges on the newer head.
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

    // S13 — the planner drives the track lifecycle to done.
    let (_done_id, done_actor, done_edge) = wait_capstone_event(
        &fx,
        "track.lifecycle_changed",
        merged_id,
        st(),
        "planner transitions the track to done",
        |p| p["to"] == json!("done") && p["id"] == json!(fx.track_id.as_str()),
    )
    .await;
    assert!(
        matches!(done_actor, ActorId::AiPlannerSession(_)),
        "→done lifecycle edge actor must be AiPlannerSession, got {done_actor:?} for {done_edge}"
    );

    // Post-run oracle.
    capstone_oracle(&fx, pr_number, &merged_head, &subject, &repo_gitdir).await;

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

/// `subject.pr_number` is optional per the descriptor, so absent/null is legal; when present it must match.
fn subject_pr_absent_or_matches(subject: &Value, pr_number: u64) -> bool {
    match subject.get("pr_number") {
        None => true,
        Some(Value::Null) => true,
        Some(v) => *v == json!(pr_number),
    }
}

/// The F4 fence round: the max-n impl `review.round` on `slice_id` with event id strictly BEFORE the merge (latest-n, not first-converged).
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

/// Capstone track goal: environment facts plus descriptor-legal planning steering; PR coordinates must flow through observations/runs.
fn capstone_goal(repo_gitdir: &str, issue_number: u64, base_sha: &str) -> String {
    format!(
        "Drive the bound issue-development template END-TO-END for issue #{issue_number}: read \
         the issue, converge design review, implement, open a pull request, converge PR review, \
         merge, close the issue, and move the track lifecycle to done.\n\
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
         blocks in dependency order: (1) inspect-issue, then \
         review-design-a, review-design-b and implement-change; (2) add open-pr only after \
         implement-change completes, embedding the implement worker's actual branch name in \
         its goal; (3) after open-pr completes, add review-pr-a and review-pr-b, then add merge \
         after both review task blocks exist, embedding the literal repo, pr number, base sha, \
         head sha and reviewed slice_id values in each of their goals.\n\
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
         - After the merge task completes and the issue is closed, transition the track \
         lifecycle to done.\n\
         - If a review subject cannot converge at the review cap, give up and fail the track; \
         do not request ratification."
    )
}

/// Stage wait with the failure terminator folded in: `ratify.requested` at ANY point or a `failed` track fails fast with agent diagnostics.
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
        if track_lifecycle_row(fx).await == "failed" {
            panic_with_agent_diag(
                fx,
                format!(
                    "track lifecycle landed `failed` (planner gave up — terminal) while waiting \
                     for {kind} ({describe})"
                ),
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
            RequiredEvent::any("plan.updated"),
            RequiredEvent::new("task.dispatched", |r| r.payload["kind"] == json!("codex")),
            RequiredEvent::any("workspace.leased"),
            RequiredEvent::any("worktree.provisioned"),
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
            RequiredEvent::new("track.lifecycle_changed", |r| {
                r.payload["to"] == json!("done")
            }),
        ],
    )
    .await;

    // Ordering 2 (latest converged design round < first impl dispatch) is deliberately NOT asserted: the scheduler never reads review state.
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
    // precedes worker_session.started for every card that has both.
    assert_provisioned_before_worker_session_started_per_card(fx).await;

    // Fence 6: subject-keyed cap enforcement. 6a (merge keyed by FULL subject) is replaced by the in-line latest-fence assert because a round may legally omit pr_number.
    assert_subject_keyed_cap_enforcement(&fx.repo, fx.track_id.as_str()).await;
    if subject.pr_number.is_some() {
        assert_converged_subject_has_merge(&fx.repo, subject).await;
    }

    // Actor table (event-row column, never payload).
    for (actor, payload) in actor_payload_rows(&fx.repo, "plan.updated").await {
        assert!(
            matches!(actor, ActorId::AiPlannerSession(_)),
            "plan.updated actor must be AiPlannerSession, got {actor:?} for {payload}"
        );
    }
    for (actor, payload) in actor_payload_rows(&fx.repo, "review.round").await {
        assert!(
            matches!(actor, ActorId::AiPlannerSession(_)),
            "review.round actor must be AiPlannerSession, got {actor:?} for {payload}"
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

    // F4 idem-key shape: the plugin idem carries `:{expected_head_sha}` ONLY when it was passed. The caller card is NOT pinned: a merge-worker seat is as legal as the planner seat.
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

    // Purity: never ratified, never failed, never the injected-plan path.
    assert_eq!(
        event_payloads(&fx.repo, "ratify.requested").await.len(),
        0,
        "steered GIVE-UP capstone must never request ratification"
    );
    assert_eq!(
        track_lifecycle_row(fx).await,
        "done",
        "capstone track row must land done"
    );
    assert!(
        !fx.used_injected_plan(),
        "RealPlannerTurn must not use injected plan path"
    );
}

/// For every card with BOTH events, the first `worktree.provisioned` precedes the first `worker_session.started` (the planner card has no worktree — vacuously skipped).
async fn assert_provisioned_before_worker_session_started_per_card(fx: &Fixture) {
    let provisioned = event_rows(&fx.repo, "worktree.provisioned").await;
    let started = event_rows(&fx.repo, "worker_session.started").await;
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
                 worker_session.started (id {first_started})"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "ordering-3 check matched no card with both worktree.provisioned and \
         worker_session.started"
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

async fn seed_design_channel_complete(fx: &Fixture, key: &str, chan: &str) {
    seed_design_channel_verdict(fx, key, chan, "approved").await;
}

async fn seed_design_channel_changes_requested(fx: &Fixture, key: &str, chan: &str) {
    seed_design_channel_verdict(fx, key, chan, "changes_requested").await;
}

/// Seed a design review-channel task pair carrying `verdict`, then fail fast unless the runs/ projection surfaces it.
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

#[cfg(feature = "fixtures")]
async fn inject_task_completed(h: &PlannerHarness, idem_key: &str) {
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
async fn inject_task_completed(_h: &PlannerHarness, _idem_key: &str) {
    panic!("inject_task_completed requires the fixtures feature");
}

#[cfg(feature = "fixtures")]
async fn inject_task_changes_requested(h: &PlannerHarness, idem_key: &str) {
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
async fn inject_task_changes_requested(_h: &PlannerHarness, _idem_key: &str) {
    panic!("inject_task_changes_requested requires the fixtures feature");
}

/// Inject the same `Observation::ReviewRound` the prod dispatcher would push; no review.round projection exists, so this is the planner's ONLY channel for round state.
#[cfg(feature = "fixtures")]
async fn inject_design_review_round_observation(
    h: &PlannerHarness,
    fx: &Fixture,
    slice_id: &str,
    n: u32,
    cap: u32,
    converged: bool,
) {
    h.observe_for_test(
        Observation::ReviewRound {
            track_id: fx.track_id.clone(),
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
    _h: &PlannerHarness,
    _fx: &Fixture,
    _slice_id: &str,
    _n: u32,
    _cap: u32,
    _converged: bool,
) {
    panic!("inject_design_review_round_observation requires the fixtures feature");
}

/// Seed one prior non-converged design review.round; actor MUST be `AiPlanner(planner card)` with `EventScope::Track` (role_gate makes review.round planner-only).
async fn seed_prior_design_review_round(fx: &Fixture, slice_id: &str, n: u32, cap: u32) {
    let track_scope = EventScope::Track {
        track: fx.track_id.clone(),
        area: fx.area_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::AiPlanner(fx.planner_card_id.clone()),
            track_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.track_area_cache,
            Event::ReviewRound {
                track_id: fx.track_id.clone(),
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
                // Canonical shape from `review_round_idempotency_key`: design subjects use the literal "design" in the pr slot.
                idempotency_key: format!(
                    "review.round:{}:design:{}:design:{}",
                    fx.track_id.as_str(),
                    slice_id,
                    n
                ),
            },
        )
        .await
        .expect("log seeded prior review.round");
}

/// First post-floor `track.lifecycle_changed` landing on `failed` for the
/// fixture track. Other lifecycle transitions are tolerated.
async fn wait_for_track_failed_edge(
    fx: &Fixture,
    floor: i64,
    budget: Duration,
) -> (ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, actor, payload FROM events \
             WHERE kind = 'track.lifecycle_changed' AND id > ?1 ORDER BY id ASC",
        )
        .bind(floor)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("track.lifecycle_changed rows after floor {floor}: {e}"));
        let hit = rows.into_iter().find_map(|(_, actor, payload)| {
            let actor: ActorId = serde_json::from_str(&actor).expect("event actor json");
            let payload: Value = serde_json::from_str(&payload).expect("event payload json");
            (payload["to"] == json!("failed") && payload["id"] == json!(fx.track_id.as_str()))
                .then_some((actor, payload))
        });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for track.lifecycle_changed to=failed \
                     after event id {floor}"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

/// First post-floor `track.lifecycle_changed` matching `from -> to` for the fixture track; returns the event id.
async fn wait_for_track_lifecycle_edge(
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
                    && payload["id"] == json!(fx.track_id.as_str())
            });
        if let Some(hit) = hit {
            return hit;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for track.lifecycle_changed \
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
         WHERE kind = 'track.lifecycle_changed' AND id > ?1 ORDER BY id ASC",
    )
    .bind(floor)
    .fetch_all(fx.repo.pool())
    .await
    .unwrap_or_else(|e| panic!("track.lifecycle_changed rows after floor {floor}: {e}"));
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

/// `track.lifecycle_changed` rows strictly inside the `(after, before)` event
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

/// First post-floor `ratify.requested`; role_gate makes it planner-session-only, so an observed row proves the real planner's own tool call.
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

async fn track_lifecycle_row(fx: &Fixture) -> String {
    sqlx::query_scalar("SELECT lifecycle FROM tracks WHERE id = ?1")
        .bind(fx.track_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select track lifecycle")
}

/// The production HTTP grant seam: the real `routes::router()` behind `actor_middleware` over the fixture's live parts, driven via `oneshot`.
fn fixture_router(fx: &Fixture) -> axum::Router {
    let state = AppState::from_parts(
        fx.repo_dyn.clone(),
        fx.events.clone(),
        fx.daemon.clone(),
        fx.plugin_host.clone(),
        fx.codex.clone(),
        Some(fx.cache.clone()),
        Some(fx.track_area_cache.clone()),
    );
    calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

/// Inject the same `Observation::RatifyResolved` the prod dispatcher would push for the grant.
#[cfg(feature = "fixtures")]
async fn inject_ratify_resolved_grant(h: &PlannerHarness, fx: &Fixture) {
    h.observe_for_test(
        Observation::RatifyResolved {
            track_id: fx.track_id.clone(),
            decision: calm_server::event::RatifyDecision::Grant,
        },
        None,
    )
    .await;
}

#[cfg(not(feature = "fixtures"))]
async fn inject_ratify_resolved_grant(_h: &PlannerHarness, _fx: &Fixture) {
    panic!("inject_ratify_resolved_grant requires the fixtures feature");
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
        "RealPlannerTurn must not use injected plan path"
    );

    let mut by_subject: std::collections::BTreeMap<(String, String, Option<u64>), Vec<&Value>> =
        std::collections::BTreeMap::new();
    for (actor, payload) in rounds {
        assert!(
            matches!(actor, ActorId::AiPlannerSession(_)),
            "review.round actor must be AiPlannerSession, got {actor:?} for {payload}"
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
        "Drive the tail of the issue-development template for issue #{issue_number}. \
         Environment facts: the `repo` argument for every gh.* MCP forge tool is exactly \
         `{repo_gitdir}`; the track's source issue is #{issue_number}. Implementation, the \
         pull request, and both PR review channels are already complete for this track; \
         their results arrive as observations. Once the impl review round for the pull \
         request reports converged, execute the merge step yourself with the MCP forge \
         tools (gh.pr.merge, then gh.issue.close for issue #{issue_number}); do not \
         dispatch further tasks. For every calm.review.round you record for the impl phase \
         of this track, set subject.slice_id to exactly the literal string \
         `{STEERED_REVIEW_SLICE}` (that exact value, verbatim — no prefix, suffix, phase \
         qualifier, or derived variant)."
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

/// Seed the ONE converged impl review.round; actor MUST be `AiPlanner(planner card)` with `EventScope::Track` (role_gate makes review.round planner-only).
async fn seed_converged_impl_review_round(
    fx: &Fixture,
    slice_id: &str,
    pr_number: u64,
    head_sha: &str,
) {
    let track_scope = EventScope::Track {
        track: fx.track_id.clone(),
        area: fx.area_id.clone(),
    };
    fx.repo
        .log_pure_event(
            ActorId::AiPlanner(fx.planner_card_id.clone()),
            track_scope,
            None,
            &fx.events,
            &fx.cache,
            &fx.track_area_cache,
            Event::ReviewRound {
                track_id: fx.track_id.clone(),
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
                // Canonical shape from `review_round_idempotency_key`: PR subjects carry the pr number in the pr slot.
                idempotency_key: format!(
                    "review.round:{}:impl:{}:{}:1",
                    fx.track_id.as_str(),
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

/// Budget for the ASK-HUMAN request-wait and the post-grant resume-wait; each spans a full real planner turn.
fn ratify_budget() -> Duration {
    std::env::var("NEIGE_PLANNER_RATIFY_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(480))
}
