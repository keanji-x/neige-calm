//! `/api/plugins/*` — plugin install, list, configure, lifecycle (M3 Slice D).
//!
//! All endpoints wire `Repo` (the installed-plugins table + tokens + kv) and
//! `PluginHost` (the runtime supervisor) together so a single REST surface
//! drives the whole install → enable → disable → uninstall lifecycle. The
//! route contract is spec'd in `docs/m3-design.md` §7.
//!
//! Notable decisions made here that the design doc leaves to the
//! implementation:
//!
//!   * **Permissions auto-granted** on install. Design §10 row 8 resolved
//!     this: M3 has no pending-permission state, so the manifest's declared
//!     `permissions` block is what the runtime enforces from the first
//!     `/enable` onwards. No UI consent flow.
//!   * **Plugin disabled by default**. `/install` does not implicitly call
//!     `/enable`. Callers (UI, e2e scripts) explicitly toggle to spawn.
//!   * **Install is symlink-by-default on Unix, copy on Windows.** Symlinks
//!     let plugin authors `cargo build` then `curl install` without copying
//!     a fresh tree; Windows requires admin privileges for symlinks so we
//!     fall back. (M3 only ships unix, but the branch is here for M4.)
//!   * **Uninstall drops overlays** owned by the plugin. Design §2.7 leaves
//!     this open ("keep for forensics" vs "drop"); we drop. Tokens, KV, and
//!     plugin-rendered overlays all go.
//!   * **M5 (m3-mcp-apps): iframe cookies are scrapped.** AppBridge owns the
//!     iframe ↔ host channel via a postMessage `MessageChannel` the host
//!     minted; the user's session reaches `POST /api/plugins/:id/tool-call`
//!     gated by the kernel's `neige.*` prefix check (§7.6 row 5). The
//!     `iframe-write` REST route and `IframeCookieCache` are gone in this
//!     slice; the new iframe-HTML route lives at
//!     `GET /api/plugins/:id/resources/:view_id` and resolves through the
//!     kernel-internal `read_ui_resource()`.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{NewPlugin, Plugin};
use crate::plugin_host::{
    Manifest, PluginRegistry, PluginRuntimeStatus, ResourceError, RpcError, read_ui_resource,
};
use crate::state::{AppState, CodexShellState, RouteState};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path as StdPath, PathBuf};
use utoipa::{IntoParams, ToSchema};

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        // /views must be registered before `/:id` paths so it doesn't match
        // the `:id` extractor — axum's router is order-sensitive only for
        // overlapping shapes, but explicit ordering avoids surprises.
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/views", get(list_plugin_views))
        .route("/api/plugins/install", post(install_plugin))
        .route(
            "/api/plugins/{id}",
            get(get_plugin_detail).delete(uninstall_plugin),
        )
        .route("/api/plugins/{id}/enable", post(enable_plugin))
        .route("/api/plugins/{id}/disable", post(disable_plugin))
        .route("/api/plugins/{id}/config", patch(patch_plugin_config))
        .route("/api/plugins/{id}/log", get(tail_plugin_log))
        .route("/api/plugins/{id}/reload", post(reload_plugin))
        .route("/api/plugins/{id}/rotate-token", post(rotate_plugin_token))
        // M5: iframe HTML lives at `GET /api/plugins/:id/resources/:view_id`.
        // The handler resolves the URL into `ui://<id>/<view_id>` and calls
        // `plugin_host::read_ui_resource`. Browsers can't speak postMessage
        // to load an iframe src — they must do a real HTTP GET — so the
        // kernel re-exposes the MCP `resources/read` payload over HTTP for
        // exactly this URL pattern. No cookies; the desktop-local CORS gate
        // and the `neige.*` prefix check on `tool-call` provide the trust
        // boundary (see migration doc §3.3).
        .route(
            "/api/plugins/{id}/resources/{view_id}",
            get(get_plugin_view_html),
        )
        // M5: AppBridge `tools/call` fan-out. The iframe never reaches the
        // plugin process — `name` MUST start with `neige.` (§7.6 row 5),
        // and the call is dispatched through the in-kernel callback router.
        .route("/api/plugins/{id}/tool-call", post(plugin_tool_call))
}

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// Compact row used by `GET /api/plugins`. Pairs the persisted `Plugin` row
/// with the runtime status the supervisor knows about. The full manifest is
/// excluded here to keep the list payload cheap; callers needing the manifest
/// hit `GET /api/plugins/:id`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginListItem {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    /// Wire-name string per design §7.1: `running | spawning | crashed |
    /// disabled | installing | installed`.
    pub state: String,
    pub manifest_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Single-plugin detail returned by GET-by-id, install, enable, disable,
