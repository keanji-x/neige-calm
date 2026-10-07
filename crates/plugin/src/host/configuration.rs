//! configuration responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    /// The effective configuration for a spawn plus the row's `enabled` bit (`None` = no row).
    /// A missing row is `{}`; a DB failure fails the spawn, since defaults could mask an operator's real configuration.
    /// Returns a bare `Err(String)` so the caller must publish `Unavailable` rather than `?` it away.
    pub(super) async fn effective_config_for_spawn(
        &self,
        id: &str,
        manifest: &Manifest,
    ) -> Result<(Option<bool>, serde_json::Map<String, serde_json::Value>), String> {
        let (enabled, user_config) = match self.repo.plugin_get_by_id(id).await {
            Ok(Some(row)) => (Some(row.enabled), row.user_config),
            Ok(None) => (None, serde_json::Value::Object(Default::default())),
            Err(e) => return Err(e.to_string()),
        };
        Ok((enabled, config::effective_config(manifest, &user_config)))
    }

    /// **The** spawn-time configuration gate for every kind: read the effective configuration, refuse if unreadable or a `required` key is missing, and hand the admission reservation back on success.
    /// The `app` path calls it before `emit_state(Spawning)`; the connector paths call it after, so those emit `Spawning` then `Unavailable`.
    /// The guard is taken by value: both failure arms consume it to publish the terminal entry.
    pub(super) async fn config_for_spawn_or_unavailable(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard<E>,
    ) -> Result<
        (
            serde_json::Map<String, serde_json::Value>,
            AdmissionGuard<E>,
        ),
        HostError,
    > {
        let id = lifecycle.id();
        // Stamped first, before either refusal arm: the claim recorded is 'the gate ran', not 'the gate passed'.
        self.config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string());
        let (enabled, effective) = match self.effective_config_for_spawn(id, manifest).await {
            Ok(pair) => pair,
            Err(detail) => {
                // A `?` here would drop `guard` un-disarmed with no live entry and no event, so `status` would answer `None`.
                let reason = format!("could not read stored configuration: {detail}");
                let _ = self
                    .publish_unavailable_under(lifecycle, Some(guard), reason.clone())
                    .await;
                return Err(HostError::ConfigUnreadable {
                    plugin_id: id.to_string(),
                    reason,
                });
            }
        };

        // The operator's `enabled` bit is spawn admission, enforced here because every spawn of every kind goes through this door.
        // A row that says `enabled = false` refuses; no row at all proceeds. No `Unavailable` entry: the operator turned it off, and dropping `guard` releases the reservation.
        if enabled == Some(false) {
            drop(guard);
            return Err(HostError::OperatorDisabled(id.to_string()));
        }

        let missing = config::missing_required(manifest, &effective);
        if !missing.is_empty() {
            let reason = config::missing_required_reason(&missing);
            // `Unavailable`, not `Crashed`: `Crashed` cannot carry the `last_error` via `status`, and nothing crashed — no child was spawned and no supervisor is installed.
            let _ = self
                .publish_unavailable_under(lifecycle, Some(guard), reason.clone())
                .await;
            return Err(HostError::MissingRequiredConfig {
                plugin_id: id.to_string(),
                reason,
            });
        }

        Ok((effective, guard))
    }

    /// Mint + persist a fresh process token; only the hash is kept, so a kernel restart forces every plugin to re-handshake with a fresh credential.
    /// Takes the guard because `uninstall` deletes `plugin_tokens` and the write must be inside the same critical section as the spawn.
    pub async fn ensure_plugin_token(&self, guard: &LifecycleGuard) -> Result<String, HostError> {
        let id = guard.id();
        let raw = PluginToken::generate();
        let hashed = hash_token(raw.as_str());
        self.repo
            .plugin_token_set(id, &hashed, i64::MAX)
            .await
            .map_err(|e| HostError::BadState(format!("plugin_token_set({id}): {e}")))?;
        Ok(raw.into_inner())
    }

    /// Forced rotation: clear the token slot, then restart (the spawn mints the new token), or — when `enabled = false` — stop and delete without restarting.
    pub async fn rotate_plugin_token(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Reject-only pre-lock probe; the same check `rotate_plugin_token_under` opens with.
        self.rotate_admission_check(id)?;
        let guard = self.try_lock_lifecycle(id)?;
        self.rotate_plugin_token_under(&guard).await
    }

    /// `rotate_plugin_token`'s opening checks, shared by the pre-lock probe and the in-guard decision. Reject-only: the returned `Manifest` is authoritative only under the guard.
    pub(super) fn rotate_admission_check(&self, id: &str) -> Result<Manifest, HostError> {
        let Some(manifest) = self.registry.get(id) else {
            return Err(HostError::NotFound(id.to_string()));
        };
        if !manifest.kind.is_app() {
            return Err(HostError::UnsupportedForKind {
                plugin_id: id.to_string(),
                kind: manifest.kind.wire_name(),
                operation: "token rotation (connectors are never issued a plugin token)",
            });
        }
        Ok(manifest)
    }

    pub(super) async fn rotate_plugin_token_under(
        self: &Arc<Self>,
        guard: &LifecycleGuard,
    ) -> Result<(), HostError> {
        let id = guard.id();
        // Refuse for connectors BEFORE the delete and the restart: they never had a token, so rotation would be a no-op delete plus a real stop+respawn. Fail closed on an unknown id.
        let _manifest = self.rotate_admission_check(id)?;
        // Read the `enabled` bit inside the guard, then branch; a read failure fails closed and is inert. An absent row is not a disabled row.
        let enabled = match self.repo.plugin_get_by_id(id).await {
            Ok(Some(row)) => row.enabled,
            Ok(None) => true,
            Err(e) => {
                return Err(HostError::BadState(format!(
                    "rotate `{id}`: the plugin row could not be read, so the \
                     `enabled` bit could not be honoured; nothing was deleted \
                     and nothing was restarted: {e}"
                )));
            }
        };

        if !enabled {
            // The restart is rotation's side effect, not its request: on a disabled plugin, stop (reconciling any orphaned `Running` process) and only then delete, so a plugin that cannot be stopped keeps its token row.
            // `NotFound` from `stop_under` is benign only if verified below: a prior failed stop leaves a `stopping` entry that answers `NotFound` while still `Running`.
            match self.stop_under(guard).await {
                Ok(()) | Err(HostError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
            // `Running` and only `Running`: `status` also answers `Some` for `Crashed`/`Unavailable`/`Spawning`, none of which has a live process behind it; refusing on those would lock the token out forever.
            if let Some(PluginRuntimeStatus::Running) = self.status(id).await.map(|s| s.status) {
                return Err(HostError::BadState(format!(
                    "rotate `{id}`: the plugin is disabled but is still \
                     `Running` after the stop; the token was NOT deleted, \
                     because clearing it while that process keeps running \
                     would leave a live plugin the kernel can no longer \
                     account for"
                )));
            }
            // On this branch the delete IS the whole rotation (nothing follows that rewrites the hash), so a failed delete must not answer `Ok`.
            self.repo.plugin_token_delete(id).await.map_err(|e| {
                HostError::BadState(format!("rotate `{id}`: plugin_token_delete: {e}"))
            })?;
            tracing::info!(
                plugin_id = %id,
                "token rotated for a disabled plugin; not restarted, and any \
                 live process was stopped — `enable` will mint a fresh token \
                 on its next spawn"
            );
            return Ok(());
        }

        // Best-effort: the restart's `ensure_plugin_token` UPSERTs the hash whether or not this DELETE landed, so propagating would refuse a rotation that was going to happen.
        if let Err(e) = self.repo.plugin_token_delete(id).await {
            tracing::warn!(
                plugin_id = %id,
                error = %e,
                "could not delete the plugin's token row; continuing to the \
                 restart — if it reaches `ensure_plugin_token` the UPSERT \
                 overwrites the hash and the rotation completes without this \
                 delete, and if it does not, the old hash outlives it"
            );
        }
        self.restart_under(guard).await
    }

    /// A spawn that reached `Running` without passing `config_for_spawn_or_unavailable`. Quantifies over every successful spawn of every kind, so there is no list to keep up to date.
    /// `debug_assert!` in test builds; a `tracing::error!` plus a count in `config_gate_breaches` in release, since tearing down a healthy process over bookkeeping would be worse.
    pub fn assert_config_gate_ran(&self, id: &str, kind: ConnectorKind) {
        let ran = self
            .config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(id);
        if ran {
            return;
        }
        *self
            .config_gate_breaches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.to_string())
            .or_insert(0) += 1;
        let msg = format!(
            "#1284 §4.7: plugin `{id}` (kind `{}`) completed a spawn without passing \
             `config_for_spawn_or_unavailable` — its effective configuration and its \
             `required` verdict came from somewhere else, or from nowhere",
            kind.wire_name()
        );
        tracing::error!("{msg}");
        debug_assert!(false, "{msg}");
    }

    /// Whether `id`'s most recent spawn attempt passed the shared configuration gate — NOT whether it succeeded or is running now.
    pub fn config_gate_ran(&self, id: &str) -> bool {
        self.config_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(id)
    }

    /// How many times `id` completed a spawn without the witness; zero on any healthy build.
    pub fn config_gate_breaches(&self, id: &str) -> u64 {
        self.config_gate_breaches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .copied()
            .unwrap_or(0)
    }
}
