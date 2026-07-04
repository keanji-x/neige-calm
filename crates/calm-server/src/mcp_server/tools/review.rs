//! Review/ratify workflow tools for issue #760 slice 5b.
//!
//! `calm.review.round` records the spec's dual-channel review round as a
//! typed wave-scoped event. The event log is the durable store, so the tool
//! enforces a strict monotonic round number per logical subject before
//! appending.
//!
//! `calm.ratify.request` is the spec-authored half of the human ratify gate:
//! it records the request and parks a working wave in `blocked` in the same
//! eventized transaction.

use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{ChannelVerdict, ChannelVerdictKind, Event, EventScope, ReviewSubject};
use crate::ids::WaveId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::{CardRole, Wave, WaveLifecycle};
use crate::ratify_state::ratify_request_pending_tx;
use crate::wave_lifecycle::apply_requested_transition_in_tx;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::HashSet;
use std::sync::Arc;

pub const TOOL_REVIEW_ROUND: &str = "calm.review.round";
pub const TOOL_RATIFY_REQUEST: &str = "calm.ratify.request";

const FIRST_REVIEW_ROUND_N: u32 = 1;
const REVIEW_ROUND_DUPLICATE_RACE: &str = "__review_round_duplicate_race__";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(review_round_descriptor(), wrap(review_round));
    registry.register(ratify_request_descriptor(), wrap(ratify_request));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn review_round_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REVIEW_ROUND.into(),
        description: "Spec-only: record one dual-channel review round for a \
             logical subject. Requires at least two channel verdicts, `n <= cap`, \
             and when `converged=true` every channel verdict must be `approved`. \
             Round numbers are strict-monotonic per subject starting at 1; an \
             exact retry of an already-recorded round is a no-op."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["subject", "n", "cap", "converged", "channels"],
            "properties": {
                "subject": {
                    "type": "object",
                    "required": ["phase", "slice_id"],
                    "properties": {
                        "phase": { "type": "string", "minLength": 1 },
                        "slice_id": { "type": "string", "minLength": 1 },
                        "pr_number": { "type": "integer", "minimum": 0 }
                    }
                },
                "head_sha": { "type": "string" },
                "n": { "type": "integer", "minimum": 1 },
                "cap": { "type": "integer", "minimum": 1 },
                "converged": { "type": "boolean" },
                "channels": {
                    "type": "array",
                    "minItems": 2,
                    "items": {
                        "type": "object",
                        "required": ["role", "verdict"],
                        "properties": {
                            "role": { "type": "string", "minLength": 1 },
                            "verdict": { "enum": ["approved", "changes_requested"] }
                        }
                    }
                },
                "root_cause": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

fn ratify_request_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_RATIFY_REQUEST.into(),
        description: "Spec-only: request human ratification for the current \
             wave. Emits `ratify.requested` and applies `working -> blocked` \
             in the same atomic write. The spec must perform any preceding \
             `reviewing -> working` transition separately."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["reason"],
            "properties": {
                "reason": { "type": "string", "minLength": 1 }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ReviewRoundArgs {
    subject: ReviewSubject,
    head_sha: Option<String>,
    n: u32,
    cap: u32,
    converged: bool,
    channels: Vec<ChannelVerdict>,
    root_cause: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RatifyRequestArgs {
    reason: String,
}

async fn review_round(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let args: ReviewRoundArgs = serde_json::from_value(args)
        .map_err(|e| RpcError::invalid_params(format!("review_round: invalid args: {e}")))?;
    validate_review_round_args(&args)?;

    let (_card, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let idempotency_key = review_round_idempotency_key(&wave.id, &args.subject, args.n);

    let prior = review_rounds_for_subject(ctx.repo.as_ref(), &wave.id, &args.subject)
        .await
        .map_err(|e| RpcError::internal(format!("review_round: query prior rounds: {e}")))?;
    match classify_review_round(&prior, &args, &idempotency_key) {
        ReviewRoundWriteDecision::DuplicateSame => {
            return Ok(json!({ "ok": true, "emitted": false }));
        }
        ReviewRoundWriteDecision::Reject(message) => {
            return Err(RpcError::invalid_params(message));
        }
        ReviewRoundWriteDecision::Append => {}
    }

    let actor = identity.to_actor_id();
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_id = wave.id.clone();
    let subject = args.subject.clone();
    let args_for_tx = args.clone();
    let idempotency_key_for_tx = idempotency_key.clone();

    let result =
        write_with_actor_events_typed::<(), _>(ctx.repo.as_ref(), None, &ctx.events, &ctx.write, {
            let wave_id = wave_id.clone();
            move |tx| {
                let actor = actor.clone();
                let scope = scope.clone();
                let wave_id = wave_id.clone();
                let subject = subject.clone();
                let args = args_for_tx.clone();
                let idempotency_key = idempotency_key_for_tx.clone();
                Box::pin(async move {
                    let prior = review_rounds_for_subject_tx(tx, &wave_id, &subject).await?;
                    match classify_review_round(&prior, &args, &idempotency_key) {
                        ReviewRoundWriteDecision::DuplicateSame => {
                            return Err(CalmError::Conflict(
                                REVIEW_ROUND_DUPLICATE_RACE.to_string(),
                            ));
                        }
                        ReviewRoundWriteDecision::Reject(message) => {
                            return Err(CalmError::BadRequest(message));
                        }
                        ReviewRoundWriteDecision::Append => {}
                    }
                    let event = Event::ReviewRound {
                        wave_id,
                        subject: args.subject,
                        head_sha: args.head_sha,
                        n: args.n,
                        cap: args.cap,
                        converged: args.converged,
                        channels: args.channels,
                        root_cause: args.root_cause,
                        idempotency_key,
                    };
                    Ok(((), vec![(actor, scope, event)]))
                })
            }
        })
        .await;

    match result {
        Ok((_unit, _ids)) => Ok(json!({ "ok": true, "emitted": true })),
        Err(CalmError::Conflict(msg)) if msg == REVIEW_ROUND_DUPLICATE_RACE => {
            Ok(json!({ "ok": true, "emitted": false }))
        }
        Err(CalmError::BadRequest(msg)) => Err(RpcError::invalid_params(msg)),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("review_round: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("review_round: {e}"))),
    }
}

async fn ratify_request(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let args: RatifyRequestArgs = serde_json::from_value(args)
        .map_err(|e| RpcError::invalid_params(format!("ratify_request: invalid args: {e}")))?;
    if args.reason.trim().is_empty() {
        return Err(RpcError::invalid_params(
            "ratify_request: reason must not be empty",
        ));
    }

    let (_card, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let actor = identity.to_actor_id();
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_id = wave.id.clone();
    let reason = args.reason;

    let result =
        write_with_actor_events_typed::<(), _>(ctx.repo.as_ref(), None, &ctx.events, &ctx.write, {
            move |tx| {
                let actor = actor.clone();
                let scope = scope.clone();
                let wave_id = wave_id.clone();
                let reason = reason.clone();
                Box::pin(async move {
                    let mut events = Vec::new();
                    let lifecycle = wave_lifecycle_in_tx(tx, &wave_id).await?;
                    let pending = ratify_request_pending_tx(tx, &wave_id).await?;
                    if lifecycle != WaveLifecycle::Working || pending {
                        return Err(CalmError::BadRequest(
                            "ratify_request: wave is not in `working` or a ratify request is already pending"
                                .into(),
                        ));
                    }

                    let lifecycle_events = apply_requested_transition_in_tx(
                        tx,
                        &wave_id,
                        WaveLifecycle::Blocked,
                        &actor,
                        reason.clone(),
                    )
                    .await?
                    .ok_or_else(|| {
                        CalmError::BadRequest(
                            "ratify_request: wave is not in `working` or a ratify request is already pending"
                                .into(),
                        )
                    })?;
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), scope.clone(), event)),
                    );
                    events.push((actor, scope, Event::RatifyRequested { wave_id, reason }));
                    Ok(((), events))
                })
            }
        })
        .await;

    match result {
        Ok((_unit, _ids)) => Ok(json!({ "ok": true })),
        Err(CalmError::BadRequest(msg)) => Err(RpcError::invalid_params(msg)),
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("ratify_request: forbidden: {msg}"),
        )),
        Err(e) => Err(RpcError::internal(format!("ratify_request: {e}"))),
    }
}

