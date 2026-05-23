//! PR8 (#136) — `calm.wait_for_events` MCP tool + shared long-poll
//! helper.
//!
//! ## What it does
//!
//! Spec daemons drive a closed loop: emit a decision via
//! `calm.dispatch_request`, then wait for the corresponding
//! `task.completed` / `task.failed` / `codex.hook` / etc. to land in
//! the wave's event stream. `calm.wait_for_events` is the blocking
//! receive end of that loop.
//!
//! Each call returns at most one batch:
//!
//!   1. **Catch-up**: if there are persisted events for the caller's
//!      wave with `id > since`, return them immediately (no live
//!      subscribe). The caller's `since` defaults to whatever the
//!      kernel's per-card cursor cache holds; an explicit `since`
//!      lets the caller rewind for a manual replay.
//!   2. **Live long-poll**: if catch-up is empty, subscribe to the
//!      bus with a `SubscribeScope::Wave` filter (with
//!      `include_descendants = true` so card-scoped emissions under
//!      that wave route up). Block up to `timeout_ms` (capped at
//!      30s — see issue v2's design note).
//!   3. **Batch window**: once the first matching event lands, drain
//!      any further matches for up to 50ms (or until the overall
//!      timeout expires, whichever fires first) so a "burst" of
//!      events doesn't force the caller into a single-event-per-call
//!      tight loop. 50ms is the eyeballed sweet spot — long enough to
//!      catch the typical "task.completed + codex.hook.stop" pair
//!      that follows one job, short enough that the caller still gets
//!      responsive notification.
//!
//! After the call, the cursor cache is bumped to the highest returned
//! `events.id` so a follow-up call with an omitted `since` picks up
//! exactly where this one stopped.
//!
//! ## Spec-only
//!
//! Workers don't loop on `wait_for_events`. The dispatcher spawns
//! them per job, they read goal/context via `calm.get_wave_state`,
//! call `calm.task_completed` / `calm.task_failed`, and exit. Only the
//! long-lived spec daemon needs the polling primitive — so the soft
//! role gate at the MCP entry refuses non-Spec callers with the same
//! `-32602 spec-only tool` shape PR7b's `update_wave_state` uses.
//!
//! ## Why a shared helper
//!
//! The bridge's Stop-hook handler hits a parallel HTTP route
//! (`/internal/codex/pending_events`) that wants identical semantics:
//! catch-up + live long-poll + 50ms batch + cursor bump. Extracting
//! [`wait_for_events_for_card`] keeps the two paths in lock-step;
//! diverging would mean a spec daemon talking through the MCP path
//! and the bridge talking through the HTTP path could see different
//! event windows for the same card, which would be a debugging
//! nightmare.

use crate::db::RouteRepo;
use crate::event::{BroadcastEnvelope, EventBus, SubscribeFilter, SubscribeScope};
use crate::event_cursor::EventCursorCache;
use crate::ids::{CardId, WaveId};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, CardIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role,
};
use crate::model::CardRole;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

pub const TOOL_WAIT_FOR_EVENTS: &str = "calm.wait_for_events";

/// Hard ceiling on the per-call timeout. Issue v2's design note pins
/// 30s as the bounded long-poll window: long enough for an idle wave
/// to amortize the connect/handshake cost, short enough that a stuck
/// caller surfaces in process tree inspection within a single
/// breath. Callers pass higher values; we silently clamp.
pub const MAX_TIMEOUT_MS: u64 = 30_000;

/// Default timeout when `timeout_ms` is omitted. Same value as the
/// ceiling — the spec system prompt tells the daemon to loop on empty
/// returns, so the default-and-cap convergence is intentional.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Per-batch drain window. The first matching event opens this window;
/// any further matches arriving within it land in the same response
/// frame. Capped at the overall deadline so a generous batch window
/// doesn't bleed past `timeout_ms`. See module doc for the 50ms
/// rationale.
pub const BATCH_WINDOW_MS: u64 = 50;

/// Max events one call ever returns. Pinned to keep the response
/// payload bounded — a sufficiently lagged caller catching up across a
/// large wave should still see paginated batches rather than a single
/// fat frame. The HTTP fallback uses the same value for symmetry.
pub const MAX_EVENTS_PER_CALL: i64 = 100;

/// Register PR8's wait_for_events tool onto a fresh registry. Called
/// from `tools::register_default_tools`.
pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(wait_for_events_descriptor(), wrap(wait_for_events_handler));
}

