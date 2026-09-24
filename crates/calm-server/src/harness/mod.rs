pub mod backend;
pub(crate) mod catch_up;
pub mod config;
pub mod lock;
pub mod observation;
pub mod profile;
pub mod queue;
mod recovery_briefing;
pub mod registry;
mod result_receipt;
pub mod run_loop;
pub mod snapshot;
pub mod state;
pub mod token_usage;
pub(crate) mod turn_outcome;

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use crate::card_role_cache::CardRoleCache;
use crate::db::{Repo, write_in_tx_typed};
use crate::error::Result;
#[cfg(test)]
use crate::event::Event;
use crate::event::EventBus;
use crate::ids::{CardId, TrackId};
use crate::model::CardRole;
use crate::per_card_lock::{KeyedLocks, lock_key};
use crate::session_projection_repo::{WorkerSessionProjection, WorkerSessionState};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::track_area_cache::TrackAreaCache;

pub use backend::PlannerBackend;
pub use config::HarnessConfig;
pub use lock::PushLockGuard;
pub use observation::{HookKind, Observation};
pub use queue::{QueueEntry, QueueEntryId};
pub use registry::{HarnessRegistry, HarnessReservation, ReservationId, Slot};
pub use run_loop::{
    MAX_PENDING_QUEUE_LEN, PlannerHarness, PlannerHarnessParams, SteerApplied, SteerRefused,
    SteerResult,
};
pub use snapshot::{
    HARNESS_MODE, HarnessPhaseTag, HarnessSnapshot, QueueEntryMeta, is_harness_snapshot_value,
};
pub use state::{HarnessState, IssuingKind, run_status_for};
pub use token_usage::{BASELINE_TOKENS, TokenUsage};

/// Recovery callers must explicitly provide the single-track deletion fence they coordinate with.
pub type TrackDeleteLocks = KeyedLocks;

pub fn new_track_delete_locks() -> TrackDeleteLocks {
    crate::per_card_lock::new_keyed_locks()
}

/// How [`spawn_recovered_harness`] claims the registry slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimMode {
    /// Boot recovery + user resume: an existing Live harness is shut down, an in-flight
    /// reservation is superseded. No daemon eligibility gate.
    Replace,
    /// Deferred (post-heal) recovery: an occupied slot means the user already touched this runtime,
    /// so skip without shutting anything down. `expected_generation` is re-checked at the claim
    /// boundary because the replay before it can be long.
    SkipIfClaimed { expected_generation: u64 },
}

pub enum RecoveryOutcome {
    Installed(PlannerHarness),
    /// Nothing installed: not recoverable, slot already claimed, or the install lost against a newer claim.
    Skipped,
    /// [`ClaimMode::SkipIfClaimed`] only: the daemon left Running or changed generation during
    /// replay; nothing was reserved, the deferred pass must re-arm.
    DaemonIneligible,
}

impl RecoveryOutcome {
    pub fn installed(self) -> Option<PlannerHarness> {
        match self {
            Self::Installed(handle) => Some(handle),
            Self::Skipped | Self::DaemonIneligible => None,
        }
    }
}

/// Install under the reservation, or shut the just-built harness down if the reservation went
/// stale: a failed install must never leak the handle's run loop.
async fn install_or_shutdown(
    reservation: HarnessReservation,
    handle: PlannerHarness,
) -> Result<Option<PlannerHarness>> {
    if reservation.install(handle.clone()) {
        Ok(Some(handle))
    } else {
        handle.shutdown().await?;
        Ok(None)
    }
}

pub(crate) fn effective_runtime_thread_id(runtime: &WorkerSessionProjection) -> Option<String> {
    runtime
        .thread_id
        .clone()
        .filter(|thread_id| !thread_id.trim().is_empty())
        .or_else(|| {
            runtime
                .handle_state_json
                .as_ref()
                .and_then(|snapshot| snapshot.get("last_thread_id"))
                .and_then(serde_json::Value::as_str)
                .filter(|thread_id| !thread_id.trim().is_empty())
                .map(str::to_owned)
        })
}

