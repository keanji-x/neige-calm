//! Issue #229 PR B — track-report MCP tools.
//!
//! Three tools the spec agent uses to maintain its track-report card's
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
//! The two write tools require the caller's per-call card to be a
//! `CardRole::Spec`. `calm.report.read` additionally accepts
//! `CardRole::Assistant` (#1189): it is the only source of `docRev` and
//! the per-block `rev`s, so an assistant that could not call it could
//! never form the `if_doc_rev` / `if_rev` a block-channel write needs.
//! The assistant's response body is trimmed: `taskDiagnostics` (the
//! dispatched-task runtime projection) is Spec-only, so opening the read
//! does not hand the assistant the state `calm.plan.list` withholds.
//! We re-use [`require_role`] / [`require_role_any`] for the soft gate; the
//! eventized write itself routes through `card_update_tx` on the
//! track-report card row, which doesn't itself touch the role gate (the
//! report card emits `CardUpdated` under its own card scope, which any
//! actor with write access to the track can do — the actual "only spec
//! may edit the report" policy lives at the MCP entry).
//!
//! The track the caller's spec card belongs to is the track whose report
//! card these tools mutate; a spec card from a different track cannot
//! reach this track's report. The lookup-by-(caller's track_id +
//! kind="track-report") path makes cross-track writes impossible by
//! construction.
//!
//! ## Edit semantics (matched to codex's Edit)
//!
//!   * `old_string == new_string` → falls through to the persist
//!     boundary as a content-equal write. Emits the same two-event pair
//!     (`CardUpdated` + `TrackReportEdited`) as every other persist
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

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role, require_role_any, role_gated_write_annotations,
};
use crate::mcp_server::tools::lifecycle_args::{
    lifecycle_schema, message_schema, parse_write_args,
};
use crate::model::{Card, CardRole, Track, TrackLifecycle};
use crate::track_report::{ReportDocOp, TrackReportPayload};
use crate::track_report_read::load_report_read_snapshot;
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
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
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
        description: "Spec + Assistant: read the track's report. Returns \
             `{ text, body, summary, schemaVersion, docRev, updated_at, \
             blocks, taskDiagnostics }` — `text` is the flat Markdown (`body` is a \
             legacy alias with the same value) and `blocks` is the \
             addressable index `[{ id, kind, rev }]` in document \
             order; `taskDiagnostics` is the read-time DB-aware task-block \
             schedulability result and is returned to Spec callers only \
             (it carries dispatched-task runtime state). \
             The report is made of blocks: address them with \
             `calm.report.blocks.upsert` / `.move` / `.delete` (all \
             take `if_rev` for optimistic concurrency; see \
             `calm.report.blocks.kinds` for the block vocabulary). \
             Pass `{ with_markers: true }` to get `text` with one \
             `<!-- neige:b_xxxx -->` line before each block so you \
             can see block boundaries; markers exist only in this \
             read output (and are accepted+stripped by \
             `calm.report.write_markdown`), they are never stored."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "with_markers": {
                    "type": "boolean",
                    "description": "Inject a `<!-- neige:b_xxxx -->` marker line before each block in `text` (default false)."
                }
            }
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

