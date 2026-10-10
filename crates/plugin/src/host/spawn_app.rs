//! spawn app responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    pub(super) async fn spawn_admitted_inner(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard<E>,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let install_path = self
            .registry
            .install_path(id)
            .unwrap_or_else(|| self.plugins_dir.join(id));

        // Branch by kind BEFORE `ensure_plugin_token()`: a token for a connector would be a `plugin_tokens` row nobody ever presents.
        match manifest.kind {
            ConnectorKind::Builtin => {
                return self.spawn_builtin(lifecycle, manifest, guard).await;
            }
            ConnectorKind::App => {}
            ConnectorKind::McpHttp => {
                return self
                    .spawn_mcp_http(lifecycle, manifest, &install_path, guard, inherit)
                    .await;
            }
            ConnectorKind::CliQuery => {
                return self
                    .spawn_cli_query(lifecycle, manifest, &install_path, guard, inherit)
                    .await;
            }
        }

        // Read before the token mint and the exec so a refusal leaves nothing behind.
        let (effective, guard) = self
            .config_for_spawn_or_unavailable(lifecycle, manifest, guard)
            .await?;

        // Every spawn mints fresh; the raw value is passed via env and must be echoed back in `initialize`.
        let token = self.ensure_plugin_token(lifecycle).await?;

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

        // Spawn the process. On failure we propagate without touching the
        // live map (the caller releases the admission reservation).
        let process = Arc::new(
            PluginProcess::spawn(manifest, &install_path, &self.plugins_data_dir, &token)
                .map_err(HostError::from)?,
        );

        // Hand stdio over to the MCP client. The supervisor task picks the
        // `Child` up below for `wait()`.
        let (stdin, stdout) = process
            .take_stdio()
            .ok_or_else(|| HostError::Mcp(McpError::TransportClosed("stdio not piped".into())))?;
        let mcp = match McpClient::connect_with_auth(
            stdout,
            stdin,
            InitializeMeta {
                kernel_version: self.kernel_version.to_string().as_str(),
                expected_echo: Some(token.as_str()),
                // `Some` even when empty: 'configuration delivered, none set' and 'kernel predates configuration delivery' are different facts.
                config: Some(&effective),
            },
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                // Failed handshake — try to clean up the child before bailing.
                if let Some(mut child) = process.take_child() {
                    let _ = child.start_kill();
                }
                // An auth-mismatch is a security event, not a transient crash: no supervisor is installed so no respawn fires.
                if matches!(&e, McpError::Framing(m) if m == "auth mismatch") {
                    let reason = "auth handshake failed";
                    // Drop any stale live entry so `status` doesn't report a stale `Running`.
                    let _ = self.lock_table().live.remove(id);
                    self.emit_crashed_under(lifecycle, reason).await;
                    return Err(HostError::AuthMismatch(id.to_string()));
                }
                self.emit_crashed_under(lifecycle, &format!("initialize failed: {e}"))
                    .await;
                return Err(HostError::InitializeRejected(e.to_string()));
            }
        };

        // Install the real `neige.*` router iff the plugin declared the kernel-callbacks capability; the bounded mpsc must always be drained.
        let inbound = match mcp.take_inbound_requests() {
            Some(rx) => rx,
            None => {
                // Somebody else already took it; use a channel that closes immediately so the router exits cleanly.
                let (_tx, rx) = mpsc::channel::<InboundRequest>(1);
                rx
            }
        };
        let inbound_notifs = mcp.take_inbound_notifications();
        let subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let router = if mcp.has_kernel_callbacks_capability(id) {
            spawn_neige_router(
                id.to_string(),
                Arc::clone(&self.callbacks),
                Arc::clone(&self.registry),
                Arc::clone(&mcp),
                Arc::clone(&subscriptions),
                inbound,
                inbound_notifs,
            )
        } else {
            tracing::info!(
                plugin_id = %id,
                "plugin did not declare experimental.dev.neige/kernel-callbacks; \
                 installing MethodNotFound drainer (neige.* calls will fail)"
            );
            spawn_methodnotfound_drainer(id.to_string(), inbound, inbound_notifs)
        };

        // Supervisor task: waits for the child, restarts on unexpected exit.
        let child_handle = process.take_child().ok_or_else(|| {
            HostError::BadState("PluginProcess lost its Child before supervision".into())
        })?;
        // The epoch is allocated BEFORE the supervisor task and the live insert, and handed to the supervisor by value.
        let run_epoch = self.next_run_epoch();
        let supervisor = {
            let host = Arc::clone(self);
            let plugin_id = id.to_string();
            tokio::spawn(async move {
                host.supervise(plugin_id, run_epoch, child_handle).await;
            })
        };

        // Swap the reservation for the live entry and disarm the guard under the SAME lock, so no interleaving sees 'neither reserved nor running' and a later drop cannot release a newer reservation.
        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: Some(process.clone()),
                    // App plugins are ALWAYS `Stdio`; `mcp_client()`'s `(Running, Stdio)` match depends on it.
                    mcp: Some(ConnectorClient::Stdio(mcp.clone())),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    run_epoch,
                    crash_attempt: 0,
                    supervisor: Some(supervisor),
                    router: Some(router),
                    subscriptions,
                    http_socket: socket::RunSocket::declared(
                        manifest,
                        &process::work_dir(&self.plugins_data_dir, id),
                    ),
                },
            );
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
        tracing::info!(plugin_id = %id, "plugin running");

        Ok(())
    }
}
