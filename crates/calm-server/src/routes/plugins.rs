//! `/api/plugins/*` — plugin install, configuration, and lifecycle.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::Plugin;
use crate::plugin_host::managed::{self, ConnectorInstall};
use crate::plugin_host::template_input::{
    TEMPLATE_INPUT_MAX_BYTES, declares_key, reject_undeclared_keys, validate_instance,
};
use crate::plugin_host::{
    Manifest, PluginRegistry, PluginRuntimeStatus, ResourceError, RpcError, effective_config,
    read_ui_resource,
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
use serde_json::{Map, Value};
use std::path::{Path as StdPath, PathBuf};
use utoipa::{IntoParams, ToSchema};

pub fn router() -> Router<AppState> {
    Router::new()
        // /views must be registered before `/:id` paths so it doesn't match the `:id` extractor.
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/views", get(list_plugin_views))
        .route("/api/plugins/install", post(install_plugin))
        .route("/api/plugins/mcp/check", post(check_mcp_connection))
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
        // Browsers must load an iframe src with a real HTTP GET, so the MCP `resources/read`
        // payload is re-exposed here. No cookies; the desktop-local CORS gate is the trust boundary.
        .route(
            "/api/plugins/{id}/resources/{view_id}",
            get(get_plugin_view_html),
        )
        // AppBridge `tools/call` fan-out: `name` MUST start with `neige.`; the iframe never
        // reaches the plugin process.
        .route("/api/plugins/{id}/tool-call", post(plugin_tool_call))
}

/// Compact row used by `GET /api/plugins`; the full manifest is excluded to keep the list cheap.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginListItem {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    /// Wire-name string: `running | spawning | crashed | unavailable | disabled | installing
    /// | installed`. `unavailable` is a normal terminal state: no process was started,
    /// nothing is watching, and nothing will retry until an operator intervenes.
    pub state: String,
    pub manifest_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Does this plugin declare a `config_schema`? Read from the registry, the same source
    /// the write path validates against; `false` when the manifest is not loaded.
    pub has_config: bool,
}

/// Single-plugin detail; the full manifest blob rides along.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginDetail {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    /// Same wire-name set as [`PluginListItem::state`].
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[schema(value_type = Object)]
    pub manifest: Value,
    /// The schema the config form renders from, read from the registry — authoritative
    /// over `manifest.config_schema`, which is the persisted blob and may be stale.
    /// `None` means no schema is in force.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub config_schema: Option<Value>,
    /// What the operator has actually **set** — the persisted row, verbatim,
    /// with no defaults folded in.
    #[schema(value_type = Object)]
    pub user_config: Value,
    /// `defaults ⊕ user_config`, i.e. what the plugin runs with. Carried alongside
    /// `user_config` so a form can tell an operator's choice from a manifest default and
    /// never posts defaults back to be persisted.
    #[schema(value_type = Object)]
    pub effective_config: Value,
    pub installed_at: i64,
    pub updated_at: i64,
}

/// One entry in the `/api/plugins/views` catalog.
#[derive(Debug, Serialize, ToSchema)]
pub struct ViewCatalogEntry {
    /// Canonical MCP Apps URI: `ui://<plugin_id>/<view_id>`.
    pub resource_uri: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_size: Option<ViewSizeWire>,
    /// `"card"` — track/area are banned.
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
    /// An `mcp-http` connector described by the request itself; the kernel synthesizes
    /// the plugin tree and owns it thereafter.
    McpHttp(Box<ConnectorInstall>),
    /// A distinct source tag so older kernels reject the request instead of silently
    /// ignoring `headers` and `tools_all`; `mcp_http` remains accepted.
    McpHttpV2(Box<ConnectorInstall>),
    /// Catch-all so tarball/url/etc. get a friendly 400 instead of a serde error.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct LogQuery {
    pub n: Option<usize>,
}

/// Query for `PATCH /api/plugins/{id}/config`.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ConfigPatchQuery {
    /// Discard the stored `user_config` entirely and apply this patch to an empty object.
    /// The recovery action for a corrupt (non-object) row; on a healthy row it means
    /// "reset this plugin to its manifest defaults".
    #[serde(default)]
    pub reset: bool,
}

/// AppBridge → kernel tool-call wire body, mirroring the JSON-RPC `tools/call` params
/// shape. `call_id` is threaded into every event written while servicing the call as
/// `correlation = "user_tool_call:<call_id>"`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ToolCallBody {
    pub name: String,
    #[serde(default = "default_arguments")]
    #[schema(value_type = Object)]
    pub arguments: Value,
    /// Optional caller-supplied tracing id; absent ⇒ `correlation = NULL`.
    #[serde(default)]
    pub call_id: Option<String>,
}