// The explicit delete fence is load-bearing: a caller cannot recover a runtime without choosing
// which server instance's destructive boundary it coordinates with.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_recovered_harness(
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    daemon: Arc<SharedCodexAppServer>,
    registry: &HarnessRegistry,
    track_delete_locks: &KeyedLocks,
    runtime: WorkerSessionProjection,
    claim_mode: ClaimMode,
) -> Result<RecoveryOutcome> {
    let Some(card) = repo.card_get(&runtime.card_id).await? else {
        return Ok(RecoveryOutcome::Skipped);
    };
    let role = repo.card_role_get(card.id.as_str()).await?;
    let Some(track) = repo.track_get(card.track_id.as_str()).await? else {
        return Ok(RecoveryOutcome::Skipped);
    };
    if !crate::workspace_recycle::workspace_allows_runtime_recovery(&track) {
        tracing::warn!(
            runtime_id = %runtime.id,
            track_id = %track.id,
            "refusing harness recovery: managed workspace is not restored at its owned path"
        );
        return Ok(RecoveryOutcome::Skipped);
    }
    if role == Some(CardRole::Planner) && track.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE)
    {
        tracing::warn!(
            runtime_id = %runtime.id,
            card_id = %card.id,
            track_id = %track.id,
            "recovered planner harness is disabled for area chat track; skipping runtime"
        );
        return Ok(RecoveryOutcome::Skipped);
    }
    let Some(state_json) = runtime.handle_state_json.clone() else {
        return Ok(RecoveryOutcome::Skipped);
    };
    if effective_runtime_thread_id(&runtime)
        .as_deref()
        .is_some_and(|thread_id| daemon.turn_thread_is_sealed(thread_id))
    {
        return Ok(RecoveryOutcome::Skipped);
    }
    let mut snapshot = HarnessSnapshot::from_value_strict(state_json);
    // Catch-up is a PLANNER-push catch-up: replaying it into a conversation harness would inject
    // a backlog it was never meant to see. Unknown/absent role falls into the no-replay arm (fail-closed).
    if role == Some(CardRole::Planner) {
        let catch_up_watermark = snapshot.push_watermark;
        replay_harness_events_since(
            repo.clone(),
            &runtime.card_id,
            &card.track_id,
            catch_up_watermark,
            &mut snapshot,
        )
        .await?;
    }
    let runtime_id = runtime.id.clone();
    let track_id = card.track_id.clone();
    // Replay may be long, so claim the lifecycle fence only at the installation boundary and then
    // revalidate every row DELETE can remove.
    let _track_delete_guard = lock_key(track_delete_locks, track_id.as_str()).await;
    let Some(current_card) = repo.card_get(&runtime.card_id).await? else {
        return Ok(RecoveryOutcome::Skipped);
    };
    if current_card.track_id != track_id || repo.track_get(track_id.as_str()).await?.is_none() {
        return Ok(RecoveryOutcome::Skipped);
    }
    let Some(current_runtime) = repo.session_projection_by_id(&runtime_id).await? else {
        return Ok(RecoveryOutcome::Skipped);
    };
    if !matches!(
        current_runtime.status,
        WorkerSessionState::Starting
            | WorkerSessionState::Running
            | WorkerSessionState::Idle
            | WorkerSessionState::TurnPending
    ) {
        return Ok(RecoveryOutcome::Skipped);
    }
    // The reservation sits after recovery replay, immediately before handle construction/install.
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
            // Re-verify eligibility IMMEDIATELY before the claim via the readiness watch (never the daemon
            // core lock); transition entry publishes `running: false`, so a transitional daemon is rejected too.
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
    let handle = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id.clone(),
        track_id: card.track_id,
        card_id: CardId::from(runtime.card_id.clone()),
        // A row with `thread_id = ''` would otherwise win as `Some("")` over the snapshot's valid
        // `last_thread_id`, and the recovered harness would issue turns against an empty thread.
        thread_id: effective_runtime_thread_id(&runtime),
        repo,
        events,
        card_role_cache,
        track_area_cache,
        backend: daemon.into(),
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
    track_id: &TrackId,
    watermark: i64,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    let observations =
        catch_up::observations_since(repo.as_ref(), track_id, watermark, None).await?;
    let mut replayed = 0usize;
    let mut entries: VecDeque<queue::QueueEntry> = snapshot.pending_entries().into();
    for (event_id, obs) in observations {
        // A dispatcher observation can never be a `UserMessage`; `QueueEntry::system` is the fence.
        // `continue` also skips the watermark, so such a row pins it and is re-attempted every boot (fail-closed).
        let entry = match queue::QueueEntry::system(obs, Some(event_id)) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    card_id,
                    event_id,
                    error = %error,
                    "harness recovery: refusing to replay a user-message observation \
                     from the dispatcher stream"
                );
                continue;
            }
        };
        // Same early fold the live enqueue applies: a replayed edit session is one entry, not one per save.
        if !matches!(
            queue::try_fold_report_edit_tail(&mut entries, &entry),
            queue::FoldOutcome::Folded { .. }
        ) {
            entries.push_back(entry);
        }
        snapshot.push_watermark = snapshot.push_watermark.max(event_id);
        replayed += 1;
    }
    if replayed > 0 {
        snapshot.set_pending_entries(entries.into());
        persist_recovered_snapshot(repo, card_id, snapshot).await?;
    }
    if replayed > 0 {
        tracing::info!(
            card_id,
            track_id = %track_id,
            watermark,
            replayed,
            "harness recovery: replayed planner push catch-up events into pending queue",
        );
    }
    Ok(())
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
    track_area_cache: TrackAreaCache,
    daemon: Arc<SharedCodexAppServer>,
    registry: &HarnessRegistry,
    track_delete_locks: &KeyedLocks,
) -> Result<usize> {
    let runtimes = repo.session_projection_recover_harnesses_on_boot().await?;
    let mut recovered = 0usize;
    for runtime in runtimes {
        let runtime_id = runtime.id.clone();
        match spawn_recovered_harness(
            repo.clone(),
            events.clone(),
            card_role_cache.clone(),
            track_area_cache.clone(),
            daemon.clone(),
            registry,
            track_delete_locks,
            runtime,
            ClaimMode::Replace,
        )
        .await
        {
            Ok(RecoveryOutcome::Installed(_)) => recovered += 1,
            Ok(RecoveryOutcome::Skipped | RecoveryOutcome::DaemonIneligible) => {}
            Err(error) => tracing::warn!(
                runtime_id = %runtime_id,
                error = %error,
                "boot harness recovery: runtime recovery failed; continuing"
            ),
        }
    }
    Ok(recovered)
}

