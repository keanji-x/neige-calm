//! PR7b (#136) wave-state tools — `calm.get_wave_state`,
//! `calm.update_wave_state`, `calm.update_task_meta`.
//!
//! These tools complete the spec-card closed loop: a spec daemon reads
//! the current wave snapshot, mutates wave-level metadata (title /
//! sort / archive), and marks individual worker results as accepted /
//! rejected during validation. The dispatcher then closes the loop by
//! pushing the next worker-emitted event onto the spec's thread as a
//! turn input (#293 — no polling).
//!
//! ## Tool surface
//!
//! * `calm.get_wave_state` — Spec **or** Worker callable. Returns the
//!   bound card's wave row + the wave's card list (id/kind/role) as
//!   one JSON snapshot. No event emission, no role gate at the MCP
//!   entry. Workers occasionally peek wave state before they report;
//!   the spec gets a full snapshot every loop iteration.
//!
//! * `calm.update_wave_state` — Spec only. Patches the wave row
//!   (`title` / `sort` / `archived_at`), stamps `updated_at`, and
//!   emits `Event::WaveUpdated(full_wave)` via
//!   `write_with_event_typed`. The MCP entry's soft role gate refuses
//!   non-Spec callers with `-32602 spec-only tool`; the real boundary
//!   is the in-tx role gate (`enforce_role`), which already pins
//!   `WaveUpdated` to spec / user / kernel actors. Re-checking at the
//!   MCP entry just gives the caller a cleaner error shape.
//!
//! * `calm.update_task_meta` — Spec only. Records the spec's
//!   accept/reject verdict on a worker's prior result. Lowers to
//!   either `Event::TaskCompleted` (verdict = "accepted") or
//!   `Event::TaskFailed` (verdict = "rejected"); the `idempotency_key`
//!   echoes the original `*.job_requested` so consumers can correlate.
//!
//!   ### Variant choice (TaskCompleted/TaskFailed reuse vs. new variant)
//!
//!   The earliest-stage design considered adding
//!   `Event::TaskMetaUpdated { idempotency_key, metadata: Value }` as
//!   an explicit metadata channel. We picked the reuse path because:
//!     * the only PR7b use case is the spec's accept/reject verdict on
//!       a completed worker run — perfectly captured by the existing
//!       success/failure semantics;
//!     * the spec's verdict *is* a terminal outcome from the spec's
//!       point of view, mirroring how the worker would report its own
//!       outcome — a single kind for "this idempotency_key is done"
//!       keeps consumer code (and the dispatcher's correlator)
//!       simpler;
//!     * a future PR that needs richer task metadata (per-iteration
//!       checkpoints, partial progress, structured artifacts) can add
//!       the dedicated variant then without rewriting today's
//!       call sites — the MCP tool name stays stable while the wire
//!       event shape evolves under it.
//!
//!   The verdict + optional reason are folded into the
//!   `TaskCompleted.result` JSON (`{status, reason}`) so the audit log
//!   carries the spec's rationale verbatim.
//!
//! ## Scope construction
//!
//! Unlike PR7a's emit tools (which scope to the caller's *card*), the
//! wave-state tools scope to the caller's *wave*. The wave is where
//! the spec card's authority lives — emitting `WaveUpdated` under
//! `EventScope::Card` would be a category error (the role gate
//! permits it but the topic routing would miss wave-level
//! subscribers). For `update_task_meta` we also use `EventScope::Wave`
//! since the verdict is wave-level metadata about a worker the spec
//! supervises, not the spec's own card state.

use crate::db::{write_with_event_typed, write_with_events_typed};
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::WaveId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, CardIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role,
};
use crate::model::{CardRole, Wave, WaveLifecycle, WavePatch};
use crate::wave_lifecycle::validate_transition;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_GET_WAVE_STATE: &str = "calm.get_wave_state";
pub const TOOL_UPDATE_WAVE_STATE: &str = "calm.update_wave_state";
pub const TOOL_UPDATE_TASK_META: &str = "calm.update_task_meta";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(get_wave_state_descriptor(), wrap(get_wave_state));
    registry.register(update_wave_state_descriptor(), wrap(update_wave_state));
    registry.register(update_task_meta_descriptor(), wrap(update_task_meta));
}

/// Common wrapper that turns a typed async fn into the boxed-future
/// `ToolHandler` the registry expects. Mirrors `emit::wrap`.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, CardIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(
        move |ctx, identity, _request_meta, args| -> ToolHandlerFuture {
            Box::pin(f(ctx, identity, args))
        },
    )
}

