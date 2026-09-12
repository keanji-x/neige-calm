//! Actual MCP and PTY paths with task/session metadata built by production helpers.
use crate::terminal_support::{Harness, assert_text_observation};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    card_with_claude_create_tx, card_with_claude_worker_create_tx, card_with_codex_create_tx,
    card_with_terminal_create_tx,
};
use calm_server::model::{CardRole, NewTrack, new_id, now_ms};
use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use calm_server::terminal_renderer::RendererConfig;
use serde_json::{Value, json};

struct Worker {
    task: String,
    card: String,
    session: String,
    terminal: String,
}
const ECHO_WORKER: &str = "printf 'WORKER_READY\\n'; while IFS= read -r line; do printf 'WORKER_REPLY:%s\\n' \"$line\"; done";
async fn worker(h: &Harness, kind: &str, track: &str, viewer: bool) -> Worker {
    worker_running(h, kind, track, viewer.then_some(ECHO_WORKER)).await
}
/// `viewer` is the shell script of the worker's PTY viewer, spawned before
/// the task row is stamped (as the scheduler does); `None` leaves the task
/// without a live view.
async fn worker_running(h: &Harness, kind: &str, track: &str, viewer: Option<&str>) -> Worker {
    let key = new_id();
    let task = format!("{track}:{key}");
    let card = new_id();
    let session = new_id();
    let op = SqlxOperationRepo::new(h.sql.pool().clone())
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.clone()),
                payload_hash: key.clone(),
            },
            json!({"track_id":track,"cmd":"true","idempotency_key":task}),
        )
        .await
        .unwrap();
    // This fixture records an already-finished spawn operation; it never drives
    // a provider adapter or starts a real model.
    sqlx::query("UPDATE operations SET phase='succeeded' WHERE id=?1")
        .bind(&op)
        .execute(h.sql.pool())
        .await
        .unwrap();
    let mut tx = h.sql.pool().begin().await.unwrap();
    let roles = CardRoleCache::new();
    let theme = calm_server::routes::theme::RequestTheme::default_dark();
    let cwd = h.root.path().to_str().unwrap().to_owned();
    let (_, terminal) = match kind {
        "codex" => {
            let (c, t, _) = card_with_codex_create_tx(
                &mut tx,
                card.clone(),
                &session,
                Some(&op),
                track.into(),
                None,
                None,
                cwd.clone(),
                json!({}),
                None,
                None,
                None,
                CardRole::Worker,
                true,
                &roles,
                theme,
            )
            .await
            .unwrap();
            (c, t)
        }
        "claude" => card_with_claude_worker_create_tx(
            &mut tx,
            card.clone(),
            &session,
            Some(&op),
            track.into(),
            None,
            None,
            "/bin/sh".into(),
            cwd.clone(),
            json!({}),
            None,
            None,
            None,
            "unused-settings".into(),
            new_id(),
            &roles,
            theme,
        )
        .await
        .unwrap(),
        "terminal" => card_with_terminal_create_tx(
            &mut tx,
            card.clone(),
            &session,
            Some(&op),
            track.into(),
            None,
            None,
            "/bin/sh".into(),
            cwd.clone(),
            json!({}),
            CardRole::Worker,
            true,
            &roles,
            theme,
        )
        .await
        .unwrap(),
        _ => panic!("unsupported fixture kind"),
    };
    tx.commit().await.unwrap();
    h.sql
        .session_projection_set_status_for_card(
            &card,
            calm_server::session_projection_repo::WorkerSessionState::Running,
        )
        .await
        .unwrap();
    if let Some(script) = viewer {
        spawn_viewer_running(h, &terminal.id, script).await;
    }
    // Stamp the task/worker association after its viewer exists, as the scheduler does.
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,declared_by,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,'test','[]','running',?5,'user',?6,?6)")
        .bind(&task).bind(track).bind(key).bind(kind).bind(&card).bind(now_ms()).execute(h.sql.pool()).await.unwrap();
    Worker {
        task,
        card,
        session,
        terminal: terminal.id,
    }
}
async fn spawn_viewer(h: &Harness, terminal: &str) {
    spawn_viewer_running(h, terminal, ECHO_WORKER).await;
}
async fn spawn_viewer_running(h: &Harness, terminal: &str, script: &str) {
    let mut config = RendererConfig {
        terminal_id: terminal.to_owned(),
        cols: 80,
        rows: 24,
        buffer_bytes: 8192,
        terminal_fg: (220, 220, 220),
        terminal_bg: (15, 20, 24),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        envs: vec![],
        cwd: h.root.path().to_str().unwrap().to_owned(),
        supervisor_sock: std::path::PathBuf::new(),
    };
    config.supervisor_sock = h.supervisor_socket();
    h.state.terminal_renderer.ensure(config).await.unwrap();
}
async fn snapshot(h: &Harness, target: Value) -> Value {
    let mut args = target;
    args["wait_ms"] = json!(50);
    h.ok("calm.terminal.observe", args).await
}
async fn stop(h: &Harness, w: &Worker) {
    h.state.terminal_renderer.drop_entry(&w.terminal).await;
}

