//! The composite plugin lifecycle operations — install / enable / disable / uninstall /
//! reload — each run inside one per-id `LifecycleGuard` lifetime.

use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use super::managed::{self, ConnectorInstall};
use super::ports::{NewPlugin, PluginRecord as Plugin, Store};
use super::{HostError, LifecycleGuard, Manifest, PluginHost, check_min_kernel_version};
use crate::ports::ErrorFactory;

type Result<T, E> = std::result::Result<T, E>;

/// The plugin-row operations the lifecycle machinery performs, behind a port narrow enough to fake.
#[async_trait]
pub trait LifecycleDb<E: ErrorFactory>: Send + Sync {
    /// `Ok(None)` — no such row; `Err` — the read itself failed and the caller must not guess.
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>, E>;

    /// Set the row's `enabled` bit; propagates the repo's `NotFound` for a missing row.
    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), E>;
}

/// Production implementation: straight delegation to the host's repo.
pub(super) struct RepoLifecycleDb<E: ErrorFactory> {
    repo: Arc<dyn Store<E>>,
}

impl<E: ErrorFactory> RepoLifecycleDb<E> {
    pub(super) fn new(repo: Arc<dyn Store<E>>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl<E: ErrorFactory> LifecycleDb<E> for RepoLifecycleDb<E> {
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>, E> {
        Ok(self.repo.plugin_get_by_id(id).await?.map(|p| p.enabled))
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), E> {
        self.repo.plugin_update_enabled(id, enabled).await?;
        Ok(())
    }
}

impl<E: ErrorFactory> PluginHost<E> {
    /// Never record a plugin this kernel could not spawn. Nothing here writes, so it may run before the guard.
    fn check_min_kernel(&self, manifest: &Manifest) -> Result<(), E> {
        let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
            E::plugin_install(format!(
                "manifest min_kernel_version `{}` is not valid semver: {e}",
                manifest.min_kernel_version
            ))
        })?;
        if let Err(err) = check_min_kernel_version(&self.kernel_version, &required) {
            return Err(E::plugin_kernel_too_old(format!(
                "plugin `{}` requires kernel >= {}, this kernel is {}",
                manifest.id, err.required, err.actual,
            )));
        }
        Ok(())
    }

    pub async fn install(&self, manifest: Manifest, src_path: &StdPath) -> Result<Plugin, E> {
        self.check_min_kernel(&manifest)?;

        // Everything below is one critical section: without the guard the duplicate-id probe and the insert are a TOCTOU pair.
        let guard = self
            .try_lock_lifecycle(&manifest.id)
            .map_err(spawn_error_to_calm::<E>)?;
        self.install_under(&guard, manifest, |install_dir| {
            materialize_install_tree(src_path, install_dir)
        })
        .await
    }

