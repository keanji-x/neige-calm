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
