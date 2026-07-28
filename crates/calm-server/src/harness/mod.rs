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
pub use registry::{HarnessRegistry, HarnessReservation, ReservationId, Slot};
pub use run_loop::{SpecHarness, SpecHarnessParams};
pub use snapshot::{HARNESS_MODE, HarnessPhaseTag, HarnessSnapshot, is_harness_snapshot_value};
pub use state::{HarnessState, IssuingKind, run_status_for};

/// #953 §5 — how [`spawn_recovered_harness`] claims the registry slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimMode {
    /// Boot recovery + user resume: today's shutdown-replace semantics via
    /// [`HarnessRegistry::reserve_replacing`] — an existing Live harness is
    /// shut down, an in-flight reservation is superseded. Carries NO daemon
    /// eligibility gate: user resume semantics are unchanged.
    Replace,
    /// Deferred (post-heal) recovery: [`HarnessRegistry::try_reserve`] as
    /// the claim — an occupied slot (Live OR Reserved) means the user
    /// already touched this runtime, so skip without shutting anything
    /// down. Shutdown-replace is unreachable from this mode by construction.
    ///
    /// PR2 review D1(a) — `expected_generation` is the Running incarnation
    /// the deferred pass acted on: the replay between eligibility and claim
    /// can be long, so eligibility (readiness still `running` with this
    /// generation) is re-checked at the claim boundary, immediately before
    /// `try_reserve`. On mismatch nothing is reserved and the pass reports
    /// [`RecoveryOutcome::DaemonIneligible`].
    SkipIfClaimed { expected_generation: u64 },
}

/// Outcome of [`spawn_recovered_harness`].
pub enum RecoveryOutcome {
    /// A harness was built and installed under the claim.
    Installed(SpecHarness),
    /// Nothing installed: the runtime is not recoverable (missing card /
    /// snapshot), the slot was already claimed ([`ClaimMode::SkipIfClaimed`]),
    /// or the install lost against a newer claim (stale-install shutdown).
    Skipped,
    /// [`ClaimMode::SkipIfClaimed`] only: the claim-boundary daemon re-check
    /// failed — the daemon left Running or changed generation during replay.
    /// Nothing was reserved or installed; the deferred pass must abandon and
    /// re-arm on the readiness watch.
    DaemonIneligible,
}

impl RecoveryOutcome {
    /// The installed handle, if any ([`ClaimMode::Replace`] callers keep
    /// their old `Option` semantics through this).
    pub fn installed(self) -> Option<SpecHarness> {
        match self {
            Self::Installed(handle) => Some(handle),
            Self::Skipped | Self::DaemonIneligible => None,
        }
    }
}

/// #953 §5 — install the just-built harness under the reservation, or shut
/// it down if the reservation went stale (superseded / slot re-claimed):
/// a failed install must never leak the handle's run loop (test 14 iv).
async fn install_or_shutdown(
    reservation: HarnessReservation,
    handle: SpecHarness,
) -> Result<Option<SpecHarness>> {
    if reservation.install(handle.clone()) {
        Ok(Some(handle))
    } else {
        handle.shutdown().await?;
        Ok(None)
    }
}

