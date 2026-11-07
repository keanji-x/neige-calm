use super::*;

pub(super) fn config(sock: &Path, id: &str, script: &str) -> RendererConfig {
    RendererConfig {
        terminal_id: id.into(),
        cols: 80,
        rows: 24,
        buffer_bytes: 1 << 20,
        terminal_fg: (216, 219, 226),
        terminal_bg: (15, 20, 24),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        envs: vec![("PATH".into(), "/usr/bin:/bin".into())],
        cwd: workspace_root().display().to_string(),
        supervisor_sock: sock.into(),
    }
}
#[cfg(target_os = "linux")]
fn sockets(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .filter(|p| p.to_string_lossy().starts_with("socket:"))
        .count()
}

#[cfg(target_os = "macos")]
fn sockets(pid: u32) -> usize {
    let mut fds = vec![unsafe { std::mem::zeroed::<libc::proc_fdinfo>() }; 4096];
    let bytes = std::mem::size_of_val(fds.as_slice());
    // libproc reports byte counts into this aligned, owned descriptor buffer.
    let used = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            bytes as i32,
        )
    };
    assert!(used > 0 && (used as usize) < bytes);
    fds.truncate(used as usize / std::mem::size_of::<libc::proc_fdinfo>());
    fds.iter()
        .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_SOCKET as u32)
        .count()
}

