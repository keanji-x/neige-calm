//! Recovery admits a source before locked native scopes; it never resends business input.
use super::*;

impl SharedCodexAppServer {
    pub(super) async fn connected_client(
        &self,
    ) -> std::result::Result<Arc<CodexAppServer>, ControlError> {
        if let Some(client) = self.ready_client().await {
            return Ok(client);
        }
        // One order for recovery: coordinator -> process transition -> per-thread replay.
        let _recovery = self.connection_recovery_serial.lock().await;
        if let Some(client) = self.ready_client().await {
            return Ok(client);
        }
        let transition = self.transition_serial.lock().await;
        self.replay_cached_threads_locked(&transition)
            .await
            .map_err(ControlError::Unanswered)?;
        self.ready_client().await.ok_or_else(|| {
            ControlError::Unanswered(CalmError::CodexAppServer(
                "shared app-server is not connected".into(),
            ))
        })
    }

    pub(super) async fn ready_client(&self) -> Option<Arc<CodexAppServer>> {
        let core = self.core.lock().await;
        match &core.state {
            SupervisorState::Running {
                client,
                replay: ConnectionReplay::Complete,
                ..
            } if !client.is_closed() => Some(client.clone()),
            _ => None,
        }
    }

    // The caller retains this process boundary through the whole replay. A fresh
    // process cannot inherit an old pass's hot mode or prepared credentials.
    pub(super) async fn reconnect_running_client_locked(
        &self,
        _transition: &tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<Option<Arc<CodexAppServer>>> {
        let previous = {
            let core = self.core.lock().await;
            match &core.state {
                SupervisorState::Running { client, .. } if !client.is_closed() => {
                    return Ok(Some(client.clone()));
                }
                SupervisorState::Running { client, .. } => client.clone(),
                _ => return Ok(None),
            }
        };
        let paths = self.verify_backend_isolation().await?;
        let (client, notifications) = connect_initialized(&paths, &self.approval_routes).await?;
        let client = Arc::new(client);
        drop(self.forward_client(client.clone(), notifications).await?);
        let mut core = self.core.lock().await;
        let SupervisorState::Running {
            client: installed,
            replay,
            ..
        } = &mut core.state
        else {
            return Ok(None);
        };
        if !Arc::ptr_eq(installed, &previous) {
            return Err(CalmError::Conflict(
                "Codex process source changed during connection recovery".into(),
            ));
        }
        // Pending cold progress belongs to this PROCESS and survives cancellation
        // and socket replacement. Only a completed process starts a new hot pass.
        if matches!(replay, ConnectionReplay::Complete) {
            *replay = ConnectionReplay::Pending(ReplayPlan::new(ResumeMode::HotTakeover));
        }
        self.native_turns.publish(&client);
        *installed = client.clone();
        core.generation = core.generation.wrapping_add(1);
        self.readiness.send_replace(DaemonReadiness {
            generation: core.generation,
            running: true,
        });
        tracing::info!(
            "shared codex connection re-established; previous request outcomes remain unknown"
        );
        Ok(Some(client))
    }
}
