pub use calm_truth::db::{
    Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, RouteRepo,
    SessionCardIdentity, SharedCodexDaemonRecord, SharedCodexDaemonUpdate, WaveEvent,
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
use crate::{card_role_cache::CardRoleCache, wave_cove_cache::WaveCoveCache};
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
    async fn coves_list(&self) -> Result<Vec<Cove>>;
    async fn coves_list_user_visible(&self) -> Result<Vec<Cove>>;
    async fn cove_get(&self, id: &str) -> Result<Option<Cove>>;
    async fn cove_get_system(&self) -> Result<Option<Cove>>;
    async fn cove_folders_by_cove(&self, cove_id: &str) -> Result<Vec<CoveFolder>>;
    async fn cove_folders_list_all(&self) -> Result<Vec<CoveFolder>>;
    async fn cove_folder_get(&self, id: i64) -> Result<Option<CoveFolder>>;
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>>;
    async fn wave_get(&self, id: &str) -> Result<Option<Wave>>;
    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>>;
    async fn waves_window(
        &self,
        cove_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Wave>>;
    async fn tasks_by_wave(&self, wave_id: &str) -> Result<Vec<Task>>;
    async fn task_get(&self, id: &str) -> Result<Option<Task>>;
    async fn tasks_nonterminal(&self) -> Result<Vec<Task>>;
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>>;
    async fn card_get(&self, id: &str) -> Result<Option<Card>>;
    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>>;
    async fn harness_item_list_by_card(
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
    async fn shared_spec_cards_for_initial_prompt_takeover(
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
    async fn seed_wave_cove_cache(&self, cache: &WaveCoveCache) -> Result<()>;
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
    async fn coves_list(&self) -> Result<Vec<Cove>> {
        calm_truth::db::RepoRead::coves_list(self)
            .await
            .map_err(Into::into)
    }
    async fn coves_list_user_visible(&self) -> Result<Vec<Cove>> {
        calm_truth::db::RepoRead::coves_list_user_visible(self)
            .await
            .map_err(Into::into)
    }
    async fn cove_get(&self, id: &str) -> Result<Option<Cove>> {
        calm_truth::db::RepoRead::cove_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn cove_get_system(&self) -> Result<Option<Cove>> {
        calm_truth::db::RepoRead::cove_get_system(self)
            .await
            .map_err(Into::into)
    }
    async fn cove_folders_by_cove(&self, cove_id: &str) -> Result<Vec<CoveFolder>> {
        calm_truth::db::RepoRead::cove_folders_by_cove(self, cove_id)
            .await
            .map_err(Into::into)
    }
    async fn cove_folders_list_all(&self) -> Result<Vec<CoveFolder>> {
        calm_truth::db::RepoRead::cove_folders_list_all(self)
            .await
            .map_err(Into::into)
    }
    async fn cove_folder_get(&self, id: i64) -> Result<Option<CoveFolder>> {
        calm_truth::db::RepoRead::cove_folder_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>> {
        calm_truth::db::RepoRead::waves_by_cove(self, cove_id)
            .await
            .map_err(Into::into)
    }
    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        calm_truth::db::RepoRead::wave_get(self, id)
            .await
            .map_err(Into::into)
    }
    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>> {
        calm_truth::db::RepoRead::wave_detail(self, id)
            .await
            .map_err(Into::into)
    }
    async fn waves_window(
        &self,
        cove_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Wave>> {
        calm_truth::db::RepoRead::waves_window(self, cove_id, since, until)
            .await
            .map_err(Into::into)
    }
    async fn tasks_by_wave(&self, wave_id: &str) -> Result<Vec<Task>> {
        calm_truth::db::RepoRead::tasks_by_wave(self, wave_id)
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
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>> {
        calm_truth::db::RepoRead::cards_by_wave(self, wave_id)
            .await
            .map_err(Into::into)
    }
    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        calm_truth::db::RepoRead::card_get(self, id)
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
    async fn shared_spec_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>> {
        calm_truth::db::RepoRead::shared_spec_cards_for_initial_prompt_takeover(self)
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
    async fn seed_wave_cove_cache(&self, cache: &WaveCoveCache) -> Result<()> {
        calm_truth::db::RepoRead::seed_wave_cove_cache(self, cache)
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
        wave_cove_cache: &WaveCoveCache,
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
    async fn events_for_wave(
        &self,
        wave_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<WaveEvent>>;
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
        wave_cove_cache: &WaveCoveCache,
        event: Event,
    ) -> Result<i64> {
        calm_truth::db::RepoEventWrite::log_pure_event(
            self,
            actor,
            scope,
            correlation,
            bus,
            card_role_cache,
            wave_cove_cache,
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
    async fn events_for_wave(
        &self,
        wave_id: &str,
        kinds: &[&str],
        since_id: Option<i64>,
    ) -> Result<Vec<WaveEvent>> {
        calm_truth::db::RepoEventWrite::events_for_wave(self, wave_id, kinds, since_id)
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
    async fn cove_create(&self, p: NewCove) -> Result<Cove>;
    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove>;
    async fn cove_delete(&self, id: &str) -> Result<()>;
    async fn wave_create(&self, p: NewWave) -> Result<Wave>;
    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave>;
    async fn wave_delete(&self, id: &str) -> Result<()>;
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
    async fn cove_create(&self, p: NewCove) -> Result<Cove> {
        calm_truth::db::RepoSyncDomainRaw::cove_create(self, p)
            .await
            .map_err(Into::into)
    }
    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove> {
        calm_truth::db::RepoSyncDomainRaw::cove_update(self, id, p)
            .await
            .map_err(Into::into)
    }
    async fn cove_delete(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoSyncDomainRaw::cove_delete(self, id)
            .await
            .map_err(Into::into)
    }
    async fn wave_create(&self, p: NewWave) -> Result<Wave> {
        calm_truth::db::RepoSyncDomainRaw::wave_create(self, p)
            .await
            .map_err(Into::into)
    }
    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave> {
        calm_truth::db::RepoSyncDomainRaw::wave_update(self, id, p)
            .await
            .map_err(Into::into)
    }
    async fn wave_delete(&self, id: &str) -> Result<()> {
        calm_truth::db::RepoSyncDomainRaw::wave_delete(self, id)
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
        wave_id: &str,
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
    async fn cove_folder_create(&self, cove_id: &str, path: &str) -> Result<CoveFolder>;
    async fn cove_folder_refresh_repo_identity(&self, id: i64) -> Result<CoveFolder>;
    async fn cove_folder_delete(&self, id: i64) -> Result<()>;
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
        wave_id: &str,
        thread_id: &str,
        turn_id: Option<&str>,
        item_uuid: Option<&str>,
        item_type: Option<&str>,
        method: &str,
        params: &str,
    ) -> Result<i64> {
        calm_truth::db::RepoOutOfDomain::harness_item_insert(
            self, runtime_id, card_id, wave_id, thread_id, turn_id, item_uuid, item_type, method,
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
    async fn cove_folder_create(&self, cove_id: &str, path: &str) -> Result<CoveFolder> {
        calm_truth::db::RepoOutOfDomain::cove_folder_create(self, cove_id, path)
            .await
            .map_err(Into::into)
    }
    async fn cove_folder_refresh_repo_identity(&self, id: i64) -> Result<CoveFolder> {
        calm_truth::db::RepoOutOfDomain::cove_folder_refresh_repo_identity(self, id)
            .await
            .map_err(Into::into)
    }
    async fn cove_folder_delete(&self, id: i64) -> Result<()> {
        calm_truth::db::RepoOutOfDomain::cove_folder_delete(self, id)
            .await
            .map_err(Into::into)
    }
}

pub mod sqlite {
    pub use calm_truth::db::sqlite::*;

    use sqlx::{Sqlite, Transaction};

    use crate::card_role_cache::CardRoleCache;
    use crate::error::Result;
    use crate::ids::WaveId;
    use crate::model::{Card, CardRole, Terminal};
    use calm_truth::model::RequestTheme;

    pub async fn require_wave_exists_tx(
        tx: &mut Transaction<'_, Sqlite>,
        wave_id: &str,
    ) -> Result<()> {
        calm_truth::db::sqlite::require_wave_exists_tx(tx, wave_id)
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
        wave_id: WaveId,
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
            wave_id,
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
        wave_id: WaveId,
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
            wave_id,
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
        wave_id: WaveId,
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
            wave_id,
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
        wave_id: WaveId,
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
            wave_id,
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
