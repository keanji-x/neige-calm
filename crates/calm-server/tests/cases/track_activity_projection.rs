//! The `kernel/track/activity` projector, driven through the production writers of every row
//! it reads and asserted on the payload it writes. The interactive PTY card's `working` needs a
//! real PTY and the renderer registry: those cases live in `terminal_signals.rs`.

use calm_server::db::sqlite::{
    begin_immediate_tx, task_complete_from_worker_tx, task_mark_running_tx,
    task_mark_sub_track_running_tx,
};
use calm_server::db::write_with_event_typed;
use calm_server::event::{EditAuthor, Event, EventScope, TrackUpdatedPayload};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::model::{CardRole, Overlay, now_ms};
use calm_server::session_projection_repo::{AgentProvider, WorkerSessionKind, WorkerSessionState};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_activity::sql::{
    E1_HARNESS_TURN_COMPLETED_SQL, N3_PLANNER_TRANSCRIPT_SQL, SessionRow,
};
use calm_server::track_activity::{
    ActivityPayload, Attention, CardActivity, CardState, NotificationSource, Recompute,
    TrackActivityProjector, WriteOutcome, fold,
};
use calm_truth::validation::OVERLAY_KIND_REGISTRY;
use calm_types::harness::HarnessPhaseTag;
use calm_types::task_recovery::{TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE};
use serde_json::json;

use super::track_activity_fixture::{Fx, fx};

fn card_state(p: &ActivityPayload, card_id: &str) -> Option<CardState> {
    p.cards
        .iter()
        .find(|c| c.card_id == card_id)
        .map(|c| c.state)
}

fn quiet(p: &ActivityPayload) -> bool {
    !p.working && p.attention == Attention::None && p.items.is_empty() && p.cards.is_empty()
}

#[tokio::test]
async fn dispatched_task_is_working_without_session_signal() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Starting,
        Some("th-w"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    let before = f.recompute(&t).await;
    assert!(quiet(&before), "a pending task is not working: {before:?}");

    f.claim(&t, "build", 2_000).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "dispatched ⇒ working: {p:?}");
    assert_eq!(p.attention, Attention::None);
    assert!(
        p.cards.is_empty(),
        "no worker card is stamped while dispatched (F2.22): {p:?}"
    );

    f.mark_running(&t, "build", &worker, 3_000).await;
    let p = f.recompute(&t).await;
    assert!(p.working);
    assert_eq!(
        p.cards,
        vec![CardActivity {
            card_id: worker.clone(),
            state: CardState::Working
        }]
    );
    assert_eq!(p.activity_at_ms, None, "nothing has completed yet");
}

/// A shared-daemon worker rests at `running` + `active` after its task is `done` — the task row decides.
#[tokio::test]
async fn completed_worker_with_stale_active_status_is_not_working() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-w"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.stamp("th-w", 3_500, "active", None).await;
    assert!(f.recompute(&t).await.working);

    f.complete(&t, "build", &worker, 4_000).await;
    // The thread keeps reporting `active` after the report.
    f.stamp("th-w", 4_050, "active", None).await;
    let p = f.recompute(&t).await;
    assert!(
        !p.working,
        "done task ⇒ not working despite `active`: {p:?}"
    );
    assert_eq!(p.attention, Attention::None);
    assert!(p.cards.is_empty());
    assert_eq!(p.activity_at_ms, Some(4_000), "E3 = finished_at_ms");
}