#[tokio::test]
async fn each_task_kind_resolves_observes_and_inputs_its_own_terminal() {
    let h = Harness::start().await;
    for kind in ["terminal", "codex", "claude"] {
        let w = worker(&h, kind, &h.track, true).await;
        let resolved = h
            .ok("calm.terminal.resolve", json!({"task_id":w.task}))
            .await;
        assert_eq!(resolved["available"], true);
        assert_eq!(resolved["card_kind"], kind);
        assert_eq!(resolved["terminal_id"], w.terminal);
        assert_eq!(resolved["worker_session_id"], w.session);
        let viewed = h
            .call(
                "calm.terminal.observe",
                json!({"task_id":w.task,"wait_ms":50,"format":"image"}),
            )
            .await;
        assert!(viewed.get("error").is_none(), "{viewed}");
        assert!(
            viewed["result"]["content"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "image")
        );
        let meta = &viewed["result"]["structuredContent"];
        assert_eq!(meta["task"]["task_id"], w.task);
        assert_eq!(meta["card_id"], w.card);
        for target in [
            json!({"task_id":w.task}),
            json!({"terminal_id":w.terminal,"format":"text"}),
        ] {
            let text = h.call("calm.terminal.observe", target).await;
            let text_meta = assert_text_observation(&text);
            assert_eq!(text_meta["task"]["task_id"], w.task);
            assert_eq!(
                text_meta["terminal_session_id"],
                meta["terminal_session_id"]
            );
        }
        let claimed = h
            .ok(
                "calm.terminal.control",
                json!({"task_id":w.task,"action":"claim","observe":true,"wait_ms":50}),
            )
            .await;
        assert_eq!(claimed["observation"]["status"], "available");
        assert_eq!(claimed["observation"]["state"]["task"]["task_id"], w.task);
        let before = snapshot(&h, json!({"task_id":w.task})).await;
        let typed=h.ok("calm.terminal.input",json!({"task_id":w.task,"observation_id":before["observation_id"],"request_id":"text","action":{"type":"text","text":"hello"}})).await;
        assert_eq!(typed["outcome"], "written");
        let before = snapshot(&h, json!({"terminal_id":w.terminal})).await;
        let entered=h.ok("calm.terminal.input",json!({"task_id":w.task,"observation_id":before["observation_id"],"request_id":"enter","action":{"type":"key","key":"Enter"}})).await;
        assert_eq!(entered["outcome"], "written");
        let after = h.observe_text(&w.terminal, "WORKER_REPLY:hello").await;
        assert_eq!(before["terminal_session_id"], after["terminal_session_id"]);
        stop(&h, &w).await;
    }
}

