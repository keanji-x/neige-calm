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
//! released, including when the SDK's browser flow ends the process.

mod runner;

pub use runner::{COUNTED, DEGRADED_BELOW, MAX_IN_FLIGHT, backoff, counts_toward_cap, degraded};

use std::{fmt::Display, sync::Arc};

use longbridge::oauth::{OAuth, OAuthError};

use crate::auth::{self, DataDirTokenStorage};
use crate::calendar::{TradingCalendar, fetch::RefreshError};
use crate::clock::{self, Clock};
use crate::config::Config;
use crate::lock::{DataDirLock, LockError};
use crate::source::LongbridgeSource;
use crate::state::{ExitKind, LastExit, Outcome};
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

/// A fixed name for an OAuth failure. The SDK's error text is never logged:
/// it can quote a server message.
fn oauth_category(err: &OAuthError) -> &'static str {
    match err {
        OAuthError::NoTokenAvailable => "no_token_available",
        OAuthError::NoRefreshToken => "no_refresh_token",
        OAuthError::RefreshTokenFailed { .. } => "refresh_token_failed",
        OAuthError::TokenFileWrite { .. } => "token_file_write",
        OAuthError::BindCallbackFailed { .. } => "bind_callback_failed",
        _ => "oauth",
    }
}

/// One start of the capture job. Returns how it ended; the caller exits
/// with `outcome.code()`.
pub async fn run<C: Clock>(config: &Config, clock: Arc<C>) -> Outcome {
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
                started,
                store: &store,
                oauth: None,
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
    drop(lock);
    outcome
}

/// One start, holding the lock and the open Store.
struct Start<'a, C: Clock> {
    config: &'a Config,
    clock: &'a Arc<C>,
    started: UnixMs,
    store: &'a Store,
    /// The one `OAuth` client, once built.
    oauth: Option<OAuth>,
}

impl<C: Clock> Start<'_, C> {
    async fn run(mut self) -> Outcome {
        match self.steps().await {
            Ok(outcome) | Err(outcome) => outcome,
        }
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
        let mut dangling_day = None;
        for run in &dangling {
            let plan = self
                .store
                .plan(*run)
                .await
                .map_err(|e| store_err("reading a dangling run", e))?;
            tracing::warn!(as_of = %plan.as_of, "a run whose window closed was left open; ended partial");
            dangling_day = Some(plan.as_of);
        }
        let dangling_partial = dangling_day.map(|d| Outcome::of(ExitKind::Partial, d));

        let calendar = self.calendar().await?;
        let now = self.clock.now();
        let open = calendar
            .window_at(now, self.config.window_start)
            .map_err(|e| fail("finding the window", &e))?;
        let Some((as_of, window)) = open else {
            self.mark_missed(&calendar, now).await?;
            tracing::info!("no capture window is open");
            return Ok(dangling_partial.unwrap_or(Outcome::new(ExitKind::NoWindow)));
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
                return Ok(dangling_partial.unwrap_or(Outcome::new(ExitKind::NoWindow)));
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
        let source = LongbridgeSource::new(oauth, &self.config.http_url, self.config.rps);
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
        // A dangling run closed at this start is a window that closed with
        // work left; it alerts even when this run went well.
        Ok(match dangling_partial {
            Some(partial) if outcome.code() == 0 => partial,
            _ => outcome,
        })
    }

    /// The shared `OAuth` client, built on first use and bounded by
    /// `deadline`. A browser flow ends the process with `AUTH_UNAVAILABLE`
    /// after writing the exit state.
    async fn oauth(&mut self, deadline: UnixMs, as_of: Option<AsOf>) -> Result<OAuth, Outcome> {
        if let Some(oauth) = &self.oauth {
            return Ok(oauth.clone());
        }
        let unavailable = Outcome {
            kind: ExitKind::AuthUnavailable,
            as_of,
            failure: None,
        };
        let storage = DataDirTokenStorage::open(self.config.oauth_dir())
            .map_err(|e| fail("opening the token storage", &e))?;
        let before_exit = {
            let clock = Arc::clone(self.clock);
            let data_dir = self.config.data_dir.clone();
            let outcome = unavailable.clone();
            move || LastExit::from_outcome(&outcome, clock.now()).write(&data_dir)
        };
        let build = auth::build_oauth(&self.config.oauth_client_id, storage, before_exit);
        match clock::within(&**self.clock, deadline, build).await {
            Some(Ok(oauth)) => {
                self.oauth = Some(oauth.clone());
                Ok(oauth)
            }
            Some(Err(err)) => {
                tracing::error!(
                    category = oauth_category(&err),
                    "no usable token; run `auth import` or `auth login`"
                );
                Err(unavailable)
            }
            None => {
                tracing::error!("the token could not be established before the deadline");
                Err(unavailable)
            }
        }
    }

    /// The calendar cache, refreshed first when it is missing, unreadable or
    /// a day old.
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
        let deadline = self.process_deadline();
        let oauth = self.oauth(deadline, None).await?;
        let fetch = crate::calendar::fetch::fetch(oauth, self.config, now);
        let fresh = match clock::within(&**self.clock, deadline, fetch).await {
            Some(Ok(fresh)) => fresh,
            Some(Err(RefreshError::Auth(code))) => {
                tracing::error!(%code, "the calendar refresh found no usable token");
                return Err(Outcome::new(ExitKind::AuthUnavailable));
            }
            Some(Err(err)) => return Err(fail("refreshing the calendar", &err)),
            None => return Err(fail("refreshing the calendar", &"deadline passed")),
        };
        let merged = TradingCalendar::merged(cached, fresh);
        merged
            .save(data_dir)
            .map_err(|e| fail("saving the calendar", &e))?;
        tracing::info!(from = %merged.from, to = %merged.to, "calendar refreshed");
        Ok(merged)
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
