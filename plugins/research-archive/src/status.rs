//! The archive's health, for a reader outside the capture job (the status
//! plugin): how the last capture start ended, whether starts still happen,
//! and the recent runs.
//!
//! It reads two files under the data dir and nothing else: the archive,
//! through [`ReadOnlyStore`], and `last_exit.json`. It never takes the lock,
//! never reads the token and never calls Longbridge, so it can run while a
//! capture is writing.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::state::{ExitKind, LastExit};
use crate::store::{PairCoverage, ReadOnlyStore, RunSummary, Store, StoreError};
use crate::types::{MembershipMode, RunOutcome, Source, UnixMs, degraded};

/// How many runs, latest first, the status shows and checks.
pub const RECENT_RUNS: u32 = 7;

/// The overall verdict. Ordered: the status takes its worst finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Nothing found.
    Ok,
    /// Data was lost or is weaker than planned, but the capture runs.
    Attention,
    /// The capture is failing or not running; someone has to act.
    Failing,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Attention => "attention",
            Health::Failing => "failing",
        }
    }
}

/// One reason the status is not `ok`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub health: Health,
    pub text: String,
}

/// One frozen index of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IndexStatus {
    pub index: String,
    pub mode: &'static str,
}

/// One recent run and its pair counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunStatus {
    pub as_of: String,
    /// `complete`, `partial` or `missed`; `open` while the run has no end.
    pub outcome: &'static str,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// Planned key × endpoint pairs. Index members count once frozen.
    pub pairs_planned: u32,
    /// Pairs with an `ok` row.
    pub pairs_ok: u32,
    /// Pairs with rows but no `ok`.
    pub pairs_failed: u32,
    /// Pairs with no row yet.
    pub pairs_untried: u32,
    pub frozen: Vec<IndexStatus>,
}

/// The archive's health at `checked_at`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArchiveStatus {
    pub checked_at: i64,
    pub health: Health,
    /// Every reason for `health`, worst first; empty when it is `ok`.
    pub findings: Vec<Finding>,
    /// `None` when `last_exit.json` is missing or unreadable.
    pub last_exit: Option<LastExit>,
    /// The recent runs, latest first; empty when the archive cannot be read.
    pub runs: Vec<RunStatus>,
}

/// What the reader decides itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// A `last_exit.json` older than this means starts stopped happening.
    /// The timer starts the capture every 30 minutes, and every start that
    /// gets the lock writes the file, so this should be well above that.
    pub stale_after_ms: i64,
}

/// Reads the status of the archive in `data_dir` at `now`.
pub async fn read(data_dir: &Path, now: UnixMs, policy: Policy) -> ArchiveStatus {
    let mut findings = Vec::new();
    let last_exit = LastExit::read(data_dir);
    exit_findings(last_exit.as_ref(), now, policy, &mut findings);
    let runs = match read_runs(data_dir).await {
        Ok(runs) => {
            run_findings(&runs, &mut findings);
            runs.into_iter().map(|(_, status)| status).collect()
        }
        Err(text) => {
            findings.push(Finding {
                health: Health::Failing,
                text,
            });
            Vec::new()
        }
    };
    // Stable: findings of one health keep the order they were found in.
    findings.sort_by_key(|f| std::cmp::Reverse(f.health));
    ArchiveStatus {
        checked_at: now.0,
        health: findings.first().map_or(Health::Ok, |f| f.health),
        findings,
        last_exit,
        runs,
    }
}

/// What only `last_exit.json` can tell: whether starts still happen, a
/// start stopped before it could record anything in the archive (no token,
/// or a fatal error), and an incomplete missed-day scan. Data loss is read
/// from the runs instead ([`run_findings`]): the capture overwrites the file
/// on every start, so a lost day would read healthy 30 minutes later.
///
/// No text names the exit's time, so the findings change only when the
/// state does; the staleness finding names it, as it is fixed while stale.
fn exit_findings(last: Option<&LastExit>, now: UnixMs, policy: Policy, out: &mut Vec<Finding>) {
    let Some(last) = last else {
        out.push(Finding {
            health: Health::Failing,
            text: "no capture start has recorded how it ended: last_exit.json is missing or \
                   unreadable"
                .into(),
        });
        return;
    };
    if now.0 - last.at > policy.stale_after_ms {
        out.push(Finding {
            health: Health::Failing,
            text: format!(
                "the last capture start ended at {}, more than {} minutes ago: the timer is \
                 not starting the capture",
                time(last.at),
                policy.stale_after_ms / 60_000
            ),
        });
    }
    if matches!(last.kind, ExitKind::AuthUnavailable | ExitKind::Fatal) {
        out.push(Finding {
            health: Health::Failing,
            text: format!(
                "the last capture start ended {}. {}",
                last.describe(),
                remedy(last.kind)
            ),
        });
    }
    if last.missed_scan_incomplete {
        out.push(Finding {
            health: Health::Attention,
            text: "the last start could not read the whole trading calendar, so missed days may \
                   not be marked yet; a later start retries"
                .into(),
        });
    }
}