/// `child_track_id` rows in `running` are the CHILD's work.
#[tokio::test]
async fn sub_track_parent_running_with_idle_child_is_not_working() {
    let f = fx().await;
    let parent = f.track("parent").await;
    let child = f.track("child").await;
    let planner = f
        .card(&child, "card-child-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "ws-child-planner",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("th-child"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    f.install_live_harness(&child, &planner, &ws).await;
    f.plan_tasks(&parent, &[("sub", "codex", TASK_CHILD_TRACK_ROUTE, None)])
        .await;
    f.claim(&parent, "sub", 2_000).await;
    sqlx::query("UPDATE tasks SET child_track_id = ?1 WHERE id = ?2")
        .bind(&child)
        .bind(Fx::task_id(&parent, "sub"))
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    let n = task_mark_sub_track_running_tx(&mut tx, &Fx::task_id(&parent, "sub"), 3_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(n, 1);
    let (status, worker, _) = f.task_status(&Fx::task_id(&parent, "sub")).await;
    assert_eq!((status.as_str(), worker), ("running", None));

    let p = f.recompute(&parent).await;
    assert!(
        !p.working,
        "a running sub-track row is not the parent's work: {p:?}"
    );
    assert!(p.cards.is_empty());
    assert_eq!(p.attention, Attention::None);
    // The child itself: idle planner ⇒ not working either.
    assert!(!f.recompute(&child).await.working);
}

#[tokio::test]
async fn sub_track_child_deleted_marks_parent_failed() {
    let f = fx().await;
    let parent = f.track("parent").await;
    let child = f.track("child").await;
    f.plan_tasks(&parent, &[("sub", "codex", TASK_CHILD_TRACK_ROUTE, None)])
        .await;
    f.claim(&parent, "sub", 2_000).await;
    sqlx::query("UPDATE tasks SET child_track_id = ?1 WHERE id = ?2")
        .bind(&child)
        .bind(Fx::task_id(&parent, "sub"))
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    task_mark_sub_track_running_tx(&mut tx, &Fx::task_id(&parent, "sub"), 3_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("DELETE FROM tracks WHERE id = ?1")
        .bind(&child)
        .execute(&f.pool)
        .await
        .unwrap();
    let (_runtime, scheduler) = f.scheduler();
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    let (status, worker, finished) = f.task_status(&Fx::task_id(&parent, "sub")).await;
    assert_eq!((status.as_str(), worker), ("failed", None));

    let p = f.recompute(&parent).await;
    assert!(!p.working);
    assert!(p.cards.is_empty(), "no worker card ⇒ no cards[] entry");
    assert_eq!(p.activity_at_ms, finished, "E3 counts the failure");
}

/// The child is `done`; the parent row carries a gate ⇒ `verifying` is the PARENT's own gate run.
#[tokio::test]
async fn parent_gate_verifying_is_working() {
    let f = fx().await;
    let parent = f.track("parent").await;
    let child = f.track("child").await;
    // No `gate.cwd`: a codex declaration carrying one is not admitted (#1727 S4 `gate_cwd_on_agent_task`).
    let gate = json!({ "steps": [{ "name": "ok", "cmd": "true" }] });
    f.plan_tasks(
        &parent,
        &[("sub", "codex", TASK_CHILD_TRACK_ROUTE, Some(gate))],
    )
    .await;
    f.claim(&parent, "sub", 2_000).await;
    sqlx::query("UPDATE tasks SET child_track_id = ?1 WHERE id = ?2")
        .bind(&child)
        .bind(Fx::task_id(&parent, "sub"))
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    task_mark_sub_track_running_tx(&mut tx, &Fx::task_id(&parent, "sub"), 3_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    f.set_closed(&child, true).await;
    let (_runtime, scheduler) = f.scheduler();
    scheduler
        .reconcile_child_track_for_test(&child)
        .await
        .unwrap();
    let (status, worker, _) = f.task_status(&Fx::task_id(&parent, "sub")).await;
    assert_eq!((status.as_str(), worker), ("verifying", None));
    let child_link: Option<String> =
        sqlx::query_scalar("SELECT child_track_id FROM tasks WHERE id = ?1")
            .bind(Fx::task_id(&parent, "sub"))
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        child_link.as_deref(),
        Some(child.as_str()),
        "the flip keeps child_track_id"
    );

    let p = f.recompute(&parent).await;
    assert!(p.working, "verifying is the parent's gate: {p:?}");
    assert!(p.cards.is_empty());
    assert_eq!(p.attention, Attention::None);
}

/// An interactive card whose session died `failed` (signal-killed / spawn compensation) carries a
/// `failed` card verdict until the card is restarted or deleted.
#[tokio::test]
async fn interactive_card_failed_session_is_failed() {
    let f = fx().await;
    let t = f.track("chat").await;
    let card = f.card(&t, "card-i", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &card,
            "ws-i",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-i"),
            None,
            1_000,
        )
        .await;
    f.exit_session(&ws, WorkerSessionState::Failed, 5_000).await;
    let p = f.recompute(&t).await;
    assert!(!p.working);
    assert_eq!(card_state(&p, &card), Some(CardState::Failed));
}

async fn harness_track(f: &Fx, state: WorkerSessionState) -> (String, String, String) {
    let t = f.track("plan").await;
    let planner = f
        .card(&t, "card-planner", "planner", CardRole::Planner)
        .await;
    let ws = f
        .session(
            &planner,
            "ws-planner",
            WorkerSessionKind::SharedPlanner,
            state,
            Some("th-planner"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    (t, planner, ws)
}

/// A `turn_pending` row with NO live handle (boot sweep before `boot_harnesses`, or a crashed loop).
#[tokio::test]
async fn turn_pending_row_without_live_harness_is_not_working() {
    let f = fx().await;
    let (t, _planner, _ws) = harness_track(&f, WorkerSessionState::TurnPending).await;
    let p = f.recompute(&t).await;
    assert!(!p.working, "{p:?}");
    assert!(p.cards.is_empty());
}

#[tokio::test]
async fn turn_pending_row_with_live_harness_is_working() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::TurnPending).await;
    f.install_live_harness(&t, &planner, &ws).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "{p:?}");
    assert_eq!(card_state(&p, &planner), Some(CardState::Working));
}

#[tokio::test]
async fn planning_track_with_idle_planner_is_not_working() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Idle).await;
    f.install_live_harness(&t, &planner, &ws).await;
    let p = f.recompute(&t).await;
    assert!(quiet(&p), "{p:?}");
}

#[tokio::test]
async fn starting_harness_is_not_working() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Starting).await;
    f.install_live_harness(&t, &planner, &ws).await;
    let p = f.recompute(&t).await;
    assert!(!p.working, "{p:?}");
    assert!(p.cards.is_empty());
}

#[tokio::test]
async fn wedged_harness_is_failed() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::TurnPending).await;
    f.exit_session(&ws, WorkerSessionState::Failed, 9_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(card_state(&p, &planner), Some(CardState::Failed));
}

/// Session minted at `t0 < t1 = done`, later signal-killed ⇒ quiet: the verdict belongs to finished work.
#[tokio::test]
async fn done_task_worker_signal_killed_is_quiet() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "claude", CardRole::Worker).await;
    let ws = f
        .session(
            &worker,
            "ws-w",
            WorkerSessionKind::ClaudeCard,
            WorkerSessionState::Running,
            None,
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "claude", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    f.exit_session(&ws, WorkerSessionState::Failed, 6_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.attention, Attention::None, "{p:?}");
    assert!(p.cards.is_empty());
    assert!(p.items.is_empty());
    assert!(!p.working);
    assert_eq!(p.activity_at_ms, Some(4_000));
}