    /// Install an `mcp-http` connector the kernel synthesizes itself. The tree is written inside
    /// the guard, at the point the path-based install materializes its symlink.
    pub async fn install_managed_connector(
        &self,
        connector: &ConnectorInstall,
    ) -> Result<Plugin, E> {
        let text = serde_json::to_string_pretty(&connector.manifest_json(&self.kernel_version))
            .map_err(|e| E::plugin_install(format!("serializing manifest: {e}")))?;
        let (manifest, secrets) = connector
            .prepare(&self.kernel_version)
            .map_err(E::plugin_install)?;
        self.check_min_kernel(&manifest)?;

        let guard = self
            .try_lock_lifecycle(&manifest.id)
            .map_err(spawn_error_to_calm::<E>)?;
        // From the manifest — the id `install_under` joins — so cleanup cannot aim at a path the install never wrote.
        let install_dir = self.plugins_dir.join(&manifest.id);
        // Whether this call wrote the tree, not whether one exists: a duplicate-id refusal leaves the previous install's tree at this path.
        let wrote_tree = std::sync::atomic::AtomicBool::new(false);
        let outcome = self
            .install_under(&guard, manifest, |dir| {
                let written =
                    managed::write_connector_tree(dir, &text, &secrets, &self.kernel_version);
                if written.is_ok() {
                    wrote_tree.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                written.map_err(|e| match e {
                    managed::WriteError::Occupied(_) => E::plugin_dir_occupied(e.to_string()),
                    managed::WriteError::Io(_) => E::plugin_install(e.to_string()),
                })
            })
            .await;

        // A failure after the tree was written must not leave its `secrets.json` behind.
        if outcome.is_err()
            && wrote_tree.load(std::sync::atomic::Ordering::Relaxed)
            && let Err(msg) = managed::remove_managed_tree(&install_dir)
        {
            tracing::warn!(target: "plugin_host", "{msg}");
        }
        outcome
    }

    /// The half of install that runs under the guard, shared by both sources.
    async fn install_under(
        &self,
        guard: &LifecycleGuard,
        manifest: Manifest,
        place_tree: impl FnOnce(&StdPath) -> Result<(), E>,
    ) -> Result<Plugin, E> {
        if crate::builtin::is_reserved(&manifest.id)
            || manifest.kind == super::ConnectorKind::Builtin
        {
            return Err(E::plugin_install(
                "built-in components cannot be installed from a directory or connector".into(),
            ));
        }
        if let Some(prev) = self.repo.plugin_get_by_id(&manifest.id).await? {
            return Err(E::plugin_conflict(format!(
                "plugin `{}` already installed at version `{}`",
                prev.id, prev.version
            )));
        }
        // A new plugin's id is one word (#2087 §6), or one of the five grandfathered external ids
        // that carry `.` or `-` (§9).
        if !super::manifest::is_installable_plugin_id(&manifest.id) {
            return Err(E::plugin_install(format!(
                "manifest id `{}` must be one word: ^[a-z0-9]{{2,32}}$ (no `.`, `-` or `_`)",
                manifest.id
            )));
        }

        // The install path the registry remembers is the in-plugins-dir target, not the user-supplied source.
        let install_dir = self.plugins_dir.join(&manifest.id);
        place_tree(&install_dir)?;

        let new_plugin = NewPlugin {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            install_path: install_dir.to_string_lossy().into_owned(),
            manifest: manifest.to_json(),
            enabled: false,
            user_config: serde_json::json!({}),
        };
        let plug = self.repo.plugin_install(new_plugin).await?;

        self.registry_insert(guard, manifest, Some(install_dir));
        self.emit_state_under(guard, &super::PluginRuntimeStatus::Disabled)
            .await;

        Ok(plug)
    }

    /// Set the plugin enabled and running; returns the row re-read after the spawn. A plugin already
    /// running (or spawning) is in that state, so it answers its row.
    /// A spawn refused with a 4xx restores the bit this call found, inside the guard, so the refusal
    /// changed nothing. A 503 wait or a 5xx keeps `enabled = true`, so autospawn keeps trying.
    pub async fn enable(self: &Arc<Self>, id: &str) -> Result<Plugin, E> {
        // The 404 probe stays before the guard: a guard taken first would turn "unknown id AND busy" into a 409.
        self.plugin_row_or_404(id).await?;
        let guard = self
            .try_lock_lifecycle(id)
            .map_err(spawn_error_to_calm::<E>)?;
        // Read inside the guard: it is the value a refusal restores.
        let found = self
            .lifecycle_db
            .enabled_row(id)
            .await?
            .ok_or_else(|| E::not_found(format!("plugin {id}")))?;
        self.lifecycle_db.set_enabled(id, true).await?;
        // A conflict refusal of a row found disabled is rolled back below, so it publishes nothing.
        let report = if found {
            super::ConflictReport::Publish
        } else {
            super::ConflictReport::Silent
        };
        match self.spawn_under_reporting(&guard, None, report).await {
            Ok(()) | Err(HostError::AlreadyRunning(_)) => {}
            Err(e) => {
                let unpublished = report == super::ConflictReport::Silent
                    && matches!(
                        e,
                        HostError::TemplateConflict { .. } | HostError::MintedNameConflict { .. }
                    );
                let answer = spawn_error_to_calm::<E>(e);
                if answer.is_client_refusal()
                    && !found
                    && let Err(rollback) = self.lifecycle_db.set_enabled(id, false).await
                {
                    // The row stays enabled after all, so the refusal the spawn left unpublished is
                    // published now: an enabled plugin says why it is not running.
                    if unpublished {
                        self.emit_crashed_under(&guard, &answer.reason()).await;
                    }
                    return Err(rollback);
                }
                return Err(answer);
            }
        }
        self.plugin_row_or_404(id).await
    }

    /// Stop, then flip `enabled = false`. Stop first: `enabled = false` beside a still-running plugin is
    /// never reconciled, whereas stopped-but-`enabled` is brought back on the next boot.
    pub async fn disable(self: &Arc<Self>, id: &str) -> Result<Plugin, E> {
        // The 404 probe stays before the guard: a guard taken first would turn "unknown id AND busy" into a 409.
        self.plugin_row_or_404(id).await?;
        if crate::builtin::get(id).is_some_and(|component| !component.can_disable()) {
            return Err(E::bad_request(
                "this built-in component is always enabled".into(),
            ));
        }
        let guard = self
            .try_lock_lifecycle(id)
            .map_err(spawn_error_to_calm::<E>)?;
        match self.stop_under(&guard).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(E::internal(format!("stop failed: {e}"))),
        }
        self.lifecycle_db.set_enabled(id, false).await?;
        self.plugin_row_or_404(id).await
    }

