//! Issue #229 PR B — wave-report MCP tools.
//!
//! Three tools the spec agent uses to maintain its wave-report card's
//! Markdown body. The argument shapes deliberately mimic codex's native
//! `Read` / `Edit` / `Write` file tools 1:1 so the agent's mental model
//! is "the report is a file I can edit," not "the report is a structured
//! kernel object I need a special API for."
//!
//! ## Tool surface
//!
//! | Tool | Shape | Notes |
//! |---|---|---|
//! | `calm.report.read`  | `{}` | Returns `{ body, summary, schemaVersion, updated_at }`. |
//! | `calm.report.write` | `{ body: String, summary?: String }` | Wholesale replace (like codex `Write`). |
//! | `calm.report.edit`  | `{ old_string: String, new_string: String, replace_all?: bool }` | Like codex `Edit` — `old_string` must be unique unless `replace_all = true`. |
//!
//! ## Authorization
//!
//! All three tools require the caller's connection-bound card to be a
//! `CardRole::Spec`. We re-use [`require_role`] for the soft gate; the
//! eventized write itself routes through `card_update_tx` on the
//! wave-report card row, which doesn't itself touch the role gate (the
//! report card emits `CardUpdated` under its own card scope, which any
//! actor with write access to the wave can do — the actual "only spec
//! may edit the report" policy lives at the MCP entry).
//!
//! The wave the caller's spec card belongs to is the wave whose report
//! card these tools mutate; a spec card from a different wave cannot
//! reach this wave's report. The lookup-by-(caller's wave_id +
//! kind="wave-report") path makes cross-wave writes impossible by
//! construction.
//!
//! ## Edit semantics (matched to codex's Edit)
//!
//!   * `old_string == new_string` → falls through to `persist_report`
//!     as a content-equal write. Emits the same two-event pair
//!     (`CardUpdated` + `WaveReportEdited`) as every other persist
//!     path, with `body_before == body_after`. PR4's UI consumer can
//!     filter no-op edits from the timeline client-side; the kernel
//!     keeps a uniform "every persist → two events" invariant so
//!     downstream consumers never have to second-guess whether an
//!     event is missing.
//!   * `old_string` not found in `body` → `-32602` "old_string not
//!     found in body".
//!   * `old_string` found multiple times and `replace_all` not true →
//!     `-32602` "old_string is not unique; pass replace_all=true to
//!     replace all matches".
//!   * `old_string` found multiple times and `replace_all = true` →
//!     replace every occurrence (Rust `str::replace` semantics — left
//!     to right, no overlap).
//!   * `old_string` found exactly once → replace it. (replace_all is
//!     redundant in this case; we accept it for codex Edit symmetry.)

use crate::db::sqlite::{card_body_crdt_get_tx, card_update_with_crdt_tx};
use crate::db::write_with_events_typed;
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventScope};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, CardIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role,
};
use crate::model::{Card, CardPatch, CardRole, Wave};
use crate::wave_report::WaveReportPayload;
use crate::wave_report_doc::ReportDoc;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_REPORT_READ: &str = "calm.report.read";
pub const TOOL_REPORT_WRITE: &str = "calm.report.write";
pub const TOOL_REPORT_EDIT: &str = "calm.report.edit";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(read_descriptor(), wrap(report_read));
    registry.register(write_descriptor(), wrap(report_write));
    registry.register(edit_descriptor(), wrap(report_edit));
}

/// Boxed-future wrapper, same shape as the other tool modules.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, CardIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

// ---------------------------------------------------------------------------
// calm.report.read
// ---------------------------------------------------------------------------

fn read_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_READ.into(),
        description: "Spec-only: read the wave's report Markdown body + \
             one-line summary. Returns `{ body, summary, schemaVersion, \
             updated_at }`. Behaves like the codex `Read` file tool — \
             call before editing so you have the current text to base \
             your `old_string` on."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

async fn report_read(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let (_, _, report_card, payload) = resolve_report_for_caller(&ctx, &identity).await?;
    Ok(json!({
        "body": payload.body,
        "summary": payload.summary,
        "schemaVersion": payload.schema_version,
        "updated_at": report_card.updated_at,
    }))
}

// ---------------------------------------------------------------------------
// calm.report.write
// ---------------------------------------------------------------------------

fn write_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_WRITE.into(),
        description: "Spec-only: wholesale-replace the wave's report \
             body (and optionally `summary`). Behaves like the codex \
             `Write` file tool — clobbers prior content. Use \
             `calm.report.edit` for targeted string replacement \
             instead when only part of the body changes. Returns \
             `{ updated_at }`. Omitting `summary` leaves the existing \
             summary unchanged."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["body"],
            "properties": {
                "body": { "type": "string" },
                "summary": { "type": "string" }
            }
        }),
    }
}

