//! Issue #229 PR B — wave-report card payload + MCP-tool support helpers.
//!
//! The wave-report card is a kernel-owned card minted at wave-create time
//! (plus backfilled for legacy waves via migration 0014). Its payload is a
//! single Markdown document the spec agent maintains via three MCP tools
//! that mimic codex's native Read/Edit/Write file tools 1:1:
//!
//!   * `calm.report.read`  — fetch current body + summary
//!   * `calm.report.write` — wholesale replace (like codex `Write`)
//!   * `calm.report.edit`  — string replacement (like codex `Edit`;
//!     `old_string` must be unique unless `replace_all = true`)
//!
//! Storage shape is intentionally one big Markdown string rather than a
//! `Vec<Section>` — sections are derived at render time by splitting at
//! H1 headings (`^# `). This keeps the spec agent's mental model simple
//! (it's editing a Markdown file), keeps the wire shape stable across
//! UI iterations on the section vocabulary, and avoids a second
//! storage-shape negotiation if the section list ever needs to change.
//!
//! ## Schema versioning (Tier A persistence contract)
//!
//! See `docs/upgrade-stability.md`. The struct carries `schema_version`
//! explicitly + matches it against
//! [`crate::validation::WAVE_REPORT_PAYLOAD_SCHEMA_VERSION`] at every
//! write boundary. v1 is the only shape that has ever existed.
//!
//! ## Field rationale ([[required-over-option]])
//!
//! `summary` and `body` are required `String` (not `Option<String>`):
//! every callsite must commit to a value. An empty `summary` is a valid
//! value ("the agent hasn't written a one-liner yet"); the `Option`
//! shape would have introduced two indistinguishable absent-states
//! (`null` vs missing) for no information gain. `WaveReportPayload::initial()`
//! seeds the canonical "agent hasn't run yet" defaults.

use crate::db::RouteRepo;
use crate::db::sqlite::{card_body_crdt_get_tx, card_update_with_crdt_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::{Card, CardPatch, Wave, WaveLifecycle};
use crate::recorder_shadow::{RecorderShadowDecisionKind, RecorderShadowProbe};
use crate::state::WriteContext;
use crate::wave_lifecycle::{apply_requested_transition_in_tx, auto_promote_draft_in_tx};
use crate::wave_report_doc::ReportDoc;
use std::sync::Arc;

// #679 PR1 — `WaveReportPayload` moved to `calm_types::wave_report`
// (Tier-A persisted payload, TS-exported). Re-exported so the
// `crate::wave_report::WaveReportPayload` path is unchanged.
pub use calm_types::wave_report::{ReportBlock, WaveReportPayload};

// ---------------------------------------------------------------------------
// Report-doc operations (#960 PR2)
// ---------------------------------------------------------------------------

/// One mutation of the report's CRDT block map, executed *inside* the
/// persist transaction by [`persist_report_with_shadow`] — the only
/// place `if_rev` may be checked, because only there is
/// `ReportDoc::block_rev` the transactional truth (the JSON `blocks`
/// cache can be arbitrarily stale under D8).
///
/// Every variant lands through the same five-step persist sequence and
/// therefore keeps the dual-event invariant: one successful op = one
/// `CardUpdated` + one `WaveReportEdited` whose `body_before/after`
/// are the flat projections.
#[derive(Debug, Clone)]
pub enum ReportDocOp {
    /// Wholesale `(summary, body)` replace — the legacy
    /// `calm.report.write`/`edit` tools and the REST user-edit path.
    /// `summary: None` keeps the doc's **current** summary, resolved
    /// inside the persist transaction against the CRDT truth (#960
    /// PR2 review: an outside-tx snapshot would let a concurrent
    /// summary write be silently reverted — TOCTOU).
    Replace {
        summary: Option<String>,
        body: String,
    },
    /// `calm.report.write_markdown`: wholesale replace whose body may
    /// carry `<!-- neige:b_xxxx -->` marker lines. Markers are
    /// stripped unconditionally in-tx (they never reach storage) and
    /// become exact id-reuse hints; unmarked slices fall back to the
    /// LCS alignment. `summary: None` keeps the current summary,
    /// resolved in-tx (same TOCTOU rule as [`Self::Replace`]).
    WriteMarkdown {
        summary: Option<String>,
        body: String,
    },
    /// `calm.report.blocks.upsert`. `id: None` creates (at `position`,
    /// default append); `id: Some` replaces and requires `if_rev`.
    UpsertBlock {
        id: Option<String>,
        kind: String,
        markdown: String,
        if_rev: Option<u32>,
        position: Option<usize>,
    },
    /// `calm.report.blocks.move`: reorder only, rev untouched.
    MoveBlock {
        id: String,
        to_index: usize,
        if_rev: Option<u32>,
    },
    /// `calm.report.blocks.delete`: `if_rev` is mandatory.
    DeleteBlock { id: String, if_rev: u32 },
}

/// `(id, rev)` a block-level [`ReportDocOp`] resolved to: the created/
/// replaced/moved block's id and its post-op rev. `None` for the
/// wholesale and delete variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockOpOutcome {
    pub id: String,
    pub rev: u32,
}

