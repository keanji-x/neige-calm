//! `neige.*` host-callback dispatcher: one plugin-originated request → permission check → repo write → emit event → respond.
//! The plugin's identity is implicit on the connection: the kernel injects `plugin_id` from `CallbackCtx` and never trusts one in the params.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::card_kind::validate_card_kind_global;
#[cfg(test)]
use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    card_create_with_id_tx, card_delete_tx, card_update_tx, overlay_delete_tx, overlay_upsert_tx,
    terminal_delete_tx,
};
use crate::db::{RepoRead, RouteRepo, write_with_actor_events_typed, write_with_event_typed};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, CardId};
use crate::model::{CardPatch, CardRole, NewCard, NewOverlay, new_id};
use crate::operation::workspace_lease::release_workspace_lease_for_card_tx;
use crate::session_projection_lookup::project_runtime_into_card_payload;
use crate::state::WriteContext;
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
#[cfg(test)]
use crate::track_area_cache::TrackAreaCache;
use crate::validation::{
    OVERLAY_ENTITY_SCOPE_REGISTRY, reject_client_supplied_server_owned_keys,
    validate_overlay_payload,
};

use super::events::SubscriptionFilter;
use super::mcp::{CallToolResult, McpClient, RpcError};
use super::registry::PluginRegistry;

/// Subscription ids are monotonic per-process; scoped to one plugin's MCP connection, so no cryptographic uniqueness is needed.
static NEXT_SUB_ID: AtomicU64 = AtomicU64::new(1);

/// Everything `dispatch` needs to service one inbound request.
pub struct CallbackCtx<'a> {
    /// The kernel-enforced plugin identity. NOT taken from request params.
    pub plugin_id: &'a str,
    /// `RouteRepo`, not `Repo`: raw sync-domain writes are unreachable so a plugin's inbound RPC cannot bypass the audit log.
    pub repo: Arc<dyn RouteRepo>,
    pub event_bus: Arc<EventBus>,
    pub registry: Arc<PluginRegistry>,
    /// Outbound MCP channel — used to deliver subscription notifications.
    pub mcp: Arc<McpClient>,
    /// Live subscription join-handles; lives on `PluginHost` so `stop()` can abort them.
    pub subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
    /// Caller-supplied tracing id, set when the dispatch enters from `routes::plugins::plugin_tool_call`; `None` when the plugin's own inbound request triggers the callback.
    pub call_id: Option<&'a str>,
    /// Write-surface caches shared with REST/worker paths.
    pub write: WriteContext,
}

impl<'a> CallbackCtx<'a> {
    /// The call_id as the `events.correlation` string.
    pub(super) fn correlation(&self) -> Option<String> {
        self.call_id.map(|c| format!("user_tool_call:{c}"))
    }

    fn actor(&self) -> ActorId {
        ActorId::Plugin(self.plugin_id.to_string())
    }
}

/// Missing rows or transient read errors collapse to `EventScope::System` rather than failing the dispatch.
async fn overlay_scope_for_callback(
    repo: &dyn RepoRead,
    entity_kind: &str,
    entity_id: &str,
) -> EventScope {
    OVERLAY_ENTITY_SCOPE_REGISTRY
        .route_scope(repo, entity_kind, entity_id)
        .await
        .unwrap_or(EventScope::System)
}

/// Falls back to `EventScope::System` when the track lookup fails so the dispatch doesn't refuse the write on a transient read error.
async fn card_scope_for_callback(repo: &dyn RepoRead, card: CardId, track_id: &str) -> EventScope {
    match repo.track_get(track_id).await {
        Ok(Some(w)) => EventScope::Card {
            card,
            track: w.id,
            area: w.area_id,
        },
        _ => EventScope::System,
    }
}

/// One live subscription; held so the bridge task can be aborted on plugin stop.
pub struct SubscriptionRecord {
    pub plugin_id: String,
    pub task: JoinHandle<()>,
}

/// What a successful `tools/call` response yields when the plugin declared the tool card-creating via `_meta.ui.resourceUri`; `structured_content` is opaque to the kernel and persisted verbatim in `Card.payload`.
#[derive(Debug, Clone)]
pub struct CardCreationFromTool {
    pub resource_uri: String,
    pub structured_content: Option<Value>,
}

