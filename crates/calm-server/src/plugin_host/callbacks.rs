//! `neige.*` host-callback dispatcher.
//!
//! Slice B's `spawn_methodnotfound_drainer` is replaced by `dispatch()`, which
//! takes one plugin-originated request and resolves it against the kernel:
//! permission check → repo write → emit event → respond.
//!
//! Identity rule (design doc §6.2): the plugin's identity is implicit on the
//! connection. The kernel **injects** `plugin_id` from `CallbackCtx`; it does
//! **not** trust any `plugin_id` field in the plugin's params. This is the
//! security spine — without it a misbehaving plugin could overlay-write under
//! another plugin's name.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::db::sqlite::{
    card_create_tx, card_delete_tx, card_update_tx, overlay_delete_tx, overlay_upsert_tx,
};
use crate::db::{Repo, write_with_event_typed};
use crate::event::{Event, EventBus};
use crate::model::{CardPatch, NewCard, NewOverlay};
use crate::validation::{validate_card_payload, validate_overlay_payload};

use super::events::SubscriptionFilter;
use super::mcp::{CallToolResult, McpClient, RpcError};
use super::registry::PluginRegistry;

/// Subscription ids are monotonic per-process. We don't need cryptographic
/// uniqueness — they're scoped to one plugin's MCP connection.
static NEXT_SUB_ID: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// CallbackCtx — handle passed to every dispatch
// ---------------------------------------------------------------------------

/// Everything `dispatch` needs to service one inbound request. Kept as
/// `&CallbackCtx` so the router can construct it once per plugin and reuse it
/// across the request loop.
pub struct CallbackCtx<'a> {
    /// The kernel-enforced plugin identity. NOT taken from request params.
    pub plugin_id: &'a str,
    pub repo: Arc<dyn Repo>,
    pub event_bus: Arc<EventBus>,
    pub registry: Arc<PluginRegistry>,
    /// Outbound MCP channel — used to deliver subscription notifications.
    pub mcp: Arc<McpClient>,
    /// Per-(plugin, sub_id) join-handle table. Lives on `PluginHost` so
    /// `stop()` can abort all subscriptions for a plugin.
    pub subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
}

/// One live subscription. Held by `PluginHost`'s subscription table so the
/// bridge task can be aborted on plugin stop.
pub struct SubscriptionRecord {
    pub plugin_id: String,
    pub sub_id: u64,
    pub task: JoinHandle<()>,
}

// ---------------------------------------------------------------------------
// M2: tools/call → card creation
// ---------------------------------------------------------------------------

/// What we pull out of a successful `tools/call` response when the plugin
/// declared it as a card-creating tool via `_meta.ui.resourceUri`.
///
/// `resource_uri` is the `ui://<plugin>/<view>` string the iframe / card
/// registry will dispatch on (M4 will fully migrate `Card.kind` to this
/// shape; M2 lets new cards adopt it directly).
///
/// `structured_content` is whatever the tool returned in
/// `result.structuredContent` — opaque to the kernel, persisted verbatim in
/// `Card.payload`. `None` is the legitimate "no payload" case; the caller
/// should default to `Value::Null` (or `{}` if the route prefers that).
#[derive(Debug, Clone)]
pub struct CardCreationFromTool {
    pub resource_uri: String,
    pub structured_content: Option<Value>,
}

/// Pull `_meta.ui.resourceUri` out of a `CallToolResult`. Returns `None` if
/// the plugin didn't signal "this tool result should become a card" — the
/// caller (M2's `routes::cards::create`) treats that as 422 / `not_a_card_tool`.
///
/// We **do not** inspect `is_error` here — that's the caller's responsibility
/// (per spec, a tool returning `isError: true` may still legitimately omit
/// `_meta.ui.resourceUri`, but the route should surface the failure as 502
/// before reaching this extractor).
///
/// We also don't validate the URI shape (e.g. `ui://` scheme) — M4 owns the
/// URI parser; for M2 we only need round-trip persistence in `Card.kind`.
pub fn extract_card_creation_from_tool_call_result(
    result: &CallToolResult,
) -> Option<CardCreationFromTool> {
    let resource_uri = result
        .meta
        .as_ref()?
        .pointer("/ui/resourceUri")?
        .as_str()?
        .to_string();
    Some(CardCreationFromTool {
        resource_uri,
        structured_content: result.structured_content.clone(),
    })
}

// ---------------------------------------------------------------------------
// dispatch — the entry point Slice B's drainer used to be
// ---------------------------------------------------------------------------