fn default_arguments() -> Value {
    Value::Object(Default::default())
}

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
            // Not running and no host record: `enabled` separates "never started" from
            // "explicitly disabled".
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
            // Registry, not the persisted blob.
            has_config: registry_manifest(&cs, &plug.id).is_some_and(|m| m.config_schema.is_some()),
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

#[utoipa::path(
    post,
    path = "/api/plugins/install",
    tag = "plugins",
    request_body = InstallBody,
    responses(
        (status = 201, description = "Plugin installed (disabled by default)", body = PluginDetail),
        (status = 400, description = "Manifest invalid / unsupported source", body = ErrorBody),
        (status = 409, description = "Plugin id already installed (`plugin_conflict`), or another lifecycle operation holds this id (`plugin_busy`)", body = ErrorBody),
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
        InstallSource::McpHttp(connector) | InstallSource::McpHttpV2(connector) => {
            let plug = cs.plugin.install_managed_connector(&connector).await?;
            return Ok((StatusCode::CREATED, Json(build_detail(&cs, plug).await)));
        }
        InstallSource::Other => {
            return Err(CalmError::PluginInstall(
                "unsupported source kind — accepted: `local_path`, `mcp_http`, `mcp_http_v2`"
                    .into(),
            ));
        }
    };

    let src_path = resolve_install_source(&raw_path)?;
    if !src_path.is_dir() {
        return Err(CalmError::PluginInstall(format!(
            "source path is not a directory: {}",
            src_path.display()
        )));
    }
    // A `local_path` install may not adopt a tree the kernel wrote: uninstall deletes
    // kernel-written trees by this marker, so adopting one would arm uninstall to delete
    // the operator's own directory.
    if managed::is_managed_tree(&src_path) {
        return Err(CalmError::PluginInstall(format!(
            "{}: {}",
            src_path.display(),
            managed::REJECT_MARKED_SOURCE_HINT
        )));
    }
    let manifest_path = src_path.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        CalmError::PluginInstall(format!("reading {}: {e}", manifest_path.display()))
    })?;
    let manifest =
        Manifest::parse(&manifest_text).map_err(|e| CalmError::PluginInstall(e.to_string()))?;

    let plug = cs.plugin.install(manifest, &src_path).await?;

    let detail = build_detail(&cs, plug).await;
    Ok((StatusCode::CREATED, Json(detail)))
}

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/enable",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Plugin enabled and spawned", body = PluginDetail),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 409, description = "Template id already registered by a running trusted plugin (`plugin_conflict`), or another lifecycle operation holds this plugin (`plugin_busy`)", body = ErrorBody),
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
        (status = 409, description = "Another lifecycle operation holds this plugin (`plugin_busy`)", body = ErrorBody),
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