#[tokio::test]
async fn foreign_track_task_and_terminal_are_both_refused() {
    let h = Harness::start().await;
    let own = h.sql.track_get(&h.track).await.unwrap().unwrap();
    let foreign = h
        .sql
        .track_create(NewTrack {
            template_input: None,
            area_id: own.area_id,
            title: "foreign".into(),
            sort: None,
            cwd: h.root.path().to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let w = worker(&h, "claude", foreign.id.as_str(), true).await;
    for target in [json!({"task_id":w.task}), json!({"terminal_id":w.terminal})] {
        let result = h.call("calm.terminal.observe", target).await;
        assert!(result.get("error").is_some(), "{result}");
    }
    stop(&h, &w).await;
}

#[tokio::test]
async fn task_without_a_viewer_reports_unavailable_without_spawning() {
    let h = Harness::start().await;
    let w = worker(&h, "codex", &h.track, false).await;
    let result = h
        .ok("calm.terminal.resolve", json!({"task_id":w.task}))
        .await;
    assert_eq!(result["available"], false);
    assert_eq!(result["controllable"], false);
    assert_eq!(result["terminal_id"], w.terminal);
    assert!(
        h.call("calm.terminal.observe", json!({"task_id":w.task}))
            .await
            .get("error")
            .is_some()
    );
    assert!(h.state.terminal_renderer.get(&w.terminal).is_none());
    assert!(
        h.sql
            .terminal_get(&w.terminal)
            .await
            .unwrap()
            .unwrap()
            .pid
            .is_none()
    );
}

#[tokio::test]
async fn selectors_are_exclusive_and_planner_cards_are_not_worker_terminals() {
    let h = Harness::start().await;
    for args in [
        json!({}),
        json!({"task_id":"x","terminal_id":"y"}),
        json!({"task_id":""}),
    ] {
        let reply = h.call("calm.terminal.resolve", args).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
    }
    let planner = h
        .sql
        .cards_by_track(&h.track)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "codex")
        .unwrap();
    let terminal = h
        .sql
        .terminal_get_by_card(planner.id.as_str())
        .await
        .unwrap()
        .unwrap();
    let reply = h
        .call("calm.terminal.resolve", json!({"terminal_id":terminal.id}))
        .await;
    assert!(reply.get("error").is_some());
}

#[tokio::test]
async fn task_completion_revokes_control_but_preserves_current_output() {
    let h = Harness::start().await;
    let w = worker(&h, "claude", &h.track, true).await;
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"claim"}),
    )
    .await;
    let before = snapshot(&h, json!({"task_id":w.task})).await;
    sqlx::query("UPDATE tasks SET status='done',finished_at_ms=?2 WHERE id=?1")
        .bind(&w.task)
        .bind(now_ms())
        .execute(h.sql.pool())
        .await
        .unwrap();
    let after = snapshot(&h, json!({"task_id":w.task})).await;
    assert_eq!(before["terminal_session_id"], after["terminal_session_id"]);
    for target in [json!({"task_id":w.task}), json!({"terminal_id":w.terminal})] {
        let mut args = target;
        args["observation_id"] = before["observation_id"].clone();
        args["request_id"] = json!("late");
        args["action"] = json!({"type":"text","text":"wrong"});
        assert!(
            h.call("calm.terminal.input", args)
                .await
                .get("error")
                .is_some()
        );
    }
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"release"}),
    )
    .await;
    stop(&h, &w).await;
}

/// The readback wait can outlive the task. A control readback with a change
/// wait parks on a quiet worker terminal; once it has subscribed to the
/// projection (so its pre-wait resolution is over) the task is finished and
/// output is injected to end the wait. The emitted status must be the
/// post-wait one: `done` and not controllable.
#[tokio::test]
async fn readback_reports_a_task_that_finished_during_the_wait() {
    let h = Harness::start().await;
    let w = worker(&h, "claude", &h.track, true).await;
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"claim"}),
    )
    .await;
    let entry = h.state.terminal_renderer.get(&w.terminal).unwrap();
    let waiters = || entry.handle.model_view.lock().unwrap().change_waiters();
    let before = waiters();
    let released = h.call(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"release","observe":true,"wait_for":"change","wait_ms":10000}),
    );
    let finish = async {
        while waiters() == before {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        sqlx::query("UPDATE tasks SET status='done',finished_at_ms=?2 WHERE id=?1")
            .bind(&w.task)
            .bind(now_ms())
            .execute(h.sql.pool())
            .await
            .unwrap();
        entry
            .handle
            .render_plane
            .lock()
            .unwrap()
            .on_pty_chunk(b"TASK_DONE\r\n".to_vec());
    };
    let (released, ()) = tokio::join!(released, finish);
    assert!(released.get("error").is_none(), "{released}");
    let receipt = &released["result"]["structuredContent"];
    assert_eq!(receipt["control_id"], Value::Null, "{receipt}");
    assert_eq!(receipt["observation"]["status"], "available", "{receipt}");
    let state = &receipt["observation"]["state"];
    assert_eq!(state["wait"]["outcome"], "changed", "{state}");
    assert!(
        state["text"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().trim_end() == "TASK_DONE"),
        "{state}"
    );
    assert_eq!(state["task_status"], "done", "{state}");
    assert_eq!(state["controllable"], false, "{state}");
    assert_eq!(state["task"]["task_id"], w.task);
    stop(&h, &w).await;
}