pub async fn dispatch(
    ctx: &CallbackCtx<'_>,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    match method {
        "neige.overlay.set" => overlay_set(ctx, params).await,
        "neige.overlay.delete" => overlay_delete(ctx, params).await,
        "neige.card.create" => card_create(ctx, params).await,
        "neige.card.update" => card_update(ctx, params).await,
        "neige.card.delete" => card_delete(ctx, params).await,
        "neige.event.subscribe" => event_subscribe(ctx, params).await,
        "neige.kv.get" => kv_get(ctx, params).await,
        "neige.kv.set" => kv_set(ctx, params).await,
        "neige.kv.list" => kv_list(ctx, params).await,
        "neige.kv.delete" => kv_delete(ctx, params).await,
        other => Err(RpcError::method_not_found(other)),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse `params` into a per-method struct. Surfaces a JSON-RPC InvalidParams
/// error with the serde message attached so plugins see exactly which field
/// failed.
fn parse_params<T: for<'de> Deserialize<'de>>(method: &str, params: &Value) -> Result<T, RpcError> {
    serde_json::from_value::<T>(params.clone())
        .map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))
}

fn permission_denied(why: impl Into<String>) -> RpcError {
    RpcError::custom(-32001, why)
}

fn entity_not_found(what: impl Into<String>) -> RpcError {
    RpcError::custom(-32004, what)
}

fn quota_exceeded(why: impl Into<String>) -> RpcError {
    RpcError::custom(-32003, why)
}

fn internal_repo_err(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(format!("repo: {e}"))
}

/// Look up the plugin's manifest from the registry. A missing manifest at this
/// point would mean the plugin was uninstalled mid-connection — we treat it as
/// an internal error since the supervisor should have stopped the process.
fn manifest_permissions(ctx: &CallbackCtx<'_>) -> Result<super::manifest::Permissions, RpcError> {
    ctx.registry
        .get(ctx.plugin_id)
        .map(|m| m.permissions)
        .ok_or_else(|| {
            RpcError::internal(format!(
                "plugin `{}` manifest not in registry (uninstalled mid-flight?)",
                ctx.plugin_id
            ))
        })
}

// ---------------------------------------------------------------------------
// neige.overlay.*
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OverlaySetParams {
    entity_kind: String,
    entity_id: String,
    kind: String,
    payload: Value,
}

async fn overlay_set(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: OverlaySetParams = parse_params("neige.overlay.set", &params)?;
    if p.entity_kind != "wave" && p.entity_kind != "card" {
        return Err(RpcError::invalid_params(format!(
            "entity_kind must be `wave` or `card`, got `{}`",
            p.entity_kind
        )));
    }
    let perms = manifest_permissions(ctx)?;
    if !perms.can_overlay_write(&p.entity_kind, &p.kind) {
        return Err(permission_denied(format!(
            "plugin `{}` not granted overlay_write on entity_kind=`{}`",
            ctx.plugin_id, p.entity_kind
        )));
    }
    // D4: validate kernel-owned overlay kinds; plugin-defined kinds opaque.
    if let Err(e) = validate_overlay_payload(&p.kind, &p.payload) {
        return Err(RpcError::invalid_params(e.to_string()));
    }
    // plugin_id is server-enforced; we ignore any field the plugin tried to set.
    let new_overlay = NewOverlay {
        plugin_id: ctx.plugin_id.to_string(),
        entity_kind: p.entity_kind.clone(),
        entity_id: p.entity_id.clone(),
        kind: p.kind.clone(),
        payload: p.payload,
    };
    let actor = format!("plugin:{}", ctx.plugin_id);
    let (stored, _id) = write_with_event_typed(
        ctx.repo.as_ref(),
        &actor,
        None,
        ctx.event_bus.as_ref(),
        move |tx| {
            Box::pin(async move {
                let stored = overlay_upsert_tx(tx, new_overlay).await?;
                Ok((stored.clone(), Event::OverlaySet(stored)))
            })
        },
    )
    .await
    .map_err(internal_repo_err)?;
    Ok(json!({ "overlay_id": stored.id, "updated_at": stored.updated_at }))
}

#[derive(Deserialize)]
struct OverlayDeleteParams {
    entity_kind: String,
    entity_id: String,
    kind: String,
}

