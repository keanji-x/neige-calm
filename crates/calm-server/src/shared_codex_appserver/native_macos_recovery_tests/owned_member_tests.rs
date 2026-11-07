//! Owned cleanup must keep the first signal's membership authority on every exit.
use super::spawn_obligation_tests::{SpawnFixture, pause_acquisition, wait_reaped};
use super::*;
use calm_truth::db::DaemonRecovery;
use plugin::proc_identity::{MacProcess, read_macos_process};

async fn marker(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("real producer marker");
}

async fn member(s: &SpawnFixture, label: &str, pgid: i32) -> Owned {
    let ready = s.f.root.path().join(label);
    let child = Owned(
        Command::new("/bin/sh")
            .args([
                "-c",
                "trap '' TERM; echo ready > \"$1\"; exec sleep 60",
                "sh",
            ])
            .arg(&ready)
            .process_group(pgid)
            .spawn()
            .unwrap(),
    );
    marker(&ready).await;
    let fact = read_macos_process(child.pid()).unwrap().unwrap();
    assert_eq!(fact.pgid, pgid);
    assert!(!fact.is_zombie && fact.start_time != 0);
    eprintln!("{label}: {fact:?}");
    child
}

async fn assert_fenced_then_empty(s: &mut SpawnFixture, mut late: Owned, original: MacProcess) {
    assert!(
        late.alive(),
        "late member received an unauthorized group signal"
    );
    assert!(s.f.unrelated.alive());
    wait_reaped(original.pid).await;
    let saved = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert!(matches!(saved.recovery, DaemonRecovery::Recorded { .. }));
    assert_eq!(
        (saved.pid, saved.pgid),
        (Some(original.pid), Some(original.pgid))
    );
    assert!(s.f.paths.paths().socket.exists());
    // Remove only the executable: further entry cannot silently create a successor.
    std::fs::remove_file(&s.f.config.codex_bin).unwrap();
    let count = s.spawn_count();
    for _ in 0..2 {
        assert!(s.f.daemon.start_or_takeover().await.is_err());
        let retained = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
        assert_eq!(retained.recovery, saved.recovery);
        assert_eq!(retained.pid, saved.pid);
        assert!(s.f.paths.paths().socket.exists());
        assert!(late.alive() && s.f.unrelated.alive());
        assert_eq!(s.spawn_count(), count);
    }
    // The fixture owns this child separately; its independent exit releases the scope.
    late.0.kill().unwrap();
    late.0.wait().unwrap();
    assert!(macos_process::group_empty(original.pgid));
    assert!(s.f.daemon.start_or_takeover().await.is_err()); // missing executable
    let cleared = s.f.repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(cleared.recovery, DaemonRecovery::Clear);
    assert!(cleared.pid.is_none() && cleared.pgid.is_none());
    assert!(!s.f.paths.paths().socket.exists());
    assert!(s.f.unrelated.alive());
}

#[tokio::test]
async fn native_macos_recovery_owned_spawn_failure_never_signals_late_member() {
    let mut s = SpawnFixture::new().await;
    std::fs::write(s.f.root.path().join("owned-producer.hold-term"), "").unwrap();
    let (acquired, release) = pause_acquisition(&s.f.daemon);
    let owner = s.f.daemon.clone();
    let caller = tokio::spawn(async move { owner.start_or_takeover().await });
    let pid = tokio::time::timeout(Duration::from_secs(10), acquired)
        .await
        .unwrap()
        .unwrap();
    let original = s.observed();
    assert_eq!(original.pid, pid);
    let mut authorized = member(&s, "authorized-ready", pid).await;
    release.send(()).unwrap();
    marker(&s.f.root.path().join("owned-producer.term-seen")).await;
    let late = member(&s, "late-ready", pid).await;
    std::fs::write(s.f.root.path().join("owned-producer.release-term"), "").unwrap();
    assert!(caller.await.unwrap().is_err());
    assert!(
        !authorized.alive(),
        "original authorized member survived cleanup"
    );
    assert_fenced_then_empty(&mut s, late, original).await;
}

