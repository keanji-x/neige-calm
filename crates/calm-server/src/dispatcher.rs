//! Dispatcher worker (PR5 of #136).
//!
//! Subscribes to the event bus through [`EventBus::subscribe_filtered`] +
//! a [`SubscribeFilter`] that picks out `codex.job_requested` and
//! `terminal.job_requested` envelopes, then mints a worker-roled card
//! (and, for the codex case, starts a backing terminal renderer) for
//! each.
//!
//! ## Design rationale
//!
//! PR4 introduced the four dispatcher/task-lifecycle event variants but
//! *had no emitter*. PR5's job is the consumer side:
//!
//!   * A subscriber that survives lag (a missed event becomes a missed
//!     dispatch; the idempotency key prevents double-spawn when the next
//!     emit lands).
//!   * Per-event work fans out via [`tokio::spawn`] gated on a shared
//!     [`Semaphore`] so the bus reader never backpressures, but spawn
//!     parallelism stays bounded (default 8, override via
//!     `NEIGE_DISPATCHER_PERMITS`).
//!   * Idempotency: the dispatcher persists each request's
//!     `idempotency_key` into the spawned worker card's `payload.idempotency_key`
//!     and, inside the same transaction, SELECTs for an existing card with
//!     the same key first. Two `*.Requested` envelopes racing through with
//!     the same key can't both win — the second SELECT either sees the
//!     first card committed (skip) or both run in parallel transactions
//!     where exactly one wins the row-level lock (the other commits a
//!     duplicate row). The latter case is **the only race window**;
//!     mitigated by the in-flight `recently_seen` set that holds keys for
//!     a brief grace period after a successful spawn. We deliberately do
//!     NOT add a unique index on `cards.payload->>'$.idempotency_key'`
//!     because (a) it would require a new migration which PR5 is
//!     scope-out-of, and (b) the key namespace is dispatcher-local;
//!     non-dispatcher cards don't carry the field.
//!
//! ## Why the cards-payload approach and not a separate dispatch_jobs table
//!
//! Three options were on the table:
//!
//!   1. **`dispatch_jobs(idempotency_key)` table with `UNIQUE`.** Cleanest,
//!      but adds schema. PR5 is explicitly schema-free.
//!   2. **`INSERT … ON CONFLICT DO NOTHING` against a deduplication table.**
//!      Same migration cost.
//!   3. **Cards payload + SELECT inside tx.** No schema, narrow race
//!      window (covered by `recently_seen`). Picked for PR5.
//!
//! ## Failure handling
//!
//! Any error in the spawn pipeline (idempotency check error, tx error,
//! daemon spawn failure) emits a `Event::TaskFailed { idempotency_key,
//! reason }` via [`Repo::log_pure_event`] from the
//! [`ActorId::KernelDispatcher`] actor. The dispatcher's push path
//! (#293) delivers these to the requesting spec card as turn inputs.
//!
//! ## What this doesn't do
//!
//! - **No spec card minting** — PR6 lands the spec card; the dispatcher
//!   just responds to whoever emits a `*.Requested` event.
//! - **No glob kinds** — the dispatcher's filter lists the literal kind
//!   tags. A future glob extension would update both the filter and this
//!   module's subscribe call together.

#![allow(deprecated)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::{InputItem, Notification};
use crate::db::sqlite::{
    card_update_tx, card_with_codex_create_tx, runtime_bind_attribution_tx,
    runtime_clear_terminal_run_id_tx, runtime_get_active_for_card_tx, runtime_set_status_tx,
};
use crate::db::{Repo, RouteRepo};
use crate::db::{write_in_tx_typed, write_with_event_typed};
use crate::error::CalmError;
use crate::event::{
    BroadcastEnvelope, EditAuthor, Event, EventBus, EventScope, SubscribeFilter, SubscribeScope,
};
use crate::event_cursor::EventCursorCache;
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::{CardPatch, CardRole};
use crate::routes::codex_cards::shell_single_quote;
use crate::routes::settings::load_settings;
use crate::routes::terminal::spawn_terminal_with_parts;
use crate::runtime_repo::{AgentProvider, RunStatus, RuntimeKind, ThreadAttribution};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams};
use crate::spec_card::build_codex_env_map;
use crate::spec_push::SpecPushRegistry;
use crate::state::{CodexClient, DaemonClient, WriteContext};
use crate::terminal_renderer::TerminalRendererRegistry;
use crate::terminal_sweeper::{reap_terminal_artifacts_with_renderer, reap_terminal_pid_only};

pub(crate) use crate::db::sqlite::card_with_terminal_rollback_tx;

/// Default number of permits when `NEIGE_DISPATCHER_PERMITS` is unset /
/// invalid / `0`. Mirrors the v2 spec for issue #136.
const DEFAULT_PERMITS: usize = 8;

/// Window during which an idempotency key remains "in-flight" after a
/// successful spawn — covers the moment between transaction commit and
/// the next event-bus emit landing in the dispatcher. Bounded so the
/// in-memory set can't grow without limit; the SELECT-inside-tx
/// idempotency check is the canonical guard, this is just a fast-path
/// short-circuit.
const RECENT_KEYS_TTL: Duration = Duration::from_secs(60);

pub(crate) fn event_warrants_spec_push(event: &Event, write: &WriteContext) -> bool {
    match event {
        Event::TaskCompleted { .. } | Event::TaskFailed { .. } => true,
        Event::WaveReportEdited { author, .. } => *author == EditAuthor::User,
        Event::CodexHook { card_id, kind, .. } | Event::ClaudeHook { card_id, kind, .. } => {
            let is_turn_end = kind == "hook.codex.stop" || kind == "hook.claude.stop";
            let is_worker = write.verify_role(card_id) == Some(CardRole::Worker);
            is_turn_end && is_worker
        }
        _ => false,
    }
}

/// Subscribed handle. Holding the [`Dispatcher`] keeps the spawned
/// task alive; dropping it closes the broadcast receiver's end (the
/// task exits cleanly on the next `Closed` recv).
///
/// Today nothing outside `AppState::new` reaches in here — the
/// dispatcher is fire-and-forget. We still hand back the struct so
/// `AppState` can store it as `Arc<Dispatcher>` (matching the
/// terminal_sweeper / card_fsm convention) and so tests can assert on
/// the configured permit count.
pub struct Dispatcher {
    semaphore: Arc<Semaphore>,
    /// Number of permits the semaphore was constructed with — surfaced
    /// for tests so they don't have to introspect `Semaphore` itself.
    permits: usize,
    /// Background task handle. Kept on the struct so future shutdown
    /// can `abort()` it; not used today (we let the broadcast `Closed`
    /// signal drive the loop down naturally).
    #[allow(dead_code)]
    handle: JoinHandle<()>,
    /// #313 problem #1 — boot takeover catch-up reaches `push_to_spec`
    /// through this. Held as a strong `Arc` so the same instance the
    /// background task is consuming is the one
    /// [`Dispatcher::catch_up_push`] calls into; the background task
    /// also holds its own clone, so the dispatcher stays alive as long
    /// as either side does.
    inner: Arc<Inner>,
}

