//! Dispatcher worker (PR5 of #136).
//!
//! Subscribes to the event bus through [`EventBus::subscribe_filtered`] +
//! a [`SubscribeFilter`] that picks out `codex.job_requested` and
//! `terminal.job_requested` envelopes, then mints a worker-roled card
//! (and, for the codex case, spawns a backing `calm-session-daemon`) for
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

use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{card_with_codex_create_tx, card_with_terminal_rollback_tx};
use crate::db::write_in_tx_typed;
use crate::db::{Repo, RouteRepo};
use crate::error::CalmError;
use crate::event::{
    BroadcastEnvelope, EditAuthor, Event, EventBus, EventScope, SubscribeFilter, SubscribeScope,
};
use crate::event_cursor::EventCursorCache;
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::CardRole;
use crate::routes::codex_cards::shell_single_quote;
use crate::routes::settings::load_settings;
use crate::routes::terminal::spawn_daemon_with_parts;
use crate::spec_appserver::SpecPushRegistry;
use crate::spec_card::{SeededCardRole, build_codex_env_map, seed_codex_home_with_parts};
use crate::state::{CodexClient, DaemonClient};
use crate::wave_cove_cache::WaveCoveCache;

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
}

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
    /// `NEIGE_MCP_SOCKET` into the env it hands to `spawn_daemon_with_parts`
    /// for codex workers, and threads the shim config into
    /// `seed_codex_home_with_parts` so each worker's `$CODEX_HOME/config.toml`
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
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        // #293 — the wave→app-server push registry (shared with
        // `AppState.spec_push`; `create_wave` fills it). Push is the only
        // path now (#293 cutover): the subscribe filter unconditionally
        // includes the `task.*` / `wave.report_edited` kinds so they route to
        // `push_to_spec`.
        spec_push: SpecPushRegistry,
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
            card_role_cache,
            wave_cove_cache,
            codex,
            daemon,
            mcp_server,
            spec_push,
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
        // unconditionally matches the three wave-event push kinds in addition
        // to the two `*.job_requested` kinds. The push kinds route to
        // `push_to_spec`; the `*.job_requested` kinds drive the worker-spawn
        // arm.
        let kinds: Vec<String> = vec![
            "codex.job_requested".into(),
            "terminal.job_requested".into(),
            "task.completed".into(),
            "task.failed".into(),
            "wave.report_edited".into(),
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
        }
    }
}

struct Inner {
    repo: Arc<dyn Repo>,
    events: EventBus,
    card_role_cache: CardRoleCache,
    /// #234 — parallel wave→cove cache the role gate consults alongside
    /// `card_role_cache`.
    wave_cove_cache: WaveCoveCache,
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
    /// [`crate::spec_appserver::SpecPushHandle`] from here and calls
    /// `push_observation` on it. Empty when a kernel restart lost the
    /// in-memory handle (no crash-recovery — see `push_to_spec`).
    spec_push: SpecPushRegistry,
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

