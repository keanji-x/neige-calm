//! Planner lifecycle tools; the objective host owns authorization and idempotency.
use crate::mcp_server::framing::{RpcError, calm_error};
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolRegistry, role_gated_write_annotations,
};
use crate::model::CardRole;
use calm_truth::objective_host::{self, InputSource, ObjectiveInput, PlannerRequestId, Reaction};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn register_into(registry: &mut ToolRegistry) {
    for (name, description, schema, evaluate) in [
        (
            "neige_objective_evaluate",
            include_str!("../../../prompts/tools/neige_objective_evaluate.md"),
            json!({"type":"object","additionalProperties":false,"required":["key","revision"],"properties":{"key":{"type":"string","minLength":1},"revision":{"type":"string","pattern":"^[0-9a-f]{40}$"}}}),
            true,
        ),
        (
            "neige_objective_settle",
            include_str!("../../../prompts/tools/neige_objective_settle.md"),
            json!({"type":"object","additionalProperties":false,"required":["key","evaluation","decision","note"],"properties":{"key":{"type":"string","minLength":1},"evaluation":{"type":"integer","minimum":0},"decision":{"type":"string","enum":["keep","discard","done"]},"note":{"type":"string"}}}),
            false,
        ),
    ] {
        registry.register(
            ToolDescriptor {
                name: name.into(),
                description: description.trim_end().into(),
                input_schema: schema,
                annotations: Some(role_gated_write_annotations()),
                roles: &[CardRole::Planner],
                listed_for: &[CardRole::Planner],
            },
            Arc::new(move |ctx, identity, args| {
                Box::pin(async move {
                    measure(ctx, identity, args, evaluate)
                        .await
                        .map(crate::mcp_server::result::ToolResult::structured)
                })
            }),
        );
    }
    registry.register(
        ToolDescriptor {
            name: "neige_objective_ask".into(),
            description: include_str!("../../../prompts/tools/neige_objective_ask.md").trim_end().into(),
            input_schema: json!({"type":"object", "additionalProperties":false, "required":["key"], "properties":{"key":{"type":"string", "minLength":1}}}),
            annotations: Some(role_gated_write_annotations()),
            roles: &[CardRole::Planner],
            listed_for: &[CardRole::Planner],
        },
        Arc::new(|ctx, identity, args| Box::pin(async move {
            request_approval(ctx, identity, args).await.map(crate::mcp_server::result::ToolResult::structured)
        })),
    );
    registry.register(
        ToolDescriptor {
            name: "neige_objective_close".into(),
            description: include_str!("../../../prompts/tools/neige_objective_close.md").trim_end().into(),
            input_schema: json!({"type":"object", "additionalProperties":false, "required":["key"], "properties":{"key":{"type":"string", "minLength":1}}}),
            annotations: Some(role_gated_write_annotations()),
            roles: &[CardRole::Planner],
            listed_for: &[CardRole::Planner],
        },
        Arc::new(|ctx, identity, args| Box::pin(async move {
            close(ctx, identity, args).await.map(crate::mcp_server::result::ToolResult::structured)
        })),
    );
}

async fn close(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("key must be a nonempty string"))?;
    let principal = identity
        .to_principal()
        .ok_or_else(|| RpcError::forbidden("objective close needs a track-bound Planner"))?;
    let track_id = identity
        .track_id
        .as_ref()
        .ok_or_else(|| RpcError::forbidden("objective close needs a track"))?;
    let db = ctx
        .sqlite_db
        .as_ref()
        .ok_or_else(|| RpcError::internal("objective storage unavailable"))?;
    let reaction = objective_host::react(
        db,
        ObjectiveInput {
            track_id: track_id.clone().into(),
            event: objective::Event::Closed {
                objective: objective::ids::ObjectiveKey::new(key),
            },
            source: InputSource::Planner {
                principal,
                request_id: PlannerRequestId::new(format!("close:{key}")),
            },
        },
    )
    .await
    .map_err(|error| calm_error(error.into()))?
    .into_reaction();
    Ok(match reaction {
        Reaction::Appended { seq, .. } => json!({"status":"Appended", "seq":seq}),
        Reaction::Duplicate { seq } => json!({"status":"Duplicate", "seq":seq}),
    })
}