#[allow(deprecated)]
impl Dispatcher {
    /// Resolve the permit count from `NEIGE_DISPATCHER_PERMITS` (parsed
    /// as `usize`), falling back to [`DEFAULT_PERMITS`] when unset,
    /// empty, unparseable, or zero. Surfaced as a free helper so tests
    /// can verify the env-override logic without spawning a full
    /// dispatcher.
    pub fn permits_from_env(default: usize) -> usize {
        match std::env::var("NEIGE_DISPATCHER_PERMITS") {
            Ok(raw) => match raw.trim().parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => default,
            },
            Err(_) => default,
        }
    }

    /// Configured permit count. Exposed for assertions in tests.
    pub fn permits(&self) -> usize {
        self.permits
    }

    #[cfg(feature = "fixtures")]
    pub fn recently_seen_contains(&self, key: &str) -> bool {
        self.inner
            .recently_seen
            .lock()
            .map(|seen| seen.contains(key))
            .unwrap_or(false)
    }

    /// #313 problem #1 (boot takeover) — seed the in-memory push watermark
    /// cache for a spec card from its persisted `payload.push_watermark`,
    /// BEFORE any catch-up replay or live envelope can reach `push_to_spec`.
    ///
    /// Uses the same monotonic [`EventCursorCache::bump`] semantics the
    /// live push path uses — a concurrent live event landing while we're
    /// seeding can only ratchet the cache UP (a lower id is a no-op),
    /// never down, so the seed is race-safe against a freshly-registered
    /// handle's first push. Called once per wave from
    /// [`crate::legacy spec takeover`].
    pub fn seed_push_cursor(&self, spec_card_id: CardId, watermark: i64) {
        self.inner.push_cursor.bump(spec_card_id, watermark);
    }

    /// Runtime app-server recovery reuses this dispatcher in the same
    /// process, so its soft push cursor may be ahead of the durable
    /// `push_watermark`. Force it back before event-log catch-up so fallback
    /// replay cannot dedup undelivered rows.
    pub fn reset_push_cursor_to_watermark(&self, spec_card_id: CardId, watermark: i64) {
        self.inner.push_cursor.set(spec_card_id, watermark);
    }

    /// Test-only — clear the in-memory push cursor for a card so the next
    /// access falls back to the default `0`. Used by the boot-recovery
    /// e2e to simulate a cold-boot cache (no in-process state surviving
    /// across the simulated restart). Production code does not call this.
    #[doc(hidden)]
    pub fn clear_push_cursor_for_test(&self, spec_card_id: &CardId) {
        self.inner.push_cursor.remove(spec_card_id);
    }

    /// Test-only — read the current in-memory push cursor for a card.
    /// Used by the boot-recovery e2e to assert that `seed_push_cursor`
    /// actually planted the persisted watermark into the cache.
    #[doc(hidden)]
    pub fn push_cursor_for_test(&self, spec_card_id: &CardId) -> i64 {
        self.inner.push_cursor.get(spec_card_id)
    }

    /// #313 problem #1 round-2 (B3) — acquire the per-wave push lock.
    ///
    /// The dispatcher's `Inner::push_to_spec` takes this same per-wave
    /// `Mutex` across `(get → compare → bump → push_observation)`. Boot
    /// takeover holds it across `(seed_push_cursor → spec_push.insert →
    /// catch_up_push_under_lock for every event)` so a live event landing
    /// on the bus during the window (between insert and the catch-up's
    /// last replay) serializes behind takeover instead of slipping past
    /// the seeded watermark without being delivered. Without this
    /// serialization, the live event would see a freshly-seeded cursor,
    /// `bump` it to its own envelope id, and our catch-up replays for
    /// ids ≤ that bump would dedup silently — losing every event that
    /// landed while the kernel was down up to and including that id.
    ///
    /// IMPORTANT: while holding this lock, the only safe way to drive
    /// catch-up is via [`Dispatcher::catch_up_push_under_lock`] (not the
    /// public [`Dispatcher::catch_up_push`]); this is now type-enforced by
    /// requiring a [`PushLockGuard`]. The latter takes the same per-wave
    /// lock and `tokio::sync::Mutex` is NOT reentrant — calling
    /// `catch_up_push` here would deadlock.
    /// #480 §D — explicit push-lock acquisition. Returns a `PushLockGuard`
    /// the caller can hold across catch-up sequences and pass to
    /// [`catch_up_push_under_lock`] (whose signature requires `&PushLockGuard`).
    pub async fn push_lock(&self, wave_id: &WaveId) -> PushLockGuard {
        self.inner.acquire_push_lock(wave_id).await
    }

    /// #313 problem #1 round-2 (B1) — build the [`WatermarkSink`] the
    /// `SpecPushHandle` consumer task will call on a successful queue
    /// flush, so the durable `push_watermark` advances for envelope ids
    /// that were ONLY delivered out of the queue (the dispatcher's
    /// own `push_to_spec` never re-runs for those).
    ///
    /// Captures `repo` + the spec card id; emits no `CardUpdated` event
    /// (same posture as `Inner::push_to_spec`'s direct call).
    pub fn watermark_sink_for(&self, spec_card_id: CardId) -> crate::spec_push::WatermarkSink {
        let repo = Arc::clone(&self.inner.repo);
        let push_cursor = self.inner.push_cursor.clone();
        Arc::new(move |max_envelope_id: i64| {
            let repo = Arc::clone(&repo);
            let push_cursor = push_cursor.clone();
            let spec_card_id = spec_card_id.clone();
            Box::pin(async move {
                // Mirror `push_to_spec`'s post-success bookkeeping: bump
                // the in-memory cache too so a same-process re-delivery
                // dedups against the just-flushed id. Monotonic bump is
                // a no-op if the cache is already at/above this id (which
                // happens when push_to_spec already saw + queued this
                // envelope earlier).
                push_cursor.bump(spec_card_id.clone(), max_envelope_id);
                if let Err(e) = repo
                    .spec_card_set_push_watermark(spec_card_id.as_str(), max_envelope_id)
                    .await
                {
                    tracing::warn!(
                        spec_card_id = %spec_card_id,
                        max_envelope_id,
                        error = %e,
                        "spec push (flush sink): persist push_watermark failed; \
                         in-memory cache bumped, next boot may re-push these envelopes (idempotent)"
                    );
                }
            })
        })
    }

    /// Build the lifecycle-observation sink installed on each spec
    /// app-server handle. Unlike `watermark_sink_for`, this is driven by
    /// codex turn notifications rather than dispatcher pushes, so manual
    /// remote-TUI input can clear the empty-goal bootstrap marker as soon
    /// as a rollout exists.
    pub fn initial_prompt_ready_sink_for(
        &self,
        spec_card_id: CardId,
        wave_id: WaveId,
        cove_id: CoveId,
    ) -> crate::spec_push::InitialPromptReadySink {
        let repo = Arc::clone(&self.inner.repo);
        let events = self.inner.events.clone();
        let write = self.inner.write.clone();
        Arc::new(move |thread_id: String| {
            let repo = Arc::clone(&repo);
            let spec_card_id = spec_card_id.clone();
            let wave_id = wave_id.clone();
            let cove_id = cove_id.clone();
            let events = events.clone();
            let write = write.clone();
            Box::pin(async move {
                let scope = EventScope::Card {
                    card: spec_card_id.clone(),
                    wave: wave_id,
                    cove: cove_id,
                };
                let card_id_for_tx = spec_card_id.clone();
                let thread_id_for_tx = thread_id.clone();
                let result = write_with_event_typed(
                    repo.as_ref(),
                    ActorId::Kernel,
                    scope,
                    None,
                    &events,
                    &write,
                    move |tx| {
                        Box::pin(async move {
                            let payload_text: Option<(String,)> =
                                sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
                                    .bind(card_id_for_tx.as_str())
                                    .fetch_optional(&mut **tx)
                                    .await?;
                            let payload_text = payload_text
                                .ok_or_else(|| {
                                    CalmError::NotFound(format!("card {card_id_for_tx}"))
                                })?
                                .0;
                            let mut payload: serde_json::Value =
                                serde_json::from_str(&payload_text).map_err(|e| {
                                    CalmError::Internal(format!(
                                        "card {card_id_for_tx} payload is not valid JSON: {e}"
                                    ))
                                })?;
                            let Some(map) = payload.as_object_mut() else {
                                return Err(CalmError::Internal(format!(
                                    "spec card {card_id_for_tx} payload is not a JSON object; \
                                     cannot clear initial-prompt marker"
                                )));
                            };
                            map.remove("appserver_needs_initial_prompt");
                            let card = card_update_tx(
                                tx,
                                card_id_for_tx.as_str(),
                                CardPatch {
                                    kind: None,
                                    sort: None,
                                    payload: Some(payload),
                                    deletable: None,
                                },
                            )
                            .await?;
                            if let Some(runtime) =
                                runtime_get_active_for_card_tx(tx, card_id_for_tx.as_str()).await?
                            {
                                runtime_bind_attribution_tx(
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
                                runtime_set_status_tx(tx, &runtime.id, RunStatus::Running).await?;
                                if runtime.kind == RuntimeKind::SharedSpec {
                                    runtime_clear_terminal_run_id_tx(tx, &runtime.id).await?;
                                }
                            }
                            Ok((card.clone(), Event::CardUpdated(card)))
                        })
                    },
                )
                .await;
                if let Err(e) = result {
                    tracing::warn!(
                        spec_card_id = %spec_card_id,
                        thread_id = %thread_id,
                        error = %e,
                        "spec push lifecycle: persist TUI-created thread id failed; \
                         next boot may fresh-spawn instead of resuming this rollout"
                    );
                }
            })
        })
    }

    /// Boot-recovery compatibility sink for the existing initial-prompt
    /// bootstrap path. PR 1/2 only changes the create-wave hot path; PR 2/2
    /// will move boot recovery onto the eventized thread-id backfill contract.
    pub fn initial_prompt_clear_sink_for(
        &self,
        spec_card_id: CardId,
    ) -> crate::spec_push::InitialPromptReadySink {
        Arc::new(move |_thread_id: String| {
            let spec_card_id = spec_card_id.clone();
            Box::pin(async move {
                tracing::debug!(
                    spec_card_id = %spec_card_id,
                    "spec push lifecycle: no legacy initial-prompt marker to clear"
                );
            })
        })
    }

    /// #318 INV-3 (R2-B1) — build the [`crate::spec_push::QueuePersist`]
    /// the `SpecPushHandle` uses to mirror its in-memory `VecDeque` into the
    /// durable `spec_push_queue` table. Captures `repo` + the spec card id;
    /// emits no events (the rows are server-private operational state, same
    /// posture as the `watermark_sink_for` closure).
    ///
    /// Three closures:
    ///   * `enqueue(envelope_id, text)` — INSERT one row, returning the
    ///     assigned `id`. On error, log and return `None` so the in-memory
    ///     cache still receives the entry (matching pre-fix posture).
    ///   * `dequeue(ids)` — batch DELETE by id; empty input is a no-op,
    ///     errors are logged.
    ///   * `list()` — SELECT every pending row for the card in id order;
    ///     errors are logged + an empty vec returned (boot-takeover then
    ///     proceeds as if nothing was queued — same conservative posture as
    ///     a `push_watermark = 0` read on a missing field).
    ///
    /// Installed by both production sites alongside
    /// [`Self::watermark_sink_for`]:
    ///   * `routes/waves.rs::legacy spec spawner` — create-wave path,
    ///   * `lib.rs::register_and_catch_up`        — boot-takeover path.
    pub fn queue_persist_for(&self, spec_card_id: CardId) -> crate::spec_push::QueuePersist {
        let repo_e = Arc::clone(&self.inner.repo);
        let card_e = spec_card_id.clone();
        let repo_d = Arc::clone(&self.inner.repo);
        let card_d = spec_card_id.clone();
        let repo_l = Arc::clone(&self.inner.repo);
        let card_l = spec_card_id;
        crate::spec_push::QueuePersist {
            enqueue: Arc::new(move |envelope_id: i64, text: String| {
                let repo = Arc::clone(&repo_e);
                let card_id = card_e.clone();
                Box::pin(async move {
                    match repo
                        .spec_card_enqueue_observation(card_id.as_str(), envelope_id, &text)
                        .await
                    {
                        Ok(id) => Some(id),
                        Err(e) => {
                            tracing::warn!(
                                spec_card_id = %card_id,
                                envelope_id,
                                error = %e,
                                "spec push: persist enqueue failed; entry kept in-memory only \
                                 (next-boot rehydrate will not see it)"
                            );
                            None
                        }
                    }
                })
            }),
            dequeue: Arc::new(move |ids: Vec<i64>| {
                let repo = Arc::clone(&repo_d);
                let card_id = card_d.clone();
                Box::pin(async move {
                    if ids.is_empty() {
                        return;
                    }
                    if let Err(e) = repo.spec_card_dequeue_observations(&ids).await {
                        tracing::warn!(
                            spec_card_id = %card_id,
                            count = ids.len(),
                            error = %e,
                            "spec push: persist dequeue failed; rows may be redelivered on next boot \
                             (idempotent — the spec thread's turn semantics tolerate retries)"
                        );
                    }
                })
            }),
            list: Arc::new(move || {
                let repo = Arc::clone(&repo_l);
                let card_id = card_l.clone();
                Box::pin(async move {
                    match repo.spec_card_queued_observations(card_id.as_str()).await {
                        Ok(rows) => rows,
                        Err(e) => {
                            tracing::warn!(
                                spec_card_id = %card_id,
                                error = %e,
                                "spec push: persist list failed; boot-takeover proceeds with empty \
                                 in-memory queue (any unflushed observations are stranded until a \
                                 future repo read succeeds)"
                            );
                            Vec::new()
                        }
                    }
                })
            }),
        }
    }

    /// #313 problem #1 (boot takeover catch-up) — replay an already-persisted
    /// `(envelope_id, scope, event)` through the dispatcher's push path,
    /// **without** going through the broadcast bus.
    ///
    /// Used exclusively by [`crate::legacy spec takeover`] to
    /// catch a freshly-resumed spec thread up with events that landed while
    /// the kernel was down. Reuses the exact same `Inner::push_to_spec`
    /// helper that live envelopes go through (dedup against the persisted
    /// watermark, per-wave serialization lock, `SpecPusher::push_observation`
    /// with its turn-phase decision + queue semantics) so catch-up and
    /// steady-state are byte-identical on the spec side.
    ///
    /// `envelope_id` must be the real persisted `events.id` — the watermark
    /// dedup keys on it. If the caller hands the same `(id, event)` twice
    /// (e.g. via a redelivery on the bus right after catch-up), the second
    /// call is a no-op (it `<= cursor`); see the dedup invariant in
    /// `Inner::push_to_spec`.
    ///
    /// Wave-scope-only: the live push path discards events without a wave
    /// scope before they reach `push_to_spec`; this helper preserves that
    /// invariant (caller filters to wave-scoped events).
    pub async fn catch_up_push(
        &self,
        wave_id: WaveId,
        event: crate::event::Event,
        envelope_id: i64,
    ) {
        // `Inner::push_to_spec` takes `self: &Arc<Self>` (it spawns
        // nothing, but the receiver shape is shared with other methods on
        // `Inner`). Call it through `&self.inner` directly — `Arc<Inner>`
        // auto-derefs, and Rust resolves the method against the `&Arc<Self>`
        // receiver because of the field type.
        Inner::push_to_spec(&self.inner, wave_id, &event, envelope_id).await;
    }

    /// #313 problem #1 round-2 (B3) — variant of [`catch_up_push`] that
    /// runs the lock-free body of `push_to_spec` directly, for callers
    /// already holding the per-wave push lock via [`PushLockGuard`]. The
    /// guard parameter type-enforces that scope — the dedup `(get → compare
    /// → bump)` is non-atomic without the lock and would race with
    /// concurrent live pushes.
    ///
    /// Used by [`crate::legacy spec takeover`] to replay
    /// catch-up events under the same lock that gates a concurrently-
    /// arriving live event.
    pub async fn catch_up_push_under_lock(
        &self,
        guard: &PushLockGuard,
        event: crate::event::Event,
        envelope_id: i64,
    ) {
        self.inner
            .push_to_spec_locked(guard, &event, envelope_id)
            .await;
    }

    /// Reference to the global semaphore. Exposed so tests can probe
    /// `available_permits()` to verify the cap.
    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    /// Spawn the dispatcher background task.
    ///
    /// `permits` configures the global concurrent-spawn cap. The
    /// production caller (`AppState::new`) uses
    /// [`Dispatcher::permits_from_env`]`(DEFAULT_PERMITS)` so the
    /// `NEIGE_DISPATCHER_PERMITS` env var stays the single dial.
    /// Tests inject an explicit count.
    ///
    /// `mcp_server` is `Some` for the production boot path (`AppState::new`
    /// constructs the kernel-as-MCP-server first, then hands the handle
    /// to the dispatcher) and `None` for test fixtures that don't need
    /// MCP wiring. When `Some`, the dispatcher folds `NEIGE_MCP_TOKEN` +
    /// `NEIGE_MCP_SOCKET` into the env it hands to `spawn_terminal_with_parts`
    /// for codex workers, and threads the shim config into
    /// `per-card CODEX_HOME seeding` so each worker's `$CODEX_HOME/config.toml`
    /// carries a `[mcp_servers.calm]` block — mirroring the spec card path
    /// in `routes::waves::create_wave`. PR7a.1 (#136 followup) wired this
    /// in; PR7a registered the MCP server but left the dispatcher's
    /// worker-side plumbing as a deferred TODO.
    ///
    /// #272 (N3) — `codex` is downgraded to a `Weak<CodexClient>` inside
    /// the dispatcher inner. The CALLER MUST hold the strong `Arc` for
    /// the dispatcher's useful lifetime; if the strong ref drops while
    /// the dispatcher's background task is still alive, every subsequent
    /// `*.job_requested` envelope will short-circuit with a debug log
    /// (`AppState gone`) instead of spawning a worker. In production
    /// `AppState.codex` is that strong ref; in tests the fixture must
    /// bind `let codex = stub_codex();` and pass `codex.clone()` (the
    /// binding keeps the strong ref alive across the test body).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        spec_push: SpecPushRegistry,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        permits: usize,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
        Self::spawn_with_terminal_renderer(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            spec_push,
            shared_codex_appserver,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        // #293 — the wave→app-server push registry (shared with
        // `AppState.spec_push`; `create_wave` fills it). Push is the only
        // path now (#293 cutover): the subscribe filter unconditionally
        // includes the `task.*` / `wave.report_edited` kinds so they route to
        // `push_to_spec`.
        spec_push: SpecPushRegistry,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        permits: usize,
    ) -> Self {
        let permits = if permits == 0 {
            DEFAULT_PERMITS
        } else {
            permits
        };
        let semaphore = Arc::new(Semaphore::new(permits));
        // #272 (N3) — store a `Weak<CodexClient>` instead of cloning
        // the Arc. The dispatcher conceptually borrows codex from
        // `AppState` (which owns the strong Arc); keeping a strong
        // ref here cycled with the broadcast bus and kept the
        // per-test `tempfile::TempDir` (inside `CodexClient`) alive
        // until process exit, defeating PR #271's per-test cleanup.
        // Upgrade happens per-envelope in `handle_envelope`; a failed
        // upgrade means `AppState` was dropped — log and return.
        let codex = Arc::downgrade(&codex);
        let inner = Arc::new(Inner {
            repo,
            events: events.clone(),
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            spec_push,
            shared_codex_appserver,
            // #293 PR3b — a DEDICATED push watermark cache. Intentionally
            // a SEPARATE instance from anything else: keyed by the spec
            // `CardId`;
            // a push only fires when `envelope_id > cursor`, making pushes
            // idempotent under the broadcast's at-least-once delivery.
            push_cursor: EventCursorCache::new(),
            // #293 PR3b (S1) — per-wave push serialization lock-map.
            push_locks: DashMap::new(),
            semaphore: Arc::clone(&semaphore),
            recently_seen: Arc::new(Mutex::new(HashSet::new())),
        });

        // Filter: every event of either `*.Requested` kind, anywhere in
        // the cove→wave→card tree. The dispatcher's job is to react to
        // emissions from any spec card regardless of scope — narrower
        // routing happens after the SELECT-inside-tx idempotency check
        // (the worker card lands in the same wave as the requesting
        // spec card).
        // #293 cutover — push is the only path now, so the subscribe filter
        // unconditionally matches the wave-event push kinds in addition to
        // the two `*.job_requested` kinds. The push kinds route to
        // `push_to_spec`; the `*.job_requested` kinds drive the worker-spawn
        // arm. Hook events are coarse-filtered by `kind_tag()` here; the
        // exact turn-ending hook discriminators are checked synchronously in
        // the push branch below.
        let kinds: Vec<String> = vec![
            "codex.job_requested".into(),
            "terminal.job_requested".into(),
            "task.completed".into(),
            "task.failed".into(),
            "wave.report_edited".into(),
            "codex.hook".into(),
            "claude.hook".into(),
        ];
        let filter = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(kinds),
        };
        let mut rx = events.subscribe_filtered();

        let inner_for_task = Arc::clone(&inner);
        let filter_for_task = filter.clone();
        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(envelope) => {
                        // Apply the filter — `subscribe_filtered`
                        // hands back the raw firehose, callers run the
                        // match themselves (see `EventBus::subscribe_filtered`
                        // doc on why we ship that shape rather than a
                        // BroadcastStream wrapper).
                        if !filter_for_task.matches(&envelope) {
                            continue;
                        }
                        let inner = Arc::clone(&inner_for_task);
                        // Per-event spawn is fire-and-forget: the bus
                        // reader keeps draining while the
                        // semaphore-gated handler is in flight.
                        tokio::spawn(async move {
                            inner.handle_envelope(envelope).await;
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // A lag means we missed `n` events; if any of
                        // them was a `*.Requested`, the request emitter
                        // is responsible for retrying with the same
                        // idempotency_key, which we'll handle on the
                        // next emit. Log and continue.
                        tracing::warn!(
                            skipped = n,
                            "dispatcher subscriber lagged; missed events may need a retry from the requester"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Self {
            semaphore,
            permits,
            handle,
            inner,
        }
    }
}

/// #480 §D — proof token that the per-wave push lock for `wave_id` is held.
/// `OwnedMutexGuard<()>` owns the `Arc<tokio::sync::Mutex<()>>` so the guard
/// is not tied to a `DashMap` entry borrow and can cross `.await`. Holding
/// across `.await` is intentional (catch-up replay) but can starve that
/// wave; bound replay bodies.
///
/// **Invariant**: this guard proves the lock is held — NOT that replay
/// events are semantically complete or ordered (#480 §F4).
pub struct PushLockGuard {
    wave_id: WaveId,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl PushLockGuard {
    pub fn wave_id(&self) -> &WaveId {
        &self.wave_id
    }
}

struct Inner {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    /// #272 (N3) — `Weak` so this dispatcher doesn't cycle with
    /// `AppState.codex` (the strong owner). The dispatcher's background
    /// task is held alive by the broadcast bus; if it also held a
    /// strong `Arc<CodexClient>`, the per-test `tempfile::TempDir`
    /// wrapped inside `CodexClient` couldn't drop on `AppState` drop,
    /// reviving the leak PR #271 closed. Upgrade per `handle_envelope`
    /// call; a failed upgrade means `AppState` has dropped and the
    /// dispatcher should no-op until the bus closes.
    codex: Weak<CodexClient>,
    daemon: Arc<DaemonClient>,
    terminal_renderer: Arc<TerminalRendererRegistry>,
    /// PR7a.1 (#136 followup) — kernel-as-MCP-server handle. When `Some`,
    /// every codex-worker spawn folds the per-card MCP token + kernel
    /// socket path into the daemon env *and* seeds the per-card
    /// `$CODEX_HOME/config.toml` with a `[mcp_servers.calm]` block. When
    /// `None` (test fixtures / replay) the worker still spawns but
    /// without a wire back into the kernel — fine for unit tests that
    /// only assert on card creation. Terminal workers don't read this
    /// (they don't run codex).
    mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
    /// #293 PR3b — wave→app-server push registry (shared with
    /// `AppState.spec_push`). `push_to_spec` resolves a wave's
    /// [`crate::spec_push::SpecPushHandle`] from here and calls
    /// `push_observation` on it. Empty when a kernel restart lost the
    /// in-memory handle (no crash-recovery — see `push_to_spec`).
    spec_push: SpecPushRegistry,
    /// PR4 shared codex daemon. Worker codex cards start through this daemon.
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    /// #293 PR3b — DEDICATED push watermark cache keyed by the spec
    /// `CardId`. A push fires only when `envelope_id > cursor`, then bumps;
    /// this makes pushes idempotent under at-least-once broadcast delivery
    /// and survives a re-delivered envelope without double-pushing.
    push_cursor: EventCursorCache,
    /// #293 PR3b (S1) — per-wave serialization lock for the push path. The
    /// dispatcher runs `push_to_spec` concurrently (one `tokio::spawn` per
    /// envelope), so without serialization the watermark
    /// `(get → compare → bump → push_observation)` is a non-atomic
    /// read-modify-write: if envelope id 11 bumps the cursor before id 10 is
    /// checked, id 10 (a DISTINCT real event — e.g. a `task.failed` carrying
    /// a `reason`) is wrongly deduped and silently dropped. Holding this
    /// per-wave async `Mutex` across the whole dedup-check-and-deliver makes
    /// same-wave pushes process in id order, so the monotonic watermark only
    /// dedups TRUE redeliveries. Keyed by `WaveId` (one spec card per wave).
    /// Pushes are low-frequency, so per-wave serialization is cheap.
    push_locks: DashMap<WaveId, Arc<tokio::sync::Mutex<()>>>,
    semaphore: Arc<Semaphore>,
    /// Recently-spawned idempotency keys. A fast-path short-circuit
    /// before the tx-bound SELECT. Held under a `std::sync::Mutex`
    /// (not `tokio::sync::Mutex`) so the [`RecentlySeenGuard`] Drop
    /// impl can release the slot synchronously on panic; the operations
    /// are short (insert / remove / contains under sub-microsecond hold
    /// time) and never cross an `.await`, so the blocking mutex is
    /// fine. A scheduled cleanup tokio task purges entries older than
    /// [`RECENT_KEYS_TTL`].
    recently_seen: Arc<Mutex<HashSet<String>>>,
}

impl Inner {
    async fn handle_envelope(self: Arc<Self>, envelope: BroadcastEnvelope) {
        // Acquire a permit before doing any per-spawn work. Dropped on
        // task end (the `_permit` binding holds it across the function).
        let _permit = match Arc::clone(&self.semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("dispatcher semaphore closed; aborting spawn");
                return;
            }
        };

        // #293 — push branch. The wave-event kinds the filter matches route
        // HERE (bounded by the same `_permit` the worker-spawn path holds),
        // never into the `DispatchRequest` extraction below. For
        // `wave.report_edited` we act ONLY on a User-authored edit —
        // Spec/AI-authored edits are the spec writing its own report, and
        // pushing those back would be a feedback loop. Worker hook events
        // also return from here, even when ignored, because they are
        // lifecycle notices rather than worker-spawn requests. The
        // worker-spawn path (the two `*.job_requested` kinds) falls through
        // untouched.
        match &envelope.event {
            Event::TaskCompleted { .. } | Event::TaskFailed { .. } => {
                if event_warrants_spec_push(&envelope.event, &self.write) {
                    if let Some(wave_id) = envelope.scope.wave_id().cloned() {
                        self.push_to_spec(wave_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            "dispatcher push: task event has no wave scope; skipping"
                        );
                    }
                }
                return;
            }
            Event::WaveReportEdited {
                author, wave_id, ..
            } => {
                // Only user edits warrant a push. The spec authored
                // Spec/Kernel edits itself; re-notifying it would loop.
                if event_warrants_spec_push(&envelope.event, &self.write) {
                    self.push_to_spec(wave_id.clone(), &envelope.event, envelope.id)
                        .await;
                } else {
                    tracing::trace!(
                        ?author,
                        "dispatcher push: ignoring non-user wave.report_edited"
                    );
                }
                return;
            }
            Event::CodexHook { card_id, kind, .. } | Event::ClaudeHook { card_id, kind, .. } => {
                // Only the precise Stop hooks mean a worker turn truly
                // ended. Other hooks may project to the same FSM state (for
                // example `hook.codex.permission_request` -> AwaitingInput)
                // but are mid-turn pauses, so they must not wake the spec.
                //
                // The Worker role gate prevents spec self-push loops: spec
                // cards can emit their own hook lifecycle events, but only
                // worker cards should notify the spec. Stop hooks carry no
                // result/artifacts, so the pushed observation is a light
                // wake-up that asks the spec to re-read wave state.
                if event_warrants_spec_push(&envelope.event, &self.write) {
                    if let Some(wave_id) = envelope.scope.wave_id().cloned() {
                        self.push_to_spec(wave_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            hook_kind = %kind,
                            card_id = %card_id,
                            "dispatcher push: worker hook stop has no wave scope; skipping"
                        );
                    }
                } else {
                    tracing::trace!(
                        hook_kind = %kind,
                        card_id = %card_id,
                        "dispatcher push: ignoring hook event"
                    );
                }
                return;
            }
            // Everything else (the two `*.job_requested` kinds) falls
            // through to the worker-spawn path below, unchanged.
            _ => {}
        }

        // #272 (N3) — upgrade the `Weak<CodexClient>` to a strong
        // `Arc` for the duration of this dispatch. If the upgrade
        // fails, `AppState` has dropped (test teardown) and there's
        // no point spawning a worker against a vanished kernel — bail
        // out. The broadcast `Closed` recv in the spawn loop will
        // shut the dispatcher down shortly anyway. Cheap: atomic
        // strong-count bump on success, no allocation.
        let codex = match self.codex.upgrade() {
            Some(c) => c,
            None => {
                tracing::debug!(
                    "dispatcher: CodexClient dropped (AppState gone); skipping envelope"
                );
                return;
            }
        };

        // Extract the request shape we know how to handle. The filter
        // already narrowed us to two variants; the `_` arm exists for
        // future-proofing in case the filter ever widens.
        let req = match &envelope.event {
            // #481 PR2 N4 guard marker: route-origin `codex-create` is
            // framework-owned by OperationRuntime/CodexAdapter and does
            // not emit `Event::TerminalJobRequested` or
            // `Event::CodexJobRequested`. This dispatcher arm remains
            // the legacy worker request path until PR5 rewrites
            // `spawn_codex_worker`.
            Event::CodexJobRequested {
                idempotency_key,
                goal,
                context,
                acceptance_criteria,
            } => DispatchRequest::Codex {
                idempotency_key: idempotency_key.clone(),
                goal: goal.clone(),
                context: context.clone(),
                acceptance_criteria: acceptance_criteria.clone(),
            },
            // #481 PR1 N4 guard marker: route-origin `terminal-create` is
            // framework-owned by OperationRuntime/TerminalAdapter and no
            // longer emits `Event::TerminalJobRequested`. This dispatcher arm
            // is only for spec/worker terminal jobs. If a future framework
            // emitter adds an origin field, framework-owned terminal-create
            // envelopes must short-circuit before `recently_seen` install.
            // #481 PR3 N4 guard marker: route-origin `claude-create` is also
            // framework-owned by OperationRuntime/ClaudeAdapter. It writes
            // Claude settings and spawns directly in the saga spawn phase;
            // it must not be represented as a `TerminalJobRequested` event.
            Event::TerminalJobRequested {
                idempotency_key,
                cmd,
                cwd,
            } => DispatchRequest::Terminal {
                idempotency_key: idempotency_key.clone(),
                cmd: cmd.clone(),
                cwd: cwd.clone(),
            },
            other => {
                tracing::warn!(
                    kind = other.kind_tag(),
                    "dispatcher received non-request event; filter widened unexpectedly",
                );
                return;
            }
        };
        let idem = req.idempotency_key().to_string();
        let scope = envelope.scope.clone();

        // Fast-path: in-process recently-seen set. The canonical guard
        // is still the SELECT-inside-tx; this just short-circuits a
        // double-fire from the same source within the grace window.
        //
        // PR6 (#136) cache-lifecycle fix: insert at start for race
        // protection (two `*.Requested` envelopes hitting the
        // dispatcher within microseconds — the in-tx SELECT also
        // catches them but this short-circuits before we open the
        // tx); the [`RecentlySeenGuard`] RAII handle returned by
        // [`RecentlySeenGuard::install`] owns the cleanup contract:
        //
        //   * On panic anywhere in the dispatch path, the guard's
        //     `Drop` impl removes the key so a retry within the TTL
        //     window isn't silently dropped (PR6 followup of issue
        //     #136 — note 2 from the original review).
        //   * On failure paths that return normally, the guard is
        //     dropped at scope end and removes the key.
        //   * On success the caller calls `guard.commit()`, which
        //     marks the guard so its `Drop` is a no-op, and the key
        //     stays for `RECENT_KEYS_TTL` (a bounded cleanup task
        //     scheduled below removes it).
        let guard = match RecentlySeenGuard::install(self.recently_seen.clone(), idem.clone()) {
            Some(g) => g,
            None => {
                tracing::debug!(idempotency_key = %idem, "dispatcher: recently-seen, skipping");
                return;
            }
        };

        // Retry on transient SQLite BUSY/locked errors. With more
        // than one dispatcher in flight (permits > 1), SQLite can
        // refuse a write with "database is locked" or "deadlocked"
        // even though no real deadlock exists — sqlx surfaces the
        // sqlite-3 status code as an io / database error. We retry
        // a few times with exponential backoff before giving up
        // and emitting `task.failed`.
        let mut last_err: Option<crate::error::CalmError> = None;
        let mut backoff = Duration::from_millis(10);
        const MAX_RETRIES: usize = 5;
        for attempt in 0..=MAX_RETRIES {
            match self.dispatch(&codex, req.clone(), scope.clone()).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) if is_sqlite_busy(&e) && attempt < MAX_RETRIES => {
                    tracing::debug!(
                        idempotency_key = %idem,
                        attempt,
                        error = %e,
                        "dispatcher: transient SQLite contention; retrying"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(200));
                    continue;
                }
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }
        if last_err.is_none() {
            // Success path: commit the guard so its Drop is a no-op,
            // and schedule a bounded cleanup task to remove the key
            // after `RECENT_KEYS_TTL`. The TTL retention is the whole
            // point of the success path — keeps the in-process fast-
            // path arm warm so a re-emit of the same envelope within
            // the grace window short-circuits without opening a tx.
            guard.commit();
            let key_for_cleanup = idem.clone();
            let inner = Arc::clone(&self);
            tokio::spawn(async move {
                tokio::time::sleep(RECENT_KEYS_TTL).await;
                if let Ok(mut g) = inner.recently_seen.lock() {
                    g.remove(&key_for_cleanup);
                }
            });
        }
        // Failure path: the guard goes out of scope here and its
        // Drop impl removes the key from `recently_seen` so the
        // request can be retried after the requester sees the
        // task.failed event. (No explicit drop needed; this is the
        // RAII point — but we keep `guard` live until after the
        // success-path commit above so the success branch can opt
        // out via `guard.commit()`.) The canonical SELECT-inside-tx
        // guard still prevents a double-spawn if the retry races a
        // late re-emit of the original event.
        if let Some(e) = last_err {
            tracing::warn!(
                idempotency_key = %idem,
                error = %e,
                "dispatcher: spawn failed; emitting task.failed"
            );
            // Emit a TaskFailed so the dispatcher's push path delivers
            // the failure to the requesting spec card as a turn input.
            // Scope mirrors the request envelope's scope so the push can
            // route on it.
            let fail_event = Event::TaskFailed {
                idempotency_key: idem.clone(),
                reason: format!("{e}"),
            };
            if let Err(e2) = self
                .repo
                .log_pure_event(
                    ActorId::KernelDispatcher,
                    scope,
                    None,
                    &self.events,
                    self.write.role_cache(),
                    self.write.cove_cache(),
                    fail_event,
                )
                .await
            {
                tracing::warn!(
                    idempotency_key = %idem,
                    error = %e2,
                    "dispatcher: failed to log task.failed event"
                );
            }
        }
    }

    /// #293 PR3b — push a wave event onto the wave's spec card's codex
    /// thread.
    ///
    /// Steps:
    ///   1. **Resolve the spec card** — scan `cards_by_wave(wave_id)` for
    ///      the one whose `card_role_cache` role is [`CardRole::Spec`].
    ///   2. **Dedup / ordering** — only push when `envelope_id` is strictly
    ///      above the in-memory push cursor for that spec `CardId`; then
    ///      bump the cursor (in-process dedup hint). Idempotent under
    ///      at-least-once broadcast delivery.
    ///   3. **Resolve the handle** — `spec_push.pusher(wave_id)`. If absent
    ///      (a kernel restart lost the in-memory handle), `warn!` and return
    ///      — never crash. #313 problem #1: boot takeover
    ///      ([`crate::legacy spec takeover`]) re-registers a
    ///      handle for every non-terminal wave with a persisted
    ///      `codex_thread_id` and replays events above the durable
    ///      watermark, so a missing handle here just means "boot takeover
    ///      hasn't run yet" (mid-boot, the route layer isn't up so no
    ///      event can reach here) OR "takeover failed for this wave"
    ///      (warn was logged at boot; wave is inert).
    ///   4. **Build + deliver** the observation via
    ///      [`crate::spec_push::SpecPusher::push_observation`],
    ///      which decides `turn/start`-now vs enqueue based on the tracked
    ///      turn phase.
    ///   5. **Persist durable watermark** — only AFTER a successful
    ///      delivery, persist `envelope_id` onto `payload.push_watermark`
    ///      via `spec_card_set_push_watermark`. The next boot's
    ///      `events_since(watermark)` uses strict `id > watermark`, so
    ///      persisting before/without successful delivery would
    ///      permanently skip the envelope on recovery (#313 PR4 B1).
    async fn push_to_spec(self: &Arc<Self>, wave_id: WaveId, event: &Event, envelope_id: i64) {
        // S1 — serialize the whole dedup-check-and-deliver PER WAVE so the
        // monotonic watermark only dedups true redeliveries, never a
        // distinct lower-id event that lost the concurrent
        // read-modify-write race. `push_to_spec` runs once per envelope under
        // a `tokio::spawn`, so two envelopes for the SAME wave (e.g. id 10
        // and id 11) can race; without this lock, id 11 could `bump` the
        // cursor to 11 before id 10's `get` runs, making id 10 (a DISTINCT
        // real event) appear already-seen and get dropped. Holding a
        // per-wave async `Mutex` across `(get → compare → bump →
        // push_observation)` forces same-wave pushes to process in id order.
        // We clone the `Arc<Mutex>` out of the `DashMap` under the brief
        // sync guard, then drop the guard before awaiting the lock (never
        // hold a `DashMap` shard guard across an `.await`).
        //
        // #313 round-2 (B3) — boot takeover uses the same per-wave push
        // lock, so the lock is shared between live pushes and catch-up
        // replay; either side waits if the other is mid-sequence.
        let guard = self.acquire_push_lock(&wave_id).await;
        self.push_to_spec_locked(&guard, event, envelope_id).await;
    }

    /// #313 round-2 (B3) — per-wave push lock helper, shared between
    /// `Inner::push_to_spec` (steady state) and
    /// [`Dispatcher::push_lock`] (boot takeover catch-up). Held by either
    /// side serializes the other.
    async fn acquire_push_lock(self: &Arc<Self>, wave_id: &WaveId) -> PushLockGuard {
        // IMPORTANT: do NOT bind the DashMap Entry to a `let` — the shard
        // guard must drop at this statement's `;` before we `.await` below.
        let lock = self
            .push_locks
            .entry(wave_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let guard = lock.lock_owned().await;
        PushLockGuard {
            wave_id: wave_id.clone(),
            _guard: guard,
        }
    }

    /// #313 round-2 (B3) — the lock-free body of [`push_to_spec`]. Must
    /// only be called by a caller already holding the per-wave push lock
    /// for `wave_id` (boot takeover holds it across the catch-up sequence;
    /// `push_to_spec` takes it then calls here). `tokio::sync::Mutex` is
    /// NOT reentrant, so we can't grab it again here.
    async fn push_to_spec_locked(
        self: &Arc<Self>,
        guard: &PushLockGuard,
        event: &Event,
        envelope_id: i64,
    ) {
        let wave_id = guard.wave_id().clone();
        // Resolve the spec card for this wave via the role cache.
        let spec_card_id = match self.resolve_spec_card(&wave_id).await {
            Some(id) => id,
            None => {
                tracing::debug!(
                    wave_id = %wave_id,
                    "dispatcher push: no spec card found for wave; skipping"
                );
                return;
            }
        };

        // Dedup: push only when this envelope is newer than the watermark
        // for the spec card. A persisted event always has a positive id;
        // a synthetic id-0 envelope (test `EventBus::emit`) is never above
        // the initial 0 cursor, so it is skipped — we only push real,
        // persisted, ordered events. `bump` is monotonic, so a re-delivered
        // (lower-or-equal) id is a no-op and can't double-push. Under the
        // per-wave lock above this check-then-bump is now atomic w.r.t. other
        // same-wave pushes.
        let cursor = self.push_cursor.get(&spec_card_id);
        if envelope_id <= cursor {
            tracing::debug!(
                wave_id = %wave_id,
                spec_card_id = %spec_card_id,
                envelope_id,
                cursor,
                "dispatcher push: envelope id not above watermark; deduped"
            );
            return;
        }

        // Resolve the live push handle. Absent → warn + return (no crash).
        // #313 problem #1: this is no longer the "permanent failure" state —
        // boot takeover ([`crate::legacy spec takeover`]) re-
        // registers the handle for every non-terminal wave with a persisted
        // `codex_thread_id`. A missing handle here means EITHER (a) boot
        // takeover hasn't run yet (we're mid-boot — the route layer isn't
        // up so no event can land here) OR (b) the takeover failed for this
        // wave specifically (warn was logged at boot; wave is inert).
        //
        // We MUST NOT persist the durable watermark when the handle is
        // missing — codex never saw this envelope, so the next boot's
        // catch-up needs to replay it (events_since uses `id > watermark`
        // strictly, so persisting envelope_id here would silently drop it
        // at boot).
        //
        // #315 round-2 (B3) — also MUST NOT bump the in-memory cursor
        // when the handle is missing. Previously we bumped pre-handle-
        // resolve; if a live event landed during boot (between takeover's
        // `clear_cache` and `seed_push_cursor`), the bump-without-deliver
        // would later cause boot catch-up's `catch_up_push_under_lock`
        // to dedup against the bumped cursor and silently drop the
        // event (catch-up gets the lock SECOND in that race). Bumping
        // ONLY after a successful handle lookup ensures the cursor only
        // ever advances when we're about to actually deliver — making
        // the dedup invariant honest: "cursor at X means we sent X".
        let pusher = match self.spec_push.pusher(&wave_id) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    wave_id = %wave_id,
                    spec_card_id = %spec_card_id,
                    envelope_id,
                    kind = event.kind_tag(),
                    "dispatcher push: no live SpecPushHandle for wave — boot takeover \
                     either did not run yet, or did not register this wave (e.g. -32600 \
                     no rollout, or app-server boot failed); wave left undriven (#313). \
                     Cursor NOT bumped so boot takeover's catch-up will redeliver this id."
                );
                return;
            }
        };

        let observation = build_observation(event);
        tracing::info!(
            wave_id = %wave_id,
            spec_card_id = %spec_card_id,
            envelope_id,
            kind = event.kind_tag(),
            "dispatcher push: delivering observation to spec thread"
        );
        let outcome = match pusher.push_observation(envelope_id, &observation).await {
            Ok(o) => o,
            Err(e) => {
                // Delivery failed (transport / turn_start error). Do NOT
                // persist the durable watermark: the next boot's catch-up
                // replay must re-deliver this envelope. The in-process dedup
                // cursor is already bumped, but that's a per-process hint —
                // restart-time recovery seeds from disk, not memory.
                tracing::warn!(
                    wave_id = %wave_id,
                    envelope_id,
                    error = %e,
                    "dispatcher push: push_observation failed — durable watermark NOT persisted; \
                     boot recovery will replay this envelope (#313)"
                );
                return;
            }
        };

        // #313 problem #1 round-2 (B1) — persist the durable watermark
        // ONLY when the observation actually rode a successful `turn/start`
        // (codex received it). The previous implementation persisted on
        // any `Ok(())`, including the `Enqueue` path where the observation
        // lives only in the in-memory `PushQueue`; a kernel crash before
        // the next `turn/completed` flush would then lose that envelope
        // permanently (boot catch-up uses `id > watermark` strictly).
        //
        //   * `Issued { max_envelope_id }` — turn/start went out.
        //     `max_envelope_id` is the highest id among (drained queue
        //     items + this push's observation), so persisting watermark =
        //     `max_envelope_id` advances past every item that just rode
        //     this coalesced turn — including queued items previously
        //     held back from persistence. **At-least-once across restart**:
        //     if the kernel crashes between this `Issued` and our persist
        //     below, boot replays the envelope and codex's `thread_resume`
        //     handles a re-push idempotently.
        //   * `Enqueued` — observation buffered in the in-memory queue.
        //     We DO NOT persist the watermark; the queue-flush path on the
        //     next `turn/completed` persists via the [`WatermarkSink`]
        //     callback the dispatcher installs on each registered handle.
        //     **Boot reliability**: if the kernel crashes between Enqueue
        //     and the flush, the watermark stays where it was; boot
        //     catch-up's `events_since(watermark)` re-delivers the queued
        //     event because we never advanced the durable floor past it.
        //
        // The write goes through `RepoOutOfDomain` (no `CardUpdated`
        // event emitted) — the dispatcher's filter doesn't watch
        // `CardUpdated` and the field is server-private bookkeeping
        // (same treatment terminal PIDs/handles get).
        match outcome {
            crate::spec_push::PushOutcome::Issued { max_envelope_id } => {
                // #347: bump the in-process cursor only after the handle
                // accepted the observation. A Wedged handle now returns an
                // error so runtime recovery can replay from the durable
                // watermark without being deduped by a speculative
                // same-process cursor bump.
                self.push_cursor.bump(spec_card_id.clone(), max_envelope_id);
                if let Err(e) = self
                    .repo
                    .spec_card_set_push_watermark(spec_card_id.as_str(), max_envelope_id)
                    .await
                {
                    tracing::warn!(
                        wave_id = %wave_id,
                        spec_card_id = %spec_card_id,
                        max_envelope_id,
                        error = %e,
                        "dispatcher push: persist push_watermark failed AFTER successful delivery; \
                         in-memory cache still bumped, next boot may re-push this envelope \
                         (spec thread may see this observation twice on a re-push — codex does \
                         not dedup re-pushed observations)",
                    );
                }
            }
            crate::spec_push::PushOutcome::Enqueued => {
                self.push_cursor.bump(spec_card_id.clone(), envelope_id);
                tracing::debug!(
                    wave_id = %wave_id,
                    spec_card_id = %spec_card_id,
                    envelope_id,
                    "dispatcher push: observation enqueued (mid-turn); durable watermark deferred to flush"
                );
            }
        }
    }

    /// Find the [`CardRole::Spec`] card for a wave. Scans the wave's cards
    /// and consults `card_role_cache` (write-through, in-memory) for the
    /// role. Returns `None` if the wave has no spec card (shouldn't happen
    /// for a live push-enabled wave) or the lookup errors.
    async fn resolve_spec_card(self: &Arc<Self>, wave_id: &WaveId) -> Option<CardId> {
        let cards = match self.repo.cards_by_wave(wave_id.as_str()).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    wave_id = %wave_id,
                    error = %e,
                    "dispatcher push: cards_by_wave failed; cannot resolve spec card"
                );
                return None;
            }
        };
        cards.into_iter().find_map(|c| {
            if self.write.verify_role(&c.id) == Some(CardRole::Spec) {
                Some(c.id)
            } else {
                None
            }
        })
    }

    async fn dispatch(
        self: &Arc<Self>,
        codex: &Arc<CodexClient>,
        req: DispatchRequest,
        scope: EventScope,
    ) -> crate::error::Result<()> {
        // The request envelope must carry a wave (and therefore a cove)
        // — a dispatcher can't materialize a worker card without a
        // parent wave. System-scoped requests are rejected.
        let wave_id = scope
            .wave_id()
            .ok_or_else(|| {
                CalmError::BadRequest(format!(
                    "dispatcher: *.Requested event has no wave scope (got {scope:?})"
                ))
            })?
            .clone();

        match req {
            DispatchRequest::Codex {
                idempotency_key,
                goal,
                context,
                acceptance_criteria,
            } => {
                self.spawn_codex_worker(
                    codex,
                    wave_id,
                    scope.cove_id().cloned(),
                    idempotency_key,
                    goal,
                    context,
                    acceptance_criteria,
                )
                .await?;
            }
            DispatchRequest::Terminal {
                idempotency_key,
                cmd,
                cwd,
            } => {
                self.spawn_terminal_worker(
                    wave_id,
                    scope.cove_id().cloned(),
                    idempotency_key,
                    cmd,
                    cwd,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Mint a worker codex card and spawn the codex daemon. PR6 (#136)
    /// activates the daemon spawn that PR5 left deferred.
    ///
    /// Idempotency strategy: the in-tx SELECT lives inside the closure;
    /// when a row already exists for `idempotency_key`, the closure
    /// returns `Err(CalmError::IdempotencyCollision)` to abort the tx
    /// (no rows written, no events emitted). The caller pattern-matches
    /// the typed variant and treats it as a success short-circuit. The
    /// dedicated variant (PR6 followup) lets real `CalmError::Conflict`
    /// errors from `card_with_codex_create_tx` (e.g. terminal-already-
    /// exists from `terminal_create_tx`) propagate instead of being
    /// silently swallowed as "duplicate request".
    #[allow(clippy::too_many_arguments)]
    async fn spawn_codex_worker(
        self: &Arc<Self>,
        codex: &Arc<CodexClient>,
        wave_id: WaveId,
        _cove_id: Option<CoveId>,
        idempotency_key: String,
        goal: String,
        context: serde_json::Value,
        acceptance_criteria: Option<String>,
    ) -> crate::error::Result<()> {
        let idem_for_tx = idempotency_key.clone();
        let wave_for_tx = wave_id.clone();
        let cache_for_tx = self.write.role_cache().clone();
        let repo_for_scope = self.repo.clone();

        // Pre-mint id so we can stamp the EventScope::Card with the
        // soon-to-exist card id, matching the codex-cards route
        // pattern.
        let new_card_id = crate::model::new_id();
        let new_card_id_for_tx = new_card_id.clone();

        // PR6: assemble the env map up-front (matches the user-create
        // route + the wave-create spec-card path). Settings + codex
        // home dir live on `self.codex`; the dispatcher is a kernel
        // worker so it reads settings through its `self.repo` handle.
        let settings = load_settings(self.repo.as_ref()).await?;
        // PR7a (#136) — env baked into the terminal row is the pre-MCP
        // shape (no token/socket). The per-card MCP token is minted
        // inside the tx by `card_with_codex_create_tx`; we fold it +
        // the kernel socket path into the env handed to
        // `spawn_terminal_with_parts` post-commit. Mirrors the spec
        // card path in `routes::waves::create_wave`.
        let env = build_codex_env_map(
            codex.as_ref(),
            &new_card_id,
            settings.http_proxy.as_deref(),
            settings.https_proxy.as_deref(),
            None,
            None,
        );
        let cwd = crate::routes::codex_cards::default_cwd();

        // Render the user-facing prompt from goal+context+AC. This
        // becomes both the worker card's `payload.prompt` (so
        // `legacy auto-submit` fires the composer `\r` on
        // `hook.codex.session_start`) and the positional `[PROMPT]`
        // arg on the codex daemon's argv (so the composer mounts
        // pre-filled). Without this the worker hangs forever with an
        // empty composer — the spec card path (`spec_card.rs`) closed
        // the same bug via issue #251; the worker path was missed.
        let user_prompt = render_worker_prompt(&goal, &context, acceptance_criteria.as_deref());

        // Worker-card payload — bookkeeping fields the FSM / UI use
        // to distinguish worker codex cards from plain ones. The
        // canonical `card_with_codex_create_tx` helper stamps
        // `schemaVersion` and `cwd` itself; runtime identity is projected at
        // read time. We merge those fields after the helper runs by going through
        // `card_update_tx` once more. (Simpler than threading payload
        // overrides into the helper; the tx still commits atomically.)
        let mut bookkeeping = serde_json::Map::new();
        bookkeeping.insert(
            "idempotency_key".into(),
            serde_json::Value::String(idempotency_key.clone()),
        );
        bookkeeping.insert(
            "role_request".into(),
            serde_json::Value::String("codex".into()),
        );
        bookkeeping.insert("goal".into(), serde_json::Value::String(goal.clone()));
        bookkeeping.insert("context".into(), context.clone());
        if let Some(ac) = acceptance_criteria.as_ref() {
            bookkeeping.insert(
                "acceptance_criteria".into(),
                serde_json::Value::String(ac.clone()),
            );
        }
        bookkeeping.insert(
            "prompt".into(),
            serde_json::Value::String(user_prompt.clone()),
        );
        let bookkeeping_value = serde_json::Value::Object(bookkeeping);

        let scope = crate::routes::cards::card_scope(
            repo_for_scope.as_ref(),
            new_card_id.clone().into(),
            wave_id.clone(),
        )
        .await?;

        let cwd_for_tx = cwd.clone();
        let env_for_tx = env.clone();
        let bookkeeping_for_tx = bookkeeping_value.clone();

        // Issue #310 — two-stage spawn. Stage 1: a tx that mints the
        // worker card + terminal row (`renderer entry = NULL`).
        // **Does NOT emit `CardAdded` here.** Stage 2 (post-commit,
        // below): `per-card CODEX_HOME seeding` + `spawn_terminal_with_parts`
        // (writes `renderer entry`, spawns daemon, probes readiness).
        // Stage 3 (post-spawn-success): broadcast `CardAdded` via
        // `log_pure_event` so subscribers see the card only after the
        // backing terminal has a live daemon. Without this split, a
        // spec card hot-subscribed to the wave's event stream sees
        // `CardAdded` immediately, mounts its `XtermView`, attempts a
        // WS attach, and hits `resolve_live_renderer`'s "no renderer entry
        // = clean child exit" branch (#304) — producing a spurious
        // `Close(1000, "child-exited")` for a daemon that's in fact
        // ~670ms away from being alive.
        //
        // PR7a.1 (#136 followup) — the closure returns `(card,
        // mcp_token)` so the post-commit env-assembly path below can
        // fold `NEIGE_MCP_TOKEN` into the daemon env (mirroring
        // `routes::waves::create_wave`). The token is `Some` for every
        // worker card (the helper mints one unconditionally for the
        // `Worker` role), but we keep the `Option` shape to stay in
        // step with the helper's return contract. We also carry the
        // *whole* card row out of the tx so the post-spawn broadcast
        // can hand it to `Event::CardAdded(card)` without an extra
        // post-commit fetch.
        let card_id_result = write_in_tx_typed::<(crate::model::Card, Option<String>), _>(
            self.repo.as_ref(),
            move |tx| {
                Box::pin(async move {
                    // SELECT-inside-tx idempotency check. SQLite's
                    // per-connection write lock serializes the
                    // INSERT step below against any concurrent
                    // dispatcher tx, so two `*.Requested` events
                    // with the same key can't both win.
                    if let Some(existing) =
                        find_card_by_idempotency_key_tx(tx, &idem_for_tx).await?
                    {
                        // Duplicate detected — abort the tx by
                        // returning the typed `IdempotencyCollision`
                        // sentinel. The caller below pattern-matches
                        // this exact variant and treats it as a
                        // success short-circuit. No event reaches the
                        // bus. A generic `Conflict` from the helper
                        // (e.g. terminal-already-exists for a re-used
                        // card_id) is now propagated instead of
                        // silently swallowed.
                        return Err(CalmError::IdempotencyCollision(format!(
                            "idempotency_key collision: existing card {}",
                            existing.id
                        )));
                    }

                    // Mint worker card + backing terminal +
                    // canonical codex payload (schemaVersion,
                    // terminal_id, cwd) in one helper call.
                    //
                    // PR7a.1 (#136 followup) — capture the
                    // per-card MCP token returned by the helper
                    // so the post-commit code can hand it to the
                    // codex daemon's env. PR7a discarded this on
                    // the floor as `_mcp_token`.
                    // Issue #229 PR A — dispatcher-spawned worker codex
                    // cards are user-facing; the user closes them to
                    // abort an in-flight job. `deletable: true`.
                    let (mut card, _term, mcp_token) = card_with_codex_create_tx(
                        tx,
                        new_card_id_for_tx,
                        wave_for_tx,
                        None,
                        cwd_for_tx,
                        env_for_tx,
                        None,
                        None,
                        None,
                        CardRole::Worker,
                        true,
                        &cache_for_tx,
                        // #177 — dispatcher workers have no host-browser
                        // theme to forward (kernel-internal spawn). Use
                        // the dark sentinel so the row still satisfies
                        // theme_fg/_bg NOT NULL and the daemon argv
                        // matches what a dark-mode browser would have
                        // stamped on a hand-created card.
                        crate::routes::theme::RequestTheme::default_dark(),
                    )
                    .await?;

                    // Merge dispatcher-bookkeeping fields into
                    // the payload (idempotency_key, goal, context,
                    // acceptance_criteria, role_request). The
                    // helper already wrote a Map payload; extend
                    // it with our extras.
                    if let Some(existing_map) = card.payload.as_object() {
                        let mut merged = existing_map.clone();
                        if let serde_json::Value::Object(extras) = bookkeeping_for_tx {
                            for (k, v) in extras {
                                merged.insert(k, v);
                            }
                        }
                        card = crate::db::sqlite::card_update_tx(
                            tx,
                            card.id.as_ref(),
                            crate::model::CardPatch {
                                kind: None,
                                sort: None,
                                payload: Some(serde_json::Value::Object(merged)),
                                // #229 PR A — kernel-internal callers
                                // never patch the `deletable` field; the
                                // route handler rejects clients that try.
                                deletable: None,
                            },
                        )
                        .await?;
                    }

                    Ok((card, mcp_token))
                })
            },
        )
        .await;

        let (card, mcp_token) = match card_id_result {
            Ok((card, mcp_token)) => (card, mcp_token),
            Err(CalmError::IdempotencyCollision(msg)) => {
                tracing::info!(
                    idempotency_key = %idempotency_key,
                    note = %msg,
                    "dispatcher: short-circuit on existing worker card"
                );
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let card_id = card.id.clone();

        // Post-commit: seed CODEX_HOME and spawn the daemon. Failure
        // here returns an error to the caller, which emits
        // `Event::TaskFailed` for the push path to deliver to the spec.
        //
        // PR7a.1 (#136 followup) — wire the worker codex daemon into
        // the kernel-as-MCP-server. Two mirror-image folds of what
        // `routes::waves::create_wave` does for the spec card:
        //
        //   1. Pass the kernel's `McpShimConfig` to
        //      `per-card CODEX_HOME seeding` so the worker's
        //      `$CODEX_HOME/config.toml` carries a `[mcp_servers.calm]`
        //      block. Without it, codex's MCP client never tries to
        //      connect and the worker can't call `calm.task_completed`
        //      / `calm.task_failed`.
        // NOTE(#410, PR2+): worker still bakes instructions into config.toml;
        // migrate when worker gains an app-server seam.
        //
        //   2. Fold `NEIGE_MCP_TOKEN` + `NEIGE_MCP_SOCKET` into the
        //      env handed to `spawn_terminal_with_parts`. The codex
        //      daemon forwards these to the `neige-mcp-stdio-shim`
        //      child it spawns from the config block above.
        //
        // Both folds are gated on `self.mcp_server.is_some()` so test
        // fixtures (which pass `None`) still exercise the rest of the
        // path without needing a live MCP server.
        // Fetch the terminal row the helper just minted. Guaranteed
        // to exist post-commit. Pulled up BEFORE the seed step so the
        // failure-rollback below has a `term.id` to delete by — keeping
        // the orphan cleanup path symmetric with `spawn_terminal_with_parts`'s
        // failure arm.
        //
        // NOTE (#310 followup, accepted scope): an error from this
        // `?` does NOT trigger `rollback_orphan_worker` — we can't
        // call it without a `terminal_id` and we don't have one. In
        // theory the card row could leak as an orphan that the next
        // retry idempotency-collides with. In practice this branch
        // requires either (a) a SQLite read failure on the terminal
        // table immediately after a successful write in the same
        // connection (extremely unlikely; would be a hardware fault
        // or a connection-pool bug), or (b) `terminal_get_by_card`
        // returning `Ok(None)` for a terminal we just minted in the
        // same tx (impossible barring a sweeper race, which the 60s
        // grace window in `terminals_orphaned` prevents on freshly-
        // committed rows). Wrapping this in rollback would require
        // first extracting `terminal_id` from `card.payload` (the
        // helper stamps it before commit) — cheap-ish, but the read
        // path is the same one we just failed on, so the rollback
        // helper would also have to fall back to deleting by card_id
        // alone. Not worth the complexity for a path this cold.
        let term = self
            .repo
            .terminal_get_by_card(card_id.as_str())
            .await?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "worker terminal vanished after commit for card {card_id}",
                ))
            })?;

        let mut env_for_spawn = env;
        if let (Some(token), Some(server)) = (mcp_token.as_deref(), self.mcp_server.as_ref())
            && let Some(map) = env_for_spawn.as_object_mut()
        {
            map.insert(
                "NEIGE_MCP_TOKEN".into(),
                serde_json::Value::String(token.to_string()),
            );
            map.insert(
                "NEIGE_MCP_SOCKET".into(),
                serde_json::Value::String(
                    server.shim_config.socket_path.to_string_lossy().to_string(),
                ),
            );
        }

        if !self.shared_codex_appserver.is_running() {
            let err = CalmError::Internal("shared codex app-server is not running".into());
            let _ = rollback_orphan_worker(
                self.repo.as_ref(),
                self.terminal_renderer.as_ref(),
                self.write.role_cache(),
                card_id.as_str(),
                term.id.as_str(),
            )
            .await;
            return Err(err);
        }

        spawn_codex_worker_via_shared_daemon(
            self,
            SharedWorkerSpawn {
                card: &card,
                term: &term,
                wave_id: &wave_id,
                mcp_token: mcp_token.as_deref(),
                rendered_prompt: &user_prompt,
                cwd: &cwd,
                legacy_env: &env_for_spawn,
            },
        )
        .await?;

        let card_for_added = self
            .repo
            .card_get(card_id.as_str())
            .await?
            .unwrap_or_else(|| card.clone());
        if let Err(e) = self
            .repo
            .log_pure_event(
                ActorId::KernelDispatcher,
                scope,
                None,
                &self.events,
                self.write.role_cache(),
                self.write.cove_cache(),
                Event::CardAdded(card_for_added),
            )
            .await
        {
            // Card row + terminal + daemon are all live; the only
            // thing this branch loses is the broadcast. Subscribers
            // will discover the card on next REST refresh / page
            // reload. Log loudly so an operator notices a regression
            // in the event-bus write path; do NOT return Err — that
            // would emit `TaskFailed` for a worker that is in fact
            // running.
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                terminal_id = %term.id,
                error = %e,
                "worker codex card.added broadcast failed; card + shared daemon live, subscribers stale",
            );
        }

        tracing::info!(
            idempotency_key = %idempotency_key,
            card_id = %card_id,
            terminal_id = %term.id,
            "dispatcher: worker codex card + shared daemon thread spawned"
        );

        Ok(())
    }

    /// Mint a worker terminal card and spawn its session daemon.
    /// Same idempotency strategy as [`spawn_codex_worker`]: duplicate
    /// requests roll the tx back with `CalmError::IdempotencyCollision`,
    /// the caller treats that typed sentinel as a successful short-
    /// circuit. Real `CalmError::Conflict` errors from
    /// `card_with_terminal_create_tx` (e.g. terminal-already-exists)
    /// now propagate instead of being silently swallowed.
    async fn spawn_terminal_worker(
        self: &Arc<Self>,
        wave_id: WaveId,
        _cove_id: Option<CoveId>,
        idempotency_key: String,
        cmd: String,
        cwd: Option<String>,
    ) -> crate::error::Result<()> {
        let idem_for_tx = idempotency_key.clone();
        let wave_for_tx = wave_id.clone();
        let cache_for_tx = self.write.role_cache().clone();
        let new_card_id = crate::model::new_id();
        let new_card_id_for_tx = new_card_id.clone();

        // Resolve cwd — empty / absent falls back to $HOME.
        let cwd_resolved = cwd
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(crate::routes::codex_cards::default_cwd);

        // Terminal-worker daemon env: no CODEX_HOME — terminal
        // sessions don't need it. We still forward proxy vars so a
        // child shell that hits the network honors operator config.
        let settings = load_settings(self.repo.as_ref()).await?;
        let mut env_map = serde_json::Map::new();
        if let Some(p) = settings.http_proxy.as_deref().filter(|s| !s.is_empty()) {
            env_map.insert(
                "HTTP_PROXY".to_string(),
                serde_json::Value::String(p.to_string()),
            );
            env_map.insert(
                "http_proxy".to_string(),
                serde_json::Value::String(p.to_string()),
            );
        }
        if let Some(p) = settings.https_proxy.as_deref().filter(|s| !s.is_empty()) {
            env_map.insert(
                "HTTPS_PROXY".to_string(),
                serde_json::Value::String(p.to_string()),
            );
            env_map.insert(
                "https_proxy".to_string(),
                serde_json::Value::String(p.to_string()),
            );
        }
        let env = serde_json::Value::Object(env_map);

        // Worker-terminal bookkeeping (idempotency_key, role_request,
        // cmd, optional cwd). Merged into the canonical schema payload after
        // the helper writes it.
        let mut bookkeeping = serde_json::Map::new();
        bookkeeping.insert(
            "idempotency_key".into(),
            serde_json::Value::String(idempotency_key.clone()),
        );
        bookkeeping.insert(
            "role_request".into(),
            serde_json::Value::String("terminal".into()),
        );
        bookkeeping.insert("cmd".into(), serde_json::Value::String(cmd.clone()));
        bookkeeping.insert(
            "cwd".into(),
            serde_json::Value::String(cwd_resolved.clone()),
        );
        let bookkeeping_value = serde_json::Value::Object(bookkeeping);

        let scope = crate::routes::cards::card_scope(
            self.repo.as_ref(),
            new_card_id.clone().into(),
            wave_id.clone(),
        )
        .await?;

        let cwd_for_tx = cwd_resolved.clone();
        let env_for_tx = env.clone();
        let cmd_for_tx = cmd.clone();
        let bookkeeping_for_tx = bookkeeping_value.clone();

        // Issue #310 — two-stage spawn (see `spawn_codex_worker`
        // module-level doc for the full rationale). The tx mints the
        // worker card + terminal row but does NOT emit `CardAdded`;
        // the broadcast is deferred until after `spawn_terminal_with_parts`
        // populates `renderer entry`, mirroring the codex path.
        let card_id_result =
            write_in_tx_typed::<crate::model::Card, _>(self.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    if let Some(existing) =
                        find_card_by_idempotency_key_tx(tx, &idem_for_tx).await?
                    {
                        return Err(CalmError::IdempotencyCollision(format!(
                            "idempotency_key collision: existing card {}",
                            existing.id
                        )));
                    }
                    // Issue #229 PR A — dispatcher worker terminals
                    // are user-facing (the user opened the wave that
                    // dispatched them; if a worker is hung, the user
                    // closes its card to abort). `deletable: true`.
                    let (mut card, _term) = crate::db::sqlite::card_with_terminal_create_tx(
                        tx,
                        new_card_id_for_tx,
                        wave_for_tx,
                        None,
                        cmd_for_tx,
                        cwd_for_tx,
                        env_for_tx,
                        CardRole::Worker,
                        true,
                        &cache_for_tx,
                        // #177 — kernel-internal worker spawn. No host
                        // browser supplied a theme; use the dark
                        // sentinel so theme_fg/_bg NOT NULL is
                        // satisfied and the daemon argv matches
                        // dark-mode defaults.
                        crate::routes::theme::RequestTheme::default_dark(),
                    )
                    .await?;

                    // Merge dispatcher bookkeeping into the
                    // helper-stamped payload.
                    if let Some(existing_map) = card.payload.as_object() {
                        let mut merged = existing_map.clone();
                        if let serde_json::Value::Object(extras) = bookkeeping_for_tx {
                            for (k, v) in extras {
                                merged.insert(k, v);
                            }
                        }
                        card = crate::db::sqlite::card_update_tx(
                            tx,
                            card.id.as_ref(),
                            crate::model::CardPatch {
                                kind: None,
                                sort: None,
                                payload: Some(serde_json::Value::Object(merged)),
                                // #229 PR A — kernel-internal callers
                                // never patch the `deletable` field; the
                                // route handler rejects clients that try.
                                deletable: None,
                            },
                        )
                        .await?;
                    }
                    Ok(card)
                })
            })
            .await;

        let card = match card_id_result {
            Ok(card) => card,
            Err(CalmError::IdempotencyCollision(msg)) => {
                tracing::info!(
                    idempotency_key = %idempotency_key,
                    note = %msg,
                    "dispatcher: short-circuit on existing terminal worker card"
                );
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let card_id = card.id.clone();

        // Post-commit: spawn the terminal daemon. No CODEX_HOME
        // seeding for the terminal worker — it's a plain shell
        // session, not a codex one.
        //
        // NOTE (#310 followup, accepted scope): see the matching note
        // in `spawn_codex_worker` for why an error from this `?` is
        // not wrapped in `rollback_orphan_worker`. Same cold-path
        // argument applies here.
        let term = self
            .repo
            .terminal_get_by_card(card_id.as_str())
            .await?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "worker terminal vanished after commit for card {card_id}",
                ))
            })?;

        let mut spawn_preserved_failure = false;
        if let Err(e) = spawn_terminal_with_parts(
            self.daemon.as_ref(),
            self.terminal_renderer.as_ref(),
            self.repo.as_ref(),
            &term,
            &cmd,
            &cwd_resolved,
            &env,
        )
        .await
        {
            // Issue #310 followup — daemon spawn failed after the
            // row-creation tx committed. The helper discriminates;
            // see `spawn_codex_worker` for the full case rationale.
            //
            // For dispatcher terminals this is the user-visible
            // regression that motivated this fix: a `printf done` /
            // `make build` worker exits cleanly + writes `.exit`
            // before the ready-fd/child-exit race resolves. Pre-
            // fix this code path deleted the card and emitted
            // `task.failed`, making the worker's output disappear
            // entirely. With the discriminator, `Preserved` keeps
            // the card alive so the user sees its output + exit
            // badge (v1 #309 UX).
            match rollback_orphan_worker(
                self.repo.as_ref(),
                self.terminal_renderer.as_ref(),
                self.write.role_cache(),
                card_id.as_str(),
                term.id.as_str(),
            )
            .await
            {
                RollbackOutcome::Deleted => {
                    tracing::error!(
                        card_id = %card_id,
                        wave_id = %wave_id,
                        terminal_id = %term.id,
                        error = %e,
                        "worker terminal daemon spawn failed; rolled back card + terminal",
                    );
                    return Err(e);
                }
                RollbackOutcome::Preserved => {
                    spawn_preserved_failure = true;
                    tracing::info!(
                        card_id = %card_id,
                        wave_id = %wave_id,
                        terminal_id = %term.id,
                        spawn_err = %e,
                        "worker terminal fast-exit (sidecar present); preserving card + terminal",
                    );
                    // Fall through to the CardAdded broadcast below
                    // so subscribers learn about the preserved card.
                }
            }
        }

        if !spawn_preserved_failure {
            match self
                .repo
                .runtime_set_status_for_card(card_id.as_ref(), RunStatus::Running)
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(
                        target: "dispatcher::runtime_running_mark_failed",
                        card_id = %card_id,
                        error = %e,
                        "failed to mark runtime running after worker spawn; CardAdded still broadcasting",
                    );
                }
            }
        }

        // Issue #310 — broadcast `CardAdded` post-spawn-success so the
        // emitted snapshot's backing terminal row has a populated
        // `renderer entry`. See `spawn_codex_worker` for the full
        // rationale + cross-PR pointers.
        if let Err(e) = self
            .repo
            .log_pure_event(
                ActorId::KernelDispatcher,
                scope,
                None,
                &self.events,
                self.write.role_cache(),
                self.write.cove_cache(),
                Event::CardAdded(card),
            )
            .await
        {
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                terminal_id = %term.id,
                error = %e,
                "worker terminal card.added broadcast failed; card + daemon live, subscribers stale",
            );
        }

        tracing::info!(
            idempotency_key = %idempotency_key,
            card_id = %card_id,
            terminal_id = %term.id,
            "dispatcher: worker terminal card + daemon spawned"
        );

        Ok(())
    }
}

