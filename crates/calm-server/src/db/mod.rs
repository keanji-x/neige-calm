pub use calm_truth::db::{
    Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, RouteRepo,
    SessionCardIdentity, SharedCodexDaemonRecord, SharedCodexDaemonUpdate, TrackEvent,
    WorkspaceLease, WriteInTxFn, WriteWithActorEventsFn, WriteWithEventFn, WriteWithEventsFn, rows,
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
    pub use super::{
        Repo, RouteRepo, ServerRepoEventWriteExt, ServerRepoOutOfDomainExt, ServerRepoReadExt,
        ServerRepoSyncDomainRawExt, WorkspaceLease,
    };
    pub use crate::session_projection_repo::WorkerSessionProjectionRepo;
    pub use calm_truth::session_repo::{CommitExitOutcome, DeadRootCandidate, SessionRepo};
}

#[async_trait]
pub trait ServerRepoReadExt {
    async fn areas_list(&self) -> Result<Vec<Area>>;
    async fn areas_list_user_visible(&self) -> Result<Vec<Area>>;
    async fn area_get(&self, id: &str) -> Result<Option<Area>>;
    async fn area_get_system(&self) -> Result<Option<Area>>;
    async fn area_folders_by_area(&self, area_id: &str) -> Result<Vec<AreaFolder>>;
    async fn area_folders_list_all(&self) -> Result<Vec<AreaFolder>>;
    async fn area_folder_get(&self, id: i64) -> Result<Option<AreaFolder>>;
    async fn tracks_by_area(&self, area_id: &str) -> Result<Vec<Track>>;
    async fn track_get(&self, id: &str) -> Result<Option<Track>>;
    /// #1253 PR1 — the Today launchpad track, or `None` before it exists.
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
    async fn card_mcp_token_exists_for_card(&self, card_id: &str) -> Result<bool>;
    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord>;
}

