use super::*;
use std::sync::atomic::Ordering;

async fn boot(max: u32) -> Boot {
    super::super::boot_with_native(max, None, false, true).await
}
fn native(boot: &Boot) -> &super::super::close_native_peer::Stats {
    &boot.native.as_ref().unwrap().peer.stats
}
fn turns(boot: &Boot) -> u64 {
    native(boot).turns.load(Ordering::SeqCst)
}
fn threads(boot: &Boot) -> usize {
    native(boot).threads.load(Ordering::SeqCst) as usize
}
fn interrupted(boot: &Boot) -> Vec<(String, String)> {
    native(boot).interrupted()
}

async fn cleanup_pass_finished(
    boot: &Boot,
    planner: &Caller,
) -> calm_server::mcp_server::transport::carrier_quiescence::ToolCallGuard {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(guard) = boot.ctx.tool_calls.fence(&planner.identity.session_id) {
                break guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the cleanup pass must finish")
}

async fn finish_planners(boot: &Boot) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while boot
            .count("SELECT count(*) FROM track_close_cleanups")
            .await
            != 0
        {
            // The fixture has no periodic task. Await the real sweep so an
            // ACK-only pass can retain cleanup and a later pass can consume END.
            calm_server::terminal_sweeper::sweep(&boot.native.as_ref().unwrap().state)
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn planner_input(boot: &Boot, planner: &Caller, key: &str) -> (StatusCode, Value) {
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/cards/{}/planner/input",
                    planner.identity.card_id
                ))
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(json!({"text":"Start this work"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn planner_input_finished(boot: &Boot, planner: &Caller, key: &str) -> (StatusCode, Value) {
    use calm_server::test_seams::{PausePoint, install_pause_for_test};
    let finished = PausePoint {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    install_pause_for_test(
        "canonical-human-wake-finished",
        &planner.identity.card_id,
        finished.clone(),
    );
    let response = planner_input(boot, planner, key).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        finished.entered.notified(),
    )
    .await
    .unwrap();
    finished.release.notify_one();
    response
}

async fn wait_native_turn(boot: &Boot, planner: &Caller) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let ready: bool = boot.scalar(
                "SELECT COALESCE(length(json_extract(handle_state_json,'$.last_turn_id')),0)>0 FROM worker_sessions WHERE id=?1",
                &planner.identity.session_id,
            ).await;
            if ready { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("the real native turn must be addressable before Close");
}

#[tokio::test]
async fn close_cancels_running_descendant_planners_and_refuses_restart_and_reopen() {
    let boot = boot(16).await;
    let (root, _) = boot.user_track("active-root").await;
    let (child, child_planner) = child_of(&boot, &root, "active-child").await;
    let (leaf, leaf_planner) = child_of(&boot, &child, "active-leaf").await;
    task(&boot, &root, "child", "running", "sub-track", Some(&child)).await;
    task(&boot, &child, "leaf", "running", "sub-track", Some(&leaf)).await;
    for planner in [&child_planner, &leaf_planner] {
        let (status, body) = planner_input_finished(&boot, planner, "child/start").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        wait_native_turn(&boot, planner).await;
        assert!(boot.harness.get(&planner.identity.session_id).is_some());
    }
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    finish_planners(&boot).await;
    let before_turns = turns(&boot);
    let before_threads = threads(&boot);
    for planner in [&child_planner, &leaf_planner] {
        assert!(boot.harness.get(&planner.identity.session_id).is_none());
        let state: String = boot
            .scalar(
                "SELECT state FROM worker_sessions WHERE id=?1",
                &planner.identity.session_id,
            )
            .await;
        assert_eq!(state, "idle");
        let (status, body) = planner_input_finished(&boot, planner, "closed/restart").await;
        assert_eq!(status, StatusCode::OK, "durable input ACK: {body}");
        assert!(boot.harness.get(&planner.identity.session_id).is_none());
    }
    assert_eq!(
        send(
            boot.app.clone(),
            "PATCH",
            &format!("/api/tracks/{child}"),
            Some(json!({"closed":false}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, body) = planner_input_finished(&boot, &child_planner, "reopened/restart").await;
    assert_eq!(status, StatusCode::OK, "durable input ACK: {body}");
    assert!(
        boot.harness
            .get(&child_planner.identity.session_id)
            .is_none()
    );
    assert_eq!(turns(&boot), before_turns);
    assert_eq!(threads(&boot), before_threads);
    assert_eq!(
        boot.repo
            .task_get(&format!("{root}:child"))
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Canceled
    );
}

#[tokio::test]
async fn close_cancels_running_self_mcp_replies_before_planner_shutdown() {
    self_mcp_close_with_held_reply(false).await;
}

#[tokio::test]
async fn close_cancels_running_other_track_while_self_mcp_reply_is_held() {
    self_mcp_close_with_held_reply(true).await;
}

async fn self_mcp_close_with_held_reply(close_other: bool) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let boot = boot(16).await;
    let (track, planner) = boot.user_track("self-close").await;
    let (status, started) = planner_input(&boot, &planner, "self-close/start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let planner = boot.planner_of(&track).await;
    assert!(
        boot.harness.get(&planner.identity.session_id).is_some(),
        "production start installs its owner"
    );
    let thread: String = boot
        .scalar(
            "SELECT thread_id FROM worker_sessions WHERE id=?1",
            &planner.identity.session_id,
        )
        .await;
    let other = if close_other {
        let (track, planner) = boot.user_track("independent-close").await;
        let (status, started) = planner_input_finished(&boot, &planner, "independent/start").await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let planner = boot.planner_of(&track).await;
        assert!(boot.harness.get(&planner.identity.session_id).is_some());
        Some((track, planner))
    } else {
        None
    };
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET mcp_token_hash=?1 WHERE id=?2")
                .bind(calm_server::mcp_server::auth::hash_token(token.as_str()))
                .bind(&planner.identity.session_id),
        )
        .await
        .unwrap();
    let accepted = Arc::new(tokio::sync::Notify::new());
    let reply = Arc::new(tokio::sync::Notify::new());
    let mut registry = ToolRegistry::new();
    let handler = boot.registry.lookup(TOOL_TRACK_CLOSE).unwrap();
    let descriptor = boot
        .registry
        .descriptors()
        .into_iter()
        .find(|d| d.name == TOOL_TRACK_CLOSE)
        .unwrap();
    registry.register(
        descriptor,
        Arc::new({
            let (accepted, reply) = (accepted.clone(), reply.clone());
            move |ctx, identity, args| {
                let (handler, accepted, reply) = (handler.clone(), accepted.clone(), reply.clone());
                Box::pin(async move {
                    let result = handler(ctx, identity, args).await;
                    accepted.notify_one();
                    reply.notified().await;
                    result
                })
            }
        }),
    );
    let socket_dir = tempfile::tempdir_in("/tmp").unwrap();
    let socket = socket_dir.path().join("mcp.sock");
    let server = calm_server::mcp_server::McpServer::spawn_with_context(
        boot.ctx.clone(),
        socket.clone(),
        PathBuf::from("unused-shim"),
        Arc::new(registry),
    )
    .await
    .unwrap();
    let (rd, mut wr) = tokio::net::UnixStream::connect(&socket)
        .await
        .unwrap()
        .into_split();
    let mut rd = BufReader::new(rd);
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"close-test","version":"1"},
        "_meta":{"dev.neige/auth":{"token":token.as_str()}}}});
    wr.write_all(format!("{init}\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    rd.read_line(&mut line).await.unwrap();
    assert!(
        serde_json::from_str::<Value>(&line)
            .unwrap()
            .get("error")
            .is_none(),
        "{line}"
    );
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":TOOL_TRACK_CLOSE,"arguments":{"message":"stop"}}});
    wr.write_all(format!("{call}\n").as_bytes()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), accepted.notified())
        .await
        .unwrap();
    assert_eq!(
        boot.count("SELECT count(*) FROM track_close_cleanups")
            .await,
        1
    );
    assert!(
        boot.harness.get(&planner.identity.session_id).is_some(),
        "inflight response retains the Planner"
    );
    // The caller-visible closed state commits without a network wait in the SQLite writer.
    assert!(!open(&boot, &track).await);
    let other_finished = if let Some((other_track, other_planner)) = &other {
        // Strictly separate request timestamps so A is first in every cleanup scan,
        // even if its random session id sorts after B's.
        let requested: i64 = boot
            .scalar(
                "SELECT requested_at_ms FROM track_close_cleanups WHERE worker_session_id=?1",
                &planner.identity.session_id,
            )
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while calm_server::model::now_ms() <= requested {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(rest_close(&boot, other_track).await, StatusCode::OK);
        let finished = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pending: i64 = boot
                    .scalar(
                        "SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1",
                        &other_planner.identity.session_id,
                    )
                    .await;
                let state: String = boot
                    .scalar(
                        "SELECT state FROM worker_sessions WHERE id=?1",
                        &other_planner.identity.session_id,
                    )
                    .await;
                if pending == 0
                    && state == "idle"
                    && boot
                        .harness
                        .get(&other_planner.identity.session_id)
                        .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let state: String = boot
            .scalar(
                "SELECT state FROM worker_sessions WHERE id=?1",
                &other_planner.identity.session_id,
            )
            .await;
        let pending: i64 = boot
            .scalar(
                "SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1",
                &other_planner.identity.session_id,
            )
            .await;
        eprintln!(
            "before A reply: B session={} state={state} harness={} cleanup={pending} completed={}",
            other_planner.identity.session_id,
            boot.harness
                .get(&other_planner.identity.session_id)
                .is_some(),
            finished.is_ok()
        );
        finished.is_ok()
    } else {
        true
    };
    assert!(boot.harness.get(&planner.identity.session_id).is_some());
    let pending: i64 = boot
        .scalar(
            "SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1",
            &planner.identity.session_id,
        )
        .await;
    assert_eq!(pending, 1, "A retains its exact obligation until its reply");
    let active: bool = boot.scalar("SELECT state IN ('starting','running','idle','turn_pending') FROM worker_sessions WHERE id=?1", &planner.identity.session_id).await;
    assert!(active);
    assert!(interrupted(&boot).iter().all(|(id, _)| id != &thread));
    reply.notify_one();
    line.clear();
    tokio::time::timeout(std::time::Duration::from_secs(5), rd.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 2);
    assert!(response.get("error").is_none(), "{response}");
    assert_ne!(response["result"]["isError"], true, "{response}");
    finish_planners(&boot).await;
    assert!(boot.harness.get(&planner.identity.session_id).is_none());
    let state: String = boot
        .scalar(
            "SELECT state FROM worker_sessions WHERE id=?1",
            &planner.identity.session_id,
        )
        .await;
    assert_eq!(state, "idle");
    assert!(interrupted(&boot).iter().any(|(id, _)| id == &thread));
    server.stop_listener_for_test().await;
    assert!(
        other_finished,
        "an independent idle-admission Planner must finish cleanup before A's held MCP reply is released"
    );
}

#[tokio::test]
async fn close_cancels_running_planner_cleanup_retries_after_unconfirmed_stop() {
    let boot = boot(16).await;
    let (track, planner) = boot.user_track("retry-stop").await;
    let (status, started) = planner_input(&boot, &planner, "retry-stop/start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let planner = boot.planner_of(&track).await;
    native(&boot).fail_stop(true);
    assert_eq!(rest_close(&boot, &track).await, StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while interrupted(&boot).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // The RPC record precedes its response and factual reads. Wait for this
    // cleanup pass's admission fence before asserting failure or poking retry.
    let settled = cleanup_pass_finished(&boot, &planner).await;
    assert_eq!(
        boot.count("SELECT count(*) FROM track_close_cleanups")
            .await,
        1
    );
    assert!(boot.harness.get(&planner.identity.session_id).is_some());
    // A failed Stop neither releases the obligation nor makes the native owner current again.
    let before_turns = turns(&boot);
    let before_threads = threads(&boot);
    let original = interrupted(&boot)[0].clone();
    let (acknowledged, release_end) = native(&boot).hold_end();
    let mut notifications = boot.codex.subscribe_notifications();
    native(&boot).fail_stop(false);
    drop(settled);
    boot.ctx
        .scheduler_poke
        .get()
        .unwrap()
        .poke_track_close_cleanups();
    tokio::time::timeout(std::time::Duration::from_secs(5), acknowledged.notified())
        .await
        .unwrap();
    let settled = cleanup_pass_finished(&boot, &planner).await;
    assert_eq!(
        boot.count("SELECT count(*) FROM track_close_cleanups")
            .await,
        1,
        "Stop ACK without the original END must retain cleanup"
    );
    assert!(boot.harness.get(&planner.identity.session_id).is_some());
    release_end.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let calm_server::codex_appserver::Notification::TurnCompleted { thread_id, turn } =
                notifications.recv().await.unwrap()
                && thread_id == original.0
                && turn["id"] == original.1
            {
                break;
            }
        }
    })
    .await
    .expect("the original END must reach the native owner");
    drop(settled);
    finish_planners(&boot).await;
    assert!(boot.harness.get(&planner.identity.session_id).is_none());
    assert_eq!(turns(&boot), before_turns);
    assert_eq!(threads(&boot), before_threads);
    assert!(interrupted(&boot).iter().all(|stop| stop == &original));
}

#[tokio::test]
async fn close_cancels_running_planner_cleanup_spares_a_successor_session_and_does_not_hold_writer()
{
    let boot = boot(16).await;
    let (track, planner) = boot.user_track("session-fence").await;
    wait_native_turn(&boot, &planner).await;
    let before = planner.identity.session_id.clone();
    let (entered, release) = native(&boot).hold_stop();
    assert_eq!(rest_close(&boot, &track).await, StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    // Simulate a predecessor discovered before a replacement. The original marker may stop A,
    // but neither card-current lookup nor completion may select B. This writer must proceed
    // while A's native Stop is blocked outside SQLite.
    let mut successor = session(
        "successor",
        &track,
        &planner.identity.card_id,
        WorkerContract::Planner,
    );
    successor.state = WorkerSessionState::Running;
    successor.thread_id = Some("successor-thread".into());
    successor.active_turn_id = Some("successor-turn".into());
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        boot.repo
            .db()
            .writer()
            .execute(
                sqlx::query(
                    "UPDATE worker_sessions SET state='superseded',completed_at_ms=2 WHERE id=?1",
                )
                .bind(&before),
            )
            .await
            .unwrap();
        boot.seed_session(successor).await;
        boot.repo
            .db()
            .writer()
            .execute(
                sqlx::query("UPDATE cards SET session_id='successor' WHERE id=?1")
                    .bind(&planner.identity.card_id),
            )
            .await
            .unwrap();
    })
    .await
    .unwrap();
    release.notify_one();
    finish_planners(&boot).await;
    let state: String = boot
        .scalar("SELECT state FROM worker_sessions WHERE id=?1", "successor")
        .await;
    assert_eq!(state, "running");
    assert!(
        interrupted(&boot)
            .iter()
            .all(|(thread, _)| thread != "successor-thread")
    );
    let current: String = boot
        .scalar(
            "SELECT session_id FROM cards WHERE id=?1",
            &planner.identity.card_id,
        )
        .await;
    assert_eq!(current, "successor");
}
