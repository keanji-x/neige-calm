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

mod runner;
mod schedule;

pub use runner::{DEGRADED_BELOW, degraded};
pub use schedule::{COUNTED, MAX_IN_FLIGHT, backoff, counts_toward_cap};

use std::{fmt::Display, sync::Arc};

use longbridge::oauth::OAuth;

use crate::auth::{self, DataDirTokenStorage};
use crate::calendar::TradingCalendar;
use crate::calendar::fetch::{QuoteDays, RefreshError, TradingDays};
use crate::clock::{self, Clock};
use crate::config::Config;
use crate::lock::{DataDirLock, LockError};
use crate::notify;
use crate::source::{LongbridgeSource, Timeouts};
use crate::state::{Dangling, ExitKind, LastExit, Outcome};
use crate::store::PlanRequest;
use crate::types::{AsOf, Source, UnixMs};
use crate::{Store, StoreError};

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
pub async fn run<C: Clock>(config: &Config, clock: Arc<C>) -> Outcome {
    run_with(config, clock, &QuoteDays).await
}

/// [`run`] with the trading days from `days`.
pub async fn run_with<C: Clock, D: TradingDays>(
    config: &Config,
    clock: Arc<C>,
    days: &D,
) -> Outcome {
    let started = clock.now();
    let lock = match DataDirLock::try_acquire(&config.data_dir) {
        Ok(lock) => lock,
        Err(err @ LockError::Busy(_)) => {
            tracing::warn!("{err}; nothing done");
            return Outcome::new(ExitKind::Busy);
        }
        Err(err) => return fail("taking the data-dir lock", &err),
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
                dangling: None,
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
    LastExit::from_outcome(&outcome, clock.now()).write(&config.data_dir);
    if matches!(
        outcome.kind,
        ExitKind::Complete | ExitKind::CompleteWithFailures
    ) {
        // Recovered: the next failure alerts, whatever its kind.
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
    /// The latest dangling run this start ended `partial`.
    dangling: Option<Dangling>,
}

impl<C: Clock, D: TradingDays> Start<'_, C, D> {
    async fn run(mut self) -> Outcome {
        let mut outcome = match self.steps().await {
            Ok(outcome) | Err(outcome) => outcome,
        };
        if let Some(dangling) = self.dangling.take() {
            // A dangling run ended `partial` here is a window that closed
            // with work left: it alerts even when this start went well, and
            // stays on record when it did not.
            if outcome.code() == 0 {
                outcome = Outcome {
                    kind: ExitKind::Partial,
                    as_of: Some(dangling.as_of),
                    failure: dangling.failure.clone(),
                    closed_dangling: None,
                };
            }
            outcome.closed_dangling = Some(dangling);
        }
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
            self.dangling = Some(Dangling {
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
            self.mark_missed(&calendar, now).await?;
            tracing::info!("no capture window is open");
            return Ok(Outcome::new(ExitKind::NoWindow));
        };

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
        let left = u64::try_from(deadline.0.saturating_sub(self.clock.now().0)).unwrap_or(0);
        let built = auth::build_oauth(
            &self.config.oauth_client_id,
            storage,
            std::time::Duration::from_millis(left),
        )
        .await;
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

    /// The calendar cache, refreshed first when it is missing, unreadable or
    /// a day old. A refresh fetches today's range and every gap back to the
    /// history the archive needs ([`TradingCalendar::ranges_to_fetch`]). A
    /// refresh that fails for any reason but auth falls back to the cache
    /// while the cache still tells whether a window is open now.
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
        if let Some(cached) = &cached
            && !cached.needs_refresh(now)
        {
            return Ok(cached.clone());
        }
        let first_run = self
            .store
            .first_as_of(Source::Longbridge)
            .await
            .map_err(|e| fail("reading the first run", &e))?
            .map(AsOf::date);
        let ranges = TradingCalendar::ranges_to_fetch(cached.as_ref(), first_run, now);
        let deadline = self.process_deadline();
        let oauth = self.oauth(deadline, None).await?;
        let fetch = self.days.fetch(&oauth, self.config, &ranges, now);
        let failure = match clock::within(&**self.clock, deadline, fetch).await {
            Some(Ok(pieces)) => {
                let merged = TradingCalendar::combined(cached, pieces);
                merged
                    .save(data_dir)
                    .map_err(|e| fail("saving the calendar", &e))?;
                tracing::info!(from = %merged.from, to = %merged.to, ranges = ranges.len(), "calendar refreshed");
                return Ok(merged);
            }
            Some(Err(RefreshError::Auth(code))) => {
                tracing::error!(%code, "the calendar refresh found no usable token");
                return Err(Outcome::new(ExitKind::AuthUnavailable));
            }
            Some(Err(err)) => err.to_string(),
            None => "the deadline passed".to_owned(),
        };
        match cached {
            Some(cached) if cached.window_at(now, self.config.window_start).is_ok() => {
                tracing::warn!(%failure, "calendar refresh failed; using the cached calendar");
                Ok(cached)
            }
            _ => Err(fail("refreshing the calendar", &failure)),
        }
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