/// What a person does about an alerting exit.
fn remedy(kind: ExitKind) -> &'static str {
    match kind {
        ExitKind::AuthUnavailable => {
            "The token is unusable: run `research-archive auth login` on the host."
        }
        _ => "Check `journalctl --user -u research-archive-capture` on the host.",
    }
}

async fn read_runs(data_dir: &Path) -> Result<Vec<(RunSummary, RunStatus)>, String> {
    let path = Store::path(data_dir);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("the archive does not exist yet: no capture has run".into());
        }
        Err(e) => return Err(format!("the archive cannot be read: {e}")),
    }
    let store = ReadOnlyStore::open(&path)
        .await
        .map_err(|e| format!("the archive cannot be read: {e}"))?;
    let runs = summarize(&store).await;
    store.close().await;
    runs.map_err(|e| format!("the archive cannot be read: {e}"))
}

async fn summarize(store: &ReadOnlyStore) -> Result<Vec<(RunSummary, RunStatus)>, StoreError> {
    let mut out = Vec::new();
    for run in store.recent_runs(Source::Longbridge, RECENT_RUNS).await? {
        let coverage = store.coverage(run.run_id).await?;
        let status = run_status(&run, &coverage);
        out.push((run, status));
    }
    Ok(out)
}

fn run_status(run: &RunSummary, coverage: &[PairCoverage]) -> RunStatus {
    let count = |f: &dyn Fn(&PairCoverage) -> bool| {
        u32::try_from(coverage.iter().filter(|p| f(p)).count()).expect("pairs fit in u32")
    };
    RunStatus {
        as_of: run.as_of.to_string(),
        outcome: run.end.as_ref().map_or("open", |e| e.outcome.as_str()),
        started_at: run.started_at.0,
        finished_at: run.end.as_ref().map(|e| e.finished_at.0),
        pairs_planned: count(&|_| true),
        pairs_ok: count(&|p| p.has_ok),
        pairs_failed: count(&|p| p.attempts > 0 && !p.has_ok),
        pairs_untried: count(&|p| p.attempts == 0),
        frozen: run
            .frozen
            .iter()
            .map(|f| IndexStatus {
                index: f.index.to_string(),
                mode: f.mode.as_str(),
            })
            .collect(),
    }
}

/// Data loss, from the runs, which keep it for good. The latest ended run
/// decides: `partial` or degraded is `failing` until a later day ends
/// `complete`; complete with failures is `attention`. An older run's loss,
/// and a missed day anywhere, is `attention`. An open run is not judged.
fn run_findings(runs: &[(RunSummary, RunStatus)], out: &mut Vec<Finding>) {
    let latest_ended = runs.iter().position(|(run, _)| run.end.is_some());
    for (i, (run, status)) in runs.iter().enumerate() {
        let Some(end) = &run.end else { continue };
        let latest = Some(i) == latest_ended;
        let lost = |text: String| Finding {
            health: if latest {
                Health::Failing
            } else {
                Health::Attention
            },
            text: if latest {
                format!(
                    "{text}. Data of that day is lost for good; check `journalctl --user -u \
                     research-archive-capture` for the cause"
                )
            } else {
                text
            },
        };
        let (ok, planned) = (status.pairs_ok, status.pairs_planned);
        match end.outcome {
            RunOutcome::Missed => out.push(Finding {
                health: Health::Attention,
                text: format!("{} was missed: no capture ran during its window", run.as_of),
            }),
            RunOutcome::Partial => out.push(lost(format!(
                "{} ended partial: {} of {planned} pairs have no ok",
                run.as_of,
                planned - ok
            ))),
            RunOutcome::Complete if degraded(ok as usize, planned as usize) => {
                out.push(lost(format!(
                    "{} ended degraded: only {ok} of {planned} pairs have an ok",
                    run.as_of
                )))
            }
            RunOutcome::Complete if status.pairs_failed > 0 => out.push(Finding {
                health: Health::Attention,
                text: format!(
                    "{} ended complete with failures: {} of {planned} pairs used up their \
                     attempts with no ok",
                    run.as_of, status.pairs_failed
                ),
            }),
            RunOutcome::Complete => {}
        }
    }
    // Only the latest run's universe matters for what is captured next.
    if let Some((latest, _)) = runs.first() {
        for index in &latest.frozen {
            if index.mode != MembershipMode::Observed {
                out.push(Finding {
                    health: Health::Attention,
                    text: format!(
                        "{}: the membership of {} is {}, not observed",
                        latest.as_of,
                        index.index,
                        index.mode.as_str()
                    ),
                });
            }
        }
    }
}

/// `YYYY-MM-DD HH:MMZ`, UTC.
pub fn time(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms).map_or_else(
        || ms.to_string(),
        |t| t.format("%Y-%m-%d %H:%MZ").to_string(),
    )
}
