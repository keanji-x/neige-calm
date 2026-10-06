//! #2275 — the scheduler's child-track bootstrap starts the child's Planner under the card's
//! start fence, as every route start does, so a reset of the just-minted child waits for the
//! bootstrap instead of interleaving with it.

use axum::http::StatusCode;
use calm_server::model::now_ms;
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
use tempfile::TempDir;

use crate::planner_first_start::Boot;
use crate::planner_repoint_restart_lock::{joined, wait_until};
use crate::planner_restart::{ordinary_track, wait_for};

const CHILD_GOAL: &str = "child-bootstrap-goal-2275";

/// A dispatched sub-track task on the parent Track, as a plan leaves one after its claim.
async fn seed_child_task(parent: &Boot, track_id: &str) -> String {
    let task_id = format!("{track_id}:child-2275");
    let now = now_ms();
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, \
           priority, status, spawn, created_at_ms, updated_at_ms, access) \
         VALUES (?1, ?2, 'child-2275', 'codex', ?3, 'null', '[]', 0, 'dispatched', ?4, ?5, ?5, \
           'read_write')",
    )
    .bind(&task_id)
    .bind(track_id)
    .bind(CHILD_GOAL)
    .bind(TASK_CHILD_TRACK_ROUTE)
    .bind(now)
    .execute(parent.repo.pool())
    .await
    .unwrap();
    task_id
}

/// The bootstrap is held in `thread/start` while it holds the child card's lock; a reset of that
/// card is shown queued on the lock, and once the bootstrap lands the reset replaces its session.
#[tokio::test]
async fn a_reset_of_a_just_minted_child_waits_for_the_scheduler_bootstrap() {
    let parent = ordinary_track().await;
    let parent_track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id = ?1")
        .bind(&parent.planner_card_id)
        .fetch_one(parent.repo.pool())
        .await
        .unwrap();
    let task_id = seed_child_task(&parent, &parent_track).await;
    let scheduler = parent.state.scheduler_for_test();

    let thread_start = parent
        .state
        .shared_codex_appserver
        .lock_thread_start_serial_for_test()
        .await;
    let sweep = {
        let scheduler = scheduler.clone();
        tokio::spawn(async move { scheduler.sweep_all().await })
    };
    let pool = parent.repo.pool().clone();
    let mut child_card = None;
    wait_for("the scheduler mints the child", async || {
        child_card = sqlx::query_scalar(
            "SELECT c.id FROM tasks t JOIN cards c ON c.track_id = t.child_track_id \
               AND c.role = 'planner' WHERE t.id = ?1",
        )
        .bind(&task_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
        child_card.is_some()
    })
    .await;
    let child = Boot {
        app: parent.app.clone(),
        state: parent.state.clone(),
        repo: parent.repo.clone(),
        planner_card_id: child_card.unwrap(),
        _tmp: TempDir::new().unwrap(),
    };
    wait_for("the scheduler submits the child's bootstrap", async || {
        child.start_ops().await == 1
    })
    .await;
    let held = child
        .state
        .planner_recovery_lock_handles_for_test(&child.planner_card_id);
    assert!(
        held > 0,
        "the scheduler submitted the child's bootstrap without holding the card's lock"
    );

    let reset = {
        let (app, uri) = (child.app.clone(), child.card_uri("planner/reset"));
        tokio::spawn(crate::planner_repoint_restart_lock::request(
            app,
            "POST",
            uri,
            serde_json::Value::Null,
        ))
    };
    wait_until("the reset queues on the child card's lock", || {
        child
            .state
            .planner_recovery_lock_handles_for_test(&child.planner_card_id)
            > held
    })
    .await;
    assert_eq!(
        child.start_ops().await,
        1,
        "the reset waits before starting"
    );
    assert!(!reset.is_finished(), "the reset waits behind the bootstrap");
    assert!(
        !sweep.is_finished(),
        "the bootstrap is still in thread/start"
    );

    drop(thread_start);
    joined(sweep, "the bootstrap finishes").await;
    let (status, body) = joined(reset, "the reset finishes").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        child.start_ops().await,
        2,
        "the bootstrap's start, then the reset's"
    );
    assert_eq!(
        child.active_session_rows().await,
        1,
        "exactly one live session"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id = ?1")
        .bind(&task_id)
        .fetch_one(child.repo.pool())
        .await
        .unwrap();
    assert_eq!(status, "running", "the bootstrap succeeded");
    child.wait_delivered(CHILD_GOAL).await;
    child.shutdown().await;
    parent.shutdown().await;
}