pub(crate) async fn report_read(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    // #1189 — Assistant reads too. This tool is the ONLY source of
    // `docRev` and the per-block `rev`s, and every block-channel write
    // takes `if_doc_rev` / `if_rev`; keeping it Spec-only would leave the
    // assistant unable to bootstrap the CAS handshake S2 opens up. The
    // write channel (`calm.report.write` / `.edit`, which can carry
    // lifecycle) stays Spec-only — that is the §3.2 dividing line.
    require_role_any(&identity, &[CardRole::Spec, CardRole::Assistant])?;
    let with_markers = match args.get("with_markers") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(RpcError::invalid_params(
                "calm.report.read: `with_markers` must be a boolean if provided",
            ));
        }
    };
    // `resolve_report_for_caller` only supplies auth + the report
    // card id; the response body comes from ONE fresh row snapshot so
    // `summary`/`text`/`blocks` can never tear against each other.
    let (_, _, report_card, _) = resolve_report_for_caller(&ctx, &identity).await?;
    let snapshot = load_report_read_snapshot(ctx.repo.as_ref(), report_card.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_report: {e}")))?;
    let text = if with_markers {
        snapshot
            .blocks
            .iter()
            .map(|block| {
                // flat_text: markdown for prose, canonical fence for
                // non-prose (#960 PR3) — the same bytes the block
                // contributes to `body`.
                format!(
                    "{}{}",
                    calm_types::report_blocks::marker_line(&block.id),
                    calm_types::report_blocks::flat_text(block)
                )
            })
            .collect::<String>()
    } else {
        snapshot.body.clone()
    };
    let index: Vec<Value> = snapshot
        .blocks
        .iter()
        .map(|block| json!({ "id": block.id, "kind": block.kind, "rev": block.rev }))
        .collect();
    let mut response = json!({
        "text": text,
        // Legacy alias — same value as `text`, kept so existing
        // consumers keyed on `body` keep working.
        "body": text,
        "summary": snapshot.summary,
        "schemaVersion": snapshot.schema_version,
        "docRev": snapshot.doc_rev,
        "updated_at": snapshot.updated_at,
        "blocks": index,
    });
    // #1189 review round 2 — `taskDiagnostics` is NOT report content. It
    // is the read-time task/track-tree projection (`status`, `statusDetail`,
    // `gateResult`, `workerCardId`, `childTrackId`), i.e. exactly the class
    // of dispatched-task runtime state `calm.plan.list` is kept Spec-only
    // to withhold. Opening `report.read` to the assistant must not become a
    // side door onto it, so the assistant gets the document (`text` /
    // `body` / `summary` / `schemaVersion` / `docRev` / `updated_at` /
    // `blocks`) and nothing else. Spec keeps the full payload.
    if identity.role == CardRole::Spec {
        response["taskDiagnostics"] = json!(snapshot.task_diagnostics);
    }
    Ok(response)
}

// ---------------------------------------------------------------------------
// calm.report.write
// ---------------------------------------------------------------------------