/// The task is `done@t1`; a restart AFTER that mints S2 (`created_at_ms = t2 > t1`) and the
/// replacement spawn fails ⇒ a `failed` card verdict: new work's failure, not the finished task's exit.
#[tokio::test]
async fn done_task_replacement_session_failure_is_failed() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "claude", CardRole::Worker).await;
    let s1 = f
        .session(
            &worker,
            "ws-s1",
            WorkerSessionKind::ClaudeCard,
            WorkerSessionState::Running,
            None,
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "claude", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    // The restart: S1 is written `exited`, S2 is minted with the current clock and takes over `cards.session_id`.
    f.exit_session(&s1, WorkerSessionState::Exited, 5_000).await;
    let s2 = f
        .session(
            &worker,
            "ws-s2",
            WorkerSessionKind::ClaudeCard,
            WorkerSessionState::Starting,
            None,
            None,
            6_000,
        )
        .await;
    let current: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
        .bind(&worker)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(current.as_deref(), Some(s2.as_str()));
    // Compensation for the failed replacement spawn.
    f.exit_session(&s2, WorkerSessionState::Failed, 7_000).await;

    let p = f.recompute(&t).await;
    assert_eq!(card_state(&p, &worker), Some(CardState::Failed), "{p:?}");
}

/// (2) the empty set: the fold applied to A's session with NO current row for A's card must NOT
/// suppress its `failed` — the exception needs at least one `done` row.
#[tokio::test]
async fn superseded_failed_attempt_session_is_not_actionable() {
    let f = fx().await;
    let t = f.track("w").await;
    let card_a = f.card(&t, "card-a", "codex", CardRole::Worker).await;
    let ws_a = f
        .session(
            &card_a,
            "ws-a",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-a"),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &card_a, 3_000).await;
    f.fail(&t, "build", &card_a, 4_000).await;
    f.exit_session(&ws_a, WorkerSessionState::Failed, 4_500)
        .await;
    let p = f.recompute(&t).await;
    assert_eq!(
        card_state(&p, &card_a),
        Some(CardState::Failed),
        "attempt A failed: {p:?}"
    );
    assert_eq!(p.activity_at_ms, Some(4_000));

    let attempt_b = f.recover(&t, "build", &Fx::task_id(&t, "build")).await;
    let card_b = f.card(&t, "card-b", "codex", CardRole::Worker).await;
    f.session(
        &card_b,
        "ws-b",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-b"),
        None,
        5_000,
    )
    .await;
    f.claim_attempt(&t, "build", &attempt_b, 6_000).await;
    let mut tx = begin_immediate_tx(&f.pool).await.unwrap();
    assert_eq!(
        task_mark_running_tx(&mut tx, &attempt_b, Some(&card_b), 7_000, i64::MAX)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        task_complete_from_worker_tx(
            &mut tx,
            &attempt_b,
            &t,
            calm_server::db::sqlite::TaskReporter::Card {
                card_id: &card_b,
                owns_key: true,
            },
            8_000,
        )
        .await
        .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    // A's row is still `failed` in `tasks`, just not current.
    assert_eq!(f.task_status(&Fx::task_id(&t, "build")).await.0, "failed");

    let p = f.recompute(&t).await;
    assert!(
        p.cards.is_empty(),
        "(1) A's failed session is fenced: {p:?}"
    );
    assert!(!p.working);
    assert_eq!(p.activity_at_ms, Some(8_000));

    // (2) the empty set: hand the fold A's session as if the S0 fence had admitted it.
    let rows = f.projector.read_rows(&t).await.unwrap().unwrap();
    assert!(
        rows.sessions.iter().all(|s| s.id != ws_a),
        "S0 fences A's session out: {:?}",
        rows.sessions
    );
    assert!(
        rows.tasks
            .iter()
            .all(|task| task.worker_card_id.as_deref() != Some(card_a.as_str())),
        "no current row names card A"
    );
    let mut admitted = rows.clone();
    admitted.sessions.push(SessionRow {
        id: ws_a.clone(),
        card_id: card_a.clone(),
        provider: "codex".into(),
        state: "failed".into(),
        updated_at_ms: 4_500,
        created_at_ms: 1_000,
        mode: None,
        task_bound: true,
        terminal_run_id: None,
        pty_open: false,
    });
    let folded = fold(&t, &admitted);
    assert!(
        folded
            .cards
            .iter()
            .any(|c| c.card_id == card_a && c.state == CardState::Failed),
        "(2) an empty current-row set never suppresses a failed session: {folded:?}"
    );
}

/// A worker card with a `failed` current attempt (`neige_task_fail` at
/// `at_ms`) whose session is still `running`. Returns `(card, session)`.
async fn failed_attempt(
    f: &Fx,
    t: &str,
    card: &str,
    ws: &str,
    key: &str,
    at_ms: i64,
) -> (String, String) {
    let worker = f.card(t, card, "codex", CardRole::Worker).await;
    let session = f
        .session(
            &worker,
            ws,
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some(&format!("th-{ws}")),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(t, &[(key, "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(t, key, 2_000).await;
    f.mark_running(t, key, &worker, 3_000).await;
    f.fail(t, key, &worker, at_ms).await;
    (worker, session)
}

/// The failed attempt's card verdict goes; the E3 high-water mark stays.
#[tokio::test]
async fn closed_track_failed_attempt_is_quiet() {
    let f = fx().await;
    let t = f.track("w").await;
    let (worker, ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    f.exit_session(&ws, WorkerSessionState::Failed, 4_500).await;
    let before = f.recompute(&t).await;
    assert_eq!(
        card_state(&before, &worker),
        Some(CardState::Failed),
        "{before:?}"
    );

    f.set_closed(&t, true).await;
    let p = f.recompute(&t).await;
    assert!(
        p.cards.is_empty(),
        "closed ⇒ the failed card verdict goes: {p:?}"
    );
    assert!(!p.working);
    assert_eq!(
        p.activity_at_ms,
        Some(4_000),
        "the mark is a high-water mark: the filter does not lower it"
    );
    assert!(f.stored(&t).await.unwrap().cards.is_empty());
}

/// A reopen through `track_update_tx` brings the failed card verdict back — the filter is a function of the row, not a one-way write.
#[tokio::test]
async fn reopened_track_failed_attempt_is_red_again() {
    let f = fx().await;
    let t = f.track("w").await;
    let (worker, _ws) = failed_attempt(&f, &t, "card-w", "ws-w", "build", 4_000).await;
    f.set_closed(&t, true).await;
    let quiet_now = f.recompute(&t).await;
    assert!(quiet_now.cards.is_empty(), "{quiet_now:?}");

    f.set_closed(&t, false).await;
    let closed_at: Option<i64> = sqlx::query_scalar("SELECT closed_at FROM tracks WHERE id = ?1")
        .bind(&t)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(closed_at, None, "reopening clears closed_at");
    let p = f.recompute(&t).await;
    assert_eq!(
        card_state(&p, &worker),
        Some(CardState::Failed),
        "reopened ⇒ the failed card verdict is back: {p:?}"
    );
}

/// The filter removes the failed verdict, not the card: a card whose `failed` verdict out-ranked its `working` one keeps the working verdict.
#[tokio::test]
async fn closed_track_running_task_is_still_working() {
    let f = fx().await;
    let t = f.track("w").await;
    let failed_worker = f.card(&t, "card-a", "codex", CardRole::Worker).await;
    let running_worker = f.card(&t, "card-b", "codex", CardRole::Worker).await;
    let both_worker = f.card(&t, "card-x", "codex", CardRole::Worker).await;
    for (card, ws, th) in [
        (&failed_worker, "ws-a", "th-a"),
        (&running_worker, "ws-b", "th-b"),
        (&both_worker, "ws-x", "th-x"),
    ] {
        f.session(
            card,
            ws,
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some(th),
            None,
            1_000,
        )
        .await;
    }
    f.plan_tasks(
        &t,
        &[
            ("build", "codex", TASK_IN_TRACK_ROUTE, None),
            ("test", "codex", TASK_IN_TRACK_ROUTE, None),
            ("lint", "codex", TASK_IN_TRACK_ROUTE, None),
            ("pack", "codex", TASK_IN_TRACK_ROUTE, None),
        ],
    )
    .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &failed_worker, 3_000).await;
    f.fail(&t, "build", &failed_worker, 4_000).await;
    f.claim(&t, "test", 5_000).await;
    f.mark_running(&t, "test", &running_worker, 6_000).await;
    // The same-card pair: `lint` failed (X, 4000) and `pack` running (X, 6000).
    f.claim(&t, "lint", 2_000).await;
    f.mark_running(&t, "lint", &both_worker, 3_000).await;
    f.fail(&t, "lint", &both_worker, 4_000).await;
    f.claim(&t, "pack", 5_000).await;
    f.mark_running(&t, "pack", &both_worker, 6_000).await;
    let before = f.recompute(&t).await;
    assert!(before.working, "{before:?}");
    assert_eq!(card_state(&before, &failed_worker), Some(CardState::Failed));
    assert_eq!(
        card_state(&before, &running_worker),
        Some(CardState::Working)
    );
    assert_eq!(
        card_state(&before, &both_worker),
        Some(CardState::Failed),
        "failed > working on a live track: {before:?}"
    );

    f.set_closed(&t, true).await;
    let p = f.recompute(&t).await;
    assert!(
        p.working,
        "a running task on a closed track is not hidden: {p:?}"
    );
    assert_eq!(
        p.cards,
        vec![
            CardActivity {
                card_id: running_worker.clone(),
                state: CardState::Working
            },
            CardActivity {
                card_id: both_worker.clone(),
                state: CardState::Working
            },
        ],
        "every card with working evidence keeps it, the failed-only card goes: {p:?}"
    );
}

/// The session-verdict form of the same-card corner: a closed track, a task running on X, and X's
/// session `failed` out-ranked the working verdict; the filter drops it, `cards == [X = working]`.
#[tokio::test]
async fn closed_track_failed_session_on_a_running_worker_is_still_working() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-x", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &worker,
            "ws-x",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-x"),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.exit_session(&ws, WorkerSessionState::Failed, 4_000).await;
    let before = f.recompute(&t).await;
    assert!(before.working, "{before:?}");
    assert_eq!(
        card_state(&before, &worker),
        Some(CardState::Failed),
        "failed > working on a live track: {before:?}"
    );

    f.set_closed(&t, true).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "{p:?}");
    assert_eq!(
        p.cards,
        vec![CardActivity {
            card_id: worker.clone(),
            state: CardState::Working
        }],
        "the working evidence outlives the filtered failed verdict: {p:?}"
    );
}

/// A worker's later turn end and stop hook do NOT relight a result E3 already lit: neither column is
/// evidence; a card's output needs the real PTY (`terminal_signals::task_bound_worker_output_is_ignored`).
#[tokio::test]
async fn worker_turn_end_after_task_done_does_not_relight() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-w"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    let t1 = now_ms() - 10_000;
    f.complete(&t, "build", &worker, t1).await;
    let t2 = t1 + 5_000;
    f.stamp("th-w", t2, "idle", Some(t2)).await;
    // A stop hook on the worker card, persisted at `now` (> t1).
    f.claude_hook(&t, &worker, ("Stop", "stop"), json!({}))
        .await;
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(t1), "one result, one unread: {p:?}");
}

/// A `failed` turn is an ending; only `interrupted` is excluded.
#[tokio::test]
async fn e1_turn_completed_row_is_the_activity_instant() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Idle).await;
    assert_eq!(f.recompute(&t).await.activity_at_ms, None, "no row yet");

    let t1 = 1_700_000_000_000_i64;
    let done = f
        .turn_outcome(
            &ws,
            &planner,
            &t,
            "turn-1",
            json!({"id": "turn-1", "status": "completed"}),
        )
        .await;
    f.pin_transcript_row(done, t1).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(t1),
        "E1 = the completed row's created_at_ms"
    );

    let interrupted = f
        .turn_outcome(
            &ws,
            &planner,
            &t,
            "turn-2",
            json!({"id": "turn-2", "status": "interrupted"}),
        )
        .await;
    f.pin_transcript_row(interrupted, t1 + 5_000).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(t1),
        "a later interrupted turn is not an ending"
    );

    let failed = f
        .turn_outcome(
            &ws,
            &planner,
            &t,
            "turn-3",
            json!({"id": "turn-3", "status": "failed"}),
        )
        .await;
    f.pin_transcript_row(failed, t1 + 7_000).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(t1 + 7_000),
        "a failed turn is an ending"
    );
}

