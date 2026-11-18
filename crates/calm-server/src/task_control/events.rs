//! Optional event batches for task control CAS transitions; nonempty batches use normal event admission.
use crate::{
    db::{RepoEventWrite, write_in_tx_typed},
    error::Result,
    event::{Event, EventScope},
    ids::ActorId,
    operation::Tx,
    state::WriteContext,
};
// Some native transitions only advance the owner CAS. Append and publish a batch only
// when a task outcome/release actually produced events; never invent a filler event.
pub(crate) async fn write_worker_events<R, F>(
    repo: &dyn RepoEventWrite,
    correlation: Option<&str>,
    bus: &crate::event::EventBus,
    write: &WriteContext,
    f: F,
) -> Result<(R, Vec<i64>)>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Tx<'_>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<(R, Vec<(ActorId, EventScope, Event)>)>>
                    + Send
                    + 'tx,
            >,
        > + Send
        + 'static,
{
    let write = write.clone();
    let correlation = correlation.map(str::to_owned);
    let (row, pending, ids) = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let (row, pending) = f(tx).await?;
            let ids = if pending.is_empty() {
                vec![]
            } else {
                crate::db::sqlite::append_actor_events_in_tx(
                    tx,
                    correlation.as_deref(),
                    &write,
                    &pending,
                )
                .await?
                .0
            };
            Ok((row, pending, ids))
        })
    })
    .await?;
    crate::db::sqlite::broadcast_actor_events(bus, &ids, pending);
    Ok((row, ids))
}
