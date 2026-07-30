//! Issue #955 §5.2 (PR-b) — the ④ proposal channel's plugin-facing
//! `neige.*` methods.
//!
//! Three methods, one gate: `permissions.proposals` must list the
//! subject kind (`Permissions::can_propose`, deny by default). The read
//! (`neige.report.get`) is gated too — it exists only to hand out an
//! anchorable baseline, so a plugin that cannot propose has no reason to
//! read the report (§5.2 "读基线").
//!
//! | method | shape | writes |
//! |---|---|---|
//! | `neige.report.get` | `{ wave_id }` → `{ blocks, doc_heads }` | — |
//! | `neige.proposal.submit` | `{ subject, base_doc_heads, ops, note, idem_key }` → `{ proposal_id }` | `ProposalSubmitted` |
//! | `neige.proposal.withdraw` | `{ proposal_id }` → `{}` | `ProposalResolved { withdrawn }` |
//!
//! Everything a plugin cannot be trusted with is kernel-injected or
//! re-checked in the write transaction: the `plugin_id` comes from the
//! connection (never from params, the §6.2 identity rule), quotas and
//! idempotency are read inside the submit tx (a lagging projection would
//! let concurrent submits break the cap, §5.2), and withdraw's factual
//! "pending AND yours" check runs in its own write tx (§5.4 assigns it to
//! the handler, because `role_gate` is a pure function that cannot read
//! the projection).
//!
//! Adjudication (`accept` / `reject`) is deliberately absent: it is
//! user-only REST (`routes::proposals`). A plugin's only exit from its
//! own pending proposal is `withdraw`.

use serde::Deserialize;
use serde_json::{Value, json};

use calm_types::proposal::{ProposalDecision, ProposalOp};