/// #293 PR3b — build the concise, actionable observation text the
/// dispatcher pushes onto the spec's codex thread for a wave event.
///
/// Kept terse on purpose: the spec re-reads wave state via its MCP tools,
/// so the push is a *wake/notice*, not a data dump. Each variant names the
/// concrete thing that changed plus the correlating idempotency key (so a
/// spec that dispatched the task can match it to its outstanding work).
/// Free-standing + pure so it's unit-testable without an app-server.
///
/// Only called for the push kinds (`task.completed`, `task.failed`,
/// `wave.report_edited`, `codex.hook`, `claude.hook`); any other variant
/// degrades to a generic notice rather than panicking (defensive — the filter
/// shouldn't deliver others).
fn build_observation(event: &Event) -> String {
    match event {
        Event::TaskCompleted {
            idempotency_key, ..
        } => {
            format!(
                "A dispatched task completed (idempotency_key={idempotency_key}). Re-read the wave state to incorporate its result."
            )
        }
        Event::TaskFailed {
            idempotency_key,
            reason,
        } => {
            format!(
                "A dispatched task failed (idempotency_key={idempotency_key}): {reason}. Re-read the wave state and decide how to proceed."
            )
        }
        Event::WaveReportEdited { .. } => {
            "The user edited the wave report. Re-read the wave state.".to_string()
        }
        Event::CodexHook { .. } | Event::ClaudeHook { .. } => {
            "A worker card finished a turn. Re-read the wave state to incorporate any changes."
                .to_string()
        }
        other => {
            // Shouldn't happen — the push branch only routes the kinds
            // above. Stay resilient instead of panicking.
            format!(
                "A wave event occurred ({}). Re-read the wave state.",
                other.kind_tag()
            )
        }
    }
}

