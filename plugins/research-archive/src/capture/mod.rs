//! `research-archive capture`: one start of the capture job (#2575,
//! "Model: Capture job" and "Run phases").
//!
//! The job owns the whole lifecycle of a run; the timer only starts it. A
//! start does exactly this, in order, on the injected [`Clock`]:
//!
//! 1. take the data-dir lock; if it is held, exit `BUSY` having opened
//!    nothing (the environment guard and the config ran before, in the CLI);
//! 2. open the Store and end every dangling run (`window_end ≤ now`) as
//!    `partial`;
//! 3. load the calendar cache, refreshing it first when it is a day old
//!    (which builds the `OAuth` client);
//! 4. find the window open now. With none, mark the missed days and exit
//!    `NO_WINDOW`;
//! 5. plan the run (an existing plan wins); exit at once if it has ended;
//! 6. build the `OAuth` client and the source, then run the phases
//!    ([`runner`]).
//!
//! The process deadline is `min(window_end, start + attempt_timeout)`. The
//! exit state goes to `last_exit.json` ([`crate::state`]) before the lock is
//! released, on every ending after the lock: a token that would need the
//! SDK's browser flow is an ordinary `AUTH_UNAVAILABLE` return
//! ([`auth::build_oauth`] stops the flow), not a process exit.

mod latch;
mod runner;
mod schedule;

pub use schedule::{COUNTED, MAX_IN_FLIGHT, backoff, counts_toward_cap};

use std::{fmt::Display, sync::Arc, time::Duration};

use longbridge::oauth::OAuth;

use crate::auth::{self, DataDirTokenStorage};
use crate::calendar::TradingCalendar;
use crate::calendar::fetch::{QuoteDays, RefreshError, TradingDays};
use crate::clock::{self, Clock};
use crate::config::Config;
use crate::lock::{DataDirLock, LockError};
use crate::notify;
use crate::source::{LongbridgeSource, Timeouts};
use crate::state::{Dangling, ExitKind, Invocation, LastExit, Outcome};
use crate::store::PlanRequest;
use crate::types::{AsOf, Source, UnixMs};
use crate::{Store, StoreError};

/// The longest building the `OAuth` client may take in a start (a token
/// refresh included), within the start's deadline: the source's own bound on
/// a token resolution ([`Timeouts::PRODUCTION`]`.token`).
pub const AUTH_BOUND: Duration = Timeouts::PRODUCTION.token;

/// A `Fatal` outcome, logging `what` failed and why. `err` is never an SDK
/// error: those are reduced to a category first.
pub(crate) fn fail(what: &str, err: &dyn Display) -> Outcome {
    tracing::error!(error = %err, "{what} failed");
    Outcome::new(ExitKind::Fatal)
}

impl Outcome {
    /// The same outcome for trading day `as_of`.
    pub(crate) fn on(mut self, as_of: AsOf) -> Outcome {
        self.as_of = Some(as_of);
        self
    }
}

/// One start of the capture job, with the SDK's quote connection as the
/// calendar source. Returns how it ended; the caller exits with
/// `outcome.code()`.
///
/// `invocation` is the systemd invocation id of this start
/// (`$INVOCATION_ID`), recorded in `last_exit.json` so the alert can tell
/// this start's state from an earlier one's.
pub async fn run<C: Clock>(
    config: &Config,
    clock: Arc<C>,
    invocation: Option<Invocation>,
) -> Outcome {
    run_with(config, clock, &QuoteDays, invocation).await
}

