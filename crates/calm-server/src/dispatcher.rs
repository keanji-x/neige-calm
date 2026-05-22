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
//! [`ActorId::KernelDispatcher`] actor. PR8's `wait_for_events` consumes
//! these on behalf of the requesting spec card.
//!
//! ## What this doesn't do
//!
//! - **No spec card minting** — PR6 lands the spec card; PR5 just
//!   responds to whoever emits a `*.Requested` event.
//! - **No `wait_for_events`** — PR8 builds the long-poll that pairs
//!   each `TaskCompleted` / `TaskFailed` back to its spec card.
//! - **No glob kinds** — the dispatcher's filter lists the two literal
//!   kind tags. A future glob extension would update both the filter
//!   and this module's subscribe call together.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::card_with_codex_create_tx;
use crate::db::write_with_event_typed;
use crate::db::{Repo, RouteRepo};
use crate::error::CalmError;
use crate::event::{
    BroadcastEnvelope, Event, EventBus, EventScope, SubscribeFilter, SubscribeScope,
};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::CardRole;
use crate::routes::settings::load_settings;
use crate::routes::terminal::spawn_daemon_with_parts;
use crate::spec_card::{build_codex_env_map, seed_codex_home_with_parts};
use crate::state::{CodexClient, DaemonClient};

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
    pub fn spawn(
        repo: Arc<dyn Repo>,
        events: EventBus,
        card_role_cache: CardRoleCache,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        permits: usize,
    ) -> Self {
        let permits = if permits == 0 {
            DEFAULT_PERMITS
        } else {
            permits
        };
        let semaphore = Arc::new(Semaphore::new(permits));
        let inner = Arc::new(Inner {
            repo,
            events: events.clone(),
            card_role_cache,
            codex,
            daemon,
            semaphore: Arc::clone(&semaphore),
            recently_seen: Mutex::new(HashSet::new()),
        });

        // Filter: every event of either `*.Requested` kind, anywhere in
        // the cove→wave→card tree. The dispatcher's job is to react to
        // emissions from any spec card regardless of scope — narrower
        // routing happens after the SELECT-inside-tx idempotency check
        // (the worker card lands in the same wave as the requesting
        // spec card).
        let filter = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(vec![
                "codex.job_requested".into(),
                "terminal.job_requested".into(),
            ]),
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
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    semaphore: Arc<Semaphore>,
    /// Recently-spawned idempotency keys. A fast-path short-circuit
    /// before the tx-bound SELECT. Held under a `Mutex` rather than a
    /// concurrent set because the operations are short (insert / remove
    /// / contains under sub-microsecond hold time) and contention is
    /// bounded by the semaphore. A scheduled cleanup tokio task purges
    /// entries older than [`RECENT_KEYS_TTL`].
    recently_seen: Mutex<HashSet<String>>,
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
        // tx), but remove on every failure path so a transient
        // spawn error doesn't permanently lock out a retry. On
        // success the key stays for `RECENT_KEYS_TTL` and a
        // bounded cleanup task removes it.
        {
            let mut g = self.recently_seen.lock().await;
            if g.contains(&idem) {
                tracing::debug!(idempotency_key = %idem, "dispatcher: recently-seen, skipping");
                return;
            }
            g.insert(idem.clone());
        }

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
            match self.dispatch(req.clone(), scope.clone()).await {
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
            // Success path: keep the idempotency key in
            // `recently_seen` for `RECENT_KEYS_TTL` so a re-emit of
            // the same envelope within the grace window is short-
            // circuited at the in-process layer. The canonical guard
            // is still the SELECT-inside-tx, but the fast-path saves
            // the round-trip when the request is genuinely a
            // duplicate.
            let key_for_cleanup = idem.clone();
            let inner = Arc::clone(&self);
            tokio::spawn(async move {
                tokio::time::sleep(RECENT_KEYS_TTL).await;
                let mut g = inner.recently_seen.lock().await;
                g.remove(&key_for_cleanup);
            });
        }
        if let Some(e) = last_err {
            // Failure path: drop the key from `recently_seen` so the
            // request can be retried after the requester sees the
            // task.failed event. The canonical SELECT-inside-tx guard
            // still prevents a double-spawn if the retry races a late
            // re-emit of the original event.
            {
                let mut g = self.recently_seen.lock().await;
                g.remove(&idem);
            }
            tracing::warn!(
                idempotency_key = %idem,
                error = %e,
                "dispatcher: spawn failed; emitting task.failed"
            );
            // Emit a TaskFailed so the requesting spec card's
            // wait_for_events (PR8) surfaces the failure. Scope mirrors
            // the request envelope's scope so PR8 can route on it.
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

    async fn dispatch(
        self: &Arc<Self>,
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
    /// returns `Err(CalmError::Conflict)` to abort the tx (no rows
    /// written, no events emitted). The caller pattern-matches the
    /// Conflict and treats it as a success short-circuit. This
    /// replaces the PR5 no-op `CardUpdated` workaround: a duplicate
    /// request now produces zero events instead of a spurious one.
    async fn spawn_codex_worker(
        self: &Arc<Self>,
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
        let env = build_codex_env_map(
            self.codex.as_ref(),
            &new_card_id,
            settings.http_proxy.as_deref(),
            settings.https_proxy.as_deref(),
        );
        let cwd = crate::routes::codex_cards::default_cwd();

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

        // Single tx: idempotency check + worker card + terminal row +
        // bookkeeping merge + CardAdded event. Closure returns
        // `Conflict` on duplicate, which rolls everything back
        // (matches the issue v2 invariant: no spurious event when the
        // request is a duplicate).
        let card_id_result = write_with_event_typed::<CardId, _>(
            self.repo.as_ref(),
            ActorId::KernelDispatcher,
            scope.clone(),
            None,
            &self.events,
            &self.card_role_cache,
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
                        // returning Conflict. The caller below
                        // pattern-matches the variant and treats
                        // it as a success short-circuit. No
                        // event reaches the bus.
                        return Err(CalmError::Conflict(format!(
                            "idempotency_key collision: existing card {}",
                            existing.id
                        )));
                    }

                    // Mint worker card + backing terminal +
                    // canonical codex payload (schemaVersion,
                    // terminal_id, cwd) in one helper call.
                    let (mut card, _term) = card_with_codex_create_tx(
                        tx,
                        new_card_id_for_tx,
                        wave_for_tx,
                        None,
                        cwd_for_tx,
                        env_for_tx,
                        None,
                        CardRole::Worker,
                        &cache_for_tx,
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
                            },
                        )
                        .await?;
                    }

                    let id = card.id.clone();
                    Ok((id, Event::CardAdded(card)))
                })
            },
        )
        .await;

        let card_id = match card_id_result {
            Ok((id, _event_id)) => id,
            Err(CalmError::Conflict(msg)) => {
                tracing::info!(
                    idempotency_key = %idempotency_key,
                    note = %msg,
                    "dispatcher: short-circuit on existing worker card"
                );
                return Ok(());
            }
            Err(e) => return Err(e),
        };

        // Post-commit: seed CODEX_HOME and spawn the daemon. Failure
        // here returns an error to the caller, which emits
        // `Event::TaskFailed` for PR8's `wait_for_events` to surface.
        if let Err(e) = seed_codex_home_with_parts(
            self.codex.as_ref(),
            card_id.as_str(),
            &cwd,
            wave_id.as_str(),
            CardRole::Worker,
        ) {
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                error = %e,
                "worker codex CODEX_HOME seed failed; card persisted; sweeper will reap terminal",
            );
            return Err(e);
        }

        // Fetch the terminal row the helper just minted. Guaranteed
        // to exist post-commit.
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
            "codex",
            &cwd,
            &env,
        )
        .await
        {
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                terminal_id = %term.id,
                error = %e,
                "worker codex daemon spawn failed; card + terminal orphaned for sweeper",
            );
            return Err(e);
        }

        tracing::info!(
            idempotency_key = %idempotency_key,
            card_id = %card_id,
            terminal_id = %term.id,
            codex_bin = %self.codex.codex_bin,
            "dispatcher: worker codex card + daemon spawned"
        );

        Ok(())
    }

    /// Mint a worker terminal card and spawn its session daemon.
    /// Same idempotency strategy as [`spawn_codex_worker`]: duplicate
    /// requests roll the tx back with `CalmError::Conflict`, the
    /// caller treats Conflict as a successful short-circuit.
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

        let card_id_result = write_with_event_typed::<CardId, _>(
            self.repo.as_ref(),
            ActorId::KernelDispatcher,
            scope.clone(),
            None,
            &self.events,
            &self.card_role_cache,
            move |tx| {
                Box::pin(async move {
                    if let Some(existing) =
                        find_card_by_idempotency_key_tx(tx, &idem_for_tx).await?
                    {
                        return Err(CalmError::Conflict(format!(
                            "idempotency_key collision: existing card {}",
                            existing.id
                        )));
                    }
                    let (mut card, _term) = crate::db::sqlite::card_with_terminal_create_tx(
                        tx,
                        new_card_id_for_tx,
                        wave_for_tx,
                        None,
                        cmd_for_tx,
                        cwd_for_tx,
                        env_for_tx,
                        CardRole::Worker,
                        &cache_for_tx,
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
                            },
                        )
                        .await?;
                    }
                    let id = card.id.clone();
                    Ok((id, Event::CardAdded(card)))
                })
            },
        )
        .await;

        let card_id = match card_id_result {
            Ok((id, _event_id)) => id,
            Err(CalmError::Conflict(msg)) => {
                tracing::info!(
                    idempotency_key = %idempotency_key,
                    note = %msg,
                    "dispatcher: short-circuit on existing terminal worker card"
                );
                return Ok(());
            }
            Err(e) => return Err(e),
        };

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
            tracing::error!(
                card_id = %card_id,
                wave_id = %wave_id,
                terminal_id = %term.id,
                error = %e,
                "worker terminal daemon spawn failed; card + terminal orphaned for sweeper",
            );
            return Err(e);
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
        r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
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
}