/// E2 is the `ask.requested` event's `at`, open or not; the user's answer is no activity of the
/// track, and a transcript tool call (here the retired notify tool's) is no evidence.
#[tokio::test]
async fn e2_ask_requested_is_the_activity_instant() {
    let f = fx().await;
    let (t, planner, ws) = harness_track(&f, WorkerSessionState::Idle).await;
    let ask_id = f
        .ask(
            &planner,
            &ws,
            &["Which branch should the release go out from?"],
        )
        .await;
    let asked_at: i64 = sqlx::query_scalar("SELECT at FROM events WHERE id = ?1")
        .bind(ask_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(asked_at),
        "E2 = the ask.requested event's at"
    );

    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    f.answer(&t, ask_id, &["main"]).await;
    let call = f
        .transcript_item(
            &ws,
            &planner,
            &t,
            "call-notify",
            "mcpToolCall",
            "item/completed",
            json!({"item": {"id": "call-notify", "type": "mcpToolCall", "server": "neige",
                            "tool": "neige_user_notify", "status": "completed", // retired-name: rejection input
                            "arguments": {"text": "Ship it?"}}}),
        )
        .await;
    f.pin_transcript_row(call, asked_at + 5_000).await;
    assert_eq!(
        f.recompute(&t).await.activity_at_ms,
        Some(asked_at),
        "neither the answer nor a tool call row is evidence"
    );
}

