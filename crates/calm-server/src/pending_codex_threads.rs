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
use crate::db::{Repo, write_with_event_typed};
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

pub struct PendingEntry {
    pub card_id: String,
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
            wave_id,
            terminal_id,
            pty_pid: None,
            registered_at: Instant::now(),
            belt_and_suspenders_attribution_via_tools_call: false,
        }
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
        let Some(entry) = self.queue.lock().await.pop_front() else {
            tracing::info!(
                target = "shared_codex_daemon::pending_orphan_thread_started",
                %thread_id,
                "shared codex thread/started had no pending empty-card registration"
            );
            return Ok(None);
        };

        let age_ms = entry.registered_at.elapsed().as_millis();
        let card_id = entry.card_id;
        let wave_id = entry.wave_id;
        self.bind_entry(&card_id, wave_id.as_deref(), thread_id)
            .await?;
        tracing::info!(
            target = "shared_codex_daemon::pending_bind",
            %thread_id,
            %card_id,
            age_ms,
            "bound pending shared codex empty-card thread start"
        );
        Ok(Some(card_id))
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

        for entry in &expired {
            tracing::info!(
                target = "shared_codex_daemon::pending_expire",
                card_id = %entry.card_id,
                age_ms = entry.registered_at.elapsed().as_millis(),
                "expired abandoned shared codex empty-card pending start"
            );
        }
        expired.len()
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

        for entry in &expired {
            tracing::info!(
                target = "shared_codex_daemon::pending_expire_dead",
                card_id = %entry.card_id,
                terminal_id = %entry.terminal_id,
                age_ms = entry.registered_at.elapsed().as_millis(),
                "expired pending shared codex empty-card start after terminal ended"
            );
        }
        expired.len()
    }

    pub async fn pending_count(&self) -> usize {
        self.queue.lock().await.len()
    }

    pub fn pending_count_snapshot(&self) -> usize {
        self.queue.try_lock().map(|queue| queue.len()).unwrap_or(0)
    }

    async fn bind_entry(
        &self,
        card_id: &str,
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
                        CardRole::Plain,
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
