//! #2516: which Claude version a Claude card runs, and which one is installed now. Both come
//! only from what `<claude_bin> --version` prints (its first token, e.g. `2.1.280`), never from
//! Claude's install layout. A card's child records the version at its start; the installed
//! version is a cache that background refreshes fill, so no read path runs the CLI.

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::time::Instant;

use crate::claude_planner::readiness_command;
use crate::session_projection_lookup::CLAUDE_VERSION_STATE_KEY;
use crate::state::{AppState, RouteState};

/// An anti-hang bound; the CLI answers `--version` in well under a second.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// The background refresh period of the installed version.
pub const REFRESH_PERIOD: Duration = Duration::from_secs(10 * 60);
/// On-demand refreshes (a page regaining focus) run at most once per this interval.
pub const ON_DEMAND_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// The first token `<claude_bin> --version` prints, or why there is none. The child gets only
/// `PATH` (a bare `claude` resolves as the card's shell resolves it) and `HOME`.
pub async fn probe(claude_bin: &str) -> Result<String, String> {
    let env: Vec<(String, OsString)> = ["PATH", "HOME"]
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.to_string(), value)))
        .collect();
    let (status, stdout) =
        readiness_command::run(Path::new(claude_bin), &["--version"], &env, VERSION_TIMEOUT)
            .await
            .map_err(|failure| format!("`{claude_bin} --version` {failure}"))?;
    if !status.success() {
        return Err(format!("`{claude_bin} --version` exited with {status}"));
    }
    String::from_utf8_lossy(&stdout)
        .split_whitespace()
        .next()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("`{claude_bin} --version` printed nothing"))
}

/// Record on Claude card `card_id`'s active runtime the version its child is about to start on.
/// Run just before the spawn, so the status write that follows it carries the value to readers.
/// A probe or write failure is logged and records nothing: the spawn never waits on it to fail.
pub async fn record_running_version(
    repo: &dyn crate::db::RouteRepo,
    claude_bin: &str,
    card_id: &str,
) {
    let version = match probe(claude_bin).await {
        Ok(version) => version,
        Err(error) => {
            tracing::warn!(card_id, %error, "claude version not recorded for this child");
            return;
        }
    };
    let card = card_id.to_string();
    let written = crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let Some(active) =
                crate::db::sqlite::session_projection_active_for_card_tx(tx, &card).await?
            else {
                return Ok(false);
            };
            let mut state = match active.handle_state_json {
                Some(Value::Object(state)) => state,
                _ => Map::new(),
            };
            state.insert(CLAUDE_VERSION_STATE_KEY.into(), Value::String(version));
            Ok(crate::db::sqlite::session_set_handle_state_tx(
                tx,
                &active.id,
                Some(Value::Object(state)),
            )
            .await?)
        })
    })
    .await;
    match written {
        Ok(true) => {}
        Ok(false) => tracing::warn!(card_id, "claude version not recorded: no active runtime"),
        Err(error) => {
            tracing::warn!(card_id, %error, "claude version not recorded for this child");
        }
    }
}

/// The installed Claude version: the last answer of `<claude_bin> --version`. Refreshed in the
/// background (boot, every [`REFRESH_PERIOD`], after a card's Update, and on demand at most once per
/// [`ON_DEMAND_MIN_INTERVAL`]); a failed refresh is logged and keeps the last value.
pub struct InstalledClaudeVersion {
    claude_bin: String,
    version: StdMutex<Option<String>>,
    last_on_demand: StdMutex<Option<Instant>>,
    /// One `--version` run at a time, so a slow run cannot overwrite a newer answer.
    refresh_serial: tokio::sync::Mutex<()>,
}

impl InstalledClaudeVersion {
    pub fn new(claude_bin: String) -> Self {
        Self {
            claude_bin,
            version: StdMutex::new(None),
            last_on_demand: StdMutex::new(None),
            refresh_serial: tokio::sync::Mutex::new(()),
        }
    }

    /// The cached answer; never runs the CLI.
    pub fn installed(&self) -> Option<String> {
        self.version.lock().expect("installed version lock").clone()
    }