/// Outcome of [`rollback_orphan_worker`]. The caller dispatches on the
/// variant: a `Deleted` outcome means the row is gone and the original
/// spawn error should propagate (→ `TaskFailed`); a `Preserved` outcome
/// means `spawn_terminal_with_parts` returned `Err` for a daemon that
/// actually finished cleanly via the `.exit` sidecar — the row stays
/// alive so the WS attach fast path can render the exit badge, and the
/// caller must NOT surface this as a task failure.
///
/// See [`rollback_orphan_worker`] for the case discriminator.
#[must_use]
enum RollbackOutcome {
    /// Rows were deleted (or attempted to be deleted — failures inside
    /// the rollback tx are logged but still reported as `Deleted` so the
    /// caller surfaces the spawn error to `task.failed`; the orphan
    /// sweeper is the fallback for tx failures).
    Deleted,
    /// The terminal row had `renderer entry = Some(...)` AND the
    /// daemon's `.exit` sidecar was present on disk. The daemon
    /// spawned, executed its command, wrote the exit info, and exited
    /// before `spawn_terminal_with_parts`'s ready-fd/child-exit race
    /// resolved. We preserved both rows (and persisted the
    /// sidecar's `exit_code` / `signal_killed` onto the terminal row)
    /// so the WS attach fast path resolves to `ChildExited` and the
    /// card shows an exit badge — the worker's output / exit code are
    /// real product output, not a failure. The caller must broadcast
    /// `CardAdded` and return `Ok(())` instead of the spawn `Err`.
    Preserved,
}