    /// Stop, then tear down every trace of the plugin except an operator-owned on-disk tree.
    /// The token / kv / overlay cascade deliberately swallows its errors; `plugin_delete` is the one write reported.
    pub async fn uninstall(self: &Arc<Self>, id: &str) -> Result<(), E> {
        if crate::builtin::is_reserved(id) {
            return Err(E::bad_request(
                "built-in components cannot be uninstalled".into(),
            ));
        }
        // Probe before guard: taking the guard first would answer 409 for an unknown id that happens to be busy.
        self.plugin_row_or_404(id).await?;
        let guard = self
            .try_lock_lifecycle(id)
            .map_err(spawn_error_to_calm::<E>)?;
        // Read inside the guard, never from the probe: a concurrent install re-materializes exactly this path.
        let row = self.repo.plugin_get_by_id(id).await?;
        // Stop first so the process can't write into state we're about to delete. NotFound is fine (already stopped).
        match self.stop_under(&guard).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(E::internal(format!("stop failed: {e}"))),
        }
        // Token + kv are FK-cascaded on sqlite but other backends won't have that; overlays have no FK at all.
        let _ = self.repo.plugin_token_delete(id).await;
        let _ = self.repo.plugin_kv_clear(id).await;
        let _ = self.repo.overlays_clear_by_plugin(id).await;
        self.repo.plugin_delete(id).await?;
        self.registry_remove(&guard);
        self.emit_state_under(&guard, &super::PluginRuntimeStatus::Disabled)
            .await;

        // The on-disk tree is left in place unless the kernel wrote it (a synthesized connector's tree holds
        // the `secrets.json` this uninstall was asked to forget). Best-effort: the row is already gone.
        if let Some(row) = row {
            match managed::remove_managed_tree(StdPath::new(&row.install_path)) {
                Ok(true) => {
                    tracing::info!(
                        target: "plugin_host",
                        plugin = %id,
                        path = %row.install_path,
                        "removed kernel-managed plugin tree"
                    );
                }
                Ok(false) => {}
                Err(msg) => tracing::warn!(target: "plugin_host", plugin = %id, "{msg}"),
            }
        }
        Ok(())
    }

    /// Dev hot-reload: stop, re-read the manifest from disk, re-validate, republish it, and respawn only
    /// if the row read inside the guard said `enabled`. The pre-guard probe is an existence check only.
    pub async fn reload(self: &Arc<Self>, id: &str) -> Result<Plugin, E> {
        // Probe before guard: without it an unknown id falls through to the manifest read and returns a 400. Its value is deliberately dropped.
        if self.lifecycle_db.enabled_row(id).await?.is_none() {
            return Err(E::not_found(format!("plugin {id}")));
        }
        let guard = self
            .try_lock_lifecycle(id)
            .map_err(spawn_error_to_calm::<E>)?;
        // The decision row. Read here, inside the guard, and NOT before it.
        let plug = self.plugin_row_or_404(id).await?;
        // Stop first (NotFound is fine — could have crashed).
        match self.stop_under(&guard).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(E::internal(format!("stop failed: {e}"))),
        }
        if let Some(component) = crate::builtin::get(id) {
            let manifest = component.manifest().clone();
            self.registry_insert(&guard, manifest.clone(), None);
            self.repo
                .plugin_update_manifest(id, manifest.to_json())
                .await?;
            if plug.enabled {
                self.spawn_under(&guard, None)
                    .await
                    .map_err(spawn_error_to_calm::<E>)?;
            }
            return self.plugin_row_or_404(id).await;
        }
        let install_dir = PathBuf::from(&plug.install_path);
        let manifest_path = install_dir.join("manifest.json");
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| E::plugin_install(format!("reading {}: {e}", manifest_path.display())))?;
        let manifest =
            Manifest::parse(&manifest_text).map_err(|e| E::plugin_install(e.to_string()))?;
        if manifest.kind == super::ConnectorKind::Builtin
            || crate::builtin::is_reserved(&manifest.id)
        {
            return Err(E::plugin_install(
                "built-in declarations cannot be loaded from disk".into(),
            ));
        }
        if manifest.id != id {
            return Err(E::plugin_install(format!(
                "manifest id changed during reload: was `{id}`, now `{}`",
                manifest.id
            )));
        }

        // Pre-check before mutating the registry or DB: a clean 422, not a half-applied reload.
        let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
            E::plugin_install(format!(
                "manifest min_kernel_version `{}` is not valid semver: {e}",
                manifest.min_kernel_version
            ))
        })?;
        if let Err(err) = check_min_kernel_version(&self.kernel_version, &required) {
            return Err(E::plugin_kernel_too_old(format!(
                "plugin `{id}` requires kernel >= {}, this kernel is {}",
                err.required, err.actual,
            )));
        }

        // Persist the on-disk manifest to the row so `GET /api/plugins/:id` reflects it.
        let manifest_value = serde_json::to_value(&manifest)
            .map_err(|e| E::internal(format!("manifest re-serialize after reload: {e}")))?;
        self.registry_insert(&guard, manifest, Some(install_dir));
        self.repo.plugin_update_manifest(id, manifest_value).await?;
        if plug.enabled
            && let Err(e) = self.spawn_under(&guard, None).await
        {
            return Err(spawn_error_to_calm(e));
        }
        self.plugin_row_or_404(id).await
    }

    /// Read the plugin row or produce the route's exact 404.
    async fn plugin_row_or_404(&self, id: &str) -> Result<Plugin, E> {
        self.repo
            .plugin_get_by_id(id)
            .await?
            .ok_or_else(|| E::not_found(format!("plugin {id}")))
    }
}