async fn report_write(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let obj = args.as_object().ok_or_else(|| {
        RpcError::invalid_params("calm.report.write: arguments must be an object")
    })?;
    let body = obj
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("calm.report.write: missing `body` (string)"))?
        .to_string();
    // `summary` is optional — if omitted, retain the existing one.
    let summary_override = match obj.get("summary") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(RpcError::invalid_params(
                "calm.report.write: `summary` must be a string if provided",
            ));
        }
    };

    let (wave, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    let next_payload = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        summary: summary_override.unwrap_or_else(|| current.summary.clone()),
        body,
    };
    persist_report(&ctx, &identity, wave, report_card, current, next_payload).await
}

// ---------------------------------------------------------------------------
// calm.report.edit
// ---------------------------------------------------------------------------

fn edit_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_EDIT.into(),
        description: "Spec-only: string-replace inside the wave's \
             report body. Behaves like the codex `Edit` file tool. \
             `old_string` must appear in the body; if it appears more \
             than once you must pass `replace_all = true`. If \
             `old_string == new_string`, the call is a content-equal \
             write that still bumps `updated_at` and emits the same \
             `CardUpdated` + `WaveReportEdited` event pair as every \
             other persist (with `body_before == body_after`). \
             Returns `{ updated_at }`. The summary is preserved — \
             call `calm.report.write` to update both at once."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["old_string", "new_string"],
            "properties": {
                "old_string": { "type": "string", "minLength": 1 },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean" }
            }
        }),
    }
}

async fn report_edit(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params("calm.report.edit: arguments must be an object"))?;
    let old_string = obj
        .get("old_string")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("calm.report.edit: missing `old_string` (non-empty string)")
        })?
        .to_string();
    let new_string = obj
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            RpcError::invalid_params(
                "calm.report.edit: missing `new_string` (string; empty is allowed)",
            )
        })?
        .to_string();
    let replace_all = match obj.get("replace_all") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(RpcError::invalid_params(
                "calm.report.edit: `replace_all` must be a boolean if provided",
            ));
        }
    };

    let (wave, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;

    // Issue #247 PR2 review: removed the `old_string == new_string`
    // short-circuit so this handler always falls through to
    // `persist_report` and emits the same `CardUpdated` +
    // `WaveReportEdited` event pair as `report.write`. The asymmetry
    // it created (write-with-identical-content → 2 events, edit-with-
    // identical-strings → 0 events) made PR4's UI consumer have to
    // special-case one persist path. We still validate `old_string`
    // is present in the body — substring-not-found stays a hard
    // error, *only* the equal-strings branch is gone.
    let occurrences = count_matches(&current.body, &old_string);
    if occurrences == 0 {
        return Err(RpcError::invalid_params(
            "calm.report.edit: old_string not found in body",
        ));
    }
    if occurrences > 1 && !replace_all {
        return Err(RpcError::invalid_params(format!(
            "calm.report.edit: old_string is not unique ({occurrences} matches); \
             pass replace_all=true to replace all matches"
        )));
    }
    // Either occurrences == 1 (replace_all is irrelevant) or
    // occurrences > 1 && replace_all (codex semantics: replace every
    // occurrence left-to-right).
    let new_body = if replace_all || occurrences > 1 {
        current.body.replace(&old_string, &new_string)
    } else {
        // Single-match path. `replacen(.., 1)` is the safe choice;
        // `replace` would also work since we already know there's
        // exactly one match.
        current.body.replacen(&old_string, &new_string, 1)
    };

    let next_payload = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        summary: current.summary.clone(),
        body: new_body,
    };
    persist_report(&ctx, &identity, wave, report_card, current, next_payload).await
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Count non-overlapping occurrences of `needle` in `haystack` using
/// `str::matches` (left-to-right, no overlap — matches `str::replace`'s
/// scan behavior so a misleading mismatch never reaches the user).
fn count_matches(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        // `str::matches("")` returns infinitely many — we treat empty
        // needle as a programmer error and refuse it upstream. Guard
        // here so a future refactor doesn't accidentally expose it.
        return 0;
    }
    haystack.matches(needle).count()
}