/// Common wrapper that turns a typed async fn into the boxed-future
/// `ToolHandler` the registry expects. Mirrors `emit::wrap`.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, CardIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn wait_for_events_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAIT_FOR_EVENTS.into(),
        description: "Spec-only long-poll: block up to `timeout_ms` (default 30000, \
             capped at 30000) waiting for events on the caller's wave. \
             `since` defaults to the per-card cursor maintained server-side; \
             pass an explicit integer to rewind. Returns `{events: [...], since: <max_id>}`. \
             An empty `events` array means the timeout expired without any \
             matching event — the spec daemon should immediately re-call \
             this tool to keep polling."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "timeout_ms": { "type": "integer", "minimum": 0 },
                "since": { "type": "integer" }
            }
        }),
    }
}

async fn wait_for_events_handler(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;

    let timeout_ms = parse_timeout_ms(&args)?;
    let explicit_since = parse_since(&args)?;

    // Resolve the wave from the bound card so the SubscribeFilter
    // scopes correctly. A missing card row between handshake and now
    // means a delete-while-active race — surface as InternalError so
    // the operator notices.
    let card = ctx
        .repo
        .card_get(identity.card_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wait_for_events: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wait_for_events: bound card {} not found (deleted mid-connection?)",
                identity.card_id.as_str()
            ))
        })?;
    let wave_id = card.wave_id;

    let (envelopes, max_id) = wait_for_events_for_card(
        ctx.repo.as_ref(),
        &ctx.events,
        &ctx.event_cursor_cache,
        &identity.card_id,
        &wave_id,
        explicit_since,
        timeout_ms,
    )
    .await
    .map_err(|e| RpcError::internal(format!("wait_for_events: {e}")))?;

    Ok(render_response(envelopes, max_id))
}