/// Pull `_meta.ui.resourceUri` out of a `CallToolResult`; `None` means the plugin didn't signal a card. `is_error` and the URI shape are the caller's business.
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

/// Parse `params` into a per-method struct; surfaces InvalidParams with the serde message so plugins see which field failed.
pub(super) fn parse_params<T: for<'de> Deserialize<'de>>(
    method: &str,
    params: &Value,
) -> Result<T, RpcError> {
    serde_json::from_value::<T>(params.clone())
        .map_err(|e| RpcError::invalid_params(format!("{method}: {e}")))
}

pub(super) fn permission_denied(why: impl Into<String>) -> RpcError {
    RpcError::custom(-32001, why)
}

pub(super) fn entity_not_found(what: impl Into<String>) -> RpcError {
    RpcError::custom(-32004, what)
}

pub(super) fn quota_exceeded(why: impl Into<String>) -> RpcError {
    RpcError::custom(-32003, why)
}

pub(super) fn internal_repo_err(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(format!("repo: {e}"))
}

/// A missing manifest here means the plugin was uninstalled mid-connection; internal error since the supervisor should have stopped the process.
pub(super) fn manifest_permissions(
    ctx: &CallbackCtx<'_>,
) -> Result<super::manifest::Permissions, RpcError> {
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

#[derive(Deserialize)]
struct OverlaySetParams {
    entity_kind: String,
    entity_id: String,
    kind: String,
    payload: Value,
}

async fn overlay_set(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: OverlaySetParams = parse_params("neige.overlay.set", &params)?;
    if !OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable(&p.entity_kind) {
        let kinds = OVERLAY_ENTITY_SCOPE_REGISTRY
            .externally_writable_kinds()
            .join(", ");
        return Err(RpcError::invalid_params(format!(
            "entity_kind must be one of [{kinds}], got `{}`",
            p.entity_kind,
        )));
    }
    let perms = manifest_permissions(ctx)?;
    if !perms.can_overlay_write(&p.entity_kind, &p.kind) {
        return Err(permission_denied(format!(
            "plugin `{}` not granted overlay_write on entity_kind=`{}`",
            ctx.plugin_id, p.entity_kind
        )));
    }
    // Kernel-owned overlay kinds are validated; plugin-defined kinds stay opaque.
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
    let actor = ctx.actor();
    let correlation = ctx.correlation();
    let scope = overlay_scope_for_callback(ctx.repo.as_ref(), &p.entity_kind, &p.entity_id).await;
    let (stored, _id) = write_with_event_typed(
        ctx.repo.as_ref(),
        actor,
        scope,
        correlation.as_deref(),
        ctx.event_bus.as_ref(),
        &ctx.write,
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
    if !OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable(&p.entity_kind) {
        let kinds = OVERLAY_ENTITY_SCOPE_REGISTRY
            .externally_writable_kinds()
            .join(", ");
        return Err(RpcError::invalid_params(format!(
            "entity_kind must be one of [{kinds}], got `{}`",
            p.entity_kind,
        )));
    }
    let perms = manifest_permissions(ctx)?;
    if !perms.can_overlay_write(&p.entity_kind, &p.kind) {
        return Err(permission_denied(format!(
            "plugin `{}` not granted overlay_write on entity_kind=`{}`",
            ctx.plugin_id, p.entity_kind
        )));
    }
    // Scoped strictly to this plugin's overlays via the server-known plugin_id.
    let actor = ctx.actor();
    let correlation = ctx.correlation();
    let plugin_id_owned = ctx.plugin_id.to_string();
    let entity_kind = p.entity_kind.clone();
    let entity_id = p.entity_id.clone();
    let kind = p.kind.clone();
    let scope = overlay_scope_for_callback(ctx.repo.as_ref(), &entity_kind, &entity_id).await;
    let result = write_with_event_typed(
        ctx.repo.as_ref(),
        actor,
        scope,
        correlation.as_deref(),
        ctx.event_bus.as_ref(),
        &ctx.write,
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
        // A missing overlay is idempotent success; plugins reissuing delete during reconnect shouldn't fail their event loop.
        Err(crate::error::CalmError::NotFound(_)) => Ok(json!({ "deleted": false })),
        Err(e) => Err(internal_repo_err(e)),
    }
}

#[derive(Deserialize)]
struct CardCreateParams {
    track_id: String,
    kind: String,
    #[serde(default)]
    payload: Value,
    #[serde(default)]
    sort: Option<f64>,
    #[serde(default)]
    title: Option<String>,
}

#[allow(deprecated)]
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
    // Server-owned payload keys are kernel-stamped; a plugin never writes them (any kind).
    reject_client_supplied_server_owned_keys(&payload)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    // Kernel-owned card kinds must match shape; plugin-prefixed and ui:// kinds remain opaque.
    validate_card_kind_global(&p.kind, &payload)
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let track_id_for_scope = p.track_id.clone();
    let new = NewCard {
        track_id: p.track_id.into(),
        kind: p.kind,
        sort: p.sort,
        payload,
        title: p.title,
    };
    let actor = ctx.actor();
    let correlation = ctx.correlation();
    // Pre-mint the card id so the audit row's `EventScope::Card` is determinable before the txn opens.
    let card_id = CardId::from(new_id());
    let scope =
        card_scope_for_callback(ctx.repo.as_ref(), card_id.clone(), &track_id_for_scope).await;
    let card_id_for_tx = card_id.0.clone();
    let write_for_tx = ctx.write.clone();
    let (mut stored, _id) = write_with_event_typed(
        ctx.repo.as_ref(),
        actor,
        scope,
        correlation.as_deref(),
        ctx.event_bus.as_ref(),
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                // Plugin-driven creates are user-deletable; only internal kernel paths mint kernel-owned cards.
                let stored = card_create_with_id_tx(
                    tx,
                    card_id_for_tx,
                    new,
                    CardRole::Worker,
                    true,
                    write_for_tx.role_cache(),
                )
                .await?;
                Ok((stored.clone(), Event::CardAdded(stored)))
            })
        },
    )
    .await
    .map_err(|e| match e {
        crate::error::CalmError::NotFound(s) => entity_not_found(s),
        other => internal_repo_err(other),
    })?;
    project_runtime_into_card_payload(ctx.repo.as_ref(), &mut stored)
        .await
        .map_err(internal_repo_err)?;
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
    #[serde(default)]
    title: Option<String>,
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
    // A `kind` change also requires can_card_create on the new kind so patching can't bypass create-permissions.
    if let Some(new_kind) = &p.kind
        && !perms.can_card_create(new_kind, ctx.plugin_id)
    {
        return Err(permission_denied(format!(
            "plugin `{}` cannot retarget card to kind `{}`",
            ctx.plugin_id, new_kind,
        )));
    }
    if let Some(payload) = p.payload.as_ref() {
        // `card_update_tx` keeps every stored server-owned key sticky across the replacement.
        reject_client_supplied_server_owned_keys(payload)
            .map_err(|e| RpcError::invalid_params(e.to_string()))?;
        let kind = p.kind.as_deref().unwrap_or(card.kind.as_str());
        validate_card_kind_global(kind, payload)
            .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    }
    let patch = CardPatch {
        title: p.title,
        kind: p.kind,
        sort: p.sort,
        payload: p.payload,
        // Plugins cannot patch `deletable`; the route-level 400 in `routes::cards::update_card` is the canonical guard.
        deletable: None,
    };
    let actor = ctx.actor();
    let correlation = ctx.correlation();
    let card_id = p.card_id.clone();
    let scope =
        card_scope_for_callback(ctx.repo.as_ref(), card.id.clone(), card.track_id.as_str()).await;
    let (mut updated, _id) = write_with_event_typed(
        ctx.repo.as_ref(),
        actor,
        scope,
        correlation.as_deref(),
        ctx.event_bus.as_ref(),
        &ctx.write,
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
    project_runtime_into_card_payload(ctx.repo.as_ref(), &mut updated)
        .await
        .map_err(internal_repo_err)?;
    serde_json::to_value(&updated).map_err(|e| RpcError::internal(format!("serde: {e}")))
}

