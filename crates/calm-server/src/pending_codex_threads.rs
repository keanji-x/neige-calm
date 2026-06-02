//! Ordering-based pending registry for empty-prompt user codex cards.
//!
//! Empty user cards can fresh-start a thread from the TUI over the shared
//! daemon, but the route does not know the thread id at spawn time. Spike 3
//! established that PTY spawn order matches `thread/started` notification
//! order, so this registry FIFO-binds the next shared-daemon thread start to
//! the oldest pending card.

use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{card_codex_thread_upsert_tx, card_update_tx};
use crate::db::{Repo, RepoEventWrite, write_with_event_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus};
use crate::ids::ActorId;
use crate::model::{CardPatch, CardRole};
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
    /// PTY pid (best-effort, for debug logs). Not used for attribution.
    pub pty_pid: Option<i32>,
    pub registered_at: Instant,
    /// Spike-3 fallback hook. PR6 records the opt-in bit but does not
    /// implement tools/call attribution yet.
    pub belt_and_suspenders_attribution_via_tools_call: bool,
}

impl PendingEntry {
    pub fn new(card_id: String, wave_id: Option<String>, terminal_id: String) -> Self {
        Self {
            card_id,
            role: CardRole::Plain,
            wave_id,
            terminal_id,
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
        let pty_pid = entry.pty_pid;
        let queue_len_after = {
            let mut queue = self.queue.lock().await;
            queue.push_back(entry);
            queue.len()
        };
        tracing::info!(
            target = "shared_codex_daemon::pending_register",
            %card_id,
            ?wave_id,
            %terminal_id,
            ?pty_pid,
            queue_len_after,
            "registered pending shared codex empty-card thread start"
        );
        Ok(())
    }

    pub async fn remove_by_card(&self, card_id: &str) -> bool {
        let mut queue = self.queue.lock().await;
        let Some(index) = queue.iter().position(|entry| entry.card_id == card_id) else {
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
            let card_id = entry.card_id;
            let role = entry.role;
            let wave_id = entry.wave_id;
            self.bind_entry(&card_id, role, wave_id.as_deref(), thread_id)
                .await?;
            tracing::info!(
                target = "shared_codex_daemon::pending_bind",
                %thread_id,
                %card_id,
                age_ms,
                "bound pending shared codex empty-card thread start"
            );
            return Ok(Some(card_id));
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

        let mut dead_card_ids = HashSet::new();
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
                dead_card_ids.insert(card_id);
            }
        }

        if dead_card_ids.is_empty() {
            return 0;
        }

        let mut expired = Vec::new();
        {
            let mut queue = self.queue.lock().await;
            let mut kept = VecDeque::with_capacity(queue.len());
            while let Some(entry) = queue.pop_front() {
                if dead_card_ids.contains(&entry.card_id) {
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
        let payload_cleared =
            card_payload_clear_pending_status(self.repo.as_ref(), &self.events, &entry.card_id)
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

    async fn bind_entry(
        &self,
        card_id: &str,
        role: CardRole,
        wave_id: Option<&str>,
        thread_id: &str,
    ) -> Result<()> {
        let card = self
            .repo
            .card_get(card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
        let mut payload = card.payload.clone();
        let Some(map) = payload.as_object_mut() else {
            return Err(CalmError::Internal(format!(
                "codex card {card_id} payload is not a JSON object; cannot bind thread id"
            )));
        };
        map.insert(
            "codex_thread_id".into(),
            serde_json::Value::String(thread_id.to_string()),
        );
        map.insert(
            "codex_thread_status".into(),
            serde_json::Value::String("started".into()),
        );

        let scope =
            crate::routes::cards::card_scope(self.repo.as_ref(), card.id.clone(), card.wave_id)
                .await?;
        let card_id_for_tx = card_id.to_string();
        let thread_id_for_tx = thread_id.to_string();
        let wave_id_for_tx = wave_id.map(ToOwned::to_owned);
        let payload_for_tx = payload;
        let card_role_cache = CardRoleCache::default();
        let wave_cove_cache = WaveCoveCache::default();
        let (_updated, _event_id) = write_with_event_typed(
            self.repo.as_ref(),
            ActorId::Kernel,
            scope,
            None,
            &self.events,
            &card_role_cache,
            &wave_cove_cache,
            move |tx| {
                Box::pin(async move {
                    card_codex_thread_upsert_tx(
                        tx,
                        &card_id_for_tx,
                        &thread_id_for_tx,
                        role,
                        wave_id_for_tx.as_deref(),
                    )
                    .await?;
                    let card = card_update_tx(
                        tx,
                        &card_id_for_tx,
                        CardPatch {
                            kind: None,
                            sort: None,
                            payload: Some(payload_for_tx),
                            deletable: None,
                        },
                    )
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            },
        )
        .await?;
        Ok(())
    }
}

pub(crate) async fn card_payload_clear_pending_status(
    repo: &dyn RepoEventWrite,
    events: &EventBus,
    card_id: &str,
) -> Result<()> {
    let card = repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let mut payload = card.payload.clone();
    let Some(map) = payload.as_object_mut() else {
        return Err(CalmError::Internal(format!(
            "codex card {card_id} payload is not a JSON object; cannot mark spawn failure"
        )));
    };
    map.insert(
        "codex_thread_status".into(),
        serde_json::Value::String("failed_to_spawn".into()),
    );

    let scope =
        crate::routes::cards::card_scope(repo, card.id.clone(), card.wave_id.clone()).await?;
    let card_id_for_tx = card_id.to_string();
    let payload_for_tx = payload;
    let card_role_cache = CardRoleCache::default();
    let wave_cove_cache = WaveCoveCache::default();
    let (_updated, _id) = write_with_event_typed(
        repo,
        ActorId::Kernel,
        scope,
        None,
        events,
        &card_role_cache,
        &wave_cove_cache,
        move |tx| {
            Box::pin(async move {
                let card = card_update_tx(
                    tx,
                    &card_id_for_tx,
                    CardPatch {
                        kind: None,
                        sort: None,
                        payload: Some(payload_for_tx),
                        deletable: None,
                    },
                )
                .await?;
                Ok((card.clone(), Event::CardUpdated(card)))
            })
        },
    )
    .await?;
    Ok(())
}

fn same_pending_entry(a: &PendingEntry, b: &PendingEntry) -> bool {
    a.card_id == b.card_id && a.terminal_id == b.terminal_id
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
