//! Healthy takeover errors preserve the persisted incarnation until settings recover.
use super::*;

#[tokio::test]
async fn healthy_takeover_settings_failure_preserves_running_and_starting_incarnations() {
    for state in [SharedDaemonState::Running, SharedDaemonState::Starting] {
        let root = tempfile::tempdir().unwrap();
        let sock = root.path().join("run/codex-appserver.sock");
        let marker = root.path().join("healthy-daemon-term");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let mut child = Command::new(fake_codex_bin())
            .args(["app-server", "--listen"])
            .arg(format!("unix://{}", sock.display()))
            .env("FAKE_CODEX_SIGTERM_MARKER", &marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as i32;
        let start = wait_for_start_time_and_socket(pid, &sock).await;
        let repo = repo().await;
        persist_running_daemon_with_signature(
            &repo,
            &root,
            pid,
            pid,
            &sock,
            start,
            Some("stale-signature".into()),
        )
        .await;
        if state == SharedDaemonState::Starting {
            repo.db()
                .writer()
                .execute(sqlx::query(
                    "UPDATE codex_backend_daemons SET state='starting' WHERE backend_id='default'",
                ))
                .await
                .unwrap();
        }
        repo.db()
            .writer()
            .execute(sqlx::query(
                "ALTER TABLE settings RENAME TO unavailable_settings",
            ))
            .await
            .unwrap();
        let cfg = quiet_heal_cfg(&root);
        let home = calm_server::shared_codex_home::SharedCodexHome::new(
            cfg.data_dir_resolved().join("codex-home"),
        );
        home.seed().unwrap();
        let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
        for _ in 0..3 {
            assert!(daemon.ensure_running().await.is_err());
            let row = repo.shared_daemon_runtime_get("default").await.unwrap();
            assert_eq!(row.state, state);
            assert_eq!(
                (row.pid, row.pgid, row.process_start_time),
                (Some(pid), Some(pid), Some(start))
            );
            assert!(child.try_wait().unwrap().is_none());
            assert!(
                !marker.exists(),
                "a failed healthy takeover must never signal the daemon"
            );
        }
        repo.db()
            .writer()
            .execute(sqlx::query(
                "ALTER TABLE unavailable_settings RENAME TO settings",
            ))
            .await
            .unwrap();
        daemon.ensure_running().await.unwrap();
        assert_eq!(daemon.status_snapshot().runtime.unwrap().pid, pid);
        assert!(child.try_wait().unwrap().is_none());
        assert!(
            !marker.exists(),
            "repair must adopt the original daemon before any TERM"
        );
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
}

#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
struct OwnedTakeoverChild(std::process::Child);
#[cfg(target_os = "macos")]
impl std::ops::Deref for OwnedTakeoverChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
#[cfg(target_os = "macos")]
impl std::ops::DerefMut for OwnedTakeoverChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
#[cfg(target_os = "macos")]
impl Drop for OwnedTakeoverChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().expect("reap owned takeover child");
    }
}

