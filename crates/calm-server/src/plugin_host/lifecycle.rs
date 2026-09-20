//! The composite plugin lifecycle operations — install / enable / disable / uninstall /
//! reload — each run inside one per-id `LifecycleGuard` lifetime.

use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use super::managed::{self, ConnectorInstall};
use super::{
    HostError, KERNEL_VERSION, LifecycleGuard, Manifest, PluginHost, check_min_kernel_version,
};
use crate::db::RouteRepo;
use crate::error::CalmError;
use crate::model::{NewPlugin, Plugin};

type Result<T> = std::result::Result<T, CalmError>;

/// The plugin-row operations the lifecycle machinery performs, behind a port narrow enough to fake.
#[async_trait]
pub trait LifecycleDb: Send + Sync {
    /// `Ok(None)` — no such row; `Err` — the read itself failed and the caller must not guess.
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>>;

    /// Set the row's `enabled` bit; propagates the repo's `NotFound` for a missing row.
    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<()>;
}

/// Production implementation: straight delegation to the host's repo.
pub(super) struct RepoLifecycleDb {
    repo: Arc<dyn RouteRepo>,
}

impl RepoLifecycleDb {
    pub(super) fn new(repo: Arc<dyn RouteRepo>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl LifecycleDb for RepoLifecycleDb {
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>> {
        Ok(self.repo.plugin_get_by_id(id).await?.map(|p| p.enabled))
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.repo.plugin_update_enabled(id, enabled).await?;
        Ok(())
    }
}

impl PluginHost {
    /// Never record a plugin this kernel could not spawn. Nothing here writes, so it may run before the guard.
    fn check_min_kernel(&self, manifest: &Manifest) -> Result<()> {
        let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
            CalmError::PluginInstall(format!(
                "manifest min_kernel_version `{}` is not valid semver: {e}",
                manifest.min_kernel_version
            ))
        })?;
        if let Err(err) = check_min_kernel_version(&KERNEL_VERSION, &required) {
            return Err(CalmError::PluginKernelTooOld(format!(
                "plugin `{}` requires kernel >= {}, this kernel is {}",
                manifest.id, err.required, err.actual,
            )));
        }
        Ok(())
    }

    pub async fn install(&self, manifest: Manifest, src_path: &StdPath) -> Result<Plugin> {
        self.check_min_kernel(&manifest)?;

        // Everything below is one critical section: without the guard the duplicate-id probe and the insert are a TOCTOU pair.
        let guard = self
            .try_lock_lifecycle(&manifest.id)
            .map_err(spawn_error_to_calm)?;
        self.install_under(&guard, manifest, |install_dir| {
            materialize_install_tree(src_path, install_dir)
        })
        .await
    }

