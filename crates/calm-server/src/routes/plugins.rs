//! `/api/plugins/*` — plugin install, configuration, and lifecycle.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::Plugin;
use crate::plugin_host::managed::{self, ConnectorSpec};
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
    /// `unavailable` is a NORMAL terminal state, and what it states is
    /// narrower than "something failed": **no process was started, nothing is
    /// watching, and so nothing will retry.** Unlike `crashed` there is no
    /// supervisor and no backoff behind it — it stands until an operator
    /// intervenes, and `last_error` is their only diagnostic. It is not an
    /// error state of the kernel.
    ///
    /// Two families of plugin reach it, and the shared property above is why
    /// they share the name rather than each getting one:
    ///
    ///   * a connector (`kind: mcp-http` / `cli-query`) whose bring-up failed
    ///     — unreachable upstream, rejected `secrets.json`, boot budget
    ///     exhausted (#1164 §2.2);
    ///   * an `app` the kernel refused to start because its stored
    ///     configuration is unusable — a `config_schema.required` key that
    ///     neither the operator nor a manifest default supplies, or a stored
    ///     configuration that could not be read at all (#1284 §2.4).
    ///
    /// Recovery is the same operator action in every case: fix the cause,
    /// then start the plugin again.
    pub state: String,
    pub manifest_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// #1284 §2.5 — does this plugin declare a `config_schema`?
    ///
    /// The list deliberately does not carry the manifest (see the struct doc),
    /// which left "this plugin has nothing to configure" and "the config
    /// screen isn't built yet" indistinguishable from list data alone — so the
    /// UI could only guess, and guessing wrong produces exactly the empty
    /// shell this work exists to remove. This is the one bit that makes the
    /// question decidable, read from the **registry** — the same source the
    /// write path validates against, so "the form is offered" and "the write
    /// is accepted" cannot disagree. A plugin whose row exists but whose
    /// manifest the kernel has not loaded (see [`registry_manifest`]) reports
    /// `false`: the kernel knows of no configurable key for it, and its
    /// `PATCH` says so explicitly rather than 400-ing as "no schema".
    pub has_config: bool,
}

