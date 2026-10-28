//! Child operations and the real booted scheduler use Dispatcher System authority.
use super::*;
use calm_truth::db::RepoRead;
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
const SEED: &str = "u11 child durable seed";
async fn until(mut check: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("child lifecycle must reach the observed boundary");
}
async fn task_status(fixture: &Fixture, task: &str) -> String {
    sqlx::query_scalar("SELECT status FROM tasks WHERE id=?1")
        .bind(task)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap()
}
async fn identities(fixture: &Fixture, task: &str) -> (String, String, String, i64, i64) {
    sqlx::query_as("SELECT t.child_track_id,c.id,c.session_id,
        (SELECT count(*) FROM worker_sessions ws WHERE ws.card_id=c.id),
        (SELECT count(*) FROM events e WHERE e.scope_track=t.child_track_id AND e.kind='track.wake_requested')
        FROM tasks t JOIN cards c ON c.track_id=t.child_track_id AND c.role='planner' WHERE t.id=?1")
        .bind(task).fetch_one(fixture.repo.pool()).await.unwrap()
}
#[derive(Clone, Copy, PartialEq)]
enum Window {
    Outage,
    Workspace,
    Installed,
    Failed,
    Routes,
}
#[tokio::test]
async fn u11_canonical_child_scheduler_outage_cold_retry_delivers_original_system_once() {
    child_flow(Window::Outage).await;
}
#[tokio::test]
async fn u11_canonical_child_attached_workspace_retry_blocks_native_start() {
    child_flow(Window::Workspace).await;
}
#[tokio::test]
async fn u11_canonical_child_installed_handle_waits_for_original_success() {
    child_flow(Window::Installed).await;
}
#[tokio::test]
async fn u11_canonical_child_failed_settlement_keeps_dispatched() {
    child_flow(Window::Failed).await;
}
#[tokio::test]
async fn u11_canonical_child_bootstrap_serializes_real_reset_and_send() {
    child_flow(Window::Routes).await;
}
async fn child_flow(window: Window) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "child-area",
        json!({"name":"Child bootstrap","color":"#123456"}),
    )
    .await;
    let mut parent_body = json!({"area_id":area["id"],"title":"parent","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}});
    if matches!(window, Window::Workspace | Window::Installed) {
        let repository = fixture.root.path().join("attached");
        std::fs::create_dir(&repository).unwrap();
        for args in [
            vec!["init"],
            vec![
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&repository)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        parent_body["cwd"] = json!(repository);
        parent_body["attach_folder"] = json!(true);
    }
    let parent = post(&app, &cookie, "/api/tracks", "child-parent", parent_body).await;
    let parent_id = parent["id"].as_str().unwrap();
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.permission_mode','full', \
         '$.model','child-model','$.reasoning_effort','high') WHERE track_id=?1 AND role='planner'",
    )
    .bind(parent_id)
    .execute(fixture.repo.pool())
    .await
    .unwrap();
    let task = format!("{parent_id}:u11-child");
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json, \
         depends_on_json,priority,status,spawn,created_at_ms,updated_at_ms,access)
        VALUES(?1,?2,'u11-child','codex',?3,'null','[]',0,'dispatched',?4,?5,?5,'read_write')",
    )
    .bind(&task)
    .bind(parent_id)
    .bind(SEED)
    .bind(TASK_CHILD_TRACK_ROUTE)
    .bind(crate::model::now_ms())
    .execute(fixture.repo.pool())
    .await
    .unwrap();
    let workspace_pause = crate::test_seams::PausePoint::default();
    if window == Window::Workspace {
        crate::test_seams::install_pause_for_test(
            "u11-child-workspace",
            &task,
            workspace_pause.clone(),
        );
    }
    let creating = tokio::spawn({
        let scheduler = state.dispatcher.scheduler();
        async move { scheduler.sweep_all().await }
    });
    until(async || {
        sqlx::query_scalar::<_, bool>("SELECT child_track_id IS NOT NULL FROM tasks WHERE id=?1")
            .bind(&task)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap()
    })
    .await;
    let original = identities(&fixture, &task).await;
    if window == Window::Workspace {
        tokio::time::timeout(Duration::from_secs(5), workspace_pause.entered.notified())
            .await
            .unwrap();
        let track = fixture.repo.track_get(&original.0).await.unwrap().unwrap();
        let worktree = track.workspace.worktree.unwrap();
        std::fs::create_dir_all(std::path::Path::new(&worktree).parent().unwrap()).unwrap();
        std::fs::write(&worktree, "blocks git worktree add").unwrap();
        workspace_pause.release.notify_one();
        creating.await.unwrap();
        let phase: String = sqlx::query_scalar(
            "SELECT phase FROM operations WHERE kind='child-track' AND idempotency_key=?1",
        )
        .bind(&task)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(phase, "awaiting_retry");
        let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
        let reconciling = tokio::spawn({
            let dispatcher = state.dispatcher.clone();
            async move { dispatcher.reconcile_tick_for_test().await }
        });
        let unexpected = tokio::time::timeout(Duration::from_millis(400), async {
            loop {
                let call = request(&mut peer).await;
                match call["method"].as_str().unwrap() {
                    "config/read" => {
                        send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                    }
                    "model/list" => {
                        send(&mut peer, json!({"id":call["id"],"error":{"code":-32603,"message":"catalog unavailable"}})).await;
                    }
                    "thread/start" | "turn/start" => break call,
                    other => panic!("unexpected native request before Workspace readiness: {other}"),
                }
            }
        }).await;
        assert!(
            unexpected.is_err(),
            "Workspace AwaitingRetry must prevent every native request; got method {:?}",
            unexpected.as_ref().ok().map(|call| &call["method"])
        );
        reconciling.await.unwrap();
        let association: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT thread_id,spawn_op_id FROM worker_sessions WHERE id=?1")
                .bind(&original.2)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert_eq!(association, (None, None));
        // Keep the filesystem obstruction through cold boot, then retry the original operation.
    } else {
        creating.await.unwrap();
    }
    assert_eq!((original.3, original.4), (1, 1));
    assert_eq!(
        task_status(&fixture, &task).await,
        "dispatched",
        "outage must not pretend bootstrap activated"
    );
    until(async || {
        fixture
            .carrier(&original.0)
            .await
            .2
            .canonical_inputs
            .pending
            .len()
            == 1
    })
    .await;
    let (_, _, snapshot) = fixture.carrier(&original.0).await;
    assert_eq!(snapshot.header.phase, HarnessPhaseTag::Idle);
    assert_eq!(
        snapshot.canonical_inputs.pending.len(),
        1,
        "durable System acceptance survives outage"
    );
    assert!(matches!(
        snapshot.canonical_inputs.pending[0],
        calm_types::conversation::PendingInput::System { .. }
    ));
    for _ in 0..2 {
        state.dispatcher.scheduler().sweep_all().await;
    }
    assert_eq!(identities(&fixture, &task).await, original);
    let child = fixture.repo.track_get(&original.0).await.unwrap().unwrap();
    let parent_track = fixture.repo.track_get(parent_id).await.unwrap().unwrap();
    assert_eq!(child.area_id, parent_track.area_id);
    let frozen_workspace = child.workspace.clone();
    let raw: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
        .bind(&original.1)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    let binding: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(binding["planner_provider"], "codex");
    assert_eq!(binding["permission_mode"], "full");
    assert_eq!(binding["model"], "child-model");
    assert_eq!(binding["reasoning_effort"], "high");
    assert!(binding.get("prompt").is_none());
    shutdown(&fixture, &state).await;
    drop((app, state));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, _app, _cookie) = fixture.application().await;
    assert_eq!(identities(&fixture, &task).await, original);
    assert_eq!(
        fixture
            .repo
            .track_get(&original.0)
            .await
            .unwrap()
            .unwrap()
            .workspace,
        frozen_workspace
    );
    if window == Window::Workspace {
        std::fs::remove_file(frozen_workspace.worktree.as_ref().unwrap()).unwrap();
    }
    let installed = crate::test_seams::PausePoint::default();
    if matches!(window, Window::Installed | Window::Failed) {
        crate::test_seams::install_pause_for_test(
            "u11-retained-installed",
            &original.1,
            installed.clone(),
        );
    }
    let result_ready = crate::test_seams::PausePoint::default();
    let probe = if window == Window::Installed {
        crate::test_seams::install_pause_for_test("u11-child-result", &task, result_ready.clone());
        let scheduler = state.dispatcher.scheduler();
        let probe = tokio::spawn(async move { scheduler.sweep_all().await });
        tokio::time::timeout(Duration::from_secs(5), result_ready.entered.notified())
            .await
            .unwrap();
        Some(probe)
    } else {
        None
    };
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let scheduler = state.dispatcher.scheduler();
    let dispatcher = state.dispatcher.clone();
    let bootstrap_card = crate::ids::CardId::from(original.1.clone());
    let bootstrap_worker = original.2.clone();
    let sweep = tokio::spawn(async move {
        // Dispatcher owns activation here so the inflight guard cannot mask the second sweep.
        if matches!(window, Window::Installed | Window::Failed) {
            dispatcher
                .bootstrap_canonical_child(&bootstrap_card, &bootstrap_worker)
                .await
                .unwrap();
        } else {
            scheduler.sweep_all().await;
        }
    });
    let mut starts = 0;
    let mut reset = None;
    let call = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => {
                    starts += 1;
                    assert_eq!(task_status(&fixture, &task).await, "dispatched", "activation must precede running");
                    let route = crate::state::RouteState::from_ref(&state);
                    let held = route.planner_recovery_locks.get(&original.1)
                        .map(|entry| Arc::strong_count(entry.value())).unwrap_or(0);
                    assert!(held > 1, "native activation holds the original card's fence");
                    if window == Window::Routes {
                        let reset_app = _app.clone();
                        let reset_cookie = _cookie.clone();
                        let uri = format!("/api/cards/{}/planner/reset", original.1);
                        let waiting = tokio::spawn(async move {
                            status(&reset_app, &reset_cookie, &uri, "u11-reset", json!({})).await
                        });
                        until(async || route.planner_recovery_locks.get(&original.1)
                            .is_some_and(|entry| Arc::strong_count(entry.value()) > held)).await;
                        assert!(!waiting.is_finished(), "real reset must wait behind bootstrap");
                        let sent = post(&_app, &_cookie, &format!("/api/cards/{}/planner/input", original.1),
                            "u11-send", json!({"text":"u11 competing Human"})).await;
                        assert_eq!(sent["card_id"], original.1);
                        assert!(!waiting.is_finished());
                        reset = Some(waiting);
                    }
                    json!({"thread":{"id":THREAD}})
                }
                "turn/start" => break call,
                "model/list" => {
                    send(&mut peer, json!({"id":call["id"],"error":{"code":-32603,"message":"catalog unavailable"}})).await;
                    continue;
                },
                other => panic!("unexpected Child native request {other}: {call}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    }).await.expect("System bootstrap reaches native peer");
    assert_eq!(starts, 1);
    assert_eq!(call["params"]["input"].to_string().matches(SEED).count(), 1);
    let human_in_initial = call["params"]["input"]
        .to_string()
        .contains("u11 competing Human");
    let mut turns = 1;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    if matches!(window, Window::Installed | Window::Failed) {
        tokio::time::timeout(Duration::from_secs(5), installed.entered.notified())
            .await
            .unwrap();
        assert!(state.harness.get(&original.2).is_some());
        let activation: String =
            sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
                .bind(&original.2)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert!(
            state
                .operation_runtime
                .operation_result(&activation)
                .await
                .unwrap()
                .is_none()
        );
        if window == Window::Installed {
            result_ready.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), probe.unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                task_status(&fixture, &task).await,
                "dispatched",
                "installed handle cannot stand in for original operation success"
            );
        }
        if window == Window::Failed {
            sqlx::query(concat!(
                "CREATE TRIGGER u11_reject_activation BEFORE UPDATE OF phase ON operations ",
                "WHEN NEW.kind='planner-harness-start' AND NEW.phase='spawn_succeeded' ",
                "BEGIN SELECT RAISE(ABORT,'u11 reject success persistence'); END",
            ))
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        }
        // Expire the original claim after Track has passed to the installed
        // harness. Another real owner keeps Track throughout the re-drive:
        // settlement must reuse the handle without entering workspace repair.
        let retry_fence = if window == Window::Installed {
            let fence = crate::per_card_lock::lock_key(
                state.operation_runtime.track_delete_locks(),
                &original.0,
            )
            .await;
            let changed = sqlx::query(
                "UPDATE operations SET lease_until_ms=0 WHERE id=?1 AND phase='spawn_started'",
            )
            .bind(&activation)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
            assert_eq!(changed.rows_affected(), 1);
            let effects = crate::test_seams::count_workspace_observation_for_test(
                "track-metadata-before",
                &fixture.root.path().join("attached"),
            );
            Some((fence, effects))
        } else {
            None
        };
        installed.release.notify_one();
        if let Some((fence, effects)) = retry_fence {
            until(async || {
                state
                    .operation_runtime
                    .operation_result(&activation)
                    .await
                    .unwrap()
                    .is_some()
            })
            .await;
            assert!(matches!(
                state
                    .operation_runtime
                    .operation_result(&activation)
                    .await
                    .unwrap()
                    .unwrap()
                    .outcome,
                crate::operation::OperationOutcome::Succeeded { .. }
            ));
            assert_eq!(
                effects.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "installed re-drive must not run Git repair while another owner holds Track"
            );
            drop(fence);
        }
        if window == Window::Failed {
            until(async || {
                state
                    .operation_runtime
                    .operation_result(&activation)
                    .await
                    .unwrap()
                    .is_some()
            })
            .await;
            assert!(matches!(
                state
                    .operation_runtime
                    .operation_result(&activation)
                    .await
                    .unwrap()
                    .unwrap()
                    .outcome,
                crate::operation::OperationOutcome::Stuck { .. }
            ));
            state.dispatcher.scheduler().sweep_all().await;
            assert_eq!(
                task_status(&fixture, &task).await,
                "dispatched",
                "failed original activation cannot become running"
            );
        }
    }
    if window != Window::Failed {
        until(async || {
            state.dispatcher.scheduler().sweep_all().await;
            task_status(&fixture, &task).await == "running"
        })
        .await;
    }
    sweep.await.unwrap();
    if let Some(reset) = reset {
        let status = tokio::time::timeout(Duration::from_secs(5), reset)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "canonical reset remains refused by its existing storage owner"
        );
    }
    fixture
        .wait_phase(&original.0, HarnessPhaseTag::TurnRunning)
        .await;
    state.dispatcher.scheduler().sweep_all().await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(identities(&fixture, &task).await, original);
    let counts: (i64,i64,i64) = sqlx::query_as("SELECT
        (SELECT count(*) FROM conversation_messages WHERE card_id=?1),
        (SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1),
        (SELECT count(*) FROM conversation_submission_inputs i JOIN conversation_input_submissions s ON s.id=i.submission_id WHERE s.card_id=?1 AND i.observation_id IS NOT NULL)")
        .bind(&original.1).fetch_one(fixture.repo.pool()).await.unwrap();
    assert_eq!(
        counts,
        (i64::from(window == Window::Routes), 1, 1),
        "one System delivery; only the real competing send may create a Human"
    );
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    fixture
        .wait_phase(&original.0, HarnessPhaseTag::TurnCompleted)
        .await;
    if window == Window::Routes && !human_in_initial {
        let human = tokio::time::timeout(Duration::from_secs(5), request(&mut peer))
            .await
            .unwrap();
        assert_eq!(human["method"], "turn/start");
        let input = human["params"]["input"].to_string();
        assert_eq!(input.matches("u11 competing Human").count(), 1);
        assert!(
            !input.contains(SEED),
            "the competing send cannot replay bootstrap"
        );
        turns += 1;
        send(
            &mut peer,
            json!({"id":human["id"],"result":{"turn":{"id":"u11-human-turn"}}}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/started",
            json!({"id":"u11-human-turn","status":"inProgress"}),
        )
        .await;
        fixture
            .wait_phase(&original.0, HarnessPhaseTag::TurnRunning)
            .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":"u11-human-turn","status":"completed"}),
        )
        .await;
        fixture
            .wait_phase(&original.0, HarnessPhaseTag::TurnCompleted)
            .await;
    }
    state.dispatcher.reconcile_tick_for_test().await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(starts, 1);
    assert_eq!(fixture.counts(&original.1).await.3, turns);
    // Crash window: activation and delivery committed, but dispatched->running did not.
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
        .bind(&task)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    // Serve shutdown interruption while rejecting every extra start.
    {
        let closing = shutdown(&fixture, &state);
        tokio::pin!(closing);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    _ = &mut closing => break,
                    call = request(&mut peer) => {
                        assert_eq!(call["method"], "turn/interrupt");
                        send(&mut peer, json!({"id":call["id"],"result":{}})).await;
                    }
                }
            }
        })
        .await
        .unwrap();
    }
    drop((_app, state));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, _, _) = fixture.application().await;
    state.dispatcher.scheduler().sweep_all().await;
    assert_eq!(
        task_status(&fixture, &task).await,
        if window == Window::Failed {
            "dispatched"
        } else {
            "running"
        },
        "cold boot must consume only the original activation's committed outcome"
    );
    assert_eq!(identities(&fixture, &task).await, original);
    assert_eq!(fixture.counts(&original.1).await.3, turns);
    // Keep reading the original peer through cold reconciliation too.
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&fixture, &state).await;
}
