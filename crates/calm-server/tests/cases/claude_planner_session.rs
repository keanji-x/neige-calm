//! #1791 PR2b: a real `ClaudePlannerSession` driving a fake `claude` (bash) through every turn
//! path — exit, kill, linger, undecodable, stalled write, immediate exit — plus the submission
//! contract's refusals through the registered executor. See `claude_planner_session_fixture.rs` for the rig.

use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::claude_planner::session::SettlePause;
use calm_server::error::CalmError;
use calm_server::harness::backend::{StartDiagnostic, TurnStartFailure};
use calm_server::planner_process::sigkill_verified_for_test;
use calm_server::proc_identity::read_proc_start_time;
use provider::InputItem;
use provider::TurnModelSelection;
use provider::control::TurnStartOutcome;
use provider::events::{ItemPhase, PlannerEvent, PlannerEventKind};
use serde_json::Value;
use tokio::sync::Notify;

use super::claude_planner_session_fixture::{
    P_D_FIXTURE, RegisteredRig as Rig, alive, client_id, cmdline, completed_turn, request,
    until_completed, wait_for_file,
};

fn started(outcome: TurnStartOutcome<CalmError, StartDiagnostic>) -> String {
    match outcome {
        TurnStartOutcome::Started { turn } => turn.into_string(),
        other => panic!("expected Started, got {other:?}"),
    }
}

/// Larger than a pipe buffer, so writing it cannot complete unless the CLI reads it.
fn oversized_text() -> Vec<InputItem> {
    vec![InputItem::Text {
        text: "x".repeat(256 * 1024),
    }]
}