#[derive(Clone)]
pub struct HarnessRecoveryContext {
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    daemon: Arc<SharedCodexAppServer>,
    registry: HarnessRegistry,
    track_delete_locks: KeyedLocks,
}

impl HarnessRecoveryContext {
    pub fn new(
        repo: Arc<dyn Repo>,
        events: EventBus,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
        daemon: Arc<SharedCodexAppServer>,
        registry: HarnessRegistry,
        track_delete_locks: KeyedLocks,
    ) -> Self {
        Self {
            repo,
            events,
            card_role_cache,
            track_area_cache,
            daemon,
            registry,
            track_delete_locks,
        }
    }
}

/// Reinstall recoverable harnesses for surviving tracks after an aborted destructive saga.
/// Deletion guards must be dropped before calling. Sealed threads are skipped.
pub async fn recover_harnesses_for_tracks(
    context: &HarnessRecoveryContext,
    track_ids: &HashSet<TrackId>,
) -> Result<usize> {
    let runtimes = context
        .repo
        .session_projection_recover_harnesses_on_boot()
        .await?;
    let mut recovered = 0;
    for runtime in runtimes {
        let Some(card) = context.repo.card_get(&runtime.card_id).await? else {
            continue;
        };
        if !track_ids.contains(&card.track_id)
            || effective_runtime_thread_id(&runtime)
                .as_deref()
                .is_some_and(|thread_id| context.daemon.turn_thread_is_sealed(thread_id))
        {
            continue;
        }
        let runtime_id = runtime.id.clone();
        match spawn_recovered_harness(
            context.repo.clone(),
            context.events.clone(),
            context.card_role_cache.clone(),
            context.track_area_cache.clone(),
            context.daemon.clone(),
            &context.registry,
            &context.track_delete_locks,
            runtime,
            ClaimMode::Replace,
        )
        .await
        {
            Ok(RecoveryOutcome::Installed(_)) => recovered += 1,
            Ok(RecoveryOutcome::Skipped | RecoveryOutcome::DaemonIneligible) => {}
            Err(error) => tracing::warn!(
                runtime_id = %runtime_id,
                error = %error,
                "aborted deletion: harness recovery failed; continuing"
            ),
        }
    }
    Ok(recovered)
}

/// Fixtures-only race hook: fired once per runtime AFTER the eligibility check and BEFORE `try_reserve`.
#[cfg(feature = "fixtures")]
pub type PostEligibilityHook = std::sync::Arc<dyn Fn(&String) + Send + Sync>;

