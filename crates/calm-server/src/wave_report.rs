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
pub use calm_types::wave_report::WaveReportPayload;

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
    persist_report_with_shadow(
        repo,
        events,
        write,
        actor,
        author,
        wave,
        report_card,
        current_payload,
        next,
        agent_message,
        lifecycle,
        auto_promote_draft,
        None,
    )
    .await
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
    next: WaveReportPayload,
    agent_message: Option<String>,
    lifecycle: Option<WaveLifecycle>,
    auto_promote_draft: bool,
    recorder_shadow: Option<Arc<dyn RecorderShadowProbe>>,
) -> Result<Card, CalmError> {
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
    let (updated, _ids) =
        write_with_actor_events_typed::<Card, _>(repo, None, events, write, move |tx| {
            let id = report_card_id_inner.as_str().to_string();
            let report_card_id = report_card_id_inner.clone();
            let wave_id = wave_id_for_event.clone();
            let scope = scope.clone();
            let wave_scope = wave_scope.clone();
            let current_payload = current_payload.clone();
            let next = next.clone();
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
                //    entry.
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
                Ok((updated, events))
            })
        })
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
        assert!(p.body.contains("# Goal"));
        assert!(p.body.ends_with('\n'));
    }

    #[test]
    fn serde_round_trip_camelcase_wire() {
        let p = WaveReportPayload {
            schema_version: 1,
            summary: "hi".to_string(),
            body: "# A\n\nb\n".to_string(),
        };
        let v = serde_json::to_value(&p).unwrap();
        // Wire shape: camelCase keys. A drift here would break the
        // frontend's zod schema silently — pin via this test.
        assert_eq!(
            v,
            json!({
                "schemaVersion": 1,
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
    fn initial_matches_migration_seed_body() {
        // Migration 0014 hard-codes the same placeholder string; if
        // this assertion fails the migration's INSERT and `initial()`
        // have diverged — fix one to match the other so backfilled
        // and freshly-minted waves render identically.
        let p = WaveReportPayload::initial();
        assert_eq!(p.body, "# Goal\n\n_The spec agent will fill this in._\n");
    }
}
