//! supervision responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    /// Supervisor loop for one plugin. Boxed because `supervise` ↔ `spawn` form a mutual recursion through `tokio::spawn`, which auto-Send inference can't see through.
    pub(super) fn supervise(
        self: Arc<Self>,
        id: String,
        run_epoch: u64,
        child: tokio::process::Child,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(self.supervise_inner(id, run_epoch, child))
    }

    /// The supervisor in three segments: [lock held] account the crash and emit `crashed`; [no lock] sleep out the backoff; [lock re-taken] re-decide everything and respawn.
    pub(super) async fn supervise_inner(
        self: Arc<Self>,
        id: String,
        run_epoch: u64,
        mut child: tokio::process::Child,
    ) {
        let exit_result = child.wait().await;

        // `await_lifecycle`, not `try`: the task was created under the spawn's own guard, so a child that dies in that window collides with it by construction.
        let (attempt, delay_ms) = {
            let guard = self.await_lifecycle(&id).await;

            // Is this still MY run instance? An old supervisor that only now got the lock must not mark a NEW entry crashed.
            let observed = {
                let table = self.lock_table();
                table.live.get(&id).map(|rp| {
                    (
                        rp.run_epoch,
                        rp.stopping,
                        matches!(rp.status, PluginRuntimeStatus::Running),
                    )
                })
            };
            match observed {
                // Removed by `stop`/`uninstall`, or replaced by a newer run:
                // not ours to report.
                None => {
                    tracing::info!(plugin_id = %id, "plugin exited; its live entry is gone");
                    return;
                }
                Some((epoch, _, _)) if epoch != run_epoch => {
                    tracing::info!(
                        plugin_id = %id,
                        "plugin exited; a newer run instance owns the entry"
                    );
                    return;
                }
                Some((_, true, _)) => {
                    tracing::info!(plugin_id = %id, "plugin exited gracefully");
                    return;
                }
                Some((_, false, false)) => {
                    // Somebody already wrote a terminal state for this exact
                    // run (`Crashed` from the auth-mismatch path, `Unavailable`
                    // from a boot fence). Not a second crash.
                    tracing::info!(
                        plugin_id = %id,
                        "plugin exited but its run instance is no longer Running"
                    );
                    return;
                }
                Some((_, false, true)) => {}
            }

            let reason = match exit_result {
                Ok(status) => format!("exited with {status}"),
                Err(e) => format!("wait failed: {e}"),
            };
            tracing::warn!(plugin_id = %id, reason = %reason, "plugin exited unexpectedly");

            // Snapshot stderr tail so the crash event carries useful detail.
            let tail = {
                let table = self.lock_table();
                table
                    .live
                    .get(&id)
                    .and_then(|rp| rp.process.as_ref())
                    .map(|p| p.stderr_tail(10).join("\n"))
                    .unwrap_or_default()
            };
            let combined_reason = if tail.is_empty() {
                reason
            } else {
                format!("{reason}\nstderr tail:\n{tail}")
            };

            // Crash-window bookkeeping.
            let (attempts, attempt, exceeded) = {
                let mut table = self.lock_table();
                let Some(entry) = table.live.get_mut(&id) else {
                    return;
                };
                if entry.window_started.elapsed() > self.backoff.crash_window {
                    entry.window_started = Instant::now();
                    entry.crashes_in_window = 0;
                }
                entry.crashes_in_window += 1;
                entry.crash_attempt += 1;
                entry.status = PluginRuntimeStatus::Crashed {
                    reason: combined_reason.clone(),
                };
                (
                    entry.crashes_in_window,
                    entry.crash_attempt,
                    entry.crashes_in_window >= self.backoff.crash_window_limit,
                )
            };

            self.emit_crashed_under(&guard, &combined_reason).await;

            if exceeded {
                tracing::error!(
                    plugin_id = %id,
                    attempts,
                    "plugin exceeded crash-window limit; not respawning",
                );
                // Leave the Crashed entry so `status()` returns it; drop the supervisor handle so it gets reaped.
                // Epoch-checked so a raced `stop`+`spawn`'s NEW entry keeps its handle.
                let mut table = self.lock_table();
                if let Some(rp) = table.live.get_mut(&id)
                    && rp.run_epoch == run_epoch
                {
                    rp.supervisor = None;
                }
                return;
            }

            // Backoff then respawn. Index by (attempts - 1) clamped to the table.
            let idx = (attempts as usize).saturating_sub(1);
            let delay_ms = self
                .backoff
                .schedule_ms
                .get(idx)
                .copied()
                .unwrap_or_else(|| *self.backoff.schedule_ms.last().expect("non-empty schedule"));
            tracing::info!(
                plugin_id = %id,
                delay_ms,
                attempts,
                "scheduling plugin respawn",
            );
            (attempt, delay_ms)
        };

        // Lock NOT held: a `disable` during a crash loop must not block for the whole backoff.
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;

        self.respawn_after_backoff(&id, run_epoch, attempt).await;
    }

    /// Segment 3 of the supervisor: re-derive everything after the sleep. The manifest is NOT carried over: a `reload` during the backoff may have replaced it.
    pub(super) async fn respawn_after_backoff(
        self: &Arc<Self>,
        id: &str,
        run_epoch: u64,
        attempt: u64,
    ) {
        for retry in 0..LIFECYCLE_DB_READ_RETRIES {
            let guard = self.await_lifecycle(id).await;

            // (a) still our run instance, still the Crashed state WE wrote,
            //     not being stopped, and no further crash has happened.
            let ok = {
                let table = self.lock_table();
                match table.live.get(id) {
                    Some(rp) => {
                        rp.run_epoch == run_epoch
                            && rp.crash_attempt == attempt
                            && !rp.stopping
                            && matches!(rp.status, PluginRuntimeStatus::Crashed { .. })
                    }
                    None => false,
                }
            };
            if !ok {
                tracing::info!(
                    plugin_id = %id,
                    "backoff elapsed but the run instance is gone or has moved on; not respawning"
                );
                return;
            }

            // (b) still installed.
            if self.registry.get(id).is_none() {
                tracing::info!(
                    plugin_id = %id,
                    "backoff elapsed but the plugin left the registry; not respawning"
                );
                return;
            }

            // Still enabled, per the DB. Fail closed: the route layer can write the plugin row without the host, so a read failure must not be treated as 'probably still enabled'.
            match self.lifecycle_db.enabled_row(id).await {
                Ok(Some(true)) => {}
                Ok(Some(false)) | Ok(None) => {
                    tracing::info!(
                        plugin_id = %id,
                        "backoff elapsed but the plugin is no longer enabled; not respawning"
                    );
                    return;
                }
                Err(e) => {
                    drop(guard);
                    tracing::warn!(
                        plugin_id = %id,
                        error = %e,
                        retry,
                        "could not read the plugin row after backoff; keeping Crashed and retrying"
                    );
                    tokio::time::sleep(LIFECYCLE_DB_READ_RETRY_DELAY).await;
                    continue;
                }
            }

            // Carry the crash window ACROSS the remove; otherwise every respawn starts from zero.
            let carried = {
                let mut table = self.lock_table();
                let carried = table.live.get(id).map(|rp| CrashWindow {
                    crashes: rp.crashes_in_window,
                    started: rp.window_started,
                });
                table.live.remove(id);
                carried
            };
            if let Err(e) = self.spawn_under(&guard, carried).await {
                tracing::error!(plugin_id = %id, error = %e, "respawn failed");
                self.emit_crashed_under(&guard, &format!("respawn failed: {e}"))
                    .await;
            }
            return;
        }
        // The exhausted terminal must be explicit and observable, not only a log line.
        // Epoch-checked: if the world moved while we slept, whoever moved it owns the terminal.
        let guard = self.await_lifecycle(id).await;
        let still_ours = {
            let table = self.lock_table();
            match table.live.get(id) {
                Some(rp) => {
                    rp.run_epoch == run_epoch
                        && rp.crash_attempt == attempt
                        && !rp.stopping
                        && matches!(rp.status, PluginRuntimeStatus::Crashed { .. })
                }
                None => false,
            }
        };
        if still_ours {
            let reason = format!(
                "plugin `{id}` crashed and the kernel gave up respawning it: its \
                 database row could not be read {LIFECYCLE_DB_READ_RETRIES} times in a \
                 row, so whether it is still enabled is unknown and a respawn would \
                 be a guess. Nothing will retry automatically — enable (or spawn) it \
                 explicitly once the database is readable again."
            );
            self.publish_unavailable_under(&guard, None, reason).await;
        }
        tracing::error!(
            plugin_id = %id,
            republished = still_ours,
            "gave up respawning after {LIFECYCLE_DB_READ_RETRIES} failed plugin-row reads"
        );
    }
}