/// Execute `op` against the (already migrated) doc. Returns
/// `CalmError::Conflict` on an `if_rev` mismatch — the persist closure
/// propagates it, aborting the transaction, so a conflicting op writes
/// nothing and emits nothing. Unknown ids / out-of-range indexes are
/// `CalmError::BadRequest`.
fn apply_report_op(
    doc: &mut ReportDoc,
    op: &ReportDocOp,
) -> Result<Option<BlockOpOutcome>, CalmError> {
    fn check_rev(doc: &ReportDoc, id: &str, expected: u32) -> Result<u32, CalmError> {
        // A malformed doc/rev is Internal (corruption), never folded
        // into "block not found" (BadRequest).
        let current = doc
            .block_rev(id)
            .map_err(|e| CalmError::Internal(format!("wave_report: block rev: {e}")))?
            .ok_or_else(|| CalmError::BadRequest(format!("block {id} not found")))?;
        if current != expected {
            return Err(CalmError::Conflict(format!(
                "rev conflict on block {id}: current rev is {current}, expected if_rev {expected} \
                 — re-read the report and retry with the current rev"
            )));
        }
        Ok(current)
    }
    let internal = |e: anyhow::Error| CalmError::Internal(format!("wave_report: block op: {e}"));
    // `summary: None` = keep the current summary. Resolved HERE,
    // inside the persist transaction, from the doc itself — never from
    // a caller-side snapshot (which could revert a summary written
    // between the caller's read and this tx).
    let tx_summary = |doc: &ReportDoc, summary: &Option<String>| -> Result<String, CalmError> {
        match summary {
            Some(summary) => Ok(summary.clone()),
            None => Ok(doc
                .project()
                .map_err(|e| {
                    CalmError::Internal(format!("wave_report: read current summary: {e}"))
                })?
                .0),
        }
    };
    match op {
        ReportDocOp::Replace { summary, body } => {
            let summary = tx_summary(doc, summary)?;
            doc.update(&summary, body).map_err(internal)?;
            Ok(None)
        }
        ReportDocOp::WriteMarkdown { summary, body } => {
            let summary = tx_summary(doc, summary)?;
            let marked = calm_types::report_blocks::strip_markers_and_split(body);
            doc.update_with_hints(&summary, &marked.slices, &marked.hints)
                .map_err(internal)?;
            Ok(None)
        }
        ReportDocOp::UpsertBlock {
            id,
            kind,
            markdown,
            if_rev,
            position,
        } => match id {
            Some(id) => {
                let expected = if_rev.ok_or_else(|| {
                    CalmError::BadRequest(
                        "if_rev is required when replacing an existing block".into(),
                    )
                })?;
                check_rev(doc, id, expected)?;
                let (id, rev) = doc
                    .upsert_block(Some(id), kind, markdown)
                    .map_err(internal)?;
                Ok(Some(BlockOpOutcome { id, rev }))
            }
            None => {
                let len = doc.block_index().map_err(internal)?.len();
                if let Some(position) = position
                    && *position > len
                {
                    return Err(CalmError::BadRequest(format!(
                        "position {position} out of range (report has {len} blocks)"
                    )));
                }
                let (id, rev) = doc.upsert_block(None, kind, markdown).map_err(internal)?;
                if let Some(position) = position
                    && *position < len
                {
                    doc.move_block(&id, *position).map_err(internal)?;
                }
                Ok(Some(BlockOpOutcome { id, rev }))
            }
        },
        ReportDocOp::MoveBlock {
            id,
            to_index,
            if_rev,
        } => {
            let current = doc
                .block_rev(id)
                .map_err(|e| CalmError::Internal(format!("wave_report: block rev: {e}")))?
                .ok_or_else(|| CalmError::BadRequest(format!("block {id} not found")))?;
            if let Some(expected) = if_rev {
                check_rev(doc, id, *expected)?;
            }
            let len = doc.block_index().map_err(internal)?.len();
            if *to_index >= len {
                return Err(CalmError::BadRequest(format!(
                    "to_index {to_index} out of range (report has {len} blocks)"
                )));
            }
            doc.move_block(id, *to_index).map_err(internal)?;
            Ok(Some(BlockOpOutcome {
                id: id.clone(),
                rev: current,
            }))
        }
        ReportDocOp::DeleteBlock { id, if_rev } => {
            check_rev(doc, id, *if_rev)?;
            doc.delete_block(id).map_err(internal)?;
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Shared persist boundary (Issue #247 PR3)
// ---------------------------------------------------------------------------

/// Look up the wave-report card for a given wave id, returning the
/// `(wave, report_card, current_payload)` triple. The invariant
/// "every wave has exactly one report card" (PR1 backfill + the
/// partial unique index on `cards.kind = 'wave-report'`) means a
/// missing report row signals a data-shape bug, not a 404.
///
/// Errors:
///   * `CalmError::NotFound` — the wave row doesn't exist.
///   * `CalmError::Internal` — wave exists but has no report card
///     (invariant violation), OR the persisted payload won't
///     deserialize (someone wrote past card kind validation).
///
/// Used by `routes::waves::update_wave_report` (REST) to gather the
/// pieces `persist_report` needs without duplicating the row-lookup
/// logic across paths. The MCP path uses its own resolver
/// (`mcp_server::tools::wave_report::resolve_report_for_caller`)
/// because it derives the wave from the connection-bound spec card
/// rather than a path parameter — but both ultimately funnel into
/// the same [`persist_report`] writer below.
pub async fn resolve_report_for_wave(
    repo: &dyn RouteRepo,
    wave_id: &str,
) -> Result<(Wave, Card, WaveReportPayload), CalmError> {
    let wave = repo
        .wave_get(wave_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
    let cards = repo.cards_by_wave(wave.id.as_str()).await?;
    let report_card = cards
        .into_iter()
        .find(|c| c.kind == "wave-report")
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "wave_report: wave {wave_id} has no wave-report card (invariant violation)"
            ))
        })?;
    let payload: WaveReportPayload =
        serde_json::from_value(report_card.payload.clone()).map_err(|e| {
            CalmError::Internal(format!(
                "wave_report: malformed payload on card {}: {e}",
                report_card.id.as_str()
            ))
        })?;
    Ok((wave, report_card, payload))
}

/// Persist a new `WaveReportPayload` onto the wave-report card row and
/// emit `Event::CardUpdated` + `Event::WaveReportEdited` from the same
/// transaction. Returns the updated `Card` row so callers can build
/// their wire response (REST returns the projected payload, MCP
/// returns `{ updated_at }`).
///
/// Single write boundary for every wave-report mutation — both the
/// spec-MCP tools (`calm.report.write` / `calm.report.edit`, with
/// `author = Spec`) and the REST user-edit endpoint (`POST
/// /api/waves/:id/report`, with `author = User`) funnel through this
/// function so the CRDT-write + dual-event invariant holds uniformly:
/// every call → one `CardUpdated` + one `WaveReportEdited`.
///
/// Issue #247 PR1 — materializes the opaque CRDT blob alongside the
/// legacy `payload` JSON. The CRDT is authoritative; the JSON column
/// is a read-cache the existing v1 REST / WS read paths and the
/// frontend continue to consume.
///
/// Issue #247 PR2 — every call also emits a structured
/// `Event::WaveReportEdited` carrying `(summary_before, summary_after,
/// body_before, body_after, author, edit_id)` so PR4's UI can render an
/// edit timeline and PR5's spec agent can wake on user-authored edits.
///
/// Issue #247 PR3 — `author` is now a parameter (was hard-coded
/// `Spec`). The MCP tools pass `EditAuthor::Spec`; the REST handler
/// passes `EditAuthor::User`. The `EditAuthor::Kernel` arm has no
/// caller today and is reserved for future server-internal rewrites.
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
/// `old_string == new_string`). PR4's UI can filter no-op entries from
/// the timeline if it wants. Keeping the invariant "every
/// `persist_report` call → one `CardUpdated` + one `WaveReportEdited`"
/// dead simple means downstream consumers never have to second-guess
/// whether an event is missing.
///
/// The `current_payload` argument is the payload as it was last seen
/// by the caller. It's used only as the seed for the first-time
/// `from_payload` branch — once `body_crdt` is non-NULL, the doc is
/// the source.
#[allow(clippy::too_many_arguments)]
pub async fn persist_report(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    wave: Wave,
    report_card: Card,
    current_payload: WaveReportPayload,
    next: WaveReportPayload,
    agent_message: Option<String>,
    lifecycle: Option<WaveLifecycle>,
    auto_promote_draft: bool,
) -> Result<Card, CalmError> {
    let (updated, _outcome) = persist_report_with_shadow(
        repo,
        events,
        write,
        actor,
        author,
        wave,
        report_card,
        current_payload,
        ReportDocOp::Replace {
            summary: Some(next.summary),
            body: next.body,
        },
        agent_message,
        lifecycle,
        auto_promote_draft,
        None,
    )
    .await?;
    Ok(updated)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_report_with_shadow(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    wave: Wave,
    report_card: Card,
    current_payload: WaveReportPayload,
    op: ReportDocOp,
    agent_message: Option<String>,
    lifecycle: Option<WaveLifecycle>,
    auto_promote_draft: bool,
    recorder_shadow: Option<Arc<dyn RecorderShadowProbe>>,
) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
    let report_card_id = report_card.id.clone();
    let wave_id = wave.id.clone();
    let cove_id = wave.cove_id.clone();
    let scope = EventScope::Card {
        card: report_card_id.clone(),
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let wave_scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id,
    };
    let report_card_id_inner = report_card_id.clone();
    let wave_id_for_event = wave_id.clone();
    let (updated, _ids) = write_with_actor_events_typed::<(Card, Option<BlockOpOutcome>), _>(
        repo,
        None,
        events,
        write,
        move |tx| {
            let id = report_card_id_inner.as_str().to_string();
            let report_card_id = report_card_id_inner.clone();
            let wave_id = wave_id_for_event.clone();
            let scope = scope.clone();
            let wave_scope = wave_scope.clone();
            let current_payload = current_payload.clone();
            let op = op.clone();
            let actor = actor.clone();
            let agent_message = agent_message.clone();
            let recorder_shadow = recorder_shadow.clone();
            Box::pin(async move {
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                if auto_promote_draft
                    && let Some(auto_events) = auto_promote_draft_in_tx(tx, &wave_id).await?
                {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &wave_id,
                        target,
                        &actor,
                        agent_message.clone().unwrap_or_default(),
                    )
                    .await?
                {
                    if let Some(probe) = recorder_shadow.as_ref() {
                        probe
                            .record(tx, RecorderShadowDecisionKind::WaveLifecycle)
                            .await?;
                    }
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), wave_scope.clone(), event)),
                    );
                }
                if let Some(probe) = recorder_shadow.as_ref() {
                    probe
                        .record(tx, RecorderShadowDecisionKind::ReportWrite)
                        .await?;
                }
                // 1. Load (or lazy-init) the CRDT doc for this card.
                //    Loaded docs may still carry the pre-#960 layout
                //    (`ROOT.body` Text, no block map) — migrate them
                //    in place, reusing the PR1-derived block ids from
                //    the payload JSON as the id hint. The migrated
                //    bytes are written back below in this same tx.
                let existing = card_body_crdt_get_tx(tx, &id).await?;
                let mut doc = match existing {
                    Some(bytes) => {
                        let mut doc = ReportDoc::from_bytes(&bytes).map_err(|e| {
                            CalmError::Internal(format!(
                                "wave_report: load CRDT for card {id}: {e}"
                            ))
                        })?;
                        doc.ensure_blocks_layout(current_payload.blocks.as_deref())
                            .map_err(|e| {
                                CalmError::Internal(format!(
                                    "wave_report: migrate CRDT block layout for card {id}: {e}"
                                ))
                            })?;
                        doc
                    }
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
                //    entry. A malformed doc surfaces as Internal here
                //    (never a panic).
                let (summary_before, body_before) = doc.project().map_err(|e| {
                    CalmError::Internal(format!("wave_report: project CRDT for card {id}: {e}"))
                })?;
                // 3. Apply the requested op on the doc. `if_rev`
                //    checks happen in here, against the CRDT truth
                //    inside this transaction — a conflict aborts the
                //    tx (nothing written, no events emitted).
                let outcome = apply_report_op(&mut doc, &op)?;
                // 4. Project back — these are the authoritative values
                //    that go into the JSON cache. Since #960 PR2 the
                //    CRDT block map is the source of truth: `body` is
                //    the per-block concatenation and `blocks` is the
                //    doc's own snapshot (id/rev alignment already
                //    happened inside `ReportDoc::update`), so nothing
                //    is re-derived at the JSON layer.
                let (summary_after, body_after) = doc.project().map_err(|e| {
                    CalmError::Internal(format!(
                        "wave_report: project CRDT post-op for card {id}: {e}"
                    ))
                })?;
                let mut projected_payload =
                    WaveReportPayload::new(summary_after.clone(), body_after.clone());
                projected_payload.blocks = Some(doc.blocks_snapshot().map_err(|e| {
                    CalmError::Internal(format!(
                        "wave_report: snapshot CRDT blocks for card {id}: {e}"
                    ))
                })?);
                let payload_value = serde_json::to_value(&projected_payload).map_err(|e| {
                    CalmError::Internal(format!("wave_report: serialize projected payload: {e}"))
                })?;
                let patch = CardPatch {
                    title: None,
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
                    author,
                    edit_id: uuid::Uuid::new_v4().to_string(),
                    summary_before,
                    summary_after,
                    body_before,
                    body_after,
                    agent_message,
                };
                events.push((
                    actor.clone(),
                    scope.clone(),
                    Event::CardUpdated(updated.clone()),
                ));
                events.push((actor, scope, report_edited));
                Ok(((updated, outcome), events))
            })
        },
    )
    .await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initial_carries_current_schema_version() {
        let p = WaveReportPayload::initial();
        assert_eq!(p.schema_version, WaveReportPayload::SCHEMA_VERSION);
        assert!(p.summary.is_empty());
        assert!(p.body.contains("# 概要"));
        assert!(p.body.ends_with('\n'));
    }

    #[test]
    fn serde_round_trip_camelcase_wire() {
        let p = WaveReportPayload::new("hi", "# A\n\nb\n");
        let v = serde_json::to_value(&p).unwrap();
        // Wire shape: camelCase keys. A drift here would break the
        // frontend's zod schema silently — pin via this test.
        assert_eq!(
            v,
            json!({
                "schemaVersion": 2,
                "summary": "hi",
                "body": "# A\n\nb\n",
            })
        );
        let back: WaveReportPayload = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn deserialize_rejects_missing_fields() {
        // No `body`.
        let err = serde_json::from_value::<WaveReportPayload>(json!({
            "schemaVersion": 1,
            "summary": "x"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("body"), "got: {err}");

        // No `summary`.
        let err = serde_json::from_value::<WaveReportPayload>(json!({
            "schemaVersion": 1,
            "body": "x"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("summary"), "got: {err}");
    }

    #[test]
    fn apply_op_with_none_summary_resolves_from_doc_inside_tx() {
        // #960 PR2 review (write_markdown TOCTOU): `summary: None`
        // must resolve against the doc — the in-tx truth — not any
        // caller-side snapshot. Simulate the race by moving the doc's
        // summary after "the caller read it".
        let mut doc =
            ReportDoc::from_payload(&WaveReportPayload::new("stale snapshot", "# A\n\nalpha\n"));
        doc.update("racing summary", "# A\n\nalpha\n").unwrap();

        let outcome = apply_report_op(
            &mut doc,
            &ReportDocOp::WriteMarkdown {
                summary: None,
                body: "# A\n\nalpha edited\n".into(),
            },
        )
        .unwrap();
        assert!(outcome.is_none());
        let (summary, body) = doc.project().unwrap();
        assert_eq!(
            summary, "racing summary",
            "None must keep the doc's current (in-tx) summary"
        );
        assert_eq!(body, "# A\n\nalpha edited\n");

        // Replace with None behaves identically; Some overrides.
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: None,
                body: "# B\n\nbeta\n".into(),
            },
        )
        .unwrap();
        assert_eq!(doc.project().unwrap().0, "racing summary");
        apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: Some("explicit".into()),
                body: "# C\n\ngamma\n".into(),
            },
        )
        .unwrap();
        assert_eq!(doc.project().unwrap().0, "explicit");
    }

    #[test]
    fn apply_op_on_malformed_doc_is_internal_not_bad_request() {
        use automerge::transaction::Transactable;
        use automerge::{AutoCommit, ObjType, ROOT};

        // Shape 1: block rev stored as a Str. check_rev must surface
        // CalmError::Internal (corruption), never fold the broken rev
        // into "block not found" (BadRequest).
        let mut raw = AutoCommit::new();
        let summary_id = raw.put_object(&ROOT, "summary", ObjType::Text).unwrap();
        raw.update_text(&summary_id, "s").unwrap();
        let blocks = raw.put_object(&ROOT, "blocks", ObjType::Map).unwrap();
        let entry = raw.put_object(&blocks, "b_0001", ObjType::Map).unwrap();
        raw.put(&entry, "kind", "prose").unwrap();
        raw.put(&entry, "rev", "three").unwrap();
        let text_id = raw.put_object(&entry, "text", ObjType::Text).unwrap();
        raw.update_text(&text_id, "# A\n").unwrap();
        let order = raw.put_object(&ROOT, "order", ObjType::List).unwrap();
        raw.insert(&order, 0, "b_0001").unwrap();
        let mut doc = ReportDoc::from_bytes(&raw.save()).unwrap();
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::UpsertBlock {
                id: Some("b_0001".into()),
                kind: "prose".into(),
                markdown: "x\n".into(),
                if_rev: Some(1),
                position: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");

        // Shape 2: blocks map present but no order list. A wholesale
        // replace must fail Internal-level — the corrupt doc must not
        // be read as an empty report and silently overwritten.
        let mut raw = AutoCommit::new();
        let summary_id = raw.put_object(&ROOT, "summary", ObjType::Text).unwrap();
        raw.update_text(&summary_id, "s").unwrap();
        raw.put_object(&ROOT, "blocks", ObjType::Map).unwrap();
        let mut doc = ReportDoc::from_bytes(&raw.save()).unwrap();
        let err = apply_report_op(
            &mut doc,
            &ReportDocOp::Replace {
                summary: Some("s".into()),
                body: String::new(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "got {err:?}");
    }

    #[test]
    fn initial_uses_current_chinese_seed_body() {
        // Migration 0014 is historical and intentionally remains
        // frozen; freshly-minted waves use the current Chinese seed.
        let p = WaveReportPayload::initial();
        assert_eq!(
            p.body,
            "# 概要\n\n_Spec agent 会在第一次 turn 时填这里。_\n"
        );
    }
}
