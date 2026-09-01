//! #1196 S0b — the five composite plugin lifecycle operations.
//!
//! `install` / `enable` / `disable` / `uninstall` / `reload` used to be
//! orchestrated inside `routes/plugins.rs`. They are moved here **verbatim**
//! (design doc `docs/architecture/1196-plugin-lifecycle-lock.md` §2.3 / §7 S0)
//! so that S1 has a single place to take the per-id `LifecycleGuard`: every
//! step of a composite operation must live inside one guard lifetime, which is
//! impossible while the orchestration is split across an HTTP handler.
//!
//! **This slice adds no lock.** It is a pure relocation with zero behavior
//! change; the error-code table proving that is in the commit message.
//!
//! Three deliberate properties of the moved code, spelled out because the
//! "natural" rewrite silently breaks each of them (§7 nails 2 / 5 / 6 / 7):
//!
//! * The methods return [`CalmError`], not [`HostError`]. `HostError` has no
//!   variant that can carry `PluginInstall` / `PluginConflict` /
//!   `PluginKernelTooOld`, so flattening them into `HostError::BadState` would
//!   turn install's documented 409/400/422 into 500s.
//! * Every method that has a runtime step re-reads the plugin row **after**
//!   that step and returns the fresh row; the caller renders it. That read
//!   point is load-bearing (it is what makes `enable` report `running`), so it
//!   is preserved exactly.
//! * The leading `plugin_get_by_id` 404 probes are preserved. Without them
//!   `uninstall` of an unknown id would return 204 (`plugin_delete` does not
//!   report `NotFound`) and `reload` would return a manifest-read 400/500.
//!   `enable`/`disable` happen to be covered by `plugin_update_enabled`'s
//!   `rows_affected() == 0 → NotFound`, but that is a coincidence, not a rule.

use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

use super::{HostError, KERNEL_VERSION, Manifest, PluginHost, check_min_kernel_version};
use crate::error::CalmError;
use crate::model::{NewPlugin, Plugin};

type Result<T> = std::result::Result<T, CalmError>;

impl PluginHost {
    // -----------------------------------------------------------------------
    // install
    // -----------------------------------------------------------------------

    /// Install a plugin whose manifest has already been read and parsed.
    ///
    /// `Manifest::parse` (and the source-path resolution that feeds it) stay in
    /// the route on purpose: the plugin id comes from the manifest, not from
    /// the request URL, so S1's guard cannot be taken any earlier than here
    /// (design §2.3).
    ///
    /// Ordering is the route's, unchanged: min-kernel check → duplicate-id
    /// check → materialize the install tree → `plugin_install` → registry
    /// insert. Nothing before `materialize_install_tree` writes anything, so a
    /// refusal is fully inert.
    pub async fn install(&self, manifest: Manifest, src_path: &StdPath) -> Result<Plugin> {
        // Issue #45: refuse to install a plugin we can never spawn. Doing this
        // at install time (vs only at spawn time) avoids littering the DB and
        // the filesystem with a row + symlink that's permanently inert.
        // Manifest validation already confirmed the field parses; we just
        // compare here.
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

        // Reject reinstall while the previous row is still around. The
        // uninstall path is the only way to clear it; idempotent-by-conflict
        // matches the §7 table.
        if let Some(prev) = self.repo.plugin_get_by_id(&manifest.id).await? {
            return Err(CalmError::PluginConflict(format!(
                "plugin `{}` already installed at version `{}`",
                prev.id, prev.version
            )));
        }

        // Place the plugin tree under plugins_dir. If the target equals the
        // source we just record the path; otherwise we materialize a symlink
        // (Unix) or copy the tree (Windows fallback). Either way the install
        // path the registry remembers is the in-plugins-dir target, not the
        // user-supplied source — supervision must point at a path under our
        // control.
        let install_dir = self.plugins_dir.join(&manifest.id);
        materialize_install_tree(src_path, &install_dir)?;

        // Slice H replaces the install-time placeholder: the token row is now
        // created lazily by `PluginHost::ensure_plugin_token` on the first
        // `spawn`. Until then, `plugin_token_get` returns None — but that's
        // fine because the install flow doesn't read the token; it just needs
        // the row to eventually exist before the plugin is enabled.

        let new_plugin = NewPlugin {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            install_path: install_dir.to_string_lossy().into_owned(),
            manifest: manifest.to_json(),
            enabled: false,
            user_config: serde_json::json!({}),
        };
        let plug = self.repo.plugin_install(new_plugin).await?;

        // Keep the in-memory registry in sync. Permissions auto-grant happens
        // implicitly: the manifest carries the perms, and the
        // registry/permission checker reads them directly on every callback —
        // no separate "granted" table to update in M3.
        self.registry_insert(manifest, Some(install_dir));

        // The row returned here is `plugin_install`'s own return value, NOT a
        // re-read: install has no runtime step, and the route never re-read
        // either.
        Ok(plug)
    }

