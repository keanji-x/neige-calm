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
use crate::db::write_with_event_typed;
use crate::db::{Repo, RouteRepo};
use crate::error::CalmError;
use crate::event::{
    BroadcastEnvelope, Event, EventBus, EventScope, SubscribeFilter, SubscribeScope,
};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::{CardRole, NewCard};
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
    #[allow(dead_code)]
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
        {
            let mut g = self.recently_seen.lock().await;
            if g.contains(&idem) {
                tracing::debug!(idempotency_key = %idem, "dispatcher: recently-seen, skipping");
                return;
            }
            g.insert(idem.clone());
        }
        // Schedule cleanup of this key once the TTL elapses. Bounded
        // task — sleep + remove + exit — so the spawn list can't grow
        // without limit.
        {
            let key_for_cleanup = idem.clone();
            let inner = Arc::clone(&self);
            tokio::spawn(async move {
                tokio::time::sleep(RECENT_KEYS_TTL).await;
                let mut g = inner.recently_seen.lock().await;
                g.remove(&key_for_cleanup);
            });
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
        if let Some(e) = last_err {
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

    /// Mint a worker codex card and (best-effort) spawn the codex
    /// daemon. Idempotency: SELECT for an existing
    /// `cards.payload.idempotency_key` inside the tx; if a row already
    /// exists, return its id without minting a duplicate.
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
        // pattern. Drop into a closure so we can capture by move into
        // the tx body without re-fetching.
        let new_card_id = crate::model::new_id();
        let new_card_id_for_tx = new_card_id.clone();

        // Build the worker-card payload. The dispatcher writes the
        // payload directly (not via `card_with_codex_create_tx`)
        // because a worker card is NOT a user-facing codex card with
        // a backing terminal yet — the terminal/daemon spawn happens
        // out of band after commit, mirroring the
        // `create_codex_card` shape. For PR5 we keep the payload
        // minimal: `idempotency_key`, `goal`, `context`,
        // `acceptance_criteria`, and a `role_request = "codex"`
        // discriminator so the FSM / UI can distinguish worker codex
        // cards from plain ones in PR6+.
        let mut payload_map = serde_json::Map::new();
        payload_map.insert(
            "idempotency_key".into(),
            serde_json::Value::String(idempotency_key.clone()),
        );
        payload_map.insert(
            "role_request".into(),
            serde_json::Value::String("codex".into()),
        );
        payload_map.insert("goal".into(), serde_json::Value::String(goal.clone()));
        payload_map.insert("context".into(), context.clone());
        if let Some(ac) = acceptance_criteria.as_ref() {
            payload_map.insert(
                "acceptance_criteria".into(),
                serde_json::Value::String(ac.clone()),
            );
        }
        let payload = serde_json::Value::Object(payload_map);

        let scope = crate::routes::cards::card_scope(
            repo_for_scope.as_ref(),
            new_card_id.clone().into(),
            wave_id.clone(),
        )
        .await?;

        // Idempotency check + worker-card insert in one tx. The
        // closure-returned Option distinguishes "minted new" vs
        // "already existed".
        let payload_for_tx = payload.clone();
        let (existing_or_new, _event_id) =
            write_with_event_typed::<(CardId, /*minted_new=*/ bool), _>(
                self.repo.as_ref(),
                ActorId::KernelDispatcher,
                scope.clone(),
                None,
                &self.events,
                &self.card_role_cache,
                move |tx| {
                    Box::pin(async move {
                        // SELECT-inside-tx idempotency check.
                        if let Some(existing) =
                            find_card_by_idempotency_key_tx(tx, &idem_for_tx).await?
                        {
                            // Existing row — return a synthetic `CardUpdated`
                            // event so the wrapper has *something* to persist
                            // (audit trail: the dispatcher saw a duplicate
                            // request and short-circuited). The frontend
                            // tolerates a repeat `card.updated` for the same
                            // row without payload changes; this is a
                            // pragmatic alternative to letting the helper
                            // refuse to commit without an event. We pass
                            // through `existing.clone()` so the emit is a
                            // no-op delta — same payload, same
                            // `updated_at` after the helper restamps it.
                            let bumped = crate::db::sqlite::card_update_tx(
                                tx,
                                existing.id.as_ref(),
                                crate::model::CardPatch {
                                    kind: None,
                                    sort: None,
                                    payload: Some(existing.payload.clone()),
                                },
                            )
                            .await?;
                            let id = bumped.id.clone();
                            return Ok(((id, false), Event::CardUpdated(bumped)));
                        }

                        // No existing row — mint a fresh worker card.
                        let card = crate::db::sqlite::card_create_with_id_tx(
                            tx,
                            new_card_id_for_tx,
                            NewCard {
                                wave_id: wave_for_tx,
                                kind: "codex".into(),
                                sort: None,
                                payload: payload_for_tx,
                            },
                            CardRole::Worker,
                            &cache_for_tx,
                        )
                        .await?;
                        let id = card.id.clone();
                        Ok(((id, true), Event::CardAdded(card)))
                    })
                },
            )
            .await?;

        let (card_id, minted_new) = existing_or_new;
        if !minted_new {
            tracing::info!(
                idempotency_key = %idempotency_key,
                card_id = %card_id,
                "dispatcher: short-circuit on existing worker card"
            );
            return Ok(());
        }

        // PR5 keeps daemon spawn shallow: we don't actually fire codex
        // because (a) no emitter exists yet for a real load test, and
        // (b) the codex CLI is heavy and PR6/PR7 will wire the
        // user-driven spawn that includes setting up CODEX_HOME, etc.
        // We touch `self.codex` to keep the field "used" and to give a
        // future PR a single seam to start running the real spawn.
        tracing::info!(
            idempotency_key = %idempotency_key,
            card_id = %card_id,
            codex_bin = %self.codex.codex_bin,
            "dispatcher: worker codex card minted (daemon spawn deferred to PR6+)"
        );

        Ok(())
    }

    /// Mint a worker terminal card. Same idempotency strategy as
    /// `spawn_codex_worker`. PR5 keeps the terminal spawn deferred —
    /// the card lands, PR6+ wires the actual `spawn_daemon_for` call.
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

        let mut payload_map = serde_json::Map::new();
        payload_map.insert(
            "idempotency_key".into(),
            serde_json::Value::String(idempotency_key.clone()),
        );
        payload_map.insert(
            "role_request".into(),
            serde_json::Value::String("terminal".into()),
        );
        payload_map.insert("cmd".into(), serde_json::Value::String(cmd.clone()));
        if let Some(c) = cwd.as_ref() {
            payload_map.insert("cwd".into(), serde_json::Value::String(c.clone()));
        }
        let payload = serde_json::Value::Object(payload_map);

        let scope = crate::routes::cards::card_scope(
            self.repo.as_ref(),
            new_card_id.clone().into(),
            wave_id.clone(),
        )
        .await?;

        let payload_for_tx = payload.clone();
        let (existing_or_new, _event_id) =
            write_with_event_typed::<(CardId, /*minted_new=*/ bool), _>(
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
                            let bumped = crate::db::sqlite::card_update_tx(
                                tx,
                                existing.id.as_ref(),
                                crate::model::CardPatch {
                                    kind: None,
                                    sort: None,
                                    payload: Some(existing.payload.clone()),
                                },
                            )
                            .await?;
                            let id = bumped.id.clone();
                            return Ok(((id, false), Event::CardUpdated(bumped)));
                        }
                        let card = crate::db::sqlite::card_create_with_id_tx(
                            tx,
                            new_card_id_for_tx,
                            NewCard {
                                wave_id: wave_for_tx,
                                kind: "terminal".into(),
                                sort: None,
                                payload: payload_for_tx,
                            },
                            CardRole::Worker,
                            &cache_for_tx,
                        )
                        .await?;
                        let id = card.id.clone();
                        Ok(((id, true), Event::CardAdded(card)))
                    })
                },
            )
            .await?;

        let (card_id, minted_new) = existing_or_new;
        if !minted_new {
            tracing::info!(
                idempotency_key = %idempotency_key,
                card_id = %card_id,
                "dispatcher: short-circuit on existing terminal worker card"
            );
            return Ok(());
        }
        tracing::info!(
            idempotency_key = %idempotency_key,
            card_id = %card_id,
            "dispatcher: worker terminal card minted (daemon spawn deferred to PR6+)"
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
/// LOCKED status that the dispatcher should retry. sqlx surfaces these
/// as a `Database(_)` error whose string contains "database is locked"
/// or "deadlocked"; we match on substrings rather than re-typing
/// against `sqlx::error::DatabaseError` because the latter requires
/// down-casting through an `Any` boundary.
fn is_sqlite_busy(e: &crate::error::CalmError) -> bool {
    let s = e.to_string();
    s.contains("database is locked")
        || s.contains("database is deadlocked")
        || s.contains("(code: 5)")
        || s.contains("(code: 6)")
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