async fn overlay_delete(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: OverlayDeleteParams = parse_params("neige.overlay.delete", &params)?;
    if p.entity_kind != "wave" && p.entity_kind != "card" {
        return Err(RpcError::invalid_params(format!(
            "entity_kind must be `wave` or `card`, got `{}`",
            p.entity_kind
        )));
    }
    let perms = manifest_permissions(ctx)?;
    if !perms.can_overlay_write(&p.entity_kind, &p.kind) {
        return Err(permission_denied(format!(
            "plugin `{}` not granted overlay_write on entity_kind=`{}`",
            ctx.plugin_id, p.entity_kind
        )));
    }
    // Scope strictly to this plugin's overlays — repo enforces by passing the
    // server-known plugin_id.
    let actor = format!("plugin:{}", ctx.plugin_id);
    let plugin_id_owned = ctx.plugin_id.to_string();
    let entity_kind = p.entity_kind.clone();
    let entity_id = p.entity_id.clone();
    let kind = p.kind.clone();
    let result = write_with_event_typed(
        ctx.repo.as_ref(),
        &actor,
        None,
        ctx.event_bus.as_ref(),
        move |tx| {
            Box::pin(async move {
                overlay_delete_tx(tx, &plugin_id_owned, &entity_kind, &entity_id, &kind).await?;
                Ok((
                    (),
                    Event::OverlayDeleted {
                        plugin_id: plugin_id_owned,
                        entity_kind,
                        entity_id,
                        kind,
                    },
                ))
            })
        },
    )
    .await;
    match result {
        Ok(_) => Ok(json!({ "deleted": true })),
        // Treat a missing overlay as idempotent success; plugins reissuing
        // delete during reconnect shouldn't fail their event loop.
        Err(crate::error::CalmError::NotFound(_)) => Ok(json!({ "deleted": false })),
        Err(e) => Err(internal_repo_err(e)),
    }
}

// ---------------------------------------------------------------------------
// neige.card.*
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CardCreateParams {
    wave_id: String,
    kind: String,
    #[serde(default)]
    payload: Value,
    #[serde(default)]
    sort: Option<f64>,
}

async fn card_create(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: CardCreateParams = parse_params("neige.card.create", &params)?;
    let perms = manifest_permissions(ctx)?;
    if !perms.can_card_create(&p.kind, ctx.plugin_id) {
        return Err(permission_denied(format!(
            "plugin `{}` cannot create cards of kind `{}` (must be `terminal` or start with `plugin:{}:`)",
            ctx.plugin_id, p.kind, ctx.plugin_id,
        )));
    }
    let payload = if p.payload.is_null() {
        json!({})
    } else {
        p.payload
    };
    // D4: kernel-owned card kinds (currently `terminal`) must match shape;
    // plugin-prefixed and ui:// kinds remain opaque.
    if let Err(e) = validate_card_payload(&p.kind, &payload) {
        return Err(RpcError::invalid_params(e.to_string()));
    }
    let new = NewCard {
        wave_id: p.wave_id,
        kind: p.kind,
        sort: p.sort,
        payload,
    };
    let actor = format!("plugin:{}", ctx.plugin_id);
    let (stored, _id) = write_with_event_typed(
        ctx.repo.as_ref(),
        &actor,
        None,
        ctx.event_bus.as_ref(),
        move |tx| {
            Box::pin(async move {
                let stored = card_create_tx(tx, new).await?;
                Ok((stored.clone(), Event::CardAdded(stored)))
            })
        },
    )
    .await
    .map_err(|e| match e {
        crate::error::CalmError::NotFound(s) => entity_not_found(s),
        other => internal_repo_err(other),
    })?;
    serde_json::to_value(&stored).map_err(|e| RpcError::internal(format!("serde: {e}")))
}

#[derive(Deserialize)]
struct CardUpdateParams {
    card_id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    sort: Option<f64>,
    #[serde(default)]
    payload: Option<Value>,
}

async fn card_update(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: CardUpdateParams = parse_params("neige.card.update", &params)?;
    let card = ctx
        .repo
        .card_get(&p.card_id)
        .await
        .map_err(internal_repo_err)?
        .ok_or_else(|| entity_not_found(format!("card {}", p.card_id)))?;
    let perms = manifest_permissions(ctx)?;
    if !perms.can_card_modify(&card.kind, ctx.plugin_id) {
        return Err(permission_denied(format!(
            "plugin `{}` cannot modify card `{}` (kind `{}` not owned by this plugin)",
            ctx.plugin_id, p.card_id, card.kind,
        )));
    }
    // If the plugin tried to change `kind`, also require can_card_create on
    // the new kind so it can't bypass create-permissions by patching.
    if let Some(new_kind) = &p.kind
        && !perms.can_card_create(new_kind, ctx.plugin_id)
    {
        return Err(permission_denied(format!(
            "plugin `{}` cannot retarget card to kind `{}`",
            ctx.plugin_id, new_kind,
        )));
    }
    // D4: if the patch carries a payload, validate against the effective
    // kind (the new kind if retargeting, otherwise the existing card's kind).
    if let Some(payload) = p.payload.as_ref() {
        let kind = p.kind.as_deref().unwrap_or(card.kind.as_str());
        if let Err(e) = validate_card_payload(kind, payload) {
            return Err(RpcError::invalid_params(e.to_string()));
        }
    }
    let patch = CardPatch {
        kind: p.kind,
        sort: p.sort,
        payload: p.payload,
    };
    let actor = format!("plugin:{}", ctx.plugin_id);
    let card_id = p.card_id.clone();
    let (updated, _id) = write_with_event_typed(
        ctx.repo.as_ref(),
        &actor,
        None,
        ctx.event_bus.as_ref(),
        move |tx| {
            Box::pin(async move {
                let updated = card_update_tx(tx, &card_id, patch).await?;
                Ok((updated.clone(), Event::CardUpdated(updated)))
            })
        },
    )
    .await
    .map_err(|e| match e {
        crate::error::CalmError::NotFound(s) => entity_not_found(s),
        other => internal_repo_err(other),
    })?;
    serde_json::to_value(&updated).map_err(|e| RpcError::internal(format!("serde: {e}")))
}