#[async_trait]
impl<T> ServerRepoReadExt for T
where
    T: calm_truth::db::RepoRead + ?Sized,
{
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
    async fn card_mcp_token_exists_for_card(&self, card_id: &str) -> Result<bool> {
        calm_truth::db::RepoRead::card_mcp_token_exists_for_card(self, card_id)
            .await
            .map_err(Into::into)
    }
    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord> {
        calm_truth::db::RepoRead::shared_daemon_runtime_get(self)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait ServerRepoEventWriteExt: ServerRepoReadExt {
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &WriteContext,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64>;
    async fn write_with_events(
        &self,
        actor: ActorId,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &WriteContext,
        f: WriteWithEventsFn<'_>,
    ) -> Result<Vec<i64>>;
    async fn write_with_actor_events(
        &self,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &WriteContext,
        f: WriteWithActorEventsFn<'_>,
    ) -> Result<Vec<i64>>;
    async fn log_pure_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        card_role_cache: &CardRoleCache,
        track_area_cache: &TrackAreaCache,
        event: Event,
    ) -> Result<i64>;
    async fn write_in_tx(&self, f: WriteInTxFn<'_>) -> Result<()>;
    async fn events_since(
        &self,
        since_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, u32, EventScope, Event)>>;
    /// Bounded RAW-row window probe (count + max id) for the WS replay cap
    /// decision — see
    /// [`calm_truth::db::RepoEventWrite::events_raw_window_since`].
    async fn events_raw_window_since(
        &self,
        since_id: i64,
        probe_limit: i64,
    ) -> Result<(i64, Option<i64>)>;
    async fn events_for_track(
        &self,
        track_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<TrackEvent>>;
    async fn events_earliest_id(&self) -> Result<Option<i64>>;
    async fn events_prune_watermark(&self) -> Result<i64>;
    async fn events_latest_id(&self) -> Result<Option<i64>>;
}

#[async_trait]
impl<T> ServerRepoEventWriteExt for T
where
    T: calm_truth::db::RepoEventWrite + ?Sized,
{
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &WriteContext,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64> {
        calm_truth::db::RepoEventWrite::write_with_event(
            self,
            actor,
            scope,
            correlation,
            bus,
            write,
            f,
        )
        .await
        .map_err(Into::into)
    }
    async fn write_with_events(
        &self,
        actor: ActorId,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &WriteContext,
        f: WriteWithEventsFn<'_>,
    ) -> Result<Vec<i64>> {
        calm_truth::db::RepoEventWrite::write_with_events(self, actor, correlation, bus, write, f)
            .await
            .map_err(Into::into)
    }
    async fn write_with_actor_events(
        &self,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &WriteContext,
        f: WriteWithActorEventsFn<'_>,
    ) -> Result<Vec<i64>> {
        calm_truth::db::RepoEventWrite::write_with_actor_events(self, correlation, bus, write, f)
            .await
            .map_err(Into::into)
    }
    async fn log_pure_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        card_role_cache: &CardRoleCache,
        track_area_cache: &TrackAreaCache,
        event: Event,
    ) -> Result<i64> {
        calm_truth::db::RepoEventWrite::log_pure_event(
            self,
            actor,
            scope,
            correlation,
            bus,
            card_role_cache,
            track_area_cache,
            event,
        )
        .await
        .map_err(Into::into)
    }
    async fn write_in_tx(&self, f: WriteInTxFn<'_>) -> Result<()> {
        calm_truth::db::RepoEventWrite::write_in_tx(self, f)
            .await
            .map_err(Into::into)
    }
    async fn events_since(
        &self,
        since_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, u32, EventScope, Event)>> {
        calm_truth::db::RepoEventWrite::events_since(self, since_id, limit)
            .await
            .map_err(Into::into)
    }
    async fn events_raw_window_since(
        &self,
        since_id: i64,
        probe_limit: i64,
    ) -> Result<(i64, Option<i64>)> {
        calm_truth::db::RepoEventWrite::events_raw_window_since(self, since_id, probe_limit)
            .await
            .map_err(Into::into)
    }
    async fn events_for_track(
        &self,
        track_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<TrackEvent>> {
        calm_truth::db::RepoEventWrite::events_for_track(self, track_id, kinds, since_id)
            .await
            .map_err(Into::into)
    }
    async fn events_earliest_id(&self) -> Result<Option<i64>> {
        calm_truth::db::RepoEventWrite::events_earliest_id(self)
            .await
            .map_err(Into::into)
    }
    async fn events_prune_watermark(&self) -> Result<i64> {
        calm_truth::db::RepoEventWrite::events_prune_watermark(self)
            .await
            .map_err(Into::into)
    }
    async fn events_latest_id(&self) -> Result<Option<i64>> {
        calm_truth::db::RepoEventWrite::events_latest_id(self)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
pub trait ServerRepoSyncDomainRawExt: ServerRepoReadExt {
    async fn area_create(&self, p: NewArea) -> Result<Area>;
    async fn area_update(&self, id: &str, p: AreaPatch) -> Result<Area>;
    async fn area_delete(&self, id: &str) -> Result<()>;
    async fn track_create(&self, p: NewTrack) -> Result<Track>;
    async fn track_update(&self, id: &str, p: TrackPatch) -> Result<Track>;
    async fn track_delete(&self, id: &str) -> Result<()>;
    async fn card_create(&self, p: NewCard) -> Result<Card>;
    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card>;
    async fn card_delete(&self, id: &str) -> Result<()>;
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay>;
    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()>;
}

#[async_trait]
impl<T> ServerRepoSyncDomainRawExt for T
where
    T: calm_truth::db::RepoSyncDomainRaw + ?Sized,
{
    async fn area_create(&self, p: NewArea) -> Result<Area> {
        calm_truth::db::RepoSyncDomainRaw::area_create(self, p)
            .await
            .map_err(Into::into)
    }
    async fn area_update(&self, id: &str, p: AreaPatch) -> Result<Area> {
        calm_truth::db::RepoSyncDomainRaw::area_update(self, id, p)
            .await
            .map_err(Into::into)
    }
    async fn area_delete(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoSyncDomainRaw::area_delete(self, id)
            .await
            .map_err(Into::into)
    }
    async fn track_create(&self, p: NewTrack) -> Result<Track> {
        calm_truth::db::RepoSyncDomainRaw::track_create(self, p)
            .await
            .map_err(Into::into)
    }
    async fn track_update(&self, id: &str, p: TrackPatch) -> Result<Track> {
        calm_truth::db::RepoSyncDomainRaw::track_update(self, id, p)
            .await
            .map_err(Into::into)
    }
    async fn track_delete(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoSyncDomainRaw::track_delete(self, id)
            .await
            .map_err(Into::into)
    }
    async fn card_create(&self, p: NewCard) -> Result<Card> {
        calm_truth::db::RepoSyncDomainRaw::card_create(self, p)
            .await
            .map_err(Into::into)
    }
    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card> {
        calm_truth::db::RepoSyncDomainRaw::card_update(self, id, p)
            .await
            .map_err(Into::into)
    }
    async fn card_delete(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoSyncDomainRaw::card_delete(self, id)
            .await
            .map_err(Into::into)
    }
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay> {
        calm_truth::db::RepoSyncDomainRaw::overlay_upsert(self, p)
            .await
            .map_err(Into::into)
    }
    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()> {
        calm_truth::db::RepoSyncDomainRaw::overlay_delete(
            self,
            plugin_id,
            entity_kind,
            entity_id,
            kind,
        )
        .await
        .map_err(Into::into)
    }
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait ServerRepoOutOfDomainExt: ServerRepoReadExt {
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal>;
    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()>;
    async fn terminal_set_exit(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) -> Result<()>;
    async fn terminal_clear_exit_for_spawn(&self, id: &str) -> Result<()>;
    async fn terminal_delete(&self, id: &str) -> Result<()>;
    async fn shared_daemon_runtime_set(&self, update: SharedCodexDaemonUpdate) -> Result<()>;
    async fn shared_daemon_record_event(&self, action: &str, error: Option<&str>) -> Result<()>;
    async fn harness_item_insert(
        &self,
        runtime_id: &str,
        card_id: &str,
        track_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
    ) -> Result<i64>;
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin>;
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin>;
    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin>;
    async fn plugin_update_manifest(&self, id: &str, manifest: serde_json::Value)
    -> Result<Plugin>;
    async fn plugin_delete(&self, id: &str) -> Result<()>;
    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()>;
    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()>;
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()>;
    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()>;
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()>;
    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()>;
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()>;
    async fn settings_delete(&self, key: &str) -> Result<()>;
    async fn area_folder_create(&self, area_id: &str, path: &str) -> Result<AreaFolder>;
    /// Issue #275 — atomic scan+insert. See
    /// [`calm_truth::db::RepoOutOfDomain::area_folder_create_checked`].
    async fn area_folder_create_checked(
        &self,
        area_id: &str,
        path: &str,
    ) -> Result<calm_truth::area_folder_claim::AreaFolderClaim>;
    async fn area_folder_delete(&self, id: i64) -> Result<()>;
}

#[async_trait]
impl<T> ServerRepoOutOfDomainExt for T
where
    T: calm_truth::db::RepoOutOfDomain + ?Sized,
{
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal> {
        calm_truth::db::RepoOutOfDomain::terminal_create(self, p)
            .await
            .map_err(Into::into)
    }
    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::terminal_set_pid(self, id, pid)
            .await
            .map_err(Into::into)
    }
    async fn terminal_set_exit(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::terminal_set_exit(self, id, exit_code, signal_killed)
            .await
            .map_err(Into::into)
    }
    async fn terminal_clear_exit_for_spawn(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::terminal_clear_exit_for_spawn(self, id)
            .await
            .map_err(Into::into)
    }
    async fn terminal_delete(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::terminal_delete(self, id)
            .await
            .map_err(Into::into)
    }
    async fn shared_daemon_runtime_set(&self, update: SharedCodexDaemonUpdate) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::shared_daemon_runtime_set(self, update)
            .await
            .map_err(Into::into)
    }
    async fn shared_daemon_record_event(&self, action: &str, error: Option<&str>) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::shared_daemon_record_event(self, action, error)
            .await
            .map_err(Into::into)
    }
    async fn harness_item_insert(
        &self,
        runtime_id: &str,
        card_id: &str,
        track_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
    ) -> Result<i64> {
        calm_truth::db::RepoOutOfDomain::harness_item_insert(
            self, runtime_id, card_id, track_id, thread_id, turn_id, item_uuid, item_type, method,
            params,
        )
        .await
        .map_err(Into::into)
    }
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin> {
        calm_truth::db::RepoOutOfDomain::plugin_install(self, p)
            .await
            .map_err(Into::into)
    }
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin> {
        calm_truth::db::RepoOutOfDomain::plugin_update_enabled(self, id, enabled)
            .await
            .map_err(Into::into)
    }
    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin> {
        calm_truth::db::RepoOutOfDomain::plugin_update_user_config(self, id, user_config)
            .await
            .map_err(Into::into)
    }
    async fn plugin_update_manifest(
        &self,
        id: &str,
        manifest: serde_json::Value,
    ) -> Result<Plugin> {
        calm_truth::db::RepoOutOfDomain::plugin_update_manifest(self, id, manifest)
            .await
            .map_err(Into::into)
    }
    async fn plugin_delete(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::plugin_delete(self, id)
            .await
            .map_err(Into::into)
    }
    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::overlays_clear_by_plugin(self, plugin_id)
            .await
            .map_err(Into::into)
    }
    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::plugin_kv_clear(self, plugin_id)
            .await
            .map_err(Into::into)
    }
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::plugin_token_set(self, plugin_id, hashed_token, expires_at)
            .await
            .map_err(Into::into)
    }
    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::plugin_token_delete(self, plugin_id)
            .await
            .map_err(Into::into)
    }
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::plugin_kv_set(self, plugin_id, key, value)
            .await
            .map_err(Into::into)
    }
    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::plugin_kv_delete(self, plugin_id, key)
            .await
            .map_err(Into::into)
    }
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::settings_upsert(self, key, value)
            .await
            .map_err(Into::into)
    }
    async fn settings_delete(&self, key: &str) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::settings_delete(self, key)
            .await
            .map_err(Into::into)
    }
    async fn area_folder_create(&self, area_id: &str, path: &str) -> Result<AreaFolder> {
        calm_truth::db::RepoOutOfDomain::area_folder_create(self, area_id, path)
            .await
            .map_err(Into::into)
    }
    async fn area_folder_create_checked(
        &self,
        area_id: &str,
        path: &str,
    ) -> Result<calm_truth::area_folder_claim::AreaFolderClaim> {
        calm_truth::db::RepoOutOfDomain::area_folder_create_checked(self, area_id, path)
            .await
            .map_err(Into::into)
    }
    async fn area_folder_delete(&self, id: i64) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::area_folder_delete(self, id)
            .await
            .map_err(Into::into)
    }
}

