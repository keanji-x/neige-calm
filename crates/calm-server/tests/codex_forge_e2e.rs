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
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{ChannelVerdict, ChannelVerdictKind, Event, EventScope, ReviewSubject};
use calm_server::harness::{HarnessState, Observation, SpecHarness};
use calm_server::ids::ActorId;
use calm_server::mcp_server::tools::wave_file::TOOL_WAVE_CAT;
use calm_server::model::{WaveLifecycle, WavePatch};
use serde_json::{Value, json};
use support::agent_diag::panic_with_agent_diag;
use support::codex_fixture::*;
use support::event_queries::*;
use support::forge_env::FORGE_ENV_LOCK;
use support::git_helpers::*;
use support::spec_turn::*;
use tokio::time::{Instant, sleep};

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
                agent_message: Some(format!("[codex-forge-e2e] seed design review {chan}")),
            },
        )
        .await
        .expect("log seeded design task.dispatched");

    // The §7.3 design-phase fixture shortcut does not mint a real review-worker
    // session, so author the completion as KernelDispatcher (gate-unrestricted
    // per role_gate rule 5). Card scope alone routes it to the completed bucket
    // (is_spec_verdict_event is false for non-Wave scope), so runs/ surfaces the
    // verdict the real spec reads.
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
                result: json!({
                    "summary": verdict,
                    "verdict": verdict,
                    "channel": chan,
                }),
                artifacts: Vec::new(),
                agent_message: Some(format!("[codex-forge-e2e] review {chan} {verdict}")),
            },
        )
        .await
        .expect("log seeded design task.completed");

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
        "seeded design review run {task_id} did not expose exact {verdict} summary in runs projection; \
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

fn review_budget() -> Duration {
    std::env::var("NEIGE_SPEC_REVIEW_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        // Default doubled vs spec_planning_budget — the review wait includes the spec's autonomous runs/ read round-trip and 1/5 stability runs exceeded 240s (design §6 headroom).
        .unwrap_or_else(|| Duration::from_secs(480))
}
