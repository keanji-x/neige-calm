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
//! * The leading 404 probes are preserved. `reload`'s is the
//!   only one that is load-bearing *on its own*: without it an unknown id
//!   reaches the manifest read and returns a 400. (#1196 S1 review P0-1 moved
//!   `reload`'s probe onto [`LifecycleDb::enabled_row`] so that it can only
//!   answer "does this row exist" and can no longer hand a stale `Plugin` to the
//!   decision below it; the 404 it produces is byte-identical. The other three
//!   still call `plugin_row_or_404` and discard its result.) The other three are today
//!   redundant with the repo layer — `plugin_update_enabled` and
//!   `plugin_delete` both raise `NotFound` on `rows_affected() == 0`
//!   (`calm-truth/src/db/sqlite/out_of_domain.rs:414` / `:470`), formatting the
//!   **byte-identical** `CalmError::NotFound(format!("plugin {id}"))`. So
//!   deleting one probe alone is unobservable; what must not be lost is the
//!   *endpoint contract*, and that is what the gate pins:
//!   `tests/cases/plugin_routes.rs::{enable,disable}_unknown_id_returns_404`,
//!   `uninstall_unknown_id_returns_404_not_204`,
//!   `reload_unknown_id_returns_404_not_manifest_read_error` and — for the
//!   `if plug.enabled` respawn guard — `reload_disabled_plugin_does_not_spawn`.
//!   (§7 nail 5's stated rationale, "`plugin_delete` does not report
//!   `NotFound`", is factually wrong; the contract it protects is not.)

use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use super::{HostError, KERNEL_VERSION, Manifest, PluginHost, check_min_kernel_version};
use crate::db::RouteRepo;
use crate::error::CalmError;
use crate::model::{NewPlugin, Plugin};

type Result<T> = std::result::Result<T, CalmError>;

// ---------------------------------------------------------------------------
// #1196 S1 — the narrow lifecycle DB port
// ---------------------------------------------------------------------------

/// The two plugin-row operations the lifecycle machinery performs, isolated
/// from the ~100-method `RouteRepo` behind a port narrow enough to fake.
///
/// **Why this exists** (design §4 acceptance 15/16). Two things are otherwise
/// untestable:
///
/// * the supervisor's third segment must **fail closed** on a plugin-row read
///   failure — keep `Crashed`, release the lock, retry — and prove that when
///   the read recovers it still honours `enabled`. `SqlxRepo` is the only
///   `RouteRepo` implementation in the repo (`MockRepo` was deliberately
///   deleted in #4) and `pool().close()` is a *permanent* failure, so it cannot
///   express "fails once, then works";
/// * `disable`'s internal order (stop first, DB write second) is invisible from
///   outside: both orders leave the same final state. A fake that observes
///   `PluginHost::status` at the instant `set_enabled` is called sees the
///   difference directly — `None` under the new order, `Some(Running)` under
///   the old one. A DB barrier cannot do this here: `tests/` run on
///   `sqlite::memory:`, where `journal_mode = WAL` is a no-op and readers get
///   no snapshot isolation, so a held write transaction merely parks the
///   `UPDATE` and both orders look identical.
///
/// `pub` and `#[async_trait]` are both load-bearing: the acceptance tests are
/// an external crate, and `dyn LifecycleDb` needs the trait object shape the
/// rest of the repo's async traits use.
#[async_trait]
pub trait LifecycleDb: Send + Sync {
    /// `Ok(None)` — no such plugin row. `Ok(Some(enabled))` — the row's
    /// `enabled` bit. `Err` — the read itself failed and the caller must not
    /// guess.
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>>;

    /// Set the row's `enabled` bit. Propagates the repo's own `NotFound` for a
    /// missing row, which is what keeps the enable/disable endpoints' 404
    /// contract byte-identical.
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
    ///
    /// #1196 §2.3 — **where the guard is taken matters.** It goes after the
    /// min-kernel check and before the duplicate-id probe. Taking it on the
    /// first line would mean "this id is busy AND the manifest needs a newer
    /// kernel" answers 409 instead of today's 422 — an error code silently
    /// changed by the lock. Nothing above the guard writes anything, so
    /// "`Busy` implies no side effects" still holds.
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