/// Translate a `PluginHost::spawn` failure into a route-shaped `CalmError`.
pub fn spawn_error_to_calm<E: ErrorFactory>(e: HostError) -> E {
    match e {
        HostError::KernelTooOld(k) => E::plugin_kernel_too_old(format!(
            "plugin requires kernel >= {}, this kernel is {}",
            k.required, k.actual,
        )),
        conflict @ (HostError::TemplateConflict { .. } | HostError::MintedNameConflict { .. }) => {
            E::plugin_conflict(conflict.to_string())
        }
        // Not a kernel fault: 503 carries the reason and the row stays `enabled`, so a re-enable is the whole recovery.
        unavailable @ HostError::ConnectorUnavailable { .. } => {
            E::service_unavailable(unavailable.to_string())
        }
        // Same reasoning: the stored configuration is incomplete; 503, row stays `enabled`.
        missing @ HostError::MissingRequiredConfig { .. } => {
            E::service_unavailable(missing.to_string())
        }
        // Same reasoning, and consistent with the `unavailable` live entry the spawn path publishes.
        unreadable @ HostError::ConfigUnreadable { .. } => {
            E::service_unavailable(unreadable.to_string())
        }
        unsupported @ HostError::UnsupportedForKind { .. } => {
            E::bad_request(unsupported.to_string())
        }
        // 409 with its own code: `enable` writes `enabled = true` before spawning, so a 500 would claim a permanent fault for a request that did nothing and can be repeated.
        busy @ HostError::LifecycleBusy(_) => E::plugin_busy(busy.to_string()),
        // 409 `plugin_conflict`: the state it conflicts with is the operator's own and `enable` is the remedy.
        // Not `plugin_busy` (retrying the identical request will not work) and not 503 (it is not trying on purpose).
        disabled @ HostError::OperatorDisabled(_) => E::plugin_conflict(disabled.to_string()),
        other => E::internal(format!("spawn failed: {other}")),
    }
}

/// Materialize the install tree at `dst`. `src == dst` is the dev shortcut where `plugins_dir` holds the working copy.
fn materialize_install_tree<E: ErrorFactory>(src: &StdPath, dst: &StdPath) -> Result<(), E> {
    if src == dst {
        return Ok(());
    }
    if dst.exists() {
        // A stale dst from a prior failed install — best-effort clean.
        // Symlinks need symlink_metadata to know not to follow.
        let md = std::fs::symlink_metadata(dst);
        match md {
            Ok(m) if m.file_type().is_symlink() => {
                std::fs::remove_file(dst).map_err(|e| {
                    E::plugin_install(format!(
                        "removing stale install link {}: {e}",
                        dst.display()
                    ))
                })?;
            }
            Ok(m) if m.is_dir() => {
                std::fs::remove_dir_all(dst).map_err(|e| {
                    E::plugin_install(format!("removing stale install dir {}: {e}", dst.display()))
                })?;
            }
            _ => {}
        }
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            E::plugin_install(format!("creating plugins parent {}: {e}", parent.display()))
        })?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).map_err(|e| {
            E::plugin_install(format!(
                "symlink {} → {}: {e}",
                src.display(),
                dst.display()
            ))
        })?;
        Ok(())
    }

    // Windows / other: deep-copy the tree; symlinks need admin on Windows.
    #[cfg(not(unix))]
    {
        copy_dir_recursive(src, dst).map_err(|e| {
            E::plugin_install(format!(
                "copying {} → {}: {e}",
                src.display(),
                dst.display()
            ))
        })?;
        Ok(())
    }
}

#[cfg(not(unix))]
fn copy_dir_recursive(src: &StdPath, dst: &StdPath) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_child = dst.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_child)?;
        } else {
            std::fs::copy(entry.path(), &dst_child)?;
        }
    }
    Ok(())
}