    // -----------------------------------------------------------------------
    // enable
    // -----------------------------------------------------------------------

    /// Flip `enabled = true` and spawn. Returns the row **re-read after** the
    /// spawn so the caller renders the post-spawn runtime state.
    ///
    /// Spawn errors leave `enabled = true` so the supervisor
    /// (`autospawn_enabled` on next boot) will keep trying. We do surface the
    /// error to the caller so the UI can show it immediately rather than
    /// waiting for a state event.
    pub async fn enable(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        self.plugin_row_or_404(id).await?;
        self.repo.plugin_update_enabled(id, true).await?;
        if let Err(e) = self.spawn(id).await {
            return Err(spawn_error_to_calm(e));
        }
        self.plugin_row_or_404(id).await
    }

    // -----------------------------------------------------------------------
    // disable
    // -----------------------------------------------------------------------

    /// Flip `enabled = false` and stop.
    ///
    /// **The DB write comes first, before the stop.** That is today's order and
    /// S0 keeps it: flipping it is a behavior change that belongs to S1
    /// (design §2.6), where it is witnessed by acceptance 16.
    ///
    /// The stop is best-effort: `NotFound` means the host wasn't running this
    /// plugin (already exited / never spawned); benign for the flip-to-disabled
    /// outcome we're trying to achieve.
    pub async fn disable(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        self.plugin_row_or_404(id).await?;
        self.repo.plugin_update_enabled(id, false).await?;
        match self.stop(id).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
        }
        self.plugin_row_or_404(id).await
    }

    // -----------------------------------------------------------------------
    // uninstall
    // -----------------------------------------------------------------------

    /// Stop, then tear down every trace of the plugin except its on-disk tree.
    ///
    /// **Contract: the token / kv / overlay cascade swallows its errors.** The
    /// three `let _ =` below are deliberate and must stay that way. Writing
    /// them as `?` is the natural shape once the code lives in the host, and it
    /// would turn "overlay cleanup failed" from a silently-tolerated hiccup
    /// into a 500 — with the plugin row already stopped but not deleted.
    /// `plugin_delete` is the one write whose failure is reported.
    pub async fn uninstall(self: &Arc<Self>, id: &str) -> Result<()> {
        self.plugin_row_or_404(id).await?;
        // Stop first so the process can't write into the state we're about to
        // delete out from under it. NotFound is fine (already stopped).
        match self.stop(id).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
        }
        // Token / kv / overlay cascade. Token + kv are also FK-cascaded on
        // sqlite (via `plugin_delete`) but the mock repo and the future
        // memory-only backends won't have that, so we call explicitly. Overlays
        // do NOT have an FK to plugins, so this is the only way to drop them.
        let _ = self.repo.plugin_token_delete(id).await;
        let _ = self.repo.plugin_kv_clear(id).await;
        let _ = self.repo.overlays_clear_by_plugin(id).await;
        self.repo.plugin_delete(id).await?;
        self.registry_remove(id);

        // The on-disk tree is left in place: removing it would race with any
        // observers (the user pointing the install at a checked-out repo loses
        // their work). Operators can rm -rf manually; the registry no longer
        // references it.
        Ok(())
    }

    // -----------------------------------------------------------------------
    // reload
    // -----------------------------------------------------------------------

    /// Dev hot-reload: stop, re-read the manifest from disk, re-validate,
    /// republish it to registry + DB, and **respawn only if the row said
    /// `enabled`**.
    ///
    /// The `enabled` bit and the install path both come from the row read
    /// **before** the stop — that pre-read row is the one that decides whether
    /// to respawn. An unconditional respawn would resurrect a disabled plugin.
    pub async fn reload(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        let plug = self.plugin_row_or_404(id).await?;
        // Stop first (NotFound is fine — could have crashed).
        match self.stop(id).await {
            Ok(()) => {}
            Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
        }
        // Re-read manifest from the recorded install path.
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

        // Issue #45: pre-check `min_kernel_version` *before* we mutate the
        // registry or DB. If the reloaded manifest now demands a newer kernel
        // than we are, we want a clean 422 — not a half-applied reload where
        // the DB shows a manifest the host can never spawn. The spawn path runs
        // the same check, but that's downstream of the registry/DB writes.
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

        // Persist the on-disk manifest back to the DB row so
        // `GET /api/plugins/:id` (which serializes from `Plugin::manifest`)
        // reflects current reality. The live `PluginRegistry` and
        // `views_catalog` were already consistent before this — this just keeps
        // the detail endpoint from lying.
        let manifest_value = serde_json::to_value(&manifest)
            .map_err(|e| CalmError::Internal(format!("manifest re-serialize after reload: {e}")))?;
        self.registry_insert(manifest, Some(install_dir));
        self.repo.plugin_update_manifest(id, manifest_value).await?;
        if plug.enabled
            && let Err(e) = self.spawn(id).await
        {
            return Err(spawn_error_to_calm(e));
        }
        self.plugin_row_or_404(id).await
    }

    // -----------------------------------------------------------------------
    // shared
    // -----------------------------------------------------------------------

    /// Read the plugin row or produce the route's exact 404.
    ///
    /// Used both for the leading existence probe and for the trailing re-read;
    /// both spots produced this same `CalmError::NotFound(format!("plugin
    /// {id}"))` before the move.
    async fn plugin_row_or_404(&self, id: &str) -> Result<Plugin> {
        self.repo
            .plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }
}

