pub mod config;
pub mod lock;
pub mod observation;
pub mod registry;
pub mod run_loop;
pub mod snapshot;
pub mod state;

use std::collections::HashSet;
use std::sync::Arc;

use crate::card_role_cache::CardRoleCache;
use crate::db::{Repo, write_in_tx_typed};
use crate::dispatcher;
use crate::error::Result;
use crate::event::{Event, EventBus};
use crate::ids::{CardId, WaveId};
use crate::model::CardRole;
use crate::runtime_repo::CardRuntime;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::wave_cove_cache::WaveCoveCache;

pub use config::HarnessConfig;
pub use lock::PushLockGuard;
pub use observation::{HookKind, Observation};
pub use registry::HarnessRegistry;
pub use run_loop::{SpecHarness, SpecHarnessParams};
pub use snapshot::{HARNESS_MODE, HarnessPhaseTag, HarnessSnapshot, is_harness_snapshot_value};
pub use state::{HarnessState, IssuingKind, run_status_for};

pub async fn spawn_recovered_harness(
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    daemon: Arc<SharedCodexAppServer>,
    registry: &HarnessRegistry,
    runtime: CardRuntime,
) -> Result<Option<SpecHarness>> {
    let Some(card) = repo.card_get(&runtime.card_id).await? else {
        return Ok(None);
    };
    let Some(state_json) = runtime.handle_state_json.clone() else {
        return Ok(None);
    };
    let watermark_missing = state_json.get("push_watermark").is_none();
    let mut snapshot = HarnessSnapshot::from_value_strict(state_json);
    if watermark_missing
        && let Some(watermark) = card.payload.get("push_watermark").and_then(|v| v.as_i64())
    {
        snapshot.push_watermark = watermark;
    }
    let catch_up_watermark = snapshot.push_watermark;
    let rehydrated_ids =
        rehydrate_spec_push_queue(repo.clone(), &runtime.card_id, &mut snapshot).await?;
    replay_harness_events_since(
        repo.clone(),
        &runtime.card_id,
        &card.wave_id,
        catch_up_watermark,
        &rehydrated_ids,
        &mut snapshot,
    )
    .await?;
    let runtime_id = runtime.id.clone();
    if let Some(existing) = registry.remove(&runtime_id) {
        existing.shutdown().await?;
    }
    let handle = SpecHarness::run(SpecHarnessParams {
        runtime_id: runtime_id.clone(),
        wave_id: card.wave_id,
        card_id: CardId::from(runtime.card_id.clone()),
        thread_id: runtime
            .thread_id
            .clone()
            .or(snapshot.last_thread_id.clone()),
        repo,
        events,
        card_role_cache,
        wave_cove_cache,
        daemon,
        config: HarnessConfig::default(),
        snapshot,
    });
    registry.insert(runtime_id, handle.clone());
    Ok(Some(handle))
}

pub async fn rehydrate_spec_push_queue(
    repo: Arc<dyn Repo>,
    card_id: &str,
    snapshot: &mut HarnessSnapshot,
) -> Result<Vec<i64>> {
    let rows = repo.spec_card_queued_observations(card_id).await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    snapshot.align_pending_envelope_ids();
    let mut row_ids = Vec::new();
    let mut rehydrated_ids = Vec::new();
    let mut seen_envelope_ids: HashSet<i64> = snapshot
        .pending_envelope_ids
        .iter()
        .flatten()
        .copied()
        .collect();
    for (id, envelope_id, text) in rows {
        let obs =
            serde_json::from_str::<Observation>(&text).unwrap_or(Observation::WaveGoal { text });
        if seen_envelope_ids.insert(envelope_id) {
            snapshot.pending_queue.push(obs);
            snapshot.pending_envelope_ids.push(Some(envelope_id));
            snapshot.push_watermark = snapshot.push_watermark.max(envelope_id);
            rehydrated_ids.push(envelope_id);
        }
        row_ids.push(id);
    }

    persist_recovered_snapshot(repo, card_id, snapshot, row_ids).await?;
    Ok(rehydrated_ids)
}

