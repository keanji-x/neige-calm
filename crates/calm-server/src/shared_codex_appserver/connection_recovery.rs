//! A connection failure is not a daemon crash and never authorizes resending input.
use super::*;

impl SharedCodexAppServer {
    pub(super) async fn connected_client(
        &self,
    ) -> std::result::Result<Arc<CodexAppServer>, ControlError> {
        if let Some(client) = self.ready_client().await {
            return Ok(client);
        }
        let _recovery = self.connection_recovery_serial.lock().await;
        // Another waiter may already have installed and restored a healthy connection.
        if let Some(client) = self.ready_client().await {
            return Ok(client);
        }
        if self
            .reconnect_running_client()
            .await
            .map_err(ControlError::Unanswered)?
            .is_some()
        {
            self.replay_cached_threads(ResumeMode::HotTakeover).await;
        }
        if self.connection_needs_replay.load(Ordering::SeqCst) {
            return Err(ControlError::Unanswered(CalmError::CodexAppServer(
                "shared app-server subscription recovery is incomplete".into(),
            )));
        }
        self.ready_client().await.ok_or_else(|| {
            ControlError::Unanswered(CalmError::CodexAppServer(
                "shared app-server is not connected".into(),
            ))
        })
    }

    // Read readiness under the SAME core lock as source publication. Checking the
    // replay flag before awaiting core could return a newly published, unrestored source.
    async fn ready_client(&self) -> Option<Arc<CodexAppServer>> {
        let core = self.core.lock().await;
        match &core.state {
            SupervisorState::Running { client, .. }
                if !client.is_closed() && !self.connection_needs_replay.load(Ordering::SeqCst) =>
            {
                Some(client.clone())
            }
            _ => None,
        }
    }

    // Serialized against process replacement. Do not hold core across handshake/RPCs.
    // Replay calls this primitive instead of connected_client (no recursive recovery).
    pub(super) async fn reconnect_running_client(&self) -> Result<Option<Arc<CodexAppServer>>> {
        let _transition = self.transition_serial.lock().await;
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
        let (client, notifications) =
            connect_initialized(&self.sock, &self.approval_routes).await?;
        let client = Arc::new(client);
        drop(self.forward_client(client.clone(), notifications).await?);
        let mut core = self.core.lock().await;
        let SupervisorState::Running {
            client: installed, ..
        } = &mut core.state
        else {
            return Ok(None);
        };
        // Only this connection is replaced: preserve the child, PID identity and watcher.
        debug_assert!(Arc::ptr_eq(installed, &previous));
        self.connection_needs_replay.store(true, Ordering::SeqCst);
        self.native_turns.publish(&client);
        *installed = client.clone();
        core.generation = core.generation.wrapping_add(1);
        self.readiness.send_replace(DaemonReadiness {
            generation: core.generation,
            running: true,
        });
        tracing::info!(
            "shared codex app-server connection re-established; previous request outcomes remain unknown"
        );
        Ok(Some(client))
    }
}