        // ---- THE guard. Everything below is one critical section. ---------
        // This is what closes design §1.2 race 2: the duplicate-id probe and
        // the insert used to be a TOCTOU pair over an `ON CONFLICT DO UPDATE`,
        // so two concurrent installs of one id could both pass the probe and
        // the loser would overwrite the winner's row.
        let guard = self
            .try_lock_lifecycle(&manifest.id)
            .map_err(spawn_error_to_calm)?;

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
        self.registry_insert(&guard, manifest, Some(install_dir));

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
        // #1196 §2.3 — the 404 probe stays **before** the guard, for the same
        // reason `install`'s min-kernel check does. Part of this endpoint's
        // unknown-id 404 is raised by the write below (`plugin_update_enabled`
        // reports `NotFound` on `rows_affected() == 0`), so a guard taken ahead
        // of the probe would turn "unknown id AND busy" into a 409 — an error
        // code silently changed by the lock, exactly the install 422→409
        // regression one endpoint over. The probe is a pure read, so keeping it
        // outside the guard does not weaken "Busy implies no side effects".
        self.plugin_row_or_404(id).await?;
        let guard = self.try_lock_lifecycle(id).map_err(spawn_error_to_calm)?;
        self.lifecycle_db.set_enabled(id, true).await?;
        if let Err(e) = self.spawn_under(&guard, None).await {
            return Err(spawn_error_to_calm(e));
        }
        self.plugin_row_or_404(id).await
    }

    // -----------------------------------------------------------------------
    // disable
    // -----------------------------------------------------------------------

    /// Stop, then flip `enabled = false`.
    ///
    /// **#1196 §2.6 — the order is reversed from S0's, and the order is the
    /// point.** Writing the DB first leaves, whenever the stop then fails,
    /// `enabled = false` beside a plugin that is still running — and the next
    /// boot's autospawn skips it *because* it is disabled, so nothing ever
    /// reconciles. Stopping first fails the other way: if the DB write fails we
    /// are left stopped but still `enabled = true`, and the next boot brings it
    /// back. That is the same philosophy `enable` already follows (a failed
    /// spawn leaves `enabled = true` on purpose).
    ///
    /// The stop is best-effort: `NotFound` means the host wasn't running this
    /// plugin (already exited / never spawned); benign for the flip-to-disabled
    /// outcome we're trying to achieve. **No other stop error may reach the DB
    /// write** — that is what makes the supervisor's epoch check authoritative
    /// over the `enabled` bit (design §2.6).
    pub async fn disable(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        // #1196 §2.3 — the 404 probe stays **before** the guard, for the same
        // reason `install`'s min-kernel check does. Part of this endpoint's
        // unknown-id 404 is raised by the write below (`plugin_update_enabled`
        // reports `NotFound` on `rows_affected() == 0`), so a guard taken ahead
        // of the probe would turn "unknown id AND busy" into a 409 — an error
        // code silently changed by the lock, exactly the install 422→409
        // regression one endpoint over. The probe is a pure read, so keeping it
        // outside the guard does not weaken "Busy implies no side effects".
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
        // #1196 §2.3 — probe before guard. `plugin_delete` raises the unknown-id
        // `NotFound` itself (`rows_affected() == 0`), so taking the guard first
        // would answer 409 for an id that does not exist and happens to be
        // busy, where today the endpoint answers 404. Pure read, no side
        // effects, so "Busy implies nothing happened" is untouched.
        self.plugin_row_or_404(id).await?;
        let guard = self.try_lock_lifecycle(id).map_err(spawn_error_to_calm)?;
        // Stop first so the process can't write into the state we're about to
        // delete out from under it. NotFound is fine (already stopped).
        //
        // #1196 §2.4 — `NotFound` no longer hides an in-flight spawn. Such a
        // spawn would hold this very guard, so by the time we are here it has
        // finished (and `stop_under` removes it) or unwound.
        match self.stop_under(&guard).await {
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
        self.registry_remove(&guard);

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
    /// The `enabled` bit and the install path both come from a row read
    /// **inside the guard** — that row is the one that decides whether to
    /// respawn. An unconditional respawn would resurrect a disabled plugin.
    ///
    /// **#1196 S1 review P0-1 — the probe may not supply decision values.** The
    /// pre-guard probe below is an *existence* probe and nothing else. The
    /// reachable interleaving it used to create: the probe reads `enabled =
    /// true`, a concurrent `disable` takes the guard, stops the plugin and
    /// commits `enabled = false`, releases; this reload then takes the guard and
    /// respawns on the **stale** bit — terminal state `DB disabled + runtime
    /// Running`, with nothing left to reconcile it. That is #1169 race 3 one
    /// endpoint over. It applies to `install_path` too: an `install` that
    /// re-materialized the tree in the same window would be read past.
    ///
    /// Why the probe goes through [`LifecycleDb::enabled_row`] rather than
    /// [`Self::plugin_row_or_404`]: the port hands back an `Option<bool>` we
    /// immediately discard, so there is no `Plugin` in scope before the guard
    /// and the defect is not *expressible* here any more — which is a stronger
    /// statement than "remember to re-read". It also gives the acceptance suite
    /// the seam it needs to open the window deterministically (`a17`).
    ///
    /// The 404 shape is byte-identical to the probe it replaces: `enabled_row`
    /// answers `Ok(None)` for a missing row, and the message is the same
    /// `plugin {id}` this module has always produced.
    pub async fn reload(self: &Arc<Self>, id: &str) -> Result<Plugin> {
        // #1196 §2.3 — probe before guard. `reload`'s 404 is the one that is
        // load-bearing on its own: without the probe an unknown id falls
        // through to the manifest read and returns a 400. Behind the guard it
        // would instead return 409 whenever the id were busy. Pure read, and
        // its VALUE is deliberately dropped on the floor.
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
        self.registry_insert(&guard, manifest, Some(install_dir));
        self.repo.plugin_update_manifest(id, manifest_value).await?;
        if plug.enabled
            && let Err(e) = self.spawn_under(&guard, None).await
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
        // #1196 §2.5 / §5 R8 — 409 with its OWN code. Mapping this to the
        // catch-all 500 below would be actively harmful, not merely imprecise:
        // `enable` writes `enabled = true` before spawning, so a 500 here
        // states a permanent kernel fault for a request that in fact did
        // nothing and can simply be repeated.
        busy @ HostError::LifecycleBusy(_) => CalmError::PluginBusy(busy.to_string()),
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