// ---------------------------------------------------------------------------
// Helpers moved from `routes/plugins.rs`
// ---------------------------------------------------------------------------

/// Translate a `PluginHost::spawn` failure into a route-shaped `CalmError`.
///
/// Most variants flatten to a 500 with the underlying string — the caller
/// (operator / UI) only needs to know "spawn failed, here's why". Two
/// exceptions get typed statuses:
///
/// * `KernelTooOld` (issue #45): the manifest demands a kernel we don't
///   ship, so we surface a 422 `PluginKernelTooOld` carrying both versions
///   in the body. That lets the UI render a "upgrade required" hint instead
///   of a generic internal-error toast.
/// * `WorkflowConflict` (#891 slice ④ review fix): the manifest declares a
///   workflow id another running trusted plugin already registers. That's a
///   409 `PluginConflict` — the request was well-formed and the kernel is
///   fine; the refusal is a state conflict the operator resolves by stopping
///   the holder — mirroring the install route's duplicate-id 409.
pub(crate) fn spawn_error_to_calm(e: HostError) -> CalmError {
    match e {
        HostError::KernelTooOld(k) => CalmError::PluginKernelTooOld(format!(
            "plugin requires kernel >= {}, this kernel is {}",
            k.required, k.actual,
        )),
        conflict @ HostError::WorkflowConflict { .. } => {
            CalmError::PluginConflict(conflict.to_string())
        }
        // #1164 §2.2 — an unreachable/misconfigured connector is not a kernel
        // fault: the request was well-formed, the upstream (or the operator's
        // `secrets.json`) is the problem. 503 carries the reason verbatim, and
        // the row stays `enabled` so a re-enable is the whole recovery.
        unavailable @ HostError::ConnectorUnavailable { .. } => {
            CalmError::ServiceUnavailable(unavailable.to_string())
        }
        unsupported @ HostError::UnsupportedForKind { .. } => {
            CalmError::BadRequest(unsupported.to_string())
        }
        other => CalmError::Internal(format!("spawn failed: {other}")),
    }
}

/// Materialize the install tree at `dst`. If `src == dst` we skip — the
/// plugin's source dir is already inside our plugins root, which is the dev
/// shortcut where `plugins_dir` itself contains the working copy.
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

    // Windows / other: deep-copy the tree. Symlinks need admin on Windows so
    // the symlink branch above is unix-only; this fallback path is M4 fodder
    // (M3 only targets unix per the design doc), kept here so the cfg cascade
    // doesn't accidentally bit-rot.
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

    /// #891 slice ④ review fix — a workflow-id refusal is an operator-visible
    /// state conflict (409 `plugin_conflict`), not a generic 500.
    #[test]
    fn workflow_conflict_maps_to_structured_409() {
        let mapped = spawn_error_to_calm(HostError::WorkflowConflict {
            plugin_id: "dev.second".into(),
            workflow_id: "issue-development".into(),
            held_by: "dev.first".into(),
        });
        assert!(
            matches!(&mapped, CalmError::PluginConflict(msg)
                if msg.contains("issue-development") && msg.contains("dev.first")),
            "expected PluginConflict naming the workflow and holder, got {mapped:?}"
        );
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_conflict");
    }

    /// Regression pin for the pre-existing KernelTooOld → 422 precedent this
    /// mapping follows.
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
}
