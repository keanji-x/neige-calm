//! Durable Planner wake backed by the exact terminal publication Operation.
use super::*;
use crate::{
    db::{RepoEventWrite, write_in_tx_typed, write_with_actor_events_typed},
    event::{Event, EventBus, EventScope},
    harness::Observation,
    ids::{ActorId, TrackId},
    operation::Tx,
    state::WriteContext,
};
use serde_json::Value;

async fn terminal_tx(tx: &mut Tx<'_>, op_id: &str) -> Result<Option<(PublicationPayload, String)>> {
    let row: Option<(String,String)> = sqlx::query_as("SELECT payload_json,phase FROM operations WHERE id=?1 AND kind='task-file-publication' AND phase IN ('succeeded','failed','stuck')")
        .bind(op_id).fetch_optional(&mut **tx).await?;
    row.map(|(payload, phase)| Ok((serde_json::from_str(&payload)?, phase)))
        .transpose()
}
pub(crate) async fn record(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    write: &WriteContext,
    op_id: &str,
) -> Result<()> {
    let op_id = op_id.to_owned();
    write_with_actor_events_typed(repo, None, events, write, move |tx| Box::pin(async move {
        let Some((payload, _)) = terminal_tx(tx, &op_id).await? else { return Err(conflict("publication has not settled")) };
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE kind='task.file_publication_settled' AND json_extract(payload,'$.operation_id')=?1)")
            .bind(&op_id).fetch_one(&mut **tx).await?;
        if exists { return Ok(((), vec![])) }
        let track = crate::track_lifecycle::track_get_tx(tx, &payload.track_id.clone().into()).await?;
        let event = Event::TaskFilePublicationSettled { task_id: payload.task_id, operation_id: op_id };
        Ok(((), vec![(ActorId::KernelDispatcher, EventScope::Track { track: track.id, area: track.area_id }, event)]))
    })).await.map(|_| ())
}
/// Revalidate the event's retained identity for live pushes and missed-event replay.
pub(crate) async fn observation(
    repo: &dyn RepoEventWrite,
    track: &TrackId,
    task_id: &str,
    op_id: &str,
) -> Result<Option<Observation>> {
    let track = track.clone();
    let task_id = task_id.to_owned();
    let op_id = op_id.to_owned();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let Some((payload, phase)) = terminal_tx(tx, &op_id).await? else { return Ok(None) };
        if payload.task_id != task_id || payload.track_id != track.as_str() { return Ok(None) }
        let Some(task) = crate::db::sqlite::task_get_tx(tx, &task_id).await? else { return Ok(None) };
        let current = crate::db::sqlite::task_attempt_current_tx(tx, &task.track_id, &task.key).await?;
        if task.track_id != track.as_str() || current.is_none_or(|current| current.attempt_id != task_id) { return Ok(None) }
        let view: Value = view_tx(tx, &task).await?;
        if matches!(selection(&task)?,Some(FileDelivery::CandidateProducer { .. })) {
            return Ok(Some(Observation::SystemContext { text: format!("Candidate file publication for task `{}` ({task_id}) settled with Operation state `{phase}`. Sealing is not machine verification or delivery qualification. Read calm.plan.list for separate candidate verification and input state. Current file delivery: {view}",task.key) }));
        }
        Ok(Some(Observation::SystemContext {
            text: format!("JSON file publication for task `{}` ({task_id}) settled with Operation state `{phase}`. Read calm.plan.list for exact publication and input preparation state. JSON syntax success is not business acceptance; publication failure does not authorize automatic retry. Current file delivery: {view}", task.key),
        }))
    })).await
}