/// config-patch, and reload. The full manifest blob rides along so the UI can
/// render version/author/views without a separate fetch.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginDetail {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[schema(value_type = Object)]
    pub manifest: Value,
    #[schema(value_type = Object)]
    pub user_config: Value,
    pub installed_at: i64,
    pub updated_at: i64,
}

/// One entry in the `/api/plugins/views` catalog. Used by Slice G's AddPanel.
///
/// The canonical identifier is the MCP Apps `ui://<plugin>/<view>` URI in
/// `resource_uri`. The frontend parses the `(plugin_id, view_id)` pair off
/// it lazily via `parsePluginCardKind`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ViewCatalogEntry {
    /// Canonical MCP Apps URI: `ui://<plugin_id>/<view_id>`. Always present —
    /// computed kernel-side so the frontend doesn't have to redo the join.
    pub resource_uri: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_size: Option<ViewSizeWire>,
    /// `"card"` for M3 — wave/cove are banned per design §10.
    pub scope: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ViewSizeWire {
    pub w: u32,
    pub h: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_w: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_h: Option<u32>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct InstallBody {
    pub source: InstallSource,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallSource {
    LocalPath {
        path: String,
    },
    /// Catch-all so we can return a friendly 400 for tarball/url/etc. instead
    /// of a serde deserialize error. Slice D scope is `local_path` only.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct LogQuery {
    pub n: Option<usize>,
}

/// M5: AppBridge → kernel tool-call wire body. Mirrors the JSON-RPC
/// `tools/call` params shape so the web-calm helper can hand it through
/// verbatim from the iframe-side `app.callServerTool({ name, arguments })`.
///
/// Scope β: an optional `call_id` is threaded through to every event the
/// kernel writes while servicing this call. Each downstream `events.row`
/// records `correlation = "user_tool_call:<call_id>"` so multi-step
/// dispatches (e.g. a plugin tool that issues several overlay writes) can
/// be grouped after the fact (design doc §9). The frontend mints the id;
/// the kernel never inspects its content beyond formatting it into the
/// correlation string.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ToolCallBody {
    pub name: String,
    #[serde(default = "default_arguments")]
    #[schema(value_type = Object)]
    pub arguments: Value,
    /// Optional caller-supplied tracing id. Omitted on legacy callers; the
    /// resulting events still write but with `correlation = NULL`.
    #[serde(default)]
    pub call_id: Option<String>,
}

fn default_arguments() -> Value {
    Value::Object(Default::default())
}

// ---------------------------------------------------------------------------
// Handlers — GET list / GET detail
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/plugins",
    tag = "plugins",
    responses(
        (status = 200, description = "Installed plugins with their runtime state", body = Vec<PluginListItem>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_plugins(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
) -> Result<Json<Vec<PluginListItem>>> {
    let rows = s.repo.plugins_list_all().await?;
    let mut out = Vec::with_capacity(rows.len());
    for plug in rows {
        let runtime = cs.plugin.status(&plug.id).await;
        let (state, last_error) = match runtime {
            Some(snap) => (
                snap.status.wire_name().to_string(),
                snap.status.last_error().map(String::from),
            ),
            // Not running and no record in the host table — match on enabled
            // to differentiate "never started" from "explicitly disabled".
            None => {
                let wire = if plug.enabled {
                    "installed"
                } else {
                    "disabled"
                };
                (wire.to_string(), None)
            }
        };
        let manifest = &plug.manifest;
        out.push(PluginListItem {
            id: plug.id.clone(),
            version: plug.version.clone(),
            enabled: plug.enabled,
            state,
            manifest_name: manifest
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&plug.id)
                .to_string(),
            manifest_description: manifest
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            last_error,
        });
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/plugins/{id}",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Plugin detail (manifest + state)", body = PluginDetail),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_plugin_detail(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    Ok(Json(build_detail(&cs, plug).await))
}

// ---------------------------------------------------------------------------
// POST install — local_path only for M3
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/plugins/install",
    tag = "plugins",
    request_body = InstallBody,
    responses(
        (status = 201, description = "Plugin installed (disabled by default)", body = PluginDetail),
        (status = 400, description = "Manifest invalid / unsupported source", body = ErrorBody),
        (status = 409, description = "Plugin id already installed", body = ErrorBody),
        (status = 422, description = "Manifest min_kernel_version exceeds kernel version", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn install_plugin(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Json(body): Json<InstallBody>,
) -> Result<(StatusCode, Json<PluginDetail>)> {
    let raw_path = match body.source {
        InstallSource::LocalPath { path } => path,
        InstallSource::Other => {
            return Err(CalmError::PluginInstall(
                "unsupported source kind — M3 only accepts `local_path`".into(),
            ));
        }
    };

    // Resolve + validate the source path. Absolute paths are accepted; relative
    // paths resolve against CWD. We disallow `..` segments after canonicalize-
    // light (we don't actually canonicalize because the path doesn't have to
    // exist under our plugins root, only the source side) to avoid trivial
    // escape attempts from a malicious install body.
    let src_path = resolve_install_source(&raw_path)?;
    if !src_path.is_dir() {
        return Err(CalmError::PluginInstall(format!(
            "source path is not a directory: {}",
            src_path.display()
        )));
    }
    let manifest_path = src_path.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        CalmError::PluginInstall(format!("reading {}: {e}", manifest_path.display()))
    })?;
    let manifest =
        Manifest::parse(&manifest_text).map_err(|e| CalmError::PluginInstall(e.to_string()))?;

    // Issue #45: refuse to install a plugin we can never spawn. Doing this at
    // install time (vs only at spawn time) avoids littering the DB and the
    // filesystem with a row + symlink that's permanently inert. Manifest
    // validation already confirmed the field parses; we just compare here.
    let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
        CalmError::PluginInstall(format!(
            "manifest min_kernel_version `{}` is not valid semver: {e}",
            manifest.min_kernel_version
        ))
    })?;
    if let Err(err) =
        crate::plugin_host::check_min_kernel_version(&crate::plugin_host::KERNEL_VERSION, &required)
    {
        return Err(CalmError::PluginKernelTooOld(format!(
            "plugin `{}` requires kernel >= {}, this kernel is {}",
            manifest.id, err.required, err.actual,
        )));
    }

    // Reject reinstall while the previous row is still around. Slice D's
    // uninstall path is the only way to clear it; idempotent-by-conflict
    // matches the §7 table.
    if let Some(prev) = s.repo.plugin_get_by_id(&manifest.id).await? {
        return Err(CalmError::PluginConflict(format!(
            "plugin `{}` already installed at version `{}`",
            prev.id, prev.version
        )));
    }

    // Place the plugin tree under plugins_dir. If the target equals the source
    // we just record the path; otherwise we materialize a symlink (Unix) or
    // copy the tree (Windows fallback). Either way the install path the
    // registry remembers is the in-plugins-dir target, not the user-supplied
    // source — supervision must point at a path under our control.
    let install_dir = cs.plugin.plugins_dir.join(&manifest.id);
    materialize_install_tree(&src_path, &install_dir)?;

    // Slice H replaces the install-time placeholder: the token row is now
    // created lazily by `PluginHost::ensure_plugin_token` on the first
    // `spawn`. Until then, `plugin_token_get` returns None — but that's fine
    // because the install flow doesn't read the token; it just needs the row
    // to eventually exist before the plugin is enabled.

    let new_plugin = NewPlugin {
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        install_path: install_dir.to_string_lossy().into_owned(),
        manifest: manifest.to_json(),
        enabled: false,
        user_config: serde_json::json!({}),
    };
    let plug = s.repo.plugin_install(new_plugin).await?;

    // Keep the in-memory registry in sync. Permissions auto-grant happens
    // implicitly: the manifest carries the perms, and the registry/permission
    // checker reads them directly on every callback — no separate "granted"
    // table to update in M3.
    cs.plugin.registry().insert(manifest, Some(install_dir));

    let detail = build_detail(&cs, plug).await;
    Ok((StatusCode::CREATED, Json(detail)))
}

// ---------------------------------------------------------------------------
// POST enable / disable
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/enable",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Plugin enabled and spawned", body = PluginDetail),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 422, description = "Manifest min_kernel_version exceeds kernel version", body = ErrorBody),
        (status = 500, description = "Spawn failed / internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn enable_plugin(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    s.repo.plugin_update_enabled(&id, true).await?;
    // Spawn errors leave enabled=true so the supervisor (autospawn_enabled on
    // next boot) will keep trying. We do surface the error to the caller so
    // the UI can show it immediately rather than waiting for a state event.
    if let Err(e) = cs.plugin.spawn(&id).await {
        return Err(spawn_error_to_calm(e));
    }
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    Ok(Json(build_detail(&cs, plug).await))
}

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/disable",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Plugin disabled and stopped", body = PluginDetail),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 500, description = "Stop failed / internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn disable_plugin(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    s.repo.plugin_update_enabled(&id, false).await?;
    // Best-effort stop. NotFound here means the host wasn't running this
    // plugin (already exited / never spawned); benign for the flip-to-disabled
    // outcome we're trying to achieve.
    match cs.plugin.stop(&id).await {
        Ok(()) => {}
        Err(crate::plugin_host::HostError::NotFound(_)) => {}
        Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
    }
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    Ok(Json(build_detail(&cs, plug).await))
}