/// The production E1 and N3 statements enter the transcript table through
/// `idx_transcript_card_method_created_at` — one index range per card, no table scan.
#[tokio::test]
async fn e1_n3_query_plans_use_the_transcript_index() {
    let f = fx().await;
    const INDEX: &str = "USING INDEX idx_transcript_card_method_created_at";
    for (label, sql, index_ranges) in [
        ("E1", E1_HARNESS_TURN_COMPLETED_SQL, 1),
        ("N3", N3_PLANNER_TRANSCRIPT_SQL, 1),
    ] {
        let details: Vec<String> = sqlx::query(&format!("EXPLAIN QUERY PLAN {sql}"))
            .bind("track-1")
            .fetch_all(&f.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| sqlx::Row::get::<String, _>(&row, "detail"))
            .collect();
        assert_eq!(
            details.iter().filter(|d| d.contains(INDEX)).count(),
            index_ranges,
            "{label} must be an index range per card in every arm, got plan {details:?}"
        );
        assert!(
            details
                .iter()
                .filter(|d| d.starts_with("SCAN"))
                .all(|d| d.starts_with("SCAN (subquery-")),
            "{label} must not scan a table, got plan {details:?}"
        );
        assert!(
            !details.iter().any(|d| d.contains("MATERIALIZE")),
            "{label} must not materialize the transcript, got plan {details:?}"
        );
    }
}

/// A close is not activity: no evidence reads `track.updated`, so the mark stays where the work left it.
#[tokio::test]
async fn closing_does_not_advance_activity() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    assert_eq!(f.recompute(&t).await.activity_at_ms, Some(4_000));

    let track_id = TrackId::from(t.clone());
    let area = f.area_id.clone();
    write_with_event_typed::<(), _>(
        f.repo_dyn.as_ref(),
        ActorId::User,
        EventScope::Track {
            track: track_id.clone(),
            area: area.into(),
        },
        None,
        &f.events,
        &f.write,
        move |tx| {
            Box::pin(async move {
                let closed = calm_server::db::sqlite::track_update_tx(
                    tx,
                    track_id.as_str(),
                    calm_server::model::TrackPatch {
                        closed: Some(true),
                        ..Default::default()
                    },
                )
                .await?;
                Ok((
                    (),
                    Event::TrackUpdated(TrackUpdatedPayload::new(closed, None)),
                ))
            })
        },
    )
    .await
    .unwrap();
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(4_000), "{p:?}");
}

#[tokio::test]
async fn quiet_track_short_task_completed_between_ticks_is_unread() {
    let f = fx().await;
    let t = f.track("quiet").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    assert!(
        f.stored(&t).await.is_none(),
        "no overlay before the first tick"
    );

    f.projector.reconcile_all().await;
    let p = f.stored(&t).await.expect("the tick seeds the row");
    assert_eq!(p.activity_at_ms, Some(4_000));
    assert!(!p.working);
}

#[tokio::test]
async fn activity_at_is_monotone() {
    let f = fx().await;
    let t = f.track("w").await;
    let big = 4_000_000_000_000_i64;
    f.seed_activity_overlay(
        &t,
        json!({
            "schemaVersion": 1, "working": true, "attention": "none",
            "activity_at_ms": big, "items": [], "cards": []
        }),
    )
    .await;
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(big), "{p:?}");
    assert!(
        !p.working,
        "the conclusions are recomputed; only the mark is kept"
    );

    // Older evidence arrives: still `big`.
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.complete(&t, "build", &worker, 4_000).await;
    let p = f.recompute(&t).await;
    assert_eq!(p.activity_at_ms, Some(big));
    assert_eq!(f.stored(&t).await.unwrap().activity_at_ms, Some(big));
}

