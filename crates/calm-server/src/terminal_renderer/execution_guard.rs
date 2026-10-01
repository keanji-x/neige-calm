//! Supervisor-confirmed terminal execution release.
use super::*;

pub(super) async fn require_stop_protocol(sock: &Path, terminal_id: &str) -> anyhow::Result<()> {
    let probe = async {
        let mut conn = UnixStream::connect(sock).await?;
        write_frame(
            &mut conn,
            &ControlMsg::Probe(calm_session::control::ProbeRequest {
                proc_id: format!("term:{terminal_id}"),
            }),
        )
        .await?;
        match read_frame::<ControlReply, _>(&mut conn).await? {
            ControlReply::ProbeOk {
                supervisor_version, ..
            } if supervisor_version == calm_session::SUPERVISOR_CONTROL_VERSION => Ok(()),
            reply => anyhow::bail!(
                "supervisor stop protocol unavailable ({reply:?}); restart supervisor"
            ),
        }
    };
    timeout(Duration::from_secs(2), probe)
        .await
        .map_err(|_| anyhow::anyhow!("supervisor stop protocol probe timed out"))?
}

/// Stop the exact durable execution identity, including escaped descendants, and
/// seal it against delayed EnsureProc before releasing the persistent writer.
pub(crate) async fn stop_and_release_terminal(
    repo: &dyn RouteRepo,
    sock: &Path,
    terminal_id: &str,
) -> crate::error::Result<()> {
    require_stop_protocol(sock, terminal_id)
        .await
        .map_err(|e| crate::error::CalmError::Conflict(format!("terminal writer retained: {e}")))?;
    let terminal_id = terminal_id.to_owned();
    let proof = async {
        let mut conn = UnixStream::connect(sock).await?;
        write_frame(
            &mut conn,
            &ControlMsg::StopAndConfirm {
                proc_id: format!("term:{terminal_id}"),
            },
        )
        .await?;
        match read_frame::<ControlReply, _>(&mut conn).await? {
            ControlReply::Stopped => Ok::<_, anyhow::Error>(()),
            reply => anyhow::bail!("terminal stop not confirmed: {reply:?}"),
        }
    };
    timeout(Duration::from_secs(10), proof)
        .await
        .map_err(|_| {
            crate::error::CalmError::Conflict(
                "terminal stop confirmation timed out; writer retained".into(),
            )
        })?
        .map_err(|e| {
            crate::error::CalmError::Conflict(format!(
                "terminal stop unverified; writer retained: {e}"
            ))
        })?;
    let stopped =
        serde_json::to_string(&crate::operation::terminal_launch::RequestState::Stopped {
            version: 1,
            terminal_id: terminal_id.clone(),
            supervisor_sock: sock.to_owned(),
        })?;
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            crate::operation::workspace_lease::execution_guard::release_stopped_execution_tx(
                tx,
                "terminal",
                &terminal_id,
            )
            .await?;
            sqlx::query(
                r#"
UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch',json(?1))
WHERE json_extract(tx_output_json,'$.data.terminal_id')=?2
AND kind IN ('terminal-create','terminal-worker','codex-worker','claude-worker')
"#,
            )
            .bind(stopped)
            .bind(terminal_id)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
    })
    .await
}

/// Reconcile durable writers after natural exit, cancellation, or server restart.
/// Only stopped supervisor executions discharge a lease; missing registry/PID alone never does.
pub(crate) async fn reconcile_terminal_writers(
    repo: &dyn crate::db::RouteRepo,
    sock: &std::path::Path,
) -> crate::error::Result<()> {
    let candidates = crate::db::write_in_tx_typed(repo, |tx| {
        Box::pin(async move {
            let rows: Vec<(String, bool)> = sqlx::query_as(
                r#"
SELECT DISTINCT l.holder_id,
COALESCE((t.id IS NULL OR t.exit_code IS NOT NULL OR t.signal_killed=1 OR
 o.phase IN ('compensating','failed','stuck') OR
 ws.state IN ('exited','failed','superseded')),0)
FROM workspace_leases l LEFT JOIN terminals t ON t.id=l.holder_id
LEFT JOIN operations o ON json_extract(o.tx_output_json,'$.data.terminal_id')=l.holder_id
LEFT JOIN worker_sessions ws ON ws.terminal_run_id=t.id
WHERE l.holder_kind='terminal' AND l.state IN ('held','releasing')
AND o.phase IN ('succeeded','spawn_succeeded','compensating','failed','stuck')
ORDER BY l.updated_at_ms,l.holder_id LIMIT 16
"#,
            )
            .fetch_all(&mut **tx)
            .await?;
            for (id, _) in &rows {
                sqlx::query(
                    r#"
UPDATE workspace_leases SET updated_at_ms=?2 WHERE holder_kind='terminal'
AND holder_id=?1 AND state IN ('held','releasing')
"#,
                )
                .bind(id)
                .bind(crate::model::now_ms())
                .execute(&mut **tx)
                .await?;
            }
            Ok(rows)
        })
    })
    .await?;
    for (id, force_stop) in candidates {
        if !force_stop {
            match tokio::time::timeout(
                Duration::from_secs(2),
                crate::probe_supervisor_for_terminal_at(Some(sock), &id),
            )
            .await
            {
                Ok(Ok(false)) => {}
                _ => continue,
            }
        }
        if let Err(e) = crate::terminal_renderer::stop_and_release_terminal(repo, sock, &id).await {
            tracing::warn!(terminal_id=%id,error=%e,"terminal writer retained pending stop reconciliation");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn old_supervisor_cannot_confirm_stop_or_admit_new_execution() {
        let dir = calm_test_sockets::socket_dir("old-stop-version");
        let sock = dir.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            assert!(matches!(
                read_frame::<ControlMsg, _>(&mut conn).await.unwrap(),
                ControlMsg::Probe(_)
            ));
            write_frame(
                &mut conn,
                &ControlReply::ProbeOk {
                    supervisor_version: 1,
                    proc_running: false,
                },
            )
            .await
            .unwrap();
        });
        let error = require_stop_protocol(&sock, "legacy").await.unwrap_err();
        assert!(error.to_string().contains("restart supervisor"));
        server.await.unwrap();
    }
}