async fn request_approval(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("key must be a nonempty string"))?;
    let key = objective::ids::ObjectiveKey::new(key);
    let call = ApprovalCall {
        principal: identity
            .to_principal()
            .ok_or_else(|| RpcError::forbidden("objective approval needs a track-bound Planner"))?,
        track: crate::ids::TrackId::from(
            identity
                .track_id
                .clone()
                .ok_or_else(|| RpcError::forbidden("objective approval needs a track"))?,
        ),
        card: crate::ids::CardId::from(identity.card_id.clone()),
        key,
    };
    // The event writer requires a nonempty batch. As with forge approval, read recorded
    // decisions first, then recheck under admission when a new ask must be appended.
    let recorded = crate::db::write_in_tx_typed(ctx.repo.as_ref(), {
        let call = call.clone();
        move |tx| {
            Box::pin(async move {
                use crate::ask::objective_approval::ObjectiveApprovalRead;
                Ok(match call.read_tx(tx).await? {
                    ObjectiveApprovalRead::Approved => Some(json!({"status":"approved"})),
                    ObjectiveApprovalRead::Awaiting(ask_id) => {
                        Some(json!({"status":"awaiting","ask_id":ask_id}))
                    }
                    ObjectiveApprovalRead::Ask(..) => None,
                })
            })
        }
    })
    .await
    .map_err(calm_error)?;
    if let Some(recorded) = recorded {
        return Ok(recorded);
    }
    // Like forge approval, persist the Planner card author that the answer proof binds.
    let actor = crate::ids::ActorId::AiPlanner(call.card.clone());
    let (_, ids) = crate::db::write_with_actor_events_typed::<(), _>(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                use crate::ask::objective_approval::ObjectiveApprovalRead;
                match call.read_tx(tx).await? {
                    ObjectiveApprovalRead::Ask(scope, event) => {
                        Ok(((), vec![(actor, scope, *event)]))
                    }
                    _ => Err(crate::error::CalmError::Conflict(
                        "the approval changed concurrently; call again".into(),
                    )),
                }
            })
        },
    )
    .await
    .map_err(calm_error)?;
    match ids.as_slice() {
        [ask_id] => Ok(json!({"status":"awaiting","ask_id":ask_id})),
        _ => Err(RpcError::internal(format!(
            "expected one objective approval ask id, got {ids:?}"
        ))),
    }
}

#[derive(Clone)]
struct ApprovalCall {
    principal: calm_truth::worker::Principal,
    track: crate::ids::TrackId,
    card: crate::ids::CardId,
    key: objective::ids::ObjectiveKey,
}
impl ApprovalCall {
    async fn read_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> crate::error::Result<crate::ask::objective_approval::ObjectiveApprovalRead> {
        calm_truth::decision_gate::PrincipalDecisionGate::new(self.principal.clone())
            .decide_card_write(tx, &self.track, &self.card, CardRole::Planner)
            .await?
            .into_result()?;
        crate::ask::objective_approval::request_tx(tx, &self.track, &self.card, &self.key).await
    }
}