/// Resolve the (wave, spec card, report card, current payload) tuple
/// for the connection-bound spec identity. Errors:
///   * spec card row missing (delete-while-active race) → InternalError;
///   * wave row missing under that spec card → InternalError;
///   * no wave-report card on the wave → InternalError (the invariant
///     is "every wave has exactly one report card"; failing this is a
///     data-shape bug, not a user-visible 404);
///   * payload deserialize fails → InternalError (a malformed row would
///     mean someone wrote past the validator).
async fn resolve_report_for_caller(
    ctx: &Arc<AppContext>,
    identity: &CardIdentity,
) -> Result<(Wave, Card, Card, WaveReportPayload), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let spec_card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("wave_report: spec card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_report: bound spec card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(spec_card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_report: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_report: wave {} for spec card {} not found",
                spec_card.wave_id.as_str(),
                card_id_str
            ))
        })?;
    // Find the wave-report card. Migration 0014 + `routes::waves::create_wave`
    // guarantee exactly one per wave; the partial unique index
    // `idx_cards_one_report_per_wave` from migration 0013 backstops it.
    // Scanning every card on the wave is fine — waves are small (single
    // digits of cards in practice).
    let cards = ctx
        .repo
        .cards_by_wave(wave.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_report: cards_by_wave: {e}")))?;
    let report_card = cards
        .into_iter()
        .find(|c| c.kind == "wave-report")
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_report: wave {} has no wave-report card (invariant violation)",
                wave.id.as_str()
            ))
        })?;
    let payload: WaveReportPayload =
        serde_json::from_value(report_card.payload.clone()).map_err(|e| {
            RpcError::internal(format!(
                "wave_report: malformed payload on card {}: {e}",
                report_card.id.as_str()
            ))
        })?;
    Ok((wave, spec_card, report_card, payload))
}

