//! `/api/plugins/*` — plugin install, configuration, and lifecycle.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::Plugin;
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
    /// Wire-name string per design §7.1, plus `unavailable` from #1164 §2.2:
    /// `running | spawning | crashed | unavailable | disabled | installing |
    /// installed`.
    ///
    /// `unavailable` is the NORMAL terminal state of a connector
    /// (`kind: mcp-http` / `cli-query`) whose bring-up failed — unreachable
    /// upstream, rejected `secrets.json`, boot budget exhausted. It is not an
    /// error state of the kernel, and unlike `crashed` there is no supervisor
    /// that will retry it: it stands until an operator re-enables. `last_error`
    /// carries the reason.
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
    /// Same wire-name set as [`PluginListItem::state`], including the
    /// connector-only `unavailable` — see that field's doc.
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

    // Everything from here on (min-kernel check, duplicate-id refusal, tree
    // materialization, DB insert, registry insert) is one composite operation
    // owned by the host — #1196 S0b. `Manifest::parse` stays here because the
    // plugin id it yields is what S1's per-id guard will be taken on.
    let plug = cs.plugin.install(manifest, &src_path).await?;

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
        (status = 409, description = "Workflow id already registered by a running trusted plugin", body = ErrorBody),
        (status = 422, description = "Manifest min_kernel_version exceeds kernel version", body = ErrorBody),
        (status = 500, description = "Spawn failed / internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn enable_plugin(
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    let plug = cs.plugin.enable(&id).await?;
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
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    let plug = cs.plugin.disable(&id).await?;
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
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    cs.plugin.uninstall(&id).await?;
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
        (status = 409, description = "Workflow id already registered by a running trusted plugin", body = ErrorBody),
        (status = 422, description = "Manifest min_kernel_version exceeds kernel version", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reload_plugin(
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    let plug = cs.plugin.reload(&id).await?;
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
    // #1164 §2.5 — a connector never had a token minted, so rotation is a
    // 400, not a 500. The host refuses BEFORE deleting any row and BEFORE
    // restarting anything, so a mistaken call is fully inert.
    cs.plugin
        .rotate_plugin_token(&id)
        .await
        .map_err(|e| match e {
            crate::plugin_host::HostError::UnsupportedForKind { .. } => {
                CalmError::BadRequest(e.to_string())
            }
            // The host fails CLOSED when it cannot determine the plugin's kind
            // (no registry entry). That is a 404, not a kernel fault — and, like
            // the connector refusal, it happens before any token delete/restart.
            crate::plugin_host::HostError::NotFound(_) => {
                CalmError::NotFound(format!("plugin {id} is not loaded"))
            }
            other => CalmError::Internal(format!("rotate failed: {other}")),
        })?;
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