#[derive(Deserialize)]
struct CardDeleteParams {
    card_id: String,
}

async fn card_delete(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: CardDeleteParams = parse_params("neige.card.delete", &params)?;
    let card = ctx
        .repo
        .card_get(&p.card_id)
        .await
        .map_err(internal_repo_err)?
        .ok_or_else(|| entity_not_found(format!("card {}", p.card_id)))?;
    let perms = manifest_permissions(ctx)?;
    if !perms.can_card_delete(&card.kind, ctx.plugin_id) {
        return Err(permission_denied(format!(
            "plugin `{}` cannot delete card `{}` (kind `{}` not owned by this plugin)",
            ctx.plugin_id, p.card_id, card.kind,
        )));
    }
    let wave_id = card.wave_id.clone();
    let card_id = p.card_id.clone();
    let actor = format!("plugin:{}", ctx.plugin_id);
    let _ = write_with_event_typed(
        ctx.repo.as_ref(),
        &actor,
        None,
        ctx.event_bus.as_ref(),
        move |tx| {
            Box::pin(async move {
                card_delete_tx(tx, &card_id).await?;
                Ok((
                    (),
                    Event::CardDeleted {
                        id: card_id,
                        wave_id,
                    },
                ))
            })
        },
    )
    .await
    .map_err(|e| match e {
        crate::error::CalmError::NotFound(s) => entity_not_found(s),
        other => internal_repo_err(other),
    })?;
    Ok(json!({}))
}

// ---------------------------------------------------------------------------
// neige.event.subscribe — long-lived
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EventSubscribeParams {
    #[serde(default)]
    filter: SubscriptionFilter,
}