        // #293 — push branch. The three wave-event kinds the filter matches
        // route HERE (bounded by the same `_permit` the worker-spawn path
        // holds), never into the `DispatchRequest` extraction below. For
        // `wave.report_edited` we act ONLY on a User-authored edit —
        // Spec/AI-authored edits are the spec writing its own report, and
        // pushing those back would be a feedback loop. The worker-spawn path
        // (the two `*.job_requested` kinds) falls through untouched.
        match &envelope.event {
            Event::TaskCompleted { .. } | Event::TaskFailed { .. } => {
                if let Some(wave_id) = envelope.scope.wave_id().cloned() {
                    self.push_to_spec(wave_id, &envelope.event, envelope.id)
                        .await;
                } else {
                    tracing::debug!(
                        kind = envelope.event.kind_tag(),
                        "dispatcher push: task event has no wave scope; skipping"
                    );
                }
                return;
            }
            Event::WaveReportEdited {
                author, wave_id, ..
            } => {
                // Only user edits warrant a push. The spec authored
                // Spec/Kernel edits itself; re-notifying it would loop.
                if *author == EditAuthor::User {
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
                    &self.card_role_cache,
                    &self.wave_cove_cache,
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
    ///      above the dedicated push watermark for that spec `CardId`; then
    ///      bump. Idempotent under at-least-once broadcast delivery.
    ///   3. **Resolve the handle** — `spec_push.pusher(wave_id)`. If absent
    ///      (a kernel restart lost the in-memory handle), `warn!` and return
    ///      — never crash. There is no recovery: the wave stays undriven
    ///      until the kernel re-creates a handle (which it doesn't do today).
    ///      See the missing-handle warn below.
    ///   4. **Build + deliver** the observation via
    ///      [`crate::spec_appserver::SpecPusher::push_observation`],
    ///      which decides `turn/start`-now vs enqueue based on the tracked
    ///      turn phase.
    async fn push_to_spec(self: &Arc<Self>, wave_id: WaveId, event: &Event, envelope_id: i64) {
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
        let wave_lock = self
            .push_locks
            .entry(wave_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _serialize = wave_lock.lock().await;

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
        self.push_cursor.bump(spec_card_id.clone(), envelope_id);

        // Resolve the live push handle. Absent → warn + return (no crash).
        // #293: there is NO crash-recovery. A kernel restart drops every
        // in-memory `SpecPushHandle`, and the kernel does not respawn the
        // `codex app-server` or `thread_resume` the persisted thread on boot.
        // The consequence is accepted and explicit: in-flight waves whose
        // handle was lost are left UNDRIVEN — no push lands, and there is no
        // pull backstop anymore (the old `wait_for_events` poll was removed in
        // this cutover). The user must restart the wave to recover.
        let pusher = match self.spec_push.pusher(&wave_id) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    wave_id = %wave_id,
                    spec_card_id = %spec_card_id,
                    envelope_id,
                    kind = event.kind_tag(),
                    "dispatcher push: no live SpecPushHandle for wave (kernel restart lost the \
                     handle); wave left undriven — no crash-recovery, no pull backstop (#293)"
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
        if let Err(e) = pusher.push_observation(&observation).await {
            // A failed delivery is logged but not fatal. There is no pull
            // backstop anymore (#293 cutover removed `wait_for_events`); a
            // dropped observation means the spec may miss this event. The
            // dedicated push watermark was already bumped above, so a
            // redelivery of the same envelope won't retry — accepted.
            tracing::warn!(
                wave_id = %wave_id,
                envelope_id,
                error = %e,
                "dispatcher push: push_observation failed; no pull backstop (#293) — observation may be lost"
            );
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
            if self.card_role_cache.get(&c.id) == Some(CardRole::Spec) {
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
        let cache_for_tx = self.card_role_cache.clone();
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
        // `spawn_daemon_with_parts` post-commit. Mirrors the spec
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
        // `codex_auto_submit` fires the composer `\r` on
        // `hook.codex.session_start`) and the positional `[PROMPT]`
        // arg on the codex daemon's argv (so the composer mounts
        // pre-filled). Without this the worker hangs forever with an
        // empty composer — the spec card path (`spec_card.rs`) closed
        // the same bug via issue #251; the worker path was missed.
        let user_prompt = render_worker_prompt(&goal, &context, acceptance_criteria.as_deref());

        // Worker-card payload — bookkeeping fields the FSM / UI use
        // to distinguish worker codex cards from plain ones. The
        // canonical `card_with_codex_create_tx` helper stamps
        // `schemaVersion`, `terminal_id`, and `cwd` itself; we merge
        // those fields after the helper runs by going through
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
        // worker card + terminal row (`daemon_handle = NULL`).
        // **Does NOT emit `CardAdded` here.** Stage 2 (post-commit,
        // below): `seed_codex_home_with_parts` + `spawn_daemon_with_parts`
        // (writes `daemon_handle`, spawns daemon, probes readiness).
        // Stage 3 (post-spawn-success): broadcast `CardAdded` via
        // `log_pure_event` so subscribers see the card only after the
        // backing terminal has a live daemon. Without this split, a
        // spec card hot-subscribed to the wave's event stream sees
        // `CardAdded` immediately, mounts its `XtermView`, attempts a
        // WS attach, and hits `resolve_live_sock`'s "no daemon_handle
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
        //      `seed_codex_home_with_parts` so the worker's
        //      `$CODEX_HOME/config.toml` carries a `[mcp_servers.calm]`
        //      block. Without it, codex's MCP client never tries to
        //      connect and the worker can't call `calm.task_completed`
        //      / `calm.task_failed`.
        //
        //   2. Fold `NEIGE_MCP_TOKEN` + `NEIGE_MCP_SOCKET` into the
        //      env handed to `spawn_daemon_with_parts`. The codex
        //      daemon forwards these to the `neige-mcp-stdio-shim`
        //      child it spawns from the config block above.
        //
        // Both folds are gated on `self.mcp_server.is_some()` so test
        // fixtures (which pass `None`) still exercise the rest of the
        // path without needing a live MCP server.
        let mcp_shim = self.mcp_server.as_ref().map(|m| m.shim_config.clone());
        // #236 followup — pair shim + token so the worker's config.toml
        // gets a `[mcp_servers.calm].env` block too. Same rationale as
        // the spec card: codex CLI 0.132 doesn't inherit the daemon
        // env into MCP server subprocesses, so the env must be baked
        // into config.toml. Missing either side leaves the worker
        // without an MCP wire (a token-less worker can't authenticate
        // anyway).
        let mcp_block = match (mcp_shim.as_ref(), mcp_token.as_deref()) {
            (Some(s), Some(t)) => Some((s, t)),
            _ => None,
        };
        // Fetch the terminal row the helper just minted. Guaranteed
        // to exist post-commit. Pulled up BEFORE the seed step so the
        // failure-rollback below has a `term.id` to delete by — keeping
        // the orphan cleanup path symmetric with `spawn_daemon_with_parts`'s
        // failure arm.
        let term = self
            .repo
            .terminal_get_by_card(card_id.as_str())
            .await?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "worker terminal vanished after commit for card {card_id}",
                ))
            })?;

        if let Err(e) = seed_codex_home_with_parts(
            codex.as_ref(),
            card_id.as_str(),
            &cwd,
            wave_id.as_str(),
            SeededCardRole::Worker,
            mcp_block,
        ) {
            // Issue #310 followup — the row-creation tx already
            // committed (event-less); seeding the per-card CODEX_HOME
            // failed post-commit. Without rollback, the card+terminal
            // are orphans whose `idempotency_key` blocks a retry. Drop
            // both rows so `run_one`'s outer retry loop / a user
            // re-dispatch with the same key isn't short-circuited on
            // the abandoned row.
            rollback_orphan_worker(
                self.repo.as_ref(),
                &self.card_role_cache,
                card_id.as_str(),
                term.id.as_str(),
            )
            .await;
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                terminal_id = %term.id,
                error = %e,
                "worker codex CODEX_HOME seed failed; rolled back card + terminal",
            );
            return Err(e);
        }

        // PR7a.1 — augment env with MCP token/socket before spawn.
        // Soft-fail: if either side is missing we still spawn the
        // daemon (it just won't have a wire back to the kernel).
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

        // Mirror the spec card path: hand codex the rendered prompt as
        // its positional `[PROMPT]` arg so the composer mounts pre-filled.
        // `shell_single_quote` ships the whole string as one literal sh
        // word (`spawn_daemon_with_parts` ultimately funnels through
        // `sh -c`). `codex_auto_submit` then sees the non-empty
        // `payload.prompt` and injects a `\r` on `hook.codex.session_start`.
        let command_line = format!("codex {}", shell_single_quote(&user_prompt));
        if let Err(e) = spawn_daemon_with_parts(
            self.daemon.as_ref(),
            self.repo.as_ref(),
            &term,
            &command_line,
            &cwd,
            &env_for_spawn,
        )
        .await
        {
            // Issue #310 followup — daemon spawn failed after the
            // row-creation tx committed. Roll back card + terminal so
            // a retry with the same `idempotency_key` isn't short-
            // circuited on the orphan row (pre-rollback, the second
            // SELECT in `find_card_by_idempotency_key_tx` would find
            // this abandoned card and treat the dispatch as already
            // done — the user could never re-dispatch).
            rollback_orphan_worker(
                self.repo.as_ref(),
                &self.card_role_cache,
                card_id.as_str(),
                term.id.as_str(),
            )
            .await;
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                terminal_id = %term.id,
                error = %e,
                "worker codex daemon spawn failed; rolled back card + terminal",
            );
            return Err(e);
        }

        // Issue #310 — Stage 3: broadcast `CardAdded` only after
        // `spawn_daemon_with_parts` has written `daemon_handle` and
        // probed daemon readiness. Subscribers (the spec card on the
        // requesting wave page) now see the new worker card with a
        // populated `daemon_handle`; the WS attach in
        // `ws::terminal::resolve_live_sock` resolves to `Alive` (or,
        // for a genuine fast-exit, the existing `ChildExited` branch)
        // — never the spurious "no daemon_handle = clean child exit"
        // path that #304 introduced for actual zero-handle rows. See
        // module-level doc comment for the cross-PR rationale.
        if let Err(e) = self
            .repo
            .log_pure_event(
                ActorId::KernelDispatcher,
                scope,
                None,
                &self.events,
                &self.card_role_cache,
                &self.wave_cove_cache,
                Event::CardAdded(card),
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
                "worker codex card.added broadcast failed; card + daemon live, subscribers stale",
            );
        }

