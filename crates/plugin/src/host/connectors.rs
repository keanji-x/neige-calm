//! connectors responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    /// `kind: mcp-http` spawn arm. No token, no process, no router, no supervisor.
    /// Materialize-before-publish is load-bearing: `running_plugin_ids` gates tool discovery and the boot audit loop, both of which read `exposes_tools`.
    pub(super) async fn spawn_mcp_http(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        install_path: &std::path::Path,
        guard: AdmissionGuard<E>,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let block = manifest.mcp_http.as_ref().ok_or_else(|| {
            HostError::BadState(format!(
                "plugin `{id}` is kind mcp-http but has no mcp_http block"
            ))
        })?;

        // Reset the ordering witness so a stale half from a failed previous attempt cannot pair with this one.
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

        // ONE outer wall-clock bound over the WHOLE bring-up; `request_timeout_ms` is the `tools/call` budget and is NOT consulted.
        // Config is read through the same gate every other kind uses, after `emit_state(Spawning)` to match `spawn_cli_query`.
        let (effective, guard) = self
            .config_for_spawn_or_unavailable(lifecycle, manifest, guard)
            .await?;

        // Render the url's `{{config.*}}` slots and re-validate; a failure is the connector's normal terminal state, not a kernel error.
        let url = match manifest::resolve_mcp_http_url(block, &effective) {
            Ok(url) => url,
            Err(reason) => return self.connector_unavailable(lifecycle, guard, reason).await,
        };

        let per_request = Duration::from_millis(block.bringup_timeout_ms());
        // One formula, one place: `autospawn_enabled_within` widens the loop budget by this same value.
        let budget = connector_bringup_budget(manifest);
        let outcome = tokio::time::timeout(
            budget,
            connect_mcp_http(
                id,
                block,
                &url,
                install_path.to_path_buf(),
                &self.kernel_version,
            ),
        )
        .await;

        let (client, upstream) = match outcome {
            Err(_elapsed) => {
                return self
                    .connector_unavailable(
                        lifecycle,
                        guard,
                        format!(
                            "connector bring-up timed out after {} ms \
                             ({} × mcp_http.bringup_timeout_ms = {} ms, plus {} ms slack)",
                            budget.as_millis(),
                            MCP_HTTP_ROUND_TRIPS,
                            per_request.as_millis(),
                            CONNECTOR_BRINGUP_SLACK.as_millis(),
                        ),
                    )
                    .await;
            }
            Ok(Err(reason)) => {
                return self.connector_unavailable(lifecycle, guard, reason).await;
            }
            Ok(Ok(v)) => v,
        };

        let tools = connector::materialize_http_tools(id, block, &upstream);
        let tool_count = tools.len();
        let published = match self.claim_minted_names(lifecycle, tools) {
            Ok(published) => published,
            Err(reason) => return self.connector_unavailable(lifecycle, guard, reason).await,
        };

        // Materialization, then the live insert: the ORDER is the invariant, and each block stamps the tick as its LAST action.

        // The publication is field-level, and a NO-OP if the id is not in the registry; abandoning the spawn is the right answer to 'the registry does not know this id' however we got there.
        if !published {
            let reason = format!(
                "plugin `{id}` left the registry while its connector was starting \
                 (uninstalled or reloaded mid-spawn); abandoning spawn"
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            drop(guard);
            // Without this the event log would sit at `spawning` forever. No live entry on purpose: a runtime row would resurrect an uninstalled id.
            self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
                .await;
            return Err(HostError::NotFound(id.to_string()));
        }
        self.stamp_spawn_order(id, SpawnOrderStep::Materialized);

        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: None,
                    mcp: Some(ConnectorClient::Http(Arc::clone(&client))),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    // Connectors have no supervisor; still allocated so `live` has one meaning of 'run instance'.
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            drop(table);
            self.stamp_spawn_order(id, SpawnOrderStep::LiveInserted);
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
        tracing::info!(
            plugin_id = %id,
            target = %client.log_target(),
            tool_count,
            "mcp-http connector running"
        );
        Ok(())
    }

    /// Bring up a `kind: cli-query` connector: same ordering invariants and failure channel as `spawn_mcp_http`, with a local resolve/probe in place of the network bring-up.
    /// No token, no supervised process, no router: each `tools/call` forks a fresh child.
    pub(super) async fn spawn_cli_query(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        install_path: &std::path::Path,
        guard: AdmissionGuard<E>,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let block = manifest.cli_query.as_ref().ok_or_else(|| {
            HostError::BadState(format!(
                "plugin `{id}` is kind cli-query but has no cli_query block"
            ))
        })?;

        // Reset the ordering witness so a stale half cannot pair with this attempt.
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

        // Read once, here: the child environment and argv config slots are built at bring-up and cached.
        let (effective, guard) = self
            .config_for_spawn_or_unavailable(lifecycle, manifest, guard)
            .await?;

        // ONE outer wall-clock bound: `AppState::new` awaits this inline. `cli_query.timeout_ms` is the `tools/call` budget and is NOT consulted.
        let budget = connector_bringup_budget(manifest);
        let outcome = tokio::time::timeout(
            budget,
            cli_query::bring_up(id, block, install_path, &effective),
        )
        .await;

        let runtime = match outcome {
            Err(_elapsed) => {
                return self
                    .connector_unavailable(
                        lifecycle,
                        guard,
                        format!(
                            "cli-query connector bring-up timed out after {} ms \
                             (command resolution plus the `--version` fingerprint probe)",
                            budget.as_millis(),
                        ),
                    )
                    .await;
            }
            Ok(Err(reason)) => {
                return self.connector_unavailable(lifecycle, guard, reason).await;
            }
            Ok(Ok(rt)) => Arc::new(rt),
        };

        let tools = connector::materialize_cli_tools(id, block);
        let tool_count = tools.len();
        let published = match self.claim_minted_names(lifecycle, tools) {
            Ok(published) => published,
            Err(reason) => return self.connector_unavailable(lifecycle, guard, reason).await,
        };

        // Materialization, then the live insert: the ORDER is the invariant.
        if !published {
            let reason = format!(
                "plugin `{id}` left the registry while its connector was starting \
                 (uninstalled or reloaded mid-spawn); abandoning spawn"
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            drop(guard);
            self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
                .await;
            return Err(HostError::NotFound(id.to_string()));
        }
        self.stamp_spawn_order(id, SpawnOrderStep::Materialized);

        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    // No supervised child: the process exists only for the
                    // duration of one `tools/call`.
                    process: None,
                    mcp: Some(ConnectorClient::Cli(Arc::clone(&runtime))),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            drop(table);
            self.stamp_spawn_order(id, SpawnOrderStep::LiveInserted);
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
        tracing::info!(
            plugin_id = %id,
            program = %runtime.program().display(),
            fingerprint = %runtime.fingerprint(),
            tool_count,
            "cli-query connector running"
        );
        Ok(())
    }

    /// Record one half of the ordering pair; kept in the production path because a `cfg(test)`-only probe cannot witness production ordering.
    pub(super) fn stamp_spawn_order(&self, id: &str, step: SpawnOrderStep) {
        let tick = SPAWN_ORDER_TICK.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut map = self
            .spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(id.to_string()).or_default();
        match step {
            SpawnOrderStep::Materialized => entry.materialized_at = Some(tick),
            SpawnOrderStep::LiveInserted => entry.live_inserted_at = Some(tick),
        }
    }

    /// The recorded ordering for `id`'s most recent connector spawn; `materialized_at < live_inserted_at` is the invariant.
    pub fn connector_spawn_order(&self, id: &str) -> Option<ConnectorSpawnOrder> {
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .copied()
    }

    /// Refuse a connector's materialized tools when they collide, else publish them, under the one
    /// table lock that admission's minted-name check also holds. The check and the publication are
    /// one step, so two concurrent spawns cannot each pass against the other's still-empty catalog
    /// (#2087 B5). `Ok(false)`: the id left the registry mid-spawn and nothing was published.
    pub(super) fn claim_minted_names(
        &self,
        lifecycle: &LifecycleGuard,
        tools: Vec<manifest::ExposedTool>,
    ) -> Result<bool, String> {
        let id = lifecycle.id();
        connector::refuse_minted_collisions(id, &tools)?;
        let table = self.lock_table();
        if let Some(conflict) = find_minted_name_conflict(
            id,
            &tools,
            &table.template_holder_ids(),
            self.registry.list(),
        ) {
            return Err(conflict.to_string());
        }
        let published = self.registry.set_exposes_tools(lifecycle, tools);
        drop(table);
        Ok(published)
    }

    /// Shared connector failure exit: swap the reservation for a live `Unavailable` entry, emit it, return a typed error.
    /// The live entry is what makes the failure observable; it does not block a later re-enable.
    pub(super) async fn connector_unavailable(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        guard: AdmissionGuard<E>,
        reason: String,
    ) -> Result<(), HostError> {
        // Discarded on purpose: this path runs strictly before the live insert, so the 'already Running' arm is unreachable.
        let _ = self
            .publish_unavailable_under(lifecycle, Some(guard), reason.clone())
            .await;
        Err(HostError::ConnectorUnavailable {
            plugin_id: lifecycle.id().to_string(),
            reason,
        })
    }

    /// The state half of `connector_unavailable`, usable without an admission guard (the boot-budget arm, whose `spawn` future was dropped).
    /// Will not regress a live `Running` entry and returns `false` when it declines: the boot timeout can elapse after the live insert.
    /// Waits for the lifecycle lock: a `Busy` folded into the failure arm would report a healthy connector as `Unavailable`.
    pub(super) async fn publish_unavailable(self: &Arc<Self>, id: &str, reason: String) -> bool {
        let guard = self.await_lifecycle(id).await;
        self.publish_unavailable_under(&guard, None, reason).await
    }

    pub(super) async fn publish_unavailable_under(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        guard: Option<AdmissionGuard<E>>,
        reason: String,
    ) -> bool {
        if !self.mark_unavailable_under(lifecycle, guard, reason.clone()) {
            return false;
        }
        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
            .await;
        true
    }

    /// The **synchronous** half of `publish_unavailable`: table write only, no await, so the boot fence can give up the event half but never this one.
    /// Returns `false` without touching anything when the id is live and `Running`.
    pub(super) fn mark_unavailable_under(
        &self,
        lifecycle: &LifecycleGuard,
        guard: Option<AdmissionGuard<E>>,
        reason: String,
    ) -> bool {
        let id = lifecycle.id();
        {
            // One lock: release the reservation and publish the terminal entry together.
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = match table.live.get(id) {
                Some(prev) => (prev.crashes_in_window, prev.window_started),
                None => (0, Instant::now()),
            };
            table.spawning.remove(id);
            if let Some(guard) = guard {
                guard.disarm();
            }
            if matches!(
                table.live.get(id).map(|rp| &rp.status),
                Some(PluginRuntimeStatus::Running)
            ) {
                return false;
            }
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: None,
                    mcp: None,
                    status: PluginRuntimeStatus::Unavailable {
                        reason: reason.clone(),
                    },
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    // Must be distinct from the run it replaces so a stale supervisor cannot match it.
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
        }
        tracing::warn!(plugin_id = %id, reason = %reason, "plugin unavailable");
        true
    }
}