/// Everything the deferred (post-heal) harness recovery task needs.
pub struct DeferredRecoveryParams {
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub card_role_cache: CardRoleCache,
    pub track_area_cache: TrackAreaCache,
    pub daemon: Arc<SharedCodexAppServer>,
    pub registry: HarnessRegistry,
    pub track_delete_locks: KeyedLocks,
    /// Fixtures-only race hook: fired once per runtime AFTER the eligibility check and BEFORE
    /// `try_reserve` — the window where a concurrent user registration must win.
    #[cfg(feature = "fixtures")]
    pub post_eligibility_hook: Option<PostEligibilityHook>,
}

/// Deferred claim-based harness recovery, armed only when the boot daemon spawn failed and
/// triggered by the first `running: true` on the readiness watch. Uses `try_reserve` as the
/// claim so a runtime the user already resumed is never shutdown-replaced.
pub async fn recover_harnesses_deferred(params: DeferredRecoveryParams) {
    let mut readiness = params.daemon.readiness_receiver();
    'arm: loop {
        // Wait for a running daemon. The task owns an Arc of the supervisor via `params.daemon`, so the
        // watch sender cannot drop while we wait; the `changed()` Err arm is defensive.
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
            "shared daemon became ready; running deferred planner harness recovery"
        );
        // Fresh re-read: the recoverable set may have changed since boot.
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
            // Per-runtime eligibility: still running, same generation as the readiness we acted on.
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
                params.track_area_cache.clone(),
                params.daemon.clone(),
                &params.registry,
                &params.track_delete_locks,
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
                    // Nothing was reserved; abandon this pass and re-arm so recovery resumes on the next heal.
                    tracing::info!(
                        runtime_id = %runtime_id,
                        "deferred harness recovery: daemon readiness changed at the claim boundary; re-arming"
                    );
                    continue 'arm;
                }
                Err(e) => {
                    // Per-runtime failures don't abort the pass.
                    tracing::warn!(
                        runtime_id = %runtime_id,
                        error = %e,
                        "deferred harness recovery: runtime recovery failed; continuing"
                    );
                }
            }
        }
        tracing::info!(recovered, "deferred planner harness recovery complete");
        return;
    }
}