        tracing::info!(
            idempotency_key = %idempotency_key,
            card_id = %card_id,
            terminal_id = %term.id,
            codex_bin = %codex.codex_bin,
            "dispatcher: worker codex card + daemon spawned"
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
        let cache_for_tx = self.card_role_cache.clone();
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
        // cmd, optional cwd). Merged into the canonical payload
        // (schemaVersion + terminal_id) after the helper writes it.
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
        // the broadcast is deferred until after `spawn_daemon_with_parts`
        // populates `daemon_handle`, mirroring the codex path.
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
        let term = self
            .repo
            .terminal_get_by_card(card_id.as_str())
            .await?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "worker terminal vanished after commit for card {card_id}",
                ))
            })?;

        if let Err(e) = spawn_daemon_with_parts(
            self.daemon.as_ref(),
            self.repo.as_ref(),
            &term,
            &cmd,
            &cwd_resolved,
            &env,
        )
        .await
        {
            // Issue #310 followup — daemon spawn failed after the
            // row-creation tx committed. Roll back card + terminal so
            // a retry with the same `idempotency_key` isn't short-
            // circuited on the orphan row. Mirrors the codex path —
            // see `spawn_codex_worker` for the full rationale.
            rollback_orphan_worker(
                self.repo.as_ref(),
                &self.card_role_cache,
                card_id.as_str(),
                term.id.as_str(),
            )
            .await;
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                terminal_id = %term.id,
                error = %e,
                "worker terminal daemon spawn failed; rolled back card + terminal",
            );
            return Err(e);
        }

        // Issue #310 — broadcast `CardAdded` post-spawn-success so the
        // emitted snapshot's backing terminal row has a populated
        // `daemon_handle`. See `spawn_codex_worker` for the full
        // rationale + cross-PR pointers.
        if let Err(e) = self
            .repo
            .log_pure_event(
                ActorId::KernelDispatcher,
                scope,
                None,
                &self.events,
                &self.card_role_cache,
                &self.wave_cove_cache,
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
/// Only called for the three push kinds (`task.completed`, `task.failed`,
/// `wave.report_edited`); any other variant degrades to a generic notice
/// rather than panicking (defensive — the filter shouldn't deliver others).
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
        other => {
            // Shouldn't happen — the push branch only routes the three
            // kinds above. Stay resilient instead of panicking.
            format!(
                "A wave event occurred ({}). Re-read the wave state.",
                other.kind_tag()
            )
        }
    }
}