async fn event_subscribe(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: EventSubscribeParams = parse_params("neige.event.subscribe", &params)?;
    let perms = manifest_permissions(ctx)?;
    // Enforce one permission check per glob the plugin asked for. An empty
    // `events` list means "match everything" — we treat that as needing the
    // firehose grant.
    if p.filter.events.is_empty() {
        if !perms.can_subscribe("*") {
            return Err(permission_denied(format!(
                "plugin `{}` not granted firehose event subscription",
                ctx.plugin_id
            )));
        }
    } else {
        for g in &p.filter.events {
            if !perms.can_subscribe(g) {
                return Err(permission_denied(format!(
                    "plugin `{}` not granted event subscription `{}`",
                    ctx.plugin_id, g
                )));
            }
        }
    }

    let sub_id = NEXT_SUB_ID.fetch_add(1, Ordering::Relaxed);
    let plugin_id = ctx.plugin_id.to_string();
    let mcp = Arc::clone(&ctx.mcp);
    let mut rx = ctx.event_bus.subscribe();
    let filter = p.filter;

    // Bridge task: pull from the broadcast, apply the filter, fan out as MCP
    // notifications. Notifications use the standard JSON-RPC notification
    // shape (no id, method `neige.event`). We try_send via call ... no —
    // McpClient doesn't expose try_send; `notify` is the public surface.
    // We don't await individual sends so a slow plugin can't stall the bus;
    // the McpClient's outbound channel is bounded and will drop if backed up.
    let task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => {
                    let ev = env.event;
                    if !filter.matches(&ev) {
                        continue;
                    }
                    // Plugin notification payload mirrors the WS wire shape:
                    // `_id` is the persisted events.id, alongside the typed
                    // event. Plugins can use `_id` for the same cursor /
                    // dedupe purposes the browser will (Scope D).
                    let mut body = serde_json::Map::new();
                    body.insert("subscription_id".into(), json!(sub_id));
                    body.insert("_id".into(), json!(env.id));
                    body.insert(
                        "event".into(),
                        serde_json::to_value(&ev).unwrap_or(serde_json::Value::Null),
                    );
                    let body = serde_json::Value::Object(body);
                    // notify returns Err on transport-closed; bail then so we
                    // don't spin until plugin stop.
                    if mcp.notify("neige.event", body).await.is_err() {
                        tracing::debug!(
                            plugin_id = %plugin_id,
                            sub_id,
                            "event subscription bridge: transport closed; exiting"
                        );
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        sub_id,
                        dropped = n,
                        "event subscription lagged; dropping events"
                    );
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    ctx.subscriptions.lock().await.push(SubscriptionRecord {
        plugin_id: ctx.plugin_id.to_string(),
        sub_id,
        task,
    });

    Ok(json!({ "subscription_id": sub_id }))
}

// ---------------------------------------------------------------------------
// neige.kv.*
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct KvGetParams {
    key: String,
}

async fn kv_get(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: KvGetParams = parse_params("neige.kv.get", &params)?;
    let v = ctx
        .repo
        .plugin_kv_get(ctx.plugin_id, &p.key)
        .await
        .map_err(internal_repo_err)?;
    Ok(json!({ "value": v }))
}

#[derive(Deserialize)]
struct KvSetParams {
    key: String,
    value: Value,
}

async fn kv_set(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: KvSetParams = parse_params("neige.kv.set", &params)?;
    let perms = manifest_permissions(ctx)?;
    let quota = perms.kv_quota_bytes();

    // Quota: byte-count of the existing keyset plus the proposed value,
    // minus the bytes the old value (if any) was using. Use serde_json's
    // textual length as the proxy.
    let new_value_bytes = serde_json::to_string(&p.value)
        .map(|s| s.len() as u64)
        .unwrap_or(0);
    let key_bytes = p.key.len() as u64;

    let existing = ctx
        .repo
        .plugin_kv_list(ctx.plugin_id, "")
        .await
        .map_err(internal_repo_err)?;
    let mut total: u64 = 0;
    let mut old_for_this_key: u64 = 0;
    for (k, v) in &existing {
        let v_bytes = serde_json::to_string(v)
            .map(|s| s.len() as u64)
            .unwrap_or(0);
        let entry_bytes = (k.len() as u64).saturating_add(v_bytes);
        total = total.saturating_add(entry_bytes);
        if k == &p.key {
            old_for_this_key = entry_bytes;
        }
    }
    let projected = total
        .saturating_sub(old_for_this_key)
        .saturating_add(key_bytes)
        .saturating_add(new_value_bytes);
    if projected > quota {
        return Err(quota_exceeded(format!(
            "kv quota exceeded: plugin `{}` would use {} bytes, limit is {}",
            ctx.plugin_id, projected, quota,
        )));
    }

    ctx.repo
        .plugin_kv_set(ctx.plugin_id, &p.key, &p.value)
        .await
        .map_err(internal_repo_err)?;
    Ok(json!({}))
}

#[derive(Deserialize)]
struct KvListParams {
    #[serde(default)]
    prefix: Option<String>,
}

async fn kv_list(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: KvListParams = parse_params("neige.kv.list", &params)?;
    let entries = ctx
        .repo
        .plugin_kv_list(ctx.plugin_id, p.prefix.as_deref().unwrap_or(""))
        .await
        .map_err(internal_repo_err)?;
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|(k, v)| json!({ "key": k, "value": v }))
        .collect();
    Ok(json!({ "entries": entries }))
}

#[derive(Deserialize)]
struct KvDeleteParams {
    key: String,
}

async fn kv_delete(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: KvDeleteParams = parse_params("neige.kv.delete", &params)?;
    ctx.repo
        .plugin_kv_delete(ctx.plugin_id, &p.key)
        .await
        .map_err(internal_repo_err)?;
    Ok(json!({}))
}

