//! Ordering-based pending registry for empty-prompt user codex cards.
//!
//! Empty user cards can fresh-start a thread from the TUI over the shared
//! daemon, but the route does not know the thread id at spawn time. Spike 3
//! established that PTY spawn order matches `thread/started` notification
//! order, so this registry FIFO-binds the next shared-daemon thread start to
//! the oldest pending card.
//!
//! A `PendingEntry` represents one codex spawn awaiting its `thread/started`.
//! Its identity is the spawn: `terminal_id` and `runtime_id` are each unique
//! per spawn, with `runtime_id == worker_session.id`. `card_id` is not unique
//! in the queue because a card can be rapidly re-spawned, queuing multiple
//! entries. Therefore every per-spawn operation (register-dedup, bind,
//! dead-terminal expiry, stale-drop cleanup, compensation-remove) must key on
//! the spawn (`runtime_id` / `terminal_id`), never on `card_id`. `card_id` in
//! this registry is informational for logging and scope only.

use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    session_bind_attribution_tx, session_clear_terminal_run_id_tx, session_complete_tx,
    session_projection_by_id_tx, session_set_status_tx,
};
use crate::db::{Repo, RepoEventWrite, write_with_event_typed, write_with_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus};
use crate::ids::ActorId;
use crate::model::CardRole;
use crate::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionKind, WorkerSessionState,
};
use crate::state::WriteContext;
use crate::wave_cove_cache::WaveCoveCache;

pub struct PendingThreadStartRegistry {
    queue: Mutex<VecDeque<PendingEntry>>,
    repo: Arc<dyn Repo>,
    events: EventBus,
}

#[derive(Clone)]
pub struct PendingEntry {
    pub card_id: String,
    pub role: CardRole,
    pub wave_id: Option<String>,
    pub terminal_id: String,
    pub runtime_id: String,
    /// PTY pid (best-effort, for debug logs). Not used for attribution.
    pub pty_pid: Option<i32>,
    pub registered_at: Instant,
    /// Spike-3 fallback hook. PR6 records the opt-in bit but does not
    /// implement tools/call attribution yet.
    pub belt_and_suspenders_attribution_via_tools_call: bool,
}

impl PendingEntry {
    pub fn new(
        card_id: String,
        wave_id: Option<String>,
        terminal_id: String,
        runtime_id: String,
    ) -> Self {
        Self {
            card_id,
            role: CardRole::Worker,
            wave_id,
            terminal_id,
            runtime_id,
            pty_pid: None,
            registered_at: Instant::now(),
            belt_and_suspenders_attribution_via_tools_call: false,
        }
    }

    pub fn with_role(mut self, role: CardRole) -> Self {
        self.role = role;
        self
    }
}