use crate::db::sqlite::{proposal_get_tx, proposal_pending_by_idem_tx, proposal_pending_count_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::new_id;
use crate::wave_report::WaveReportPayload;
use crate::wave_report_doc::ReportDoc;
use crate::wave_report_proposal::validate_proposal_ops;

use super::callbacks::{
    CallbackCtx, entity_not_found, internal_repo_err, invalid_params_for, manifest_permissions,
    parse_params, permission_denied, quota_exceeded, rpc_conflict,
};
use super::manifest::PROPOSAL_SUBJECT_KINDS;
use super::mcp::RpcError;

/// The only subject kind implemented today (design D2: report-only, wire
/// shape unchanged when a second kind lands). Kept as a named constant so
/// the gate, the subject validator and the event payload agree.
const SUBJECT_KIND_REPORT: &str = "report";

// ---------------------------------------------------------------------------
// Quotas (§5.2 — hard, enforced at submit)
// ---------------------------------------------------------------------------

/// Max serialized size of the COMPLETE persisted proposal (§5.2: 单
/// proposal 序列化 ≤ 64 KiB).
///
/// Measured on the very `Event::ProposalSubmitted` that lands in the
/// append-only log, not on a hand-picked subset of its fields: Tier-A
/// data is never GC'd, so any plugin-controlled field left out of the
/// measurement is a disk-burning primitive (§1.1 判据 3). The numbers
/// are tunable; the ceilings are not optional.
const MAX_PROPOSAL_BYTES: usize = 64 * 1024;
/// Max ops in one proposal (§5.2). Same constant the kernel's apply-time
/// batch ceiling uses, so submit and apply cannot disagree.
const MAX_PROPOSAL_OPS: usize = crate::wave_report::MAX_BATCH_OPS;
/// Max `note` length in bytes (§5.2).
const MAX_NOTE_BYTES: usize = 4 * 1024;
/// Max `idem_key` length in bytes. Plugin-chosen and persisted verbatim
/// into the event + the projection's TEXT column, so it needs its own
/// bound — a precise error beats blowing the whole-payload ceiling with
/// an unreadable message.
const MAX_IDEM_KEY_BYTES: usize = 256;
/// Max `base_doc_heads` length in bytes. Same reasoning: it is an opaque
/// token the kernel handed out (tens of bytes in practice), but the wire
/// lets a plugin echo back anything.
const MAX_BASE_DOC_HEADS_BYTES: usize = 4 * 1024;
/// Max simultaneously-pending proposals per `(plugin, wave)` (§5.2).
/// Escape hatches when a plugin hits it: `neige.proposal.withdraw`, or
/// re-use a pending `idem_key` to recover the id and withdraw that.
const MAX_PENDING_PER_PLUGIN_WAVE: i64 = 4;

/// Sentinel prefix smuggling "this was an idempotent replay, here is the
/// original id" out of the submit transaction.
///
/// The write helper rejects an empty event batch by design (every write
/// tx must be auditable), but an idem hit must write *nothing* and still
/// answer with the original `proposal_id`. Rolling the tx back with a
/// tagged error is exactly right — nothing was written, so there is
/// nothing to commit — and the handler translates it back into success.
const IDEM_HIT: &str = "neige.proposal.submit: idempotent replay of proposal ";

/// Prefix on the pending-quota conflict, so it can be told apart from a
/// genuine DB/serialization `Conflict` coming out of the same
/// transaction. Without it every non-sentinel in-tx conflict was reported
/// to the plugin as `quota_exceeded`, which is a lie about a
/// serialization failure.
const PENDING_QUOTA: &str = "pending proposal quota: ";

// ---------------------------------------------------------------------------
// neige.report.get
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ReportGetParams {
    wave_id: String,
}

/// Read baseline for proposing: the report's blocks plus the opaque
/// `doc_heads` token a later `submit` must echo as `base_doc_heads`.
///
/// Pure read — no tx, no events. Gated by `can_propose("report")`: this
/// is the proposal channel's baseline read, not a general card read.
///
/// Note on the dormant `permissions.cards_read_all` bit (§5.2): no
/// callback consumes it today, and this method deliberately does NOT
/// wire it. Should a general card-read callback ever honour it, it would
/// bypass this gate, and that relationship must be adjudicated
/// explicitly at that point — recorded here, not pre-empted.
///
/// Related reach, also recorded rather than pre-empted: a plugin's own
/// iframe can call all three of these methods through
/// `POST /api/plugins/:id/tool-call` when some view's
/// `permissions.tools` glob matches AND `permissions.proposals` grants
/// the kind — so report content can legitimately reach the plugin's own
/// browser iframe. Attribution stays `Plugin(id)` and adjudication is
/// not a `neige.*` method, so this is the plugin acting as itself, not a
/// privilege escalation.
pub(super) async fn report_get(ctx: &CallbackCtx<'_>, params: Value) -> Result<Value, RpcError> {
    let p: ReportGetParams = parse_params("neige.report.get", &params)?;
    require_propose(ctx, SUBJECT_KIND_REPORT)?;
    let (_, card, payload) = resolve_report(ctx, &p.wave_id).await?;
    let card_id = card.id.as_str().to_string();
    let existing = ctx
        .repo
        .card_body_crdt(&card_id)
        .await
        .map_err(internal_repo_err)?;
    let mut doc = match existing {
        Some(bytes) => {
            let mut doc = ReportDoc::from_bytes(&bytes).map_err(|e| {
                RpcError::internal(format!("neige.report.get: load CRDT for {card_id}: {e}"))
            })?;
            // A pre-#960 doc has no block map; migrate the in-memory copy
            // so the snapshot has ids/revs to anchor on. The migration is
            // NOT persisted here (this is a read), which means such a
            // doc's `doc_heads` will not match the accept transaction's
            // (which migrates again, with a fresh automerge actor id).
            //
            // Consequence, stated precisely: the accept-time heads gate
            // is `ops.iter().any(needs_whole_doc_anchor)`, so ANY
            // proposal *containing* an `at_start`/`at_end` creation is
            // heads-gated — mixed proposals (a block-anchored replace
            // PLUS an at_end create) included, not only proposals whose
            // sole anchor is whole-document. On a legacy doc all of those
            // come back `stale` until some write migrates the doc for
            // real. The same applies to the no-blob legacy seed path
            // below (`ReportDoc::from_payload`), whose freshly minted
            // heads can never match the accept tx's either. Only
            // purely block-anchored proposals are unaffected, because
            // they never consult `doc_heads` at all. Fail-closed and
            // self-healing — the first real write ends it.
            doc.ensure_blocks_layout(payload.blocks.as_deref())
                .map_err(|e| {
                    RpcError::internal(format!(
                        "neige.report.get: migrate CRDT layout for {card_id}: {e}"
                    ))
                })?;
            doc
        }
        // No blob yet (legacy seed): the JSON payload is the baseline the
        // persist path would itself seed from.
        None => ReportDoc::from_payload(&payload),
    };
    let blocks = doc
        .blocks_snapshot()
        .map_err(|e| RpcError::internal(format!("neige.report.get: snapshot blocks: {e}")))?;
    let doc_heads = doc.doc_heads();
    Ok(json!({ "blocks": blocks, "doc_heads": doc_heads }))
}

// ---------------------------------------------------------------------------
// neige.proposal.submit
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ProposalSubject {
    kind: String,
    wave_id: String,
}

#[derive(Deserialize)]
struct ProposalSubmitParams {
    subject: ProposalSubject,
    base_doc_heads: String,
    ops: Vec<ProposalOp>,
    note: String,
    idem_key: String,
}

/// Open a pending proposal against a wave's report.
///
/// All of the following happen **inside one write transaction** so a
/// concurrent submit cannot slip past a quota or mint a duplicate under
/// the same idempotency key (§5.2):
///
///   1. subject exists — wave row present, wave not terminal, report card
///      present (§5.5 submit-time checkpoints);
///   2. pending-scoped idem lookup — an existing pending proposal for
///      `(plugin, wave, idem_key)` short-circuits to its id, writing
///      nothing;
///   3. pending quota for `(plugin, wave)`.
///
/// Structural op validation (§5.2.1) runs before the tx: it needs no
/// database state, and the anchoring checks that DO need state are the
/// accept transaction's authority — pre-judging them here would reject
/// proposals a human could legitimately accept later.
pub(super) async fn proposal_submit(
    ctx: &CallbackCtx<'_>,
    params: Value,
) -> Result<Value, RpcError> {
    const METHOD: &str = "neige.proposal.submit";
    let p: ProposalSubmitParams = parse_params(METHOD, &params)?;
    // `subject.kind` is both the gate key and the wire's single extension
    // point, so an unknown kind is InvalidParams (not a denial): the
    // plugin's manifest could not have granted it in the first place.
    if !PROPOSAL_SUBJECT_KINDS.contains(&p.subject.kind.as_str()) {
        return Err(invalid_params_for(
            METHOD,
            format!(
                "unknown subject.kind `{}` — supported kinds: [{}]",
                p.subject.kind,
                PROPOSAL_SUBJECT_KINDS.join(", ")
            ),
        ));
    }
    require_propose(ctx, &p.subject.kind)?;
    if p.base_doc_heads.trim().is_empty() {
        return Err(invalid_params_for(
            METHOD,
            "`base_doc_heads` is required — read it from neige.report.get",
        ));
    }
    if p.base_doc_heads.len() > MAX_BASE_DOC_HEADS_BYTES {
        return Err(quota_exceeded(format!(
            "{METHOD}: base_doc_heads is {} bytes, over the {MAX_BASE_DOC_HEADS_BYTES}-byte \
             limit — echo the token neige.report.get returned, verbatim",
            p.base_doc_heads.len()
        )));
    }
    if p.idem_key.trim().is_empty() {
        return Err(invalid_params_for(
            METHOD,
            "`idem_key` is required (pending-scoped idempotency key)",
        ));
    }
    if p.idem_key.len() > MAX_IDEM_KEY_BYTES {
        return Err(quota_exceeded(format!(
            "{METHOD}: idem_key is {} bytes, over the {MAX_IDEM_KEY_BYTES}-byte limit",
            p.idem_key.len()
        )));
    }
    if p.ops.len() > MAX_PROPOSAL_OPS {
        return Err(quota_exceeded(format!(
            "{METHOD}: {} ops exceeds the {MAX_PROPOSAL_OPS}-op limit",
            p.ops.len()
        )));
    }
    if p.note.len() > MAX_NOTE_BYTES {
        return Err(quota_exceeded(format!(
            "{METHOD}: note is {} bytes, over the {MAX_NOTE_BYTES}-byte limit",
            p.note.len()
        )));
    }
    validate_proposal_ops(&p.ops).map_err(|e| invalid_params_for(METHOD, e))?;

    let plugin_id = ctx.plugin_id.to_string();
    let wave_id = p.subject.wave_id.clone();
    // Outside-tx resolve: establishes the event scope (cove ancestry) and
    // the "wave has its report card" invariant, and shapes a friendly 404
    // before a write lock is taken. The lifecycle half is re-checked
    // in-tx below, where it is authoritative.
    let (wave, _card, _payload) = resolve_report(ctx, &wave_id).await?;
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    // `pp_` prefix per the design's wire examples; the body is the
    // same uuid shape every other kernel id uses.
    let proposal_id = format!("pp_{}", new_id());
    let correlation = ctx.correlation();
    let idem_key = p.idem_key.clone();
    let wave_row_id = wave.id.clone();
    let actor = ActorId::Plugin(plugin_id.clone());
    // Build the event BEFORE the transaction so the whole-proposal
    // ceiling can be measured on exactly the bytes that get persisted
    // (§5.2), `base_doc_heads` and `idem_key` included. Every field is
    // already determined — the transaction only decides WHETHER to
    // append it.
    let event = Event::ProposalSubmitted {
        wave_id: wave_row_id.clone(),
        proposal_id: proposal_id.clone(),
        plugin_id: plugin_id.clone(),
        subject_kind: p.subject.kind.clone(),
        base_doc_heads: p.base_doc_heads.clone(),
        ops: p.ops.clone(),
        note: p.note.clone(),
        idem_key: p.idem_key.clone(),
    };
    let serialized = serde_json::to_vec(&event)
        .map_err(|e| RpcError::internal(format!("{METHOD}: serialize proposal event: {e}")))?;
    if serialized.len() > MAX_PROPOSAL_BYTES {
        return Err(quota_exceeded(format!(
            "{METHOD}: serialized proposal is {} bytes, over the {MAX_PROPOSAL_BYTES}-byte limit",
            serialized.len()
        )));
    }

    let result = write_with_actor_events_typed::<String, _>(
        ctx.repo.as_ref(),
        correlation.as_deref(),
        ctx.event_bus.as_ref(),
        &ctx.write,
        move |tx| {
            let plugin_id = plugin_id.clone();
            let wave_row_id = wave_row_id.clone();
            let scope = scope.clone();
            let idem_key = idem_key.clone();
            let proposal_id = proposal_id.clone();
            let actor = actor.clone();
            let event = event.clone();
            Box::pin(async move {
                // §5.5 subject check, in-tx so it is authoritative: an
                // outside-tx read could be overtaken by a terminalizing
                // write, and PR-a's projection drops a terminal wave's
                // pending rows in that same transaction — so a proposal
                // minted just after it would be a permanently
                // unadjudicable row.
                let wave = crate::wave_lifecycle::wave_get_tx(tx, &wave_row_id).await?;
                if wave.lifecycle.is_terminal() {
                    return Err(CalmError::BadRequest(format!(
                        "wave {wave_row_id} is terminal (`{}`) — a terminal wave's report \
                         cannot be proposed against",
                        wave.lifecycle.as_db_str()
                    )));
                }
                // Pending-scoped dedup: a resolved proposal releases its
                // key, so only a live pending row short-circuits.
                if let Some(existing) =
                    proposal_pending_by_idem_tx(tx, &plugin_id, wave_row_id.as_str(), &idem_key)
                        .await?
                {
                    return Err(CalmError::Conflict(format!(
                        "{IDEM_HIT}{}",
                        existing.proposal_id
                    )));
                }
                let pending =
                    proposal_pending_count_tx(tx, &plugin_id, wave_row_id.as_str()).await?;
                if pending >= MAX_PENDING_PER_PLUGIN_WAVE {
                    return Err(CalmError::Conflict(format!(
                        "{PENDING_QUOTA}plugin `{plugin_id}` already has {pending} pending \
                         proposals on wave {wave_row_id} (limit \
                         {MAX_PENDING_PER_PLUGIN_WAVE}) — withdraw one first"
                    )));
                }
                Ok((proposal_id, vec![(actor, scope, event)]))
            })
        },
    )
    .await;

    // Classify by SENTINEL FIRST, then by class. The two tagged
    // conflicts this transaction can mint are recognized explicitly;
    // everything else — including a genuine SQLite serialization or
    // busy conflict — falls through to `map_write_err` and is reported
    // as the conflict it is, not mislabeled `quota_exceeded`.
    match result {
        Ok((proposal_id, _)) => Ok(json!({ "proposal_id": proposal_id })),
        // Idempotent replay: no new row, no new event, original id.
        Err(CalmError::Conflict(msg)) if msg.starts_with(IDEM_HIT) => {
            Ok(json!({ "proposal_id": &msg[IDEM_HIT.len()..] }))
        }
        Err(CalmError::Conflict(msg)) if msg.starts_with(PENDING_QUOTA) => Err(quota_exceeded(
            format!("{METHOD}: {}", &msg[PENDING_QUOTA.len()..]),
        )),
        Err(e) => Err(map_write_err(METHOD, e)),
    }
}

// ---------------------------------------------------------------------------
// neige.proposal.withdraw
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ProposalWithdrawParams {
    proposal_id: String,
}

/// Reclaim one of this plugin's own pending proposals — the only
/// plugin-side exit from `pending`, and the reason a quota cannot be
/// pinned forever by abandoned proposals (§5.6).
///
/// `role_gate` already pins "withdrawn ⇒ actor is the submitter plugin"
/// by comparing the actor against the event payload, but it is a pure
/// function of `(actor, event, scope)` and cannot read the projection.
/// The *factual* half — this proposal exists, is still pending, and
/// really belongs to this plugin — is therefore checked here, in the same
/// write transaction as the append (§5.4). Concurrent withdraw/accept:
/// first commit wins, the loser sees "no longer pending".
pub(super) async fn proposal_withdraw(
    ctx: &CallbackCtx<'_>,
    params: Value,
) -> Result<Value, RpcError> {
    const METHOD: &str = "neige.proposal.withdraw";
    let p: ProposalWithdrawParams = parse_params(METHOD, &params)?;
    // Two gates, in this order on purpose. First: a plugin with NO
    // `permissions.proposals` grant at all is off the channel entirely
    // and must not even learn whether a proposal id exists.
    require_any_propose(ctx)?;
    let plugin_id = ctx.plugin_id.to_string();
    // Pre-flight only shapes the error (404 vs 403 vs 409) and finds the
    // wave scope; the transaction below re-checks everything.
    let row = ctx
        .repo
        .proposal_get(&p.proposal_id)
        .await
        .map_err(internal_repo_err)?
        .ok_or_else(|| entity_not_found(format!("{METHOD}: proposal {}", p.proposal_id)))?;
    // Second: the kind gate is checked against the ROW's own
    // `subject_kind`, not a hardcoded one. When a second subject kind
    // lands, a plugin granted only that kind must still be able to
    // withdraw its own proposal — otherwise §5.6's quota escape hatch
    // closes for it and abandoned proposals pin the pending cap forever.
    require_propose(ctx, &row.subject_kind)?;
    if row.plugin_id != plugin_id {
        // Another plugin's proposal: a denial, not a 404 — the id is a
        // kernel-minted opaque value the caller already holds, so there
        // is nothing to hide behind an existence lie, and a plugin
        // debugging its own integration deserves the real reason.
        return Err(permission_denied(format!(
            "{METHOD}: proposal {} was submitted by plugin `{}`, not `{plugin_id}`",
            p.proposal_id, row.plugin_id
        )));
    }
    if row.status != "pending" {
        return Err(rpc_conflict(format!(
            "{METHOD}: proposal {} is already resolved (`{}`)",
            p.proposal_id, row.status
        )));
    }
    let wave = ctx
        .repo
        .wave_get(&row.wave_id)
        .await
        .map_err(internal_repo_err)?
        .ok_or_else(|| entity_not_found(format!("{METHOD}: wave {}", row.wave_id)))?;
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let correlation = ctx.correlation();
    let proposal_id = p.proposal_id.clone();
    let actor = ActorId::Plugin(plugin_id.clone());
    let wave_row_id = wave.id.clone();

    let result = write_with_actor_events_typed::<(), _>(
        ctx.repo.as_ref(),
        correlation.as_deref(),
        ctx.event_bus.as_ref(),
        &ctx.write,
        move |tx| {
            let plugin_id = plugin_id.clone();
            let proposal_id = proposal_id.clone();
            let scope = scope.clone();
            let actor = actor.clone();
            let wave_row_id = wave_row_id.clone();
            Box::pin(async move {
                let row = proposal_get_tx(tx, &proposal_id)
                    .await?
                    .ok_or_else(|| CalmError::NotFound(format!("proposal {proposal_id}")))?;
                if row.plugin_id != plugin_id {
                    return Err(CalmError::Forbidden(format!(
                        "proposal {proposal_id} belongs to plugin `{}`",
                        row.plugin_id
                    )));
                }
                if row.status != "pending" {
                    return Err(CalmError::Conflict(format!(
                        "proposal {proposal_id} is already resolved (`{}`)",
                        row.status
                    )));
                }
                let event = Event::ProposalResolved {
                    wave_id: wave_row_id,
                    proposal_id,
                    plugin_id,
                    decision: ProposalDecision::Withdrawn,
                };
                Ok(((), vec![(actor, scope, event)]))
            })
        },
    )
    .await;

    match result {
        Ok(_) => Ok(json!({})),
        Err(e) => Err(map_write_err(METHOD, e)),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The single `permissions.proposals` gate all three methods share.
fn require_propose(ctx: &CallbackCtx<'_>, subject_kind: &str) -> Result<(), RpcError> {
    let perms = manifest_permissions(ctx)?;
    if !perms.can_propose(subject_kind) {
        return Err(permission_denied(format!(
            "plugin `{}` is not granted `permissions.proposals` for subject kind \
             `{subject_kind}`",
            ctx.plugin_id
        )));
    }
    Ok(())
}

/// "Is this plugin on the proposal channel at all?" — the kind-agnostic
/// half of the gate, for `withdraw`, which must read the proposal row
/// before it knows which kind to check but must not disclose a
/// proposal's existence to a plugin with no grant whatsoever.
fn require_any_propose(ctx: &CallbackCtx<'_>) -> Result<(), RpcError> {
    let perms = manifest_permissions(ctx)?;
    if !PROPOSAL_SUBJECT_KINDS
        .iter()
        .any(|kind| perms.can_propose(kind))
    {
        return Err(permission_denied(format!(
            "plugin `{}` is not granted `permissions.proposals` for any subject kind",
            ctx.plugin_id
        )));
    }
    Ok(())
}

/// Resolve `(wave, report_card, payload)` for a wave id, mapping the
/// kernel's error classes onto RPC codes. A wave with no report card is
/// an invariant violation (every wave has exactly one), so it stays
/// Internal rather than becoming a 404.
async fn resolve_report(
    ctx: &CallbackCtx<'_>,
    wave_id: &str,
) -> Result<(crate::model::Wave, crate::model::Card, WaveReportPayload), RpcError> {
    crate::wave_report::resolve_report_for_wave(ctx.repo.as_ref(), wave_id)
        .await
        .map_err(|e| match e {
            CalmError::NotFound(what) => entity_not_found(what),
            other => internal_repo_err(other),
        })
}

/// Map a write-path `CalmError` onto the callback error vocabulary.
fn map_write_err(method: &str, e: CalmError) -> RpcError {
    match e {
        CalmError::NotFound(what) => entity_not_found(format!("{method}: {what}")),
        CalmError::Forbidden(why) => permission_denied(format!("{method}: {why}")),
        CalmError::Conflict(why) => rpc_conflict(format!("{method}: {why}")),
        CalmError::BadRequest(why) => invalid_params_for(method, why),
        other => internal_repo_err(other),
    }
}