// ---------------------------------------------------------------------------
// calm.get_wave_state
// ---------------------------------------------------------------------------

fn get_wave_state_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_GET_WAVE_STATE.into(),
        description: "Read the current wave snapshot bound to the calling card. \
             Returns the wave row plus a card list (`id`, `kind`, `role`) so \
             a spec daemon can see worker progress without a second call. \
             Callable by spec and worker cards alike; no event is emitted."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

async fn get_wave_state(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let cards = ctx
        .repo
        .cards_by_wave(wave.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("get_wave_state: cards_by_wave: {e}")))?;

    // We re-query the role cache rather than fetching `cards.role` on
    // the card row — the cache is the canonical source the role gate
    // already trusts, and `Card` doesn't carry `role` on the struct
    // (it's a column the cache mirrors). One cache hit per card; the
    // cache is in-process and lock-free for reads.
    let cards_json: Vec<Value> = cards
        .iter()
        .map(|c| {
            let role = ctx.card_role_cache.get(&c.id).unwrap_or_default();
            json!({
                "id": c.id,
                "kind": c.kind,
                "role": role,
                "sort": c.sort,
                "created_at": c.created_at,
                "updated_at": c.updated_at,
            })
        })
        .collect();

    Ok(json!({
        "wave": wave,
        "cards": cards_json,
    }))
}

// ---------------------------------------------------------------------------
// calm.update_wave_state
// ---------------------------------------------------------------------------

fn update_wave_state_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_UPDATE_WAVE_STATE.into(),
        description: "Spec-only: patch the wave row (`title` / `sort` / \
             `archived_at` / `lifecycle`) and emit `wave.updated` (plus \
             `wave.lifecycle_changed` when `lifecycle` is set and the \
             requested transition actually changes state). Omitted \
             fields are left unchanged. `archived_at = null` \
             unarchives; a positive ms timestamp archives. `lifecycle` \
             accepts the typed state names — see issue #145 / \
             `wave_lifecycle.rs` for the allowed `from → to` table. \
             A same-state lifecycle request (e.g. setting `lifecycle` \
             to the wave's current state) is an idempotent silent \
             success: no `wave.lifecycle_changed` event is emitted, \
             and if `lifecycle` was the only field supplied the row is \
             not rewritten. An illegal transition is rejected with \
             `-32403`; the wave row and event log are both untouched. \
             Returns the post-patch wave."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "sort": { "type": "number" },
                "archived_at": { "type": ["integer", "null"] },
                "lifecycle": {
                    "type": "string",
                    "enum": [
                        "draft", "planning", "dispatching", "working",
                        "blocked", "reviewing", "done", "canceled", "failed"
                    ],
                    "description": "Request a Wave lifecycle transition. The kernel \
                         validates (from, to, actor=spec) via wave_lifecycle::validate_transition."
                }
            }
        }),
    }
}