// ---------------------------------------------------------------------------
// PATCH config
// ---------------------------------------------------------------------------

#[utoipa::path(
    patch,
    path = "/api/plugins/{id}/config",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    request_body(content = Object, description = "Free-form user-config JSON object"),
    responses(
        (status = 200, description = "Config updated", body = PluginDetail),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn patch_plugin_config(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<PluginDetail>> {
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    let plug = s.repo.plugin_update_user_config(&id, body.clone()).await?;
    // Choice: the running plugin keeps running with the *old* config. Reading
    // the new config happens on next spawn. The alternative — fire a
    // `neige.config.changed` notification — is reasonable but the design doc
    // doesn't pin a method name, plugin authors haven't seen this hook yet,
    // and most M3 plugins won't read user_config dynamically. We can wire the
    // notification later without an API break.
    Ok(Json(build_detail(&cs, plug).await))
}

// ---------------------------------------------------------------------------
// DELETE — full uninstall
// ---------------------------------------------------------------------------

#[utoipa::path(
    delete,
    path = "/api/plugins/{id}",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 204, description = "Plugin uninstalled"),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn uninstall_plugin(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    // Stop first so the process can't write into the state we're about to
    // delete out from under it. NotFound is fine (already stopped).
    match cs.plugin.stop(&id).await {
        Ok(()) => {}
        Err(crate::plugin_host::HostError::NotFound(_)) => {}
        Err(e) => return Err(CalmError::Internal(format!("stop failed: {e}"))),
    }
    // Token / kv / overlay cascade. Token + kv are also FK-cascaded on sqlite
    // (via `plugin_delete`) but the mock repo and the future memory-only
    // backends won't have that, so we call explicitly. Overlays do NOT have
    // an FK to plugins, so this is the only way to drop them.
    let _ = s.repo.plugin_token_delete(&id).await;
    let _ = s.repo.plugin_kv_clear(&id).await;
    let _ = s.repo.overlays_clear_by_plugin(&id).await;
    s.repo.plugin_delete(&id).await?;
    cs.plugin.registry().remove(&id);

    // The on-disk tree is left in place: removing it would race with any
    // observers (the user pointing the install at a checked-out repo loses
    // their work). Operators can rm -rf manually; the registry no longer
    // references it.
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// GET log
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/plugins/{id}/log",
    tag = "plugins",
    params(
        ("id" = String, Path, description = "Plugin id"),
        LogQuery,
    ),
    responses(
        (status = 200, description = "Recent stderr lines (newest last)", body = Vec<String>),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn tail_plugin_log(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
) -> Result<Json<Vec<String>>> {
    // Verify the plugin exists at the persistence layer first — 404 here
    // means the plugin was never installed, distinct from "installed but
    // never ran" (which returns []).
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    let n = q.n.unwrap_or(200).min(1024);
    let lines = cs.plugin.stderr_tail(&id, n).await.unwrap_or_default();
    Ok(Json(lines))
}

// ---------------------------------------------------------------------------
// POST reload — dev hot-reload of manifest + restart if enabled
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/reload",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Manifest reloaded + plugin restarted if enabled", body = PluginDetail),
        (status = 400, description = "Manifest invalid / id mismatch after reload", body = ErrorBody),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 422, description = "Manifest min_kernel_version exceeds kernel version", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reload_plugin(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    // Stop first (NotFound is fine — could have crashed).
    match cs.plugin.stop(&id).await {
        Ok(()) => {}
        Err(crate::plugin_host::HostError::NotFound(_)) => {}
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
    // than we are, we want a clean 422 — not a half-applied reload where the
    // DB shows a manifest the host can never spawn. The spawn path runs the
    // same check, but that's downstream of the registry/DB writes.
    let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
        CalmError::PluginInstall(format!(
            "manifest min_kernel_version `{}` is not valid semver: {e}",
            manifest.min_kernel_version
        ))
    })?;
    if let Err(err) =
        crate::plugin_host::check_min_kernel_version(&crate::plugin_host::KERNEL_VERSION, &required)
    {
        return Err(CalmError::PluginKernelTooOld(format!(
            "plugin `{id}` requires kernel >= {}, this kernel is {}",
            err.required, err.actual,
        )));
    }

    // Persist the on-disk manifest back to the DB row so `GET /api/plugins/:id`
    // (which serializes from `Plugin::manifest`) reflects current reality. The
    // live `PluginRegistry` and `views_catalog` were already consistent before
    // this — this just keeps the detail endpoint from lying.
    let manifest_value = serde_json::to_value(&manifest)
        .map_err(|e| CalmError::Internal(format!("manifest re-serialize after reload: {e}")))?;
    cs.plugin.registry().insert(manifest, Some(install_dir));
    s.repo.plugin_update_manifest(&id, manifest_value).await?;
    if plug.enabled
        && let Err(e) = cs.plugin.spawn(&id).await
    {
        return Err(spawn_error_to_calm(e));
    }
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    Ok(Json(build_detail(&cs, plug).await))
}

// ---------------------------------------------------------------------------
// GET views catalog
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/plugins/views",
    tag = "plugins",
    responses(
        (status = 200, description = "Catalog of views from currently enabled plugins", body = Vec<ViewCatalogEntry>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_plugin_views(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
) -> Result<Json<Vec<ViewCatalogEntry>>> {
    // Only emit entries for plugins that are currently enabled — disabled
    // plugins can't actually render. Take a snapshot of the installed table
    // and join against the registry's manifest cache.
    let installed = s.repo.plugins_list_all().await?;
    let registry: &PluginRegistry = cs.plugin.registry();
    let mut out = Vec::new();
    for plug in installed {
        if !plug.enabled {
            continue;
        }
        let Some(manifest) = registry.get(&plug.id) else {
            // Installed but the manifest didn't load (corrupt, missing); skip.
            continue;
        };
        for view in &manifest.views {
            let resource_uri = format!("ui://{}/{}", manifest.id, view.view_id);
            out.push(ViewCatalogEntry {
                resource_uri,
                title: view.title.clone(),
                icon: view.icon.clone(),
                default_size: view.default_size.as_ref().map(|sz| ViewSizeWire {
                    w: sz.w,
                    h: sz.h,
                    min_w: sz.min_w,
                    min_h: sz.min_h,
                }),
                scope: view.scope.clone(),
            });
        }
    }
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// GET /api/plugins/:id/resources/:view_id — M5 iframe HTML
// ---------------------------------------------------------------------------
//
// Browsers load iframes by URL, so we need an HTTP entry to the same data
// MCP's `resources/read` returns. The handler reconstructs the canonical
// `ui://<id>/<view_id>` URI and calls the kernel-internal
// `read_ui_resource` pure function (no `tools/call`, no plugin process
// round-trip — the kernel knows the manifest and the on-disk path).
//
// Trust model: this is the desktop-local server; CORS is locked to the
// `web-calm` origin in `main.rs`, and the URL contains a stable
// `<plugin>/<view>` pair that's also what we'd render inline. No
// iframe-specific cookie — see migration doc §3.3 + module-level docs.
//
// Response: `Content-Type: text/html;profile=mcp-app` so AppBridge's
// sandbox proxy recognizes the body as MCP-App HTML, plus a derived
// `Content-Security-Policy` header from the view's manifest `csp` block
// (when set; absent → no header → AppBridge's default no-network sandbox
// kicks in).

#[utoipa::path(
    get,
    path = "/api/plugins/{id}/resources/{view_id}",
    tag = "plugins",
    params(
        ("id" = String, Path, description = "Plugin id"),
        ("view_id" = String, Path, description = "View id within the plugin manifest"),
    ),
    responses(
        (status = 200, description = "MCP-App HTML (Content-Type: text/html;profile=mcp-app)", body = String, content_type = "text/html;profile=mcp-app"),
        (status = 400, description = "Malformed ui:// URI", body = ErrorBody),
        (status = 404, description = "Plugin or view not found / asset missing", body = ErrorBody),
        (status = 500, description = "I/O error reading asset", body = ErrorBody),
    ),
)]
pub(crate) async fn get_plugin_view_html(
    State(cs): State<CodexShellState>,
    Path((id, view_id)): Path<(String, String)>,
) -> Response {
    let uri = format!("ui://{id}/{view_id}");
    match read_ui_resource(cs.plugin.registry(), &uri) {
        Ok(contents) => {
            let entry = match contents.contents.into_iter().next() {
                Some(e) => e,
                None => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": "resources/read returned empty contents",
                            "code": "internal",
                        })),
                    )
                        .into_response();
                }
            };
            let body = entry.text.unwrap_or_default();
            let mime = entry
                .mime_type
                .unwrap_or_else(|| "text/html;profile=mcp-app".to_string());
            let csp_header = csp_header_from_meta(entry.meta.as_ref());

            let mut resp = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime);
            if let Some(csp) = csp_header {
                resp = resp.header(header::CONTENT_SECURITY_POLICY, csp);
            }
            resp.body(Body::from(body)).unwrap_or_else(|e| {
                tracing::error!(error = %e, "failed to build view_html response");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("response build: {e}"),
                        "code": "internal",
                    })),
                )
                    .into_response()
            })
        }
        Err(ResourceError::MalformedUri(_)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("malformed ui:// uri derived from {id}/{view_id}"),
                "code": "bad_request",
            })),
        )
            .into_response(),
        Err(ResourceError::PluginNotFound(plugin)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("plugin `{plugin}` not installed"),
                "code": "not_found",
            })),
        )
            .into_response(),
        Err(ResourceError::ViewNotFound { plugin_id, view_id }) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("view `{view_id}` not found on plugin `{plugin_id}`"),
                "code": "not_found",
            })),
        )
            .into_response(),
        Err(ResourceError::Io { path, source }) => {
            // ENOENT on the HTML asset is a packaging mistake (4xx-style) —
            // surface as 404 with the path so operators can spot it; any
            // other I/O error is a 500.
            let status = if source.kind() == std::io::ErrorKind::NotFound {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({
                    "error": format!("reading view html {path}: {source}"),
                    "code": if status == StatusCode::NOT_FOUND { "not_found" } else { "internal" },
                })),
            )
                .into_response()
        }
    }
}