pub fn initial_snapshot_with_goal(goal: Option<String>) -> HarnessSnapshot {
    let entries = goal
        .filter(|text| !text.trim().is_empty())
        .map(|text| {
            vec![
                QueueEntry::system(Observation::TrackGoal { text }, None)
                    .expect("a track goal is never a user message"),
            ]
        })
        .unwrap_or_default();
    HarnessSnapshot::initial(0, entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::Duration;

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

        let handle = PlannerHarness::run(PlannerHarnessParams {
            worker_session_id: runtime_id.clone(),
            track_id: TrackId::from("track-install-failure".to_string()),
            card_id: CardId::from("card-install-failure".to_string()),
            thread_id: None,
            repo,
            events: EventBus::new(),
            card_role_cache: CardRoleCache::new(),
            track_area_cache: TrackAreaCache::new(),
            backend: daemon.into(),
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
                    .observe(Observation::TrackGoal {
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
    use crate::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use crate::shared_codex_appserver::SharedCodexAppServer;
    use crate::track_area_cache::TrackAreaCache;
    use calm_types::event::{ChannelVerdict, ChannelVerdictKind, ReviewSubject};
    use serde_json::json;

    #[tokio::test]
    async fn boot_catch_up_skips_quiet_kinds_and_stale_stop_hooks_and_replays_the_wakes() {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let role_cache = CardRoleCache::new();
        let track_area_cache = TrackAreaCache::new();
        let area = repo
            .area_create(NewArea {
                name: "catch-up parity".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "catch-up parity".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        track_area_cache.insert(track.id.clone(), area.id.clone());

        let mut tx = repo.pool().begin().await.unwrap();
        let planner_card = card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1, "planner_harness": true, "planner_provider": "codex"}),
            },
            CardRole::Planner,
            false,
            &role_cache,
        )
        .await
        .unwrap();
        async fn worker_card(
            tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
            track_id: &TrackId,
            role_cache: &CardRoleCache,
        ) -> crate::model::Card {
            card_create_with_id_tx(
                tx,
                new_id(),
                NewCard {
                    track_id: track_id.clone(),
                    title: None,
                    kind: "codex".into(),
                    sort: None,
                    payload: json!({"schemaVersion": 1}),
                },
                CardRole::Worker,
                true,
                role_cache,
            )
            .await
            .unwrap()
        }
        let running_worker = worker_card(&mut tx, &track.id, &role_cache).await;
        let verifying_worker = worker_card(&mut tx, &track.id, &role_cache).await;
        // Tasks rows: the running worker's row still needs its stop hook;
        // the verifying worker's row is past that, and its gate result is
        // the wake instead.
        let mk_task =
            |key: &str, card: &CardId, status: crate::model::TaskStatus| crate::model::Task {
                id: format!("{}:{key}", track.id),
                track_id: track.id.to_string(),
                key: key.into(),
                kind: crate::model::TaskKind::Codex,
                goal: "g".into(),
                context_json: "null".into(),
                acceptance_criteria: None,
                cwd: None,
                depends_on_json: "[]".into(),
                priority: 0,
                gate_json: Some("{\"steps\":[{\"name\":\"t\",\"cmd\":\"true\"}]}".into()),
                status,
                status_detail: None,
                worker_card_id: Some(card.to_string()),
                gate_result_json: None,
                gate_attempt: 1,
                gate_pid: None,
                gate_pid_starttime: None,
                gate_pid_boot_id: None,
                running_deadline_ms: None,
                context_stale_at_ms: None,
                declared_by: calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR.into(),
                spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
                created_at_ms: 1,
                updated_at_ms: 1,
                finished_at_ms: None,
            };
        let running_task = mk_task("run", &running_worker.id, crate::model::TaskStatus::Running);
        let verifying_task = mk_task(
            "verify",
            &verifying_worker.id,
            crate::model::TaskStatus::Verifying,
        );
        crate::test_support::insert_task_tx(&mut tx, &running_task)
            .await
            .unwrap();
        crate::test_support::insert_task_tx(&mut tx, &verifying_task)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let card_scope = |card: &CardId| EventScope::Card {
            card: card.clone(),
            track: track.id.clone(),
            area: area.id.clone(),
        };
        let track_scope = EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        };
        let workspace_path = "/tmp/workspace-replay".to_string();
        // (actor, scope, event, expected to replay)
        let rows: Vec<(ActorId, EventScope, Event, bool)> = vec![
            (
                ActorId::KernelDispatcher,
                card_scope(&running_worker.id),
                Event::WorkspaceLeased {
                    track_id: track.id.clone(),
                    card_id: running_worker.id.clone(),
                    lease_id: "lease-replay".into(),
                    path: workspace_path.clone(),
                },
                false,
            ),
            (
                ActorId::KernelDispatcher,
                card_scope(&running_worker.id),
                Event::WorktreeProvisioned {
                    track_id: track.id.clone(),
                    card_id: running_worker.id.clone(),
                    path: "/tmp/worktree-replay".into(),
                },
                false,
            ),
            (
                ActorId::User,
                card_scope(&running_worker.id),
                Event::CodexHook {
                    card_id: running_worker.id.clone(),
                    kind: "hook.codex.stop".into(),
                    hook_idempotency_key: "hook-running-stop".into(),
                    payload: serde_json::Value::Null,
                },
                true,
            ),
            (
                ActorId::User,
                card_scope(&verifying_worker.id),
                Event::ClaudeHook {
                    card_id: verifying_worker.id.clone(),
                    kind: "hook.claude.stop".into(),
                    hook_idempotency_key: "hook-verifying-stop".into(),
                    payload: serde_json::Value::Null,
                },
                false,
            ),
            (
                ActorId::KernelDispatcher,
                card_scope(&verifying_worker.id),
                Event::WorktreeCommitted {
                    track_id: track.id.clone(),
                    card_id: verifying_worker.id.clone(),
                    commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                    branch: "neige/replay/verify".into(),
                    delivery_id: None,
                    base_is_ancestor: None,
                },
                false,
            ),
            (
                ActorId::KernelDispatcher,
                card_scope(&verifying_worker.id),
                Event::WorkspaceReleased {
                    track_id: track.id.clone(),
                    card_id: verifying_worker.id.clone(),
                    lease_id: "lease-replay".into(),
                },
                false,
            ),
            (
                ActorId::AiPlanner(planner_card.id.clone()),
                track_scope.clone(),
                Event::ReviewRound {
                    track_id: track.id.clone(),
                    subject: ReviewSubject {
                        phase: "impl".into(),
                        slice_id: "5b".into(),
                        pr_number: Some(760),
                    },
                    head_sha: Some("head-sha".into()),
                    n: 1,
                    cap: 8,
                    converged: false,
                    channels: vec![ChannelVerdict {
                        role: "design-correctness".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    }],
                    root_cause: Some("tests failing".into()),
                    idempotency_key: format!("review.round:{}:impl:5b:760:1", track.id),
                },
                false,
            ),
            (
                ActorId::KernelDispatcher,
                track_scope.clone(),
                Event::TaskGateResult {
                    task_id: verifying_task.id.clone(),
                    idempotency_key: verifying_task.id.clone(),
                    passed: true,
                    failing_step: None,
                    exit_code: Some(0),
                    log_tail: "ok\n".into(),
                    log_path: "/tmp/gate.log".into(),
                    attempt: 1,
                    agent_message: None,
                    status_detail: None,
                    target: None,
                },
                true,
            ),
        ];
        let mut expected_ids = Vec::new();
        let mut last_id = 0;
        for (actor, scope, event, replays) in &rows {
            let mut tx = repo.pool().begin().await.unwrap();
            let id = append_decision_event_in_tx(&mut tx, actor, scope, None, event)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            if *replays {
                expected_ids.push(Some(id));
            }
            last_id = id;
        }

        let runtime_id = new_id();
        let thread_id = "thread-catch-up-parity".to_string();
        let mut snapshot = HarnessSnapshot::initial(0, vec![]);
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some(thread_id.clone());
        let mut tx = repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: runtime_id.clone(),
                card_id: planner_card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
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
            planner_card.id.as_str(),
            &track.id,
            0,
            &mut snapshot,
        )
        .await
        .unwrap();
        assert_eq!(
            snapshot.pending_observations(),
            vec![
                Observation::WorkerHookStop {
                    track_id: track.id.clone(),
                    card_id: running_worker.id.clone(),
                    kind: HookKind::CodexStop,
                    idempotency_key: "hook-running-stop".into(),
                },
                Observation::TaskGateResult {
                    idempotency_key: verifying_task.id.clone(),
                    key: "verify".into(),
                    passed: true,
                    failing_step: None,
                    exit_code: Some(0),
                    log_tail: "ok\n".into(),
                    attempt: 1,
                    status_detail: None,
                    target: None,
                },
            ],
            "only the running worker's stop hook and the gate result replay"
        );
        assert_eq!(
            snapshot
                .pending_entries()
                .iter()
                .map(QueueEntry::envelope_id)
                .collect::<Vec<_>>(),
            expected_ids
        );
        // The gate result is the last row, so the watermark lands on it.
        assert_eq!(snapshot.push_watermark, last_id);

        let runtime = repo
            .session_projection_by_id(&runtime_id)
            .await
            .unwrap()
            .unwrap();
        let stored: HarnessSnapshot =
            serde_json::from_value(runtime.handle_state_json.clone().unwrap()).unwrap();
        assert_eq!(stored.pending_entries(), snapshot.pending_entries());
        assert_eq!(stored.push_watermark, last_id);

        let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
        let registry = HarnessRegistry::new();
        let handle = spawn_recovered_harness(
            repo.clone(),
            EventBus::new(),
            role_cache,
            track_area_cache,
            daemon.clone(),
            &registry,
            &crate::per_card_lock::new_keyed_locks(),
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
        .expect("the replayed stop hook + gate result backlog should issue a turn");
        assert_eq!(daemon.turn_start_count_for_test(), 1);
        let turns = daemon.started_turns_for_test();
        let crate::codex_appserver::InputItem::Text { text } = &turns[0].1[0] else {
            panic!("expected text input, got {:?}", turns[0].1)
        };
        assert!(text.contains("gate passed"), "{text}");
        assert!(text.contains("hook_id=hook-running-stop"), "{text}");
        for quiet in [
            workspace_path.as_str(),
            "/tmp/worktree-replay",
            "neige/replay/verify",
            "lease was released",
            "Review round",
            "hook-verifying-stop",
        ] {
            assert!(
                !text.contains(quiet),
                "{quiet:?} leaked into the turn: {text}"
            );
        }

        let after_issue = handle.snapshot().await;
        assert!(after_issue.pending_entries().is_empty());
        assert_eq!(after_issue.push_watermark, last_id);
        assert_eq!(
            after_issue.last_thread_id.as_deref(),
            Some(thread_id.as_str())
        );
        handle.shutdown().await.unwrap();
    }
}