async fn update_wave_state(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;

    let patch = parse_wave_patch(&args)?;
    let (_, current) = resolve_wave_for_identity(&ctx, &identity).await?;

    let wave_id = current.id.clone();
    let cove_id = current.cove_id.clone();
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let actor = identity.to_actor_id();

    // Issue #145 — lifecycle transitions go through the state machine
    // *before* the row update. The MCP entry mirrors the REST handler
    // in `routes::waves::update_wave`: validate (from → to, actor),
    // surface `-32403` on rejection so the spec agent sees a clear
    // shape, otherwise emit `WaveLifecycleChanged` alongside the
    // usual `WaveUpdated` in one transaction.
    //
    // Idempotent same-state: when `patch.lifecycle == Some(current)`
    // (e.g. the spec retried `update_wave_state(lifecycle="planning")`
    // while already planning), the validator returns `Ok(())` and we
    // strip `lifecycle` from the patch so neither
    // `WaveLifecycleChanged` nor a pointless column rewrite happens.
    // If `lifecycle` was the *only* supplied field, the resulting
    // patch is fully empty and we return the wave row without
    // touching the DB — distinct from an explicit `{}` ping (which
    // still bumps `updated_at` and re-broadcasts as before).
    let lifecycle_was_only_field = patch.lifecycle.is_some()
        && patch.title.is_none()
        && patch.sort.is_none()
        && patch.archived_at.is_none();
    let mut patch = patch;
    let lifecycle_change = if let Some(to) = patch.lifecycle {
        validate_transition(current.lifecycle, to, &actor)
            .map_err(|e| RpcError::custom(-32403, format!("update_wave_state: lifecycle: {e}")))?;
        if current.lifecycle == to {
            patch.lifecycle = None;
            None
        } else {
            Some((current.lifecycle, to))
        }
    } else {
        None
    };

    // Idempotent shortcut: lifecycle was the sole patch field and it
    // turned out to be a no-op, so there's nothing to write and
    // nothing to emit. Return the unchanged wave row. The `{}`
    // ping path is unaffected (`lifecycle_was_only_field` is false
    // when `patch.lifecycle` is None).
    if lifecycle_was_only_field && lifecycle_change.is_none() {
        return Ok(json!({ "ok": true, "wave": current }));
    }

    let wave_id_for_event = wave_id.clone();
    let cove_id_for_event = cove_id.clone();
    let scope_for_tx = scope.clone();
    let patch_for_tx = patch.clone();
    let ((updated_wave, _emitted_lifecycle), _ids) =
        write_with_events_typed::<(Wave, Option<(WaveLifecycle, WaveLifecycle)>), _>(
            ctx.repo.as_ref(),
            actor,
            None,
            &ctx.events,
            &ctx.card_role_cache,
            &ctx.wave_cove_cache,
            move |tx| {
                let scope_inner = scope_for_tx.clone();
                let wave_id_inner = wave_id.clone();
                let patch_inner = patch_for_tx.clone();
                let wave_id_event = wave_id_for_event.clone();
                let cove_id_event = cove_id_for_event.clone();
                Box::pin(async move {
                    let updated = apply_wave_patch_tx(tx, &wave_id_inner, patch_inner).await?;
                    let mut events: Vec<(EventScope, Event)> = Vec::new();
                    if let Some((from, to)) = lifecycle_change {
                        events.push((
                            scope_inner.clone(),
                            Event::WaveLifecycleChanged {
                                id: wave_id_event,
                                cove_id: cove_id_event,
                                from,
                                to,
                            },
                        ));
                    }
                    events.push((scope_inner, Event::WaveUpdated(updated.clone())));
                    Ok(((updated, lifecycle_change), events))
                })
            },
        )
        .await
        .map_err(map_emit_error)?;

    Ok(json!({ "ok": true, "wave": updated_wave }))
}

/// Apply the patch to the wave row inside the supplied transaction.
/// Mirrors `db::sqlite::wave_update_tx` but accepts the patch shape
/// PR7b emits (all fields direct-optional, no double-Option dance —
/// MCP callers pass `archived_at: null` to unarchive and an integer
/// to archive, which we coerce into the existing `WavePatch` shape
/// on the way in via `parse_wave_patch`).
async fn apply_wave_patch_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &WaveId,
    patch: WavePatch,
) -> Result<Wave, CalmError> {
    crate::db::sqlite::wave_update_tx(tx, id.as_str(), patch).await
}

/// Translate the on-the-wire `args` JSON into the kernel's `WavePatch`.
/// The MCP shape is simpler than the HTTP `PATCH /waves/:id` body:
/// `archived_at: null` means unarchive; omitting the key means leave
/// alone. We convert that to the kernel's double-Option encoding so
/// the existing `wave_update_tx` doesn't need a parallel code path.
fn parse_wave_patch(args: &Value) -> Result<WavePatch, RpcError> {
    let obj = args.as_object().ok_or_else(|| {
        RpcError::invalid_params("update_wave_state: arguments must be an object")
    })?;

    let title = match obj.get("title") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(other) => {
            return Err(RpcError::invalid_params(format!(
                "update_wave_state: `title` must be a string, got {}",
                shape_of(other)
            )));
        }
    };

    let sort = match obj.get("sort") {
        Some(Value::Number(n)) => Some(n.as_f64().ok_or_else(|| {
            RpcError::invalid_params("update_wave_state: `sort` not representable as f64")
        })?),
        Some(Value::Null) | None => None,
        Some(other) => {
            return Err(RpcError::invalid_params(format!(
                "update_wave_state: `sort` must be a number, got {}",
                shape_of(other)
            )));
        }
    };

    // Distinguish "omitted" (None) from "null" (Some(None)) from
    // "integer" (Some(Some(n))). The `Value::is_null()` check is the
    // critical bit — `obj.get("archived_at")` returns
    // `Some(Value::Null)` for an explicit `archived_at: null`.
    let archived_at = match obj.get("archived_at") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Number(n)) => Some(Some(n.as_i64().ok_or_else(|| {
            RpcError::invalid_params("update_wave_state: `archived_at` not representable as i64")
        })?)),
        Some(other) => {
            return Err(RpcError::invalid_params(format!(
                "update_wave_state: `archived_at` must be integer or null, got {}",
                shape_of(other)
            )));
        }
    };

    // Issue #145 — optional lifecycle transition. Accepts only the
    // typed state names; anything else is rejected at parse time so
    // an LLM typo never reaches `validate_transition`. The actual
    // (from, to, actor) check runs inside `update_wave_state` once
    // we know the current state of the wave.
    let lifecycle = match obj.get("lifecycle") {
        None => None,
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(parse_lifecycle_name(s)?),
        Some(other) => {
            return Err(RpcError::invalid_params(format!(
                "update_wave_state: `lifecycle` must be a string, got {}",
                shape_of(other)
            )));
        }
    };

    // An empty patch is a valid no-op call — the spec might "ping"
    // the wave by passing `{}` to refresh `updated_at` + re-broadcast
    // the row. `wave_update_tx` stamps `updated_at = now_ms()`
    // unconditionally on every call, so the no-op still bumps the
    // freshness clock.
    Ok(WavePatch {
        title,
        sort,
        archived_at,
        pinned_at: None,
        lifecycle,
    })
}

