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
use crate::session_projection_repo::WorkerSessionProjection;
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
    runtime: WorkerSessionProjection,
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
                "workspace.leased",
                "workspace.released",
                "forge.scan.completed",
                "forge.pr.opened",
                "forge.pr.checks",
                "forge.issue.closed",
                "worktree.provisioned",
                "forge.pr.merged",
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
            crate::db::sqlite::session_set_handle_state_tx(tx, &runtime_id, Some(runtime_state))
                .await?;
            Ok(())
        })
    })
    .await
}

async fn snapshot_runtime_id(repo: &dyn Repo, card_id: &str) -> Result<String> {
    let runtime = repo
        .session_projection_active_for_card(&card_id.to_string())
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
    let runtimes = repo.session_projection_recover_harnesses_on_boot().await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::Duration;

    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::{
        SqlxRepo, append_decision_event_in_tx, card_create_with_id_tx, session_start_runtime_tx,
    };
    use crate::event::EventScope;
    use crate::ids::ActorId;
    use crate::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use crate::shared_codex_appserver::SharedCodexAppServer;
    use crate::wave_cove_cache::WaveCoveCache;
    use calm_truth::decision_gate::PermissiveGate;
    use serde_json::json;

    #[tokio::test]
    async fn workspace_leased_replays_into_recovered_harness_and_issues_turn() {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let role_cache = CardRoleCache::new();
        let wave_cove_cache = WaveCoveCache::new();
        let cove = repo
            .cove_create(NewCove {
                name: "workspace replay".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "workspace replay".into(),
                sort: None,
                cwd: "/tmp".into(),
                workflow_id: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        wave_cove_cache.insert(wave.id.clone(), cove.id.clone());

        let mut tx = repo.pool().begin().await.unwrap();
        let spec_card = card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1, "spec_harness": true}),
            },
            CardRole::Spec,
            false,
            &role_cache,
        )
        .await
        .unwrap();
        let worker_card = card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1}),
            },
            CardRole::Worker,
            true,
            &role_cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let lease_id = "lease-replay".to_string();
        let workspace_path = "/tmp/workspace-replay".to_string();
        let workspace_event = Event::WorkspaceLeased {
            wave_id: wave.id.clone(),
            card_id: worker_card.id.clone(),
            lease_id: lease_id.clone(),
            path: workspace_path.clone(),
        };
        let scope = EventScope::Card {
            card: worker_card.id.clone(),
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        };
        let mut tx = repo.pool().begin().await.unwrap();
        let event_id = append_decision_event_in_tx(
            &mut tx,
            &PermissiveGate,
            &ActorId::KernelDispatcher,
            &scope,
            None,
            &workspace_event,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let runtime_id = new_id();
        let thread_id = "thread-workspace-recovered".to_string();
        let mut snapshot = HarnessSnapshot::initial(0, vec![]);
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some(thread_id.clone());
        let mut tx = repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: runtime_id.clone(),
                card_id: spec_card.id.to_string(),
                kind: WorkerSessionKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some(thread_id.clone()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        replay_harness_events_since(
            repo.clone(),
            spec_card.id.as_str(),
            &wave.id,
            0,
            &mut snapshot,
        )
        .await
        .unwrap();
        assert_eq!(
            snapshot.pending_queue,
            vec![Observation::WorkspaceLeased {
                wave_id: wave.id.clone(),
                card_id: worker_card.id.clone(),
                lease_id: lease_id.clone(),
                path: workspace_path.clone(),
            }]
        );
        assert_eq!(snapshot.pending_envelope_ids, vec![Some(event_id)]);
        assert_eq!(snapshot.push_watermark, event_id);
        assert!(
            !snapshot.pending_queue[0].is_hard_fire(),
            "workspace observations must remain soft-fire"
        );

        let runtime = repo
            .session_projection_by_id(&runtime_id)
            .await
            .unwrap()
            .unwrap();
        let stored: HarnessSnapshot =
            serde_json::from_value(runtime.handle_state_json.clone().unwrap()).unwrap();
        assert_eq!(stored.pending_queue, snapshot.pending_queue);
        assert_eq!(stored.pending_envelope_ids, vec![Some(event_id)]);
        assert_eq!(stored.push_watermark, event_id);

        let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
        let registry = HarnessRegistry::new();
        let handle = spawn_recovered_harness(
            repo.clone(),
            EventBus::new(),
            role_cache,
            wave_cove_cache,
            daemon.clone(),
            &registry,
            runtime,
        )
        .await
        .unwrap()
        .expect("recovered harness");
        assert!(registry.get(&runtime_id).is_some());

        tokio::time::timeout(Duration::from_millis(750), async {
            loop {
                if daemon.turn_start_count_for_test() > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recovered workspace lease backlog should issue a turn");
        assert_eq!(daemon.turn_start_count_for_test(), 1);

        let after_issue = handle.snapshot().await;
        assert!(after_issue.pending_queue.is_empty());
        assert!(after_issue.pending_envelope_ids.is_empty());
        assert_eq!(after_issue.push_watermark, event_id);
        assert_eq!(
            after_issue.last_thread_id.as_deref(),
            Some(thread_id.as_str())
        );

        handle.shutdown().await.unwrap();
    }
}
