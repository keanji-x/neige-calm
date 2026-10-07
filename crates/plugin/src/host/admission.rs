//! admission responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    /// Spawn a plugin by id. Returns `Ok(())` once `initialize` has handshaken and the supervisor is wired.
    pub async fn spawn(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        self.spawn_admission_check(id)?;
        let guard = self.try_lock_lifecycle(id)?;
        self.spawn_under(&guard, None).await
    }

    /// `spawn_under`'s opening pair — config-disabled, then registered — shared with the pre-lock probe in `spawn` / `restart` so an unknown id cannot mint a lock cell.
    /// Reject-only for the pre-lock callers: the `Manifest` is authoritative only under the guard.
    pub(super) fn spawn_admission_check(&self, id: &str) -> Result<Manifest, HostError> {
        if self.plugins_disabled.iter().any(|d| d == id) {
            return Err(HostError::Disabled(id.to_string()));
        }
        self.registry
            .get(id)
            .ok_or_else(|| HostError::NotFound(id.to_string()))
    }

    /// `spawn` for a caller that already holds the guard. `inherit` carries the crash-window counters across a supervisor respawn; `None` reads the live entry, so an explicit spawn after a crash does not zero them.
    pub(super) async fn spawn_under(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        self.spawn_under_reporting(lifecycle, inherit, ConflictReport::Publish)
            .await
    }

    /// [`Self::spawn_under`], with the caller deciding whether a conflict refusal publishes `crashed`.
    pub(super) async fn spawn_under_reporting(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        inherit: Option<CrashWindow>,
        report: ConflictReport,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
        let manifest = self.spawn_admission_check(id)?;

        // Refuse to spawn plugins that demand a newer kernel; parse failures were already caught by `Manifest::validate`.
        let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
            HostError::BadState(format!(
                "plugin `{id}` has an unparseable min_kernel_version `{}` \
                 (should have been rejected at manifest load): {e}",
                manifest.min_kernel_version
            ))
        })?;
        if let Err(err) = check_min_kernel_version(&self.kernel_version, &required) {
            tracing::warn!(
                plugin_id = %id,
                required = %err.required,
                actual = %err.actual,
                "plugin '{id}' requires kernel >= {}, this kernel is {} — refusing to load",
                err.required,
                err.actual,
            );
            return Err(HostError::KernelTooOld(err));
        }

        // Atomic admission under ONE table lock: refuse already running/admitted, check template-id uniqueness against running ∧ admitted holders, then reserve.
        // Ordered before the token mint so a refusal has zero side effects.
        let admission = {
            let mut table = self.lock_table();
            if table.spawning.contains(id) {
                return Err(HostError::AlreadyRunning(id.to_string()));
            }
            if let Some(rp) = table.live.get(id)
                && matches!(
                    rp.status,
                    PluginRuntimeStatus::Running | PluginRuntimeStatus::Spawning
                )
            {
                // Crashed→spawn is the recovery path; the supervisor cleared
                // its handle, so we treat that as "go ahead".
                return Err(HostError::AlreadyRunning(id.to_string()));
            }
            match find_minted_name_conflict(
                id,
                &manifest.exposes_tools,
                &table.template_holder_ids(),
                self.registry.list(),
            )
            .or_else(|| {
                find_template_conflict(
                    &manifest,
                    self.registry.list(),
                    &table.template_holder_ids(),
                    &|id| self.backends.trusted_forge(id),
                )
            }) {
                Some(conflict) => Err(conflict),
                None => {
                    table.spawning.insert(id.to_string());
                    // The reservation's lifetime is owned by the RAII guard; only the success path's atomic swap disarms.
                    Ok(AdmissionGuard::new(Arc::clone(self), id.to_string()))
                }
            }
        };
        let admitted = match admission {
            Ok(guard) => guard,
            Err(conflict) => {
                tracing::warn!(
                    plugin_id = %id,
                    error = %conflict,
                    "refusing to spawn plugin with a conflicting template id or minted name"
                );
                // Surface the refusal as a failed `PluginState` event so operators see why the plugin isn't running.
                if report == ConflictReport::Publish {
                    self.emit_crashed_under(lifecycle, &conflict.to_string())
                        .await;
                }
                return Err(conflict);
            }
        };

        self.spawn_admitted(lifecycle, &manifest, admitted, inherit)
            .await
    }

    /// Everything downstream of a successful admission reservation. Owns the `AdmissionGuard`: every failure exit drops it (releasing the reservation); the success swap disarms it.
    /// Clears the config-gate witness, runs the kind arm, and asserts the witness on success.
    pub(super) async fn spawn_admitted(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard<E>,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        self.config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(lifecycle.id());
        let outcome = self
            .spawn_admitted_inner(lifecycle, manifest, guard, inherit)
            .await;
        if outcome.is_ok() {
            self.assert_config_gate_ran(lifecycle.id(), manifest.kind);
        }
        outcome
    }
}
