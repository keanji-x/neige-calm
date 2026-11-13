//! Shared legacy establishment and exact-alias control transport.
use super::*;

/// How [`TerminalRendererRegistry::ensure_with_launch`] may set up a renderer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Establish {
    /// Start the terminal's program unless the supervisor already runs it (`EnsureProc`).
    SpawnOrAttach,
    /// Attach to a running child only (#2516).
    AttachRunning,
    /// Observe for disposal, including sticky Exited; never Ensure.
    Disposal,
}

pub(super) struct EstablishedRenderer {
    pub(super) entry: RendererEntry,
    pub(super) handoff: Option<(crate::operation::task_launch::TaskLaunch, u32)>,
}

pub(super) async fn ensure_entry(
    mut cfg: RendererConfig,
    repo: Option<Arc<dyn RouteRepo>>,
    task_hook: Option<Arc<crate::scheduler::TerminalTaskHook>>,
    launch: Option<crate::operation::task_launch::TaskLaunch>,
    output_wake: OutputWake,
    establish: Establish,
) -> anyhow::Result<EstablishedRenderer> {
    use crate::operation::terminal_launch::{self, TerminalStart};
    // Match the absolute endpoint persisted in the one-use launch record.
    if !cfg.supervisor_sock.is_absolute() {
        cfg.supervisor_sock = std::env::current_dir()?.join(&cfg.supervisor_sock);
    }
    let start = match repo.as_deref() {
        _ if establish == Establish::Disposal => {
            TerminalStart::AttachOnly(cfg.supervisor_sock.clone())
        }
        Some(repo) => {
            terminal_launch::resolve(repo, &cfg.terminal_id, &cfg.supervisor_sock, launch).await?
        }
        None if launch.is_some() => anyhow::bail!("task launch requires its repository"),
        None => TerminalStart::Unbound,
    };
    let (launch, attach_only) = match start {
        TerminalStart::Fresh(launch) => (Some(*launch), false),
        TerminalStart::Unbound => (None, establish == Establish::AttachRunning),
        TerminalStart::AttachOnly(sock) => {
            cfg.supervisor_sock = sock;
            (None, true)
        }
    };
    // A replay alone cannot reconstruct geometry changes from an earlier
    // server lifetime. Human reattachment remains available; model observation
    // refuses that unproven projection instead of inventing a fresh screen.
    // The same fact (no pid persisted before this spawn) is what makes the
    // replay this lifetime's output for the activity stamp below.
    let observation_replay_proven = !attach_only
        && match repo.as_deref() {
            Some(repo) => repo
                .terminal_get(&cfg.terminal_id)
                .await?
                .is_some_and(|terminal| terminal.pid.is_none()),
            None => true,
        };
    let mut handoff = None;
    let proc_id = format!("term:{}", cfg.terminal_id);
    let mut control_conn = match calm_session::control::execution::connect_legacy(
        &cfg.supervisor_sock,
    )
    .await
    {
        Ok(connection) => connection,
        Err(error) => {
            if let (Some(repo), Some(launch)) = (repo.as_deref(), launch.as_ref())
                && let Err(reset_error) = terminal_launch::reset_unissued(repo, launch).await
            {
                tracing::warn!(terminal_id=%cfg.terminal_id, %reset_error, "unable to record terminal request not sent; ownership retained");
            }
            return Err(anyhow::anyhow!(
                "connect proc supervisor {}: {error}",
                cfg.supervisor_sock.display()
            ));
        }
    };
    if !attach_only {
        let request = ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.clone(),
            program: cfg.program.clone(),
            args: cfg.args.clone(),
            envs: cfg.envs.clone(),
            cwd: cfg.cwd.clone(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty {
                cols: cfg.cols,
                rows: cfg.rows,
            },
            replay_bytes: cfg.buffer_bytes,
        });
        let launch_sock = cfg.supervisor_sock.clone();
        let launch_proc_id = proc_id.clone();
        let exchange = async move {
            write_frame(&mut control_conn, &request)
                .await
                .map_err(|error| crate::error::CalmError::Internal(error.to_string()))?;
            // Unacknowledged launch cleanup remains in the existing bounded reader.
            let reply = read_control_reply_or_kill(
                &mut control_conn,
                SPAWN_CONTROL_READ_TIMEOUT,
                "spawn",
                &launch_sock,
                &launch_proc_id,
            )
            .await
            .map_err(|error| crate::error::CalmError::Internal(error.to_string()))?;
            Ok((reply, control_conn))
        };
        let (reply, returned_connection) = match launch.as_ref() {
            Some(launch) => {
                let repo = repo
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("task launch requires its repository"))?;
                match launch.clone().run_observed(repo, exchange).await {
                    Ok(reply) => reply,
                    Err(failure) => {
                        if !failure.effect_started
                            && let Err(error) = terminal_launch::reset_unissued(repo, launch).await
                        {
                            tracing::warn!(terminal_id=%cfg.terminal_id, %error, "could not record unissued terminal request; retaining ownership");
                        }
                        if failure.effect_started {
                            request_terminal_stop(&cfg.supervisor_sock, &cfg.terminal_id).await;
                        }
                        return Err(failure.error.into());
                    }
                }
            }
            None => exchange.await?,
        };
        control_conn = returned_connection;
        // The admission transaction is over before PID and session persistence.
        match reply {
            ControlReply::Spawned { pid } => {
                if let Some(repo) = repo.as_ref() {
                    match repo.terminal_set_pid(&cfg.terminal_id, Some(pid)).await {
                        Ok(()) => handoff = launch.map(|launch| (launch, pid)),
                        Err(e) => tracing::warn!(terminal_id=%cfg.terminal_id, pid, error=%e,
                            "failed to persist terminal pid after supervisor spawn; ownership retained"),
                    }
                }
            }
            ControlReply::SpawnFailed {
                error, disposition, ..
            } => {
                if disposition == calm_session::control::SpawnFailedDisposition::NoChildCreated
                    && let (Some(repo), Some(launch)) = (repo.as_deref(), launch.as_ref())
                    && launch.terminal_role()
                        == crate::operation::task_launch::TerminalLaunchRole::BusinessProcess
                {
                    terminal_launch::reject_no_child(
                        repo,
                        launch,
                        &cfg.terminal_id,
                        &cfg.supervisor_sock,
                    )
                    .await?;
                }
                anyhow::bail!("{error}")
            }
            other => anyhow::bail!("unexpected proc-supervisor spawn reply: {other:?}"),
        }
        match read_control_reply(&mut control_conn, SPAWN_CONTROL_READ_TIMEOUT, "ready").await? {
            ControlReply::Ready => {}
            ControlReply::ReadyFailed { error, .. } => anyhow::bail!("{error}"),
            other => anyhow::bail!("unexpected proc-supervisor ready reply: {other:?}"),
        }
    }
    #[cfg(feature = "fixtures")]
    attach_hold_for_test::hold(&cfg.terminal_id).await;
    let render_plane: SharedRenderPlane = Arc::new(StdMutex::new(RenderPlane::with_colors(
        cfg.cols,
        cfg.rows,
        cfg.buffer_bytes,
        SCROLLBACK_MAX_LINES,
        Some(cfg.terminal_fg),
        Some(cfg.terminal_bg),
    )));
    let model_view = ModelView::new(cfg.cols, cfg.rows, cfg.terminal_fg, cfg.terminal_bg);
    if !observation_replay_proven {
        model_view
            .lock()
            .unwrap()
            .invalidate("terminal renderer reattached without complete geometry history");
    }

    render_plane
        .lock()
        .unwrap()
        .install_observer(Box::new(model_view::Observer(model_view.clone())));
    let owner_registry: SharedOwnerRegistry = Arc::new(StdMutex::new(OwnerRegistry::new()));
    let exit = Arc::new(StdMutex::new(None));
    let session_id = Uuid::new_v4();
    let (event_tx, initial_event_rx) = broadcast::channel::<DaemonMsg>(2048);
    let event_rx = event_tx.subscribe();
    let (supervisor_tx, supervisor_rx) = mpsc::unbounded_channel::<SupervisorControl>();

    // Completion outlives the transport so failed writes can retry the exact observation.
    let (exit_persisted_tx, exit_persisted) = watch::channel(false);
    let attachment = if establish == Establish::Disposal {
        timeout(
            Duration::from_secs(2),
            open_attachment(&cfg.supervisor_sock, &proc_id),
        )
        .await??
    } else {
        open_attachment(&cfg.supervisor_sock, &proc_id).await?
    };
    let Some((attach_conn, attached)) = attachment else {
        return Err(LegacyAbsent.into());
    };
    // Shared with the reader (its only writer after this function); read through the registry by
    // the activity projector.
    let last_output_ms = Arc::new(AtomicI64::new(0));
    let Attached {
        cursor_head,
        replay,
        running,
        ..
    } = attached;
    if establish == Establish::AttachRunning && !running {
        anyhow::bail!(
            "terminal {} has no running child to attach to",
            cfg.terminal_id
        );
    }
    let output_capture = {
        let output_capture = output_capture::TerminalOutputCapture::shared(cursor_head, &replay);
        if cursor_head != 0 {
            model_view
                .lock()
                .unwrap()
                .invalidate("supervisor history gap before renderer attach");
        }
        if !replay.is_empty() {
            let effects = match render_plane.lock() {
                Ok(mut rp) => rp.on_pty_chunk(replay),
                Err(_) => Vec::new(),
            };
            client_pump::apply_broadcaster_effects(&event_tx, &supervisor_tx, effects);
            // The supervisor sends no `Output` frame for replayed bytes. A fresh launch's
            // replay is output this lifetime printed before the attach and stamps like a
            // frame; a reattach replay is an earlier lifetime's screen and stamps nothing.
            if observation_replay_proven {
                last_output_ms.store(crate::model::now_ms(), Ordering::Relaxed);
            }
        }
        output_capture
    };

    let control_task = control_writer::spawn_supervisor_control_writer(
        control_conn,
        proc_id.clone(),
        supervisor_rx,
        exit_persisted.clone(),
    );

    let completion = super::legacy_completion::LegacyCompletion::new(
        repo.clone(),
        task_hook,
        cfg.terminal_id.clone(),
        output_wake.clone(),
        exit_persisted_tx,
    );
    let (exited_tx, exited_rx) = oneshot::channel::<Option<i32>>();
    let attach_task = attach_reader::spawn_supervisor_attach_reader(
        attach_conn,
        proc_id.clone(),
        render_plane.clone(),
        exit.clone(),
        event_tx.clone(),
        supervisor_tx.clone(),
        exited_tx,
        cfg.terminal_id.clone(),
        completion.clone(),
        output_capture,
        Arc::clone(&last_output_ms),
        output_wake,
    );
    let ready_task = child_ready::spawn_child_ready_poller(render_plane.clone(), event_tx.clone());

    Ok(EstablishedRenderer {
        entry: RendererEntry {
            terminal_id: cfg.terminal_id.clone(),
            proc_id,
            supervisor_sock: cfg.supervisor_sock.clone(),
            handle: RendererHandle {
                model_view,
                input_barrier: Arc::new(InputBarrier::default()),
                session_id,
                event_rx,
                event_tx,
                render_plane,
                owner_registry,
                supervisor_tx,
            },
            config: cfg,
            control_via_engine: false,
            exit,
            signals: SignalRing::new(),
            last_output_ms,
            initial_event_rx: StdMutex::new(Some(initial_event_rx)),
            exited_rx: StdMutex::new(Some(exited_rx)),
            attach_task: StdMutex::new(Some(attach_task)),
            exit_persisted,
            legacy_completion: Some(completion),
            tasks: StdMutex::new(vec![control_task, ready_task]),
        },
        handoff,
    })
}

