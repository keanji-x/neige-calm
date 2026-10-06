//! Capstone stage waits, track goal and post-run oracle.

use std::path::PathBuf;
use std::time::Duration;

use super::scenario_support::*;
use crate::support::agent_diag::panic_with_agent_diag;
use crate::support::codex_fixture::*;
use crate::support::event_queries::*;
use crate::support::git_helpers::*;
use crate::support::oracle::{
    OrderingEdge, RequiredEvent, assert_event_skeleton_superset, assert_ordering,
};
use crate::support::planner_turn::*;
use calm_server::ids::ActorId;
use calm_server::session_projection_repo::WorkerSessionProjectionRepo;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

/// The capstone's source issue number. An environment fact for the gh shim
/// (state keyed per repo selector); any number works.
pub(super) const CAPSTONE_ISSUE_NUMBER: u64 = 840;

/// The real code task (P2): named-function contract so the oracle can assert
/// the diff at content-invariant level without matching stochastic text.
pub(super) const CAPSTONE_ISSUE_BODY: &str = "Add `pub fn is_palindrome(s: &str) -> bool` to `src/lib.rs` \
with a `///` doc comment and a `#[test]` unit test covering a palindrome and a \
non-palindrome. Do not push; do not merge.\n";

pub(super) fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// head_sha of the latest `forge.pr.diff.read` for `pr_number` strictly BEFORE the merge.
pub(super) async fn latest_reviewed_head_before_merge(
    fx: &Fixture,
    merged_id: i64,
    pr_number: u64,
) -> String {
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

/// Capstone track goal: environment facts plus planning steering; PR coordinates must flow through observations/runs.
pub(super) fn capstone_goal(repo_gitdir: &str, issue_number: u64, base_sha: &str) -> String {
    format!(
        "Complete issue #{issue_number} through the bound dev template.\n\
         Inspect, implement, publish, review, merge, close the issue, then close the track.\n\
         Environment: every gh.* tool's repo argument is exactly {repo_gitdir}. Embed that literal in worker goals. Base branch main has SHA {base_sha}.\n\
         Copy the bound template gate's exact cmd to every task; no no_gate_reason. Declare tasks in dependency order: inspect-issue, then implement-change.\n\
         implement-change edits src/lib.rs as the issue requires, calls the git_commit MCP tool with a message and nonempty idem, and reports its branch with neige_task_done. Workers must not push or open a PR and must use MCP for git.\n\
         After implement-change is Done and its delivery settled, YOU the Planner must call neige_dev_publish with title, body and a stable idempotency_key. Use the returned pr_number, head_sha and branch. Call gh_pr_checks for that PR. Do not create an open-pr worker task.\n\
         Then declare review-pr with literal repo, pr_number, base_sha and head_sha: it must call gh_pr_diff, review against the issue, and report approved or changes_requested.\n\
         Declare merge only after the review task exists and merge only after approval and successful checks. Its gh_pr_merge must pass expected_head_sha equal to the reviewed head. It then calls gh_issue_close for #{issue_number} at the same repo.\n\
         After merge completes and the issue closes, call neige_track_close. If review cannot converge, close the track with a reason; do not ask the user."
    )
}

/// Stage wait with the failure terminator folded in: `ask.requested` at ANY point, or a close before the awaited one, fails fast with agent diagnostics.
pub(super) async fn wait_capstone_event(
    fx: &Fixture,
    kind: &str,
    floor: i64,
    budget: Duration,
    describe: &str,
    predicate: impl Fn(&Value) -> bool,
) -> (i64, ActorId, Value) {
    let deadline = Instant::now() + budget;
    loop {
        if !event_payloads(&fx.repo, "ask.requested").await.is_empty() {
            panic_with_agent_diag(
                fx,
                format!(
                    "ask.requested emitted during the steered GIVE-UP capstone (purity \
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
pub(super) async fn capstone_oracle(
    fx: &Fixture,
    pr_number: u64,
    merged_head: &str,
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
            RequiredEvent::any("worker_session.started"),
            RequiredEvent::any("task.completed"),
            RequiredEvent::any("worktree.committed"),
            RequiredEvent::new("task.gate_result", |r| r.payload["passed"] == json!(true)),
            RequiredEvent::new("forge.issue.read", |r| {
                r.payload["issue_number"] == json!(CAPSTONE_ISSUE_NUMBER)
            }),
            RequiredEvent::any("forge.pr.published"),
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

    // The real Planner publish operation identifies its caller, the same PR/head, and its session.
    let publications: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT payload_json, tx_output_json, created_at_ms FROM operations \
         WHERE kind = 'forge-action' AND idempotency_key LIKE '%:track.publish:%' AND phase = 'succeeded'")
        .fetch_all(fx.repo.pool()).await.unwrap();
    let opened = event_rows(&fx.repo, "forge.pr.published")
        .await
        .into_iter()
        .find(|row| row.payload["pr_number"] == json!(pr_number))
        .expect("published PR");
    let head = opened.payload["head_sha"].as_str().unwrap();
    let matched = publications
        .iter()
        .find(|(raw, _, _)| {
            let payload: Value = serde_json::from_str(raw).unwrap();
            payload["card_id"] == json!(fx.planner_card_id.as_str())
                && payload["track_id"] == json!(fx.track_id.as_str())
                && payload["argv"]
                    .as_array()
                    .is_some_and(|argv| argv.iter().any(|arg| arg == head))
        })
        .expect("Planner-owned publish operation for the opened head");
    let output: Value = serde_json::from_str(&matched.1).unwrap();
    assert_eq!(output["result"]["event"]["pr_number"], pr_number);
    assert_eq!(output["result"]["event"]["head_sha"], head);
    let session_id = actor_payload_rows(&fx.repo, "track.updated")
        .await
        .into_iter()
        .find_map(|(actor, payload)| match actor {
            ActorId::AiPlannerSession(id) if !payload["closed_at"].is_null() => Some(id),
            _ => None,
        })
        .expect("real Planner close session");
    let session = fx
        .repo
        .session_projection_by_id(session_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        session.thread_id.is_some(),
        "real Planner session must have a thread"
    );
    assert_eq!(session.card_id, fx.planner_card_id.as_str());
    let attempt: (String, i64) = sqlx::query_as(
        "SELECT t.id, t.updated_at_ms FROM task_candidates c \
         JOIN tasks t ON t.id = c.producer_attempt_id \
         WHERE c.track_id = ?1 AND c.commit_sha = ?2 AND t.key = 'implement-change' AND t.status = 'done'")
        .bind(fx.track_id.as_str()).bind(head).fetch_one(fx.repo.pool()).await.unwrap();
    assert!(
        attempt.1 <= matched.2,
        "implement Done must precede publish submission"
    );
    for kind in ["task.gate_result", "task.git_delivery_settled"] {
        assert!(
            event_rows(&fx.repo, kind)
                .await
                .iter()
                .any(|row| row.id < opened.id
                    && row.payload["task_id"] == attempt.0
                    && (kind != "task.gate_result" || row.payload["passed"] == true)),
            "{kind} for implement attempt must precede opened"
        );
    }

    // Merge idem-key shape: the plugin idem carries `:{expected_head_sha}` ONLY when it was passed. The caller card is NOT pinned: a merge-worker seat is as legal as the planner seat.
    let merge_keys = forge_action_idem_keys_containing(fx, ":gh.pr.merge:").await;
    assert!(
        !merge_keys.is_empty(),
        "expected a parked forge-action gh_pr_merge operation row"
    );
    let merge_suffix = format!(":gh.pr.merge:{repo_gitdir}:{pr_number}:{merged_head}");
    for key in &merge_keys {
        assert!(
            key.ends_with(&merge_suffix),
            "every gh_pr_merge forge-action op must carry the WITH-sha idem key \
             (expected suffix {merge_suffix}): {merge_keys:?}"
        );
    }
    let close_keys = forge_action_idem_keys_containing(fx, ":gh.issue.close:").await;
    assert!(
        !close_keys.is_empty(),
        "expected a parked forge-action gh_issue_close operation row"
    );
    let close_suffix = format!(":gh.issue.close:{repo_gitdir}:{CAPSTONE_ISSUE_NUMBER}");
    for key in &close_keys {
        assert!(
            key.ends_with(&close_suffix),
            "every gh_issue_close forge-action op must target the goal issue at the \
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
        "publish must leave the bare origin main unchanged"
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

    // Purity: never asked the user, never the injected-plan path, and the track closed.
    assert_eq!(
        event_payloads(&fx.repo, "ask.requested").await.len(),
        0,
        "steered GIVE-UP capstone must never ask the user"
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