/// Parse the wire-side lowercase lifecycle string into the typed enum.
/// Keeps the accepted vocabulary in one place so it stays in sync with
/// `WaveLifecycle`'s serde rename. A future variant addition surfaces
/// as a compile-time `match` exhaustiveness diff if you write the new
/// arm here too.
fn parse_lifecycle_name(s: &str) -> Result<WaveLifecycle, RpcError> {
    match s {
        "draft" => Ok(WaveLifecycle::Draft),
        "planning" => Ok(WaveLifecycle::Planning),
        "dispatching" => Ok(WaveLifecycle::Dispatching),
        "working" => Ok(WaveLifecycle::Working),
        "blocked" => Ok(WaveLifecycle::Blocked),
        "reviewing" => Ok(WaveLifecycle::Reviewing),
        "done" => Ok(WaveLifecycle::Done),
        "canceled" => Ok(WaveLifecycle::Canceled),
        "failed" => Ok(WaveLifecycle::Failed),
        other => Err(RpcError::invalid_params(format!(
            "update_wave_state: unknown lifecycle `{other}`. \
             Allowed: draft, planning, dispatching, working, blocked, \
             reviewing, done, canceled, failed."
        ))),
    }
}

// ---------------------------------------------------------------------------
// calm.update_task_meta
// ---------------------------------------------------------------------------

fn update_task_meta_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_UPDATE_TASK_META.into(),
        description: "Spec-only: record the spec's accept/reject verdict on \
             a worker's prior result. `idempotency_key` echoes the original \
             `*.job_requested`. `status = \"accepted\"` emits \
             `task.completed`; `status = \"rejected\"` emits `task.failed` \
             with `reason` (free-form). The verdict is persisted on the \
             events log so audit replay surfaces the spec's rationale."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key", "status"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "status": { "type": "string", "enum": ["accepted", "rejected"] },
                "reason": { "type": "string" }
            }
        }),
    }
}