/// Shared long-poll implementation reachable from both the MCP tool
/// and the HTTP fallback (`routes::codex::pending_events`).
///
/// Parameters:
///   * `repo` — for the catch-up `events_since` query.
///   * `events` — bus for the live subscribe.
///   * `cursor_cache` — per-card cursor map. When `explicit_since` is
///     `None`, the cache's current value (default `0`) is used as the
///     `since` cursor; on a non-empty return, the cursor is bumped to
///     the highest returned id.
///   * `card_id` — used as the cache key.
///   * `wave_id` — scope for the SubscribeFilter / catch-up filter.
///   * `explicit_since` — caller-supplied override (Some(0) is a
///     legitimate "from the beginning" rewind).
///   * `timeout_ms` — caller-supplied timeout, already clamped.
///
/// Returns `(envelopes, Some(max_id))` on a non-empty batch, or
/// `(empty, None)` when the timeout expired without any match.
pub async fn wait_for_events_for_card(
    repo: &dyn RouteRepo,
    events: &EventBus,
    cursor_cache: &EventCursorCache,
    card_id: &CardId,
    wave_id: &WaveId,
    explicit_since: Option<i64>,
    timeout_ms: u64,
) -> anyhow::Result<(Vec<BroadcastEnvelope>, Option<i64>)> {
    // Resolve the effective `since` cursor. Caller's explicit value
    // wins; otherwise read the cache (which returns 0 for no entry).
    let since = explicit_since.unwrap_or_else(|| cursor_cache.get(card_id));

    let filter = SubscribeFilter {
        scope: SubscribeScope::Wave(wave_id.clone()),
        include_descendants: true,
        kinds: None,
    };

    // ----- Phase 1: catch-up. Pull persisted events at id > since
    // that match the wave scope, capped at MAX_EVENTS_PER_CALL.
    //
    // We over-fetch by MAX_EVENTS_PER_CALL since `events_since`
    // doesn't itself filter by scope; the post-filter is a tight in-
    // process loop. On a wave with a low event rate this is fine; if
    // a future wave's traffic is dominated by cross-wave noise, a
    // followup PR can push the filter into the SQL layer.
    let rows = repo.events_since(since, Some(MAX_EVENTS_PER_CALL)).await?;
    let mut catch_up: Vec<BroadcastEnvelope> = rows
        .into_iter()
        .filter(|(_, _, scope, _)| scope.wave_id() == Some(wave_id))
        .map(|(id, event_version, scope, event)| BroadcastEnvelope {
            id,
            event_version,
            // PR8 doesn't have a persisted actor at the replay-row
            // layer (the column exists but `events_since` doesn't
            // hand it back — see db/mod.rs). Stamp `User` as the
            // conservative default; the wire envelope's `actor`
            // field is informational, not authorization-bearing.
            actor: crate::ids::ActorId::User,
            scope,
            event,
        })
        .collect();
    if !catch_up.is_empty() {
        let max_id = catch_up.iter().map(|e| e.id).max();
        if let Some(id) = max_id {
            cursor_cache.bump(card_id.clone(), id);
        }
        // Sort by id ascending so the caller sees the canonical
        // append-only order. `events_since` already returns rows in
        // id-ascending order (see its impl), but defense-in-depth.
        catch_up.sort_by_key(|e| e.id);
        return Ok((catch_up, max_id));
    }

    // ----- Phase 2: live long-poll. Subscribe AFTER the catch-up
    // returned empty; any event that lands while catch-up was running
    // would have been visible to it, so we can't miss it.
    //
    // Subtle: the catch-up's SELECT runs BEFORE the subscribe; an
    // event committed between the two would NOT appear in catch-up
    // and would land on the subscribe (commit-then-broadcast ordering
    // — see `write_with_event` doc). So the window between the two
    // is safe.
    //
    // If timeout_ms == 0 the caller wants a strict catch-up-only
    // call. Short-circuit to avoid burning a subscribe + immediate
    // timeout.
    if timeout_ms == 0 {
        return Ok((Vec::new(), None));
    }

    let mut rx = events.subscribe_filtered();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

    // Wait for the first match (or timeout).
    let first = loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(env)) => {
                if filter.matches(&env) {
                    break Some(env);
                }
                // Filter miss — keep draining without resetting the
                // deadline.
                continue;
            }
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                // We missed `n` envelopes; if any matched our wave
                // scope, the caller will see them on the NEXT call
                // via catch-up (the events table is the source of
                // truth). Log + keep waiting on this call.
                tracing::warn!(
                    skipped = n,
                    "wait_for_events subscriber lagged; missed events will surface via catch-up on next call",
                );
                continue;
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => break None,
            Err(_) => break None, // overall timeout
        }
    };

    let Some(first) = first else {
        // Timeout / closed bus with no match.
        return Ok((Vec::new(), None));
    };

    // Drain the batch window. The batch deadline is the *earlier* of
    // (now + BATCH_WINDOW_MS) and the overall deadline — a generous
    // window doesn't get to bleed past timeout_ms.
    let mut batch = vec![first];
    let batch_deadline = tokio::time::Instant::now() + Duration::from_millis(BATCH_WINDOW_MS);
    let effective_batch_deadline = batch_deadline.min(deadline);

    loop {
        if batch.len() as i64 >= MAX_EVENTS_PER_CALL {
            // Stop draining — let the caller pick up the rest next call.
            break;
        }
        match tokio::time::timeout_at(effective_batch_deadline, rx.recv()).await {
            Ok(Ok(env)) => {
                if filter.matches(&env) {
                    batch.push(env);
                }
                // Otherwise: not for us, but stay in the window.
                continue;
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => break,
            Err(_) => break, // batch window expired
        }
    }

    let max_id = batch.iter().map(|e| e.id).max();
    if let Some(id) = max_id {
        cursor_cache.bump(card_id.clone(), id);
    }
    Ok((batch, max_id))
}

/// Build the on-wire response envelope. `since` mirrors PR2's
/// `EventScope`-on-WS shape: the caller wants both the events and a
/// stable cursor for the next call.
///
/// Each envelope is rendered into the same `{_id, ev, data, eventVersion, scope}`
/// JSON shape the WS handler produces, so a spec daemon can pattern-
/// match on the same fields whether it's reading from the WS stream
/// or polling here. PR2/PR8 share the wire format on purpose — one
/// frame parser handles both.
pub fn render_response(envelopes: Vec<BroadcastEnvelope>, max_id: Option<i64>) -> Value {
    let events_json: Vec<Value> = envelopes.iter().map(render_envelope_json).collect();
    // Echo back the cursor the caller should pass as `since` on the
    // next call. When the batch was empty, `max_id` is `None` and we
    // surface the wire as `null` — the caller can either re-issue
    // with no `since` (cache picks up the previous value) or pass
    // their own previous max id back.
    json!({
        "events": events_json,
        "since": max_id,
    })
}

/// Render one envelope to the wire shape. Mirrors
/// `ws::events::render_envelope`. Lifted out because:
///   * `ws::events::render_envelope` is private to that module;
///   * duplicating five lines is cheaper than restructuring the WS
///     module to export it;
///   * if the wire shape ever diverges between WS frames and
///     wait/pending response frames, we want the divergence to be a
///     local edit, not a cross-module refactor.
fn render_envelope_json(env: &BroadcastEnvelope) -> Value {
    let mut value = serde_json::to_value(&env.event).unwrap_or(Value::Null);
    if let Value::Object(ref mut map) = value {
        map.insert("_id".to_string(), Value::from(env.id));
        map.insert("eventVersion".to_string(), Value::from(env.event_version));
        map.insert(
            "scope".to_string(),
            serde_json::to_value(&env.scope).unwrap_or(Value::Null),
        );
    }
    value
}