/// Admission and append share writer admission, preventing a changed standard from
/// slipping an unsupported check into an accepted request.
async fn measure(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
    evaluate: bool,
) -> Result<Value, RpcError> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("key must be a nonempty string"))?;
    let principal = identity
        .to_principal()
        .ok_or_else(|| RpcError::forbidden("objective measurement needs a track-bound Planner"))?;
    let track: crate::ids::TrackId = identity
        .track_id
        .clone()
        .ok_or_else(|| RpcError::forbidden("objective measurement needs a track"))?
        .into();
    let key = objective::ids::ObjectiveKey::new(key);
    let revision = if evaluate {
        let revision = args
            .get("revision")
            .and_then(Value::as_str)
            .filter(|s| calm_types::task_execution::is_full_commit_id(s))
            .ok_or_else(|| {
                RpcError::invalid_params("revision must be 40 lowercase hexadecimal characters")
            })?;
        Some(objective::ids::Revision::new(revision))
    } else {
        None
    };
    let settlement = if !evaluate {
        let evaluation = args
            .get("evaluation")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::invalid_params("evaluation must be a nonnegative integer"))?;
        let decision = serde_json::from_value::<objective::ledger::Decision>(
            args.get("decision").cloned().unwrap_or(Value::Null),
        )
        .map_err(|_| RpcError::invalid_params("decision must be keep, discard or done"))?;
        let note = args
            .get("note")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("note must be a string"))?
            .to_owned();
        Some((
            objective::ids::EvaluationId::new(evaluation),
            decision,
            note,
        ))
    } else {
        None
    };
    let db = ctx
        .sqlite_db
        .as_ref()
        .ok_or_else(|| RpcError::internal("objective storage unavailable"))?;
    let track_for_tx = track.clone();
    let (result, dispatch, payloads) = crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| Box::pin(async move {
        use crate::error::CalmError;
        use objective::engine::Evaluation;
        // Registry roles are the first fence; persistently recheck the current Planner.
        calm_truth::decision_gate::PrincipalDecisionGate::new(principal.clone()).decide_planner(tx, &track_for_tx).await?.into_result()?;
        let input = |event, request_id| ObjectiveInput {
            track_id: track_for_tx.clone(), event,
            source: InputSource::Planner { principal: principal.clone(), request_id: PlannerRequestId::new(request_id) },
        };
        let input = if let Some(revision) = revision {
            let (_, state) = objective_host::load_state_tx(tx, &track_for_tx).await?;
            let o = state.objectives.get(&key).ok_or(CalmError::NotFound("objective does not exist in this track; read objective.kr and choose an existing key".into()))?;
            if !o.is_approved() { return Err(CalmError::Conflict("approval_required: call neige_objective_ask".into())); }
            if o.status != objective::engine::Status::Open { return Err(CalmError::Conflict("objective is not open; read objective.kr and choose an open objective".into())); }
            if !o.definition.checks.iter().all(|c| crate::objective_dispatch::supports(&c.config)) {
                return Err(CalmError::Conflict("objective contains a check without an executor; supported: fact TaskAccepted, AskAnswered, evaluator, command".into()));
            }
            let event = objective::Event::Requested { objective: key.clone(), revision: revision.clone() };
            if let Some(current) = o.evaluation.as_ref().filter(|e| e.revision() == &revision) {
                // Reuse this round's identity, never the next round's counter. The host
                // still checks the selected session before returning existing state.
                let replay = objective_host::replay_request_tx(tx, &input(event, format!("evaluate:{key}:{}:{revision}", current.id()))).await?;
                let payloads = if let Some(Reaction::Duplicate {seq}) = replay {
                    crate::objective_dispatch::evaluator::bind_request_tx(tx,&track_for_tx,seq).await?
                } else if let Some(seq) = objective_host::check_jobs::evaluation_request_tx(tx,&track_for_tx,&key,current.id()).await? {
                    crate::objective_dispatch::evaluator::bind_request_tx(tx,&track_for_tx,seq).await?
                } else {vec![]};
                let status = match current { Evaluation::Running { .. } => "Running", Evaluation::Ready { .. } => "Ready" };
                return Ok((json!({"status":status, "evaluation":current.id()}), true, payloads));
            }
            input(event, format!("evaluate:{key}:{}:{revision}", o.evaluations))
        } else {
            let (evaluation, decision, note) = settlement.unwrap();
            let input = input(objective::Event::Settled { objective: key.clone(), evaluation, decision, note }, format!("settle:{key}:{evaluation}"));
            // A settled retry remains valid after state/approval changes. A conflicting
            // payload must conflict before the new-request admission below.
            if let Some(Reaction::Duplicate { seq }) = objective_host::replay_request_tx(tx, &input).await? {
                return Ok((json!({"status":"Duplicate", "seq":seq}), false, vec![]));
            }
            let (_, state) = objective_host::load_state_tx(tx, &track_for_tx).await?;
            let o = state.objectives.get(&key).ok_or(CalmError::NotFound("objective does not exist in this track; read objective.kr and choose an existing key".into()))?;
            if !o.is_approved() { return Err(CalmError::Conflict("approval_required: approval is absent or invalidated; call neige_objective_ask".into())); }
            if !matches!(&o.evaluation, Some(Evaluation::Ready { id, .. }) if *id == evaluation) {
                let current = match &o.evaluation {
                    Some(Evaluation::Running { id, .. }) => format!("Running evaluation {id}"),
                    Some(Evaluation::Ready { id, .. }) => format!("Ready evaluation {id}"),
                    None => "no current evaluation".into(),
                };
                return Err(CalmError::Conflict(format!("objective {key} has {current}, not Ready evaluation {evaluation}; read objective.kr, wait for \
            EvaluationReady or call neige_objective_evaluate to start an evaluation, then settle the \
            current Ready id")));
            }
            input
        };
        let reaction = objective_host::react_tx(tx, &input, crate::model::now_ms()).await?;
        let seq = match &reaction { Reaction::Appended {seq,..} | Reaction::Duplicate {seq} => *seq };
        let payloads = if evaluate { crate::objective_dispatch::evaluator::bind_request_tx(tx,&track_for_tx,seq).await? } else {vec![]};
        let mut result = match reaction {
            Reaction::Appended { seq, .. } => json!({"status":"Appended", "seq":seq}),
            Reaction::Duplicate { seq } => json!({"status":"Duplicate", "seq":seq}),
        };
        if evaluate && let Some(evaluation) = objective_host::load_state_tx(tx, &track_for_tx).await?.1.objectives.get(&key).and_then(|o| o.evaluation.as_ref()).map(|e| e.id()) {
            result["evaluation"] = json!(evaluation);
        }
        Ok::<_, CalmError>((result, true, payloads))
    })).await.map_err(calm_error)?;
    crate::objective_dispatch::evaluator::declare(&ctx, &identity, &track, payloads)
        .await
        .map_err(calm_error)?;
    // Persisted requests remain recoverable by the projector if immediate dispatch fails.
    if dispatch
        && let Err(error) = crate::objective_dispatch::dispatch_track(
            ctx.repo.as_ref(),
            db,
            &ctx.events,
            track.as_str(),
            ctx.operation_runtime.get().map(AsRef::as_ref),
        )
        .await
    {
        tracing::warn!(%track, %error, "objective dispatch deferred to reconciliation");
    }
    Ok(result)
}