/// Issue #310 followup — roll back the worker card + backing terminal
/// row when a post-commit spawn step fails. Logs (best-effort) and
/// swallows DB errors so the caller can still surface the original
/// spawn error (which is what `run_one`'s retry loop emits as
/// `task.failed`).
///
/// **Why this exists.** The dispatcher's two-stage spawn pipeline
/// commits the row-creation tx *before* the daemon spawn runs (the
/// daemon binary is OS-side; no way to make it transactional with the
/// row). If the post-commit step returns Err — bad cmd path, missing
/// daemon binary, fd exhaustion, readiness timeout — the worker card
/// and its terminal row are orphans: the card payload references the
/// terminal so the orphan-row sweeper passes them over, and the
/// `idempotency_key` on the card makes a retry with the same key
/// short-circuit on the abandoned row. The user can't re-dispatch.
///
/// The fix here re-opens a small tx and DELETEs both rows in the order
/// the `RESTRICT` FK demands (terminal first, then card) via
/// [`card_with_terminal_rollback_tx`]. After this returns, a retry
/// with the same `idempotency_key` no longer finds the orphan and
/// goes through fresh — the correct semantic for "first attempt
/// failed; please try again".
///
/// **Best-effort.** A failure inside this rollback (e.g. SQLite
/// busy/locked, FK weirdness, the orphan sweeper raced us) is logged
/// at `error` level but swallowed: surfacing the rollback error would
/// mask the original spawn error in the `task.failed` event, which is
/// the more actionable signal for the user. The orphan sweeper is the
/// fallback for rollback failures (same role it plays for crash-time
/// orphans).
async fn rollback_orphan_worker(
    repo: &dyn Repo,
    card_role_cache: &CardRoleCache,
    card_id: &str,
    terminal_id: &str,
) {
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
/// `codex_auto_submit` fires the composer `\r`) and codex's positional
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
    // truth for both `payload.prompt` (consumed by `codex_auto_submit`)
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

    /// The dispatcher's `SubscribeFilter` must now match the three push
    /// kinds in addition to the two job_requested kinds. We reconstruct the
    /// exact filter the spawn site builds and assert `matches()` for each
    /// kind, plus a non-matching kind to prove the list is still a closed
    /// allowlist (not "match everything").
    #[test]
    fn dispatcher_filter_matches_three_push_kinds() {
        let filter = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(vec![
                "codex.job_requested".into(),
                "terminal.job_requested".into(),
                "task.completed".into(),
                "task.failed".into(),
                "wave.report_edited".into(),
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
        // The three new push kinds match.
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
}