/// [`run`] with the trading days from `days`.
pub async fn run_with<C: Clock, D: TradingDays>(
    config: &Config,
    clock: Arc<C>,
    days: &D,
    invocation: Option<Invocation>,
) -> Outcome {
    let started = clock.now();
    let lock = match DataDirLock::try_acquire(&config.data_dir) {
        Ok(lock) => lock,
        Err(err @ LockError::Busy(_)) => {
            tracing::warn!("{err}; nothing done");
            return Outcome::new(ExitKind::Busy);
        }
        Err(err) => {
            // Not held, but notify must not read an older start's state.
            let outcome = fail("taking the data-dir lock", &err);
            LastExit::from_outcome(&outcome, clock.now(), invocation.clone())
                .write(&config.data_dir);
            return outcome;
        }
    };
    let outcome = match Store::open(&config.archive_path()).await {
        Ok(store) => {
            let start = Start {
                config,
                clock: &clock,
                days,
                started,
                store: &store,
                oauth: None,
                dangling: Vec::new(),
                missed_scan_incomplete: false,
            };
            let outcome = start.run().await;
            store.close().await;
            outcome
        }
        Err(err) => fail("opening the archive", &err),
    };
    tracing::info!(
        kind = outcome.kind.as_str(),
        code = outcome.code(),
        as_of = outcome.as_of.map(|d| d.to_string()).unwrap_or_default(),
        "capture finished"
    );
    LastExit::from_outcome(&outcome, clock.now(), invocation.clone()).write(&config.data_dir);
    if matches!(
        outcome.kind,
        ExitKind::Complete | ExitKind::CompleteWithFailures | ExitKind::Resumable
    ) {
        // Working again: the next failure alerts, whatever its kind.
        notify::clear_sent(&config.data_dir);
    }
    drop(lock);
    outcome
}

/// One start, holding the lock and the open Store.
struct Start<'a, C: Clock, D: TradingDays> {
    config: &'a Config,
    clock: &'a Arc<C>,
    days: &'a D,
    started: UnixMs,
    store: &'a Store,
    /// The one `OAuth` client, once built.
    oauth: Option<OAuth>,
    /// Every dangling run this start ended `partial`, oldest first.
    dangling: Vec<Dangling>,
    /// The calendar back-fill failed.
    missed_scan_incomplete: bool,
}

