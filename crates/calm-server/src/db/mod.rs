pub use calm_truth::db::{
    Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, RouteRepo,
    SessionCardIdentity, SharedCodexDaemonRecord, SharedCodexDaemonUpdate, SharedDaemonState,
    TrackAreaMembership, TrackEvent, TranscriptRow, WorkspaceLease, WriteInTxFn,
    WriteWithActorEventsFn, WriteWithEventFn, WriteWithEventsFn, rows,
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use sqlx::{Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::*;
use crate::state::WriteContext;
use crate::{card_role_cache::CardRoleCache, track_area_cache::TrackAreaCache};
use calm_types::worker::{WorkerSession, WorkerSessionId};

pub mod prelude {
    pub use super::{Repo, RouteRepo, ServerRepoReadExt, WorkspaceLease};
    pub use crate::session_projection_repo::WorkerSessionProjectionRepo;
    pub use calm_truth::db::RepoSyncDomainRaw;
    pub use calm_truth::session_repo::{CommitExitOutcome, SessionRepo};
}

#[async_trait]
pub trait ServerRepoReadExt {
    /// Current registry/membership reads; internal global readers remain separate.
    async fn application_user_require_registered(&self, user_id: &str) -> Result<()>;
    async fn track_area_membership(
        &self,
        track_id: &str,
        user_id: &str,
    ) -> Result<Option<TrackAreaMembership>>;
    async fn areas_list_for_member(&self, user_id: &str) -> Result<Vec<Area>>;
    async fn area_require_access(
        &self,
        area_id: &str,
        user_id: &str,
        access: calm_types::organization::AreaAccess,
    ) -> Result<Area>;

    async fn areas_list(&self) -> Result<Vec<Area>>;
    async fn areas_list_user_visible(&self) -> Result<Vec<Area>>;
    async fn area_get(&self, id: &str) -> Result<Option<Area>>;
    async fn area_get_system(&self) -> Result<Option<Area>>;
    async fn area_folders_by_area(&self, area_id: &str) -> Result<Vec<AreaFolder>>;
    async fn area_folders_list_all(&self) -> Result<Vec<AreaFolder>>;
    async fn area_folder_get(&self, id: i64) -> Result<Option<AreaFolder>>;
    async fn tracks_by_area(&self, area_id: &str) -> Result<Vec<Track>>;
    async fn track_get(&self, id: &str) -> Result<Option<Track>>;
    /// The Today launchpad track, or `None` before it exists.
    async fn track_get_launchpad(&self) -> Result<Option<Track>>;
    async fn track_detail(&self, id: &str) -> Result<Option<TrackDetail>>;
    async fn tracks_window(
        &self,
        area_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Track>>;
    async fn tasks_by_track(&self, track_id: &str) -> Result<Vec<Task>>;
    async fn task_get(&self, id: &str) -> Result<Option<Task>>;
    async fn task_current_get(&self, track_id: &str, key: &str) -> Result<Option<Task>>;
    async fn task_for_worker_card(&self, card_id: &str) -> Result<Option<Task>>;
    async fn task_history_by_key(&self, track_id: &str, key: &str) -> Result<Vec<Task>>;
    async fn tasks_nonterminal(&self) -> Result<Vec<Task>>;
    async fn task_contexts_by_dst_track(
        &self,
        dst_track_id: &str,
    ) -> Result<Vec<calm_truth::db::TaskContextRow>>;
    async fn stale_task_contexts_by_dst_track(
        &self,
        dst_track_id: &str,
    ) -> Result<Vec<calm_truth::db::TaskContextRow>>;
    async fn task_contexts_inflight_fresh(&self) -> Result<Vec<calm_truth::db::TaskContextRow>>;
    async fn task_contexts_inflight_stale(&self) -> Result<Vec<calm_truth::db::TaskContextRow>>;
    async fn cards_by_track(&self, track_id: &str) -> Result<Vec<Card>>;
    async fn track_report_cards_by_area(&self, area_id: &str) -> Result<Vec<Card>>;
    async fn card_get(&self, id: &str) -> Result<Option<Card>>;
    async fn card_get_with_body_crdt(&self, id: &str) -> Result<Option<(Card, Option<Vec<u8>>)>>;
    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>>;
    async fn harness_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>>;
    async fn harness_item_list_transcript_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>>;
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>>;
    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>>;
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>>;
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>>;
    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>>;
    async fn terminals_running(&self) -> Result<Vec<Terminal>>;
    async fn shared_planner_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>>;
    async fn plugins_list(&self) -> Result<Vec<Plugin>>;
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>>;
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>>;
    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>>;
    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>>;
    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>>;
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>>;
    async fn seed_card_role_cache(&self, cache: &CardRoleCache) -> Result<()>;
    async fn seed_track_area_cache(&self, cache: &TrackAreaCache) -> Result<()>;
    async fn card_mcp_token_lookup_by_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<(String, String)>>;
    async fn card_identity_get_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionCardIdentity>>;
    async fn session_get_by_active_token_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<WorkerSession>>;
    async fn session_get_by_id(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>>;
    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord>;
}

#[async_trait]
impl<T> ServerRepoReadExt for T
where
    T: calm_truth::db::RepoRead + ?Sized,
{
    async fn application_user_require_registered(&self, user_id: &str) -> Result<()> {
        calm_truth::db::RepoRead::application_user_require_registered(self, user_id)
            .await
            .map_err(Into::into)
    }
    async fn track_area_membership(
        &self,
        track_id: &str,
        user_id: &str,
    ) -> Result<Option<TrackAreaMembership>> {
        calm_truth::db::RepoRead::track_area_membership(self, track_id, user_id)
            .await
            .map_err(Into::into)
    }
    async fn areas_list_for_member(&self, user_id: &str) -> Result<Vec<Area>> {
        calm_truth::db::RepoRead::areas_list_for_member(self, user_id)
            .await
            .map_err(Into::into)
    }
    async fn area_require_access(
        &self,
        area_id: &str,
        user_id: &str,
        access: calm_types::organization::AreaAccess,
    ) -> Result<Area> {
        calm_truth::db::RepoRead::area_require_access(self, area_id, user_id, access)
            .await
            .map_err(Into::into)
    }

    async fn areas_list(&self) -> Result<Vec<Area>> {
        calm_truth::db::RepoRead::areas_list(self)
            .await
            .map_err(Into::into)
    }
    async fn areas_list_user_visible(&self) -> Result<Vec<Area>> {
        calm_truth::db::RepoRead::areas_list_user_visible(self)
            .await
            .map_err(Into::into)
    }
    async fn area_get(&self, id: &str) -> Result<Option<Area>> {
        calm_truth::db::RepoRead::area_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn area_get_system(&self) -> Result<Option<Area>> {
        calm_truth::db::RepoRead::area_get_system(self)
            .await
            .map_err(Into::into)
    }
    async fn area_folders_by_area(&self, area_id: &str) -> Result<Vec<AreaFolder>> {
        calm_truth::db::RepoRead::area_folders_by_area(self, area_id)
            .await
            .map_err(Into::into)
    }
    async fn area_folders_list_all(&self) -> Result<Vec<AreaFolder>> {
        calm_truth::db::RepoRead::area_folders_list_all(self)
            .await
            .map_err(Into::into)
    }
    async fn area_folder_get(&self, id: i64) -> Result<Option<AreaFolder>> {
        calm_truth::db::RepoRead::area_folder_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn tracks_by_area(&self, area_id: &str) -> Result<Vec<Track>> {
        calm_truth::db::RepoRead::tracks_by_area(self, area_id)
            .await
            .map_err(Into::into)
    }
    async fn track_get(&self, id: &str) -> Result<Option<Track>> {
        calm_truth::db::RepoRead::track_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn track_get_launchpad(&self) -> Result<Option<Track>> {
        calm_truth::db::RepoRead::track_get_launchpad(self)
            .await
            .map_err(Into::into)
    }
    async fn track_detail(&self, id: &str) -> Result<Option<TrackDetail>> {
        calm_truth::db::RepoRead::track_detail(self, id)
            .await
            .map_err(Into::into)
    }
    async fn tracks_window(
        &self,
        area_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Track>> {
        calm_truth::db::RepoRead::tracks_window(self, area_id, since, until)
            .await
            .map_err(Into::into)
    }
    async fn tasks_by_track(&self, track_id: &str) -> Result<Vec<Task>> {
        calm_truth::db::RepoRead::tasks_by_track(self, track_id)
            .await
            .map_err(Into::into)
    }
    async fn task_get(&self, id: &str) -> Result<Option<Task>> {
        calm_truth::db::RepoRead::task_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn task_for_worker_card(&self, card_id: &str) -> Result<Option<Task>> {
        calm_truth::db::RepoRead::task_for_worker_card(self, card_id)
            .await
            .map_err(Into::into)
    }

    async fn task_current_get(&self, track_id: &str, key: &str) -> Result<Option<Task>> {
        calm_truth::db::RepoRead::task_current_get(self, track_id, key)
            .await
            .map_err(Into::into)
    }
    async fn task_history_by_key(&self, track_id: &str, key: &str) -> Result<Vec<Task>> {
        calm_truth::db::RepoRead::task_history_by_key(self, track_id, key)
            .await
            .map_err(Into::into)
    }
    async fn tasks_nonterminal(&self) -> Result<Vec<Task>> {
        calm_truth::db::RepoRead::tasks_nonterminal(self)
            .await
            .map_err(Into::into)
    }
    async fn task_contexts_by_dst_track(
        &self,
        dst_track_id: &str,
    ) -> Result<Vec<calm_truth::db::TaskContextRow>> {
        calm_truth::db::RepoRead::task_contexts_by_dst_track(self, dst_track_id)
            .await
            .map_err(Into::into)
    }
    async fn stale_task_contexts_by_dst_track(
        &self,
        dst_track_id: &str,
    ) -> Result<Vec<calm_truth::db::TaskContextRow>> {
        calm_truth::db::RepoRead::stale_task_contexts_by_dst_track(self, dst_track_id)
            .await
            .map_err(Into::into)
    }
    async fn task_contexts_inflight_fresh(&self) -> Result<Vec<calm_truth::db::TaskContextRow>> {
        calm_truth::db::RepoRead::task_contexts_inflight_fresh(self)
            .await
            .map_err(Into::into)
    }
    async fn task_contexts_inflight_stale(&self) -> Result<Vec<calm_truth::db::TaskContextRow>> {
        calm_truth::db::RepoRead::task_contexts_inflight_stale(self)
            .await
            .map_err(Into::into)
    }
    async fn cards_by_track(&self, track_id: &str) -> Result<Vec<Card>> {
        calm_truth::db::RepoRead::cards_by_track(self, track_id)
            .await
            .map_err(Into::into)
    }
    async fn track_report_cards_by_area(&self, area_id: &str) -> Result<Vec<Card>> {
        calm_truth::db::RepoRead::track_report_cards_by_area(self, area_id)
            .await
            .map_err(Into::into)
    }
    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        calm_truth::db::RepoRead::card_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn card_get_with_body_crdt(&self, id: &str) -> Result<Option<(Card, Option<Vec<u8>>)>> {
        calm_truth::db::RepoRead::card_get_with_body_crdt(self, id)
            .await
            .map_err(Into::into)
    }
    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>> {
        calm_truth::db::RepoRead::card_role_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn harness_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>> {
        calm_truth::db::RepoRead::harness_item_list_by_card(
            self, card_id, after_id, limit, descending,
        )
        .await
        .map_err(Into::into)
    }
    async fn harness_item_list_transcript_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>> {
        calm_truth::db::RepoRead::harness_item_list_transcript_by_card(
            self, card_id, after_id, limit, descending,
        )
        .await
        .map_err(Into::into)
    }
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>> {
        calm_truth::db::RepoRead::overlays_for(self, entity_kind, entity_id)
            .await
            .map_err(Into::into)
    }
    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>> {
        calm_truth::db::RepoRead::overlays_by_kind(self, entity_kind)
            .await
            .map_err(Into::into)
    }
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>> {
        calm_truth::db::RepoRead::terminal_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>> {
        calm_truth::db::RepoRead::terminal_get_by_card(self, card_id)
            .await
            .map_err(Into::into)
    }
    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>> {
        calm_truth::db::RepoRead::terminals_orphaned(self, grace_seconds)
            .await
            .map_err(Into::into)
    }
    async fn terminals_running(&self) -> Result<Vec<Terminal>> {
        calm_truth::db::RepoRead::terminals_running(self)
            .await
            .map_err(Into::into)
    }
    async fn shared_planner_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>> {
        calm_truth::db::RepoRead::shared_planner_cards_for_initial_prompt_takeover(self)
            .await
            .map_err(Into::into)
    }
    async fn plugins_list(&self) -> Result<Vec<Plugin>> {
        calm_truth::db::RepoRead::plugins_list(self)
            .await
            .map_err(Into::into)
    }
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>> {
        calm_truth::db::RepoRead::plugins_list_all(self)
            .await
            .map_err(Into::into)
    }
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>> {
        calm_truth::db::RepoRead::plugin_get_by_id(self, id)
            .await
            .map_err(Into::into)
    }
    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>> {
        calm_truth::db::RepoRead::plugin_token_get(self, plugin_id)
            .await
            .map_err(Into::into)
    }
    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>> {
        calm_truth::db::RepoRead::plugin_kv_get(self, plugin_id, key)
            .await
            .map_err(Into::into)
    }
    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        calm_truth::db::RepoRead::plugin_kv_list(self, plugin_id, prefix)
            .await
            .map_err(Into::into)
    }
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>> {
        calm_truth::db::RepoRead::settings_get_all(self)
            .await
            .map_err(Into::into)
    }
    async fn seed_card_role_cache(&self, cache: &CardRoleCache) -> Result<()> {
        calm_truth::db::RepoRead::seed_card_role_cache(self, cache)
            .await
            .map_err(Into::into)
    }
    async fn seed_track_area_cache(&self, cache: &TrackAreaCache) -> Result<()> {
        calm_truth::db::RepoRead::seed_track_area_cache(self, cache)
            .await
            .map_err(Into::into)
    }
    async fn card_mcp_token_lookup_by_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<(String, String)>> {
        calm_truth::db::RepoRead::card_mcp_token_lookup_by_hash(self, hashed_token)
            .await
            .map_err(Into::into)
    }
    async fn card_identity_get_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionCardIdentity>> {
        calm_truth::db::RepoRead::card_identity_get_by_session(self, session_id)
            .await
            .map_err(Into::into)
    }
    async fn session_get_by_active_token_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<WorkerSession>> {
        calm_truth::db::RepoRead::session_get_by_active_token_hash(self, hashed_token)
            .await
            .map_err(Into::into)
    }
    async fn session_get_by_id(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>> {
        calm_truth::db::RepoRead::session_get_by_id(self, id)
            .await
            .map_err(Into::into)
    }
    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord> {
        calm_truth::db::RepoRead::shared_daemon_runtime_get(self)
            .await
            .map_err(Into::into)
    }
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