/// A stored payload THIS binary cannot parse — a v1 row with a v1 item, what the release before
/// #1829 left behind — is rewritten as v2 and keeps its high-water mark: the mark is read from the raw
/// JSON, otherwise the upgrade would re-seed it.
#[tokio::test]
async fn high_water_mark_survives_an_unparseable_stored_payload() {
    let f = fx().await;
    let t = f.track("w").await;
    let big = 4_000_000_000_000_i64;
    f.seed_activity_overlay(
        &t,
        json!({
            "schemaVersion": 1, "working": true, "attention": "failed",
            "activity_at_ms": big,
            "items": [{"kind": "failed", "source": "session", "id": "ws-1",
                       "card_id": "card-1", "at_ms": 1_000}],
            "cards": [{"card_id": "card-1", "state": "failed"}]
        }),
    )
    .await;
    assert!(
        serde_json::from_value::<ActivityPayload>(
            f.repo_dyn.overlays_for("track", &t).await.unwrap()[0]
                .payload
                .clone()
        )
        .is_err(),
        "the seeded row must NOT parse, or this test proves nothing"
    );
    let p = match f.projector.recompute_track(&t).await.unwrap() {
        Recompute::Written(p) => p,
        other => panic!("an unparseable row is rewritten in this binary's shape: {other:?}"),
    };
    assert_eq!(p.activity_at_ms, Some(big), "{p:?}");
    let stored = f.stored(&t).await.unwrap();
    assert_eq!(stored.schema_version, 3);
    assert_eq!(stored.activity_at_ms, Some(big));
    assert!(
        !stored.working,
        "the conclusions come from the rows, not the old row"
    );
    assert_eq!(stored.attention, Attention::None);
    assert!(
        stored.items.is_empty() && stored.cards.is_empty(),
        "{stored:?}"
    );
}

/// A change writes exactly one `overlay.set` with the track scope.
#[tokio::test]
async fn unchanged_recompute_emits_no_event() {
    let f = fx().await;
    let t = f.track("w").await;
    let mut rx = f.events.subscribe();
    assert!(matches!(
        f.projector.recompute_track(&t).await.unwrap(),
        Recompute::Written(_)
    ));
    let env = rx.recv().await.unwrap();
    match &env.event {
        Event::OverlaySet(o) => {
            assert_eq!(
                (o.kind.as_str(), o.entity_kind.as_str()),
                ("activity", "track")
            );
            assert_eq!(o.entity_id, t);
            assert_eq!(o.plugin_id, "kernel");
        }
        other => panic!("expected overlay.set, got {other:?}"),
    }
    assert_eq!(env.scope.track_id().map(|x| x.as_str()), Some(t.as_str()));
    assert!(matches!(
        f.projector.recompute_track(&t).await.unwrap(),
        Recompute::Unchanged(_)
    ));
    assert!(
        rx.try_recv().is_err(),
        "an unchanged recompute must not emit"
    );
    // A deleted track: nothing to project.
    assert!(matches!(
        f.projector.recompute_track("no-such-track").await.unwrap(),
        Recompute::NoTrack
    ));
}

/// The reads and the write share no snapshot: a track deleted between them has already lost every
/// overlay row, and the late write must not put an orphan `activity` row back (no FK) nor emit.
#[tokio::test]
async fn deleted_track_is_not_resurrected_by_a_late_write() {
    let f = fx().await;
    let t = f.track("w").await;
    let _worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    // The read half, before the delete.
    let rows = f
        .projector
        .read_rows(&t)
        .await
        .unwrap()
        .expect("the track exists at read time");
    let folded = fold(&t, &rows);
    let late = ActivityPayload {
        schema_version: 1,
        working: folded.working,
        attention: folded.attention(),
        activity_at_ms: None,
        items: folded.items,
        cards: folded.cards,
    };
    assert!(late.working, "the late write would say working: {late:?}");
    // The delete lands: card overlays, track overlays, sessions, tasks, the track row — one transaction.
    f.repo_dyn.track_delete(&t).await.unwrap();
    let mut rx = f.events.subscribe();
    // The write half, with the pre-delete payload.
    assert_eq!(
        f.projector.write_overlay(&t, &late).await.unwrap(),
        WriteOutcome::TrackGone
    );
    let orphans: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM overlays WHERE entity_kind = 'track' AND entity_id = ?1",
    )
    .bind(&t)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(orphans, 0, "no overlay row for a deleted track");
    assert!(rx.try_recv().is_err(), "no overlay.set was broadcast");
    // A full recomputation of the deleted track is a no-op as well.
    assert!(matches!(
        f.projector.recompute_track(&t).await.unwrap(),
        Recompute::NoTrack
    ));
    assert!(rx.try_recv().is_err());
}

