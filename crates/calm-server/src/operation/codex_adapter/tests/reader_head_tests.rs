//! #1933 — a read-only task may declare `head`: the kernel refuses its launch unless the track
//! checkout is at it, on a fresh start (prepare) and on a `SpawnStarted` re-drive (spawn). The
//! task prompt states the reader's repo, checkout, head and base.
use super::*;
use crate::operation::workspace_lease::worker::{TRACK_HEAD_MISMATCH, TRACK_HEAD_UNKNOWN};

/// A dispatched read-only codex task (`tasks.access`, `tasks.head`, `tasks.base`) for
/// [`try_prepare_worker_and_op`], which keeps the row.
async fn insert_reader(
    harness: &WorkerLeaseHarness,
    key: &str,
    head: Option<&str>,
    base: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms, access, head, base) \
         VALUES (?1, ?2, ?3, 'codex', 'review', 'null', '[]', 'dispatched', 1, 1, 'read_only', ?4, ?5)",
    )
    .bind(format!("{}:{key}", harness.track_id))
    .bind(&harness.track_id)
    .bind(key)
    .bind(head)
    .bind(base)
    .execute(harness.repo.pool())
    .await
    .unwrap();
}

async fn lease_rows(harness: &WorkerLeaseHarness) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases")
        .fetch_one(harness.repo.pool())
        .await
        .unwrap()
}

/// T1: the track checkout is not at the declared head, so the prepare refuses before any row is
/// written, naming both commits. A head the repository lacks is refused with its own word.
#[tokio::test]
async fn codex_reader_at_another_head_is_refused_before_spawn() {
    let harness = worker_lease_harness().await;
    let declared = git_head(&harness.worktree);
    run_git(
        &harness.worktree,
        ["commit", "--allow-empty", "-m", "moved"],
    );
    let actual = git_head(&harness.worktree);
    insert_reader(&harness, "moved", Some(&declared), None).await;
    let message = try_prepare_worker_and_op(&harness, "moved", "moved")
        .await
        .expect_err("a reader at another head is refused")
        .to_string();
    assert!(
        message.contains(&format!("refused: {TRACK_HEAD_MISMATCH}:"))
            && message.contains(&declared)
            && message.contains(&actual),
        "{message}"
    );

    let unknown = "0123456789abcdef0123456789abcdef01234567";
    insert_reader(&harness, "unknown", Some(unknown), None).await;
    let message = try_prepare_worker_and_op(&harness, "unknown", "unknown")
        .await
        .expect_err("a head the repository lacks is refused")
        .to_string();
    assert!(
        message.contains(&format!("refused: {TRACK_HEAD_UNKNOWN}:"))
            && message.contains(unknown)
            && message.contains(&actual),
        "{message}"
    );
    assert_eq!(
        lease_rows(&harness).await,
        0,
        "a refused prepare writes nothing"
    );
}

/// T2: the checkout moves after a prepare that matched; the spawn a `SpawnStarted` re-drive runs
/// (which skips `app_server_interact`) refuses before the provider is asked to start.
#[tokio::test]
async fn codex_reader_spawn_redrive_refuses_a_checkout_that_left_the_head() {
    let harness = worker_lease_harness().await;
    let head = git_head(&harness.worktree);
    insert_reader(&harness, "redrive", Some(&head), None).await;
    let (output, _events, op) = prepare_worker_and_op(&harness, "redrive", "redrive").await;
    run_git(
        &harness.worktree,
        ["commit", "--allow-empty", "-m", "moved"],
    );
    let actual = git_head(&harness.worktree);

    let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let ctx = SpawnCtx::new(
        route_repo,
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let message = match harness.adapter.spawn_side_effect(&output, &op, &ctx).await {
        Ok(_) => panic!("a checkout that left the declared head must not spawn"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains(&format!("refused: {TRACK_HEAD_MISMATCH}:"))
            && message.contains(&head)
            && message.contains(&actual),
        "{message}"
    );
}

/// T5 and T6: a reader's prompt states its repo (the track branch's own upstream remote, #2112:
/// never that of the branch the main checkout is on), checkout, head and base; a writer's prompt
/// and output are unchanged.
#[tokio::test]
async fn codex_reader_prompt_states_repo_checkout_head_and_base() {
    let harness = worker_lease_harness().await;
    let main = harness.repo_root.path();
    let url = "https://github.com/example/neige.git";
    let track_branch = format!("neige/track-{}", harness.track_id);
    run_git(main, ["remote", "add", "origin", url]);
    run_git(
        main,
        ["config", &format!("branch.{track_branch}.remote"), "origin"],
    );
    run_git(
        main,
        [
            "config",
            &format!("branch.{track_branch}.merge"),
            "refs/heads/main",
        ],
    );
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .current_dir(main)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "the main checkout's branch has no upstream"
    );

    let head = git_head(&harness.worktree);
    let base = git_head(main);
    insert_reader(&harness, "reader", Some(&head), Some(&base)).await;
    let (output, _, _) = prepare_worker_and_op(&harness, "reader", "reader").await;
    let prompt = output.output_string("prompt", "test").unwrap();
    for fact in [
        format!("\nrepo: {url}\n"),
        format!("\ncheckout: {}\n", harness.worktree.display()),
        format!("\nhead: {head}\n"),
        format!("\nbase: {base}\n"),
    ] {
        assert!(prompt.contains(&fact), "{fact:?} missing from {prompt}");
    }

    insert_reader(&harness, "headless", None, None).await;
    let (output, _, _) = prepare_worker_and_op(&harness, "headless", "headless").await;
    let prompt = output.output_string("prompt", "test").unwrap();
    assert!(prompt.contains(&format!("\nrepo: {url}\n")), "{prompt}");
    assert!(
        !prompt.contains("\nhead: ") && !prompt.contains("\nbase: "),
        "{prompt}"
    );
    assert!(output.data.get("declared_head").is_none());

    let (output, _) = prepare_worker(&harness, "writer").await;
    let prompt = output.output_string("prompt", "test").unwrap();
    assert_eq!(
        prompt,
        format!(
            "Goal:\ndo writer\n\nTask attempt_id: {}:writer\nEcho this exact attempt_id when \
             reporting completion or failure.",
            harness.track_id
        )
    );
    assert!(output.data.get("declared_head").is_none());
}