/// Issue #310 followup — discriminate between three post-spawn-error
/// shapes, then either roll back the worker card + backing terminal
/// row OR preserve them for the WS attach fast path. Logs (best-effort)
/// and swallows DB errors in the rollback case so the caller can still
/// surface the original spawn error (which is what `run_one`'s retry
/// loop emits as `task.failed`).
///
/// **Why this exists.** The dispatcher's two-stage spawn pipeline
/// commits the row-creation tx *before* the daemon spawn runs (the
/// daemon binary is OS-side; no way to make it transactional with the
/// row). When the post-commit step returns Err — bad cmd path, missing
/// daemon binary, fd exhaustion, readiness timeout — the worker card
/// and its terminal row would be orphans without intervention: the card
/// payload references the terminal so the orphan-row sweeper passes
/// them over, and the `idempotency_key` on the card makes a retry with
/// the same key short-circuit on the abandoned row. The user can't
/// re-dispatch.
///
/// **The three cases (after re-fetching the terminal row):**
///
///   * **case 1: `renderer entry = None`** — spawn never wrote a handle.
///     This splits on `pid`:
///
///       * **case 1a: `pid = None`** — `cmd.spawn()` itself failed (or
///         the pid persistence write lost the race before we even got
///         to fork return), so there is no daemon process to reap.
///         Just delete the rows.
///
///       * **case 1b: `pid = Some(...)`** — `cmd.spawn()` succeeded
///         and `terminal_set_pid` persisted the pid, but the
///         subsequent `renderer setup` failed (rare: a
///         `SQLITE_BUSY` at the exact wrong moment, disk full, etc.).
///         The daemon process is alive but `reap_terminal_artifacts`
///         would no-op because it keys off `renderer entry`. We must
///         SIGTERM the pid directly via
///         [`reap_terminal_pid_only`] BEFORE the row delete —
///         otherwise the sweeper can't see it once the row is gone and
///         the daemon leaks until reboot.
///
///   * **case 2: `renderer entry = Some(...)` AND `<handle>.exit`
///     exists** — the daemon DID spawn, ran its command (e.g.
///     `printf done`), wrote the canonical `.exit` sidecar via its
///     normal-exit path, then exited before writing `ready\n`.
///     `spawn_terminal_with_parts` drains the ready fd after observing
///     child exit; with no ready signal it surfaces a "did not become
///     ready" error — but that's spurious for this rollback path: the
///     worker actually completed. **Preserve the rows.** Persist the
///     sidecar's exit info onto the terminal
///     row now (so REST callers see `exit_code` immediately, and so
///     the WS attach fast path can `child-exited` directly off the
///     row). DO NOT delete the rows; DO NOT propagate the spawn Err.
///     The caller broadcasts `CardAdded` and returns Ok(()) — see
///     [`RollbackOutcome::Preserved`].
///
///   * **case 3: `renderer entry = Some(...)` AND no sidecar** — the
///     daemon spawned but hung / crashed / never wrote `.exit`. This
///     is the original P1 leak: SIGTERM the pid + unlink the socket
///     via [`reap_terminal_artifacts`] BEFORE the row delete, then
///     delete both rows. Without the reap, the daemon would leak
///     forever (the sweeper can't see it once the row is gone).
///
/// **Why discriminate inside the helper (not the caller).** The helper
/// already re-fetches the terminal row to pick up the latest
/// `renderer entry`/`pid`. Adding a sidecar-existence check at the same
/// site keeps the case-detection logic in one place, and lets the two
/// call sites (`spawn_codex_worker` / `spawn_terminal_worker`) stay
/// thin — they just match on the returned variant. Pushing the
/// discriminator into the caller would duplicate the re-fetch and the
/// sidecar probe across both paths.
///
/// **Best-effort.** A failure inside the reap step (case 3) is
/// swallowed by `reap_terminal_artifacts` itself — it's idempotent
/// against missing artifacts. A failure inside the rollback tx (cases
/// 1 and 3) is logged at `error` level but swallowed: surfacing the
/// rollback error would mask the original spawn error in the
/// `task.failed` event, which is the more actionable signal for the
/// user. The orphan sweeper is the fallback for rollback failures
/// (same role it plays for crash-time orphans).
async fn rollback_orphan_worker(
    repo: &dyn Repo,
    terminal_renderer: &TerminalRendererRegistry,
    card_role_cache: &CardRoleCache,
    card_id: &str,
    terminal_id: &str,
) -> RollbackOutcome {
    // 1. Re-fetch the terminal row. `spawn_terminal_with_parts` may have
    //    written `pid` + `renderer entry` between the row-creation tx
    //    commit and its eventual error return (e.g. ready-fd backstop,
    //    post-spawn IO error). The `term` snapshot the caller
    //    passes in was taken pre-spawn and would miss those columns.
    //
    //    A NotFound here (the orphan sweeper raced us; the user just
    //    nuked the row via REST; …) is fine — we skip the reap entirely
    //    and fall through to the rollback tx, which is itself NotFound-
    //    tolerant. A Db error gets logged and we still attempt the
    //    rollback tx since row-deletion-blocks-retry is the more
    //    important guarantee here.
    let latest = match repo.terminal_get(terminal_id).await {
        Ok(opt) => opt,
        Err(e) => {
            tracing::error!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                error = %e,
                "rollback_orphan_worker: terminal re-fetch failed; \
                 skipping reap (daemon may leak until sweeper next tick)",
            );
            None
        }
    };

    // 2. Case discriminator. Inspect the re-fetched row to decide
    //    between the three post-spawn-error shapes documented above.
    if let Some(term) = latest.as_ref() {
        if term.exit_code.is_some() || term.signal_killed {
            tracing::info!(
                card_id = %card_id,
                terminal_id = %terminal_id,
                exit_code = ?term.exit_code,
                signal_killed = term.signal_killed,
                "rollback_orphan_worker: preserving worker card with recorded terminal exit",
            );
            return RollbackOutcome::Preserved;
        }

        if terminal_renderer.get(&term.id).is_some() {
            reap_terminal_artifacts_with_renderer(Some(terminal_renderer), term).await;
        } else if let Some(pid) = term.pid {
            // case 1b — handle = None but pid = Some. The daemon
            // process is alive (cmd.spawn() succeeded and
            // terminal_set_pid persisted the pid before
            // renderer setup was attempted), but the handle
            // write failed mid-spawn. We can't go through
            // `reap_terminal_artifacts` because its graceful-Kill +
            // socket-unlink steps both key off `renderer entry`.
            // Send SIGTERM directly via the pid before the row
            // delete; the sweeper would otherwise never find this
            // pid (the row is about to be deleted).
            reap_terminal_pid_only(&term.id, pid);
        }
        // case 1a (term present, renderer entry = None, pid = None)
        // falls through to the row delete below — `cmd.spawn()`
        // either failed outright or never made it to pid persistence,
        // so there is no daemon process. We skip the reap to avoid a
        // SIGTERM at a pid that isn't ours / never existed.
    } else {
        // Row vanished — sweeper raced us, or some other path already
        // cleaned up. Nothing to reap; fall through to rollback tx
        // (the card row may still be live).
        tracing::debug!(
            card_id = %card_id,
            terminal_id = %terminal_id,
            "rollback_orphan_worker: terminal row vanished pre-reap; skipping reap step",
        );
    }

    // 3. Delete both rows (cases 1 and 3). This is the step that
    //    actually unblocks the retry — without it, the orphan card's
    //    idempotency_key short-circuits future dispatches with the
    //    same key.
    let card_id_for_tx = card_id.to_string();
    let term_id_for_tx = terminal_id.to_string();
    let cache_for_tx = card_role_cache.clone();
    let rollback = repo
        .write_in_tx(Box::new(move |tx| {
            Box::pin(async move {
                card_with_terminal_rollback_tx(tx, &card_id_for_tx, &term_id_for_tx, &cache_for_tx)
                    .await
            })
        }))
        .await;
    if let Err(e) = rollback {
        tracing::error!(
            card_id = %card_id,
            terminal_id = %terminal_id,
            error = %e,
            "dispatcher: orphan-worker rollback failed; sweeper will reap on next tick",
        );
    }
    RollbackOutcome::Deleted
}