/// Write authority is decided under the connection's serial lock (#1618
/// round 2). An input readback with a long change wait holds the serial on a
/// quiet task terminal (no echo, so typing changes nothing); a second input is
/// called while the task still runs and queues behind it; the task then
/// finishes and injected output ends the first wait. The task is finished
/// only once the second input is counted as waiting for the serial
/// (`serial_waiters`), so it provably reached the lock while the task still
/// ran. When the queued input's turn comes it must be refused with the
/// write-authority error. A check taken before the serial would have passed
/// while the task was running and, with the revision moved and the saved
/// control still current, answered `stale_observation` although write
/// authority is gone.
#[tokio::test]
async fn input_queued_behind_a_readback_rechecks_write_authority_under_the_serial() {
    let h = Harness::start().await;
    let w = worker_running(
        &h,
        "claude",
        &h.track,
        Some("stty -echo; printf 'WORKER_READY\\n'; cat >/dev/null"),
    )
    .await;
    h.observe_text(&w.terminal, "WORKER_READY").await;
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"claim"}),
    )
    .await;
    let before = snapshot(&h, json!({"task_id":w.task})).await;
    let entry = h.state.terminal_renderer.get(&w.terminal).unwrap();
    let waiters = || entry.handle.model_view.lock().unwrap().change_waiters();
    let subscribed = waiters();
    let holder = h.call(
        "calm.terminal.input",
        json!({"task_id":w.task,"observation_id":before["observation_id"],"request_id":"hold","action":{"type":"text","text":"a"},"observe":true,"wait_for":"change","wait_ms":10000}),
    );
    let driver = async {
        let start = std::time::Instant::now();
        while waiters() == subscribed {
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // The queued input is called while the task is still running and the
        // task is finished only once that input is waiting for the serial.
        let queued = h.call(
            "calm.terminal.input",
            json!({"task_id":w.task,"observation_id":before["observation_id"],"request_id":"queued","action":{"type":"text","text":"b"}}),
        );
        let service = h.interaction();
        let finish = async {
            let start = std::time::Instant::now();
            while service.serial_waiters(&w.terminal).await == 0 {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(10),
                    "the queued input never reached the serial"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            sqlx::query("UPDATE tasks SET status='done',finished_at_ms=?2 WHERE id=?1")
                .bind(&w.task)
                .bind(now_ms())
                .execute(h.sql.pool())
                .await
                .unwrap();
            entry
                .handle
                .render_plane
                .lock()
                .unwrap()
                .on_pty_chunk(b"TASK_DONE\r\n".to_vec());
        };
        let (queued, ()) = tokio::join!(queued, finish);
        queued
    };
    let (held, queued) = tokio::join!(holder, driver);
    assert!(held.get("error").is_none(), "{held}");
    let receipt = &held["result"]["structuredContent"];
    assert_eq!(receipt["outcome"], "written", "{receipt}");
    assert_eq!(receipt["observation"]["status"], "available", "{receipt}");
    assert_eq!(receipt["observation"]["state"]["task_status"], "done");
    assert_eq!(receipt["observation"]["state"]["controllable"], false);
    let message = queued["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("queued input must be refused, got {queued}"));
    assert!(
        message.contains("task or worker session is not running; terminal control refused"),
        "{queued}"
    );
    assert!(!h.interaction().input_pending(&w.terminal).await);
    stop(&h, &w).await;
}

#[tokio::test]
async fn worker_session_replacement_invalidates_previous_task_observations() {
    use calm_server::db::sqlite::session_supersede_and_start_tx;
    use calm_server::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let h = Harness::start().await;
    let w = worker(&h, "codex", &h.track, true).await;
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"claim"}),
    )
    .await;
    let before = snapshot(&h, json!({"task_id":w.task})).await;
    let old = h
        .sql
        .session_get_by_id(&w.session.clone().into())
        .await
        .unwrap()
        .unwrap();
    let next = new_id();
    let mut tx = h.sql.pool().begin().await.unwrap();
    session_supersede_and_start_tx(
        &mut tx,
        &w.session,
        WorkerSessionInit {
            id: next.clone(),
            card_id: w.card.clone(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: Some(w.terminal.clone()),
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: old.spawn_op_id,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let resolved = h
        .ok("calm.terminal.resolve", json!({"task_id":w.task}))
        .await;
    assert_eq!(resolved["worker_session_id"], next);
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"claim"}),
    )
    .await;
    let fresh = snapshot(&h, json!({"task_id":w.task})).await;
    assert_ne!(before["connection_id"], fresh["connection_id"]);
    let rejected=h.call("calm.terminal.input",json!({"task_id":w.task,"observation_id":before["observation_id"],"request_id":"old","action":{"type":"text","text":"stale"}})).await;
    assert!(rejected.get("error").is_some(), "{rejected}");
    let accepted=h.ok("calm.terminal.input",json!({"task_id":w.task,"observation_id":fresh["observation_id"],"request_id":"new","action":{"type":"text","text":"current"}})).await;
    assert_eq!(accepted["outcome"], "written");
    stop(&h, &w).await;
}