/// Single-plugin detail returned by GET-by-id, install, enable, disable,
/// config-patch, and reload. The full manifest blob rides along so the UI can
/// render version/author/views without a separate fetch.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginDetail {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    /// Same wire-name set as [`PluginListItem::state`], including
    /// `unavailable` — which is reachable for both connectors and `app`
    /// plugins; see that field's doc for what the state asserts.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[schema(value_type = Object)]
    pub manifest: Value,
    /// #1284 §2.5 / §2.7 — the schema the config form renders from, read from
    /// the **registry**, i.e. from the same document the PATCH validates
    /// against and every S2/S3 consumer will read.
    ///
    /// **Which copy is which, since there are now two.** `manifest` above is
    /// still the persisted `plugins.manifest` blob, verbatim, as it has always
    /// been published — it is the row, and rewriting it here would make this
    /// response a document that exists nowhere. This field is the registry's,
    /// and for `config_schema` specifically it is the authoritative one; when
    /// the two disagree (an install by a pre-#1284 kernel dropped the key from
    /// the blob; a `reload` refreshed the registry) `manifest.config_schema`
    /// is stale or absent and this field is not.
    ///
    /// Round 1 left this half-done and the contradiction was pinned by a test
    /// of its own: `has_config` and `effective_config` had moved to the
    /// registry while the schema a form needs was still only reachable through
    /// the blob, so the API could answer "yes, this plugin is configurable"
    /// and "here is what is in force" while being unable to produce the
    /// document §2.5 renders controls from.
    ///
    /// `None` means the same thing `has_config: false` means on the list row:
    /// no schema is in force — either the manifest declares none, or the
    /// kernel has not loaded it (see [`registry_manifest`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub config_schema: Option<Value>,
    /// What the operator has actually **set** — the persisted row, verbatim,
    /// with no defaults folded in.
    #[schema(value_type = Object)]
    pub user_config: Value,
    /// #1284 §2.3 — `defaults ⊕ user_config`, i.e. what the plugin runs with.
    ///
    /// Carried **alongside** `user_config` rather than replacing it, and that
    /// is a contract, not redundancy. §2.2.4 says defaults are applied on read
    /// and never persisted, and §2.2.5 says a Save may only carry the keys the
    /// operator edited; a form that could not tell "the operator chose `dark`"
    /// from "the manifest defaults to `dark`" would have to post the merged
    /// object back, and its very first Save would materialize every default
    /// into the DB — killing §2.2.4 outright. `user_config` answers "what did
    /// the operator choose" (so a default renders as a placeholder and stays
    /// out of the payload); `effective_config` answers "what is in force".
    /// Dropping `user_config` would also be a wire break for the field's
    /// existing readers, for no gain.
    #[schema(value_type = Object)]
    pub effective_config: Value,
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
    /// `"card"` for M3 — track/area are banned per design §10.
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
    /// #1480 — an `mcp-http` connector described by the request itself. The
    /// kernel synthesizes the plugin tree (`manifest.json`, and `secrets.json`
    /// when a credential is given) and owns it thereafter; see
    /// `plugin_host::managed`.
    ///
    /// This is the only install source that does not require a directory to
    /// exist on the server beforehand, which is what makes "add a connector"
    /// expressible from the UI at all.
    McpHttp(ConnectorSpec),
    /// Catch-all so we can return a friendly 400 for tarball/url/etc. instead
    /// of a serde deserialize error.
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
    /// Discard the stored `user_config` entirely and apply this patch to an
    /// empty object (#1284 S1 review P0-C).
    ///
    /// The recovery action for a row whose `user_config` is not a JSON object,
    /// which the kernel otherwise refuses to merge into (409
    /// `plugin_config_corrupt`) precisely so it does not silently discard
    /// data. Without an explicit knob that refusal is permanent: this endpoint
    /// is the only writer of the field on an installed row (`plugin_install`
    /// sets it once at row creation and its upsert path leaves it alone —
    /// P2-3), so no request could ever restore it.
    ///
    /// It is destructive on purpose and never implicit — that is what makes it
    /// compatible with "a write path must never be the thing that loses
    /// configuration": the deletion is the operator's, named in the request.
    /// It works on a healthy row too, where it means "reset this plugin to its
    /// manifest defaults".
    #[serde(default)]
    pub reset: bool,
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
            // Registry, not the persisted blob — see [`registry_manifest`].
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
        InstallSource::McpHttp(spec) => {
            // The whole operation — manifest synthesis, validation, writing the
            // tree, the row — belongs to the host, which is where the per-id
            // lifecycle guard lives. See `install_managed_connector` for why
            // the tree may not be written out here.
            let plug = cs.plugin.install_managed_connector(&spec).await?;
            return Ok((StatusCode::CREATED, Json(build_detail(&cs, plug).await)));
        }
        InstallSource::Other => {
            return Err(CalmError::PluginInstall(
                "unsupported source kind — accepted: `local_path`, `mcp_http`".into(),
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
    // #1480 — a `local_path` install may not adopt a tree the kernel wrote.
    // Uninstall deletes kernel-written trees and decides that by the marker
    // this refusal keeps exclusive: without it, pointing `local_path` at a
    // directory carrying a copied marker would arm uninstall to delete the
    // operator's own directory.
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

// ---------------------------------------------------------------------------
// PATCH config
// ---------------------------------------------------------------------------

/// #1284 §2.2 — a real PATCH with a real validator.
///
/// What this used to be: `Json<Value>` written to the row wholesale, with the
/// name PATCH and the semantics of PUT and no validation of any kind. Three
/// things change, all of them observable:
///
/// 1. **No `config_schema` ⇒ 400.** A plugin that declares no configurable
///    surface has no key this endpoint could meaningfully store; accepting
///    arbitrary JSON there was how "configuration" stayed a field with no
///    semantics. This **overturns** the existing assertion in
///    `tests/cases/plugin_routes.rs::patch_config_writes_user_config`, which
///    pinned the 200; that test now drives a plugin that declares a schema,
///    and its sibling pins the 400 for one that does not.
/// 2. **Patch semantics** per `INV-SETTINGS-001`: absent keys keep their
///    stored value, an explicit `null` deletes the key (and thereby restores
///    its manifest default — see
///    [`effective_config`](crate::plugin_host::effective_config)).
/// 3. **Validation** against the manifest's `config_schema`, with the byte cap
///    `template_input` already uses.
///
/// ## Every branch that reaches (or refuses) the write, audited
///
/// Round 2 of the S1 review, and the reason this table is in the source rather
/// than in a PR comment. Round 1 introduced two defects **of the class it was
/// fixing**: guarding against "an invisible key locks the operator out" it
/// added a silent destructive write, and guarding against "a coerced row
/// silently loses data" it added a 500 that no API could clear. Patching those
/// two cells one at a time would have been the same mistake a third time, so
/// the branches are enumerated and each is asked the two questions that
/// generated both defects. Any cell answering "yes" is a bug, not a trade-off.
///
/// * **(a) Can this branch lose configuration the operator did not delete?**
/// * **(b) If this branch refuses, is the operator then unable to fix it
///   through any API?**
///
/// | # | branch | writes? | (a) silent loss | (b) locked out | why |
/// |---|---|---|---|---|---|
/// | 1 | unknown id ⇒ 404 | no | no | n/a | there is no row to lose or repair |
/// | 2 | registry has no `Manifest` ⇒ 409 `plugin_manifest_unloaded` | no | no | **no** | `POST /reload` re-reads the manifest; for the durable half (a `manifest.json` that fails to parse) the message names *that* as the thing to fix, because a reload alone would fail again |
/// | 3 | stored `user_config` is not an object ⇒ 409 `plugin_config_corrupt` | no | no | **no** | `?reset=true` replaces it with `{}` from the API. This cell was the round-1 defect: a 500 on the only write path for this field *on an installed row* (round 3 P2-3 moved that claim onto its carrier — `plugin_install`'s upsert no longer resets `user_config`, so it no longer rests on the duplicate-id 409), so every later request failed identically and the outs were uninstall/reinstall or a hand-edited DB |
/// | 4 | `?reset=true` ⇒ base is `{}` | yes | **no** | n/a | destructive, but only on an explicit query parameter — the operator *is* the deletion. Without a knob like this, cell 3 has no exit |
/// | 5 | manifest declares no `config_schema` ⇒ 400 | no | no | no | permanent by construction; there is no configuration to be locked out of |
/// | 6 | body is not a JSON object ⇒ 400 | no | no | no | fixed by resending |
/// | 7 | request names an undeclared key ⇒ 400 | no | no | no | fixed by dropping that key; the request document is the operator's and fully visible to them |
/// | 8 | request key with a non-`null` value | yes | no | n/a | overwrites exactly the key the operator named |
/// | 9 | request key with `null` | yes | no | n/a | deletes exactly the key the operator named |
/// | 10 | stored keys the current schema no longer declares | yes | **no** | no | they are **kept** in the row and merely excluded from what is validated. Round 1 pruned them *and wrote the pruned map back*, destroying an operator's values on an unrelated PATCH — and if the manifest ever widens back, the setting returns. `effective_config` ignores them either way, so nothing runs with them and the unlock is identical |
/// | 11 | merged declared-key document violates the schema ⇒ 400 | no | no | no | the previous row stands; every key named in the error is one the form shows |
/// | 12 | byte cap over the merged declared-key document ⇒ 400 | no | no | **no** | the operator shrinks or clears a declared key and the same request succeeds. It is deliberately **not** taken over the residue in cell 10: those bytes are not shrinkable by an ordinary patch, so refusing on them here would be a lockout — and they are not what a consumer reads, since `effective_config` drops them. **This bounds each write, not the row**: cell 12b is what bounds the row |
/// | 12b | total cap over the whole stored document, residue included ⇒ 400 | no | no | **no** | round 3 (P1-1). Cell 12's exclusion of residue is right for *that* cap and says nothing about the total: residue only ever grows (nothing prunes it, the write is whole-document, `reload` does not touch `user_config`), so `declare {a}` → fill `a` → narrow to `{b}` → reload → fill `b` → narrow … adds ~8 KiB per turn with every step a legal 200, and the row is echoed in every detail response. The refusal names the exit and the exit is real: `?reset=true` carrying the operator's current keys keeps their configuration and drops exactly the residue, in one request. That is why this is a cap and not the lockout cell 12 avoids |
/// | 13 | `plugin_update_user_config` fails ⇒ 500 | no | no | no | genuinely server-side and transient; the row is untouched |
/// | 14 | another lifecycle operation holds the id ⇒ 409 `plugin_busy` | no | no | **no** | round 3 (P1-2). The handler is a read-modify-write and used to take no lock while every other lifecycle entry point does, so **(a) was "yes"**: two concurrent PATCHes both read the old row and the loser's key vanished from the winner's write. The other interleaving is PATCH against `reload`: judge against the registry's schema at line N, store for the schema a consumer reads at line N+1 — and `effective_config` type-checks nothing on read, so that value reaches S2/S3a/S3b verbatim. The guard closes both. Retry is the whole remedy: a refused acquisition has done nothing |
/// | 15 | the `Query` / `Json` extractors reject (`?reset=x`, malformed JSON, no `application/json`) ⇒ 400 / 415 / 422 | no | no | no | **runs before cell 1** — before the handler exists — so it outranks even the 404, and its body is axum's plain text with no `code`: this row is the one place where a response from this path is *outside* the `ErrorBody` contract the rest of the table assumes. Left as-is deliberately: it is the shape of every extractor in this tree, and a rejection wrapper for one endpoint would make this route the exception rather than the rule. The `utoipa` responses list 415 and 422 so the published contract does not claim they cannot happen |
///
/// Three further things the S1 review settled, all of which are semantics
/// rather than plumbing:
///
/// 4. **The request document is judged first, values second.** A key the
///    schema does not declare is refused whatever its value — including
///    `null`. The first cut interpreted `null` as "delete" *before* validating
///    and so let `{"ghost": null}` through with a 200 on a schema that has no
///    `ghost`, which made "the request is validated against the schema" false
///    as written. The key-name rule comes from
///    [`reject_undeclared_keys`](crate::plugin_host::template_input::reject_undeclared_keys),
///    the same function `validate_instance` uses.
/// 5. **Stored keys the current schema no longer declares are excluded from
///    validation, and kept in the row.** They are residue from an older
///    manifest; [`effective_config`](crate::plugin_host::effective_config)
///    already drops them, so nothing runs with them. Carrying them into
///    validation meant that after a schema narrowed, *every* subsequent PATCH
///    — however legal — failed with "unknown field `old`" until the operator
///    guessed to send `{"old": null}` for a key no UI shows. An operator must
///    not be locked out by a key they cannot see. **Round 2 corrected the
///    remedy**: round 1 pruned the merged map and wrote the pruned result
///    back, which unlocked the write by *deleting the operator's data* on an
///    unrelated edit. Validating a pruned copy unlocks it identically and
///    keeps the row intact.
/// 6. **`required` is not enforced here** (design adjudication on the S1
///    review). §2.2.5 says a Save carries only the keys the operator edited,
///    so enforcing `required` on the write would make the first Save of any
///    plugin with two no-default required keys unconditionally 400 — the two
///    rules are incompatible and this is the one that gives. `required` is
///    enforced at **consumption** (S2/S3 bring-up): a plugin missing required
///    configuration does not start, and lands in the `unavailable` +
///    `last_error` terminal state §2.4 already defines. This **overturns**
///    `patch_config_enforces_required_keys_but_lets_defaults_satisfy_them`.
///    With `required` gone, the two validation passes the first cut ran
///    collapse into one — the second existed only to enforce it.
///
/// ## Refusal priority: `404 → 409 → 400`
///
/// The gates run in that order, and the order is part of the contract. Two
/// things sit outside it and are stated rather than hidden:
///
/// * **The extractors run first** (table cell 15). `Query<ConfigPatchQuery>`
///   and `Json<Value>` reject `?reset=x`, malformed JSON and a missing
///   `application/json` before this function is entered, with axum's plain-text
///   400 / 415 / 422 and no `code`. Those responses are *not* `ErrorBody`s;
///   everything the priority list below covers is.
/// * **The lifecycle guard is taken before the 404** (cell 14), the same
///   ordering `install` uses. It cannot change any answer below — a
///   nonexistent id's lock is always free — and it exists so this
///   read-modify-write cannot interleave with itself, or with a `reload`.
///
/// * **404** — an unknown id is an unknown id regardless of what the body
///   says. Checking anything else first would leak "this plugin has no config
///   schema" for plugins that do not exist.
/// * **409** — the two *state* refusals (the registry does not hold this
///   manifest; the stored row is corrupt). Both mean "not right now, and here
///   is the action that changes that".
/// * **400** — everything about this manifest or this request being wrong:
///   no `config_schema` at all, a non-object body, an undeclared key, a value
///   the schema rejects.
///
/// The cell that decides the order is **no schema *and* a registry gap**: it
/// answers **409**, not 400, because the kernel does not know whether this
/// plugin declares a schema — it has not loaded the manifest. Answering 400
/// there would tell the operator "this plugin will never be configurable" on
/// the strength of a document the kernel never read, which is exactly the
/// wrong action (they would stop, rather than reload / fix `manifest.json`).
/// That is why the registry lookup precedes the `config_schema` check in the
/// body below and not the other way round.
///
/// Timing is likewise unchanged, but the comment that used to sit in the body
/// overstated it: it claimed the new config would be read "on next spawn",
/// which was never true of anything — nothing read `user_config` at all.
/// §2.4: the write does not touch the running process or connector, and taking
/// effect needs an explicit `POST /api/plugins/{id}/reload`. For a `cli-query`
/// connector that is not a nicety — its command, PATH and entire env are built
/// once at bring-up and cached, so an un-reloaded config change is
/// *completely* inert, not partly.
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
    // (P1-2, S1 review round 3) **The lifecycle guard.** Everything below is
    // one read-modify-write over `user_config`: read the row, read the
    // registry's schema, judge the merge, write the merge back. Until this
    // round it ran with no lock at all, while `enable` / `disable` / `reload`
    // / `uninstall` all take this same per-id guard — so two concurrent
    // PATCHes could both read the old row and the second write would drop the
    // first one's key (a silent loss of configuration the operator never
    // deleted, which is question (a) of the branch table), and a PATCH
    // interleaved with a `reload` could validate against the schema the
    // registry held at line N and store the result for the schema a consumer
    // reads at line N+1 — `effective_config` does no type checking on read, so
    // that value goes to S2/S3 as-is.
    //
    // Refused acquisitions answer the 409 `plugin_busy` the §2.4 table already
    // defines for every other lifecycle entry point, and — like them — having
    // done nothing at all. The guard is taken before the 404 for the same
    // reason `install` takes it before its duplicate-id probe: nothing above
    // it writes, so ordering it first costs no correctness, and an id that
    // does not exist has a free lock anyway, which is why
    // `patch_config_unknown_id_is_still_404` still holds.
    let _guard = cs
        .plugin
        .try_lock_lifecycle(&id)
        .map_err(crate::plugin_host::lifecycle::spawn_error_to_calm)?;

    // Gate order is `404 → 409 → 400` — see the priority section on this
    // handler's doc for why the registry lookup has to precede the
    // `config_schema` check rather than follow it.
    let existing = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;

    let manifest = registry_manifest(&cs, &id).ok_or_else(|| registry_gap(&id))?;

    // (P0-C) A row whose `user_config` is not an object is a corrupt row, not
    // an empty one: coercing it to `{}` would silently discard whatever it
    // held, and a write path must never be the thing that loses configuration.
    // But refusing is only half an answer — this endpoint is the *only* writer
    // of this field on an existing row, so a refusal with no way out is a
    // permanent lockout with uninstall/reinstall or a hand-edited DB as the
    // only remedies. Hence a 409 with an explicit, operator-initiated escape
    // hatch rather than a 500.
    //
    // "On an existing row" is exact, and round 3 (P2-3) made the code carry
    // it: `plugin_install`'s `INSERT` supplies the initial `{}`, and its
    // `ON CONFLICT DO UPDATE` used to reset `user_config` too — so this
    // sentence held only by way of the duplicate-id 409 in
    // `PluginHost::install`, one TOCTOU-shaped check away from the argument
    // this whole cell rests on. `user_config` is no longer in that update set.
    let mut merged = if q.reset {
        // (P2-4) This is the one branch in the tree that throws an operator's
        // whole configuration document away, and it left no trace anywhere:
        // the row afterwards is indistinguishable from one that was never
        // configured. The count is logged, never the values — a corrupt
        // `user_config` can hold anything, and the refusal above already
        // declines to echo it (see `kind_of`).
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

    // (4) The request's **key names** are judged against the schema first,
    // before `null` is allowed to mean anything, so a key the schema does not
    // declare cannot slip past validation by being sent as a deletion.
    reject_undeclared_keys("config", &schema, patch.keys().map(String::as_str))
        .map_err(CalmError::BadRequest)?;

    // Absent = unchanged, explicit null = delete. Anything else overwrites the
    // one key it names.
    for (key, value) in patch {
        if value.is_null() {
            merged.remove(key);
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }

    // (5) **Validate a pruned copy; store the unpruned map.**
    //
    // Stored keys the current schema no longer declares are residue from an
    // older manifest. They must not be *validated* — carrying them into the
    // validator meant that after a schema narrowed, every subsequent PATCH,
    // however legal, failed with "unknown field `old`" until the operator
    // guessed to send `{"old": null}` for a key no UI shows. But round 1
    // pruned `merged` itself and then wrote the pruned map back, so an
    // unrelated edit of one key silently deleted the operator's values for
    // others — a destructive write, and the very failure the round-1 comment
    // two paragraphs up swore off. Excluding them from the judgement is enough
    // to unlock the write; deleting them from the row buys nothing on top of
    // that and costs the data. `effective_config` drops them on read either
    // way, so nothing runs with them, and if the manifest ever widens back the
    // setting is still there.
    let mut judged = merged.clone();
    judged.retain(|key, _| declares_key(&schema, key));

    // One pass, because there is one question left: is the declared-key
    // document we are about to **store** well-formed — right types, inside the
    // byte cap. `required` is stripped (6): a key left to its manifest default
    // is not missing, and enforcing it here would make a partial Save
    // impossible. The cap is measured over `judged` — see cell 12 of the
    // branch table: the residue in `merged` is not shrinkable through any API
    // and is not what a consumer reads, so counting it would be a lockout.
    let mut structural = schema.clone();
    if let Some(obj) = structural.as_object_mut() {
        obj.remove("required");
    }
    validate_instance("config", &structural, &Value::Object(judged))
        .map_err(CalmError::BadRequest)?;

    // (P1-1, S1 review round 3) **A second cap, on the whole stored
    // document.** The one above is measured over the declared-key subset,
    // deliberately (cell 12): counting residue there would refuse requests the
    // operator has no way to make smaller. But "each write is bounded" is not
    // "the row is bounded", and round 2's note conflated the two. Residue only
    // ever accumulates — nothing prunes it, `plugin_update_user_config` writes
    // the document whole, `reload` does not touch `user_config`, and
    // `effective_config` merely ignores it on read — so the loop
    // `declare {a} → fill a → narrow to {b} → reload → fill b → narrow …`
    // grows the row by ~8 KiB per turn, forever, with every step a legal 200.
    // The row is then re-serialized verbatim into every `GET /api/plugins/:id`.
    //
    // 4× the per-write cap: large enough that the useful case this leaves room
    // for — a manifest that narrows and later widens again, with the operator's
    // old values still waiting (cell 10) — survives several rounds of schema
    // churn, and small enough that the response payload stays bounded by a
    // constant.
    //
    // This is a refusal, so it owes question (b) an answer, and the message has
    // to carry it: `?reset=true` with the keys the operator wants keeps their
    // current configuration and drops exactly the residue, in one request. That
    // is what keeps the cap from being the lockout cell 12 avoided.
    //
    // (#1284 S4 review P2-A) It owes that answer *machine-readably* as well.
    // The message names the exit, but a client cannot act on an English
    // sentence — the web UI offered its "discard the stored configuration"
    // button only for `plugin_config_corrupt`, so the operator read an
    // instruction they had no control to follow, and §2.2.1's "the cap is not a
    // lockout" held only for people with `curl`. Hence
    // `CalmError::PluginConfigTooLarge` rather than a bare `BadRequest`: same
    // 400, same message, but distinguishable from the schema violations that
    // share the status and have no such exit.
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
/// names rewritten to CSP form (`default-src`, etc.); the `extras`
/// flatten-bucket from `CspBlock` is forwarded under its raw key with a
/// best-effort `snake → kebab` rewrite for the same five-or-so canonical
/// directives the CSP specification names. Unknown keys flow through verbatim.
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
        (status = 409, description = "Another lifecycle operation holds this plugin (`plugin_busy`)", body = ErrorBody),
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
        .map_err(|e| rotate_error_to_calm(&id, e))?;
    let plug = s
        .repo
        .plugin_get_by_id(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
    Ok(Json(build_detail(&cs, plug).await))
}

/// `POST /api/plugins/{id}/rotate-token`'s [`HostError`] → HTTP mapping.
///
/// A named function rather than an inline `match` so each cell can be pinned by
/// a unit test the way `spawn_error_to_calm`'s are. #1196 S1 review r4 found the
/// gap this closes: nothing anywhere exercised rotation against an id in
/// `plugins_disabled`, so a probe that changed the host's answer to
/// `HostError::Disabled` silently turned a 404/400 into a 500 and every gate
/// stayed green.
fn rotate_error_to_calm(id: &str, e: crate::plugin_host::HostError) -> CalmError {
    use crate::plugin_host::HostError;
    match e {
        // #1164 §2.5 — a connector never had a token minted, so rotation is a
        // 400, not a 500. The host refuses BEFORE deleting any row and BEFORE
        // restarting anything, so a mistaken call is fully inert.
        unsupported @ HostError::UnsupportedForKind { .. } => {
            CalmError::BadRequest(unsupported.to_string())
        }
        // The host fails CLOSED when it cannot determine the plugin's kind
        // (no registry entry). That is a 404, not a kernel fault — and, like
        // the connector refusal, it happens before any token delete/restart.
        HostError::NotFound(_) => CalmError::NotFound(format!("plugin {id} is not loaded")),
        // #1196 §2.5 — 409 `plugin_busy`, not a 500. Rotation takes the
        // lifecycle guard as its first act, so a busy answer means the
        // token row was NOT deleted and nothing was restarted; the identical
        // request will work once the holder finishes.
        busy @ HostError::LifecycleBusy(_) => CalmError::PluginBusy(busy.to_string()),
        // #1226 — the same 409 `plugin_conflict` `spawn_error_to_calm` gives it,
        // for the same reason: an operator's stored `enabled = false` is not a
        // kernel fault.
        //
        // Reaching this arm is rare but NOT impossible, which is why it exists
        // rather than a comment saying it cannot happen. Rotation itself checks
        // the `enabled` bit before restarting, so the ordinary disabled-plugin
        // rotation returns `Ok` and never gets here. What can still get here is
        // the residual design §2.3 registers explicitly: the route layer holds
        // an `Arc<dyn RouteRepo>` and can flip the row without going through
        // the host, so a write landing between rotation's own read and the
        // spawn door's (`config_for_spawn_or_unavailable`) makes the door
        // refuse a restart rotation had already decided to make. 500 would be
        // the wrong word for that.
        //
        // Known gap, deliberately not closed here: this endpoint's `utoipa`
        // 409 description still names only `plugin_busy`. Widening it rewrites
        // both `openapi.json`s and the five generated TypeScript artifacts,
        // which is a Node-toolchain change this Rust bug fix does not carry.
        disabled @ HostError::OperatorDisabled(_) => {
            CalmError::PluginConflict(disabled.to_string())
        }
        // `Disabled` deliberately falls through to the 500 below and that is a
        // documented pre-existing wart, not an oversight: it can only be reached
        // for a *registered app* named in `plugins_disabled`, i.e. after the
        // token row has already been deleted and the plugin already stopped, so
        // the request genuinely did do something and genuinely did not finish.
        //
        // The cells that must NOT reach it — unregistered, and connector — are
        // answered above and pinned by `a20`. Precisely what "restored" means,
        // since two baselines are easy to confuse: the 404/400 arms themselves
        // were introduced by **#1164** (`6065ef0a`) and were never edited by
        // #1196 — this whole `match` is byte-identical to the one at S1's first
        // commit. What #1196 S1 broke was the *host* answer feeding them: its
        // P1/P2 commit `1fc10775` put a shared `reject_unknown_before_locking`
        // in front of rotate whose first question was `plugins_disabled`, so for
        // an id on the operator's kill switch the host returned `Disabled` and
        // these two arms stopped being reached at all. r4's fix was on the host
        // side (`rotate_admission_check`); re-coding *this* arm is a separate
        // decision. At `main`'s merge-base with this branch the route mapped
        // every `HostError` to 500, but that predates #1164 and is not the
        // baseline either statement is about.
        other => CalmError::Internal(format!("rotate failed: {other}")),
    }
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
    // Merged against the registry's typed manifest — the same document the
    // write path validates against and the same one every runtime consumer
    // (S2/S3) will read, so "which defaults are in force" has one answer. The
    // published `config_schema` comes off that same `Manifest`, so the three
    // config-facing answers this response gives (is it configurable / what is
    // in force / what does the form render) cannot come from different
    // documents — the contradiction round 1 shipped.
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

/// The **registry's** typed manifest for a plugin id — the one and only source
/// of `config_schema` on the route side.
///
/// #1284 S1 review, one root cause behind three defects. The first cut read
/// the schema out of the persisted `plugins.manifest` blob by string key, on
/// the theory that the blob is what `PluginDetail.manifest` publishes. That
/// theory cost more than it bought:
///
/// * **Upgrade.** The blob is written at install time from the manifest as it
///   parsed *then* (`lifecycle.rs`), and a pre-#1284 `Manifest` had no
///   `config_schema` field, so serde dropped the key. Every plugin installed
///   before this kernel would therefore have come up with no config surface at
///   all — no form, and a PATCH that 400s as "declares no `config_schema`" —
///   until someone thought to `/reload` it by hand.
/// * **Drift.** The blob and the registry are written in *opposite orders* by
///   the two paths that write both (install: DB then registry; reload:
///   registry then DB) and boot rebuilds only the registry, so there is a
///   window in which they disagree. Validating a write against the older of
///   the two can store a value the runtime — which reads the registry — will
///   then reject or misread.
/// * **The seam.** §2.3 names `effective_config(&Manifest, ..)` as the one
///   function every consumer goes through. A route side that could not produce
///   a `Manifest` forced a second public entry point taking a bare schema, and
///   a seam with two doors is not a seam.
///
/// (The comment this replaces had the drift direction backwards: it claimed
/// the registry lags the blob. Reload writes the registry first and the DB
/// second; boot writes the registry only.)
///
/// `None` means the row exists but the kernel has not loaded that manifest —
/// the install window (the DB row is written before the registry insert) and,
/// durably, a plugin whose on-disk manifest failed to parse at boot, which
/// `registry::load_from_dir` skips with a `warn!`. Readers report "no
/// configurable keys"; the writer refuses with [`registry_gap`] instead of
/// pretending the plugin declared nothing.
/// Cap on the **whole** serialized `user_config` document a PATCH may leave in
/// the row, residue included — as opposed to
/// [`TEMPLATE_INPUT_MAX_BYTES`], which
/// `patch_plugin_config` measures over the declared-key subset only.
///
/// Two caps because they answer two different questions. The per-write cap
/// bounds what a consumer will read and must exclude residue, or an operator
/// would be refused for bytes no API of theirs can shrink (branch table cell
/// 12). This one bounds what the row can *accumulate*: residue is append-only
/// (see the comment at the check site), so without it a schema that narrows
/// repeatedly grows the row without limit, one legal 200 at a time — and that
/// row is echoed verbatim in every plugin-detail response.
///
/// 4× is a judgement, not a derivation: enough headroom that the narrow →
/// widen round trip cell 10 preserves values for still works across a few
/// rounds of schema churn, while keeping the detail payload bounded by a
/// constant. `?reset=true` is the way back under it.
const USER_CONFIG_MAX_BYTES: usize = 4 * TEMPLATE_INPUT_MAX_BYTES;

fn registry_manifest(cs: &CodexShellState, id: &str) -> Option<Manifest> {
    cs.plugin.registry().get(id)
}

/// The refusal for the [`registry_manifest`] gap: 409, not 400 and not 500.
///
/// It is deliberately a different answer from "this plugin declares no
/// `config_schema`" (400). That one means *never* — stop asking. This one
/// means the kernel does not currently hold this plugin's manifest, which the
/// operator fixes by reloading the plugin (or by fixing the `manifest.json`
/// that failed to parse), and the identical request then succeeds.
///
/// **The distinction lives in the error code** ([`CalmError::PluginManifestUnloaded`],
/// `plugin_manifest_unloaded`), not in this string — round 1 used the generic
/// `Conflict` (`code: "conflict"`), which left every consumer, the test
/// included, matching on message text. `error.rs` says in as many words that
/// codes are where 409s are told apart; this is that rule applied.
///
/// The message names **both** causes because they need different actions. The
/// transient one (the install window, before the registry insert) clears by
/// itself and a reload is a fine reflex. The durable one — a `manifest.json`
/// that `registry::load_from_dir` failed to parse and skipped with a `warn!` —
/// will fail the reload too, and an operator told only "reload the plugin"
/// would loop on it.
fn registry_gap(id: &str) -> CalmError {
    CalmError::PluginManifestUnloaded(format!(
        "plugin `{id}` is installed but its manifest is not loaded in the kernel \
         registry, so there is no schema to validate against; reload the plugin, \
         or fix its manifest.json if it failed to parse (a reload will fail again \
         until it does)"
    ))
}

/// Name a JSON value's kind for an error message, without printing the value —
/// a corrupt `user_config` may hold anything, including something an operator
/// would rather not see echoed into a log line.
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

// Touch the PluginRuntimeStatus enum so its variants stay in the public
// surface — Slice D doesn't construct one directly but consumes
// `wire_name()` / `last_error()` everywhere.
#[allow(dead_code)]
const _RUNTIME_STATUS_LIVE: Option<PluginRuntimeStatus> = None;

#[cfg(test)]
mod rotate_error_mapping_tests {
    //! #1196 S1 review r4 — the `rotate-token` error table, cell by cell.
    //!
    //! The host-side half (which `HostError` each `plugins_disabled` cell
    //! actually produces) is pinned by
    //! `tests/cases/plugin_lifecycle_lock.rs::a20_*`; this is the other half —
    //! the pairing of a `HostError` with a status code, which is where the r4
    //! defect was.
    //!
    //! **These two do not add up to an HTTP contract on their own**, and r5
    //! corrected an earlier version of this note that said they did: a
    //! conjunction of two tests is an argument, and nothing here runs the
    //! handler. The observation that does is
    //! `tests/cases/connector_host.rs::rotate_token_over_http_keeps_its_codes_for_ids_on_the_kill_switch`,
    //! which drives `POST /api/plugins/{id}/rotate-token` for both cells that
    //! are reachable through the route. What this module buys on top of that is
    //! cell-by-cell coverage of arms the route cannot reach — notably the
    //! `Disabled` → 500 residual — at unit cost.

    use super::rotate_error_to_calm;
    use crate::error::CalmError;
    use crate::plugin_host::HostError;
    use axum::http::StatusCode;

    /// An id the registry does not know — including one that is *also* in
    /// `plugins_disabled`, which is the regression this file exists for.
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

    /// A request that did nothing because somebody else holds the guard is a
    /// retryable 409 with its own code, never `internal`.
    #[test]
    fn lifecycle_busy_is_a_409_plugin_busy() {
        let mapped = rotate_error_to_calm("dev.app", HostError::LifecycleBusy("dev.app".into()));
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_busy");
    }

    /// The documented residual: `Disabled` is only reachable here for a
    /// registered app whose token row was already deleted and whose process was
    /// already stopped, so it stays a 500. Pinned so that a future probe cannot
    /// widen this cell to swallow the two above it — which is exactly how the
    /// r4 regression happened.
    #[test]
    fn disabled_is_the_documented_500_and_nothing_else_is() {
        let mapped = rotate_error_to_calm("dev.app", HostError::Disabled("dev.app".into()));
        assert_eq!(mapped.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            matches!(&mapped, CalmError::Internal(m) if m.contains("rotate failed")),
            "got {mapped:?}"
        );
    }

    /// #1226 — the *row's* `enabled = false`, as distinct from the cell above
    /// it, is a 409 `plugin_conflict`.
    ///
    /// Mutation witness: delete the `OperatorDisabled` arm from
    /// `rotate_error_to_calm` and this reports `internal` / 500.
    #[test]
    fn operator_disabled_is_a_409_not_the_disabled_500() {
        let mapped = rotate_error_to_calm("dev.app", HostError::OperatorDisabled("dev.app".into()));
        assert_eq!(mapped.status(), StatusCode::CONFLICT);
        assert_eq!(mapped.code(), "plugin_conflict");
    }
}