/// Best-effort exact supervisor request. An acknowledgement proves neither
/// descendant termination nor permission to discard a workspace.
pub(crate) async fn request_terminal_stop(supervisor_sock: &Path, terminal_id: &str) {
    let proc_id = format!("term:{terminal_id}");
    if timeout(
        Duration::from_secs(1),
        signal_child_direct(supervisor_sock, &proc_id, ProcSignal::Kill),
    )
    .await
    .is_err()
    {
        tracing::warn!(
            terminal_id,
            "owned terminal stop request timed out; resources remain retained"
        );
    }
}

async fn read_control_reply_or_kill<R>(
    conn: &mut R,
    read_timeout: Duration,
    what: &str,
    supervisor_sock: &Path,
    proc_id: &str,
) -> anyhow::Result<ControlReply>
where
    R: AsyncRead + Unpin,
{
    match read_control_reply(conn, read_timeout, what).await {
        Ok(reply) => Ok(reply),
        Err(e) => {
            signal_child_direct(supervisor_sock, proc_id, ProcSignal::Kill).await;
            Err(e)
        }
    }
}

async fn read_control_reply<R>(
    conn: &mut R,
    read_timeout: Duration,
    what: &str,
) -> anyhow::Result<ControlReply>
where
    R: AsyncRead + Unpin,
{
    match timeout(read_timeout, read_frame::<ControlReply, _>(conn)).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            anyhow::bail!("proc-supervisor {what} reply timed out after {read_timeout:?}")
        }
    }
}

