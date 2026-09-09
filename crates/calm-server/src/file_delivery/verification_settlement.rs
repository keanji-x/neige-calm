//! Durable verification wake; publication identity is never substituted.
use super::*;
use crate::{
    db::{RepoEventWrite, write_in_tx_typed, write_with_actor_events_typed},
    event::{Event, EventBus, EventScope},
    harness::Observation,
    ids::{ActorId, TrackId},
    operation::Tx,
    state::WriteContext,
};
async fn terminal_tx(tx: &mut Tx<'_>, id: &str) -> Result<Option<(candidate::Candidate, String)>> {
    let row: Option<(String,String)> = sqlx::query_as("SELECT payload_json,phase FROM operations WHERE id=?1 AND kind='candidate-verify' AND phase IN ('succeeded','failed','stuck')")
        .bind(id).fetch_optional(&mut **tx).await?;
    let Some((payload, phase)) = row else {
        return Ok(None);
    };
    let payload: candidate_verify::Payload = serde_json::from_str(&payload)?;
    Ok(Some((
        candidate::load_tx(tx, &payload.publication_operation_id).await?,
        phase,
    )))
}
// No-event replay rolls back through the domain sentinel, preserving the shared
// event writer's nonempty-batch invariant. The boolean reports a new notice only.
const ALREADY_RECORDED: &str = "candidate verification settlement already recorded";

pub(crate) async fn record(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    write: &WriteContext,
    id: &str,
) -> Result<bool> {
    let id = id.to_owned();
    let result = write_with_actor_events_typed(repo,None,events,write,move |tx| Box::pin(async move {
        let Some((candidate,_)) = terminal_tx(tx,&id).await? else { return Err(conflict("candidate verification has not settled")); };
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE kind='task.candidate_verification_settled' AND json_extract(payload,'$.operation_id')=?1)").bind(&id).fetch_one(&mut **tx).await?;
        if exists { return Err(conflict(ALREADY_RECORDED)); }
        let track = crate::track_lifecycle::track_get_tx(tx,&candidate.source.track_id.clone().into()).await?;
        Ok(((),vec![(ActorId::KernelDispatcher,EventScope::Track { track:track.id,area:track.area_id },Event::TaskCandidateVerificationSettled { task_id:candidate.source.task_id,operation_id:id })]))
    })).await;
    match result {
        Ok(_) => Ok(true),
        Err(CalmError::Conflict(reason)) if reason == ALREADY_RECORDED => Ok(false),
        Err(error) => Err(error),
    }
}
pub(crate) async fn observation(
    repo: &dyn RepoEventWrite,
    track: &TrackId,
    task: &str,
    id: &str,
) -> Result<Option<Observation>> {
    let track = track.clone();
    let task = task.to_owned();
    let id = id.to_owned();
    write_in_tx_typed(repo,move |tx| Box::pin(async move {
        let Some((candidate,phase)) = terminal_tx(tx,&id).await? else { return Ok(None); };
        if candidate.source.track_id != track.as_str() || candidate.source.task_id != task { return Ok(None); }
        let Some(task) = crate::db::sqlite::task_get_tx(tx,&task).await? else { return Ok(None); };
        if crate::db::sqlite::task_attempt_current_tx(tx,&task.track_id,&task.key).await?.is_none_or(|a| a.attempt_id != task.id) { return Ok(None); }
        let view = view_tx(tx,&task).await?;
        Ok(Some(Observation::SystemContext { text:format!("Candidate verification for task `{}` settled with Operation state `{phase}`. Read calm.plan.list for exact candidate, check result and consumer preparation. Operation completion alone does not mean checks passed; only matching successful evidence qualifies delivery. Scope is declared check exit status, not full test coverage or semantic review. Current delivery: {view}",task.key) }))
    })).await
}