#[tokio::test]
async fn recovered_task_cannot_be_followed_through_an_old_task_or_terminal_id() {
    use calm_types::task_recovery::{
        TASK_IN_TRACK_ROUTE, TaskRecoveryConstraint, TaskRecoveryRequest,
    };
    let h = Harness::start().await;
    let w = worker(&h, "terminal", &h.track, true).await;
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"claim"}),
    )
    .await;
    let task = h.sql.task_get(&w.task).await.unwrap().unwrap();
    let mut tx = h.sql.pool().begin().await.unwrap();
    sqlx::query("UPDATE tasks SET status='failed',finished_at_ms=?2 WHERE id=?1")
        .bind(&w.task)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .unwrap();
    let recovery = calm_server::db::sqlite::task_recovery_allocate_tx(
        &mut tx,
        &h.track,
        &task.key,
        &TaskRecoveryRequest {
            expected_attempt_id: w.task.clone(),
            idempotency_key: "next-generation".into(),
            reason: "test explicit new attempt".into(),
        },
        "fingerprint",
        &TaskRecoveryConstraint::V1 {
            refs: vec![calm_types::event::TaskContextRef {
                track_id: h.track.clone().into(),
                block_id: "b_1000".into(),
                rev: 1,
                hash: "0".repeat(64),
                is_root: true,
            }],
            spawn: TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        },
        &calm_server::ids::ActorId::User,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_ne!(recovery.attempt_id, w.task);
    for target in [json!({"task_id":w.task}), json!({"terminal_id":w.terminal})] {
        assert!(
            h.call("calm.terminal.observe", target)
                .await
                .get("error")
                .is_some()
        );
    }
    h.ok(
        "calm.terminal.control",
        json!({"task_id":w.task,"action":"detach"}),
    )
    .await;
    stop(&h, &w).await;
}