fn validate_review_round_args(args: &ReviewRoundArgs) -> Result<(), RpcError> {
    if args.subject.phase.trim().is_empty() {
        return Err(RpcError::invalid_params(
            "review_round: subject.phase must not be empty",
        ));
    }
    if args.subject.slice_id.trim().is_empty() {
        return Err(RpcError::invalid_params(
            "review_round: subject.slice_id must not be empty",
        ));
    }
    if args.n > args.cap {
        return Err(RpcError::invalid_params(format!(
            "review_round: n ({}) must be <= cap ({})",
            args.n, args.cap
        )));
    }
    if args.channels.len() < 2 {
        return Err(RpcError::invalid_params(
            "review_round: at least two channel verdicts are required",
        ));
    }
    if args.channels.iter().any(|c| c.role.trim().is_empty()) {
        return Err(RpcError::invalid_params(
            "review_round: channel role must not be empty",
        ));
    }
    let distinct_roles = args
        .channels
        .iter()
        .map(|c| c.role.trim())
        .collect::<HashSet<_>>();
    if distinct_roles.len() != args.channels.len() {
        return Err(RpcError::invalid_params(
            "review_round: channel roles must be distinct (two independent reviewers required)",
        ));
    }
    if args.converged && !args.channels.iter().all(is_approving_channel) {
        return Err(RpcError::invalid_params(
            "review_round: converged=true requires every channel verdict to be approved",
        ));
    }
    Ok(())
}