/// PATCH semantics: absent keys keep their stored value, an explicit `null` deletes the
/// key. Stored keys the schema no longer declares are kept in the row but excluded from
/// validation. `required` is enforced at consumption, not here. Gate order is
/// `404 → 409 → 400`. Taking effect needs an explicit `POST /api/plugins/{id}/reload`.
#[utoipa::path(
    patch,
    path = "/api/plugins/{id}/config",
    tag = "plugins",
    params(
        ("id" = String, Path, description = "Plugin id"),
        ConfigPatchQuery,
    ),
    request_body(
        content = Object,
        description = "Partial user-config object: only the keys being edited. \
                       An explicit `null` deletes a key; absent keys are left alone. \
                       Validated against the plugin manifest's `config_schema`."
    ),
    responses(
        (status = 200, description = "Config updated", body = PluginDetail),
        (status = 400, description = "Plugin declares no `config_schema`, or the patched config violates it (`bad_request`); or the whole stored document would exceed its byte cap because of residue no ordinary patch can shrink (`plugin_config_too_large`, clearable with `?reset=true`)", body = ErrorBody),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 409, description = "Another lifecycle operation holds this plugin (`plugin_busy`); or the plugin row exists but its manifest is not loaded in the kernel registry (`plugin_manifest_unloaded`); or its stored `user_config` is not a JSON object (`plugin_config_corrupt`, clearable with `?reset=true`)", body = ErrorBody),
        (status = 415, description = "Extractor-level rejection (missing/!= `application/json` content type). Raised by axum's `Json` extractor **before** this handler runs, so the body is plain text and carries no `code` — outside the `ErrorBody` contract"),
        (status = 422, description = "Extractor-level rejection (well-formed JSON that is not deserializable into the request type). Same caveat as 415: plain text, no `code`"),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn patch_plugin_config(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
    Query(q): Query<ConfigPatchQuery>,
    Json(body): Json<Value>,
) -> Result<Json<PluginDetail>> {
    // The lifecycle guard: this is a read-modify-write over `user_config`. Without it two
    // concurrent PATCHes drop each other's keys, and a PATCH interleaved with `reload`
    // validates against a stale schema. Taken before the 404; an unknown id's lock is free.
    let _guard = cs
        .plugin
        .try_lock_lifecycle(&id)
        .map_err(crate::plugin_host::lifecycle::spawn_error_to_calm)?;

    // Gate order is `404 → 409 → 400`: the registry lookup must precede the `config_schema` check.
    let existing = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;

    let manifest = registry_manifest(&cs, &id).ok_or_else(|| registry_gap(&id))?;

    // A non-object `user_config` is a corrupt row, not an empty one: coercing it to `{}`
    // would silently discard it. 409 with `?reset=true` as the explicit escape hatch, since
    // this endpoint is the only writer of the field on an installed row.
    let mut merged = if q.reset {
        // The count is logged, never the values — a corrupt `user_config` can hold anything.
        tracing::warn!(
            plugin = %id,
            discarded_keys = existing.user_config.as_object().map_or(0, Map::len),
            stored_kind = kind_of(&existing.user_config),
            "?reset=true discarded the stored plugin user_config"
        );
        Map::new()
    } else {
        match &existing.user_config {
            Value::Object(map) => map.clone(),
            other => {
                return Err(CalmError::PluginConfigCorrupt(format!(
                    "plugin `{id}` has a stored user_config that is not a JSON object \
                     (found {}); refusing to merge into it. Resend with `?reset=true` \
                     to discard it and start from an empty config",
                    kind_of(other)
                )));
            }
        }
    };

    let schema = manifest.config_schema.clone().ok_or_else(|| {
        CalmError::BadRequest(format!(
            "plugin `{id}` declares no `config_schema`, so it has no configurable keys"
        ))
    })?;

    let patch = body.as_object().ok_or_else(|| {
        CalmError::BadRequest(
            "config patch must be a JSON object of the keys being edited".to_string(),
        )
    })?;

    // Key names are judged before `null` may mean anything, so an undeclared key cannot
    // slip past validation by being sent as a deletion.
    reject_undeclared_keys("config", &schema, patch.keys().map(String::as_str))
        .map_err(CalmError::BadRequest)?;

    // Absent = unchanged, explicit null = delete.
    for (key, value) in patch {
        if value.is_null() {
            merged.remove(key);
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }

    // Validate a pruned copy; store the unpruned map. Keys the schema no longer declares
    // are residue that must not lock the operator out, but pruning them from the row
    // would destroy their values.
    let mut judged = merged.clone();
    judged.retain(|key, _| declares_key(&schema, key));

    // `required` is stripped: a key left to its manifest default is not missing. The cap
    // is measured over `judged`; residue is not shrinkable by any ordinary patch.
    let mut structural = schema.clone();
    if let Some(obj) = structural.as_object_mut() {
        obj.remove("required");
    }
    validate_instance("config", &structural, &Value::Object(judged))
        .map_err(CalmError::BadRequest)?;

    // A second cap on the whole stored document: residue only ever accumulates, so
    // without it a schema that narrows repeatedly grows the row without limit.
    // `PluginConfigTooLarge` rather than a bare `BadRequest` so a client can offer the
    // `?reset=true` exit machine-readably.
    let stored = Value::Object(merged);
    let stored_bytes = serde_json::to_string(&stored)
        .map(|s| s.len())
        .unwrap_or(usize::MAX);
    if stored_bytes > USER_CONFIG_MAX_BYTES {
        return Err(CalmError::PluginConfigTooLarge(format!(
            "config: storing this patch would make plugin `{id}`'s user_config {stored_bytes} \
             bytes, over the {USER_CONFIG_MAX_BYTES}-byte cap on the whole stored document. \
             Its declared keys are within the {TEMPLATE_INPUT_MAX_BYTES}-byte cap, so the excess \
             is residue left by keys earlier manifests declared and this one does not — no \
             ordinary patch can shrink it. Resend this request with `?reset=true` to discard the \
             stored document, residue included, and keep exactly the keys you send"
        )));
    }

    let plug = s.repo.plugin_update_user_config(&id, stored).await?;
    Ok(Json(build_detail(&cs, plug).await))
}

#[utoipa::path(
    delete,
    path = "/api/plugins/{id}",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 204, description = "Plugin uninstalled"),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 409, description = "Another lifecycle operation holds this plugin (`plugin_busy`)", body = ErrorBody),
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
    // 404 means never installed, distinct from "installed but never ran" (which returns []).
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    let n = q.n.unwrap_or(200).min(1024);
    let lines = cs.plugin.stderr_tail(&id, n).await.unwrap_or_default();
    Ok(Json(lines))
}

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/reload",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Manifest reloaded + plugin restarted if enabled", body = PluginDetail),
        (status = 400, description = "Manifest invalid / id mismatch after reload", body = ErrorBody),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 409, description = "Template id already registered by a running trusted plugin (`plugin_conflict`), or another lifecycle operation holds this plugin (`plugin_busy`)", body = ErrorBody),
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
    // Only enabled plugins can render.
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
            // ENOENT on the HTML asset is a packaging mistake — 404 with the path; any other I/O
            // error is a 500.
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