/// Persist a new `WaveReportPayload` onto the wave-report card row and
/// emit `Event::CardUpdated` + `Event::WaveReportEdited` from the same
/// transaction. Returns the MCP wire shape (`{ updated_at }`).
///
/// Issue #247 PR1 — this is the single write boundary that materializes
/// the opaque CRDT blob alongside the legacy `payload` JSON. The CRDT
/// is authoritative; the JSON column is a read-cache the existing v1
/// REST / WS read paths and the frontend continue to consume.
///
/// Issue #247 PR2 — every call also emits a structured
/// `Event::WaveReportEdited` carrying `(summary_before, summary_after,
/// body_before, body_after, author, edit_id)` so PR4's UI can render an
/// edit timeline and PR5's spec agent can wake on user-authored edits.
/// `author` is hard-coded to [`EditAuthor::Spec`] here — the spec-MCP
/// tools are the only write path that exists today. PR3 plumbs an
/// `Actor` through this function when REST entry lands.
///
/// In-tx sequence:
///
///   1. Read the current `body_crdt`. NULL = first post-PR1 write on
///      this row (legacy seed / pre-#247 mint); seed a fresh doc from
///      `current_payload`. Non-NULL = load via `ReportDoc::from_bytes`.
///   2. Project the doc to capture `(summary_before, body_before)` —
///      the authoritative pre-write state for the edit-log entry.
///   3. Apply the new `(summary, body)` via `ReportDoc::update` —
///      automerge does the per-field Myers diff internally.
///   4. Project back to `(summary_after, body_after)` and re-serialize
///      a `WaveReportPayload` from those values (not the raw `next`
///      input). The projection is what the JSON cache must mirror so
///      a future read sees the post-merge text rather than a partially-
///      applied input — under single-writer it's identical to `next`,
///      but reading from the doc keeps the JSON-cache contract
///      ("CRDT is source of truth") true by construction.
///   5. Write both columns and emit both events in one tx — via
///      `write_with_events_typed` so the events are persisted in the
///      same transaction as the row update (commit-then-emit invariant
///      preserved).
///
/// **Both events fire on every call, including content-equal writes**
/// (e.g. re-asserting the same body, or `report.edit` with
/// `old_string == new_string`). The `WaveReportEdited` row records
/// `summary_before == summary_after && body_before == body_after`,
/// which PR4's UI can filter out client-side if it wants to suppress
/// no-ops from the timeline. Keeping the invariant "every
/// `persist_report` call → one `CardUpdated` + one `WaveReportEdited`"
/// dead simple means downstream consumers never have to second-guess
/// whether an event is missing — every MCP write path that reaches
/// this function emits exactly the same two-event pair.
///
/// The `current_payload` argument is the payload as it was last seen
/// by the caller (passed in from `resolve_report_for_caller`). It's
/// used only as the seed for the first-time `from_payload` branch —
/// once `body_crdt` is non-NULL, the doc is the source.
async fn persist_report(
    ctx: &Arc<AppContext>,
    identity: &CardIdentity,
    wave: Wave,
    report_card: Card,
    current_payload: WaveReportPayload,
    next: WaveReportPayload,
) -> Result<Value, RpcError> {
    let report_card_id = report_card.id.clone();
    let wave_id = wave.id.clone();
    let cove_id = wave.cove_id.clone();
    let scope = EventScope::Card {
        card: report_card_id.clone(),
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let actor = identity.to_actor_id();
    let report_card_id_inner = report_card_id.clone();
    let wave_id_for_event = wave_id.clone();
    let res = write_with_events_typed::<Card, _>(
        ctx.repo.as_ref(),
        actor,
        None,
        &ctx.events,
        &ctx.card_role_cache,
        &ctx.wave_cove_cache,
        move |tx| {
            let id = report_card_id_inner.as_str().to_string();
            let report_card_id = report_card_id_inner.clone();
            let wave_id = wave_id_for_event.clone();
            let scope = scope.clone();
            let current_payload = current_payload.clone();
            let next = next.clone();
            Box::pin(async move {
                // 1. Load (or lazy-init) the CRDT doc for this card.
                let existing = card_body_crdt_get_tx(tx, &id).await?;
                let mut doc = match existing {
                    Some(bytes) => ReportDoc::from_bytes(&bytes).map_err(|e| {
                        CalmError::Internal(format!("wave_report: load CRDT for card {id}: {e}"))
                    })?,
                    // Safe: current_payload was read outside the tx,
                    // but is only consulted here when body_crdt is
                    // still NULL in-tx. SQLite's single-writer means
                    // no concurrent writer can have populated the
                    // blob between that read and this branch; once
                    // body_crdt is non-NULL we take the Some arm and
                    // ignore current_payload entirely.
                    None => ReportDoc::from_payload(&current_payload),
                };
                // 2. Capture the pre-write projection for the edit-log
                //    entry. Reading from the doc (not from
                //    `current_payload`) keeps the before-state
                //    consistent with whatever the CRDT actually holds
                //    after the lazy-init branch above.
                let (summary_before, body_before) = doc.project();
                // 3. Apply the proposed update uniformly to both fields.
                doc.update(&next.summary, &next.body);
                // 4. Project back — these are the authoritative values
                //    that go into the JSON cache.
                let (summary_after, body_after) = doc.project();
                let projected_payload = WaveReportPayload {
                    schema_version: WaveReportPayload::SCHEMA_VERSION,
                    summary: summary_after.clone(),
                    body: body_after.clone(),
                };
                let payload_value = serde_json::to_value(&projected_payload).map_err(|e| {
                    CalmError::Internal(format!("wave_report: serialize projected payload: {e}"))
                })?;
                let patch = CardPatch {
                    kind: None,
                    sort: None,
                    payload: Some(payload_value),
                    deletable: None,
                };
                let crdt_bytes = doc.to_bytes();
                // 5. One transactional write rewriting both columns +
                //    two events tagged with the same card scope. Order
                //    matters: `CardUpdated` first so an existing
                //    subscriber that processes both events sees the
                //    generic "row changed" signal before the structured
                //    edit-log entry (matches the historical broadcast
                //    order before PR2 added the structured event).
                let updated = card_update_with_crdt_tx(tx, &id, patch, crdt_bytes).await?;
                let report_edited = Event::WaveReportEdited {
                    wave_id,
                    card_id: report_card_id,
                    // PR2 hard-codes Spec — only the spec-MCP path
                    // exists today. PR3 introduces REST + User.
                    author: EditAuthor::Spec,
                    edit_id: uuid::Uuid::new_v4().to_string(),
                    summary_before,
                    summary_after,
                    body_before,
                    body_after,
                };
                let events = vec![
                    (scope.clone(), Event::CardUpdated(updated.clone())),
                    (scope, report_edited),
                ];
                Ok((updated, events))
            })
        },
    )
    .await;
    match res {
        Ok((updated, _ids)) => Ok(json!({ "updated_at": updated.updated_at })),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("wave_report: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("wave_report: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_empty_needle_returns_zero() {
        assert_eq!(count_matches("abc", ""), 0);
    }

    #[test]
    fn count_matches_basic() {
        assert_eq!(count_matches("abcabc", "abc"), 2);
        assert_eq!(count_matches("aaa", "aa"), 1); // non-overlapping
        assert_eq!(count_matches("abc", "xyz"), 0);
        assert_eq!(count_matches("# Goal\n\n# Goal\n", "# Goal"), 2);
    }

    #[test]
    #[allow(clippy::no_effect_replace)] // The whole point of this test
    // is to pin that `str::replace(s, s)` is the identity map — that
    // identity is what makes the post-fix `report.edit` with equal
    // strings produce `body_before == body_after` instead of being a
    // bypass. The lint is exactly right that the call is a no-op;
    // that's the assertion.
    fn edit_equal_strings_replace_is_identity() {
        // Sanity-pin: PR2 review removed the `old == new` short-circuit
        // in `report_edit`, so equal strings now fall through to the
        // normal `str::replace` path. That path is the identity map
        // — `body.replace(s, s) == body` — which is what makes the
        // resulting `WaveReportEdited` carry `body_before ==
        // body_after`. End-to-end coverage lives in
        // `tests/mcp_wave_report.rs::edit_with_identical_old_and_new_still_emits_both_events`.
        let body = "the body XYZ";
        assert_eq!(body.replace("XYZ", "XYZ"), body);
        assert_eq!(body.replacen("XYZ", "XYZ", 1), body);
    }
}
