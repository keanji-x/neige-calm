//! boot responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    /// Auto-spawn every enabled plugin known to the repo. Per-plugin failures are logged and swallowed.
    /// Connector bring-up stays inline and serial: the boot audit loop in `AppState::new` reads `exposes_tools` after this returns, so a background task would race the materialization.
    pub async fn autospawn_enabled(self: &Arc<Self>) {
        self.autospawn_enabled_within(CONNECTOR_AUTOSPAWN_BUDGET)
            .await;
    }

    /// `autospawn_enabled` with the connector budget supplied, so a test can drive the real loop against a small budget.
    pub async fn autospawn_enabled_within(self: &Arc<Self>, connector_budget: Duration) {
        // Enumeration needs its own wall: neither per-app nor connector-loop fences can start until rows exist.
        let rows = match tokio::time::timeout(
            self.plugin_list_wall,
            self.plugin_list_db.plugins_list_all(),
        )
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(
                    error = %e,
                    reason = "plugin enumeration failed",
                    "plugin autospawn skipped: plugin enumeration failed"
                );
                return;
            }
            Err(_elapsed) => {
                // No plugin id is known yet, and this arm may not await an event-store write: the same wedged repo may be what exhausted the fence.
                tracing::warn!(
                    wall_ms = self.plugin_list_wall.as_millis(),
                    reason = "plugin enumeration timed out",
                    "plugin autospawn skipped: plugin enumeration timed out"
                );
                return;
            }
        };
        // The budget must never be smaller than a single connector's own cap, or a lone connector is cut off by the LOOP bound at boot but comes up fine through `POST /enable`.
        // Bounded: `connector_bringup_budget` is capped at manifest parse time.
        let widest = rows
            .iter()
            .filter(|p| p.enabled)
            .filter_map(|p| self.registry.get(&p.id))
            .filter(|m| matches!(m.kind, ConnectorKind::McpHttp | ConnectorKind::CliQuery))
            .map(|m| connector_bringup_budget(&m))
            .max()
            .unwrap_or_default();
        let connector_budget = widened_connector_budget(connector_budget, widest);

        // CONNECTOR-only elapsed time: a slow `app` child ahead of a connector must not consume this budget.
        let ceiling = connector_phase_ceiling(connector_budget);
        let mut connector_elapsed = Duration::ZERO;
        for plug in rows {
            if !plug.enabled {
                continue;
            }
            // `app` plugins spawn a local child and are not network-bound, so
            // they are outside this budget — it exists for the remote half.
            let is_connector = self.registry.get(&plug.id).is_some_and(|m| {
                matches!(m.kind, ConnectorKind::McpHttp | ConnectorKind::CliQuery)
            });
            if !is_connector {
                // The fence wraps the whole iteration (lock wait, spawn, every emission), not a named step inside it.
                match tokio::time::timeout(self.app_autospawn_wall, self.autospawn_one(&plug.id))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(plugin_id = %plug.id, error = %e, "plugin autospawn failed");
                    }
                    Err(_elapsed) => {
                        let reason = format!(
                            "plugin `{}` was cut off at boot: it did not finish starting \
                             within {} ms (a wedged lifecycle lock, a stuck child, or a \
                             slow event store). Enable it again once that is fixed.",
                            plug.id,
                            self.app_autospawn_wall.as_millis()
                        );
                        tracing::warn!(plugin_id = %plug.id, "{reason}");
                        // Terminal + observable. `try`, not `await`: the likely holder is the guard we just gave up waiting for.
                        // `mark_unavailable_under`, not `publish_`: this arm runs outside the fence, and the event-store write could hang boot.
                        match self.try_lock_lifecycle(&plug.id) {
                            Ok(g) => {
                                self.mark_unavailable_under(&g, None, reason);
                            }
                            Err(_) => tracing::warn!(
                                plugin_id = %plug.id,
                                "app autospawn was cut off and the lifecycle lock is \
                                 still held; leaving the runtime state to its holder"
                            ),
                        }
                    }
                }
                continue;
            }

            // One fence over the WHOLE per-connector iteration — bring-up, reconciliation, every persisted emission — so a slow event store cannot hold boot.
            let started = Instant::now();
            let spawn_deadline = started + connector_budget.saturating_sub(connector_elapsed);
            let phase_deadline = started + ceiling.saturating_sub(connector_elapsed);
            let fenced = tokio::time::timeout_at(
                tokio::time::Instant::from_std(phase_deadline),
                self.autospawn_one_connector(&plug.id, spawn_deadline, connector_budget),
            )
            .await;
            // Only connector iterations are charged.
            connector_elapsed += started.elapsed();
            if fenced.is_err() {
                // Out of wall clock: leave a terminal entry with NO await, because an await is what ran us out.
                let reason = format!(
                    "connector `{}` was cut off at boot: the connector phase's {} ms \
                     wall-clock ceiling was reached (a slow or unreachable connector, \
                     or a slow event store). Re-enable it once that is fixed.",
                    plug.id,
                    ceiling.as_millis()
                );
                tracing::warn!(plugin_id = %plug.id, "{reason}");
                // Take the lifecycle lock synchronously: an unlocked table write could be the last runtime write of a concurrent `stop`, resurrecting a plugin with no matching event.
                match self.try_lock_lifecycle(&plug.id) {
                    Ok(g) => {
                        self.mark_unavailable_under(&g, None, reason);
                    }
                    Err(_) => tracing::warn!(
                        plugin_id = %plug.id,
                        "boot budget elapsed and the lifecycle lock was held; \
                         leaving the runtime state to its holder"
                    ),
                }
            }
        }
    }

    /// One non-connector autospawn attempt. `Busy` waits rather than guessing; the wait is unbounded here and both callers fence it.
    pub(super) async fn autospawn_one(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        match self.try_lock_lifecycle(id) {
            Ok(g) => self.spawn_under(&g, None).await,
            Err(HostError::LifecycleBusy(_)) => {
                tracing::info!(
                    plugin_id = %id,
                    "autospawn found the lifecycle lock held; waiting for it"
                );
                let g = self.await_lifecycle(id).await;
                self.spawn_under(&g, None).await
            }
            Err(other) => Err(other),
        }
    }

    /// One connector's boot iteration: bring it up within `spawn_deadline`, then reconcile. Every await here is inside the caller's phase fence.
    pub(super) async fn autospawn_one_connector(
        self: &Arc<Self>,
        id: &str,
        spawn_deadline: Instant,
        connector_budget: Duration,
    ) {
        let remaining = spawn_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let reason = format!(
                "connector `{id}` was not brought up at boot: the {} ms budget for \
                 starting all connectors was already spent by earlier ones. \
                 Re-enable it once the slow/unreachable connectors are fixed.",
                connector_budget.as_millis()
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            // Terminal + observable; never regress an id that is already live and `Running`.
            let _ = self.publish_unavailable(id, reason).await;
            return;
        }
        let outcome = tokio::time::timeout_at(
            tokio::time::Instant::from_std(spawn_deadline),
            self.autospawn_one(id),
        )
        .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(plugin_id = %id, error = %e, "plugin autospawn failed");
            }
            Err(_elapsed) => {
                // The dropped `spawn` future released its reservation but emitted no terminal state.
                // This arm must RECONCILE: the timeout may have elapsed after the live insert but before `emit_state(Running)`, so `publish_unavailable` refuses to regress a `Running` entry.
                let reason = format!(
                    "connector `{id}` did not finish starting within the remaining \
                     {} ms of the {} ms boot budget for all connectors",
                    remaining.as_millis(),
                    connector_budget.as_millis()
                );
                if self.publish_unavailable(id, reason.clone()).await {
                    tracing::warn!(plugin_id = %id, "{reason}");
                } else {
                    tracing::info!(
                        plugin_id = %id,
                        "connector boot budget elapsed after it had already come up; \
                         keeping it Running"
                    );
                    // The dropped future never reached its own `emit_state(Running)`. Not `emit_state` directly: `reaffirm_running` re-decides and emits as one serialized step so a concurrent `stop()` cannot be overwritten.
                    self.reaffirm_running(id).await;
                }
            }
        }
    }
}