// ===========================================================================
// Unit tests — direct calls against `dispatch` with a hand-rolled
// CallbackCtx (in-memory SqlxRepo + in-process EventBus + a stub McpClient
// that we build with `tokio::io::duplex`). Slice C's binding spec calls these
// "acceptable as long as they cover every method"; the end-to-end stub
// alternative is heavier and adds little extra signal once the router is
// directly exercised.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::model::{NewCove, NewPlugin, NewWave};
    use crate::plugin_host::manifest::Manifest;
    use crate::plugin_host::mcp::McpClient;
    use crate::plugin_host::registry::PluginRegistry;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::Mutex;

    /// Test scaffold: builds a CallbackCtx with a seeded cove + wave so card
    /// tests have something to attach to. The McpClient is a real one wired
    /// to a stub "plugin" that auto-replies to `initialize` then drains.
    struct Harness {
        ctx_storage: Arc<HarnessStorage>,
        wave_id: String,
    }

    /// Owned state that backs every test's CallbackCtx. Stored behind an
    /// Arc so we can clone-and-borrow without struggling with self-ref
    /// lifetimes inside the test functions.
    struct HarnessStorage {
        plugin_id: String,
        repo: Arc<dyn Repo>,
        event_bus: Arc<EventBus>,
        registry: Arc<PluginRegistry>,
        mcp: Arc<McpClient>,
        subs: Arc<Mutex<Vec<SubscriptionRecord>>>,
    }

    fn manifest_with_full_perms(id: &str) -> Manifest {
        let json = serde_json::json!({
            "manifest_version": 1,
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Test",
            "entrypoint": { "command": "bin/stub" },
            "permissions": {
                "overlays_write": ["wave", "card"],
                "cards_create": true,
                "cards_read_all": true,
                "events_subscribe": ["*"],
                "kv_quota_bytes": 1048576
            }
        });
        Manifest::parse(&json.to_string()).expect("manifest parses")
    }

    fn manifest_no_perms(id: &str) -> Manifest {
        let json = serde_json::json!({
            "manifest_version": 1,
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Test",
            "entrypoint": { "command": "bin/stub" }
        });
        Manifest::parse(&json.to_string()).expect("manifest parses")
    }

    /// Build a real `McpClient` wired to an in-process stub. The stub just
    /// answers `initialize` and silently drops everything else; that's all
    /// our callback tests need (they don't actually consume notifications).
    async fn stub_mcp_client() -> Arc<McpClient> {
        let (kernel, plugin) = tokio::io::duplex(64 * 1024);
        let (k_r, k_w) = tokio::io::split(kernel);
        let (p_r, p_w) = tokio::io::split(plugin);

        tokio::spawn(async move {
            let mut reader = BufReader::new(p_r);
            let mut writer = p_w;
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader.read_line(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let v: serde_json::Value = match serde_json::from_str(buf.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(id) = v.get("id").cloned() {
                    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let reply = if method == "initialize" {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": "2025-11-25",
                                "serverInfo": { "name": "stub", "version": "0.0.0" },
                                "capabilities": {}
                            }
                        })
                    } else {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {}
                        })
                    };
                    let mut s = serde_json::to_string(&reply).unwrap();
                    s.push('\n');
                    let _ = writer.write_all(s.as_bytes()).await;
                    let _ = writer.flush().await;
                }
            }
        });

        McpClient::connect(k_r, k_w).await.expect("stub connect")
    }

    impl Harness {
        async fn new(plugin_id: &str, manifest: Manifest) -> Self {
            let repo: Arc<dyn Repo> = Arc::new(
                SqlxRepo::open("sqlite::memory:")
                    .await
                    .expect("open in-memory sqlite repo"),
            );
            // Seed a plugin row so kv writes pass the FK check (the production
            // host always installs the plugin row before the child can call
            // `neige.kv.*`; we mirror that here).
            repo.plugin_install(NewPlugin {
                id: plugin_id.into(),
                version: "0.1.0".into(),
                install_path: format!("/tmp/{plugin_id}"),
                manifest: json!({}),
                enabled: true,
                user_config: json!({}),
            })
            .await
            .unwrap();
            // Seed a cove + wave so card tests can attach.
            let cove = repo
                .cove_create(NewCove {
                    name: "test".into(),
                    color: "#fff".into(),
                    sort: None,
                })
                .await
                .unwrap();
            let wave = repo
                .wave_create(NewWave {
                    cove_id: cove.id.clone(),
                    title: "w".into(),
                    sort: None,
                })
                .await
                .unwrap();

            let event_bus = Arc::new(EventBus::new());
            let registry = Arc::new(PluginRegistry::empty());
            registry.insert(manifest, None);
            let mcp = stub_mcp_client().await;
            let subs = Arc::new(Mutex::new(Vec::new()));

            Self {
                ctx_storage: Arc::new(HarnessStorage {
                    plugin_id: plugin_id.to_string(),
                    repo,
                    event_bus,
                    registry,
                    mcp,
                    subs,
                }),
                wave_id: wave.id,
            }
        }

        fn ctx(&self) -> CallbackCtx<'_> {
            CallbackCtx {
                plugin_id: &self.ctx_storage.plugin_id,
                repo: Arc::clone(&self.ctx_storage.repo),
                event_bus: Arc::clone(&self.ctx_storage.event_bus),
                registry: Arc::clone(&self.ctx_storage.registry),
                mcp: Arc::clone(&self.ctx_storage.mcp),
                subscriptions: Arc::clone(&self.ctx_storage.subs),
            }
        }
    }

    // ----- overlay -----------------------------------------------------------

    #[tokio::test]
    async fn overlay_set_writes_with_server_plugin_id() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let res = dispatch(
            &h.ctx(),
            "neige.overlay.set",
            json!({
                // Plugin tries to lie about plugin_id — server must ignore it.
                "plugin_id": "evil",
                "entity_kind": "wave",
                "entity_id": h.wave_id,
                "kind": "status",
                "payload": { "state": "running" }
            }),
        )
        .await
        .expect("set");
        assert!(res["overlay_id"].is_string());

        let stored = h
            .ctx_storage
            .repo
            .overlays_for("wave", &h.wave_id)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].plugin_id, "p1", "plugin_id is server-enforced");
        assert_eq!(stored[0].kind, "status");
    }

    #[tokio::test]
    async fn overlay_set_denied_without_permission() {
        let h = Harness::new("p1", manifest_no_perms("p1")).await;
        let err = dispatch(
            &h.ctx(),
            "neige.overlay.set",
            json!({
                "entity_kind": "wave",
                "entity_id": h.wave_id,
                "kind": "status",
                "payload": {}
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32001, "PluginPermissionDenied");
    }

    #[tokio::test]
    async fn overlay_delete_round_trip() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        dispatch(
            &h.ctx(),
            "neige.overlay.set",
            json!({
                "entity_kind": "wave",
                "entity_id": h.wave_id,
                "kind": "status",
                // D4: `status` payload must include `state` since it's a
                // kernel-owned overlay kind.
                "payload": { "state": "running" }
            }),
        )
        .await
        .unwrap();
        let del = dispatch(
            &h.ctx(),
            "neige.overlay.delete",
            json!({
                "entity_kind": "wave",
                "entity_id": h.wave_id,
                "kind": "status",
            }),
        )
        .await
        .unwrap();
        assert_eq!(del["deleted"], true);

        let stored = h
            .ctx_storage
            .repo
            .overlays_for("wave", &h.wave_id)
            .await
            .unwrap();
        assert!(stored.is_empty());
    }

    #[tokio::test]
    async fn overlay_set_rejects_bogus_entity_kind() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let err = dispatch(
            &h.ctx(),
            "neige.overlay.set",
            json!({
                "entity_kind": "cove",
                "entity_id": "x",
                "kind": "status",
                "payload": {}
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
    }

    // ----- card --------------------------------------------------------------

    #[tokio::test]
    async fn card_create_with_own_prefix() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let res = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({
                "wave_id": h.wave_id,
                "kind": "plugin:p1:demo",
                "payload": { "x": 1 }
            }),
        )
        .await
        .expect("create");
        assert_eq!(res["kind"], "plugin:p1:demo");
        let cards = h.ctx_storage.repo.cards_by_wave(&h.wave_id).await.unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, "plugin:p1:demo");
    }

    #[tokio::test]
    async fn card_create_terminal_allowed() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let res = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({
                "wave_id": h.wave_id,
                "kind": "terminal"
            }),
        )
        .await
        .expect("terminal card create allowed");
        assert_eq!(res["kind"], "terminal");
    }

    #[tokio::test]
    async fn card_create_denies_other_plugin_prefix() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let err = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({
                "wave_id": h.wave_id,
                "kind": "plugin:other:demo"
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32001);
    }

    #[tokio::test]
    async fn card_update_only_for_own_cards() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        // Pre-seed a terminal card we don't own (plugin can't modify).
        let card = h
            .ctx_storage
            .repo
            .card_create(NewCard {
                wave_id: h.wave_id.clone(),
                kind: "terminal".into(),
                sort: None,
                payload: json!({}),
            })
            .await
            .unwrap();
        let err = dispatch(
            &h.ctx(),
            "neige.card.update",
            json!({ "card_id": card.id, "payload": { "y": 2 } }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32001, "cannot modify terminal cards");
    }

    #[tokio::test]
    async fn card_update_own_card_works() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let create = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({ "wave_id": h.wave_id, "kind": "plugin:p1:demo" }),
        )
        .await
        .unwrap();
        let cid = create["id"].as_str().unwrap().to_string();
        let upd = dispatch(
            &h.ctx(),
            "neige.card.update",
            json!({ "card_id": cid, "payload": { "x": 42 } }),
        )
        .await
        .unwrap();
        assert_eq!(upd["payload"]["x"], 42);
    }

    #[tokio::test]
    async fn card_delete_own_card_works() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let create = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({ "wave_id": h.wave_id, "kind": "plugin:p1:demo" }),
        )
        .await
        .unwrap();
        let cid = create["id"].as_str().unwrap().to_string();
        let res = dispatch(&h.ctx(), "neige.card.delete", json!({ "card_id": cid }))
            .await
            .unwrap();
        assert_eq!(res, json!({}));
        let cards = h.ctx_storage.repo.cards_by_wave(&h.wave_id).await.unwrap();
        assert!(cards.is_empty());
    }

    // ----- kv ----------------------------------------------------------------

    #[tokio::test]
    async fn kv_set_get_round_trip() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        dispatch(
            &h.ctx(),
            "neige.kv.set",
            json!({ "key": "answer", "value": 42 }),
        )
        .await
        .unwrap();
        let got = dispatch(&h.ctx(), "neige.kv.get", json!({ "key": "answer" }))
            .await
            .unwrap();
        assert_eq!(got["value"], 42);
    }

    #[tokio::test]
    async fn kv_get_missing_returns_null() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let got = dispatch(&h.ctx(), "neige.kv.get", json!({ "key": "missing" }))
            .await
            .unwrap();
        assert!(got["value"].is_null());
    }

    #[tokio::test]
    async fn kv_list_with_prefix() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        for k in &["run/1", "run/2", "other/3"] {
            dispatch(&h.ctx(), "neige.kv.set", json!({ "key": k, "value": k }))
                .await
                .unwrap();
        }
        let list = dispatch(&h.ctx(), "neige.kv.list", json!({ "prefix": "run/" }))
            .await
            .unwrap();
        let entries = list["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn kv_delete_removes_key() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        dispatch(
            &h.ctx(),
            "neige.kv.set",
            json!({ "key": "k", "value": "v" }),
        )
        .await
        .unwrap();
        dispatch(&h.ctx(), "neige.kv.delete", json!({ "key": "k" }))
            .await
            .unwrap();
        let got = dispatch(&h.ctx(), "neige.kv.get", json!({ "key": "k" }))
            .await
            .unwrap();
        assert!(got["value"].is_null());
    }

    #[tokio::test]
    async fn kv_quota_enforced() {
        // Manifest with a tiny 64-byte budget.
        let json = serde_json::json!({
            "manifest_version": 1,
            "id": "p1",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Test",
            "entrypoint": { "command": "bin/stub" },
            "permissions": { "kv_quota_bytes": 64 }
        });
        let m = Manifest::parse(&json.to_string()).unwrap();
        let h = Harness::new("p1", m).await;
        // 64-byte quota: short value fits.
        dispatch(
            &h.ctx(),
            "neige.kv.set",
            json!({ "key": "k", "value": "small" }),
        )
        .await
        .unwrap();
        // Large value should bust the quota.
        let big = "x".repeat(256);
        let err = dispatch(
            &h.ctx(),
            "neige.kv.set",
            json!({ "key": "k2", "value": big }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32003);
    }

    // ----- event.subscribe ---------------------------------------------------

    #[tokio::test]
    async fn event_subscribe_returns_id_and_registers_task() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let res = dispatch(
            &h.ctx(),
            "neige.event.subscribe",
            json!({ "filter": { "events": ["card.*"] } }),
        )
        .await
        .unwrap();
        assert!(res["subscription_id"].is_number());
        let subs = h.ctx_storage.subs.lock().await;
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].plugin_id, "p1");
    }

    #[tokio::test]
    async fn event_subscribe_denied_without_permission() {
        let h = Harness::new("p1", manifest_no_perms("p1")).await;
        let err = dispatch(
            &h.ctx(),
            "neige.event.subscribe",
            json!({ "filter": { "events": ["card.*"] } }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32001);
    }

    // ----- M2: extract_card_creation_from_tool_call_result -------------------

    #[test]
    fn extract_card_creation_picks_resource_uri_and_structured_content() {
        let result = CallToolResult {
            content: vec![],
            is_error: Some(false),
            meta: Some(json!({
                "ui": { "resourceUri": "ui://dev.neige.hello-world/status" }
            })),
            structured_content: Some(json!({ "state": "running" })),
        };
        let got =
            extract_card_creation_from_tool_call_result(&result).expect("expected resource_uri");
        assert_eq!(got.resource_uri, "ui://dev.neige.hello-world/status");
        assert_eq!(got.structured_content, Some(json!({ "state": "running" })));
    }

    #[test]
    fn extract_card_creation_none_when_meta_missing() {
        let result = CallToolResult::default();
        assert!(extract_card_creation_from_tool_call_result(&result).is_none());
    }

    #[test]
    fn extract_card_creation_none_when_ui_resource_uri_absent() {
        // `_meta` present but no `ui.resourceUri` → not a card-creating tool.
        let result = CallToolResult {
            meta: Some(json!({ "ui": { "permissions": {} } })),
            ..Default::default()
        };
        assert!(extract_card_creation_from_tool_call_result(&result).is_none());
    }

    // ----- unknown method ----------------------------------------------------

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let err = dispatch(&h.ctx(), "neige.nope", json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.code, RpcError::METHOD_NOT_FOUND);
    }
}