fn item_types(seen: &[PlannerEvent]) -> Vec<(String, String)> {
    seen.iter()
        .filter_map(|n| match &n.kind {
            PlannerEventKind::Item {
                phase,
                params,
                questions,
            } => {
                assert!(
                    questions.is_empty(),
                    "a Claude item never asks: {questions:?}"
                );
                Some((
                    phase.method().to_string(),
                    params["item"]["type"].as_str().unwrap_or("").to_string(),
                ))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_path_records_the_outcome_before_turn_completed() {
    let rig = Rig::new("exit").await;
    let pause = SettlePause {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    rig.session()
        .set_before_turn_completed_pause_for_test(pause.clone());
    let mut rx = rig.session().subscribe_events();
    let turn = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    assert_eq!(
        rig.session()
            .active_turn_id_for_thread(&rig.thread)
            .as_deref(),
        Some(turn.as_str())
    );
    tokio::time::timeout(Duration::from_secs(30), pause.entered.notified())
        .await
        .expect("settlement reaches the pause");
    let outcomes = rig.outcomes().await;
    assert_eq!(
        outcomes.len(),
        1,
        "outcome durable at the pause: {outcomes:?}"
    );
    assert_eq!(outcomes[0]["id"], turn.as_str());
    assert_eq!(outcomes[0]["status"], "completed");
    assert!(
        outcomes[0]["durationMs"].as_u64().is_some(),
        "the stored row carries the turn's measured duration: {outcomes:?}"
    );
    let mut before = Vec::new();
    while let Ok(n) = rx.try_recv() {
        before.push(n);
    }
    assert!(
        !before
            .iter()
            .any(|n| matches!(n.kind, PlannerEventKind::TurnCompleted { .. })),
        "TurnCompleted must not precede the durable outcome: {before:?}"
    );
    assert!(matches!(
        before.first().map(|n| &n.kind),
        Some(PlannerEventKind::TurnStarted { .. })
    ));
    assert!(
        rig.instructions_files().is_empty(),
        "file removed before emit"
    );
    pause.release.notify_one();
    let seen = until_completed(&mut rx).await;
    let completed = completed_turn(&seen);
    assert_eq!(completed["id"], turn.as_str());
    assert_eq!(completed["status"], "completed");
    let items = item_types(&before);
    assert!(items.contains(&("item/completed".into(), "userMessage".into())));
    assert!(items.contains(&("item/completed".into(), "agentMessage".into())));
    assert!(
        before
            .iter()
            .any(|n| matches!(n.kind, PlannerEventKind::TokenUsage { .. }))
    );
    assert_eq!(rig.session().active_turn_id_for_thread(&rig.thread), None);
    let argv = rig.read_bin("argv").expect("argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv
        .iter()
        .position(|a| *a == "--session-id")
        .expect("new session");
    assert_eq!(argv[at + 1], rig.thread);
    assert!(!argv.contains(&"--resume"));
    assert_env_is_the_allowlist(&rig).await;
    // The first init bound the session: the next turn resumes it.
    let second = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("again"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("second turn_start");
    pause.release.notify_one();
    let seen = until_completed(&mut rx).await;
    assert_eq!(completed_turn(&seen)["id"], second.as_str());
    let argv = rig.read_bin("argv").expect("argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv.iter().position(|a| *a == "--resume").expect("resume");
    assert_eq!(argv[at + 1], rig.thread);
}

async fn assert_env_is_the_allowlist(rig: &Rig) {
    let env = rig.read_bin("env").expect("env");
    let allowed = [
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LANGUAGE",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "TZ",
        "TMPDIR",
        "TEMP",
        "TMP",
        "NO_PROXY",
        "no_proxy",
        "ALL_PROXY",
        "all_proxy",
        "SSL_CERT_FILE",
        "PATH",
        "CLAUDE_CONFIG_DIR",
        "NEIGE_MCP_SOCKET",
        "NEIGE_MCP_TOKEN",
        "NEIGE_CLAUDE_PLANNER",
        "DISABLE_AUTOUPDATER",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
        // bash's own
        "PWD",
        "SHLVL",
        "_",
        "OLDPWD",
    ];
    let pairs: Vec<(&str, &str)> = env.lines().filter_map(|l| l.split_once('=')).collect();
    for (key, _) in &pairs {
        assert!(allowed.contains(key), "{key} is not on the allowlist");
    }
    let get = |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    assert_eq!(get("NEIGE_CLAUDE_PLANNER"), None);
    // The first turn minted the credential: the spawn carries the plaintext of the row's hash.
    let token = get("NEIGE_MCP_TOKEN").expect("token");
    assert_eq!(
        rig.mcp_token_hash().await.as_deref(),
        Some(calm_server::mcp_server::auth::hash_token(token).as_str())
    );
    assert_eq!(get("DISABLE_AUTOUPDATER"), Some("1"));
    // #1814: past `env_clear()`, the spawned process itself sees auto-memory off.
    assert_eq!(get("CLAUDE_CODE_DISABLE_AUTO_MEMORY"), Some("1"));
    assert_eq!(
        get("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from),
        Some(
            rig.host
                .configured()
                .expect("configured")
                .config_dir
                .clone()
        )
    );
    assert!(get("PATH").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_surviving_setsid_child_of_a_successful_exit_is_gone_before_turn_completed() {
    let rig = Rig::new("exit-with-orphan").await;
    let mut rx = rig.session().subscribe_events();
    rig.session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    let seen = until_completed(&mut rx).await;
    let orphan_alive = rig
        .read_bin("orphan")
        .and_then(|pid| pid.trim().parse::<i32>().ok())
        .map(alive);
    assert_eq!(completed_turn(&seen)["status"], "completed");
    assert_eq!(
        orphan_alive,
        Some(false),
        "the setsid child outlived settlement"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_cli_settles_failed_with_its_exit_status() {
    let rig = Rig::new("hold").await;
    let mut rx = rig.session().subscribe_events();
    let turn = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    // Kill only once the tool call is open, so settlement has an item to close.
    loop {
        let next = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the tool call starts")
            .expect("notification");
        if matches!(&next.kind, PlannerEventKind::Item { phase, params, .. }
            if *phase == ItemPhase::Started && params["item"]["id"] == "toolu_hold")
        {
            break;
        }
    }
    // The fake is the session's live, unreaped child here, so its pid names it; the kill still goes
    // through the start_time-verified signal like every other test kill.
    let pid: i32 = rig
        .read_bin("pid")
        .expect("pid")
        .trim()
        .parse()
        .expect("pid");
    let start_time = read_proc_start_time(pid).expect("the fake is alive");
    assert!(
        sigkill_verified_for_test(pid, start_time),
        "the fake was killed"
    );
    let seen = until_completed(&mut rx).await;
    let completed = completed_turn(&seen);
    assert_eq!(completed["status"], "failed");
    let message = completed["error"]["message"].as_str().unwrap_or("");
    assert!(message.starts_with("claude exited"), "{message}");
    let closed = seen.iter().find_map(|n| match &n.kind {
        PlannerEventKind::Item { phase, params, .. }
            if *phase == ItemPhase::Completed && params["item"]["id"] == "toolu_hold" =>
        {
            Some(params["item"]["status"].clone())
        }
        _ => None,
    });
    assert_eq!(
        closed,
        Some(Value::from("failed")),
        "the open tool call closes before TurnCompleted"
    );
    let outcomes = rig.outcomes().await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["id"], turn.as_str());
    assert_eq!(outcomes[0]["status"], "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cli_lingering_after_its_result_is_stopped_and_the_turn_completes() {
    let rig = Rig::new("linger").await;
    let mut rx = rig.session().subscribe_events();
    rig.session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    let seen = until_completed(&mut rx).await;
    assert_eq!(completed_turn(&seen)["status"], "completed");
    assert!(rig.instructions_files().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_undecodable_line_fails_the_turn_as_protocol() {
    let rig = Rig::new("undecodable").await;
    let mut rx = rig.session().subscribe_events();
    rig.session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    let completed = completed_turn(&until_completed(&mut rx).await);
    assert_eq!(completed["status"], "failed");
    let message = completed["error"]["message"].as_str().unwrap_or("");
    assert!(message.starts_with("protocol"), "{message}");
    assert_eq!(rig.outcomes().await[0]["status"], "failed");
}

/// Count every successful spawn on the session's side (the after-spawn hook runs only then), so a
/// test never depends on a CLI that the refusal stopped having recorded itself first.
fn count_spawns(rig: &Rig) -> Arc<std::sync::atomic::AtomicUsize> {
    let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&spawns);
    rig.session()
        .set_after_spawn_hook_for_test(Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
    spawns
}

fn spawned(spawns: &std::sync::atomic::AtomicUsize) -> usize {
    spawns.load(std::sync::atomic::Ordering::SeqCst)
}

async fn assert_refused_before_ok(
    rig: &Rig,
    input: Vec<InputItem>,
) -> Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
    let mut rx = rig.session().subscribe_events();
    let started = Instant::now();
    let result = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                input,
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(
        rig.outcomes().await.is_empty(),
        "no outcome for a refused turn"
    );
    assert!(
        rig.instructions_files().iter().all(|path| !path.is_file()),
        "no instructions file left: result={result:?}, files={:?}",
        rig.instructions_files()
    );
    let claim =
        calm_truth::db::sqlite::provider_execution::load(rig.repo.pool(), &rig.worker_session_id)
            .await
            .unwrap();
    assert!(
        claim.is_none(),
        "refused start retained {claim:?}; result={result:?}"
    );
    let unsettled: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_executions WHERE settled_at_ms IS NULL")
            .fetch_one(rig.repo.pool())
            .await
            .unwrap();
    assert_eq!(unsettled, 0);
    assert!(rx.try_recv().is_err(), "no notification for a refused turn");
    assert_eq!(rig.session().active_turn_id_for_thread(&rig.thread), None);
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_write_is_refused_before_ok() {
    let rig = Rig::new("stall").await;
    let spawns = count_spawns(&rig);
    let Ok(TurnStartOutcome::Failed { error }) =
        assert_refused_before_ok(&rig, oversized_text()).await
    else {
        panic!("expected failed start failure");
    };
    let error = error.to_string();
    assert!(error.contains("in time"), "{error}");
    assert_eq!(spawned(&spawns), 1, "the fake was spawned");
    assert_eq!(
        rig.session().user_line_writes_for_test(),
        1,
        "the write was tried"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_immediate_exit_is_refused_before_ok_with_no_outcome() {
    let rig = Rig::new("immediate-exit").await;
    let spawns = count_spawns(&rig);
    let Ok(TurnStartOutcome::Failed { error }) =
        assert_refused_before_ok(&rig, oversized_text()).await
    else {
        panic!("expected failed start failure");
    };
    let _ = error;
    assert_eq!(spawned(&spawns), 1, "the fake was spawned");
    assert_eq!(
        rig.session().user_line_writes_for_test(),
        1,
        "the write was tried"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawn_failure_leaves_no_instructions_file() {
    let rig = Rig::new("vanish-after-version").await;
    let mut rx = rig.session().subscribe_events();
    let result = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await;
    match result {
        Ok(TurnStartOutcome::Started { .. }) => {
            let seen = until_completed(&mut rx).await;
            assert_eq!(completed_turn(&seen)["status"], "failed");
            assert_eq!(rig.outcomes().await.len(), 1);
        }
        Ok(TurnStartOutcome::Failed { error }) => {
            let error = error.to_string();
            assert!(
                error.contains("StdinTorn") || error.contains("provider exited before release"),
                "{error}"
            );
            assert!(rig.outcomes().await.is_empty());
        }
        other => panic!("unexpected vanished CLI result: {other:?}"),
    }
    assert!(rig.instructions_files().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seal_after_spawn_stops_the_cli_before_any_input() {
    let rig = Rig::new("hold").await;
    let daemon = Arc::clone(&rig.daemon);
    let thread = rig.thread.clone();
    let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&spawns);
    rig.session()
        .set_after_spawn_hook_for_test(Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            daemon.thread_seals().seal_for_deletion(&thread);
        }));
    let Err(TurnStartFailure::Transient(error)) =
        assert_refused_before_ok(&rig, rig.text("hello")).await
    else {
        panic!("expected transient start failure");
    };
    let error = error.to_string();
    assert!(error.contains("sealed"), "{error}");
    // Both observed on the session's side: the stop may end the fake before it records anything.
    assert_eq!(spawned(&spawns), 1, "the fake was spawned");
    assert_eq!(
        rig.session().user_line_writes_for_test(),
        0,
        "no user line after the seal"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_version_receives_no_user_input() {
    let rig = Rig::new("exit").await;
    std::fs::write(rig.bin("version"), "2.1.279").expect("version");
    let Err(TurnStartFailure::Refused { error, reader }) =
        assert_refused_before_ok(&rig, rig.text("hello")).await
    else {
        panic!("expected refused start failure");
    };
    assert!(reader.contains("2.1.279"), "{reader}");
    let error = error.to_string();
    assert!(error.contains("2.1.279"), "{error}");
    assert!(
        rig.read_bin("spawns").is_none(),
        "no turn process after a wrong version"
    );
    assert_eq!(
        rig.session().user_line_writes_for_test(),
        0,
        "no user input after a wrong version"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_interrupt_then_an_is_error_result_is_interrupted() {
    let rig = Rig::new("interrupt-result-line").await;
    let p_d_result = std::fs::read_to_string(P_D_FIXTURE)
        .expect("P-D fixture")
        .lines()
        .rfind(|line| line.contains("\"type\":\"result\""))
        .expect("P-D result line")
        .to_string();
    let parsed: Value = serde_json::from_str(&p_d_result).expect("json");
    assert_eq!(parsed["is_error"], true, "the P-D result is is_error:true");
    std::fs::write(rig.bin("result_line"), format!("{p_d_result}\n")).expect("result line");
    let mut rx = rig.session().subscribe_events();
    let turn = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    let _outcome = rig.session().turn_interrupt(&rig.thread, &turn).await;
    let completed = completed_turn(&until_completed(&mut rx).await);
    assert_eq!(completed["status"], "interrupted");
    let outcomes = rig.outcomes().await;
    assert_eq!(outcomes[0]["status"], "interrupted");
    let stdin = rig.read_bin("stdin").expect("stdin");
    assert!(stdin.contains("\"subtype\":\"interrupt\""), "{stdin}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_private_instructions_never_reach_a_cmdline() {
    let sentinel = format!("SENTINEL-{}", uuid::Uuid::new_v4().simple());
    let rig = Rig::with_instructions("hold", &format!("Planner. {sentinel}")).await;
    let mut rx = rig.session().subscribe_events();
    let turn = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    let pids = [rig.read_bin("pid").unwrap().trim().parse::<i32>().unwrap()];
    let cmdlines: Vec<String> = pids.iter().map(|pid| cmdline(*pid)).collect();
    let delivered = rig.read_bin("instructions").unwrap_or_default();
    let intents: Vec<String> = sqlx::query_scalar("SELECT intent_json FROM workspace_executions")
        .fetch_all(rig.repo.pool())
        .await
        .unwrap();
    assert_eq!(intents.len(), 1);
    assert!(intents.iter().all(|intent| !intent.contains(&sentinel)));
    rig.assert_execution_journal_excludes(&sentinel);

    let _outcome = rig.session().turn_interrupt(&rig.thread, &turn).await;
    let completed = completed_turn(&until_completed(&mut rx).await);

    assert!(!pids.is_empty(), "the executor-owned CLI was live");
    assert!(
        cmdlines
            .iter()
            .any(|c| c.contains("--append-system-prompt-file")),
        "{cmdlines:?}"
    );
    for line in &cmdlines {
        assert!(!line.contains(&sentinel), "sentinel on a cmdline: {line}");
    }
    assert!(
        delivered.contains(&sentinel),
        "the file carried the instructions"
    );
    assert_eq!(completed["status"], "interrupted");
    assert!(rig.instructions_files().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_interrupts_the_running_turn_and_refuses_the_next() {
    let rig = Rig::new("hold").await;
    let mut rx = rig.session().subscribe_events();
    let turn = rig
        .session()
        .turn_start(
            &rig.thread,
            request(
                rig.text("hello"),
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    rig.session().shutdown().await.expect("shutdown stops");
    let completed = completed_turn(&until_completed(&mut rx).await);

    assert_eq!(completed["id"], turn.as_str());
    assert_eq!(completed["status"], "interrupted");
    assert_eq!(rig.outcomes().await[0]["status"], "interrupted");
    assert!(matches!(
        rig.session()
            .turn_start(
                &rig.thread,
                request(
                    rig.text("later"),
                    &TurnModelSelection::inherit(),
                    calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                    &client_id()
                ),
            )
            .await,
        Err(TurnStartFailure::Transient(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_goes_out_as_base64_and_is_stored_as_its_placeholder() {
    let rig = Rig::new("exit").await;
    let image = rig.dir.path().join(format!("{}.png", uuid::Uuid::new_v4()));
    std::fs::write(&image, b"PNGDATA").expect("image");
    let path = image.to_string_lossy().into_owned();
    let mut rx = rig.session().subscribe_events();
    rig.session()
        .turn_start(
            &rig.thread,
            request(
                vec![
                    InputItem::Text {
                        text: "what colour?".into(),
                    },
                    InputItem::LocalImage { path: path.clone() },
                ],
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &client_id(),
            ),
        )
        .await
        .map(started)
        .expect("turn_start");
    let seen = until_completed(&mut rx).await;

    let line: Value =
        serde_json::from_str(rig.read_bin("stdin").expect("stdin").trim()).expect("user line");
    assert_eq!(
        line["message"]["content"][1],
        serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": "image/png", "data": "UE5HREFUQQ==" },
        })
    );
    let stored = seen
        .iter()
        .find_map(|n| match &n.kind {
            PlannerEventKind::Item { params, .. } if params["item"]["type"] == "userMessage" => {
                Some(params["item"]["content"].clone())
            }
            _ => None,
        })
        .expect("user message item");
    assert_eq!(
        stored[1],
        serde_json::json!({ "type": "localImage", "path": path })
    );
    assert!(!stored.to_string().contains("UE5HREFUQQ=="));
}