#[tokio::test]
async fn reassigned_task_worker_and_manual_restart_do_not_bypass_execution_binding() {
    let h = Harness::start().await;
    let old = worker(&h, "claude", &h.track, true).await;
    let other = worker(&h, "claude", &h.track, true).await;
    sqlx::query("UPDATE tasks SET worker_card_id=?2 WHERE id=?1")
        .bind(&old.task)
        .bind(&other.card)
        .execute(h.sql.pool())
        .await
        .unwrap();
    for target in [
        json!({"task_id":old.task}),
        json!({"terminal_id":old.terminal}),
    ] {
        assert!(
            h.call("calm.terminal.resolve", target)
                .await
                .get("error")
                .is_some()
        );
    }
    let restarted = worker(&h, "claude", &h.track, true).await;
    // The current task still names this card, but its replaced session
    // no longer carries the owning task operation. Direct ID must not bypass it.
    sqlx::query("UPDATE worker_sessions SET spawn_op_id=NULL WHERE id=?1")
        .bind(&restarted.session)
        .execute(h.sql.pool())
        .await
        .unwrap();
    assert!(
        h.call(
            "calm.terminal.resolve",
            json!({"terminal_id":restarted.terminal})
        )
        .await
        .get("error")
        .is_some()
    );
    stop(&h, &old).await;
    stop(&h, &other).await;
    stop(&h, &restarted).await;
}

#[tokio::test]
async fn missing_task_projection_cannot_be_reclassified_as_a_manual_terminal() {
    let h = Harness::start().await;
    let w = worker(&h, "codex", &h.track, true).await;
    assert_eq!(
        h.ok("calm.terminal.resolve", json!({"task_id":w.task}))
            .await["available"],
        true
    );
    sqlx::query("DELETE FROM tasks WHERE id=?1")
        .bind(&w.task)
        .execute(h.sql.pool())
        .await
        .unwrap();
    let result = h
        .call("calm.terminal.resolve", json!({"terminal_id":w.terminal}))
        .await;
    stop(&h, &w).await;
    assert!(
        result.get("error").is_some(),
        "lost task row must not turn its Worker into a manual terminal: {result}"
    );
}

#[tokio::test]
async fn manual_codex_and_claude_workers_are_controllable_without_a_task() {
    let h = Harness::start().await;
    for kind in ["codex", "claude"] {
        let card = new_id();
        let session = new_id();
        let roles = CardRoleCache::new();
        let theme = calm_server::routes::theme::RequestTheme::default_dark();
        let cwd = h.root.path().to_str().unwrap().to_owned();
        let mut tx = h.sql.pool().begin().await.unwrap();
        let terminal = if kind == "codex" {
            card_with_codex_create_tx(
                &mut tx,
                card.clone(),
                &session,
                None,
                h.track.clone().into(),
                None,
                None,
                cwd,
                json!({}),
                None,
                None,
                None,
                CardRole::Worker,
                true,
                &roles,
                theme,
            )
            .await
            .unwrap()
            .1
        } else {
            card_with_claude_create_tx(
                &mut tx,
                card.clone(),
                &session,
                h.track.clone().into(),
                None,
                None,
                "/bin/sh".into(),
                cwd,
                json!({}),
                None,
                None,
                None,
                "unused-settings".into(),
                new_id(),
                CardRole::Worker,
                true,
                &roles,
                theme,
            )
            .await
            .unwrap()
            .1
        };
        tx.commit().await.unwrap();
        spawn_viewer(&h, &terminal.id).await;
        let resolved = h
            .ok("calm.terminal.resolve", json!({"terminal_id":terminal.id}))
            .await;
        assert_eq!(resolved["task"], Value::Null);
        assert_eq!(resolved["card_kind"], kind);
        assert_eq!(resolved["available"], true);
        assert_eq!(resolved["controllable"], true);
        h.ok(
            "calm.terminal.control",
            json!({"terminal_id":terminal.id,"action":"claim"}),
        )
        .await;
        let before = h.observe_text(&terminal.id, "WORKER_READY").await;
        let written = h
            .input(
                &terminal.id,
                &before,
                "manual-text",
                json!({"type":"text","text":"manual"}),
            )
            .await;
        assert_eq!(written["outcome"], "written");
        let before = snapshot(&h, json!({"terminal_id":terminal.id})).await;
        let entered = h
            .input(
                &terminal.id,
                &before,
                "manual-enter",
                json!({"type":"key","key":"Enter"}),
            )
            .await;
        assert_eq!(entered["outcome"], "written");
        h.observe_text(&terminal.id, "WORKER_REPLY:manual").await;
        h.state.terminal_renderer.drop_entry(&terminal.id).await;
    }
}
