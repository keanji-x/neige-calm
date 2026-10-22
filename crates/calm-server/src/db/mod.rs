pub use calm_truth::db::{
    Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, RouteRepo,
    SessionCardIdentity, SharedCodexDaemonRecord, SharedCodexDaemonUpdate, SharedDaemonState,
    TrackAreaMembership, TrackEvent, TranscriptRow, WorkspaceLease, WriteInTxFn,
    WriteWithActorEventsFn, WriteWithEventFn, WriteWithEventsFn, rows,
};

use futures::future::BoxFuture;
use sqlx::{Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::state::WriteContext;

pub mod prelude {
    pub use super::{Repo, RouteRepo, WorkspaceLease};
    pub use crate::session_projection_repo::WorkerSessionProjectionRepo;
    pub use calm_truth::db::RepoSyncDomainRaw;
    pub use calm_truth::session_repo::{CommitExitOutcome, SessionRepo};
}

pub use calm_truth::db::sqlite;

#[allow(clippy::too_many_arguments)]
pub async fn write_with_event_typed<R, F>(
    repo: &dyn RepoEventWrite,
    actor: ActorId,
    scope: EventScope,
    correlation: Option<&str>,
    bus: &EventBus,
    write: &WriteContext,
    f: F,
) -> Result<(R, i64)>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, std::result::Result<(R, Event), CalmError>>
        + Send
        + 'static,
{
    calm_truth::db::write_with_event_typed(repo, actor, scope, correlation, bus, write, move |tx| {
        Box::pin(async move { f(tx).await.map_err(Into::into) })
    })
    .await
    .map_err(Into::into)
}

pub async fn write_with_events_typed<R, F>(
    repo: &dyn RepoEventWrite,
    actor: ActorId,
    correlation: Option<&str>,
    bus: &EventBus,
    write: &WriteContext,
    f: F,
) -> Result<(R, Vec<i64>)>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<
            'tx,
            std::result::Result<(R, Vec<(EventScope, Event)>), CalmError>,
        > + Send
        + 'static,
{
    calm_truth::db::write_with_events_typed(repo, actor, correlation, bus, write, move |tx| {
        Box::pin(async move { f(tx).await.map_err(Into::into) })
    })
    .await
    .map_err(Into::into)
}

pub async fn write_with_actor_events_typed<R, F>(
    repo: &dyn RepoEventWrite,
    correlation: Option<&str>,
    bus: &EventBus,
    write: &WriteContext,
    f: F,
) -> Result<(R, Vec<i64>)>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<
            'tx,
            std::result::Result<(R, Vec<(ActorId, EventScope, Event)>), CalmError>,
        > + Send
        + 'static,
{
    calm_truth::db::write_with_actor_events_typed(repo, correlation, bus, write, move |tx| {
        Box::pin(async move { f(tx).await.map_err(Into::into) })
    })
    .await
    .map_err(Into::into)
}

pub async fn write_in_tx_typed<R, F>(repo: &dyn RepoEventWrite, f: F) -> Result<R>
where
    R: Send + 'static,
    F: for<'tx> FnOnce(
            &'tx mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'tx, std::result::Result<R, CalmError>>
        + Send
        + 'static,
{
    calm_truth::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { f(tx).await.map_err(Into::into) })
    })
    .await
    .map_err(Into::into)
}