    /// Run `--version` now and keep its answer; a failure keeps the last one.
    pub async fn refresh(&self) {
        let _serial = self.refresh_serial.lock().await;
        match probe(&self.claude_bin).await {
            Ok(version) => *self.version.lock().expect("installed version lock") = Some(version),
            Err(error) => {
                tracing::warn!(%error, "installed claude version not refreshed; keeping the last one")
            }
        }
    }

    /// The on-demand refresh at `now`: runs [`Self::refresh`] unless one ran less than
    /// [`ON_DEMAND_MIN_INTERVAL`] before. Returns whether it ran.
    pub async fn refresh_on_demand(&self, now: Instant) -> bool {
        {
            let mut last = self.last_on_demand.lock().expect("on-demand refresh lock");
            if last.is_some_and(|last| now.saturating_duration_since(last) < ON_DEMAND_MIN_INTERVAL)
            {
                return false;
            }
            *last = Some(now);
        }
        self.refresh().await;
        true
    }

    /// [`Self::refresh`] off the caller's task: a dropped caller does not cancel the run.
    pub fn spawn_refresh(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move { this.refresh().await })
    }

    /// The boot refresh, then one every [`REFRESH_PERIOD`], for the life of the process.
    pub fn spawn_periodic(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(REFRESH_PERIOD);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                this.refresh().await;
            }
        })
    }
}

/// The server's installed-version refresher: at boot, then every [`REFRESH_PERIOD`].
pub fn spawn_periodic_refresh(state: &AppState) -> tokio::task::JoinHandle<()> {
    <RouteState as axum::extract::FromRef<AppState>>::from_ref(state)
        .claude_installed_version
        .spawn_periodic()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::*;

    /// A stand-in for the CLI: `--version` prints `<dir>/version` and exits with `<dir>/status`.
    fn stand_in(dir: &Path) -> PathBuf {
        let bin = dir.join("claude");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n[ \"$1\" = --version ] || exit 2\ncat '{0}/version'\nexit \"$(cat '{0}/status')\"\n",
                dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        answer(dir, "2.1.259 (Claude Code)", 0);
        bin
    }

    fn answer(dir: &Path, printed: &str, status: i32) {
        std::fs::write(dir.join("version"), format!("{printed}\n")).unwrap();
        std::fs::write(dir.join("status"), status.to_string()).unwrap();
    }

    #[tokio::test]
    async fn the_installed_version_is_the_first_token_and_a_failed_run_keeps_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let cache = InstalledClaudeVersion::new(stand_in(dir.path()).display().to_string());
        assert_eq!(cache.installed(), None, "nothing runs until a refresh");

        cache.refresh().await;
        assert_eq!(cache.installed().as_deref(), Some("2.1.259"));

        answer(dir.path(), "2.1.280 (Claude Code)", 1);
        cache.refresh().await;
        answer(dir.path(), "", 0);
        cache.refresh().await;
        assert_eq!(cache.installed().as_deref(), Some("2.1.259"));

        answer(dir.path(), "2.1.280 (Claude Code)", 0);
        cache.refresh().await;
        assert_eq!(cache.installed().as_deref(), Some("2.1.280"));
    }

    /// The time seam is the `now` the route passes in: no sleeps.
    #[tokio::test]
    async fn an_on_demand_refresh_runs_at_most_once_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let cache = InstalledClaudeVersion::new(stand_in(dir.path()).display().to_string());
        let t0 = Instant::now();

        assert!(cache.refresh_on_demand(t0).await);
        assert_eq!(cache.installed().as_deref(), Some("2.1.259"));

        answer(dir.path(), "2.1.280 (Claude Code)", 0);
        let just_before = t0 + ON_DEMAND_MIN_INTERVAL - Duration::from_millis(1);
        assert!(!cache.refresh_on_demand(just_before).await);
        assert_eq!(cache.installed().as_deref(), Some("2.1.259"));

        assert!(cache.refresh_on_demand(t0 + ON_DEMAND_MIN_INTERVAL).await);
        assert_eq!(cache.installed().as_deref(), Some("2.1.280"));
    }
}