/// Every payload the projector writes passes the `activity` entry of the overlay kind registry.
#[tokio::test]
async fn activity_payload_passes_the_overlay_registry() {
    let f = fx().await;
    let t = f.track("w").await;
    let worker = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &worker,
            "ws-w",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-w"),
            None,
            1_000,
        )
        .await;
    f.plan_tasks(
        &t,
        &[
            ("build", "codex", TASK_IN_TRACK_ROUTE, None),
            ("test", "codex", TASK_IN_TRACK_ROUTE, None),
        ],
    )
    .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;
    f.fail(&t, "build", &worker, 4_000).await;
    f.claim(&t, "test", 5_000).await;
    let _ = ws;
    // A never-task-bound interactive card whose session died `failed`: the one `session` verdict.
    let chat = f.card(&t, "card-chat", "codex", CardRole::Worker).await;
    let chat_ws = f
        .session(
            &chat,
            "ws-chat",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-chat"),
            None,
            1_000,
        )
        .await;
    f.exit_session(&chat_ws, WorkerSessionState::Failed, 6_000)
        .await;
    // The Planner card: one `neige_user_ask` ask and a failed turn (planner down).
    let planner = f
        .card(&t, "card-planner", "planner", CardRole::Planner)
        .await;
    let planner_ws = f
        .session(
            &planner,
            "ws-planner",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("th-planner"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    f.ask(&planner, &planner_ws, &["Ship it?"]).await;
    f.turn_outcome(
        &planner_ws,
        &planner,
        &t,
        "turn-failed",
        json!({"id": "turn-failed", "status": "failed", "error": {"message": "boom"}}),
    )
    .await;
    let p = f.recompute(&t).await;
    // Both sources are represented: an ask and planner down; a card folded to `failed`; working from `test`.
    assert!(p.working);
    assert_eq!(p.attention, Attention::Failed);
    let mut sources: Vec<NotificationSource> = p.items.iter().map(|i| i.source()).collect();
    sources.sort();
    assert_eq!(
        sources,
        [NotificationSource::Ask, NotificationSource::PlannerDown],
        "{p:?}"
    );
    assert_eq!(card_state(&p, &worker), Some(CardState::Failed));
    let stored = f
        .repo_dyn
        .overlays_for("track", &t)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.kind == "activity")
        .unwrap();
    OVERLAY_KIND_REGISTRY
        .validate("activity", &stored.payload)
        .expect("the projector's payload is the registry's shape");
    assert_eq!(stored.payload["schemaVersion"], json!(3));
    // Every item key is present, nothing else: only an ask carries its id and questions.
    for item in stored.payload["items"].as_array().unwrap() {
        let mut keys: Vec<&String> = item.as_object().unwrap().keys().collect();
        keys.sort();
        match item["source"].as_str() {
            Some("ask") => assert_eq!(
                keys,
                ["ask_id", "at_ms", "key", "questions", "source", "text"]
            ),
            _ => assert_eq!(keys, ["at_ms", "key", "source", "text"]),
        }
    }
    // Every key is present, nothing else.
    let mut keys: Vec<&String> = stored.payload.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "activity_at_ms",
            "attention",
            "cards",
            "items",
            "schemaVersion",
            "working"
        ]
    );
}