pub(super) async fn signal_child_direct(supervisor_sock: &Path, proc_id: &str, sig: ProcSignal) {
    if let Err(error) = signal_child_checked(supervisor_sock, proc_id, sig).await {
        tracing::warn!(%error, ?sig, proc_id, "direct supervisor signal failed");
    }
}

pub(super) async fn signal_child_checked(
    supervisor_sock: &Path,
    proc_id: &str,
    sig: ProcSignal,
) -> anyhow::Result<()> {
    let mut conn = calm_session::control::execution::connect_legacy(supervisor_sock).await?;
    write_frame(
        &mut conn,
        &ControlMsg::Signal(SignalRequest {
            proc_id: proc_id.into(),
            sig,
        }),
    )
    .await?;
    match read_control_reply(&mut conn, Duration::from_millis(200), "signal").await? {
        ControlReply::SignalOk => Ok(()),
        reply => anyhow::bail!("supervisor refused signal: {reply:?}"),
    }
}

#[derive(Debug, Error)]
#[error("terminal absent from current legacy registry")]
pub(super) struct LegacyAbsent;

/// None means exact UnknownProc, never a lossy Probe result or transport failure.
pub(super) async fn open_attachment(
    socket: &Path,
    proc_id: &str,
) -> anyhow::Result<Option<(tokio::net::UnixStream, Attached)>> {
    let mut conn = calm_session::control::execution::connect_legacy(socket).await?;
    write_frame(
        &mut conn,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: None,
            reader_id: "calm-server-renderer".into(),
        }),
    )
    .await?;
    match read_control_reply(&mut conn, SPAWN_CONTROL_READ_TIMEOUT, "attach").await? {
        ControlReply::AttachOk(attached) => {
            anyhow::ensure!(
                attached.proc_id == proc_id,
                "supervisor attached a different terminal"
            );
            Ok(Some((conn, attached)))
        }
        ControlReply::Error {
            kind: calm_session::control::ControlErrorKind::UnknownProc,
            ..
        } => Ok(None),
        reply => anyhow::bail!("supervisor refused attach: {reply:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::{UnixListener, UnixStream};

    #[tokio::test]
    async fn read_control_reply_or_kill_sends_kill_on_timeout() {
        let dir = calm_test_sockets::socket_dir("tr");
        let sock = calm_test_sockets::socket_path(dir.path(), "supervisor.sock");
        let listener = UnixListener::bind(&sock).expect("bind listener");
        let proc_id = "term:test-timeout".to_string();
        let expected_proc_id = proc_id.clone();
        let accept_task = tokio::spawn(async move {
            let (_silent_stream, _) = listener.accept().await.expect("accept silent connection");
            let (mut signal_conn, _) = listener.accept().await.expect("accept signal connection");
            assert!(
                matches!(read_frame::<ControlMsg, _>(&mut signal_conn).await.unwrap(), ControlMsg::Probe(request) if request.proc_id.is_empty())
            );
            write_frame(
                &mut signal_conn,
                &ControlReply::ProbeOk {
                    supervisor_version: 2,
                    proc_running: false,
                },
            )
            .await
            .unwrap();
            let msg = timeout(
                Duration::from_millis(500),
                read_frame::<ControlMsg, _>(&mut signal_conn),
            )
            .await
            .expect("signal frame should arrive")
            .expect("read signal frame");
            match msg {
                ControlMsg::Signal(SignalRequest { proc_id, sig }) => {
                    assert_eq!(proc_id, expected_proc_id);
                    assert_eq!(sig, ProcSignal::Kill);
                }
                other => panic!("unexpected control message: {other:?}"),
            }
            write_frame(&mut signal_conn, &ControlReply::SignalOk)
                .await
                .expect("write signal ack");
        });

        let mut conn = UnixStream::connect(&sock).await.expect("connect");
        let err = timeout(
            Duration::from_millis(1000),
            read_control_reply_or_kill(
                &mut conn,
                Duration::from_millis(50),
                "spawn",
                &sock,
                &proc_id,
            ),
        )
        .await
        .expect("read helper should return before outer timeout")
        .expect_err("silent supervisor should time out");

        assert!(
            err.to_string()
                .contains("proc-supervisor spawn reply timed out after 50ms"),
            "unexpected error: {err}"
        );

        timeout(Duration::from_millis(1000), accept_task)
            .await
            .expect("supervisor task should finish")
            .expect("supervisor task should not panic");
    }

    #[tokio::test]
    async fn read_control_reply_times_out_when_supervisor_is_silent() {
        let dir = calm_test_sockets::socket_dir("tr");
        let sock = calm_test_sockets::socket_path(dir.path(), "supervisor.sock");
        let listener = UnixListener::bind(&sock).expect("bind listener");
        let accept_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept connection");
            tokio::time::sleep(Duration::from_secs(1)).await;
            drop(stream);
        });

        let mut conn = UnixStream::connect(&sock).await.expect("connect");
        let err = timeout(
            Duration::from_millis(500),
            read_control_reply(&mut conn, Duration::from_millis(50), "spawn"),
        )
        .await
        .expect("read helper should return before outer timeout")
        .expect_err("silent supervisor should time out");

        assert!(
            err.to_string()
                .contains("proc-supervisor spawn reply timed out after 50ms"),
            "unexpected error: {err}"
        );

        accept_task.abort();
        let _ = accept_task.await;
    }
}