#[tokio::test]
async fn fd_resources_natural_exit_keeps_screen_and_peer_releases_connection() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&socket).await;
    let registry = TerminalRendererRegistry::new();
    let peer_id = Uuid::new_v4().to_string();
    let peer = registry
        .ensure(config(
            &socket,
            &peer_id,
            "printf ready; read value; printf '%s' \"$value\"; sleep 600",
        ))
        .await
        .unwrap();
    let (peer_tx, mut peer_rx, peer_pump) = spawn_client_pump(peer.clone());
    send_client_hello(&peer_tx, &peer_id).await;
    assert!(matches!(
        peer_rx.recv().await,
        Some(DaemonMsg::ServerHello { .. })
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let baseline = (
        sockets(std::process::id()),
        sockets(supervisor.id().unwrap()),
    );
    let mut entries = Vec::new();
    for _ in 0..8 {
        let id = Uuid::new_v4().to_string();
        let entry = registry
            .ensure(config(&socket, &id, "printf retained-screen; exit 7"))
            .await
            .unwrap();
        assert_eq!(
            timeout(LIVENESS_BUDGET, entry.wait_exited()).await.unwrap(),
            Some(Some(7))
        );
        assert!(entry.wait_exit_persisted_for_test(LIVENESS_BUDGET).await);
        entries.push((id, entry));
    }
    // This observes the real writer receiver closing on both Linux and Darwin.
    // Holding RendererEntry/sender Arcs must not keep the UDS writer alive.
    let released = timeout(Duration::from_secs(5), async {
        for (_, entry) in &entries {
            entry.handle.supervisor_tx.closed().await;
        }
    })
    .await;
    for (id, entry) in &entries {
        assert!(entry.observable());
        assert!(registry.get(id).is_some());
        let (tx, mut rx, pump) = spawn_client_pump(entry.clone());
        send_client_hello(&tx, id).await;
        let hello = timeout(LIVENESS_BUDGET, rx.recv()).await.unwrap().unwrap();
        let DaemonMsg::ServerHello { snapshot, .. } = hello else {
            panic!("{hello:?}")
        };
        assert!(String::from_utf8_lossy(&snapshot.data).contains("retained-screen"));
        assert_eq!(wait_for_daemon_terminal_exited(&mut rx).await.code, Some(7));
        drop(tx);
        timeout(LIVENESS_BUDGET, pump)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert!(!peer.handle.supervisor_tx.is_closed());
    peer_tx
        .send(ClientMsg::Input {
            data: b"alive\n".to_vec(),
            input_seq: 1,
        })
        .await
        .unwrap();
    wait_for_input_ack(&mut peer_rx, 1).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let after = loop {
        let after = (
            sockets(std::process::id()),
            sockets(supervisor.id().unwrap()),
        );
        if (after.0 <= baseline.0 + 1 && after.1 <= baseline.1 + 1)
            || tokio::time::Instant::now() >= deadline
        {
            break after;
        }
        // The peer runtime may not have consumed the final socket EOF yet.
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    // Explicit Stop still has its independent signal connection after natural writer closure.
    for (_, entry) in &entries {
        entry
            .shutdown_signal(calm_session::control::ProcSignal::Term)
            .await;
        entry
            .shutdown_signal(calm_session::control::ProcSignal::Kill)
            .await;
    }
    peer.shutdown_signal(calm_session::control::ProcSignal::Term)
        .await;
    peer.shutdown_signal(calm_session::control::ProcSignal::Kill)
        .await;
    assert!(
        timeout(LIVENESS_BUDGET, peer.wait_exited())
            .await
            .unwrap()
            .is_some()
    );
    drop(peer_tx);
    timeout(LIVENESS_BUDGET, peer_pump)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for (id, _) in &entries {
        registry.drop_entry(id).await;
    }
    registry.drop_entry(&peer_id).await;
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
    assert!(
        released.is_ok(),
        "natural Exited retained the control writer/UDS"
    );
    assert!(
        after.0 <= baseline.0 + 1 && after.1 <= baseline.1 + 1,
        "control sockets grew with retained history: before={baseline:?}, after={after:?}"
    );
}

#[tokio::test]
async fn fd_resources_output_disconnect_does_not_complete_writer_or_exit() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&socket).await;
    let registry = TerminalRendererRegistry::new();
    let id = Uuid::new_v4().to_string();
    let entry = registry
        .ensure(config(&socket, &id, "exec sleep 3"))
        .await
        .unwrap();
    supervisor.kill().await.unwrap();
    supervisor.wait().await.unwrap();
    // Reader completion on EOF/error drops the exit sender without sending an exit fact.
    assert_eq!(
        timeout(LIVENESS_BUDGET, entry.wait_exited()).await.unwrap(),
        None
    );
    assert!(entry.exit.lock().unwrap().is_none());
    assert!(
        !entry
            .wait_exit_persisted_for_test(Duration::from_secs(1))
            .await
    );
    let closed = timeout(
        Duration::from_millis(100),
        entry.handle.supervisor_tx.closed(),
    )
    .await;
    registry.drop_entry(&id).await;
    assert!(
        closed.is_err(),
        "output disconnection was treated as a natural exit"
    );
}

// ps has the same non-zombie observation contract on Linux and Darwin. kill(pid, 0)
// also succeeds for zombies, which have already released their descriptors.
pub(super) async fn member_alive(pid: i32) -> bool {
    let status = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .await
        .unwrap();
    let status = String::from_utf8(status.stdout).unwrap();
    let status = status.trim();
    !status.is_empty() && !status.starts_with('Z') && !status.starts_with('X')
}

#[tokio::test]
async fn fd_resources_owner_kill_finishes_after_leader_exit_and_pump_end() {
    use calm_session::control::ProcSignal;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&socket).await;
    let registry = TerminalRendererRegistry::new();
    let id = Uuid::new_v4().to_string();
    let pid_file = temp.path().join("member.pid");
    let baseline = (
        sockets(std::process::id()),
        sockets(supervisor.id().unwrap()),
    );
    let entry = registry.ensure(config(&socket, &id, &format!(
        "trap 'exit 0' HUP; sh -c 'trap \"\" HUP TERM; echo $$ > \"$1\"; exec sleep 600' sh '{}' & printf retained-stop-screen; while :; do sleep 1; done",
        pid_file.display()
    ))).await.unwrap();
    let (tx, mut rx, pump) = spawn_client_pump(entry.clone());
    let pump_abort = pump.abort_handle();
    let outcome = async {
        let pid = timeout(Duration::from_secs(5), wait_for_pid_file(&pid_file)).await?;
        anyhow::ensure!(member_alive(pid).await, "member must be live before Kill");
        timeout(Duration::from_secs(5), async {
            loop {
                let painted = {
                    let plane = entry.handle.render_plane.lock().unwrap();
                    String::from_utf8_lossy(&plane.transcript().snapshot())
                        .contains("retained-stop-screen")
                };
                if painted {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        send_client_hello(&tx, &id).await;
        anyhow::ensure!(matches!(
            rx.recv().await,
            Some(DaemonMsg::ServerHello {
                client_role: calm_session::Role::Owner,
                ..
            })
        ));
        tx.send(ClientMsg::Kill).await?;
        let exit = timeout(Duration::from_secs(5), entry.wait_exited()).await?;
        anyhow::ensure!(exit == Some(Some(0)), "leader must exit on HUP: {exit:?}");
        anyhow::ensure!(
            entry
                .wait_exit_persisted_for_test(Duration::from_secs(5))
                .await
        );
        // The leader has exited but the HUP-ignoring member still holds the slave.
        anyhow::ensure!(
            member_alive(pid).await,
            "must exercise leader-exit before group-exit"
        );
        drop(tx);
        timeout(Duration::from_secs(5), pump).await???;
        // No pump remains to submit the second half of Stop.
        timeout(Duration::from_secs(5), async {
            while member_alive(pid).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("delayed KILL was lost after leader exit"))?;
        timeout(Duration::from_secs(5), entry.handle.supervisor_tx.closed()).await?;
        timeout(Duration::from_secs(5), async {
            while sockets(std::process::id()) > baseline.0
                || sockets(supervisor.id().unwrap()) > baseline.1
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("completed Stop retained control sockets"))?;
        anyhow::ensure!(entry.observable());
        let (late_tx, mut late_rx, late_pump) = spawn_client_pump(entry.clone());
        send_client_hello(&late_tx, &id).await;
        let Some(DaemonMsg::ServerHello { snapshot, .. }) = late_rx.recv().await else {
            anyhow::bail!("missing late hello");
        };
        anyhow::ensure!(String::from_utf8_lossy(&snapshot.data).contains("retained-stop-screen"));
        anyhow::ensure!(wait_for_daemon_terminal_exited(&mut late_rx).await.code == Some(0));
        drop(late_tx);
        timeout(Duration::from_secs(5), late_pump).await???;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    pump_abort.abort();
    // Always clean the isolated group through its existing owner, including on a red run.
    entry.shutdown_signal(ProcSignal::Kill).await;
    registry.drop_entry(&id).await;
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[tokio::test]
async fn fd_resources_teardown_cancels_stop_before_same_id_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("supervisor.sock");
    let mut supervisor = spawn_proc_supervisor(&socket).await;
    let registry = TerminalRendererRegistry::new();
    let id = Uuid::new_v4().to_string();
    let ready = temp.path().join("ready.pid");
    let old = registry
        .ensure(config(
            &socket,
            &id,
            &format!(
                "trap 'exit 0' HUP; echo $$ > '{}'; while :; do sleep 1; done",
                ready.display()
            ),
        ))
        .await
        .unwrap();
    let (tx, mut rx, pump) = spawn_client_pump(old.clone());
    let pump_abort = pump.abort_handle();
    let outcome = async {
        timeout(Duration::from_secs(5), wait_for_pid_file(&ready)).await?;
        send_client_hello(&tx, &id).await;
        anyhow::ensure!(matches!(
            rx.recv().await,
            Some(DaemonMsg::ServerHello { .. })
        ));
        tx.send(ClientMsg::Kill).await?;
        anyhow::ensure!(timeout(Duration::from_secs(5), old.wait_exited()).await? == Some(Some(0)));
        // Teardown completes the original group via the direct entry point and cancels
        // the writer before ensure is allowed to reuse this terminal identity.
        registry.drop_entry(&id).await;
        drop(tx);
        timeout(Duration::from_secs(5), pump).await???;
        timeout(Duration::from_secs(5), old.handle.supervisor_tx.closed()).await?;
        let replacement = registry.ensure(config(&socket, &id, "exec cat")).await?;
        anyhow::ensure!(!Arc::ptr_eq(&old, &replacement));
        tokio::time::sleep(Duration::from_secs(3)).await;
        anyhow::ensure!(
            replacement.exit.lock().unwrap().is_none(),
            "old Stop killed replacement"
        );
        let (new_tx, mut new_rx, new_pump) = spawn_client_pump(replacement.clone());
        send_client_hello(&new_tx, &id).await;
        anyhow::ensure!(matches!(
            new_rx.recv().await,
            Some(DaemonMsg::ServerHello { .. })
        ));
        new_tx
            .send(ClientMsg::Input {
                data: b"replacement-alive\n".to_vec(),
                input_seq: 1,
            })
            .await?;
        timeout(Duration::from_secs(5), wait_for_input_ack(&mut new_rx, 1)).await?;
        drop(new_tx);
        timeout(Duration::from_secs(5), new_pump).await???;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    pump_abort.abort();
    registry.drop_entry(&id).await;
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
    assert!(outcome.is_ok(), "{outcome:?}");
}