#[cfg(target_os = "macos")]
struct OwnedSpawnWitness(plugin::proc_identity::MacProcess);
#[cfg(target_os = "macos")]
impl Drop for OwnedSpawnWitness {
    fn drop(&mut self) {
        if let Ok(Some(current)) = plugin::proc_identity::read_macos_process(self.0.pid)
            && current.start_time == self.0.start_time
            && current.pgid == self.0.pgid
        {
            // This is the direct child just returned by this fixture's owner.
            // Reap it even if a later assertion skips production cleanup.
            unsafe {
                libc::kill(self.0.pid, libc::SIGKILL);
                libc::waitpid(self.0.pid, std::ptr::null_mut(), 0);
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn healthy_takeover_runtime_read_failure_then_repair_adopts_original_pid() {
    for state in [SharedDaemonState::Running, SharedDaemonState::Starting] {
        for recorded in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let sock = root.path().join("run/codex-appserver.sock");
            std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
            let marker = root.path().join("healthy-daemon-term");
            let mut child = OwnedTakeoverChild(
                std::process::Command::new(fake_codex_bin())
                    .args(["app-server", "--listen"])
                    .arg(format!("unix://{}", sock.display()))
                    .env("FAKE_CODEX_SIGTERM_MARKER", &marker)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .unwrap(),
            );
            let pid = child.id() as i32;
            let birth = wait_for_start_time_and_socket(pid, &sock).await;
            let url = format!(
                "sqlite://{}?mode=rwc",
                root.path().join("recovery.db").display()
            );
            let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
            persist_running_daemon_with_signature(
                &repo,
                &root,
                pid,
                pid,
                &sock,
                birth,
                Some("stale-signature".into()),
            )
            .await;
            repo.db()
                .writer()
                .execute(
                    sqlx::query(
                        "UPDATE codex_backend_daemons SET state=? WHERE backend_id='default'",
                    )
                    .bind(state.as_db_str()),
                )
                .await
                .unwrap();
            if recorded {
                repo.db().writer().execute(sqlx::query("UPDATE codex_backend_daemons SET recovery_phase='recorded',recovery_id=? WHERE backend_id='default'").bind(calm_truth::db::RecoveryId::fresh().to_string())).await.unwrap();
            }
            let old = repo.shared_daemon_runtime_get("default").await.unwrap();
            let cfg = quiet_heal_cfg(&root);
            let home = calm_server::shared_codex_home::SharedCodexHome::new(
                cfg.data_dir_resolved().join("codex-home"),
            );
            home.seed().unwrap();
            let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
            repo.db()
                .writer()
                .execute(sqlx::query(
                    "ALTER TABLE codex_backend_daemons RENAME TO paused_daemons",
                ))
                .await
                .unwrap();
            let (paused, release) = daemon.pause_failure_finalize_for_test();
            let owner = daemon.clone();
            let start = tokio::spawn(async move { owner.ensure_running().await });
            tokio::time::timeout(Duration::from_secs(10), paused)
                .await
                .unwrap()
                .unwrap();
            repo.db()
                .writer()
                .execute(sqlx::query(
                    "ALTER TABLE paused_daemons RENAME TO codex_backend_daemons",
                ))
                .await
                .unwrap();
            release.send(()).unwrap();
            assert!(start.await.unwrap().is_err());
            let saved = repo.shared_daemon_runtime_get("default").await.unwrap();
            assert_eq!(saved.state, state);
            assert_eq!(saved.recovery, old.recovery);
            assert_eq!(
                (saved.pid, saved.pgid, saved.process_start_time),
                (Some(pid), Some(pid), Some(birth))
            );
            assert!(!marker.exists());
            daemon.ensure_running().await.unwrap();
            assert_eq!(daemon.status_snapshot().runtime.unwrap().pid, pid);
            assert!(child.try_wait().unwrap().is_none());
            assert!(!marker.exists());
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn settled_scope_stale_socket_reopens_and_respawns() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let mut old = OwnedTakeoverChild(
        std::process::Command::new(fake_codex_bin())
            .args(["app-server", "--listen"])
            .arg(format!("unix://{}", sock.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let pid = old.id() as i32;
    let birth = wait_for_start_time_and_socket(pid, &sock).await;
    let url = format!(
        "sqlite://{}?mode=rwc",
        root.path().join("recovery.db").display()
    );
    let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    persist_running_daemon_with_signature(&repo, &root, pid, pid, &sock, birth, None).await;
    let id = calm_truth::db::RecoveryId::fresh();
    repo.db().writer().execute(sqlx::query("UPDATE codex_backend_daemons SET state='failed',recovery_phase='recorded',recovery_id=? WHERE backend_id='default'").bind(id.to_string())).await.unwrap();
    old.kill().unwrap();
    old.wait().unwrap();
    assert!(sock.exists());
    repo.db().close().await;
    let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    let mut unrelated = OwnedTakeoverChild(
        std::process::Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let wrapper = root.path().join("counted-codex");
    std::fs::write(
        &wrapper,
        format!(
            r#"#!/usr/bin/python3
import os,pathlib,sys
with pathlib.Path(__file__).with_suffix('.started').open('a') as f:
    f.write(str(os.getpid()) + '\n')
binary={:?}
os.execv(binary,[binary,*sys.argv[1:]])
"#,
            fake_codex_bin()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut cfg = quiet_heal_cfg(&root);
    cfg.codex_bin = wrapper.to_str().unwrap().into();
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
    daemon.ensure_running().await.unwrap();
    let runtime = daemon.status_snapshot().runtime.unwrap();
    let _owned = OwnedSpawnWitness(
        plugin::proc_identity::read_macos_process(runtime.pid)
            .unwrap()
            .unwrap(),
    );

    daemon.ensure_running().await.unwrap();
    let spawns = std::fs::read_to_string(wrapper.with_extension("started")).unwrap();
    assert_eq!(
        spawns.lines().count(),
        1,
        "reopened stale endpoint must cause exactly one real spawn"
    );
    assert!(unrelated.try_wait().unwrap().is_none());
    let row = repo.shared_daemon_runtime_get("default").await.unwrap();
    assert_eq!(row.state, SharedDaemonState::Running);
    assert_eq!(row.pid, Some(spawns.trim().parse::<i32>().unwrap()));
    assert_ne!(row.recovery.scope_id(), Some(id));
    assert_ne!(row.pid, Some(pid));
    assert!(sock.exists());
    // Use the production replacement lifecycle to reap the new owned child.
    std::fs::write(
        root.path().join("codex-home/config.toml"),
        "[mcp_servers.evil]\ncommand='/missing'\n",
    )
    .unwrap();
    assert!(
        daemon
            .transition_replace_for_test("fixture cleanup", ReplacePrecondition::Always)
            .await
            .is_err()
    );
    assert!(unrelated.try_wait().unwrap().is_none());
}