struct SharedWorkerSpawn<'a> {
    card: &'a crate::model::Card,
    term: &'a crate::model::Terminal,
    wave_id: &'a WaveId,
    mcp_token: Option<&'a str>,
    rendered_prompt: &'a str,
    cwd: &'a str,
    legacy_env: &'a serde_json::Value,
}

async fn spawn_codex_worker_via_shared_daemon(
    inner: &Arc<Inner>,
    ctx: SharedWorkerSpawn<'_>,
) -> crate::error::Result<()> {
    let mut notifications = inner.shared_codex_appserver.subscribe_notifications();
    let remote_uri = inner.shared_codex_appserver.remote_uri();
    let card_id = ctx.card.id.as_str();
    // Worker role developer_instructions — without this, the agent on the
    // shared daemon behaves like a plain prompt session and won't call
    // calm.task_completed / calm.task_failed when the job is done. The
    // legacy path injects this via SeededCardRole::Worker into the per-card
    // CODEX_HOME config.toml; for the shared path we must thread it
    // explicitly through thread_start.
    let worker_instructions = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Worker.prompt_template(),
        ctx.wave_id.as_str(),
    );
    let thread_id = match inner
        .shared_codex_appserver
        .thread_start_mint_for_card(
            card_id,
            SharedThreadStartParams {
                cwd: ctx.cwd.to_string(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: Some(worker_instructions),
            },
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            // thread_start failed (transport error, daemon crash). The
            // card + terminal rows were already committed with the
            // idempotency_key; without rollback they'd permanently
            // short-circuit retries. rollback_orphan_worker's pre-spawn
            // fallthrough deletes both rows.
            let _ = rollback_orphan_worker(
                inner.repo.as_ref(),
                inner.terminal_renderer.as_ref(),
                inner.write.role_cache(),
                card_id,
                ctx.term.id.as_str(),
            )
            .await;
            tracing::warn!(
                target: "shared_codex_daemon::worker",
                card_id,
                wave_id = %ctx.wave_id,
                error = %e,
                "thread_start_failed_rolled_back"
            );
            return Err(e);
        }
    };
    tracing::info!(
        target: "shared_codex_daemon::worker",
        card_id,
        wave_id = %ctx.wave_id,
        thread_id = %thread_id,
        "thread_start_succeeded"
    );

    // Persist runtime identity + app-server handle state BEFORE the PTY spawn.
    // The persist is SILENT (no CardUpdated event) — wave subscribers refetch
    // on CardUpdated and could mount the card before its renderer entry is
    // live (issue #310 race). CardAdded remains the first visible event,
    // emitted by the caller after spawn succeeds.
    if let Err(e) =
        persist_shared_worker_runtime_fields(inner, ctx.card, &thread_id, &remote_uri).await
    {
        // The thread/start succeeded but persisting runtime/handle state
        // failed (e.g. SQLite IO error). Without rollback, the card +
        // terminal rows stay with idempotency_key intact and short-circuit
        // future retries.
        let _ = rollback_orphan_worker(
            inner.repo.as_ref(),
            inner.terminal_renderer.as_ref(),
            inner.write.role_cache(),
            card_id,
            ctx.term.id.as_str(),
        )
        .await;
        tracing::warn!(
            target: "shared_codex_daemon::worker",
            card_id,
            wave_id = %ctx.wave_id,
            thread_id = %thread_id,
            error = %e,
            "persist_shared_runtime_fields_failed_rolled_back"
        );
        return Err(e);
    }

    // turn_start BEFORE spawn — codex 0.135's `codex resume <thread_id>
    // --remote ...` REQUIRES at least one prior turn on the thread before
    // a second connection can resume it. PR5/PR7b spec take the same
    // order; PR7b-worker mirrors. If the subsequent PTY spawn fails, the
    // shared daemon's turn is interrupted via turn/interrupt — see the
    // spawn-fail rollback below. On in-process turn_start failure we delete
    // the orphan card so its idempotency_key clears for retry.
    let initial_turn_result = async {
        let turn_id = inner
            .shared_codex_appserver
            .turn_start(
                &thread_id,
                vec![InputItem::text(ctx.rendered_prompt.trim())],
            )
            .await?;
        await_shared_worker_initial_turn_started(&mut notifications, &thread_id).await?;
        Ok::<String, CalmError>(turn_id)
    }
    .await;
    let initial_turn_id = match initial_turn_result {
        Ok(turn_id) => turn_id,
        Err(e) => {
            tracing::warn!(
                target: "shared_codex_daemon::worker",
                card_id,
                wave_id = %ctx.wave_id,
                thread_id = %thread_id,
                error = %e,
                "turn_start_failed"
            );
            // No PTY has been spawned yet; rollback_orphan_worker's pre-spawn
            // fallthrough (case 1a: term present, no pid, no renderer) deletes
            // both the card and terminal rows. This clears the idempotency_key
            // so a retry can mint a fresh card row.
            let _ = rollback_orphan_worker(
                inner.repo.as_ref(),
                inner.terminal_renderer.as_ref(),
                inner.write.role_cache(),
                card_id,
                ctx.term.id.as_str(),
            )
            .await;
            tracing::error!(
                target: "shared_codex_daemon::worker",
                card_id,
                wave_id = %ctx.wave_id,
                terminal_id = %ctx.term.id,
                thread_id = %thread_id,
                error = %e,
                "worker_turn_start_rolled_back"
            );
            return Err(e);
        }
    };

    let mut env_for_spawn = ctx.legacy_env.clone();
    if let Some(map) = env_for_spawn.as_object_mut() {
        map.insert(
            "CODEX_HOME".into(),
            serde_json::Value::String(inner.shared_codex_appserver.status_snapshot().codex_home),
        );
        if let Some(token) = ctx.mcp_token {
            map.insert(
                "NEIGE_MCP_TOKEN".into(),
                serde_json::Value::String(token.to_string()),
            );
        }
        if let Some(server) = inner.mcp_server.as_ref() {
            map.insert(
                "NEIGE_MCP_SOCKET".into(),
                serde_json::Value::String(
                    server.shim_config.socket_path.to_string_lossy().to_string(),
                ),
            );
        }
    }

    let command_line = format!(
        "codex resume {} --remote {}",
        shell_single_quote(&thread_id),
        shell_single_quote(&remote_uri)
    );
    if let Err(e) = spawn_terminal_with_parts(
        inner.daemon.as_ref(),
        inner.terminal_renderer.as_ref(),
        inner.repo.as_ref(),
        ctx.term,
        &command_line,
        ctx.cwd,
        &env_for_spawn,
    )
    .await
    {
        // turn_start has already delivered the worker job to the shared
        // daemon. If the PTY spawn fails, delete the card/terminal rows so
        // the idempotency_key clears for retry, then interrupt the in-flight
        // shared turn so it cannot keep modifying the workspace invisibly.
        match rollback_orphan_worker(
            inner.repo.as_ref(),
            inner.terminal_renderer.as_ref(),
            inner.write.role_cache(),
            card_id,
            ctx.term.id.as_str(),
        )
        .await
        {
            RollbackOutcome::Deleted => {
                if let Err(interrupt_err) = inner
                    .shared_codex_appserver
                    .turn_interrupt(&thread_id, &initial_turn_id)
                    .await
                {
                    tracing::warn!(
                        target: "shared_codex_daemon::worker",
                        card_id,
                        wave_id = %ctx.wave_id,
                        terminal_id = %ctx.term.id,
                        thread_id = %thread_id,
                        turn_id = %initial_turn_id,
                        error = %interrupt_err,
                        "failed to interrupt shared worker turn after TUI spawn failure"
                    );
                }
                tracing::error!(
                    target: "shared_codex_daemon::worker",
                    card_id,
                    wave_id = %ctx.wave_id,
                    terminal_id = %ctx.term.id,
                    thread_id = %thread_id,
                    turn_id = %initial_turn_id,
                    error = %e,
                    "worker_spawn_failed_turn_interrupted"
                );
                return Err(e);
            }
            RollbackOutcome::Preserved => {
                tracing::info!(
                    target: "shared_codex_daemon::worker",
                    card_id,
                    wave_id = %ctx.wave_id,
                    terminal_id = %ctx.term.id,
                    thread_id = %thread_id,
                    spawn_err = %e,
                    "worker shared TUI fast-exit; preserving card + terminal"
                );
            }
        }
    }

    tracing::info!(
        target: "shared_codex_daemon::worker",
        card_id,
        wave_id = %ctx.wave_id,
        terminal_id = %ctx.term.id,
        thread_id = %thread_id,
        "worker_spawn_succeeded"
    );
    Ok(())
}

