//! #1791 PR2b: a real `ClaudePlannerSession` driving a fake `claude` (bash) through every turn
//! path — exit, kill, linger, undecodable, stalled write, immediate exit — plus the submission
//! contract's refusals and the stop seam. See `claude_planner_session_fixture.rs` for the rig.

use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::claude_planner::session::SettlePause;
use calm_server::claude_planner::stop::{
    SeamPolicy, clear_claude_planner_stop_failure_for_test, fail_claude_planner_stop_for_test,
    sigkill_verified_for_test, stop, sweep,
};
use calm_server::codex_appserver::{InputItem, Notification};
use calm_server::proc_identity::read_proc_start_time;
use serde_json::Value;
use tokio::sync::Notify;

use super::claude_planner_session_fixture::{
    P_D_FIXTURE, Rig, alive, client_id, cmdline, completed_turn, until_completed, wait_for_file,
};

/// Larger than a pipe buffer, so writing it cannot complete unless the CLI reads it.
fn oversized_text() -> Vec<InputItem> {
    vec![InputItem::Text {
        text: "x".repeat(256 * 1024),
    }]
}

fn item_types(seen: &[Notification]) -> Vec<(String, String)> {
    seen.iter()
        .filter_map(|n| match n {
            Notification::Item { method, params } => Some((
                method.clone(),
                params["item"]["type"].as_str().unwrap_or("").to_string(),
            )),
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
    let mut rx = rig.session().subscribe_notifications();

    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
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
    let mut before = Vec::new();
    while let Ok(n) = rx.try_recv() {
        before.push(n);
    }
    assert!(
        !before
            .iter()
            .any(|n| matches!(n, Notification::TurnCompleted { .. })),
        "TurnCompleted must not precede the durable outcome: {before:?}"
    );
    assert!(matches!(
        before.first(),
        Some(Notification::TurnStarted { .. })
    ));
    assert!(
        rig.instructions_files().is_empty(),
        "file removed before emit"
    );
    assert!(
        rig.marked_pids().is_empty(),
        "no marked process before emit"
    );

    pause.release.notify_one();
    let seen = until_completed(&mut rx).await;
    let completed = completed_turn(&seen);
    assert_eq!(completed["id"], turn.as_str());
    assert_eq!(completed["status"], "completed");
    let items = item_types(&before);
    assert!(items.contains(&("item/completed".into(), "userMessage".into())));
    assert!(items.contains(&("item/completed".into(), "agentMessage".into())));
    assert!(before.iter().any(
        |n| matches!(n, Notification::Other { method, .. } if method == "thread/tokenUsage/updated")
    ));
    assert_eq!(rig.session().active_turn_id_for_thread(&rig.thread), None);

    let argv = rig.read_bin("argv").expect("argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv
        .iter()
        .position(|a| *a == "--session-id")
        .expect("new session");
    assert_eq!(argv[at + 1], rig.thread);
    assert!(!argv.contains(&"--resume"));
    assert_env_is_the_allowlist(&rig);

    // The first init bound the session: the next turn resumes it.
    let second = rig
        .session()
        .turn_start(&rig.thread, rig.text("again"), &client_id())
        .await
        .expect("second turn_start");
    pause.release.notify_one();
    let seen = until_completed(&mut rx).await;
    assert_eq!(completed_turn(&seen)["id"], second.as_str());
    let argv = rig.read_bin("argv").expect("argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv.iter().position(|a| *a == "--resume").expect("resume");
    assert_eq!(argv[at + 1], rig.thread);
}

fn assert_env_is_the_allowlist(rig: &Rig) {
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
        "DISABLE_AUTOUPDATER", // bash's own
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
    assert_eq!(get("NEIGE_CLAUDE_PLANNER"), Some(rig.marker().as_str()));
    assert_eq!(get("NEIGE_MCP_TOKEN"), Some("tok-rig"));
    assert_eq!(get("DISABLE_AUTOUPDATER"), Some("1"));
    assert_eq!(
        get("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from),
        Some(rig.host.config.config_dir.clone())
    );
    assert!(get("PATH").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_surviving_setsid_child_of_a_successful_exit_is_gone_before_turn_completed() {
    let rig = Rig::new("exit-with-orphan").await;
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    let seen = until_completed(&mut rx).await;
    let orphan_alive = rig
        .read_bin("orphan")
        .and_then(|pid| pid.trim().parse::<i32>().ok())
        .map(alive);
    let marked = rig.marked_pids();

    assert_eq!(completed_turn(&seen)["status"], "completed");
    assert_eq!(
        orphan_alive,
        Some(false),
        "the setsid child outlived settlement"
    );
    assert!(
        marked.is_empty(),
        "marked processes at TurnCompleted: {marked:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_cli_settles_failed_with_its_exit_status() {
    let rig = Rig::new("hold").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    // Kill only once the tool call is open, so settlement has an item to close.
    loop {
        let next = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the tool call starts")
            .expect("notification");
        if matches!(&next, Notification::Item { method, params }
            if method == "item/started" && params["item"]["id"] == "toolu_hold")
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
    let closed = seen.iter().find_map(|n| match n {
        Notification::Item { method, params }
            if method == "item/completed" && params["item"]["id"] == "toolu_hold" =>
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
    assert!(rig.marked_pids().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cli_lingering_after_its_result_is_stopped_and_the_turn_completes() {
    let rig = Rig::new("linger").await;
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    let seen = until_completed(&mut rx).await;

    assert_eq!(completed_turn(&seen)["status"], "completed");
    assert!(
        rig.marked_pids().is_empty(),
        "the lingering CLI was stopped"
    );
    assert!(rig.instructions_files().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_undecodable_line_fails_the_turn_as_protocol() {
    let rig = Rig::new("undecodable").await;
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    let completed = completed_turn(&until_completed(&mut rx).await);

    assert_eq!(completed["status"], "failed");
    let message = completed["error"]["message"].as_str().unwrap_or("");
    assert!(message.starts_with("protocol"), "{message}");
    assert_eq!(rig.outcomes().await[0]["status"], "failed");
    assert!(rig.marked_pids().is_empty());
}

async fn assert_refused_before_ok(rig: &Rig, input: Vec<InputItem>) -> String {
    let mut rx = rig.session().subscribe_notifications();
    let started = Instant::now();
    let error = rig
        .session()
        .turn_start(&rig.thread, input, &client_id())
        .await
        .expect_err("turn_start must refuse");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(
        rig.outcomes().await.is_empty(),
        "no outcome for a refused turn"
    );
    assert!(
        rig.instructions_files().is_empty(),
        "no instructions file left"
    );
    assert!(rig.marked_pids().is_empty(), "no marked process left");
    assert!(rx.try_recv().is_err(), "no notification for a refused turn");
    assert_eq!(rig.session().active_turn_id_for_thread(&rig.thread), None);
    error.to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_write_is_refused_before_ok() {
    let rig = Rig::new("stall").await;
    let error = assert_refused_before_ok(&rig, oversized_text()).await;
    assert!(error.contains("in time"), "{error}");
    assert!(rig.read_bin("spawns").is_some(), "the fake was spawned");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_immediate_exit_is_refused_before_ok_with_no_outcome() {
    let rig = Rig::new("immediate-exit").await;
    assert_refused_before_ok(&rig, oversized_text()).await;
    assert!(rig.read_bin("spawns").is_some(), "the fake was spawned");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawn_failure_leaves_no_instructions_file() {
    let rig = Rig::new("vanish-after-version").await;
    let error = assert_refused_before_ok(&rig, rig.text("hello")).await;
    assert!(error.contains("spawn"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seal_after_spawn_stops_the_cli_before_any_input() {
    let rig = Rig::new("hold").await;
    let daemon = Arc::clone(&rig.daemon);
    let thread = rig.thread.clone();
    rig.session()
        .set_after_spawn_hook_for_test(Arc::new(move || {
            daemon.seal_turn_thread_for_deletion(&thread);
        }));
    let error = assert_refused_before_ok(&rig, rig.text("hello")).await;
    assert!(error.contains("sealed"), "{error}");
    assert!(rig.read_bin("spawns").is_some(), "the fake was spawned");
    assert!(
        rig.read_bin("stdin").is_none(),
        "no user line after the seal"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_version_receives_no_user_input() {
    let rig = Rig::new("exit").await;
    std::fs::write(rig.bin("version"), "2.1.279").expect("version");
    let error = assert_refused_before_ok(&rig, rig.text("hello")).await;
    assert!(error.contains("2.1.279"), "{error}");
    assert!(
        rig.read_bin("spawns").is_none(),
        "no turn process after a wrong version"
    );
    assert!(
        rig.read_bin("stdin").is_none(),
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
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
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
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    let pids = rig.marked_pids();
    let cmdlines: Vec<String> = pids.iter().map(|pid| cmdline(*pid)).collect();
    let delivered = rig.read_bin("instructions").unwrap_or_default();
    rig.session()
        .turn_interrupt(&rig.thread, &turn)
        .await
        .expect("interrupt");
    let completed = completed_turn(&until_completed(&mut rx).await);

    assert!(!pids.is_empty(), "the CLI was live and marked");
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
async fn the_stop_seam_fails_without_signalling_until_cleared() {
    let rig = Rig::new("exit").await;
    let mut marked = std::process::Command::new("/bin/bash")
        .args(["-c", "exec sleep 300"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("NEIGE_CLAUDE_PLANNER", rig.marker())
        .spawn()
        .expect("spawn marked sleep");
    let pid = marked.id() as i32;
    fail_claude_planner_stop_for_test(&rig.worker_session_id);

    let stopped = stop(&rig.host.instance, &rig.worker_session_id).await;
    let scoped = sweep(
        &rig.host.instance,
        &[rig.worker_session_id.as_str()],
        SeamPolicy::Consult,
    )
    .await;
    let alive_while_armed = alive(pid);
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await;
    clear_claude_planner_stop_failure_for_test(&rig.worker_session_id);
    let cleared = stop(&rig.host.instance, &rig.worker_session_id).await;
    let _ = marked.wait();

    assert!(
        stopped.is_err() && scoped.is_err(),
        "armed seam fails the stop"
    );
    assert!(alive_while_armed, "an armed seam signals nothing");
    assert!(turn.is_err(), "turn_start's own stop fails closed");
    assert!(
        rig.read_bin("spawns").is_none(),
        "no spawn while the seam is armed"
    );
    assert!(cleared.is_ok(), "cleared: {cleared:?}");
    assert!(!alive(pid), "the cleared stop ends the marked process");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_boot_sweep_ignores_the_seam() {
    let rig = Rig::new("exit").await;
    let mut marked = std::process::Command::new("/bin/bash")
        .args(["-c", "exec sleep 300"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("NEIGE_CLAUDE_PLANNER", rig.marker())
        .spawn()
        .expect("spawn marked sleep");
    let pid = marked.id() as i32;
    fail_claude_planner_stop_for_test(&rig.worker_session_id);
    let swept = sweep(
        &rig.host.instance,
        &[rig.worker_session_id.as_str()],
        SeamPolicy::Ignore,
    )
    .await;
    clear_claude_planner_stop_failure_for_test(&rig.worker_session_id);
    let _ = marked.wait();

    assert!(swept.is_ok(), "{swept:?}");
    assert!(!alive(pid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_interrupts_the_running_turn_and_refuses_the_next() {
    let rig = Rig::new("hold").await;
    let mut rx = rig.session().subscribe_notifications();
    let turn = rig
        .session()
        .turn_start(&rig.thread, rig.text("hello"), &client_id())
        .await
        .expect("turn_start");
    wait_for_file(&rig.bin("stdin")).await;
    rig.session().shutdown().await.expect("shutdown stops");
    let completed = completed_turn(&until_completed(&mut rx).await);

    assert_eq!(completed["id"], turn.as_str());
    assert_eq!(completed["status"], "interrupted");
    assert_eq!(rig.outcomes().await[0]["status"], "interrupted");
    assert!(rig.marked_pids().is_empty());
    assert!(
        rig.session()
            .turn_start(&rig.thread, rig.text("later"), &client_id())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_goes_out_as_base64_and_is_stored_as_its_placeholder() {
    let rig = Rig::new("exit").await;
    let image = rig.dir.path().join(format!("{}.png", uuid::Uuid::new_v4()));
    std::fs::write(&image, b"PNGDATA").expect("image");
    let path = image.to_string_lossy().into_owned();
    let mut rx = rig.session().subscribe_notifications();
    rig.session()
        .turn_start(
            &rig.thread,
            vec![
                InputItem::Text {
                    text: "what colour?".into(),
                },
                InputItem::LocalImage { path: path.clone() },
            ],
            &client_id(),
        )
        .await
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
        .find_map(|n| match n {
            Notification::Item { params, .. } if params["item"]["type"] == "userMessage" => {
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