/// Compose a `Content-Security-Policy` header value from a view's `_meta.ui.csp` block.
/// `None` when absent or empty — callers then omit the header so AppBridge's default
/// no-network sandbox applies.
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
    // Hard gate: the plugin's own tools are unreachable from the iframe.
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

    // Plugin must be running: the registry copy + subscription table sit on the
    // RunningPlugin record.
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

    // Enforce the manifest's per-view `permissions.tools` allow-list server-side. A
    // running plugin without a registry entry is treated as denied.
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

    // Empty-string call_id is normalized to absent so no `correlation = "user_tool_call:"`
    // row is written.
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

#[utoipa::path(
    post,
    path = "/api/plugins/{id}/rotate-token",
    tag = "plugins",
    params(("id" = String, Path, description = "Plugin id")),
    responses(
        (status = 200, description = "Token rotated", body = PluginDetail),
        (status = 404, description = "Plugin not found", body = ErrorBody),
        (status = 409, description = "Another lifecycle operation holds this plugin (`plugin_busy`)", body = ErrorBody),
        (status = 500, description = "Rotate failed", body = ErrorBody),
    ),
)]
pub(crate) async fn rotate_plugin_token(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    Path(id): Path<String>,
) -> Result<Json<PluginDetail>> {
    // 404 if unknown, rather than the host's BadState wrapping.
    s.repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    cs.plugin
        .rotate_plugin_token(&id)
        .await
        .map_err(|e| rotate_error_to_calm(&id, e))?;
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    Ok(Json(build_detail(&cs, plug).await))
}

/// `POST /api/plugins/{id}/rotate-token`'s [`HostError`] → HTTP mapping; a named
/// function so each cell can be unit-tested.
fn rotate_error_to_calm(id: &str, e: crate::plugin_host::HostError) -> CalmError {
    use crate::plugin_host::HostError;
    match e {
        // A connector never had a token minted, so rotation is a 400. The host refuses
        // before deleting any row or restarting anything.
        unsupported @ HostError::UnsupportedForKind { .. } => {
            CalmError::BadRequest(unsupported.to_string())
        }
        // The host fails CLOSED when it cannot determine the plugin's kind; a 404, before any
        // token delete/restart.
        HostError::NotFound(_) => CalmError::NotFound(format!("plugin {id} is not loaded")),
        // Rotation takes the lifecycle guard as its first act, so busy means nothing was
        // deleted or restarted.
        busy @ HostError::LifecycleBusy(_) => CalmError::PluginBusy(busy.to_string()),
        // An operator's stored `enabled = false` is not a kernel fault.
        disabled @ HostError::OperatorDisabled(_) => {
            CalmError::PluginConflict(disabled.to_string())
        }
        // `Disabled` falls through to the 500 deliberately: it is only reachable for a
        // registered app after the token row was deleted and the plugin stopped, so the
        // request did something and did not finish.
        other => CalmError::Internal(format!("rotate failed: {other}")),
    }
}

fn rpc_to_calm(e: RpcError) -> CalmError {
    // Kernel-extension codes map to plugin-aware variants; bare JSON-RPC codes land as 400.
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

/// Resolve a user-supplied install source path; relative paths resolve against CWD.
/// `..` components are rejected; no `canonicalize`, because the source need not exist
/// under any specific root.
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

/// Join the persisted row with the current runtime status (if any).
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
    // Merged against the registry's typed manifest, the same document the write path
    // validates against.
    let registry = registry_manifest(cs, &plug.id);
    let effective = registry
        .as_ref()
        .map(|m| effective_config(m, &plug.user_config))
        .unwrap_or_default();
    let config_schema = registry.and_then(|m| m.config_schema.clone());
    PluginDetail {
        id: plug.id,
        version: plug.version,
        enabled: plug.enabled,
        state,
        last_error,
        manifest: plug.manifest,
        config_schema,
        user_config: plug.user_config,
        effective_config: Value::Object(effective),
        installed_at: plug.installed_at,
        updated_at: plug.updated_at,
    }
}