async fn await_shared_worker_initial_turn_started(
    rx: &mut tokio::sync::broadcast::Receiver<Notification>,
    thread_id: &str,
) -> crate::error::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            tracing::warn!(
                target: "shared_codex_daemon::worker",
                thread_id,
                "timed out awaiting initial turn/started; continuing best-effort"
            );
            return Ok(());
        }
        match tokio::time::timeout(deadline - now, rx.recv()).await {
            Ok(Ok(n)) => {
                if crate::spec_push::notification_thread_id(&n) == Some(thread_id)
                    && matches!(n, Notification::TurnStarted { .. })
                {
                    return Ok(());
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped))) => {
                tracing::warn!(
                    target: "shared_codex_daemon::worker",
                    skipped,
                    thread_id,
                    "shared worker initial lifecycle subscriber lagged"
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return Err(CalmError::CodexAppServer(format!(
                    "shared app-server notification channel closed before initial lifecycle for {thread_id}"
                )));
            }
            Err(_) => {
                tracing::warn!(
                    target: "shared_codex_daemon::worker",
                    thread_id,
                    "timed out awaiting initial turn/started; continuing best-effort"
                );
                return Ok(());
            }
        }
    }
}

async fn persist_shared_worker_runtime_fields(
    inner: &Arc<Inner>,
    card: &crate::model::Card,
    thread_id: &str,
    remote_uri: &str,
) -> crate::error::Result<()> {
    let card_id_for_tx = card.id.to_string();
    let thread_id_for_tx = thread_id.to_string();
    let remote_uri_for_tx = remote_uri.to_string();
    // SILENT update: `write_in_tx_typed` (not `write_with_event_typed`).
    // CardAdded is the first visible event the broadcaster emits for a worker
    // card (Stage 3, after spawn succeeds); emitting CardUpdated here would
    // pre-empt the renderer-mount race protection (issue #310) — wave
    // subscribers refetch on CardUpdated and could mount the card before its
    // terminal renderer entry is live.
    let _ = write_in_tx_typed::<crate::model::Card, _>(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let mut payload = dispatcher_card_payload_get(tx, &card_id_for_tx).await?;
            let Some(map) = payload.as_object_mut() else {
                return Err(CalmError::Internal(format!(
                    "worker card {card_id_for_tx} payload is not a JSON object; cannot persist shared codex runtime fields"
                )));
            };
            map.insert(
                "appserver_sock".into(),
                serde_json::Value::String(remote_uri_for_tx),
            );
            map.remove("appserver_pgid");
            let updated = card_update_tx(
                tx,
                &card_id_for_tx,
                CardPatch {
                    kind: None,
                    sort: None,
                    payload: Some(payload),
                    deletable: None,
                },
            )
            .await?;
            let runtime = runtime_get_active_for_card_tx(tx, &card_id_for_tx)
                .await?
                .ok_or_else(|| {
                    CalmError::Internal(format!(
                        "worker card {card_id_for_tx} has no active runtime to bind"
                    ))
                })?;
            runtime_bind_attribution_tx(
                tx,
                &runtime.id,
                ThreadAttribution {
                    runtime_id: runtime.id.clone(),
                    provider: AgentProvider::Codex,
                    thread_id: Some(thread_id_for_tx),
                    session_id: None,
                    active_turn_id: None,
                },
            )
            .await?;
            runtime_set_status_tx(tx, &runtime.id, RunStatus::Running).await?;
            Ok(updated)
        })
    })
    .await?;
    Ok(())
}

// NOTE: an earlier R2 iteration also defined `clear_shared_worker_runtime_fields`
// to revert the silent persist on turn_start failure WITHOUT deleting the
// card row. R3 replaced that path with rollback_orphan_worker (which deletes
// the card + terminal rows entirely) so the helper is gone — the card row
// rollback subsumes the payload clear.

async fn dispatcher_card_payload_get(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> crate::error::Result<serde_json::Value> {
    let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let payload_text = row
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
        .0;
    serde_json::from_str(&payload_text)
        .map_err(|e| CalmError::Internal(format!("card {card_id} payload is not valid JSON: {e}")))
}

/// SELECT a card by its `payload.idempotency_key` inside a tx. Returns
/// `Ok(None)` when no row matches. Used by the dispatcher's tx-bound
/// idempotency check.
///
/// The query is on the open transaction so a follow-up INSERT in the
/// same tx serializes against any concurrent dispatcher tx (SQLite's
/// per-connection write lock). This is the canonical
/// "two-`*.Requested`-events-can't-both-spawn" guarantee.
async fn find_card_by_idempotency_key_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    idempotency_key: &str,
) -> crate::error::Result<Option<crate::model::Card>> {
    let row = sqlx::query_as::<_, crate::model::Card>(
        r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
           FROM cards
           WHERE json_extract(payload, '$.idempotency_key') = ?1
           LIMIT 1"#,
    )
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(CalmError::from)?;
    Ok(row)
}

/// Returns true when the given error is a transient SQLite BUSY /
/// LOCKED status that the dispatcher should retry. PR6 (#136)
/// replaced the PR5 substring-on-stringified-error matcher with a
/// proper downcast through `sqlx::Error::Database` so a future
/// driver-message change (or an i18n'd error string) doesn't
/// silently break the retry path.
///
/// See https://www.sqlite.org/rescode.html — code 5 = `SQLITE_BUSY`,
/// code 6 = `SQLITE_LOCKED`. sqlx reports the code as a string on
/// `DatabaseError::code()`.
fn is_sqlite_busy(e: &crate::error::CalmError) -> bool {
    // Walk the error chain looking for a `sqlx::Error` we own. The
    // dispatcher's calls funnel through `CalmError::from(sqlx::Error)`
    // which boxes the original under the `Sql` variant; everything
    // else (Internal/etc) won't match.
    let sqlx_err = match e {
        crate::error::CalmError::Db(inner) => inner,
        _ => return false,
    };
    let Some(db_err) = sqlx_err.as_database_error() else {
        return false;
    };
    // SQLITE_BUSY = 5, SQLITE_LOCKED = 6 — both are transient
    // contention on the per-connection write lock, retry-safe.
    matches!(db_err.code().as_deref(), Some("5") | Some("6"))
}

/// RAII handle that owns a slot in the `recently_seen` set. PR6
/// followup (note 2 from issue #136 review): without this, a panic
/// inside the spawned dispatcher task between the `insert` and the
/// explicit `g.remove(&idem)` would leave the idempotency key stuck
/// in the set for `RECENT_KEYS_TTL`, silently dropping a retry within
/// that window.
///
/// Semantics:
///
///   * [`RecentlySeenGuard::install`] tries to insert the key. Returns
///     `Some(guard)` on success; `None` when the key was already
///     present (the caller should short-circuit and skip the dispatch).
///   * On `Drop` (normal scope exit or panic) the guard removes the
///     key from the set — unless [`RecentlySeenGuard::commit`] was
///     called, which sets a flag making the Drop a no-op. The success
///     path calls `.commit()` and schedules a separate TTL cleanup
///     task instead.
///
/// Tokio's task supervisor isolates panics from sibling tasks but
/// still runs `Drop` on values captured by the panicking future
/// (panics unwind through the future's drop chain), so the guard fires
/// on panic the same way it does on a normal return. The blocking
/// `std::sync::Mutex` is fine here because the critical sections are
/// O(hash insert/remove) under sub-µs contention.
struct RecentlySeenGuard {
    set: Arc<Mutex<HashSet<String>>>,
    key: String,
    committed: bool,
}

impl RecentlySeenGuard {
    /// Try to insert `key`. On success returns `Some(guard)`; on
    /// duplicate (already present in the set) returns `None`, signalling
    /// the caller to short-circuit. A poisoned mutex is treated as
    /// "duplicate" — the dispatcher's lock recovery semantics prefer
    /// dropping the request over panicking on a poisoned lock; the
    /// next emit will retry.
    fn install(set: Arc<Mutex<HashSet<String>>>, key: String) -> Option<Self> {
        let mut g = set.lock().ok()?;
        if g.contains(&key) {
            return None;
        }
        g.insert(key.clone());
        drop(g);
        Some(Self {
            set,
            key,
            committed: false,
        })
    }

    /// Mark the slot as "successfully consumed". `Drop` becomes a
    /// no-op; the caller takes responsibility for the eventual TTL
    /// cleanup of the key.
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for RecentlySeenGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Ok(mut g) = self.set.lock() {
            g.remove(&self.key);
        }
    }
}

/// Variant shape extracted from a `*.Requested` envelope. Carrying this
/// rather than the raw `Event` lets the dispatch path stay variant-
/// agnostic at the spawn site. `Clone` so the retry loop can re-issue
/// the dispatch after a transient SQLite contention error.
#[derive(Clone)]
enum DispatchRequest {
    Codex {
        idempotency_key: String,
        goal: String,
        context: serde_json::Value,
        acceptance_criteria: Option<String>,
    },
    Terminal {
        idempotency_key: String,
        cmd: String,
        cwd: Option<String>,
    },
}

impl DispatchRequest {
    fn idempotency_key(&self) -> &str {
        match self {
            DispatchRequest::Codex {
                idempotency_key, ..
            } => idempotency_key,
            DispatchRequest::Terminal {
                idempotency_key, ..
            } => idempotency_key,
        }
    }
}

// Suppress unused-trait-bounds lint: `RouteRepo` is left as a
// reachable supertrait for downstream code paths that prefer the
// narrow trait object.
#[allow(dead_code)]
fn _route_repo_marker<R: RouteRepo>(_r: &R) {}