async fn replay_harness_events_since(
    repo: Arc<dyn Repo>,
    card_id: &str,
    wave_id: &WaveId,
    watermark: i64,
    rehydrated_ids: &[i64],
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    let rehydrated_skip: HashSet<i64> = rehydrated_ids.iter().copied().collect();
    let rows = repo.events_since(watermark, None).await?;
    let mut replayed = 0usize;
    let mut skipped_rehydrated = 0usize;
    for (id, _version, scope, event) in rows {
        if scope.wave_id() != Some(wave_id) {
            continue;
        }
        let role = role_needed_for_spec_push_filter(repo.as_ref(), &event).await?;
        if !dispatcher::event_warrants_spec_push_with_role(&event, |_| role) {
            continue;
        }
        if rehydrated_skip.contains(&id) {
            skipped_rehydrated += 1;
            continue;
        }
        let Some(obs) = dispatcher::harness_observation_from_event(wave_id, &event) else {
            continue;
        };
        snapshot.pending_queue.push(obs);
        snapshot.pending_envelope_ids.push(Some(id));
        snapshot.push_watermark = snapshot.push_watermark.max(id);
        replayed += 1;
    }
    if replayed > 0 {
        persist_recovered_snapshot(repo, card_id, snapshot, Vec::new()).await?;
    }
    if replayed > 0 || skipped_rehydrated > 0 {
        tracing::info!(
            card_id,
            wave_id = %wave_id,
            watermark,
            replayed,
            skipped_rehydrated,
            "harness recovery: replayed spec push catch-up events into pending queue",
        );
    }
    Ok(())
}

async fn role_needed_for_spec_push_filter(
    repo: &dyn Repo,
    event: &Event,
) -> Result<Option<CardRole>> {
    match event {
        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => {
            repo.card_role_get(card_id.as_str()).await
        }
        _ => Ok(None),
    }
}

async fn persist_recovered_snapshot(
    repo: Arc<dyn Repo>,
    card_id: &str,
    snapshot: &HarnessSnapshot,
    row_ids: Vec<i64>,
) -> Result<()> {
    let runtime_state = serde_json::to_value(snapshot)?;
    let runtime_id = snapshot_runtime_id(repo.as_ref(), card_id).await?;
    write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::runtime_set_handle_state_tx(tx, &runtime_id, Some(runtime_state))
                .await?;
            if !row_ids.is_empty() {
                let placeholders = std::iter::repeat_n("?", row_ids.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!("DELETE FROM spec_push_queue WHERE id IN ({placeholders})");
                let mut q = sqlx::query(&sql);
                for id in row_ids {
                    q = q.bind(id);
                }
                q.execute(&mut **tx).await?;
            }
            Ok(())
        })
    })
    .await
}

async fn snapshot_runtime_id(repo: &dyn Repo, card_id: &str) -> Result<String> {
    let runtime = repo
        .runtime_get_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("runtime for card {card_id}")))?;
    Ok(runtime.id)
}

pub async fn recover_harnesses_on_boot(
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    daemon: Arc<SharedCodexAppServer>,
    registry: &HarnessRegistry,
) -> Result<usize> {
    let runtimes = repo.runtimes_recover_harnesses_on_boot().await?;
    let mut recovered = 0usize;
    for runtime in runtimes {
        if spawn_recovered_harness(
            repo.clone(),
            events.clone(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
            daemon.clone(),
            registry,
            runtime,
        )
        .await?
        .is_some()
        {
            recovered += 1;
        }
    }
    Ok(recovered)
}

pub fn initial_snapshot_with_goal(goal: Option<String>) -> HarnessSnapshot {
    let pending_queue = goal
        .filter(|text| !text.trim().is_empty())
        .map(|text| vec![Observation::WaveGoal { text }])
        .unwrap_or_default();
    HarnessSnapshot::initial(0, pending_queue)
}