fn write_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_WRITE.into(),
        description: "Spec-only compatibility interface: wholesale-\
             replace the track's report body (and optionally `summary`). \
             Prefer the block-addressed tools — `calm.report.blocks.\
             upsert`/`.move`/`.delete` for targeted changes, or \
             `calm.report.write_markdown` for full rewrites that \
             preserve block identity via markers. Behaves like the \
             codex `Write` file tool — clobbers prior content; block \
             ids are re-derived best-effort. Returns `{ updated_at, docRev }`. \
             Omitting `summary` leaves the existing summary unchanged. \
             `message` is required and is persisted as `agent_message`; \
             optional `lifecycle` advances the track state machine in \
             the same atomic write."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["body", "message", "if_doc_rev"],
            "properties": {
                "body": { "type": "string" },
                "if_doc_rev": { "type": "integer", "minimum": 0, "description": "The document-wide docRev returned by calm.report.read; not a block rev." },
                "summary": { "type": "string" },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn report_write(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let write_args = parse_write_args(&args, "calm.report.write")?;
    let obj = args.as_object().ok_or_else(|| {
        RpcError::invalid_params("calm.report.write: arguments must be an object")
    })?;
    let body = obj
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("calm.report.write: missing `body` (string)"))?
        .to_string();
    let if_doc_rev = required_doc_rev(obj, "calm.report.write")?;
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

    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    // Omitted summary = keep the existing one. The op carries `None`
    // and the persist layer resolves it against the doc INSIDE the
    // transaction — resolving from the `current` snapshot here would
    // let a concurrent summary write be silently reverted (TOCTOU,
    // #960 PR2 review).
    commit_report_write_for_identity(
        &ctx,
        &identity,
        ReportSinkCall {
            track,
            report_card,
            current_payload: current,
            summary: summary_override,
            body,
            agent_message: write_args.message,
            lifecycle: write_args.lifecycle,
            if_doc_rev,
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// calm.report.edit
// ---------------------------------------------------------------------------

fn edit_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_EDIT.into(),
        description: "Spec-only compatibility interface: string-replace \
             inside the track's report body. Prefer \
             `calm.report.blocks.upsert` with `id` + `if_rev` for \
             targeted block replacement. Behaves like the codex `Edit` \
             file tool. \
             `old_string` must appear in the body; if it appears more \
             than once you must pass `replace_all = true`. If \
             `old_string == new_string`, the call is a content-equal \
             write that still bumps `updated_at` and emits the same \
             `CardUpdated` + `TrackReportEdited` event pair as every \
             other persist (with `body_before == body_after`). \
             Returns `{ updated_at, docRev }`. The summary is preserved — \
             call `calm.report.write` to update both at once. `message` \
             is required and is persisted as `agent_message`; optional \
             `lifecycle` advances the track state machine in the same \
             atomic write."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["old_string", "new_string", "message", "if_doc_rev"],
            "properties": {
                "old_string": { "type": "string", "minLength": 1 },
                "new_string": { "type": "string" },
                "if_doc_rev": { "type": "integer", "minimum": 0, "description": "The document-wide docRev returned by calm.report.read; not a block rev." },
                "replace_all": { "type": "boolean" },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn report_edit(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let write_args = parse_write_args(&args, "calm.report.edit")?;
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
    let if_doc_rev = required_doc_rev(obj, "calm.report.edit")?;

    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;

    // Issue #247 PR2 review: removed the `old_string == new_string`
    // short-circuit so this handler always falls through to the
    // persist boundary and emits the same `CardUpdated` +
    // `TrackReportEdited` event pair as `report.write`. The asymmetry
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

    // `edit` never touches the summary: `None` keeps whatever the doc
    // holds at commit time (resolved in-tx by the persist layer).
    commit_report_write_for_identity(
        &ctx,
        &identity,
        ReportSinkCall {
            track,
            report_card,
            current_payload: current,
            summary: None,
            body: new_body,
            agent_message: write_args.message,
            lifecycle: write_args.lifecycle,
            if_doc_rev,
        },
    )
    .await
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

/// Resolve the (track, spec card, report card, current payload) tuple
/// for the per-call spec identity. Errors:
///   * spec card row missing (delete-while-active race) → InternalError;
///   * track row missing under that spec card → InternalError;
///   * no track-report card on the track → InternalError (the invariant
///     is "every track has exactly one report card"; failing this is a
///     data-shape bug, not a user-visible 404);
///   * payload deserialize fails → InternalError (a malformed row would
///     mean someone wrote past the validator).
///
/// #960 PR3: `report.write` / `report.edit` land as `ReportDocOp::
/// Replace`, whose in-tx guard (`track_report::guard_non_prose_stomp`)
/// refuses any write that would modify or delete a non-prose block.
pub(crate) async fn resolve_report_for_caller(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(Track, Card, Card, TrackReportPayload), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let spec_card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("track_report: spec card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_report: bound spec card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let track = ctx
        .repo
        .track_get(spec_card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_report: track lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_report: track {} for spec card {} not found",
                spec_card.track_id.as_str(),
                card_id_str
            ))
        })?;
    let (report_card, payload) = load_report_for_track(ctx, &track).await?;
    Ok((track, spec_card, report_card, payload))
}

/// Load the track-report card and current payload for an already-resolved track.
///
/// This helper is role-agnostic by design: callers must enforce their own MCP
/// entry gate and track binding before reaching it.
pub(crate) async fn load_report_for_track(
    ctx: &Arc<AppContext>,
    track: &Track,
) -> Result<(Card, TrackReportPayload), RpcError> {
    // Find the track-report card. Migration 0014 + `routes::tracks::create_track`
    // guarantee exactly one per track; the partial unique index
    // `idx_cards_one_report_per_track` from migration 0013 backstops it.
    // Scanning every card on the track is fine — tracks are small (single
    // digits of cards in practice).
    let cards = ctx
        .repo
        .cards_by_track(track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_report: cards_by_track: {e}")))?;
    let report_card = cards
        .into_iter()
        .find(|c| c.kind == "track-report")
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_report: track {} has no track-report card (invariant violation)",
                track.id.as_str()
            ))
        })?;
    let payload: TrackReportPayload =
        serde_json::from_value(report_card.payload.clone()).map_err(|e| {
            RpcError::internal(format!(
                "track_report: malformed payload on card {}: {e}",
                report_card.id.as_str()
            ))
        })?;
    Ok((report_card, payload))
}

