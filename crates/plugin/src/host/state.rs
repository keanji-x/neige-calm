//! state responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    /// Re-emit `Running` for a connector whose own `emit_state(Running)` never ran, but only if it is still running when the emission happens; returns whether it emitted.
    /// The lifecycle guard is held across the check and the emission, and `stopping` is checked as well as `Running`, so a concurrent `stop` cannot be overwritten by a stale `Running`.
    pub async fn reaffirm_running(self: &Arc<Self>, id: &str) -> bool {
        // Boot reconciliation waits: a `false` on a busy lock would be indistinguishable from 'not running' and leave the log stuck at `spawning`.
        let serialized = self.await_lifecycle(id).await;
        self.reaffirm_running_under(&serialized).await
    }

    pub(super) async fn reaffirm_running_under(self: &Arc<Self>, guard: &LifecycleGuard) -> bool {
        {
            let table = self.lock_table();
            match table.live.get(guard.id()) {
                Some(rp) if matches!(rp.status, PluginRuntimeStatus::Running) && !rp.stopping => {}
                _ => return false,
            }
        }
        self.emit_state_under(guard, &PluginRuntimeStatus::Running)
            .await;
        true
    }

    /// Gracefully stop a plugin: sets `stopping=true` so the supervisor won't respawn, SIGTERMs, awaits exit.
    /// Stopping a never-successfully-enabled connector is `Ok` and clears its `Unavailable` entry; the reason stays in the event log.
    pub async fn stop(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Reject-only pre-lock probe so an unknown id cannot mint a `lifecycle` map entry.
        // `spawning` must be part of it: a mid-spawn id must answer `Busy`, not 404.
        {
            let table = self.lock_table();
            if !table.live.contains_key(id) && !table.spawning.contains(id) {
                return Err(HostError::NotFound(id.to_string()));
            }
        }
        let guard = self.try_lock_lifecycle(id)?;
        self.stop_under(&guard).await
    }

    /// `stop` for a caller that already holds the guard. An in-flight spawn holds the same guard, so by now it has either landed or unwound.
    pub(super) async fn stop_under(
        self: &Arc<Self>,
        guard: &LifecycleGuard,
    ) -> Result<(), HostError> {
        let id = guard.id();
        let (process, supervisor, subs) = {
            let mut table = self.lock_table();
            let rp = table
                .live
                .get_mut(id)
                .ok_or_else(|| HostError::NotFound(id.to_string()))?;
            // Unreachable while the guard is held; kept as a debug assertion so an unlocked stop path trips in tests.
            debug_assert!(
                !rp.stopping,
                "{id} was already stopping while its lifecycle guard was held"
            );
            if rp.stopping {
                return Err(HostError::NotFound(id.to_string()));
            }
            rp.stopping = true;
            rp.stop_serving();
            let process = rp.process.clone();
            let supervisor = rp.supervisor.take();
            let subs = Arc::clone(&rp.subscriptions);
            // Abort the router so it doesn't race the channel-close on mcp drop; abort() is idempotent. Connectors have no router.
            if let Some(router) = rp.router.as_ref() {
                router.abort();
            }
            (process, supervisor, subs)
        };

        // Abort every active `neige.event.subscribe` bridge task. Holding
        // these past process exit would leak event-bus subscribers.
        {
            let mut s = subs.lock().await;
            for rec in s.drain(..) {
                rec.task.abort();
            }
        }

        // Abort the supervisor *before* we kill the process so it doesn't
        // race us into a respawn attempt.
        if let Some(h) = supervisor {
            h.abort();
        }
        // A connector has no child to signal: dropping the last `ConnectorClient` clone closes the HTTP client / releases the CLI runtime.
        if let Some(process) = process {
            match process.stop(STOP_GRACE).await {
                Ok(_status) => {}
                Err(ProcessError::AlreadyDead) => {
                    // Supervisor was already going to react to this. Fine.
                }
                Err(e) => {
                    return Err(HostError::Spawn(e));
                }
            }
        }

        // Removing the entry and announcing `Disabled` is ONE serialized step, or `reaffirm_running` can slip a stale `Running` between them.
        // Locked after the slow child teardown so it never spans a SIGTERM grace.
        {
            let mut table = self.lock_table();
            table.live.remove(id);
        }
        self.emit_state_under(guard, &PluginRuntimeStatus::Disabled)
            .await;
        Ok(())
    }

    /// Stop then spawn, as ONE critical section: a concurrent `uninstall` in the gap would have the respawn resurrect a deleted plugin.
    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Reject-only; the same pair `spawn_under` opens with, moved before a map cell is minted.
        self.spawn_admission_check(id)?;
        let guard = self.try_lock_lifecycle(id)?;
        self.restart_under(&guard).await
    }

    pub(super) async fn restart_under(
        self: &Arc<Self>,
        guard: &LifecycleGuard,
    ) -> Result<(), HostError> {
        // Stop is best-effort: if it returns NotFound (e.g. already crashed
        // and cleaned up), we proceed to spawn.
        match self.stop_under(guard).await {
            Ok(()) | Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        self.spawn_under(guard, None).await
    }

    /// Snapshot current status for one plugin. An admission-reserved id
    /// (mid-spawn, no live entry yet) reports as `Spawning` with no pid.
    pub async fn status(&self, id: &str) -> Option<PluginHostStatus> {
        let table = self.lock_table();
        if table.spawning.contains(id) {
            return Some(PluginHostStatus {
                id: id.to_string(),
                status: PluginRuntimeStatus::Spawning,
                pid: None,
            });
        }
        table.live.get(id).map(|rp| PluginHostStatus {
            id: id.to_string(),
            status: rp.status.clone(),
            pid: rp.process.as_ref().and_then(|p| p.pid()),
        })
    }

    /// Snapshot the full table. Admission-reserved ids report as `Spawning` and shadow any stale crashed live entry.
    pub async fn list_running(&self) -> Vec<PluginHostStatus> {
        let table = self.lock_table();
        let mut out: Vec<PluginHostStatus> = table
            .spawning
            .iter()
            .map(|id| PluginHostStatus {
                id: id.clone(),
                status: PluginRuntimeStatus::Spawning,
                pid: None,
            })
            .collect();
        out.extend(
            table
                .live
                .iter()
                .filter(|(id, _)| !table.spawning.contains(*id))
                .map(|(id, rp)| PluginHostStatus {
                    id: id.clone(),
                    status: rp.status.clone(),
                    pid: rp.process.as_ref().and_then(|p| p.pid()),
                }),
        );
        out
    }

    /// Snapshot ids that are currently running. Admission-reserved
    /// (`Spawning`) ids are deliberately NOT included: tool visibility and
    /// dispatch must not expose a plugin before its handshake completed.
    pub async fn running_plugin_ids(&self) -> BTreeSet<String> {
        let table = self.lock_table();
        table
            .live
            .iter()
            .filter(|(_, rp)| matches!(rp.status, PluginRuntimeStatus::Running))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Most-recent stderr lines, oldest → newest. A connector has no stderr ring and reports an empty tail rather than `None`.
    pub async fn stderr_tail(&self, id: &str, n: usize) -> Option<Vec<String>> {
        let table = self.lock_table();
        table.live.get(id).map(|rp| {
            rp.process
                .as_ref()
                .map(|p| p.stderr_tail(n))
                .unwrap_or_default()
        })
    }

    /// Borrow the live **stdio** MCP client; `None` for connectors. Callers that structurally need an `app` plugin use this; ordinary dispatch uses `connector_client`.
    pub async fn mcp_client(&self, id: &str) -> Option<Arc<McpClient>> {
        let table = self.lock_table();
        table
            .live
            .get(id)
            .filter(|rp| matches!(rp.status, PluginRuntimeStatus::Running))
            .and_then(|rp| rp.mcp.as_ref()?.as_stdio().cloned())
    }

    /// Borrow the live client of a running plugin whatever its kind.
    /// The clone happens under the sync table mutex, which is why every `ConnectorClient` variant is `Arc`-wrapped.
    pub async fn connector_client(&self, id: &str) -> Option<ConnectorClient> {
        let table = self.lock_table();
        table
            .live
            .get(id)
            .filter(|rp| matches!(rp.status, PluginRuntimeStatus::Running))
            .and_then(|rp| rp.mcp.clone())
    }

    /// Dispatch a `neige.*` callback against the in-kernel handler, using the same `CallbackCtx` the plugin's inbound router builds; the plugin process is never asked.
    /// `call_id` lands in `events.correlation` as `user_tool_call:<call_id>`. Returns `RpcError::unavailable` (-32503) if the plugin isn't running.
    pub async fn dispatch_neige_callback(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
        call_id: Option<&str>,
    ) -> Result<serde_json::Value, RpcError> {
        let (mcp, subscriptions) = {
            let table = self.lock_table();
            let rp = table
                .live
                .get(plugin_id)
                .ok_or_else(|| RpcError::unavailable("plugin not running"))?;
            if !matches!(rp.status, PluginRuntimeStatus::Running) {
                return Err(RpcError::unavailable("plugin not running"));
            }
            // The `neige.*` channel does not exist for connectors, so a non-`Stdio` client is refused here.
            let Some(stdio) = rp.mcp.as_ref().and_then(|c| c.as_stdio()) else {
                let detail = match rp.mcp.as_ref() {
                    Some(client) => format!("is a `{}` connector", client.variant_name()),
                    None => {
                        "has no MCP client (it is running without a live transport)".to_string()
                    }
                };
                return Err(RpcError::unavailable(format!(
                    "plugin `{plugin_id}` {detail}; \
                         neige.* callbacks are only available to app plugins"
                )));
            };
            (Arc::clone(stdio), Arc::clone(&rp.subscriptions))
        };

        self.callbacks
            .dispatch(
                CallbackInvocation {
                    plugin_id: plugin_id.into(),
                    registry: self.registry.clone(),
                    mcp,
                    subscriptions,
                    call_id: call_id.map(str::to_owned),
                },
                method,
                params,
            )
            .await
    }

    /// Persist a `plugin.state` event and broadcast it; the bus broadcast fires only after commit succeeds.
    /// The ONLY emitter, and it demands the guard, so an emission outside its decision's critical section is not expressible in this module.
    pub(super) async fn emit_state_under(
        &self,
        guard: &LifecycleGuard,
        status: &PluginRuntimeStatus,
    ) {
        let id = guard.id();
        self.state_sink.emit(id, status).await;
    }

    pub(super) async fn emit_crashed_under(&self, guard: &LifecycleGuard, reason: &str) {
        let status = PluginRuntimeStatus::Crashed {
            reason: reason.to_string(),
        };
        self.emit_state_under(guard, &status).await;
    }
}