// Pre-existing 7-arg surface (every arg an independently-owned AppState
// part) + the #953 ClaimMode; the boot/user/deferred callers already thread
// these parts individually, so a params struct would be churn without
// clarity.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_recovered_harness(
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    daemon: Arc<SharedCodexAppServer>,
    registry: &HarnessRegistry,
    runtime: WorkerSessionProjection,
    claim_mode: ClaimMode,
) -> Result<RecoveryOutcome> {
    let Some(card) = repo.card_get(&runtime.card_id).await? else {
        return Ok(RecoveryOutcome::Skipped);
    };
    let Some(state_json) = runtime.handle_state_json.clone() else {
        return Ok(RecoveryOutcome::Skipped);
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
    // #953 §5 placement invariant: the reservation sits exactly where the
    // old `remove()` sat — after recovery replay, immediately before handle
    // construction/install — so the accepted reserve→install residual is
    // provably no wider than the old remove-vs-insert window.
    let reservation = match claim_mode {
        ClaimMode::Replace => {
            let (reservation, previous_live) = registry.reserve_replacing(runtime_id.clone());
            if let Some(existing) = previous_live {
                existing.shutdown().await?;
            }
            reservation
        }
        ClaimMode::SkipIfClaimed {
            expected_generation,
        } => {
            // #953 PR2 review D1(a) — claim-boundary daemon re-check: the
            // replay above can be long, so re-verify the eligibility the
            // deferred pass acted on IMMEDIATELY before the claim. Consults
            // the readiness watch (never the daemon core lock — no core
            // acquisition near registry entry ops); transition ENTRY
            // publishes `running: false`, so a transitional daemon is
            // rejected here too. No reservation exists yet, so there is
            // nothing to release on failure.
            let readiness = *daemon.readiness_receiver().borrow();
            if !readiness.running || readiness.generation != expected_generation {
                tracing::info!(
                    runtime_id = %runtime_id,
                    running = readiness.running,
                    generation = readiness.generation,
                    expected_generation,
                    "deferred harness recovery: daemon left Running during replay; abandoning claim"
                );
                return Ok(RecoveryOutcome::DaemonIneligible);
            }
            match registry.try_reserve(runtime_id.clone()) {
                Some(reservation) => reservation,
                None => {
                    tracing::info!(
                        runtime_id = %runtime_id,
                        "deferred harness recovery: runtime already claimed (Live or Reserved); skipping"
                    );
                    return Ok(RecoveryOutcome::Skipped);
                }
            }
        }
    };
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
    Ok(match install_or_shutdown(reservation, handle).await? {
        Some(handle) => RecoveryOutcome::Installed(handle),
        None => RecoveryOutcome::Skipped,
    })
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
                "review.round",
                "ratify.requested",
                "ratify.resolved",
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
            ClaimMode::Replace,
        )
        .await?
        .installed()
        .is_some()
        {
            recovered += 1;
        }
    }
    Ok(recovered)
}

/// Fixtures-only deterministic-race hook (#953 test 8): fired once per
/// runtime AFTER the per-runtime eligibility check and BEFORE the
/// `try_reserve` claim.
#[cfg(feature = "fixtures")]
pub type PostEligibilityHook =
    std::sync::Arc<dyn Fn(&crate::session_projection_repo::RuntimeId) + Send + Sync>;

/// #953 §5 — everything the deferred (post-heal) harness recovery task
/// needs. Cloned out of `AppState` at arm time so the task owns its parts.
pub struct DeferredRecoveryParams {
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub card_role_cache: CardRoleCache,
    pub wave_cove_cache: WaveCoveCache,
    pub daemon: Arc<SharedCodexAppServer>,
    pub registry: HarnessRegistry,
    /// Fixtures-only deterministic-race hook (#953 test 8): fired once per
    /// runtime AFTER the per-runtime eligibility check (readiness still
    /// running, generation unchanged) and BEFORE the `try_reserve` claim —
    /// the window where a concurrent user registration must win, and (PR2
    /// review D1) where a daemon transition during replay must make the
    /// claim-boundary re-check abandon the pass.
    #[cfg(feature = "fixtures")]
    pub post_eligibility_hook: Option<PostEligibilityHook>,
}