#[tokio::test]
async fn native_macos_recovery_owned_replacement_never_signals_late_member() {
    let mut s = SpawnFixture::new().await;
    std::fs::write(s.f.root.path().join("owned-producer.hold-term"), "").unwrap();
    let mut command = tokio::process::Command::new(&s.f.config.codex_bin);
    command
        .args(["app-server", "--listen"])
        .arg(format!("unix://{}", s.f.paths.paths().socket.display()))
        .process_group(0);
    let child = command.spawn().unwrap();
    let pid = child.id().unwrap() as i32;
    s.capture_owned(pid);
    marker(&s.f.paths.paths().socket).await;
    let original = s.observed();
    let mut authorized = member(&s, "authorized-ready", pid).await;
    let (client, _peer, forwarder) = native_owner_tests::connect_unpublished(&s.f.daemon).await;
    let runtime = SharedDaemonRuntime {
        pid,
        pgid: pid,
        process_start_time: original.start_time,
        boot_id: read_boot_id().unwrap(),
        started_at: now_ms(),
    };
    // Persist the actual owned identity through the existing fixture's DB entry.
    let boot = runtime.boot_id.clone();
    s.f.repo
        .shared_daemon_runtime_set(
            "default",
            SharedCodexDaemonUpdate {
                recovery: calm_truth::db::RecoveryWrite::Legacy,
                state: SharedDaemonState::Running,
                pid: Some(pid),
                pgid: Some(pid),
                paths_are_legacy_display: false,
                sock_path: Some(s.f.paths.paths().socket.as_os_str().as_bytes().to_vec()),
                codex_home_path: Some(s.f.paths.paths().home.as_os_str().as_bytes().to_vec()),
                process_start_time: Some(original.start_time),
                boot_id: Some(boot),
                started_at: Some(runtime.started_at),
                last_error: None,
                increment_restart_count: false,
                daemon_env_signature: None,
            },
        )
        .await
        .unwrap();
    s.f.daemon
        .start_new_process_typestate(|_| async move {
            Ok(LaunchedSharedDaemon {
                recovery: DaemonRecovery::LegacyUntracked,
                child: Some(child),
                client,
                runtime,
                replay: ConnectionReplay::Complete,
                watcher: SupervisorWatcher {
                    kind: WatcherKind::SpawnedChild,
                    handle: forwarder,
                },
            })
        })
        .await
        .unwrap();
    let owner = s.f.daemon.clone();
    let caller = tokio::spawn(async move {
        owner
            .transition_replace("owned fixture", ReplacePrecondition::Always, true, None)
            .await
    });
    marker(&s.f.root.path().join("owned-producer.term-seen")).await;
    let late = member(&s, "late-ready", pid).await;
    std::fs::write(s.f.root.path().join("owned-producer.release-term"), "").unwrap();
    assert!(caller.await.unwrap().is_err());
    assert!(
        !authorized.alive(),
        "original authorized member survived cleanup"
    );
    assert_fenced_then_empty(&mut s, late, original).await;
}

#[tokio::test]
async fn native_macos_recovery_reaped_child_drop_cannot_signal_original_group() {
    let mut s = SpawnFixture::new().await;
    let mut command = tokio::process::Command::new("/bin/sleep");
    command.arg("60").process_group(0).kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    let pid = child.id().unwrap() as i32;
    s.capture_owned(pid);
    let mut member = Owned(
        Command::new("/usr/bin/python3")
            .args([
                "-c",
                "print('ready',flush=True); input(); print('alive',flush=True)",
            ])
            .process_group(pid)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    use std::io::{BufRead, Write};
    let mut replies = std::io::BufReader::new(member.0.stdout.take().unwrap());
    let mut reply = String::new();
    replies.read_line(&mut reply).unwrap();
    assert_eq!(reply, "ready\n");
    let original = read_macos_process(member.pid()).unwrap().unwrap();
    assert_eq!(original.pgid, pid);
    child.kill().await.unwrap();
    assert!(child.id().is_none());
    drop_spawned_child_guard_for_test(child, pid);
    // A real reply after Drop proves the TERM-cooperative member survived it.
    member
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"probe\n")
        .unwrap();
    reply.clear();
    replies.read_line(&mut reply).unwrap();
    assert_eq!(reply, "alive\n");
    assert!(s.f.unrelated.alive());
}
