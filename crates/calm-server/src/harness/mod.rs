pub mod config;
pub mod lock;
pub mod observation;
pub mod registry;
pub mod run_loop;
pub mod snapshot;
pub mod state;

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
    let mut snapshot = HarnessSnapshot::from_value_strict(state_json);
    let catch_up_watermark = snapshot.push_watermark;
    replay_harness_events_since(
        repo.clone(),
        &runtime.card_id,
        &card.wave_id,
        catch_up_watermark,
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
        // Normalize blank/whitespace thread IDs to `None` before the
        // fallback chain: a row with `thread_id = ''` would otherwise win as
        // `Some("")` over the snapshot's valid `last_thread_id`, and the
        // recovered harness would issue turns against an empty thread.
        thread_id: runtime
            .thread_id
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| {
                snapshot
                    .last_thread_id
                    .clone()
                    .filter(|t| !t.trim().is_empty())
            }),
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

async fn replay_harness_events_since(
    repo: Arc<dyn Repo>,
    card_id: &str,
    wave_id: &WaveId,
    watermark: i64,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    let rows = repo
        .events_for_wave(
            wave_id.as_str(),
            &[
                "task.completed",
                "task.failed",
                // Issue #644 PR-C (§6.5/§8) — gate verdicts that
                // landed while the kernel was down replay like live
                // pushes.
                "task.gate_result",
                "wave.report_edited",
                "codex.hook",
                "claude.hook",
            ],
            Some(watermark),
        )
        .await?;
    let mut replayed = 0usize;
    for row in rows {
        let role = role_needed_for_spec_push_filter(repo.as_ref(), &row.event).await?;
        if !dispatcher::event_warrants_spec_push_with_role(&row.event, &row.actor, |_| role) {
            continue;
        }
        // Issue #644 PR-C (§6.5) — the SAME gated-self-report
        // consultation the live push branch runs: a crash between the
        // emit tx and the live push must not replay a gated task's
        // raw self-report to the spec.
        if dispatcher::is_gated_self_report(repo.as_ref(), &row.event).await {
            continue;
        }
        let Some(obs) = dispatcher::harness_observation_from_event(wave_id, &row.event) else {
            continue;
        };
        snapshot.pending_queue.push(obs);
        snapshot.pending_envelope_ids.push(Some(row.id));
        snapshot.push_watermark = snapshot.push_watermark.max(row.id);
        replayed += 1;
    }
    if replayed > 0 {
        persist_recovered_snapshot(repo, card_id, snapshot).await?;
    }
    if replayed > 0 {
        tracing::info!(
            card_id,
            wave_id = %wave_id,
            watermark,
            replayed,
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
        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => repo
            .card_role_get(card_id.as_str())
            .await
            .map_err(Into::into),
        _ => Ok(None),
    }
}

async fn persist_recovered_snapshot(
    repo: Arc<dyn Repo>,
    card_id: &str,
    snapshot: &HarnessSnapshot,
) -> Result<()> {
    let runtime_state = serde_json::to_value(snapshot)?;
    let runtime_id = snapshot_runtime_id(repo.as_ref(), card_id).await?;
    write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::runtime_set_handle_state_tx(tx, &runtime_id, Some(runtime_state))
                .await?;
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