fn parse_timeout_ms(args: &Value) -> Result<u64, RpcError> {
    let raw = match args.get("timeout_ms") {
        None | Some(Value::Null) => DEFAULT_TIMEOUT_MS,
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_i64().filter(|v| *v >= 0).map(|v| v as u64))
            .ok_or_else(|| {
                RpcError::invalid_params(
                    "wait_for_events: `timeout_ms` must be a non-negative integer",
                )
            })?,
        Some(_) => {
            return Err(RpcError::invalid_params(
                "wait_for_events: `timeout_ms` must be a non-negative integer",
            ));
        }
    };
    Ok(raw.min(MAX_TIMEOUT_MS))
}

fn parse_since(args: &Value) -> Result<Option<i64>, RpcError> {
    match args.get("since") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let v = n.as_i64().ok_or_else(|| {
                RpcError::invalid_params("wait_for_events: `since` must be an integer (i64)")
            })?;
            if v < 0 {
                return Err(RpcError::invalid_params(
                    "wait_for_events: `since` must be non-negative",
                ));
            }
            Ok(Some(v))
        }
        Some(_) => Err(RpcError::invalid_params(
            "wait_for_events: `since` must be an integer",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_timeout_default_is_max() {
        assert_eq!(parse_timeout_ms(&json!({})).unwrap(), DEFAULT_TIMEOUT_MS);
        assert_eq!(
            parse_timeout_ms(&json!({"timeout_ms": null})).unwrap(),
            DEFAULT_TIMEOUT_MS
        );
    }

    #[test]
    fn parse_timeout_clamps_to_max() {
        // Caller asks for 5 minutes; we cap at 30s.
        assert_eq!(
            parse_timeout_ms(&json!({"timeout_ms": 300_000u64})).unwrap(),
            MAX_TIMEOUT_MS
        );
    }

    #[test]
    fn parse_timeout_accepts_zero_for_strict_catch_up() {
        assert_eq!(parse_timeout_ms(&json!({"timeout_ms": 0})).unwrap(), 0);
    }

    #[test]
    fn parse_timeout_rejects_negative() {
        let err = parse_timeout_ms(&json!({"timeout_ms": -1})).expect_err("must reject negative");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn parse_timeout_rejects_non_number() {
        let err = parse_timeout_ms(&json!({"timeout_ms": "soon"})).expect_err("must reject string");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn parse_since_optional_default_none() {
        assert!(parse_since(&json!({})).unwrap().is_none());
        assert!(parse_since(&json!({"since": null})).unwrap().is_none());
    }

    #[test]
    fn parse_since_zero_is_rewind_to_start() {
        // `Some(0)` is a legitimate "give me everything from the
        // beginning" — distinct from `None` which means "use cache".
        assert_eq!(parse_since(&json!({"since": 0})).unwrap(), Some(0));
    }

    #[test]
    fn parse_since_rejects_negative_and_non_integer() {
        assert!(parse_since(&json!({"since": -1})).is_err());
        assert!(parse_since(&json!({"since": "abc"})).is_err());
    }

    #[test]
    fn render_response_shape_pinned() {
        let v = render_response(Vec::new(), None);
        assert!(v["events"].is_array());
        assert_eq!(v["events"].as_array().unwrap().len(), 0);
        assert!(v["since"].is_null());

        // With a single envelope (synthetic), since echoes the max id.
        use crate::event::{Event, EventScope, SYNC_EVENT_VERSION};
        use crate::ids::{ActorId, CoveId, WaveId};
        let env = BroadcastEnvelope {
            id: 7,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::User,
            scope: EventScope::Wave {
                wave: WaveId::from("w"),
                cove: CoveId::from("c"),
            },
            event: Event::TaskFailed {
                idempotency_key: "k".into(),
                reason: "r".into(),
            },
        };
        let v = render_response(vec![env], Some(7));
        assert_eq!(v["since"], 7);
        let arr = v["events"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["_id"], 7);
        assert_eq!(arr[0]["ev"], "task.failed");
        assert_eq!(arr[0]["data"]["reason"], "r");
        assert_eq!(arr[0]["scope"]["kind"], "Wave");
    }
}