/// Cap on the whole serialized `user_config` document a PATCH may leave in the row,
/// residue included; [`TEMPLATE_INPUT_MAX_BYTES`] bounds the declared-key subset only.
/// `?reset=true` is the way back under it.
const USER_CONFIG_MAX_BYTES: usize = 4 * TEMPLATE_INPUT_MAX_BYTES;

fn registry_manifest(cs: &CodexShellState, id: &str) -> Option<Manifest> {
    cs.plugin.registry().get(id)
}

/// The refusal for the [`registry_manifest`] gap: 409, not 400 — the kernel does not
/// currently hold the manifest, and a reload (or fixing a `manifest.json` that failed
/// to parse) makes the identical request succeed.
fn registry_gap(id: &str) -> CalmError {
    CalmError::PluginManifestUnloaded(format!(
        "plugin `{id}` is installed but its manifest is not loaded in the kernel \
         registry, so there is no schema to validate against; reload the plugin, \
         or fix its manifest.json if it failed to parse (a reload will fail again \
         until it does)"
    ))
}

/// Name a JSON value's kind for an error message without printing the value, which an
/// operator may rather not see echoed into a log line.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

// Touch the PluginRuntimeStatus enum so its variants stay in the public surface.
#[allow(dead_code)]
const _RUNTIME_STATUS_LIVE: Option<PluginRuntimeStatus> = None;

#[cfg(test)]
mod rotate_error_mapping_tests {
    //! The `rotate-token` error table, cell by cell.

    use super::rotate_error_to_calm;
    use crate::error::CalmError;
    use crate::plugin_host::HostError;
    use axum::http::StatusCode;

    /// An id the registry does not know — including one that is also in `plugins_disabled`.
    #[test]
    fn not_found_is_a_404_naming_the_plugin() {
        let mapped = rotate_error_to_calm("dev.gone", HostError::NotFound("dev.gone".into()));
        assert_eq!(mapped.status(), StatusCode::NOT_FOUND);
        assert!(
            matches!(&mapped, CalmError::NotFound(m) if m.contains("dev.gone") && m.contains("not loaded")),
            "got {mapped:?}"
        );
    }

    /// Rotating a connector is a client mistake, not a kernel fault.
    #[test]
    fn unsupported_for_kind_is_a_400() {
        let mapped = rotate_error_to_calm(
            "dev.conn",
            HostError::UnsupportedForKind {
                plugin_id: "dev.conn".into(),
                kind: "mcp-http",
                operation: "token rotation",
            },
        );
        assert_eq!(mapped.status(), StatusCode::BAD_REQUEST);
        assert!(matches!(mapped, CalmError::BadRequest(_)));
    }

    #[test]
    fn lifecycle_busy_is_a_409_plugin_busy() {
        let mapped = rotate_error_to_calm("dev.app", HostError::LifecycleBusy("dev.app".into()));
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_busy");
    }

    /// `Disabled` is only reachable for a registered app whose token row was already
    /// deleted, so it stays a 500.
    #[test]
    fn disabled_is_the_documented_500_and_nothing_else_is() {
        let mapped = rotate_error_to_calm("dev.app", HostError::Disabled("dev.app".into()));
        assert_eq!(mapped.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            matches!(&mapped, CalmError::Internal(m) if m.contains("rotate failed")),
            "got {mapped:?}"
        );
    }

    #[test]
    fn operator_disabled_is_a_409_not_the_disabled_500() {
        let mapped = rotate_error_to_calm("dev.app", HostError::OperatorDisabled("dev.app".into()));
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_conflict");
    }
}

/// Transient authenticated diagnostic; never installs or enables a plugin.
#[utoipa::path(post, path = "/api/plugins/mcp/check", request_body = ConnectorInstall,
    responses((status = 200, body = crate::plugin_host::mcp_setup::McpCheckResult),
        (status = 400, body = ErrorBody), (status = 502, body = ErrorBody)), tag = "plugins")]
pub(crate) async fn check_mcp_connection(Json(body): Json<ConnectorInstall>) -> Response {
    match crate::plugin_host::mcp_setup::check(body).await {
        Ok(result) => Json(result).into_response(),
        Err((network, message)) => (
            if network {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::BAD_REQUEST
            },
            Json(serde_json::json!({"code": "mcp_setup_failed", "error": message})),
        )
            .into_response(),
    }
}