pub mod sqlite {
    pub use calm_truth::db::sqlite::*;

    use sqlx::{Sqlite, Transaction};

    use crate::card_role_cache::CardRoleCache;
    use crate::error::Result;
    use crate::ids::TrackId;
    use crate::model::{Card, CardRole, Terminal};
    use calm_truth::model::RequestTheme;

    pub async fn require_track_exists_tx(
        tx: &mut Transaction<'_, Sqlite>,
        track_id: &str,
    ) -> Result<()> {
        calm_truth::db::sqlite::require_track_exists_tx(tx, track_id)
            .await
            .map_err(Into::into)
    }

    pub async fn task_mark_running_tx(
        tx: &mut Transaction<'_, Sqlite>,
        id: &str,
        worker_card_id: Option<&str>,
        now: i64,
        running_deadline_ms: i64,
    ) -> Result<u64> {
        calm_truth::db::sqlite::task_mark_running_tx(
            tx,
            id,
            worker_card_id,
            now,
            running_deadline_ms,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn terminal_create_tx(
        tx: &mut Transaction<'_, Sqlite>,
        p: crate::model::NewTerminal,
    ) -> Result<Terminal> {
        calm_truth::db::sqlite::terminal_create_tx(tx, p)
            .await
            .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn card_with_terminal_create_tx(
        tx: &mut Transaction<'_, Sqlite>,
        card_id: String,
        runtime_id: &str,
        spawn_op_id: Option<&str>,
        track_id: TrackId,
        title: Option<String>,
        sort: Option<f64>,
        program: String,
        cwd: String,
        env: serde_json::Value,
        role: CardRole,
        deletable: bool,
        card_role_cache: &CardRoleCache,
        theme: RequestTheme,
    ) -> Result<(Card, Terminal)> {
        calm_truth::db::sqlite::card_with_terminal_create_tx(
            tx,
            card_id,
            runtime_id,
            spawn_op_id,
            track_id,
            title,
            sort,
            program,
            cwd,
            env,
            role,
            deletable,
            card_role_cache,
            theme,
        )
        .await
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn card_with_codex_create_tx(
        tx: &mut Transaction<'_, Sqlite>,
        card_id: String,
        runtime_id: &str,
        spawn_op_id: Option<&str>,
        track_id: TrackId,
        title: Option<String>,
        sort: Option<f64>,
        cwd: String,
        env: serde_json::Value,
        prompt: Option<String>,
        icon_bg: Option<String>,
        icon_fg: Option<String>,
        role: CardRole,
        deletable: bool,
        card_role_cache: &CardRoleCache,
        theme: RequestTheme,
    ) -> Result<(Card, Terminal, Option<String>)> {
        calm_truth::db::sqlite::card_with_codex_create_tx(
            tx,
            card_id,
            runtime_id,
            spawn_op_id,
            track_id,
            title,
            sort,
            cwd,
            env,
            prompt,
            icon_bg,
            icon_fg,
            role,
            deletable,
            card_role_cache,
            theme,
        )
        .await
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn card_with_claude_create_tx(
        tx: &mut Transaction<'_, Sqlite>,
        card_id: String,
        runtime_id: &str,
        track_id: TrackId,
        title: Option<String>,
        sort: Option<f64>,
        program: String,
        cwd: String,
        env: serde_json::Value,
        prompt: Option<String>,
        icon_bg: Option<String>,
        icon_fg: Option<String>,
        settings_path: String,
        claude_session_id: String,
        role: CardRole,
        deletable: bool,
        card_role_cache: &CardRoleCache,
        theme: RequestTheme,
    ) -> Result<(Card, Terminal)> {
        calm_truth::db::sqlite::card_with_claude_create_tx(
            tx,
            card_id,
            runtime_id,
            track_id,
            title,
            sort,
            program,
            cwd,
            env,
            prompt,
            icon_bg,
            icon_fg,
            settings_path,
            claude_session_id,
            role,
            deletable,
            card_role_cache,
            theme,
        )
        .await
        .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn card_with_claude_worker_create_tx(
        tx: &mut Transaction<'_, Sqlite>,
        card_id: String,
        runtime_id: &str,
        spawn_op_id: Option<&str>,
        track_id: TrackId,
        title: Option<String>,
        sort: Option<f64>,
        program: String,
        cwd: String,
        env: serde_json::Value,
        prompt: Option<String>,
        icon_bg: Option<String>,
        icon_fg: Option<String>,
        settings_path: String,
        claude_session_id: String,
        card_role_cache: &CardRoleCache,
        theme: RequestTheme,
    ) -> Result<(Card, Terminal)> {
        calm_truth::db::sqlite::card_with_claude_worker_create_tx(
            tx,
            card_id,
            runtime_id,
            spawn_op_id,
            track_id,
            title,
            sort,
            program,
            cwd,
            env,
            prompt,
            icon_bg,
            icon_fg,
            settings_path,
            claude_session_id,
            card_role_cache,
            theme,
        )
        .await
        .map_err(Into::into)
    }
}

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
