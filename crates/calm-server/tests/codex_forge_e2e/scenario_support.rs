//! Shared steps for the scripted and autonomous planner scenarios: goals, seeded task pairs,
//! planner-turn waits, forge-event waits and shim counters.

use std::path::Path;
use std::time::Duration;

use crate::support::agent_diag::panic_with_agent_diag;
use crate::support::codex_fixture::*;
use crate::support::planner_turn::*;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventScope};
use calm_server::harness::{HarnessState, Observation, PlannerHarness};
use calm_server::ids::{ActorId, TrackId};
use calm_server::mcp_server::tools::track_file::TOOL_TRACK_CAT;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

/// The environment fact every bound-template goal states: the dev input names the
/// fixture's GitHub repository, which the gh shim serves only through its local selector.
pub(super) fn gh_selector_fact(selector: &str) -> String {
    format!(
        "Environment facts: the track's GitHub repository is served locally in this environment; \
         the `repo` argument for every gh.* MCP forge tool is exactly `{selector}`, not input.repo. \
         Embed this exact literal in the goal of every task that must call a gh.* tool."
    )
}

/// Seed a pipeline task pair whose completion carries `result`, then fail fast unless the runs/ projection surfaces `expected_summary`.
pub(super) async fn seed_completed_task_pair(
    fx: &Fixture,
    key: &str,
    result: Value,
    expected_summary: &str,
) {
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

pub(super) async fn recover_planner_harness(fx: &Fixture) -> Option<PlannerHarness> {
    let runtime = fx
        .repo
        .session_projection_active_for_card(&fx.planner_card_id.to_string())
        .await
        .ok()
        .flatten()?;
    fx.harness.get(&runtime.id)
}

pub(super) async fn track_is_closed(fx: &Fixture) -> bool {
    sqlx::query_scalar("SELECT closed_at IS NOT NULL FROM tracks WHERE id = ?1")
        .bind(fx.track_id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .expect("select track closed_at")
}

pub(super) async fn wait_for_planner_turn_settled(
    fx: &Fixture,
    h: &PlannerHarness,
    budget: Duration,
) {
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

pub(super) async fn max_event_id(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events")
        .fetch_one(repo.pool())
        .await
        .expect("select max event id")
}

pub(super) fn merge_close_goal(repo_gitdir: &str, issue_number: u64) -> String {
    format!(
        "Drive the tail of the bound dev template for issue #{issue_number}. \
         {} Implementation, the \
         pull request, and its review are already complete for this track; their results \
         arrive as observations. Once the review approves the pull request, execute the \
         merge step yourself with the MCP forge tools (gh.pr.merge, then gh.issue.close \
         for issue #{issue_number}); do not dispatch further tasks.",
        gh_selector_fact(repo_gitdir)
    )
}

/// The live planner session's bound codex thread id, the identity for scripted daemon-socket `tools/call`s.
pub(super) async fn planner_session_thread_id(fx: &Fixture) -> String {
    fx.repo
        .session_projection_active_for_card(&fx.planner_card_id.to_string())
        .await
        .expect("active planner session lookup")
        .expect("live planner session for planner card")
        .thread_id
        .expect("planner session bound to a codex thread")
}

pub(super) fn assert_forge_tool_accepted(resp: &Value, label: &str) {
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
pub(super) async fn wait_for_track_forge_event(
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

pub(super) async fn latest_event_id_of_kind(fx: &Fixture, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("max {kind} event id: {e}"))
}

/// All forge-action op idempotency keys containing `needle`, oldest first; the shape `{plugin}:{track}:{caller card}:{plugin idem}` pins both the seat and the plugin idem.
pub(super) async fn forge_action_idem_keys_containing(fx: &Fixture, needle: &str) -> Vec<String> {
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

pub(super) fn shim_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

#[cfg(feature = "fixtures")]
pub(super) async fn inject_observation(h: &PlannerHarness, obs: Observation) {
    h.observe_for_test(obs, None).await;
}

#[cfg(not(feature = "fixtures"))]
pub(super) async fn inject_observation(_h: &PlannerHarness, _obs: Observation) {
    panic!("inject_observation requires the fixtures feature");
}

pub(super) fn review_budget() -> Duration {
    std::env::var("NEIGE_PLANNER_REVIEW_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        // Doubled vs planner_planning_budget: the review wait includes the planner's autonomous runs/ read round-trip.
        .unwrap_or_else(|| Duration::from_secs(480))
}