/// MCP-side thin wrapper around [`CardDecisionSink::commit_report_write`].
///
/// Resolves the session-shaped actor from the per-call [`ToolCallIdentity`]
/// (Spec maps to `ActorId::AiSpecSession`; `require_role` upstream guarantees
/// the role is Spec by the time we reach this site), tags every write as the
/// spec-MCP emitter inside the sink, and projects the returned
/// `Card` into the MCP wire shape `{ updated_at, docRev }`. The error mapping
/// reproduces the pre-PR3 contract
/// (`CalmError::Forbidden` → `-32403`, anything else → internal).
///
/// Issue #247 PR3 — the heavy lifting (CRDT load / project / update /
/// dual-event emit) lives in `crate::track_report::write::persist` so
/// the REST user-edit endpoint (`POST /api/tracks/:id/report`) reaches
/// the same write boundary. The two callers share one persist path;
/// one event-pair contract; one transactional write. Anything else
/// would be two parallel implementations of the same invariant, with
/// the corresponding drift risk.
///
/// #1318 §1 — they no longer reach it through the same *function*: that
/// writer is private to `track_report::write`, and this path enters via
/// `write::agent_report_op` (through `decision_sink`) while the REST
/// endpoint enters via `write::rest_user_replace`. The sharing is
/// unchanged and the attribution got stricter — only this path can name
/// an `EditAuthor` at all, and it takes it from the role.
struct ReportSinkCall {
    track: Track,
    report_card: Card,
    current_payload: TrackReportPayload,
    /// `None` keeps the current summary, resolved by the persist
    /// layer inside the transaction (#960 PR2 review — never snapshot
    /// the summary outside the tx).
    summary: Option<String>,
    body: String,
    agent_message: String,
    lifecycle: Option<TrackLifecycle>,
    if_doc_rev: u64,
}

async fn commit_report_write_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    call: ReportSinkCall,
) -> Result<Value, RpcError> {
    match CardDecisionSink::from_app_context(ctx)
        .commit_report_op(
            identity,
            call.track,
            call.report_card,
            call.current_payload,
            ReportDocOp::Replace {
                summary: call.summary,
                body: call.body,
                if_doc_rev: call.if_doc_rev,
            },
            Some(call.agent_message),
            call.lifecycle,
        )
        .await
    {
        Ok((updated, _outcome)) => {
            let doc_rev = updated_report_doc_rev(&updated, "track_report")?;
            Ok(json!({ "updated_at": updated.updated_at, "docRev": doc_rev }))
        }
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("track_report: forbidden: {msg}"),
        )),
        // #960 PR3 — the in-tx guard/validation of `ReportDocOp::
        // Replace` (non-prose stomp, malformed/invalid neige fences)
        // surfaces as BadRequest and must map to -32602, not internal.
        Err(CalmError::BadRequest(msg)) => {
            Err(RpcError::invalid_params(format!("track_report: {msg}")))
        }
        Err(CalmError::Conflict(msg)) => Err(RpcError::custom(
            crate::mcp_server::tools::track_report_blocks::RPC_REV_CONFLICT,
            format!("track_report: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("track_report: {e}"))),
    }
}

pub(crate) fn updated_report_doc_rev(card: &Card, tool: &str) -> Result<u64, RpcError> {
    card.payload
        .get("docRev")
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::internal(format!("{tool}: updated report payload has no docRev")))
}

fn required_doc_rev(obj: &serde_json::Map<String, Value>, tool: &str) -> Result<u64, RpcError> {
    obj.get("if_doc_rev")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "{tool}: missing `if_doc_rev` (document-wide docRev; use 0 for a new document)"
            ))
        })
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
        // resulting `TrackReportEdited` carry `body_before ==
        // body_after`. End-to-end coverage lives in
        // `tests/mcp_track_report.rs::edit_with_identical_old_and_new_still_emits_both_events`.
        let body = "the body XYZ";
        assert_eq!(body.replace("XYZ", "XYZ"), body);
        assert_eq!(body.replacen("XYZ", "XYZ", 1), body);
    }
}