impl PendingThreadStartRegistry {
    pub fn new(repo: Arc<dyn Repo>, events: EventBus) -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            repo,
            events,
        }
    }

    pub async fn register(&self, entry: PendingEntry) -> Result<()> {
        let card_id = entry.card_id.clone();
        let wave_id = entry.wave_id.clone();
        let terminal_id = entry.terminal_id.clone();
        let runtime_id = entry.runtime_id.clone();
        let pty_pid = entry.pty_pid;
        let (queue_len_after, already_registered) = {
            let mut queue = self.queue.lock().await;
            if queue.iter().any(|pending| {
                pending.card_id == card_id.as_str() && pending.runtime_id == runtime_id.as_str()
            }) {
                (queue.len(), true)
            } else {
                queue.push_back(entry);
                (queue.len(), false)
            }
        };
        tracing::info!(
            target = "shared_codex_daemon::pending_register",
            %card_id,
            ?wave_id,
            %terminal_id,
            %runtime_id,
            ?pty_pid,
            queue_len_after,
            already_registered,
            "registered pending shared codex empty-card thread start"
        );
        Ok(())
    }

    pub async fn remove_by_runtime(&self, runtime_id: &str) -> bool {
        let mut queue = self.queue.lock().await;
        let Some(index) = queue
            .iter()
            .position(|entry| entry.runtime_id == runtime_id)
        else {
            return false;
        };
        queue.remove(index).is_some()
    }

    pub async fn on_thread_started(&self, thread_id: &str) -> Result<Option<String>> {
        loop {
            let Some(entry_to_check) = self.queue.lock().await.front().cloned() else {
                tracing::info!(
                    target = "shared_codex_daemon::pending_orphan_thread_started",
                    %thread_id,
                    "shared codex thread/started had no pending empty-card registration"
                );
                return Ok(None);
            };

            if !self.is_terminal_alive(&entry_to_check.terminal_id).await {
                let dropped = {
                    let mut queue = self.queue.lock().await;
                    let Some(front) = queue.front() else {
                        continue;
                    };
                    if !same_pending_entry(front, &entry_to_check) {
                        continue;
                    }
                    queue.pop_front().expect("front checked")
                };
                self.drop_stale_entry(dropped, "thread_started_stale_front")
                    .await;
                // Followup gate #3 (PR6 R6 P2-A pragmatic mitigation):
                // STOP after dropping a stale front rather than looping with
                // the SAME thread_id. The next-in-queue entry (if any) is
                // soft-deterministically tied to a DIFFERENT pending PTY
                // spawn — binding it to THIS thread_id would be a cross-
                // attribution: the new card would receive a thread that the
                // dropped card's TUI requested.
                //
                // codex 0.135 has no opaque request-id passthrough in
                // thread/start / thread/started, so we cannot harden the
                // FIFO attribution any further. Treating the thread_id as
                // an orphan here is the least-surprising failure mode —
                // the legitimate next-in-queue entry must wait for its OWN
                // thread/started event (the daemon will emit one per
                // outstanding thread/start RPC). Some empty cards may miss
                // a bind, but no card receives a thread that wasn't its
                // own.
                tracing::warn!(
                    target = "shared_codex_daemon::pending_orphan_thread_started",
                    %thread_id,
                    "stale front pending entry dropped; treating thread_id as orphan rather than cross-attributing to the next-in-queue entry"
                );
                return Ok(None);
            }

            let entry = {
                let mut queue = self.queue.lock().await;
                let Some(front) = queue.front() else {
                    continue;
                };
                if !same_pending_entry(front, &entry_to_check) {
                    continue;
                }
                queue.pop_front().expect("front checked")
            };

            let age_ms = entry.registered_at.elapsed().as_millis();
            let card_id = entry.card_id.clone();
            match self.bind_entry(&entry, thread_id).await {
                Ok(BindEntryOutcome::Bound) => {
                    tracing::info!(
                        target = "shared_codex_daemon::pending_bind",
                        %thread_id,
                        %card_id,
                        age_ms,
                        "bound pending shared codex empty-card thread start"
                    );
                    return Ok(Some(card_id));
                }
                Ok(BindEntryOutcome::Orphan { reason }) => {
                    self.drop_stale_entry(entry, reason).await;
                    tracing::warn!(
                        target = "shared_codex_daemon::pending_orphan_thread_started",
                        %thread_id,
                        %card_id,
                        reason,
                        "registered runtime missing or inactive; treating thread_id as orphan"
                    );
                    return Ok(None);
                }
                Err(err) => {
                    let mut queue = self.queue.lock().await;
                    queue.push_front(entry);
                    tracing::warn!(
                        target = "shared_codex_daemon::pending_bind",
                        %thread_id,
                        %card_id,
                        error = %err,
                        "pending bind failed; re-parked entry"
                    );
                    return Ok(None);
                }
            }
        }
    }

    pub async fn expire(&self, ttl: Duration) -> usize {
        let mut expired = Vec::new();
        {
            let mut queue = self.queue.lock().await;
            let now = Instant::now();
            let mut kept = VecDeque::with_capacity(queue.len());
            while let Some(entry) = queue.pop_front() {
                if now.duration_since(entry.registered_at) >= ttl {
                    expired.push(entry);
                } else {
                    kept.push_back(entry);
                }
            }
            *queue = kept;
        }

        let expired_len = expired.len();
        for entry in expired {
            self.drop_stale_entry(entry, "ttl_expire").await;
        }
        expired_len
    }

    pub async fn expire_dead_pending(&self) -> usize {
        let snapshot = {
            let queue = self.queue.lock().await;
            queue
                .iter()
                .map(|entry| (entry.card_id.clone(), entry.terminal_id.clone()))
                .collect::<Vec<_>>()
        };

        let mut dead_terminals = HashSet::new();
        for (card_id, terminal_id) in snapshot {
            let terminal = match self.repo.terminal_get(&terminal_id).await {
                Ok(terminal) => terminal,
                Err(err) => {
                    tracing::warn!(
                        target = "shared_codex_daemon::pending_expire_dead",
                        %card_id,
                        %terminal_id,
                        error = %err,
                        "failed to read terminal while expiring pending thread starts"
                    );
                    continue;
                }
            };
            let is_dead = match terminal {
                None => true,
                Some(terminal) => terminal.exit_code.is_some() || terminal.signal_killed,
            };
            if is_dead {
                dead_terminals.insert(terminal_id);
            }
        }

        if dead_terminals.is_empty() {
            return 0;
        }

        let mut expired = Vec::new();
        {
            let mut queue = self.queue.lock().await;
            let mut kept = VecDeque::with_capacity(queue.len());
            while let Some(entry) = queue.pop_front() {
                if dead_terminals.contains(&entry.terminal_id) {
                    expired.push(entry);
                } else {
                    kept.push_back(entry);
                }
            }
            *queue = kept;
        }

        let expired_len = expired.len();
        for entry in expired {
            self.drop_stale_entry(entry, "terminal_dead_expire").await;
        }
        expired_len
    }

    pub async fn pending_count(&self) -> usize {
        self.queue.lock().await.len()
    }

    pub fn pending_count_snapshot(&self) -> usize {
        self.queue.try_lock().map(|queue| queue.len()).unwrap_or(0)
    }

    async fn is_terminal_alive(&self, terminal_id: &str) -> bool {
        self.repo
            .terminal_get(terminal_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|terminal| terminal.exit_code.is_none() && !terminal.signal_killed)
    }

    async fn drop_stale_entry(&self, entry: PendingEntry, reason: &str) {
        let payload_cleared = card_payload_clear_pending_status(
            self.repo.as_ref(),
            &self.events,
            &entry.card_id,
            &entry.runtime_id,
        )
        .await
        .is_ok();
        tracing::warn!(
            target = "shared_codex_daemon::pending_drop_stale",
            card_id = %entry.card_id,
            terminal_id = %entry.terminal_id,
            age_ms = entry.registered_at.elapsed().as_millis(),
            reason,
            payload_cleared,
            "stale pending entry dropped"
        );
    }

    async fn bind_entry(&self, entry: &PendingEntry, thread_id: &str) -> Result<BindEntryOutcome> {
        let card_id = &entry.card_id;
        let card = self
            .repo
            .card_get(card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

        let scope = crate::routes::cards::card_scope(
            self.repo.as_ref(),
            card.id.clone(),
            card.wave_id.clone(),
        )
        .await?;
        let card_id_for_tx = card_id.to_string();
        let runtime_id_for_tx = entry.runtime_id.clone();
        let thread_id_for_tx = thread_id.to_string();
        let card_for_event = card;
        let card_role_cache = CardRoleCache::default();
        let wave_cove_cache = WaveCoveCache::default();
        let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
        let result = write_with_events_typed(
            self.repo.as_ref(),
            ActorId::Kernel,
            None,
            &self.events,
            &write,
            move |tx| {
                Box::pin(async move {
                    let runtime = session_projection_by_id_tx(tx, &runtime_id_for_tx)
                        .await?
                        .ok_or_else(|| {
                            pending_runtime_orphan_error(
                                PENDING_RUNTIME_ORPHAN_MISSING,
                                &runtime_id_for_tx,
                            )
                        })?;
                    if !runtime_status_is_active(&runtime.status) {
                        return Err(pending_runtime_orphan_error(
                            PENDING_RUNTIME_ORPHAN_NOT_ACTIVE,
                            &runtime_id_for_tx,
                        ));
                    }
                    assert_eq!(
                        runtime.card_id.as_str(),
                        card_id_for_tx.as_str(),
                        "pending runtime/card mismatch during pending thread bind"
                    );
                    let old_status = runtime.status;
                    let runtime_id = runtime.id.clone();
                    session_bind_attribution_tx(
                        tx,
                        &runtime.id,
                        ThreadAttribution {
                            runtime_id: runtime.id.clone(),
                            provider: AgentProvider::Codex,
                            thread_id: Some(thread_id_for_tx.clone()),
                            session_id: None,
                            active_turn_id: None,
                        },
                    )
                    .await?;
                    if old_status != WorkerSessionState::Running {
                        session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running).await?;
                    }
                    // SharedSpec runtimes switch to thread-keyed identity; CodexCard runtimes keep terminal_run_id as their completion handle.
                    if runtime.kind == WorkerSessionKind::SharedSpec {
                        session_clear_terminal_run_id_tx(tx, &runtime.id).await?;
                    }
                    let card = card_for_event;
                    let mut events = vec![(scope.clone(), Event::CardUpdated(card.clone()))];
                    if old_status != WorkerSessionState::Running {
                        events.push((
                            scope,
                            Event::RuntimeStatusChanged {
                                runtime_id,
                                card_id: card_id_for_tx,
                                old_status,
                                new_status: WorkerSessionState::Running,
                            },
                        ));
                    }
                    Ok((card, events))
                })
            },
        )
        .await;
        match result {
            Ok((_updated, _event_ids)) => Ok(BindEntryOutcome::Bound),
            Err(err) => {
                if let Some(reason) = pending_runtime_orphan_reason(&err) {
                    Ok(BindEntryOutcome::Orphan { reason })
                } else {
                    Err(err)
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindEntryOutcome {
    Bound,
    Orphan { reason: &'static str },
}

// TODO(#679 cleanup, off-roadmap): orphan-ness is signaled OUT of the bind tx
// closure by encoding it into a magic-prefixed `CalmError::Internal` string and
// decoding it back in `pending_runtime_orphan_reason` — control flow via an error
// string. Footgun: adding a new orphan reason requires touching TWO places (the
// `PENDING_RUNTIME_ORPHAN_*` consts AND the decode `match`); forgetting the decode
// arm silently degrades drop -> re-park. Cleaner shape: resolve the runtime status
// BEFORE opening the write tx and decide Orphan there (no tx, no sentinel); let the
// rare in-tx TOCTOU fall to the plain-Err re-park path. Not worth a dedicated PR on
// this just-stabilized file — fold the refactor in opportunistically the next time
// this module is touched for real.
const PENDING_RUNTIME_ORPHAN_PREFIX: &str = "__pending_codex_runtime_orphan__";
const PENDING_RUNTIME_ORPHAN_MISSING: &str = "thread_started_runtime_missing";
const PENDING_RUNTIME_ORPHAN_NOT_ACTIVE: &str = "thread_started_runtime_not_active";

fn pending_runtime_orphan_error(reason: &'static str, runtime_id: &str) -> CalmError {
    CalmError::Internal(format!(
        "{PENDING_RUNTIME_ORPHAN_PREFIX}:{reason}:{runtime_id}"
    ))
}

fn pending_runtime_orphan_reason(err: &CalmError) -> Option<&'static str> {
    let CalmError::Internal(message) = err else {
        return None;
    };
    let reason = message
        .strip_prefix(PENDING_RUNTIME_ORPHAN_PREFIX)?
        .strip_prefix(':')?
        .split(':')
        .next()?;
    match reason {
        PENDING_RUNTIME_ORPHAN_MISSING => Some(PENDING_RUNTIME_ORPHAN_MISSING),
        PENDING_RUNTIME_ORPHAN_NOT_ACTIVE => Some(PENDING_RUNTIME_ORPHAN_NOT_ACTIVE),
        _ => None,
    }
}

fn runtime_status_is_active(status: &WorkerSessionState) -> bool {
    matches!(
        status,
        WorkerSessionState::Starting
            | WorkerSessionState::Running
            | WorkerSessionState::Idle
            | WorkerSessionState::TurnPending
    )
}

pub(crate) async fn card_payload_clear_pending_status(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    card_id: &str,
    runtime_id: &str,
) -> Result<()> {
    let card = repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let scope =
        crate::routes::cards::card_scope(repo, card.id.clone(), card.wave_id.clone()).await?;
    let runtime_id_for_tx = runtime_id.to_string();
    let card_for_event = card;
    let card_role_cache = CardRoleCache::default();
    let wave_cove_cache = WaveCoveCache::default();
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
    let (_updated, _id) = write_with_event_typed(
        repo,
        ActorId::Kernel,
        scope,
        None,
        events,
        &write,
        move |tx| {
            Box::pin(async move {
                if let Some(runtime) = session_projection_by_id_tx(tx, &runtime_id_for_tx).await?
                    && runtime_status_is_active(&runtime.status)
                {
                    session_complete_tx(tx, &runtime.id, WorkerSessionState::Failed).await?;
                }
                let card = card_for_event;
                Ok((card.clone(), Event::CardUpdated(card)))
            })
        },
    )
    .await?;
    Ok(())
}

fn same_pending_entry(a: &PendingEntry, b: &PendingEntry) -> bool {
    a.runtime_id == b.runtime_id && a.terminal_id == b.terminal_id
}

pub fn spawn_periodic_expire_task(
    registry: Arc<PendingThreadStartRegistry>,
    interval: Duration,
    ttl: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let dead_expired = registry.expire_dead_pending().await;
            let ttl_expired = registry.expire(ttl).await;
            let expired = dead_expired + ttl_expired;
            if expired > 0 {
                tracing::info!(
                    target: "shared_codex_daemon::pending_expire_batch",
                    expired,
                    dead_expired,
                    ttl_expired,
                    "expired pending thread-start entries"
                );
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{NewCard, NewCove, NewWave};
    use serde_json::json;

    async fn seed_card_without_runtime() -> (Arc<SqlxRepo>, EventBus, String) {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let cove = repo
            .cove_create(NewCove {
                name: "pending".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "pending".into(),
                sort: None,
                cwd: "/workspace".into(),
                workflow_id: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                wave_id: wave.id,
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1}),
            })
            .await
            .unwrap();
        (repo, EventBus::new(), card.id.to_string())
    }

    #[tokio::test]
    async fn bind_orphans_when_registered_runtime_missing() {
        let (repo, events, card_id) = seed_card_without_runtime().await;
        let registry = PendingThreadStartRegistry::new(repo, events);
        let entry = PendingEntry::new(
            card_id,
            None,
            "terminal-missing-runtime".to_string(),
            "runtime-missing".to_string(),
        );

        let outcome = registry.bind_entry(&entry, "T-missing-runtime").await;

        assert_eq!(
            outcome.unwrap(),
            BindEntryOutcome::Orphan {
                reason: PENDING_RUNTIME_ORPHAN_MISSING,
            }
        );
    }
}
