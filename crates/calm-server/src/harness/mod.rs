pub mod config;
pub mod observation;
pub mod registry;
pub mod run_loop;
pub mod snapshot;
pub mod state;

use std::sync::Arc;

use crate::db::{Repo, write_in_tx_typed};
use crate::error::Result;
use crate::ids::CardId;
use crate::runtime_repo::CardRuntime;
use crate::shared_codex_appserver::SharedCodexAppServer;

pub use config::HarnessConfig;
pub use observation::{HookKind, Observation};
pub use registry::HarnessRegistry;
pub use run_loop::{SpecHarness, SpecHarnessParams};
pub use snapshot::{HARNESS_MODE, HarnessPhaseTag, HarnessSnapshot, is_harness_snapshot_value};
pub use state::{HarnessState, IssuingKind, run_status_for};

pub async fn spawn_recovered_harness(
    repo: Arc<dyn Repo>,
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
    rehydrate_spec_push_queue(repo.clone(), &runtime.card_id, &mut snapshot).await?;
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
) -> Result<()> {
    let rows = repo.spec_card_queued_observations(card_id).await?;
    if rows.is_empty() {
        return Ok(());
    }
    let row_ids = rows.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
    for (_, _, text) in rows {
        let obs =
            serde_json::from_str::<Observation>(&text).unwrap_or(Observation::WaveGoal { text });
        snapshot.pending_queue.push(obs);
    }

    let runtime_state = serde_json::to_value(&snapshot)?;
    let runtime_id = snapshot_runtime_id(repo.as_ref(), card_id).await?;
    let ids_for_delete = row_ids.clone();
    write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::runtime_set_handle_state_tx(tx, &runtime_id, Some(runtime_state))
                .await?;
            if !ids_for_delete.is_empty() {
                let placeholders = std::iter::repeat_n("?", ids_for_delete.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!("DELETE FROM spec_push_queue WHERE id IN ({placeholders})");
                let mut q = sqlx::query(&sql);
                for id in ids_for_delete {
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
    daemon: Arc<SharedCodexAppServer>,
    registry: &HarnessRegistry,
) -> Result<usize> {
    let runtimes = repo.runtimes_recover_orphans_on_boot().await?;
    let mut recovered = 0usize;
    for runtime in runtimes {
        if spawn_recovered_harness(repo.clone(), daemon.clone(), registry, runtime)
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