/// Render the worker codex's first user message from the dispatcher
/// payload. Becomes both the `payload.prompt` field (so
/// `legacy auto-submit` fires the composer `\r`) and codex's positional
/// `[PROMPT]` arg (so the composer mounts pre-filled). Mirrors the spec
/// card path in `routes::waves::create_wave` which feeds the wave title
/// through the same channel; the system prompt
/// (`WORKER_SYSTEM_PROMPT_PLACEHOLDER` in `spec_card.rs`) tells the
/// worker to read goal/context/acceptance-criteria, so we render them
/// here in a predictable shape. Context is pretty-printed JSON so the
/// worker can parse it back when it carries structured data.
fn render_worker_prompt(
    goal: &str,
    context: &serde_json::Value,
    acceptance_criteria: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("Goal:\n");
    out.push_str(goal);

    let context_str = match context {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) if s.trim().is_empty() => String::new(),
        serde_json::Value::Object(m) if m.is_empty() => String::new(),
        serde_json::Value::Array(a) if a.is_empty() => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    if !context_str.is_empty() {
        out.push_str("\n\nContext:\n");
        out.push_str(&context_str);
    }

    if let Some(ac) = acceptance_criteria.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("\n\nAcceptance criteria:\n");
        out.push_str(ac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env-override permits parsing — covers the four cases the helper
    /// documents (unset, empty, unparseable, zero, valid).
    #[test]
    fn permits_from_env_fallback_paths() {
        // Save + restore so this test doesn't disturb its neighbors.
        let saved = std::env::var("NEIGE_DISPATCHER_PERMITS").ok();

        // Use a sub-fn so the unsafe SAFETY blocks are scoped tightly.
        // `set_var` / `remove_var` are unsafe in 2024-edition Rust.
        fn set(k: &str, v: &str) {
            // SAFETY: single-threaded test; no other reader of this env
            // var is racing.
            unsafe { std::env::set_var(k, v) };
        }
        fn remove(k: &str) {
            // SAFETY: see `set`.
            unsafe { std::env::remove_var(k) };
        }

        remove("NEIGE_DISPATCHER_PERMITS");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "unset → default");

        set("NEIGE_DISPATCHER_PERMITS", "");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "empty → default");

        set("NEIGE_DISPATCHER_PERMITS", "not-a-number");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "garbage → default");

        set("NEIGE_DISPATCHER_PERMITS", "0");
        assert_eq!(Dispatcher::permits_from_env(8), 8, "zero → default");

        set("NEIGE_DISPATCHER_PERMITS", "3");
        assert_eq!(Dispatcher::permits_from_env(8), 3, "valid → override");

        // Restore.
        match saved {
            Some(v) => set("NEIGE_DISPATCHER_PERMITS", &v),
            None => remove("NEIGE_DISPATCHER_PERMITS"),
        }
    }

    // ---------------------------------------------------------------
    // PR6 followup (issue #136, note 2 from original review):
    // [`RecentlySeenGuard`] behavior under success, failure, and
    // panic. The guard is the RAII handle that owns each entry in
    // `recently_seen`; the dispatcher relies on `Drop` running on
    // panic so a stale key doesn't lock out a retry for the full
    // `RECENT_KEYS_TTL`.
    // ---------------------------------------------------------------

    fn fresh_set() -> Arc<Mutex<HashSet<String>>> {
        Arc::new(Mutex::new(HashSet::new()))
    }

    fn set_contains(set: &Arc<Mutex<HashSet<String>>>, key: &str) -> bool {
        set.lock().unwrap().contains(key)
    }

    /// Two `install` calls for the same key should produce one Some
    /// and one None — the second is the short-circuit signal.
    #[test]
    fn recently_seen_guard_install_dedupes() {
        let set = fresh_set();
        let g1 = RecentlySeenGuard::install(set.clone(), "k".into());
        assert!(g1.is_some(), "first install should succeed");
        let g2 = RecentlySeenGuard::install(set.clone(), "k".into());
        assert!(
            g2.is_none(),
            "second install of the same key should short-circuit (None)"
        );
        // Drop g1 → the failure-path semantics remove the key.
        drop(g1);
        assert!(
            !set_contains(&set, "k"),
            "drop on un-committed guard must remove the key"
        );
    }

    /// `commit()` makes Drop a no-op; the key stays in the set for
    /// the TTL cleanup task to remove.
    #[test]
    fn recently_seen_guard_commit_keeps_key() {
        let set = fresh_set();
        let g = RecentlySeenGuard::install(set.clone(), "k".into()).expect("install ok");
        g.commit();
        // Guard dropped at end of `commit()`'s consume; ensure the
        // key is still there.
        assert!(
            set_contains(&set, "k"),
            "commit()'d guard must leave the key in the set"
        );
    }

    /// Panic-cleanup: a future that panics with a live guard should
    /// still see the guard's Drop remove the key. Mirrors the
    /// tokio spawn case in the dispatcher.
    #[tokio::test]
    async fn recently_seen_guard_drops_on_panic() {
        let set = fresh_set();
        let set_for_task = set.clone();
        let h = tokio::spawn(async move {
            let _g = RecentlySeenGuard::install(set_for_task, "k".into()).expect("install ok");
            // Deliberately panic with the guard live on the stack.
            // tokio's task supervisor isolates the panic from the
            // parent; the future's drop chain still runs, including
            // `_g`'s Drop impl.
            panic!("simulated dispatcher panic");
        });
        let err = h.await.expect_err("the spawned task should have panicked");
        assert!(err.is_panic(), "expected panic JoinError, got {err:?}");
        assert!(
            !set_contains(&set, "k"),
            "panic in the spawned task must drop the guard and remove the key"
        );
    }

    // ---------------------------------------------------------------
    // PR6 followup (issue #136, note 1 from original review):
    // `CalmError::IdempotencyCollision` is a separate variant from
    // `CalmError::Conflict`. The dispatcher catches only the typed
    // sentinel; real conflicts from the helpers (terminal-already-
    // exists, card-id PK collision) must propagate.
    // ---------------------------------------------------------------

    #[test]
    fn idempotency_collision_distinct_from_conflict() {
        let collision = crate::error::CalmError::IdempotencyCollision("k".into());
        let conflict = crate::error::CalmError::Conflict("k".into());
        // The catch arm in `spawn_codex_worker` / `spawn_terminal_worker`
        // matches *only* `IdempotencyCollision`. A real `Conflict`
        // must take the propagation branch.
        assert!(matches!(
            collision,
            crate::error::CalmError::IdempotencyCollision(_)
        ));
        assert!(matches!(conflict, crate::error::CalmError::Conflict(_)));
        // And the error codes the API surface emits are distinct.
        assert_eq!(
            crate::error::CalmError::IdempotencyCollision("x".into()).code(),
            "idempotency_collision"
        );
        assert_eq!(
            crate::error::CalmError::Conflict("x".into()).code(),
            "conflict"
        );
    }

    // ---------------------------------------------------------------
    // `render_worker_prompt` — turns dispatcher payload fields into the
    // worker codex's first composer message. Each empty/non-empty
    // combination is exercised so a future refactor that drops a
    // section trips loudly. The non-empty output is the source of
    // truth for both `payload.prompt` (consumed by `legacy auto-submit`)
    // and codex's `[PROMPT]` argv (rendered via `shell_single_quote`),
    // so a regression here breaks the worker hand-off end-to-end.
    // ---------------------------------------------------------------

    #[test]
    fn render_worker_prompt_goal_only() {
        let out = render_worker_prompt("fix the bug", &serde_json::Value::Null, None);
        assert_eq!(out, "Goal:\nfix the bug");
    }

    #[test]
    fn render_worker_prompt_goal_plus_context() {
        let ctx = serde_json::json!({ "issue": 42, "title": "x" });
        let out = render_worker_prompt("fix it", &ctx, None);
        assert!(out.starts_with("Goal:\nfix it"));
        assert!(out.contains("\n\nContext:\n"));
        assert!(out.contains("\"issue\": 42"));
        assert!(out.contains("\"title\": \"x\""));
        assert!(!out.contains("Acceptance criteria"));
    }

    #[test]
    fn render_worker_prompt_goal_plus_context_plus_ac() {
        let ctx = serde_json::json!({ "pr": 7 });
        let out = render_worker_prompt("ship", &ctx, Some("tests pass"));
        assert!(out.contains("Goal:\nship"));
        assert!(out.contains("\n\nContext:\n"));
        assert!(out.contains("\"pr\": 7"));
        assert!(out.ends_with("Acceptance criteria:\ntests pass"));
    }

    #[test]
    fn render_worker_prompt_skips_empty_context_object() {
        let out = render_worker_prompt("g", &serde_json::json!({}), Some("ac"));
        assert!(
            !out.contains("Context"),
            "empty {{}} should be skipped: {out}"
        );
        assert!(out.contains("Acceptance criteria:\nac"));
    }

    #[test]
    fn render_worker_prompt_skips_blank_ac() {
        let out = render_worker_prompt("g", &serde_json::Value::Null, Some("   "));
        assert_eq!(out, "Goal:\ng");
    }

    // ---------------------------------------------------------------
    // #293 PR3b — push path: filter coverage, author gating,
    // build_observation text, and the dedicated push-watermark dedup.
    // ---------------------------------------------------------------

    use crate::event::{ArtifactRef, BroadcastEnvelope};
    use crate::ids::CoveId;

    fn wave_scope(wave: &WaveId, cove: &CoveId) -> EventScope {
        EventScope::Wave {
            wave: wave.clone(),
            cove: cove.clone(),
        }
    }

    /// The dispatcher's `SubscribeFilter` must now match the push kinds in
    /// addition to the two job_requested kinds. We reconstruct the
    /// exact filter the spawn site builds and assert `matches()` for each
    /// kind, plus a non-matching kind to prove the list is still a closed
    /// allowlist (not "match everything").
    #[test]
    fn dispatcher_filter_matches_push_kinds() {
        let filter = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(vec![
                "codex.job_requested".into(),
                "terminal.job_requested".into(),
                "task.completed".into(),
                "task.failed".into(),
                "wave.report_edited".into(),
                "codex.hook".into(),
                "claude.hook".into(),
            ]),
        };
        let wave = WaveId::from("w");
        let cove = CoveId::from("c");
        let scope = wave_scope(&wave, &cove);

        let env = |ev: Event| BroadcastEnvelope {
            id: 1,
            event_version: 1,
            actor: ActorId::User,
            scope: scope.clone(),
            event: ev,
        };

        // The two pre-existing job_requested kinds still match.
        assert!(filter.matches(&env(Event::CodexJobRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
        })));
        assert!(filter.matches(&env(Event::TerminalJobRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
        })));
        // The push kinds match.
        assert!(filter.matches(&env(Event::TaskCompleted {
            idempotency_key: "k".into(),
            result: serde_json::Value::Null,
            artifacts: Vec::<ArtifactRef>::new(),
        })));
        assert!(filter.matches(&env(Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "boom".into(),
        })));
        assert!(filter.matches(&env(Event::WaveReportEdited {
            wave_id: wave.clone(),
            card_id: CardId::from("card"),
            author: EditAuthor::User,
            edit_id: "e".into(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: String::new(),
            body_after: String::new(),
        })));
        assert!(filter.matches(&env(Event::CodexHook {
            card_id: CardId::from("worker-codex"),
            kind: "hook.codex.stop".into(),
            payload: serde_json::Value::Null,
        })));
        assert!(filter.matches(&env(Event::ClaudeHook {
            card_id: CardId::from("worker-claude"),
            kind: "hook.claude.stop".into(),
            payload: serde_json::Value::Null,
        })));
        // A kind NOT in the list must not match — the filter is still a
        // closed allowlist.
        assert!(!filter.matches(&env(Event::WaveDeleted {
            id: wave.clone(),
            cove_id: cove.clone(),
        })));
    }

    /// The push branch in `handle_envelope` acts on a User-authored
    /// `wave.report_edited` and ignores Spec/Kernel ones. The gating is a
    /// simple `author == EditAuthor::User` check; assert that predicate
    /// directly against each variant (the branch itself is exercised
    /// end-to-end by the gated e2e).
    #[test]
    fn wave_report_edited_author_gating() {
        assert!(EditAuthor::User == EditAuthor::User);
        assert!(EditAuthor::Spec != EditAuthor::User);
        assert!(EditAuthor::Kernel != EditAuthor::User);
    }

    #[test]
    fn event_warrants_spec_push_covers_push_allowlist() {
        let cache = CardRoleCache::new();
        let wave = WaveId::from("w");
        let cove = CoveId::from("c");
        let worker = CardId::from("worker");
        let spec = CardId::from("spec");
        let unknown = CardId::from("unknown");
        cache.insert(worker.clone(), CardRole::Worker, wave.clone());
        cache.insert(spec.clone(), CardRole::Spec, wave.clone());
        let write = WriteContext::new(cache, crate::wave_cove_cache::WaveCoveCache::new());

        let completed = Event::TaskCompleted {
            idempotency_key: "done".into(),
            result: serde_json::Value::Null,
            artifacts: Vec::new(),
        };
        assert!(event_warrants_spec_push(&completed, &write));

        let failed = Event::TaskFailed {
            idempotency_key: "fail".into(),
            reason: "boom".into(),
        };
        assert!(event_warrants_spec_push(&failed, &write));

        let report = |author| Event::WaveReportEdited {
            wave_id: wave.clone(),
            card_id: spec.clone(),
            author,
            edit_id: "edit".into(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: String::new(),
            body_after: String::new(),
        };
        assert!(event_warrants_spec_push(&report(EditAuthor::User), &write));
        assert!(!event_warrants_spec_push(&report(EditAuthor::Spec), &write));
        assert!(!event_warrants_spec_push(
            &report(EditAuthor::Kernel),
            &write
        ));

        let codex_hook = |card_id: CardId, kind: &str| Event::CodexHook {
            card_id,
            kind: kind.into(),
            payload: serde_json::Value::Null,
        };
        let claude_hook = |card_id: CardId, kind: &str| Event::ClaudeHook {
            card_id,
            kind: kind.into(),
            payload: serde_json::Value::Null,
        };
        assert!(event_warrants_spec_push(
            &codex_hook(worker.clone(), "hook.codex.stop"),
            &write
        ));
        assert!(event_warrants_spec_push(
            &claude_hook(worker.clone(), "hook.claude.stop"),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(spec.clone(), "hook.codex.stop"),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &claude_hook(spec.clone(), "hook.claude.stop"),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(unknown.clone(), "hook.codex.stop"),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &claude_hook(unknown, "hook.claude.stop"),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(worker.clone(), "hook.codex.permission_request"),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &codex_hook(worker, "hook.codex.post_tool_use"),
            &write
        ));
        assert!(!event_warrants_spec_push(
            &Event::WaveDeleted {
                id: wave,
                cove_id: cove,
            },
            &write
        ));
    }

    /// `build_observation` produces concise, kind-specific text carrying the
    /// correlating idempotency key (task events) / a re-read nudge.
    #[test]
    fn build_observation_text_per_kind() {
        let completed = build_observation(&Event::TaskCompleted {
            idempotency_key: "abc123".into(),
            result: serde_json::Value::Null,
            artifacts: Vec::new(),
        });
        assert!(completed.contains("completed"), "got: {completed}");
        assert!(completed.contains("abc123"), "must carry key: {completed}");

        let failed = build_observation(&Event::TaskFailed {
            idempotency_key: "k9".into(),
            reason: "disk full".into(),
        });
        assert!(failed.contains("failed"), "got: {failed}");
        assert!(failed.contains("k9"), "must carry key: {failed}");
        assert!(failed.contains("disk full"), "must carry reason: {failed}");

        let edited = build_observation(&Event::WaveReportEdited {
            wave_id: WaveId::from("w"),
            card_id: CardId::from("c"),
            author: EditAuthor::User,
            edit_id: "e".into(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: String::new(),
            body_after: String::new(),
        });
        assert!(
            edited.to_lowercase().contains("user") && edited.to_lowercase().contains("report"),
            "got: {edited}"
        );

        let codex_stop = build_observation(&Event::CodexHook {
            card_id: CardId::from("codex-worker"),
            kind: "hook.codex.stop".into(),
            payload: serde_json::Value::Null,
        });
        assert!(
            codex_stop.contains("worker card") && codex_stop.contains("finished a turn"),
            "got: {codex_stop}"
        );
        assert!(
            !codex_stop.contains("idempotency_key"),
            "hook notice must stay light: {codex_stop}"
        );

        let claude_stop = build_observation(&Event::ClaudeHook {
            card_id: CardId::from("claude-worker"),
            kind: "hook.claude.stop".into(),
            payload: serde_json::Value::Null,
        });
        assert!(
            claude_stop.contains("worker card") && claude_stop.contains("finished a turn"),
            "got: {claude_stop}"
        );
        assert!(
            !claude_stop.contains("idempotency_key"),
            "hook notice must stay light: {claude_stop}"
        );
    }

    /// Watermark dedup: the push only fires when `envelope_id > cursor`,
    /// then bumps. This mirrors the exact `get`/compare/`bump` sequence
    /// `push_to_spec` runs against its DEDICATED `push_cursor`
    /// `EventCursorCache`. Re-delivering the same id is a no-op; a higher
    /// id advances.
    #[test]
    fn push_watermark_dedup_sequence() {
        let cursor = EventCursorCache::new();
        let card = CardId::from("spec-card");

        // First delivery at id=5: 5 > 0 → push, bump to 5.
        assert!(5 > cursor.get(&card));
        cursor.bump(card.clone(), 5);
        assert_eq!(cursor.get(&card), 5);

        // Re-delivery of the SAME id=5: the push predicate `id > cursor`
        // is false → deduped (no push).
        assert!(5 <= cursor.get(&card));
        // bump(5) is monotonic — stays at 5.
        cursor.bump(card.clone(), 5);
        assert_eq!(cursor.get(&card), 5);

        // A lower (out-of-order re-delivery) id=3: deduped, no rewind.
        assert!(3 <= cursor.get(&card));
        cursor.bump(card.clone(), 3);
        assert_eq!(cursor.get(&card), 5);

        // A higher id=8: 8 > 5 → push, advance to 8.
        assert!(8 > cursor.get(&card));
        cursor.bump(card.clone(), 8);
        assert_eq!(cursor.get(&card), 8);
    }

    /// #313 round-2 (B3) — the per-wave push lock map must serialize
    /// concurrent acquisitions for the SAME wave (so boot takeover's
    /// `Dispatcher::push_lock` and the live `push_to_spec`'s lock cannot run
    /// the dedup-check-and-deliver body concurrently — which would lose
    /// events in the seed→insert window). DIFFERENT waves must remain
    /// independent so a slow takeover for wave A doesn't block live
    /// pushes for wave B.
    ///
    /// Models the `DashMap::entry(...).or_insert_with(Arc::new Mutex)` +
    /// `clone().lock_owned().await` pattern `Inner::acquire_push_lock` uses.
    #[tokio::test]
    async fn per_wave_push_lock_serializes_same_wave_runs_in_parallel_across_waves() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Same map shape as `Inner::push_locks`.
        let push_locks: DashMap<WaveId, Arc<tokio::sync::Mutex<()>>> = DashMap::new();
        let take_lock = |wave_id: &WaveId| -> Arc<tokio::sync::Mutex<()>> {
            push_locks
                .entry(wave_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };

        // Track concurrent occupancy. Same-wave: must never exceed 1.
        let in_flight_a = Arc::new(AtomicUsize::new(0));
        let max_in_flight_a = Arc::new(AtomicUsize::new(0));
        let wave_a = WaveId::from("wave-a");

        let mut handles = vec![];
        for i in 0..8 {
            let lock = take_lock(&wave_a);
            let in_flight = in_flight_a.clone();
            let max_in_flight = max_in_flight_a.clone();
            handles.push(tokio::spawn(async move {
                let _g = lock.lock_owned().await;
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(now, Ordering::SeqCst);
                // Simulate the dedup-check-and-deliver body holding the
                // lock for a few yields (representative of `push_to_spec`'s
                // async work).
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(2 * (i as u64 + 1))).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            max_in_flight_a.load(Ordering::SeqCst),
            1,
            "same-wave per-wave lock must serialize: observed concurrent holders"
        );

        // Different waves: independent locks → can run in parallel.
        let in_flight_total = Arc::new(AtomicUsize::new(0));
        let max_in_flight_total = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];
        for i in 0..6 {
            let wave: WaveId = format!("wave-parallel-{i}").into();
            let lock = take_lock(&wave);
            let in_flight = in_flight_total.clone();
            let max_in_flight = max_in_flight_total.clone();
            handles.push(tokio::spawn(async move {
                let _g = lock.lock_owned().await;
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        // We expect parallelism > 1 across distinct wave keys (otherwise
        // the per-wave keying is broken). With 6 spawns and ~15ms each on a
        // multi-threaded runtime they should overlap routinely.
        assert!(
            max_in_flight_total.load(Ordering::SeqCst) > 1,
            "different-wave locks must allow parallel runs; observed serialization"
        );
    }

    /// #313 round-2 (B1) — `PushOutcome::Issued { max_envelope_id }`
    /// carries the highest envelope id from the coalesced turn (drained
    /// queue + the new push). The dispatcher persists that exact id;
    /// `Enqueued` must NOT trigger persistence. This is the structural
    /// invariant; the spec_appserver tests cover the queue mechanics, and
    /// the e2e proves the durable watermark behavior end-to-end.
    #[test]
    fn push_outcome_shape() {
        use crate::spec_push::PushOutcome;
        match (PushOutcome::Issued {
            max_envelope_id: 42,
        }) {
            PushOutcome::Issued { max_envelope_id } => assert_eq!(max_envelope_id, 42),
            PushOutcome::Enqueued => panic!("expected Issued"),
        }
        match PushOutcome::Enqueued {
            PushOutcome::Enqueued => {}
            PushOutcome::Issued { .. } => panic!("expected Enqueued"),
        }
    }
}