/// Compose a `Content-Security-Policy` header value from a view's
/// `_meta.ui.csp` block. Returns `None` if the meta is absent or empty —
/// callers should then omit the header entirely and let AppBridge's
/// default no-network sandbox enforce policy.
///
/// The mapping is deliberately conservative: `default_src`, `script_src`,
/// `style_src`, `connect_src`, `img_src` are emitted with their snake_case
/// names rewritten to spec form (`default-src`, etc.); the `extras`
/// flatten-bucket from `CspBlock` is forwarded under its raw key with a
/// best-effort `snake → kebab` rewrite for the same five-or-so canonical
/// directives the spec names. Unknown keys flow through verbatim.
fn csp_header_from_meta(meta: Option<&Value>) -> Option<String> {
    let csp = meta?.pointer("/ui/csp")?.as_object()?;
    let mut parts: Vec<String> = Vec::new();
    for (key, value) in csp.iter() {
        let directive = key.replace('_', "-");
        let sources: Vec<String> = match value {
            Value::Array(items) => items
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => continue,
        };
        if sources.is_empty() {
            continue;
        }
        parts.push(format!("{directive} {}", sources.join(" ")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

// ---------------------------------------------------------------------------
// POST /api/plugins/:id/tool-call — M5 AppBridge fan-out
// ---------------------------------------------------------------------------
//
// The host-side endpoint the web-calm AppBridge hits when a plugin iframe
// calls `app.callServerTool({ name, arguments })`. Per migration doc
// §7.6 row 5, **only `neige.*` kernel-namespace tools are callable from
// the iframe** — the plugin's own server tools are denied. The dispatch
// reuses the same `callbacks::dispatch` machinery the plugin's inbound MCP
// router uses (via `PluginHost::dispatch_neige_callback`), so permissions,
// quotas, and ownership rules all apply identically.

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/tool-call",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    request_body = ToolCallBody,
    responses(
        (status = 200, description = "Tool result JSON (shape depends on dispatched neige.* callback)", body = Object),
        (status = 403, description = "Tool outside iframe-allowed scope (non-neige.* namespace, or not in manifest's permissions.tools)", body = ErrorBody),
        (status = 404, description = "Plugin not running", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn plugin_tool_call(
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
    Json(body): Json<ToolCallBody>,
) -> Response {
    // §7.6 row 5: hard gate. The plugin's own tools are unreachable from
    // the iframe — we never forward this call to the plugin process.
    if !body.name.starts_with("neige.") {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "only neige.* tools are callable from iframes",
                "code": "forbidden_tool",
            })),
        )
            .into_response();
    }

    // Plugin must be running for any neige.* dispatch (permissions live on
    // the manifest, but the registry copy + subscription table sit on the
    // RunningPlugin record).
    if cs.plugin.status(&id).await.is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("plugin `{id}` is not running"),
                "code": "not_found",
            })),
        )
            .into_response();
    }

    // #198 (concern 5): enforce the manifest's per-view `permissions.tools`
    // allow-list. The struct was previously shipped to the iframe under
    // `_meta.ui.permissions.tools` but never consulted server-side, so a
    // compromised iframe could call any neige.* tool the plugin's running
    // state allowed. We now reject anything not in scope with 403.
    //
    // Lookup is best-effort: a running plugin without a registry entry is
    // an internal-state bug — treat it as denied rather than panic, and let
    // the operator notice via the response code. (In practice the registry
    // is populated at install + spawn time and dropped only on uninstall,
    // by which point `status()` above would already have returned None.)
    let manifest_allows = cs
        .plugin
        .registry()
        .get(&id)
        .map(|m| m.can_call_tool(&body.name))
        .unwrap_or(false);
    if !manifest_allows {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": format!(
                    "tool `{}` is not in plugin `{id}`'s declared permissions.tools",
                    body.name
                ),
                "code": "forbidden_tool",
            })),
        )
            .into_response();
    }

    // Empty-string call_id is normalized to absent so we never write the
    // useless `correlation = "user_tool_call:"` row. A legacy/buggy client
    // that sends `call_id: ""` behaves identically to one that omits the
    // field — see scope-β review feedback on PR #37.
    let call_id = body.call_id.as_deref().filter(|s| !s.is_empty());
    match cs
        .plugin
        .dispatch_neige_callback(&id, &body.name, body.arguments, call_id)
        .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => rpc_to_calm(e).into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /api/plugins/:id/rotate-token — admin endpoint per design §6.3
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/rotate-token",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Token rotated", body = PluginDetail),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 500, description = "Rotate failed", body = ErrorBody),
    ),
)]
pub(crate) async fn rotate_plugin_token(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    // 404 if unknown — gives the UI a clear "wrong id" signal rather than the
    // host's BadState wrapping.
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    cs.plugin
        .rotate_plugin_token(&id)
        .await
        .map_err(|e| CalmError::Internal(format!("rotate failed: {e}")))?;
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    Ok(Json(build_detail(&cs, plug).await))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rpc_to_calm(e: RpcError) -> CalmError {
    // Map kernel-extension codes to plugin-aware HTTP variants; bare
    // JSON-RPC codes (Invalid Params, Method Not Found) land as 400.
    match e.code {
        -32001 => CalmError::PluginPermission(e.message),
        -32002 => CalmError::PluginInstall(e.message),
        -32003 => CalmError::PluginPermission(e.message),
        -32004 => CalmError::NotFound(e.message),
        RpcError::INVALID_PARAMS => CalmError::BadRequest(e.message),
        RpcError::METHOD_NOT_FOUND => CalmError::BadRequest(e.message),
        _ => CalmError::Internal(e.message),
    }
}

/// Translate a `PluginHost::spawn` failure into a route-shaped `CalmError`.
///
/// Most variants flatten to a 500 with the underlying string — the caller
/// (operator / UI) only needs to know "spawn failed, here's why". The
/// exception is `KernelTooOld` (issue #45): the manifest demands a kernel we
/// don't ship, so we surface a 422 `PluginKernelTooOld` carrying both
/// versions in the body. That lets the UI render a "upgrade required" hint
/// instead of a generic internal-error toast.
fn spawn_error_to_calm(e: crate::plugin_host::HostError) -> CalmError {
    match e {
        crate::plugin_host::HostError::KernelTooOld(k) => CalmError::PluginKernelTooOld(format!(
            "plugin requires kernel >= {}, this kernel is {}",
            k.required, k.actual,
        )),
        other => CalmError::Internal(format!("spawn failed: {other}")),
    }
}

/// Resolve a user-supplied install source path. Absolute paths are accepted
/// as-is; relative paths resolve against CWD. We reject `..` components to
/// prevent a malicious caller from walking outside the obvious tree, but
/// we don't `canonicalize` because the source doesn't have to exist under
/// any specific root.
fn resolve_install_source(raw: &str) -> Result<PathBuf> {
    let path = StdPath::new(raw);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| CalmError::PluginInstall(format!("cwd: {e}")))?
            .join(path)
    };
    for comp in resolved.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(CalmError::PluginInstall(
                "install path may not contain `..` segments".into(),
            ));
        }
    }
    Ok(resolved)
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

/// Assemble the `PluginDetail` payload by joining the persisted row with the
/// current runtime status (if any).
async fn build_detail(cs: &CodexShellState, plug: Plugin) -> PluginDetail {
    let runtime = cs.plugin.status(&plug.id).await;
    let (state, last_error) = match runtime {
        Some(snap) => (
            snap.status.wire_name().to_string(),
            snap.status.last_error().map(String::from),
        ),
        None => {
            let wire = if plug.enabled {
                "installed"
            } else {
                "disabled"
            };
            (wire.to_string(), None)
        }
    };
    PluginDetail {
        id: plug.id,
        version: plug.version,
        enabled: plug.enabled,
        state,
        last_error,
        manifest: plug.manifest,
        user_config: plug.user_config,
        installed_at: plug.installed_at,
        updated_at: plug.updated_at,
    }
}

// Touch the PluginRuntimeStatus enum so its variants stay in the public
// surface — Slice D doesn't construct one directly but consumes
// `wire_name()` / `last_error()` everywhere.
#[allow(dead_code)]
const _RUNTIME_STATUS_LIVE: Option<PluginRuntimeStatus> = None;
