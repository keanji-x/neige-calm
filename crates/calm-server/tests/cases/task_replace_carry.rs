//! #1785 S2: the carry a `calm.task.replace` successor's lease starts from (design §4.5) — real
//! git repositories, the production base resolution through the attempt lease seam, and one
//! dispatch through the real Codex worker adapter — plus the dispatch-time route recheck (§4.4).
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::git_delivery::*;
use super::task_replace::*;
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use crate::task_recovery::current;
use calm_server::dispatcher::task_event_pushes_planner_for_test;
use calm_server::model::{Task, TaskStatus};
use calm_server::operation::ProviderAdapter;
use calm_server::operation::codex_adapter::CodexWorkerAdapter;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::CodexClient;
use calm_server::test_seams::take_kernel_workspace_lease_for_attempt_for_test;
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
use serde_json::{Value, json};

/// `git show <rev>:<path>`, `None` when the path is not in that tree.
fn file_at(repo: &Path, rev: &str, path: &str) -> Option<String> {
    let output = git_output(repo, &["show", &format!("{rev}:{path}")]);
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

/// The parents of `commit`, in order.
fn parents(repo: &Path, commit: &str) -> Vec<String> {
    git(repo, &["rev-list", "--parents", "-n", "1", commit])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

async fn lease_base_columns(fx: &Fx, lease_id: &str) -> (String, String, Option<String>) {
    sqlx::query_as(
        "SELECT base_sha, base_source, base_attempt_id FROM workspace_leases WHERE lease_id = ?1",
    )
    .bind(lease_id)
    .fetch_one(&fx.pool())
    .await
    .unwrap()
}

/// A settled producer, replaced by default: its successor task.
async fn replaced(
    fx: &Fx,
    key: &str,
    files: &[(&str, &str)],
) -> (Task, CandidateRowView, Value, Task) {
    let (task, candidate) = produced(fx, key, files, json!({})).await;
    let response = replace(fx, replace_args(&task, &format!("{key}-r")))
        .await
        .unwrap();
    let successor = current(&fx.boot, response["successor"]["key"].as_str().unwrap()).await;
    (task, candidate, response, successor)
}

#[tokio::test]
async fn carry_merges_the_candidate_onto_the_upstream() {
    let fx = replace_fixture().await;
    let (task, _, response, successor) = replaced(&fx, "feat", &[("a.txt", "A\n")]).await;
    let upstream = commit_file(&fx.track_root, "x.txt", "X\n", "upstream moves on");

    let (worker, lease) = attempt_lease(&fx, "feat-2", &successor.id).await;

    let carry = lease.base_sha.clone();
    assert_eq!(
        parents(&fx.track_root, &carry),
        vec![upstream.clone()],
        "C'^1 is U, the only parent"
    );
    assert_eq!(
        file_at(&fx.track_root, &carry, "a.txt").as_deref(),
        Some("A\n")
    );
    assert_eq!(
        file_at(&fx.track_root, &carry, "x.txt").as_deref(),
        Some("X\n")
    );
    assert_eq!(
        std::fs::read_to_string(lease.path.join("a.txt")).unwrap(),
        "A\n"
    );
    let (base_sha, source, attempt) = lease_base_columns(&fx, &lease.lease_id).await;
    assert_eq!(
        (base_sha.as_str(), source.as_str()),
        (carry.as_str(), "attempt")
    );
    assert_eq!(attempt.as_deref(), Some(task.id.as_str()));
    fx.claim_running(&successor.id, &worker.card_id).await;
    let entry = fx.plan_entry("feat.2").await;
    assert_eq!(
        entry["candidate"]["carry"],
        json!({"receipt_id": response["receipt_id"], "carry_sha": carry, "upstream_sha": upstream})
    );
    let summary = fx.plan_summary_entry("feat.2").await;
    assert_eq!(summary["candidate"]["carry"], entry["candidate"]["carry"]);
}

/// `U0 → C'1(+A) → cand1(+B)`, then the upstream moves to `U1(+X)`: the second replacement's
/// carry holds A, B and X.
#[tokio::test]
async fn chained_replacements_keep_every_carried_change() {
    let fx = replace_fixture().await;
    let (_, _, _, second) = replaced(&fx, "chain", &[("a.txt", "A\n")]).await;
    let (_, _) = produce_successor(&fx, &second, &[("b.txt", "B\n")]).await;
    let upstream = commit_file(&fx.track_root, "x.txt", "X\n", "upstream moves on");
    let second = current(&fx.boot, "chain.2").await;
    replace(&fx, replace_args(&second, "chain-r2"))
        .await
        .unwrap();
    let third = current(&fx.boot, "chain.3").await;

    let (_, lease) = attempt_lease(&fx, "chain-3", &third.id).await;

    for (file, content) in [("a.txt", "A\n"), ("b.txt", "B\n"), ("x.txt", "X\n")] {
        assert_eq!(
            file_at(&fx.track_root, &lease.base_sha, file).as_deref(),
            Some(content),
            "{file}"
        );
    }
    assert_eq!(parents(&fx.track_root, &lease.base_sha), vec![upstream]);
}

/// The same chain with the upstream never moving (`U == U0`): A and B both survive.
#[tokio::test]
async fn chained_replacement_on_an_unmoved_upstream_keeps_both_rounds() {
    let fx = replace_fixture().await;
    let head = git(&fx.track_root, &["rev-parse", "HEAD"]);
    let (_, _, _, second) = replaced(&fx, "still", &[("a.txt", "A\n")]).await;
    let (_, _) = produce_successor(&fx, &second, &[("b.txt", "B\n")]).await;
    let second = current(&fx.boot, "still.2").await;
    replace(&fx, replace_args(&second, "still-r2"))
        .await
        .unwrap();
    let third = current(&fx.boot, "still.3").await;

    let (_, lease) = attempt_lease(&fx, "still-3", &third.id).await;

    assert_eq!(
        file_at(&fx.track_root, &lease.base_sha, "a.txt").as_deref(),
        Some("A\n")
    );
    assert_eq!(
        file_at(&fx.track_root, &lease.base_sha, "b.txt").as_deref(),
        Some("B\n")
    );
    assert_eq!(parents(&fx.track_root, &lease.base_sha), vec![head]);
}

/// A gate-red predecessor (`failed`) still has its candidate; the successor carries it.
#[tokio::test]
async fn gate_red_predecessor_carries_its_candidate() {
    let fx = replace_fixture().await;
    let gate = json!({"gate": {"steps": [{"name": "t", "cmd": "exit 1"}], "timeout_secs": 60}, "no_gate_reason": null});
    let (task, candidate) = produced(&fx, "red", &[("a.txt", "A\n")], gate).await;
    let task = wait_status(&fx, &task.key, TaskStatus::Failed).await;
    assert_eq!(task.status_detail.as_deref(), Some("gate-red"));

    let response = replace(&fx, replace_args(&task, "red-r")).await.unwrap();
    let (_, lease) = attempt_lease(&fx, "red-2", &current(&fx.boot, "red.2").await.id).await;

    assert_eq!(
        response["carry"]["source_candidate_id"],
        candidate.candidate_id.as_str()
    );
    assert_eq!(
        file_at(&fx.track_root, &lease.base_sha, "a.txt").as_deref(),
        Some("A\n")
    );
}

/// #1772's main path: `calm.task.verdict rejected` leaves the producer row `done`; the successor
/// carries its candidate.
#[tokio::test]
async fn rejected_done_predecessor_carries_its_candidate() {
    let fx = replace_fixture().await;
    let (task, candidate) = produced(&fx, "rejected", &[("a.txt", "A\n")], json!({})).await;
    call_tool(
        &fx.boot,
        "calm.task.verdict",
        planner_identity(&fx.boot),
        json!({"idempotency_key": task.id, "status": "rejected", "message": "blockers", "reason": "blockers"}),
    )
    .await
    .unwrap();
    assert_eq!(current(&fx.boot, "rejected").await.status, TaskStatus::Done);

    let response = replace(&fx, replace_args(&task, "rej-r")).await.unwrap();
    let (_, lease) =
        attempt_lease(&fx, "rejected-2", &current(&fx.boot, "rejected.2").await.id).await;

    assert_eq!(
        response["carry"]["candidate_sha"],
        candidate.commit_sha.as_str()
    );
    assert_eq!(
        file_at(&fx.track_root, &lease.base_sha, "a.txt").as_deref(),
        Some("A\n")
    );
}

/// The carried commit is gone from the repository: `merge-tree` exits 1 printing nothing.
#[tokio::test]
async fn missing_carry_source_fails_as_carry_infra() {
    let fx = replace_fixture().await;
    let (_, candidate, _, successor) = replaced(&fx, "gone", &[("a.txt", "A\n")]).await;
    let objects = git(&fx.track_root, &["rev-parse", "--git-path", "objects"]);
    let objects = fx.track_root.join(objects);
    let loose = objects
        .join(&candidate.commit_sha[..2])
        .join(&candidate.commit_sha[2..]);
    std::fs::remove_file(&loose).unwrap();

    let worker = fx
        .new_worker(
            "gone-2",
            calm_server::session_projection_repo::AgentProvider::Codex,
        )
        .await;
    let error = take_kernel_workspace_lease_for_attempt_for_test(
        &fx.pool(),
        fx.track(),
        &worker.card_id,
        &fx.workspace_root,
        &successor.id,
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(error.contains("carry-infra"), "{error}");
    assert_eq!(
        fx.table_count("workspace_leases").await,
        1,
        "no lease for the successor"
    );
}

/// Two prepares of one receipt over one upstream compute the same carry commit.
#[tokio::test]
async fn carry_is_deterministic_for_one_receipt() {
    let fx = replace_fixture().await;
    let (_, _, _, successor) = replaced(&fx, "same", &[("a.txt", "A\n")]).await;

    let (_, first) = attempt_lease(&fx, "same-a", &successor.id).await;
    let (_, second) = attempt_lease(&fx, "same-b", &successor.id).await;

    assert_eq!(first.base_sha, second.base_sha);
}

/// `carry: "none"`: the successor starts from the upstream alone.
#[tokio::test]
async fn carry_none_starts_from_the_upstream() {
    let fx = replace_fixture().await;
    let (task, _) = produced(&fx, "fresh", &[("a.txt", "A\n")], json!({})).await;
    let mut args = replace_args(&task, "fresh-r");
    args["carry"] = json!("none");
    replace(&fx, args).await.unwrap();

    let (_, lease) = attempt_lease(&fx, "fresh-2", &current(&fx.boot, "fresh.2").await.id).await;

    assert_eq!(lease.base_sha, git(&fx.track_root, &["rev-parse", "HEAD"]));
    let (_, source, attempt) = lease_base_columns(&fx, &lease.lease_id).await;
    assert_eq!((source.as_str(), attempt), ("head", None));
    assert!(file_at(&fx.track_root, &lease.base_sha, "a.txt").is_none());
}

/// Wait for `key` to fail; return its row.
async fn wait_failed(fx: &Fx, key: &str) -> Task {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let task = current(&fx.boot, key).await;
            if task.status == TaskStatus::Failed {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{key} never failed: {:?}", fx.debug_state()))
}

/// The kernel `task.failed` of `attempt` wakes the Planner.
async fn assert_failure_pushes(fx: &Fx, attempt: &str) {
    let failed = fx.events_for(TASK_FAILED_KIND, attempt).await;
    assert_eq!(failed.len(), 1, "one kernel task.failed");
    assert!(
        task_event_pushes_planner_for_test(
            fx.boot.repo.as_ref(),
            &fx.boot.ctx.write,
            &failed[0].event,
            &failed[0].actor
        )
        .await
    );
}

/// A conflict fails the successor's worker prepare as `spawn-failed: refused: carry-conflict:
/// <paths>` through the real Codex worker adapter, and wakes the Planner; the failed execution can
/// be replaced again.
#[tokio::test]
async fn carry_conflict_fails_the_successor_spawn_and_wakes_the_planner() {
    let fx = fixture_on_with_adapters(
        boot().await,
        |tmp| {
            let repo = tmp.join("repo");
            init_repo(&repo);
            repo
        },
        |boot| {
            let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
            vec![Arc::new(CodexWorkerAdapter::new(
                route_repo,
                Arc::new(CodexClient::new_stub()),
                SharedCodexAppServer::new_stub(boot.repo.clone()),
                None,
                boot.card_role_cache.clone(),
                calm_server::track_area_cache::TrackAreaCache::new(),
                std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
            )) as Arc<dyn ProviderAdapter>]
        },
    )
    .await;
    prepare_replace(&fx).await;
    let (task, _, _, _) = replaced(&fx, "clash", &[("README.md", "worker\n")]).await;
    commit_file(
        &fx.track_root,
        "README.md",
        "upstream\n",
        "upstream edits the same line",
    );

    open_claims(&fx).await;
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    let successor = wait_failed(&fx, "clash.2").await;

    let detail = successor.status_detail.clone().unwrap_or_default();
    assert!(
        detail.starts_with("spawn-failed: refused: carry-conflict: README.md"),
        "{detail}"
    );
    assert_failure_pushes(&fx, &successor.id).await;
    hold_claims(&fx).await;
    let again = replace(&fx, replace_args(&successor, "clash-r2"))
        .await
        .unwrap();
    assert_eq!(again["successor"]["key"], "clash.3");
    assert_eq!(
        again["carry"]["source_attempt_id"], task.id,
        "the receipt's source is kept"
    );
}

/// Edit the successor block at `pointer`, as the Planner's ordinary declaration edit.
async fn edit_block(fx: &Fx, key: &str, pointer: &str, value: Value) {
    let block = block_of(fx, key).await;
    let mut payload = block.payload;
    match payload.pointer_mut(pointer) {
        Some(slot) => *slot = value,
        None => {
            let field = pointer.trim_start_matches('/');
            payload[field] = value;
        }
    }
    call_tool(
        &fx.boot,
        "calm.report.blocks.upsert",
        planner_identity(&fx.boot),
        json!({"id": block.id, "kind": "task", "payload": payload, "if_rev": block.rev}),
    )
    .await
    .unwrap();
}

async fn track_count(fx: &Fx) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(&fx.pool())
        .await
        .unwrap()
}

/// §4.7 last kernel row: the successor edited off the route fails its first dispatch as
/// `replace-route-changed`, and no route it names now runs.
async fn assert_route_changed_on_first_dispatch(fx: &Fx, key: &str) {
    let tracks_before = track_count(fx).await;
    open_claims(fx).await;
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    let successor = wait_failed(fx, key).await;
    let detail = successor.status_detail.clone().unwrap_or_default();
    assert!(
        detail.starts_with("spawn-failed: refused: replace-route-changed"),
        "{detail}"
    );
    assert!(successor.worker_card_id.is_none());
    assert_eq!(track_count(fx).await, tracks_before, "no child Track");
    assert_failure_pushes(fx, &successor.id).await;
}

#[tokio::test]
async fn successor_edited_to_a_child_track_route_fails_its_first_dispatch() {
    let fx = replace_fixture().await;
    replaced(&fx, "sub", &[("a.txt", "A\n")]).await;
    edit_block(&fx, "sub.2", "/spawn", json!(TASK_CHILD_TRACK_ROUTE)).await;
    assert_eq!(
        current(&fx.boot, "sub.2").await.spawn,
        TASK_CHILD_TRACK_ROUTE
    );

    assert_route_changed_on_first_dispatch(&fx, "sub.2").await;
}

#[tokio::test]
async fn successor_edited_to_an_isolated_selector_fails_its_first_dispatch() {
    let fx = replace_fixture().await;
    replaced(&fx, "iso", &[("a.txt", "A\n")]).await;
    let selector =
        json!({"neige_execution": {"version": "isolated-codex-v1", "workspace": "empty"}});
    edit_block(&fx, "iso.2", "/context", selector.clone()).await;
    let edited: Value =
        serde_json::from_str(&current(&fx.boot, "iso.2").await.context_json).unwrap();
    assert_eq!(edited, selector);

    assert_route_changed_on_first_dispatch(&fx, "iso.2").await;
}

/// A repository-selected merge driver runs inside the carry's `git merge-tree`; it sees only the
/// allowlisted environment, never a variable of the server's own.
#[tokio::test]
async fn carry_merge_driver_sees_only_the_allowlisted_environment() {
    // SAFETY: nextest runs each test in its own process; nothing else reads the environment here.
    unsafe { std::env::set_var("NEIGE_CARRY_ENV_SENTINEL", "server-secret") };
    let fx = replace_fixture().await;
    let probe = fx.track_root.parent().unwrap().join("driver-env.txt");
    git(
        &fx.track_root,
        &[
            "config",
            "merge.probe.driver",
            &format!("env > '{}'; cp %B %A; exit 0", probe.display()),
        ],
    );
    let info = fx.track_root.join(".git/info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(info.join("attributes"), "probe.txt merge=probe\n").unwrap();
    commit_file(&fx.track_root, "probe.txt", "base\n", "probe file");
    let (_, _, _, successor) = replaced(&fx, "driven", &[("probe.txt", "worker\n")]).await;
    commit_file(
        &fx.track_root,
        "probe.txt",
        "upstream\n",
        "upstream edits the probe",
    );

    let (_, lease) = attempt_lease(&fx, "driven-2", &successor.id).await;

    let seen = std::fs::read_to_string(&probe).expect("the merge driver ran");
    assert!(seen.contains("PATH="), "{seen}");
    assert!(!seen.contains("NEIGE_CARRY_ENV_SENTINEL"), "leaked: {seen}");
    assert_eq!(
        file_at(&fx.track_root, &lease.base_sha, "probe.txt").as_deref(),
        Some("worker\n")
    );
}