fn is_approving_channel(channel: &ChannelVerdict) -> bool {
    channel.verdict == ChannelVerdictKind::Approved
}

fn review_round_idempotency_key(wave_id: &WaveId, subject: &ReviewSubject, n: u32) -> String {
    let pr = subject
        .pr_number
        .map(|n| n.to_string())
        .unwrap_or_else(|| "design".to_string());
    format!(
        "review.round:{}:{}:{}:{}:{}",
        wave_id.as_str(),
        subject.phase,
        subject.slice_id,
        pr,
        n
    )
}

#[derive(Debug, PartialEq, Eq)]
enum ReviewRoundWriteDecision {
    DuplicateSame,
    Append,
    Reject(String),
}

fn classify_review_round(
    prior: &[Event],
    args: &ReviewRoundArgs,
    idempotency_key: &str,
) -> ReviewRoundWriteDecision {
    let mut max_n: Option<u32> = None;
    let mut same_n_same_payload = false;
    for event in prior {
        let Event::ReviewRound {
            n,
            cap,
            converged,
            head_sha,
            channels,
            root_cause,
            idempotency_key: existing_idempotency_key,
            ..
        } = event
        else {
            continue;
        };
        max_n = Some(max_n.map_or(*n, |max| max.max(*n)));
        if *n == args.n
            && *cap == args.cap
            && *converged == args.converged
            && head_sha == &args.head_sha
            && channels == &args.channels
            && root_cause == &args.root_cause
            && existing_idempotency_key == idempotency_key
        {
            same_n_same_payload = true;
        }
    }

    if same_n_same_payload {
        return ReviewRoundWriteDecision::DuplicateSame;
    }

    let expected = max_n.map_or(FIRST_REVIEW_ROUND_N, |n| n.saturating_add(1));
    if args.n != expected {
        return ReviewRoundWriteDecision::Reject(format!(
            "review_round: stale/out-of-order round for subject phase={} slice_id={} pr_number={:?}: got n={}, expected n={expected}",
            args.subject.phase, args.subject.slice_id, args.subject.pr_number, args.n,
        ));
    }

    ReviewRoundWriteDecision::Append
}

async fn review_rounds_for_subject(
    repo: &dyn crate::db::RouteRepo,
    wave_id: &WaveId,
    subject: &ReviewSubject,
) -> Result<Vec<Event>, CalmError> {
    let rows = repo
        .events_for_wave(wave_id.as_str(), &["review.round"], None)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| review_round_for_subject(row.event, subject))
        .collect())
}

async fn review_rounds_for_subject_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    subject: &ReviewSubject,
) -> Result<Vec<Event>, CalmError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT payload FROM events WHERE scope_wave = ?1 AND kind = 'review.round' ORDER BY id ASC",
    )
    .bind(wave_id.as_str())
    .fetch_all(&mut **tx)
    .await?;

    let mut out = Vec::new();
    for (payload_text,) in rows {
        let payload: Value = serde_json::from_str(&payload_text)?;
        let event = Event::from_kind_and_payload("review.round", payload)?;
        if let Some(event) = review_round_for_subject(event, subject) {
            out.push(event);
        }
    }
    Ok(out)
}

fn review_round_for_subject(event: Event, subject: &ReviewSubject) -> Option<Event> {
    match &event {
        Event::ReviewRound {
            subject: event_subject,
            ..
        } if event_subject == subject => Some(event),
        _ => None,
    }
}

async fn wave_lifecycle_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<WaveLifecycle, CalmError> {
    let lifecycle = sqlx::query_scalar::<_, String>("SELECT lifecycle FROM waves WHERE id = ?1")
        .bind(wave_id.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {}", wave_id.as_str())))?;
    WaveLifecycle::try_from(lifecycle)
        .map_err(|e| CalmError::Internal(format!("waves.lifecycle decode: {e}")))
}

async fn resolve_wave_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(crate::model::Card, Wave), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("review: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "review: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("review: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "review: wave {} for card {} not found",
                card.wave_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, wave))
}