impl<C: Clock, D: TradingDays> Start<'_, C, D> {
    async fn run(mut self) -> Outcome {
        let mut outcome = match self.steps().await {
            Ok(outcome) | Err(outcome) => outcome,
        };
        if let Some(latest) = self.dangling.last() {
            // A dangling run ended `partial` here is a window that closed
            // with work left: it alerts even when this start went well, and
            // stays on record when it did not.
            if outcome.code() == 0 {
                let mut partial = Outcome::of(ExitKind::Partial, latest.as_of);
                partial.failure = latest.failure.clone();
                outcome = partial;
            }
        }
        outcome.closed_dangling = std::mem::take(&mut self.dangling);
        outcome.missed_scan_incomplete = self.missed_scan_incomplete;
        outcome
    }

    /// `started + attempt_timeout`.
    fn process_deadline(&self) -> UnixMs {
        clock::after(self.started, self.config.attempt_timeout)
    }

    async fn steps(&mut self) -> Result<Outcome, Outcome> {
        let store_err = |what: &str, err: StoreError| fail(what, &err);
        let dangling = self
            .store
            .close_dangling_runs(self.clock.now())
            .await
            .map_err(|e| store_err("closing dangling runs", e))?;
        for run in &dangling {
            let plan = self
                .store
                .plan(*run)
                .await
                .map_err(|e| store_err("reading a dangling run", e))?;
            let failure = self
                .store
                .dominant_failure(*run, &COUNTED)
                .await
                .map_err(|e| store_err("reading a dangling run", e))?
                .map(|(class, code, _)| format!("{class}:{code}"));
            tracing::warn!(as_of = %plan.as_of, "a run whose window closed was left open; ended partial");
            self.dangling.push(Dangling {
                as_of: plan.as_of,
                failure,
            });
        }

        let calendar = self.calendar().await?;
        let now = self.clock.now();
        let open = calendar
            .window_at(now, self.config.window_start)
            .map_err(|e| fail("finding the window", &e))?;
        let Some((as_of, window)) = open else {
            let calendar = self.backfill(calendar).await?;
            self.mark_missed(&calendar, self.clock.now()).await?;
            tracing::info!("no capture window is open");
            return Ok(Outcome::new(ExitKind::NoWindow));
        };
        // Inside a window the history waits for the next start without one.
        if self.history_incomplete(&calendar).await? {
            self.missed_scan_incomplete = true;
        }

        let request = PlanRequest {
            source: Source::Longbridge,
            as_of,
            window,
            started_at: self.clock.now(),
            config: self.config.research.clone(),
            indexes: self.config.indexes.clone(),
        };
        let plan = match self.store.plan_run(&request).await {
            Ok(plan) => plan,
            Err(StoreError::PlanOutsideWindow { .. }) => {
                tracing::info!(%as_of, "the window closed before the plan");
                return Ok(Outcome::new(ExitKind::NoWindow));
            }
            Err(err) => return Err(store_err("planning the run", err).on(as_of)),
        };
        tracing::info!(
            %as_of,
            run = %plan.run_id,
            created = plan.created,
            keys = plan.config.len(),
            indexes = plan.indexes.len(),
            frozen = plan.frozen,
            "run planned"
        );
        if let Some(ended) = plan.ended {
            tracing::info!(%as_of, outcome = ended.as_str(), "the day's run has ended");
            return Ok(Outcome::of(ExitKind::AlreadyEnded, as_of));
        }

        let deadline = plan.window.end().min(self.process_deadline());
        let oauth = self.oauth(deadline, Some(as_of)).await?;
        let source = LongbridgeSource::new(
            oauth,
            &self.config.http_url,
            self.config.rps,
            Timeouts::PRODUCTION,
        );
        let carry_deadline = clock::after(plan.window.start(), self.config.carry_deadline);
        let outcome = runner::drive(runner::RunCtx {
            clock: Arc::clone(self.clock),
            store: self.store.clone(),
            source: Arc::new(source),
            plan,
            deadline,
            carry_deadline,
            attempt_cap: self.config.attempt_cap,
        })
        .await;
        Ok(outcome)
    }

    /// The shared `OAuth` client, built on first use and bounded by
    /// `deadline`. A token that cannot be used without a browser login, a
    /// refresh that fails, and a build still running at the deadline are all
    /// `AUTH_UNAVAILABLE`.
    async fn oauth(&mut self, deadline: UnixMs, as_of: Option<AsOf>) -> Result<OAuth, Outcome> {
        if let Some(oauth) = &self.oauth {
            return Ok(oauth.clone());
        }
        let storage = DataDirTokenStorage::open(self.config.oauth_dir())
            .map_err(|e| fail("opening the token storage", &e))?;
        // Bounded by the auth bound and the start's deadline: a refresh
        // that hangs must not hold the lock for the whole start.
        let left = u64::try_from(deadline.0.saturating_sub(self.clock.now().0)).unwrap_or(0);
        let bound = Duration::from_millis(left).min(AUTH_BOUND);
        let built = auth::build_oauth(&self.config.oauth_client_id, storage, bound).await;
        match built {
            Ok(oauth) => {
                self.oauth = Some(oauth.clone());
                Ok(oauth)
            }
            Err(reason) => {
                // `reason` is a fixed variant name, never SDK text.
                tracing::error!(%reason, "no usable token; run `auth import` or `auth login`");
                let mut outcome = Outcome::new(ExitKind::AuthUnavailable);
                outcome.as_of = as_of;
                Err(outcome)
            }
        }
    }

    /// The calendar: the cache, refreshed when it is missing, unreadable or a
    /// day old. The refresh fetches today's range alone and saves it (merged
    /// with the cache when the two touch). It needs a usable token (else
    /// `AUTH_UNAVAILABLE`); any other failure falls back to the cache while
    /// it still tells whether a window is open now, else `FATAL`. History is
    /// [`backfill`](Self::backfill)'s, on a start with no window.
    async fn calendar(&mut self) -> Result<TradingCalendar, Outcome> {
        let data_dir = &self.config.data_dir;
        let cached = match TradingCalendar::load(data_dir) {
            Ok(cached) => cached,
            Err(err) => {
                tracing::warn!("{err}; refreshing it");
                None
            }
        };
        let now = self.clock.now();
        let deadline = self.process_deadline();
        let calendar = match cached {
            Some(cached) if !cached.needs_refresh(now) => cached,
            cached => {
                let today = [TradingCalendar::refresh_range(now)];
                let oauth = self.oauth(deadline, None).await?;
                let fetch = self.days.fetch(&oauth, self.config, &today, now);
                let failure = match clock::within(&**self.clock, deadline, fetch).await {
                    Some(Ok(pieces)) => {
                        let fresh = TradingCalendar::combined(pieces);
                        let merged = TradingCalendar::merged(cached.clone(), fresh);
                        merged
                            .save(data_dir)
                            .map_err(|e| fail("saving the calendar", &e))?;
                        tracing::info!(from = %merged.from, to = %merged.to, "calendar refreshed");
                        Ok(merged)
                    }
                    Some(Err(RefreshError::Auth(code))) => {
                        tracing::error!(%code, "the calendar refresh found no usable token");
                        return Err(Outcome::new(ExitKind::AuthUnavailable));
                    }
                    Some(Err(err)) => Err(err.to_string()),
                    None => Err("the deadline passed".to_owned()),
                };
                match (failure, cached) {
                    (Ok(merged), _) => merged,
                    (Err(failure), Some(cached))
                        if cached.window_at(now, self.config.window_start).is_ok() =>
                    {
                        tracing::warn!(%failure, "calendar refresh failed; using the cached calendar");
                        cached
                    }
                    (Err(failure), _) => return Err(fail("refreshing the calendar", &failure)),
                }
            }
        };
        Ok(calendar)
    }

    /// The history back-fill, only on a start with no window open: the
    /// missed-day scan is about closed days, and a slow history request must
    /// never take a capture's time. Fetches [`TradingCalendar::backfill_ranges`]
    /// and merges them; a failure only warns and sets
    /// `missed_scan_incomplete`, and the next start without a window retries.
    async fn backfill(
        &mut self,
        mut calendar: TradingCalendar,
    ) -> Result<TradingCalendar, Outcome> {
        let data_dir = &self.config.data_dir;
        let now = self.clock.now();
        let deadline = self.process_deadline();
        let first_run = self
            .store
            .first_as_of(Source::Longbridge)
            .await
            .map_err(|e| fail("reading the first run", &e))?
            .map(AsOf::date);
        let history = calendar.backfill_ranges(first_run);
        if history.is_empty() {
            return Ok(calendar);
        }
        let backfilled = match self.oauth(deadline, None).await {
            Ok(oauth) => {
                let fetch = self.days.fetch(&oauth, self.config, &history, now);
                match clock::within(&**self.clock, deadline, fetch).await {
                    Some(Ok(mut pieces)) => {
                        pieces.push(calendar.clone());
                        let combined = TradingCalendar::combined(pieces);
                        combined
                            .save(data_dir)
                            .map(|()| combined)
                            .map_err(|e| e.to_string())
                    }
                    Some(Err(err)) => Err(err.to_string()),
                    None => Err("the deadline passed".to_owned()),
                }
            }
            Err(_) => Err("no usable token".to_owned()),
        };
        match backfilled {
            Ok(combined) => {
                tracing::info!(from = %combined.from, ranges = history.len(), "calendar back-filled");
                calendar = combined;
            }
            Err(failure) => {
                tracing::warn!(%failure, "calendar back-fill failed; the missed-day scan is incomplete");
                self.missed_scan_incomplete = true;
            }
        }
        Ok(calendar)
    }

    /// Whether the calendar still lacks history back to the first run.
    async fn history_incomplete(&self, calendar: &TradingCalendar) -> Result<bool, Outcome> {
        let first_run = self
            .store
            .first_as_of(Source::Longbridge)
            .await
            .map_err(|e| fail("reading the first run", &e))?
            .map(AsOf::date);
        Ok(!calendar.backfill_ranges(first_run).is_empty())
    }

    /// Mark every trading day after the first run whose window has closed
    /// and that has no run as `missed`.
    async fn mark_missed(&self, calendar: &TradingCalendar, now: UnixMs) -> Result<(), Outcome> {
        let source = Source::Longbridge;
        let Some(first) = self
            .store
            .first_as_of(source)
            .await
            .map_err(|e| fail("reading the first run", &e))?
        else {
            return Ok(());
        };
        for (as_of, window) in calendar.closed_windows(now, self.config.window_start) {
            if as_of <= first {
                continue;
            }
            match self.store.record_missed(source, as_of, window, now).await {
                Ok(_) => tracing::warn!(%as_of, "no run captured this day; marked missed"),
                Err(StoreError::RunExists(_)) => {}
                Err(StoreError::WindowConflict { with }) => tracing::warn!(
                    %as_of,
                    %with,
                    "the calendar's window for this day conflicts with a stored run; not marked"
                ),
                Err(err) => return Err(fail("marking a missed day", &err)),
            }
        }
        Ok(())
    }
}