    /// Install an `mcp-http` connector the kernel synthesizes itself. The tree is written inside
    /// the guard, at the point the path-based install materializes its symlink.
    pub async fn install_managed_connector(&self, connector: &ConnectorInstall) -> Result<Plugin> {
        let text = serde_json::to_string_pretty(&connector.manifest_json())
            .map_err(|e| CalmError::PluginInstall(format!("serializing manifest: {e}")))?;
        let (manifest, secrets) = connector.prepare().map_err(CalmError::PluginInstall)?;
        self.check_min_kernel(&manifest)?;

        let guard = self
            .try_lock_lifecycle(&manifest.id)
            .map_err(spawn_error_to_calm)?;
        // From the manifest — the id `install_under` joins — so cleanup cannot aim at a path the install never wrote.
        let install_dir = self.plugins_dir.join(&manifest.id);
        // Whether this call wrote the tree, not whether one exists: a duplicate-id refusal leaves the previous install's tree at this path.
        let wrote_tree = std::sync::atomic::AtomicBool::new(false);
        let outcome = self
            .install_under(&guard, manifest, |dir| {
                let written = managed::write_connector_tree(dir, &text, &secrets);
                if written.is_ok() {
                    wrote_tree.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                written.map_err(|e| match e {
                    managed::WriteError::Occupied(_) => CalmError::PluginConflict(e.to_string()),
                    managed::WriteError::Io(_) => CalmError::PluginInstall(e.to_string()),
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
        place_tree: impl FnOnce(&StdPath) -> Result<()>,
    ) -> Result<Plugin> {
        if let Some(prev) = self.repo.plugin_get_by_id(&manifest.id).await? {
            return Err(CalmError::PluginConflict(format!(
                "plugin `{}` already installed at version `{}`",
                prev.id, prev.version
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

        Ok(plug)
    }

    /// Flip `enabled = true` and spawn; returns the row re-read after the spawn.
    /// Spawn errors leave `enabled = true` so autospawn keeps trying.
    pub async fn enable(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        // The 404 probe stays before the guard: a guard taken first would turn "unknown id AND busy" into a 409.
        self.plugin_row_or_404(id).await?;
        let guard = self.try_lock_lifecycle(id).map_err(spawn_error_to_calm)?;
        self.lifecycle_db.set_enabled(id, true).await?;
        if let Err(e) = self.spawn_under(&guard, None).await {
            return Err(spawn_error_to_calm(e));
        }
        self.plugin_row_or_404(id).await
    }

    /// Stop, then flip `enabled = false`. Stop first: `enabled = false` beside a still-running plugin is
    /// never reconciled, whereas stopped-but-`enabled` is brought back on the next boot.
    pub async fn disable(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        // The 404 probe stays before the guard: a guard taken first would turn "unknown id AND busy" into a 409.
        self.plugin_row_or_404(id).await?;
        let guard = self.try_lock_lifecycle(id).map_err(spawn_error_to_calm)?;
        match self.stop_under(&guard).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
        }
        self.lifecycle_db.set_enabled(id, false).await?;
        self.plugin_row_or_404(id).await
    }

    /// Stop, then tear down every trace of the plugin except an operator-owned on-disk tree.
    /// The token / kv / overlay cascade deliberately swallows its errors; `plugin_delete` is the one write reported.
    pub async fn uninstall(self: &Arc<Self>, id: &str) -> Result<()> {
        // Probe before guard: taking the guard first would answer 409 for an unknown id that happens to be busy.
        self.plugin_row_or_404(id).await?;
        let guard = self.try_lock_lifecycle(id).map_err(spawn_error_to_calm)?;
        // Read inside the guard, never from the probe: a concurrent install re-materializes exactly this path.
        let row = self.repo.plugin_get_by_id(id).await?;
        // Stop first so the process can't write into state we're about to delete. NotFound is fine (already stopped).
        match self.stop_under(&guard).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
        }
        // Token + kv are FK-cascaded on sqlite but other backends won't have that; overlays have no FK at all.
        let _ = self.repo.plugin_token_delete(id).await;
        let _ = self.repo.plugin_kv_clear(id).await;
        let _ = self.repo.overlays_clear_by_plugin(id).await;
        self.repo.plugin_delete(id).await?;
        self.registry_remove(&guard);

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
    pub async fn reload(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        // Probe before guard: without it an unknown id falls through to the manifest read and returns a 400. Its value is deliberately dropped.
        if self.lifecycle_db.enabled_row(id).await?.is_none() {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        let guard = self.try_lock_lifecycle(id).map_err(spawn_error_to_calm)?;
        // The decision row. Read here, inside the guard, and NOT before it.
        let plug = self.plugin_row_or_404(id).await?;
        // Stop first (NotFound is fine — could have crashed).
        match self.stop_under(&guard).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
        }
        let install_dir = PathBuf::from(&plug.install_path);
        let manifest_path = install_dir.join("manifest.json");
        let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
            CalmError::PluginInstall(format!("reading {}: {e}", manifest_path.display()))
        })?;
        let manifest =
            Manifest::parse(&manifest_text).map_err(|e| CalmError::PluginInstall(e.to_string()))?;
        if manifest.id != id {
            return Err(CalmError::PluginInstall(format!(
                "manifest id changed during reload: was `{id}`, now `{}`",
                manifest.id
            )));
        }

        // Pre-check before mutating the registry or DB: a clean 422, not a half-applied reload.
        let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
            CalmError::PluginInstall(format!(
                "manifest min_kernel_version `{}` is not valid semver: {e}",
                manifest.min_kernel_version
            ))
        })?;
        if let Err(err) = check_min_kernel_version(&KERNEL_VERSION, &required) {
            return Err(CalmError::PluginKernelTooOld(format!(
                "plugin `{id}` requires kernel >= {}, this kernel is {}",
                err.required, err.actual,
            )));
        }

        // Persist the on-disk manifest to the row so `GET /api/plugins/:id` reflects it.
        let manifest_value = serde_json::to_value(&manifest)
            .map_err(|e| CalmError::Internal(format!("manifest re-serialize after reload: {e}")))?;
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
    async fn plugin_row_or_404(&self, id: &str) -> Result<Plugin> {
        self.repo
            .plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }
}

/// Translate a `PluginHost::spawn` failure into a route-shaped `CalmError`.
pub(crate) fn spawn_error_to_calm(e: HostError) -> CalmError {
    match e {
        HostError::KernelTooOld(k) => CalmError::PluginKernelTooOld(format!(
            "plugin requires kernel >= {}, this kernel is {}",
            k.required, k.actual,
        )),
        conflict @ HostError::TemplateConflict { .. } => {
            CalmError::PluginConflict(conflict.to_string())
        }
        // Not a kernel fault: 503 carries the reason and the row stays `enabled`, so a re-enable is the whole recovery.
        unavailable @ HostError::ConnectorUnavailable { .. } => {
            CalmError::ServiceUnavailable(unavailable.to_string())
        }
        // Same reasoning: the stored configuration is incomplete; 503, row stays `enabled`.
        missing @ HostError::MissingRequiredConfig { .. } => {
            CalmError::ServiceUnavailable(missing.to_string())
        }
        // Same reasoning, and consistent with the `unavailable` live entry the spawn path publishes.
        unreadable @ HostError::ConfigUnreadable { .. } => {
            CalmError::ServiceUnavailable(unreadable.to_string())
        }
        unsupported @ HostError::UnsupportedForKind { .. } => {
            CalmError::BadRequest(unsupported.to_string())
        }
        // 409 with its own code: `enable` writes `enabled = true` before spawning, so a 500 would claim a permanent fault for a request that did nothing and can be repeated.
        busy @ HostError::LifecycleBusy(_) => CalmError::PluginBusy(busy.to_string()),
        // 409 `plugin_conflict`: the state it conflicts with is the operator's own and `enable` is the remedy.
        // Not `plugin_busy` (retrying the identical request will not work) and not 503 (it is not trying on purpose).
        disabled @ HostError::OperatorDisabled(_) => {
            CalmError::PluginConflict(disabled.to_string())
        }
        other => CalmError::Internal(format!("spawn failed: {other}")),
    }
}

/// Materialize the install tree at `dst`. `src == dst` is the dev shortcut where `plugins_dir` holds the working copy.
fn materialize_install_tree(src: &StdPath, dst: &StdPath) -> Result<()> {
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
                    CalmError::PluginInstall(format!(
                        "removing stale install link {}: {e}",
                        dst.display()
                    ))
                })?;
            }
            Ok(m) if m.is_dir() => {
                std::fs::remove_dir_all(dst).map_err(|e| {
                    CalmError::PluginInstall(format!(
                        "removing stale install dir {}: {e}",
                        dst.display()
                    ))
                })?;
            }
            _ => {}
        }
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CalmError::PluginInstall(format!("creating plugins parent {}: {e}", parent.display()))
        })?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).map_err(|e| {
            CalmError::PluginInstall(format!(
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
            CalmError::PluginInstall(format!(
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

#[cfg(test)]
mod spawn_error_mapping_tests {
    use super::spawn_error_to_calm;
    use crate::error::CalmError;
    use crate::plugin_host::HostError;
    use axum::http::StatusCode;

    #[test]
    fn template_conflict_maps_to_structured_409() {
        let mapped = spawn_error_to_calm(HostError::TemplateConflict {
            plugin_id: "dev.second".into(),
            template_id: "issue-development".into(),
            held_by: "dev.first".into(),
        });
        assert!(
            matches!(&mapped, CalmError::PluginConflict(msg)
                if msg.contains("issue-development") && msg.contains("dev.first")),
            "expected PluginConflict naming the template and holder, got {mapped:?}"
        );
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_conflict");
    }

    #[test]
    fn kernel_too_old_still_maps_to_422() {
        let mapped =
            spawn_error_to_calm(HostError::KernelTooOld(crate::plugin_host::KernelTooOld {
                required: semver::Version::new(9, 9, 9),
                actual: semver::Version::new(0, 1, 0),
            }));
        assert_eq!(mapped.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(mapped.code(), "plugin_kernel_too_old");
    }

    #[test]
    fn operator_disabled_maps_to_structured_409() {
        let mapped = spawn_error_to_calm(HostError::OperatorDisabled("dev.app".into()));
        assert!(
            matches!(&mapped, CalmError::PluginConflict(msg)
                if msg.contains("dev.app") && msg.contains("enabled")),
            "expected PluginConflict naming the plugin and the bit, got {mapped:?}"
        );
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_conflict");
    }

    /// `plugins_disabled` is a config file the running kernel cannot change, so `enable` is not its remedy.
    #[test]
    fn config_disabled_is_not_the_same_cell_as_operator_disabled() {
        let mapped = spawn_error_to_calm(HostError::Disabled("dev.app".into()));
        assert_eq!(mapped.code(), "internal");
        assert_eq!(mapped.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