async fn update_task_meta(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("update_task_meta: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("update_task_meta: missing `status`"))?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let event = match status {
        "accepted" => Event::TaskCompleted {
            idempotency_key,
            // Fold the verdict + reason into `result` so audit replay
            // sees the spec's rationale verbatim. Workers' own
            // task.completed emits leave `result` to free-form agent
            // output; the spec's emits use this structured shape so a
            // downstream consumer can pattern-match on
            // `result.status == "accepted"` to tell verdicts apart
            // from worker self-reports.
            result: json!({
                "status": "accepted",
                "reason": reason.unwrap_or_default(),
            }),
            artifacts: vec![],
        },
        "rejected" => Event::TaskFailed {
            idempotency_key,
            // `reason` is required-by-convention for rejections; an
            // empty string is a valid value (the spec might reject
            // for "no reason given" — we don't second-guess the
            // verdict).
            reason: reason.unwrap_or_default(),
        },
        other => {
            return Err(RpcError::invalid_params(format!(
                "update_task_meta: unknown status `{other}` (expected `accepted` or `rejected`)"
            )));
        }
    };

    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let actor = identity.to_actor_id();
    let kind_tag = event.kind_tag();

    let res = write_with_event_typed::<(), _>(
        ctx.repo.as_ref(),
        actor,
        scope,
        None,
        &ctx.events,
        &ctx.card_role_cache,
        &ctx.wave_cove_cache,
        move |_tx| {
            let event = event.clone();
            Box::pin(async move { Ok(((), event)) })
        },
    )
    .await;

    match res {
        Ok(_) => Ok(json!({ "ok": true })),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("emit {kind_tag}: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Look up the wave the calling card belongs to, returning the card +
/// wave rows. Mirrors PR7a's `emit_event_for_identity` resolve step:
/// the bound card was minted at handshake time; a missing row between
/// then and now means a delete-while-active race, which we surface as
/// `InternalError` (the operator wants to see this loud).
async fn resolve_wave_for_identity(
    ctx: &Arc<AppContext>,
    identity: &CardIdentity,
) -> Result<(crate::model::Card, Wave), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("wave_state: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_state: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_state: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_state: wave {} for card {} not found",
                card.wave_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, wave))
}

/// Map an eventized-write error into the appropriate `RpcError`. The
/// in-tx role gate's `Forbidden` becomes `-32403`; everything else is
/// `-32603` internal. Centralized so all three tools surface the
/// same error shape for the same DB-side failure modes.
fn map_emit_error(e: CalmError) -> RpcError {
    match e {
        CalmError::Forbidden(msg) => {
            RpcError::custom(-32403, format!("update_wave_state: forbidden: {msg}"))
        }
        other => RpcError::internal(format!("update_wave_state: {other}")),
    }
}

/// Human-readable label for a JSON value's variant — used in error
/// messages so the caller sees `"got string"` rather than the value
/// itself (which may be large / contain secrets).
fn shape_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CardId;
    use crate::model::CardRole;

    fn identity_with_role(role: CardRole) -> CardIdentity {
        CardIdentity {
            card_id: CardId::from("card-1"),
            role,
        }
    }

    #[test]
    fn require_role_accepts_matching_role() {
        let id = identity_with_role(CardRole::Spec);
        assert!(require_role(&id, CardRole::Spec).is_ok());
    }

    #[test]
    fn require_role_rejects_worker_for_spec_tool() {
        let id = identity_with_role(CardRole::Worker);
        let err = require_role(&id, CardRole::Spec).expect_err("worker must be denied");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("Spec"),
            "error should mention required role: {err:?}"
        );
        assert!(
            err.message.contains("Worker"),
            "error should mention got role: {err:?}"
        );
    }

    #[test]
    fn require_role_rejects_plain_for_spec_tool() {
        let id = identity_with_role(CardRole::Plain);
        let err = require_role(&id, CardRole::Spec).expect_err("plain must be denied");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
    }

    #[test]
    fn parse_wave_patch_empty_is_ok() {
        let p = parse_wave_patch(&json!({})).expect("empty patch is a valid no-op");
        assert!(p.title.is_none());
        assert!(p.sort.is_none());
        assert!(p.archived_at.is_none());
    }

    #[test]
    fn parse_wave_patch_title_and_sort() {
        let p = parse_wave_patch(&json!({"title": "new", "sort": 1.5})).expect("happy-path patch");
        assert_eq!(p.title.as_deref(), Some("new"));
        assert_eq!(p.sort, Some(1.5));
        assert!(p.archived_at.is_none());
    }

    #[test]
    fn parse_wave_patch_archive_then_unarchive() {
        // Integer → archive (`Some(Some(ts))`).
        let archived =
            parse_wave_patch(&json!({"archived_at": 12345})).expect("archive patch parses");
        assert_eq!(archived.archived_at, Some(Some(12345)));

        // Explicit null → unarchive (`Some(None)`).
        let unarchived =
            parse_wave_patch(&json!({"archived_at": null})).expect("unarchive patch parses");
        assert_eq!(unarchived.archived_at, Some(None));

        // Omitted → leave alone (`None`).
        let untouched = parse_wave_patch(&json!({})).expect("omitted patch parses");
        assert!(untouched.archived_at.is_none());
    }

    #[test]
    fn parse_wave_patch_rejects_wrong_types() {
        // title must be string
        let err = parse_wave_patch(&json!({"title": 7})).expect_err("integer title rejected");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("title"));

        // sort must be number
        let err = parse_wave_patch(&json!({"sort": "abc"})).expect_err("string sort rejected");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("sort"));

        // archived_at must be integer or null
        let err = parse_wave_patch(&json!({"archived_at": "yesterday"}))
            .expect_err("string archived_at rejected");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("archived_at"));
    }

    #[test]
    fn parse_wave_patch_rejects_non_object_args() {
        let err = parse_wave_patch(&json!("not-an-object")).expect_err("string args rejected");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("object"));
    }
}