/// The wake-up table, every row, table-driven, including the events that must not wake anything.
#[tokio::test]
async fn wakeup_table_resolves_every_row_of_the_design() {
    let f = fx().await;
    let t = f.track("w").await;
    let card = f.card(&t, "card-w", "codex", CardRole::Worker).await;
    let track = f.repo_dyn.track_get(&t).await.unwrap().unwrap();
    let tid = TrackId::from(t.clone());
    let cid = CardId::from(card.clone());
    let area = AreaId::from(f.area_id.clone());
    let overlay = |plugin_id: &str, entity_kind: &str, entity_id: &str, kind: &str| {
        Event::OverlaySet(Overlay {
            id: "o".into(),
            plugin_id: plugin_id.into(),
            entity_kind: entity_kind.into(),
            entity_id: entity_id.into(),
            kind: kind.into(),
            payload: json!({}),
            updated_at: 0,
        })
    };
    let task_events = [
        (
            "task.dispatched",
            Event::TaskDispatched {
                idempotency_key: format!("{t}:build"),
                kind: "codex".into(),
                agent_message: None,
            },
        ),
        (
            "task.completed",
            Event::TaskCompleted {
                idempotency_key: format!("{t}:build"),
                result: json!({}),
                artifacts: vec![],
                agent_message: None,
            },
        ),
        (
            "task.failed",
            Event::TaskFailed {
                idempotency_key: format!("{t}:build"),
                reason: "fixture".into(),
                details: None,
                agent_message: None,
            },
        ),
        (
            "task.gate_result",
            Event::TaskGateResult {
                task_id: format!("{t}:build"),
                idempotency_key: format!("{t}:build#g1"),
                passed: true,
                failing_step: None,
                exit_code: Some(0),
                log_tail: String::new(),
                log_path: String::new(),
                attempt: 1,
                agent_message: None,
                status_detail: None,
                target: None,
            },
        ),
    ];
    let session_events = [
        (
            "worker_session.started",
            Event::WorkerSessionStarted {
                worker_session_id: "ws-w".into(),
                card_id: card.clone(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
            },
        ),
        (
            "worker_session.status_changed",
            Event::WorkerSessionStatusChanged {
                worker_session_id: "ws-w".into(),
                card_id: card.clone(),
                old_status: WorkerSessionState::Starting,
                new_status: WorkerSessionState::Running,
            },
        ),
        (
            "worker_session.superseded",
            Event::WorkerSessionSuperseded {
                old_worker_session_id: "ws-w".into(),
                new_worker_session_id: "ws-w2".into(),
                card_id: card.clone(),
            },
        ),
    ];

    let mut rows: Vec<(String, EventScope, Event, Option<&str>)> = vec![
        // No `overlay.set` wakes the projector: the projector's own row must not wake it, and a
        // plugin's row never did.
        (
            "overlay.set kernel/track/activity — the projector's own row".into(),
            f.track_scope(&t),
            overlay("kernel", "track", &t, "activity"),
            None,
        ),
        (
            "overlay.set of a kernel card row, track scope".into(),
            f.track_scope(&t),
            overlay("kernel", "card", &card, "eta"),
            None,
        ),
        (
            "overlay.set of a plugin, card/eta".into(),
            f.track_scope(&t),
            overlay("plugin-x", "card", &card, "eta"),
            None,
        ),
        (
            "harness.phase.changed".into(),
            EventScope::System,
            Event::HarnessPhaseChanged {
                worker_session_id: "ws-w".into(),
                card_id: cid.clone(),
                track_id: tid.clone(),
                old_phase: HarnessPhaseTag::TurnRunning,
                new_phase: HarnessPhaseTag::Idle,
            },
            Some(t.as_str()),
        ),
        // #2209: no tool call is notification evidence any more.
        (
            "harness.item.added mcpToolCall item/completed".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/completed", Some("mcpToolCall")),
            None,
        ),
        (
            "harness.item.added mcpToolCall item/started (not a completion)".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/started", Some("mcpToolCall")),
            None,
        ),
        (
            "harness.item.added agentMessage item/completed".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/completed", Some("agentMessage")),
            None,
        ),
        (
            "harness.item.added without an item type".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "item/completed", None),
            None,
        ),
        // A codex system error persists its failed turn row after the phase event; this is its one event.
        (
            "harness.item.added turn/completed (no item type)".into(),
            EventScope::System,
            Fx::item_added(&t, &card, "turn/completed", None),
            Some(t.as_str()),
        ),
        (
            "harness.user_message.enqueued".into(),
            EventScope::System,
            Event::HarnessUserMessageEnqueued {
                worker_session_id: "ws-w".into(),
                card_id: cid.clone(),
                track_id: tid.clone(),
                char_count: 5,
            },
            Some(t.as_str()),
        ),
        // #2209: historical rows; nothing reads them for a notification.
        (
            "ratify.requested".into(),
            EventScope::System,
            Event::RatifyRequested {
                track_id: tid.clone(),
                reason: "merge?".into(),
            },
            None,
        ),
        (
            "ratify.resolved".into(),
            EventScope::System,
            Event::RatifyResolved {
                track_id: tid.clone(),
                decision: calm_types::event::RatifyDecision::Grant,
                message: None,
            },
            None,
        ),
        (
            "ask.requested".into(),
            EventScope::System,
            Event::AskRequested {
                track_id: tid.clone(),
                questions: vec![calm_server::event::AskQuestion {
                    title: "Merge?".into(),
                    options: Vec::new(),
                }],
                source_item_id: None,
            },
            Some(t.as_str()),
        ),
        (
            "ask.answered".into(),
            EventScope::System,
            Event::AskAnswered {
                ask_id: 1,
                track_id: tid.clone(),
                answers: vec!["yes".into()],
            },
            Some(t.as_str()),
        ),
        (
            "track.report_edited".into(),
            EventScope::System,
            Event::TrackReportEdited {
                track_id: tid.clone(),
                card_id: cid.clone(),
                author: EditAuthor::Planner,
                author_plugin_id: None,
                edit_id: "e1".into(),
                summary_before: String::new(),
                summary_after: String::new(),
                body_before: String::new(),
                body_after: String::new(),
                agent_message: None,
            },
            Some(t.as_str()),
        ),
        (
            "track.updated".into(),
            EventScope::System,
            Event::TrackUpdated(TrackUpdatedPayload::new(track, None)),
            Some(t.as_str()),
        ),
        (
            "track.deleted".into(),
            f.track_scope(&t),
            Event::TrackDeleted {
                id: tid.clone(),
                area_id: area.clone(),
            },
            None,
        ),
    ];
    for (label, event) in session_events {
        rows.push((
            label.into(),
            EventScope::System,
            event.clone(),
            Some(t.as_str()),
        ));
        let Event::WorkerSessionStarted { .. } = &event else {
            continue;
        };
        rows.push((
            format!("{label} of a card no row knows"),
            EventScope::System,
            Event::WorkerSessionStarted {
                worker_session_id: "ws-x".into(),
                card_id: "no-such-card".into(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
            },
            None,
        ));
    }
    for (label, event) in task_events {
        rows.push((
            format!("{label}, track scope"),
            f.track_scope(&t),
            event.clone(),
            Some(t.as_str()),
        ));
        rows.push((
            format!("{label}, System scope (no track to wake)"),
            EventScope::System,
            event,
            None,
        ));
    }
    assert!(
        rows.len() >= 25,
        "every §4.3 row plus its negatives: {}",
        rows.len()
    );
    for (label, scope, event, expected) in rows {
        let env = Fx::envelope(scope, event);
        assert_eq!(
            f.projector.track_for_event(&env).await.as_deref(),
            expected,
            "{label}"
        );
    }
}

/// The overlay flips to `working` within a bounded wait far inside the 30 s tick, so only the event path can have done it.
#[tokio::test]
async fn projector_loop_recomputes_on_task_dispatched() {
    let f = fx().await;
    let t = f.track("w").await;
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    let looped = TrackActivityProjector::new(
        f.repo_dyn.clone(),
        f.events.clone(),
        f.write.clone(),
        f.harness.clone(),
        TerminalRendererRegistry::new(),
    )
    .expect("sqlite-backed repo");
    let loop_task = tokio::spawn(looped.run());
    // The boot sweep (the interval's first tick completes immediately) seeds a quiet row.
    let seeded = f.await_stored(&t, "the boot sweep's row", |_| true).await;
    assert!(quiet(&seeded), "{seeded:?}");

    // A silent claim (the fixture's claim appends no event), then the wake-up the scheduler's claim would have carried.
    f.claim(&t, "build", 2_000).await;
    assert!(
        !f.stored(&t).await.unwrap().working,
        "nothing woke the loop yet"
    );
    f.events.emit_envelope_for_test(Fx::envelope(
        f.track_scope(&t),
        Event::TaskDispatched {
            idempotency_key: format!("{t}:build"),
            kind: "codex".into(),
            agent_message: None,
        },
    ));
    let p = f
        .await_stored(&t, "working after task.dispatched", |p| p.working)
        .await;
    assert_eq!(p.attention, Attention::None);
    loop_task.abort();
}
