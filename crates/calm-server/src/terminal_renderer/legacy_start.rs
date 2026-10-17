//! Existing Legacy launch/attach contract; registered launches never enter it.
use super::*;

pub(super) struct LegacyStart {
    pub cfg: RendererConfig,
    pub control_conn: tokio::net::UnixStream,
    pub observation_replay_proven: bool,
    pub handoff: Option<(crate::operation::task_launch::TaskLaunch, u32)>,
}

pub(super) async fn start(
    mut cfg: RendererConfig,
    repo: Option<Arc<dyn RouteRepo>>,
    launch: Option<crate::operation::task_launch::TaskLaunch>,
    establish: Establish,
) -> anyhow::Result<LegacyStart> {
    use crate::operation::terminal_launch::{self, TerminalStart};
    // Match the absolute endpoint persisted in the one-use launch record.
    if !cfg.supervisor_sock.is_absolute() {
        cfg.supervisor_sock = std::env::current_dir()?.join(&cfg.supervisor_sock);
    }
    let start = match repo.as_deref() {
        Some(repo) => {
            terminal_launch::resolve(repo, &cfg.terminal_id, &cfg.supervisor_sock, launch).await?
        }
        None if launch.is_some() => anyhow::bail!("task launch requires its repository"),
        None => TerminalStart::Unbound,
    };
    let (launch, attach_only) = match start {
        TerminalStart::Fresh(launch) => (Some(*launch), false),
        TerminalStart::Unbound => (None, matches!(establish, Establish::AttachRunning)),
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
    Ok(LegacyStart {
        cfg,
        control_conn,
        observation_replay_proven,
        handoff,
    })
}