/// #953 §5 — deferred claim-based harness recovery. Armed ONLY when the boot
/// daemon spawn failed (boot used to skip spec-harness recovery forever);
/// triggered by the first `running: true` observed on the supervisor's
/// readiness watch. Claim-based: fresh DB re-read of recoverable runtimes at
/// trigger time, per-runtime readiness re-check (running + unchanged
/// generation) both before replay AND at the claim boundary immediately
/// before `try_reserve` (PR2 review D1(a) — replay can be long), with
/// `try_reserve` as the claim ([`ClaimMode::SkipIfClaimed`]) so a runtime
/// the user already resumed is never shutdown-replaced.
pub async fn recover_harnesses_deferred(params: DeferredRecoveryParams) {
    let mut readiness = params.daemon.readiness_receiver();
    'arm: loop {
        // Wait for a running daemon (first heal success). Lifecycle (PR2
        // review D3): this task is spawned DETACHED
        // (`AppState::arm_deferred_harness_recovery`) and owns an Arc of the
        // supervisor via `params.daemon`, so the watch sender can never drop
        // while we wait — the `changed()` Err arm below is defensive dead
        // code, not the documented exit. The task actually ends by
        // completing a recovery pass (the `return` at the bottom) or at
        // process teardown.
        let observed = loop {
            let current = *readiness.borrow_and_update();
            if current.running {
                break current;
            }
            if readiness.changed().await.is_err() {
                return;
            }
        };
        tracing::info!(
            generation = observed.generation,
            "shared daemon became ready; running deferred spec harness recovery"
        );
        // Fresh re-read: the recoverable set may have changed since boot
        // (user resumes, shutdowns, new runtimes).
        let runtimes = match params
            .repo
            .session_projection_recover_harnesses_on_boot()
            .await
        {
            Ok(runtimes) => runtimes,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "deferred harness recovery: recoverable-runtime read failed; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue 'arm;
            }
        };
        let mut recovered = 0usize;
        for runtime in runtimes {
            // Per-runtime eligibility: still running, same generation as the
            // readiness we acted on. On change, re-read readiness and either
            // continue (still running, new incarnation) or re-arm (failed
            // again).
            let current = *readiness.borrow();
            if !current.running || current.generation != observed.generation {
                tracing::info!(
                    running = current.running,
                    generation = current.generation,
                    "deferred harness recovery: daemon readiness changed mid-pass; re-evaluating"
                );
                continue 'arm;
            }
            #[cfg(feature = "fixtures")]
            if let Some(hook) = params.post_eligibility_hook.as_ref() {
                hook(&runtime.id);
            }
            let runtime_id = runtime.id.clone();
            match spawn_recovered_harness(
                params.repo.clone(),
                params.events.clone(),
                params.card_role_cache.clone(),
                params.wave_cove_cache.clone(),
                params.daemon.clone(),
                &params.registry,
                runtime,
                ClaimMode::SkipIfClaimed {
                    expected_generation: observed.generation,
                },
            )
            .await
            {
                Ok(RecoveryOutcome::Installed(_)) => recovered += 1,
                Ok(RecoveryOutcome::Skipped) => {}
                Ok(RecoveryOutcome::DaemonIneligible) => {
                    // PR2 review D1(a) — the claim-boundary re-check failed
                    // (daemon left Running or was replaced during replay).
                    // Nothing was reserved; abandon this pass and re-arm the
                    // wait loop so recovery resumes on the next heal.
                    tracing::info!(
                        runtime_id = %runtime_id,
                        "deferred harness recovery: daemon readiness changed at the claim boundary; re-arming"
                    );
                    continue 'arm;
                }
                Err(e) => {
                    // Per-runtime failures don't abort the pass: recover
                    // what can be recovered, log the rest.
                    tracing::warn!(
                        runtime_id = %runtime_id,
                        error = %e,
                        "deferred harness recovery: runtime recovery failed; continuing"
                    );
                }
            }
        }
        tracing::info!(recovered, "deferred spec harness recovery complete");
        return;
    }
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

    /// #953 test 14(iv) — install failure shuts down the just-built harness
    /// (no leaked run loop): a reservation superseded between reserve and
    /// install makes `install_or_shutdown` return `Ok(None)` after shutting
    /// the handle down, leaving the newer claim untouched.
    #[tokio::test]
    async fn install_failure_shuts_down_just_built_harness() {
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone());
        let registry = HarnessRegistry::new();
        let runtime_id = "rt-install-failure".to_string();

        let reservation = registry.try_reserve(runtime_id.clone()).expect("vacant");
        // Concurrent replace lands between reserve and install.
        let (winner, previous_live) = registry.reserve_replacing(runtime_id.clone());
        assert!(previous_live.is_none());

        let handle = SpecHarness::run(SpecHarnessParams {
            runtime_id: runtime_id.clone(),
            wave_id: WaveId::from("wave-install-failure".to_string()),
            card_id: CardId::from("card-install-failure".to_string()),
            thread_id: None,
            repo,
            events: EventBus::new(),
            card_role_cache: CardRoleCache::new(),
            wave_cove_cache: WaveCoveCache::new(),
            daemon,
            config: HarnessConfig::default(),
            snapshot: HarnessSnapshot::initial(0, vec![]),
        });
        let installed = install_or_shutdown(reservation, handle.clone())
            .await
            .unwrap();
        assert!(installed.is_none(), "stale install must report failure");
        // The run loop is gone: its observation channel no longer accepts.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if handle
                    .observe(Observation::WaveGoal {
                        text: "leaked?".into(),
                    })
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("shut-down harness must stop accepting observations");
        // The newer claim was untouched throughout.
        assert!(registry.get(&runtime_id).is_none());
        drop(winner);
        assert!(registry.try_reserve(runtime_id).is_some());
    }

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
    use calm_types::event::{ChannelVerdict, ChannelVerdictKind, ReviewSubject};
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
                workflow_input: None,
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
                title: None,
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
                title: None,
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
            ClaimMode::Replace,
        )
        .await
        .unwrap()
        .installed()
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

    #[tokio::test]
    async fn review_round_replays_into_recovered_harness_and_issues_turn() {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let role_cache = CardRoleCache::new();
        let wave_cove_cache = WaveCoveCache::new();
        let cove = repo
            .cove_create(NewCove {
                name: "review replay".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: cove.id.clone(),
                title: "review replay".into(),
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
                title: None,
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
        tx.commit().await.unwrap();

        let review_event = Event::ReviewRound {
            wave_id: wave.id.clone(),
            subject: ReviewSubject {
                phase: "impl".into(),
                slice_id: "5b".into(),
                pr_number: Some(760),
            },
            head_sha: Some("head-sha".into()),
            n: 1,
            cap: 8,
            converged: false,
            channels: vec![
                ChannelVerdict {
                    role: "design-correctness".into(),
                    verdict: ChannelVerdictKind::ChangesRequested,
                },
                ChannelVerdict {
                    role: "failure-path".into(),
                    verdict: ChannelVerdictKind::Approved,
                },
            ],
            root_cause: Some("tests failing".into()),
            idempotency_key: format!("review.round:{}:impl:5b:760:1", wave.id),
        };
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        };
        let mut tx = repo.pool().begin().await.unwrap();
        let event_id = append_decision_event_in_tx(
            &mut tx,
            &PermissiveGate,
            &ActorId::AiSpec(spec_card.id.clone()),
            &scope,
            None,
            &review_event,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let runtime_id = new_id();
        let thread_id = "thread-review-recovered".to_string();
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
            vec![Observation::ReviewRound {
                wave_id: wave.id.clone(),
                phase: "impl".into(),
                slice_id: "5b".into(),
                pr_number: Some(760),
                head_sha: Some("head-sha".into()),
                n: 1,
                cap: 8,
                converged: false,
            }]
        );
        assert_eq!(snapshot.pending_envelope_ids, vec![Some(event_id)]);
        assert_eq!(snapshot.push_watermark, event_id);
        assert!(
            snapshot.pending_queue[0].is_hard_fire(),
            "review.round observations must hard-fire"
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
            ClaimMode::Replace,
        )
        .await
        .unwrap()
        .installed()
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
        .expect("recovered review.round backlog should issue a turn");
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