#[derive(Deserialize)]
struct CardDeleteParams {
    card_id: String,
}

#[allow(deprecated)]
async fn card_delete(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: CardDeleteParams = parse_params("neige.card.delete", &params)?;
    let card = ctx
        .repo
        .card_get(&p.card_id)
        .await
        .map_err(internal_repo_err)?
        .ok_or_else(|| entity_not_found(format!("card {}", p.card_id)))?;
    // Kernel-owned card guard runs before the permission check so the policy is greppable at every delete entry.
    if !card.deletable {
        return Err(permission_denied(format!(
            "card `{}` is kernel-owned and cannot be deleted via plugin callback",
            p.card_id
        )));
    }
    let perms = manifest_permissions(ctx)?;
    if !perms.can_card_delete(&card.kind, ctx.plugin_id) {
        return Err(permission_denied(format!(
            "plugin `{}` cannot delete card `{}` (kind `{}` not owned by this plugin)",
            ctx.plugin_id, p.card_id, card.kind,
        )));
    }
    let track_id = card.track_id.clone();
    let card_id = p.card_id.clone();
    let actor = ctx.actor();
    let correlation = ctx.correlation();
    let scope =
        card_scope_for_callback(ctx.repo.as_ref(), card.id.clone(), track_id.as_str()).await;
    let write_for_tx = ctx.write.clone();

    // Eager teardown: `terminals.card_id` is `ON DELETE RESTRICT`, so the terminal is reaped and its row dropped in the same txn as the card, unconditionally.
    let term = ctx
        .repo
        .terminal_get_by_card(card_id.as_str())
        .await
        .map_err(internal_repo_err)?;
    if let Some(t) = term.as_ref() {
        reap_terminal_artifacts_with_renderer(None, t).await;
    }
    let terminal_id = term.map(|t| t.id);

    let _ = write_with_actor_events_typed(
        ctx.repo.as_ref(),
        correlation.as_deref(),
        ctx.event_bus.as_ref(),
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                if let Some(tid) = terminal_id.as_deref() {
                    match terminal_delete_tx(tx, tid)
                        .await
                        .map_err(crate::error::CalmError::from)
                    {
                        Ok(()) => {}
                        Err(crate::error::CalmError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                let mut events = release_workspace_lease_for_card_tx(tx, &card_id).await?;
                card_delete_tx(tx, &card_id, write_for_tx.role_cache()).await?;
                events.push((
                    actor,
                    scope,
                    Event::CardDeleted {
                        id: card_id.into(),
                        track_id,
                    },
                ));
                Ok(((), events))
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

#[derive(Deserialize)]
struct EventSubscribeParams {
    #[serde(default)]
    filter: SubscriptionFilter,
}

async fn event_subscribe(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: EventSubscribeParams = parse_params("neige.event.subscribe", &params)?;
    let perms = manifest_permissions(ctx)?;
    // One permission check per glob; an empty `events` list means "match everything" and needs the firehose grant.
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

    // Bridge task: the McpClient's outbound channel is bounded and drops if backed up, so a slow plugin can't stall the bus.
    let task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => {
                    let ev = env.event;
                    if !filter.matches(&ev) {
                        continue;
                    }
                    // Mirrors the WS wire shape: `_id` is the persisted events.id, usable as a cursor / dedupe key.
                    let mut body = serde_json::Map::new();
                    body.insert("subscription_id".into(), json!(sub_id));
                    body.insert("_id".into(), json!(env.id));
                    body.insert(
                        "event".into(),
                        serde_json::to_value(&ev).unwrap_or(serde_json::Value::Null),
                    );
                    let body = serde_json::Value::Object(body);
                    // notify returns Err on transport-closed; bail so we don't spin until plugin stop.
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
        task,
    });

    Ok(json!({ "subscription_id": sub_id }))
}

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

    // Quota: existing keyset bytes plus the proposed value minus the old value's bytes, using serde_json's textual length as the proxy.
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

#[cfg(test)]
mod tests {
    use super::*;
    // Tests seed fixtures via raw sync-domain writes, so the harness keeps a full `Arc<dyn Repo>`; `ctx()` upcasts to `RouteRepo`.
    use crate::db::Repo;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::model::{NewArea, NewCard, NewPlugin, NewTrack};
    use crate::plugin_host::InitializeMeta;
    use crate::plugin_host::manifest::Manifest;
    use crate::plugin_host::mcp::McpClient;
    use crate::plugin_host::registry::PluginRegistry;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::Mutex;

    /// Test scaffold with a seeded area + track; the McpClient is real, wired to a stub plugin that answers `initialize` then drains.
    struct Harness {
        ctx_storage: Arc<HarnessStorage>,
        track_id: String,
    }

    struct HarnessStorage {
        plugin_id: String,
        repo: Arc<dyn Repo>,
        /// Concrete handle so tests can reach the sqlx pool for transactional helpers; production never reaches the concrete type.
        sqlx_repo: Arc<SqlxRepo>,
        event_bus: Arc<EventBus>,
        registry: Arc<PluginRegistry>,
        mcp: Arc<McpClient>,
        subs: Arc<Mutex<Vec<SubscriptionRecord>>>,
        write: WriteContext,
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
                "overlays_write": ["track", "card"],
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

    /// The stub answers `initialize` and silently drops everything else.
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

        McpClient::connect_with_auth(
            k_r,
            k_w,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .expect("stub connect")
    }

    impl Harness {
        async fn new(plugin_id: &str, manifest: Manifest) -> Self {
            let sqlx_repo = Arc::new(
                SqlxRepo::open("sqlite::memory:")
                    .await
                    .expect("open in-memory sqlite repo"),
            );
            let repo: Arc<dyn Repo> = sqlx_repo.clone();
            // Seed a plugin row so kv writes pass the FK check.
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
            let area = repo
                .area_create(NewArea {
                    name: "test".into(),
                    color: "#fff".into(),
                    sort: None,
                })
                .await
                .unwrap();
            let track = repo
                .track_create(NewTrack {
                    template_input: None,
                    area_id: area.id.clone(),
                    title: "w".into(),
                    sort: None,
                    cwd: String::new(),
                    template_id: None,
                    plugin_scope: None,
                    attach_folder: false,
                    theme: crate::routes::theme::RequestTheme::default_dark(),
                })
                .await
                .unwrap();

            let event_bus = Arc::new(EventBus::new());
            let registry = Arc::new(PluginRegistry::from_manifests([(manifest, None)]));
            let mcp = stub_mcp_client().await;
            let subs = Arc::new(Mutex::new(Vec::new()));

            // Seed the role cache so card_create dispatch tests see the roles for the cards they create.
            let card_role_cache = CardRoleCache::new();
            repo.seed_card_role_cache(&card_role_cache)
                .await
                .expect("seed role cache");
            let track_area_cache = TrackAreaCache::new();
            repo.seed_track_area_cache(&track_area_cache)
                .await
                .expect("seed track-area cache");

            Self {
                ctx_storage: Arc::new(HarnessStorage {
                    plugin_id: plugin_id.to_string(),
                    repo,
                    sqlx_repo,
                    event_bus,
                    registry,
                    mcp,
                    subs,
                    write: WriteContext::new(card_role_cache, track_area_cache),
                }),
                track_id: track.id.to_string(),
            }
        }

        fn ctx(&self) -> CallbackCtx<'_> {
            // The explicit `Arc<dyn RouteRepo>` binding drives trait-object upcasting; `Arc::clone` alone wouldn't coerce.
            let route_repo: Arc<dyn RouteRepo> = self.ctx_storage.repo.clone();
            CallbackCtx {
                plugin_id: &self.ctx_storage.plugin_id,
                repo: route_repo,
                event_bus: Arc::clone(&self.ctx_storage.event_bus),
                registry: Arc::clone(&self.ctx_storage.registry),
                mcp: Arc::clone(&self.ctx_storage.mcp),
                subscriptions: Arc::clone(&self.ctx_storage.subs),
                call_id: None,
                write: self.ctx_storage.write.clone(),
            }
        }
    }

    #[tokio::test]
    async fn overlay_set_writes_with_server_plugin_id() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let res = dispatch(
            &h.ctx(),
            "neige.overlay.set",
            json!({
                // Plugin tries to lie about plugin_id — server must ignore it.
                "plugin_id": "evil",
                "entity_kind": "track",
                "entity_id": h.track_id,
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
            .overlays_for("track", &h.track_id)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].plugin_id, "p1", "plugin_id is server-enforced");
        assert_eq!(stored[0].kind, "status");
    }

    #[tokio::test]
    async fn overlay_set_accepts_externally_writable_entity_kinds() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let card = h
            .ctx_storage
            .repo
            .card_create(NewCard {
                track_id: h.track_id.clone().into(),
                title: None,
                kind: "terminal".into(),
                sort: None,
                payload: json!({}),
            })
            .await
            .unwrap();

        for (entity_kind, entity_id) in [("track", h.track_id.as_str()), ("card", card.id.as_str())]
        {
            let res = dispatch(
                &h.ctx(),
                "neige.overlay.set",
                json!({
                    "entity_kind": entity_kind,
                    "entity_id": entity_id,
                    "kind": "status",
                    "payload": { "state": "running" }
                }),
            )
            .await
            .expect("set");
            assert!(res["overlay_id"].is_string());
        }
    }

    #[tokio::test]
    async fn overlay_set_denied_without_permission() {
        let h = Harness::new("p1", manifest_no_perms("p1")).await;
        let err = dispatch(
            &h.ctx(),
            "neige.overlay.set",
            json!({
                "entity_kind": "track",
                "entity_id": h.track_id,
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
                "entity_kind": "track",
                "entity_id": h.track_id,
                "kind": "status",
                "payload": { "state": "running" }
            }),
        )
        .await
        .unwrap();
        let del = dispatch(
            &h.ctx(),
            "neige.overlay.delete",
            json!({
                "entity_kind": "track",
                "entity_id": h.track_id,
                "kind": "status",
            }),
        )
        .await
        .unwrap();
        assert_eq!(del["deleted"], true);

        let stored = h
            .ctx_storage
            .repo
            .overlays_for("track", &h.track_id)
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
                "entity_kind": "area",
                "entity_id": "x",
                "kind": "status",
                "payload": {}
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn overlay_set_rejects_kernel_reserved_entity_kinds() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        for entity_kind in ["view", "system"] {
            let err = dispatch(
                &h.ctx(),
                "neige.overlay.set",
                json!({
                    "entity_kind": entity_kind,
                    "entity_id": "x",
                    "kind": "status",
                    "payload": { "state": "running" }
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, RpcError::INVALID_PARAMS);
            assert_eq!(
                err.message,
                format!("entity_kind must be one of [card, track], got `{entity_kind}`")
            );
        }
    }

    #[tokio::test]
    async fn card_create_with_own_prefix() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let res = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({
                "track_id": h.track_id,
                "kind": "plugin:p1:demo",
                "payload": { "x": 1 }
            }),
        )
        .await
        .expect("create");
        assert_eq!(res["kind"], "plugin:p1:demo");
        let cards = h
            .ctx_storage
            .repo
            .cards_by_track(&h.track_id)
            .await
            .unwrap();
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
                "track_id": h.track_id,
                "kind": "terminal"
            }),
        )
        .await
        .expect("terminal card create allowed");
        assert_eq!(res["kind"], "terminal");
    }

    #[tokio::test]
    async fn card_create_and_update_reject_client_server_owned_keys() {
        use crate::validation::SERVER_OWNED_CARD_PAYLOAD_KEYS;
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        assert_eq!(
            SERVER_OWNED_CARD_PAYLOAD_KEYS,
            [
                "terminal_signals",
                "claude_permissions",
                "claude_permissions_source",
                "template_context"
            ]
        );
        let probes = [json!(true), json!({}), json!("declared"), Value::Null];
        for key in SERVER_OWNED_CARD_PAYLOAD_KEYS {
            for kind in ["terminal", "plugin:p1:demo"] {
                for value in &probes {
                    let mut payload = json!({ "schemaVersion": 1 });
                    payload[key] = value.clone();
                    let err = dispatch(
                        &h.ctx(),
                        "neige.card.create",
                        json!({
                            "track_id": h.track_id,
                            "kind": kind,
                            "payload": payload
                        }),
                    )
                    .await
                    .unwrap_err();
                    assert_eq!(err.code, RpcError::INVALID_PARAMS, "kind={kind} key={key}");
                    assert!(
                        err.message.contains(key) && err.message.contains("server-owned"),
                        "kind={kind} key={key}: {}",
                        err.message
                    );
                }
            }
        }
        assert!(
            h.ctx_storage
                .repo
                .cards_by_track(&h.track_id)
                .await
                .unwrap()
                .is_empty(),
            "nothing was written"
        );

        let create = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({ "track_id": h.track_id, "kind": "plugin:p1:demo" }),
        )
        .await
        .unwrap();
        let cid = create["id"].as_str().unwrap().to_string();
        for key in SERVER_OWNED_CARD_PAYLOAD_KEYS {
            for value in &probes {
                let mut payload = json!({});
                payload[key] = value.clone();
                let err = dispatch(
                    &h.ctx(),
                    "neige.card.update",
                    json!({ "card_id": cid, "payload": payload }),
                )
                .await
                .unwrap_err();
                assert_eq!(err.code, RpcError::INVALID_PARAMS, "key={key}");
                assert!(
                    err.message.contains(key) && err.message.contains("server-owned"),
                    "key={key}: {}",
                    err.message
                );
            }
        }
        let stored = h.ctx_storage.repo.card_get(&cid).await.unwrap().unwrap();
        for key in SERVER_OWNED_CARD_PAYLOAD_KEYS {
            assert!(
                stored.payload.get(key).is_none(),
                "{key}: {}",
                stored.payload
            );
        }
    }

    #[tokio::test]
    async fn card_create_denies_other_plugin_prefix() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let err = dispatch(
            &h.ctx(),
            "neige.card.create",
            json!({
                "track_id": h.track_id,
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
                track_id: h.track_id.clone().into(),
                title: None,
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
            json!({ "track_id": h.track_id, "kind": "plugin:p1:demo" }),
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
            json!({ "track_id": h.track_id, "kind": "plugin:p1:demo" }),
        )
        .await
        .unwrap();
        let cid = create["id"].as_str().unwrap().to_string();
        let res = dispatch(&h.ctx(), "neige.card.delete", json!({ "card_id": cid }))
            .await
            .unwrap();
        assert_eq!(res, json!({}));
        let cards = h
            .ctx_storage
            .repo
            .cards_by_track(&h.track_id)
            .await
            .unwrap();
        assert!(cards.is_empty());
    }

    /// The undeletable card is minted via `card_create_with_id_tx` with a plugin-owned kind, so the kind check would let the plugin through and only the `deletable` guard refuses.
    #[tokio::test]
    #[allow(deprecated)]
    async fn card_delete_refused_for_undeletable_card() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let cache = h.ctx_storage.write.role_cache().clone();
        let mut tx = h.ctx_storage.sqlx_repo.pool().begin().await.unwrap();
        let undeletable = crate::db::sqlite::card_create_with_id_tx(
            &mut tx,
            crate::model::new_id(),
            NewCard {
                track_id: h.track_id.clone().into(),
                title: None,
                kind: "plugin:p1:demo".into(),
                sort: None,
                payload: json!({}),
            },
            CardRole::Worker,
            false, // ← kernel-owned bit
            &cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let cid = undeletable.id.as_str().to_string();

        let err = dispatch(&h.ctx(), "neige.card.delete", json!({ "card_id": cid }))
            .await
            .expect_err("undeletable card must refuse plugin-callback delete");
        // Asserting on the code (-32001, permission_denied) rather than the message keeps the test resistant to wording tweaks.
        assert_eq!(
            err.code, -32001,
            "expected permission_denied (-32001); got: {err:?}",
        );

        let still_there = h.ctx_storage.repo.card_get(&cid).await.unwrap();
        assert!(still_there.is_some(), "undeletable card survives refusal");
    }

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
        dispatch(
            &h.ctx(),
            "neige.kv.set",
            json!({ "key": "k", "value": "small" }),
        )
        .await
        .unwrap();
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
        let result = CallToolResult {
            meta: Some(json!({ "ui": { "permissions": {} } })),
            ..Default::default()
        };
        assert!(extract_card_creation_from_tool_call_result(&result).is_none());
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let h = Harness::new("p1", manifest_with_full_perms("p1")).await;
        let err = dispatch(&h.ctx(), "neige.nope", json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.code, RpcError::METHOD_NOT_FOUND);
    }
}
