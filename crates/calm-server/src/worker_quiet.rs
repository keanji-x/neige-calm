//! The quiet-worker detector (#1755, #2492): hands each quiet episode of a running task's worker
//! terminal, once, to the Track's worker watcher. A worker that is busy (running a tool, waiting
//! for its model) keeps repainting; one stopped at a startup prompt or idle at its input prompt
//! prints nothing. The kernel reads only the renderer's last-output instant, never the screen: the
//! watcher, sent the `attempt_id`, reads the screen itself and reports to the Planner
//! (`worker_watch`). The detector only hands over a screen the terminal tools can read. After a
//! server restart a reattached worker terminal has no readable screen and is skipped, so a worker
//! already stuck before the restart is not detected (#2499).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

use crate::db::RouteRepo;
use crate::model::{Task, TaskStatus};
use crate::terminal_interaction::{Target, TerminalInteraction};
use crate::terminal_renderer::TerminalRendererRegistry;

/// A worker terminal is quiet once its last output is at least this old. A busy worker repaints
/// at least every couple of seconds, so a minute of silence means stopped or idle.
pub const QUIET_AFTER: Duration = Duration::from_secs(60);

/// How often the detector sweeps; a quiet worker is reported at most this long after
/// [`QUIET_AFTER`].
pub const TICK_INTERVAL: Duration = Duration::from_secs(15);

/// Where a quiet episode goes: the Track's worker watcher (production:
/// [`crate::worker_watch::WorkerWatcher`]). `episode_key` keys the delivery, so a retry of an
/// episode whose first answer was lost replays it instead of delivering it twice.
pub trait QuietWorkerInbox: Send + Sync {
    fn deliver<'a>(
        &'a self,
        track_id: &'a str,
        episode_key: &'a str,
        text: String,
    ) -> BoxFuture<'a, crate::error::Result<()>>;
}

/// One quiet episode: an attempt's worker terminal since its last output. New output starts a new
/// episode, so a later silence is handed over again.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Episode {
    attempt_id: String,
    last_output_ms: i64,
}

impl Episode {
    fn key(&self) -> String {
        format!("{}:{}", self.attempt_id, self.last_output_ms)
    }
}

pub struct WorkerQuietDetector {
    repo: Arc<dyn RouteRepo>,
    renderer: Arc<TerminalRendererRegistry>,
    inbox: Arc<dyn QuietWorkerInbox>,
    quiet_after_ms: i64,
    /// Episodes already delivered, in-process only: a restart forgets them, and the terminals it
    /// reattaches have no readable screen, so they are skipped rather than delivered again (#2499).
    delivered: HashSet<Episode>,
}

impl WorkerQuietDetector {
    pub fn new(
        repo: Arc<dyn RouteRepo>,
        renderer: Arc<TerminalRendererRegistry>,
        inbox: Arc<dyn QuietWorkerInbox>,
        quiet_after: Duration,
    ) -> Self {
        Self {
            repo,
            renderer,
            inbox,
            quiet_after_ms: i64::try_from(quiet_after.as_millis()).unwrap_or(i64::MAX),
            delivered: HashSet::new(),
        }
    }

    /// One sweep at `now_ms`: deliver each quiet episode not delivered yet, then forget the
    /// episodes that ended. Returns the keys of the episodes it delivered; a failed delivery stays
    /// due, so the next sweep retries it under the same key.
    pub async fn tick(&mut self, now_ms: i64) -> Vec<String> {
        let tasks = match self.repo.tasks_nonterminal().await {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::warn!(%error, "worker_quiet: task enumeration failed");
                return Vec::new();
            }
        };
        let mut current = HashSet::new();
        let mut delivered = Vec::new();
        for task in tasks {
            let Some(episode) = self.episode(&task).await else {
                continue;
            };
            current.insert(episode.clone());
            let quiet_ms = now_ms - episode.last_output_ms;
            if quiet_ms < self.quiet_after_ms || self.delivered.contains(&episode) {
                continue;
            }
            let key = episode.key();
            // The threshold, not the measured silence: an episode's text never changes, so a retry
            // under its key replays the first delivery instead of conflicting with it.
            let text = match crate::worker_watch::watch_text(
                &task.key,
                &task.id,
                self.quiet_after_ms / 1000,
            ) {
                Ok(text) => text,
                Err(error) => {
                    tracing::error!(%error, "worker_quiet: watch message does not render");
                    return delivered;
                }
            };
            match self.inbox.deliver(&task.track_id, &key, text).await {
                Ok(()) => {
                    self.delivered.insert(episode);
                    delivered.push(key);
                }
                Err(error) => {
                    tracing::warn!(attempt_id = %task.id, %error, "worker_quiet: delivery to the watcher failed");
                }
            }
        }
        self.delivered.retain(|episode| current.contains(episode));
        delivered
    }

    /// The current episode of `task`'s worker terminal: `None` unless the task is an agent
    /// worker the scheduler holds to a running-liveness deadline, the terminal tools can
    /// reach it by `attempt_id`, the attempt is running, its worker session is live, and its PTY
    /// has a live renderer entry that printed, has not exited and whose screen can be read.
    async fn episode(&self, task: &Task) -> Option<Episode> {
        // Only an agent worker is stuck or idle when silent: a terminal task's command (or a child
        // Track's row) may print nothing for long, and the scheduler's deadline covers it.
        if !crate::scheduler::task_has_running_liveness_deadline(task) {
            return None;
        }
        let target = Target::Attempt(task.id.clone());
        let resolved = match TerminalInteraction::resolve_in_track(
            self.repo.as_ref(),
            &task.track_id,
            &target,
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::trace!(attempt_id = %task.id, %error, "worker_quiet: not targetable");
                return None;
            }
        };
        let attempt_id = resolved.binding.task.as_ref()?.attempt_id.clone();
        if resolved.task_status != Some(TaskStatus::Running) || !resolved.session_active {
            return None;
        }
        let terminal_id = resolved.binding.terminal_id.as_str();
        let entry = self.renderer.get(terminal_id)?;
        if entry.exit.lock().map_or(true, |exit| exit.is_some()) || !entry.observable() {
            return None;
        }
        let last_output_ms = self.renderer.last_output_ms(terminal_id)?;
        Some(Episode {
            attempt_id,
            last_output_ms,
        })
    }

    pub async fn run(mut self, period: Duration) {
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.tick(crate::model::now_ms()).await;
        }
    }
}

/// Spawn the detector loop over `state` with the production threshold, period and watcher.
pub fn spawn(state: &crate::state::AppState) {
    let detector = WorkerQuietDetector::new(
        state.repo.clone(),
        state.terminal_renderer.clone(),
        Arc::new(crate::worker_watch::WorkerWatcher::new(state)),
        QUIET_AFTER,
    );
    tokio::spawn(detector.run(TICK_INTERVAL));
}

#[cfg(test)]
mod tests {
    use crate::mcp_server::tools::track_file::GUIDES;

    /// #2492: the watcher's rules left the Planner's guide; the watch message carries them.
    #[test]
    fn the_planner_terminal_guide_no_longer_carries_the_quiet_worker_rules() {
        let guide = GUIDES
            .iter()
            .find(|(name, _)| *name == "terminal.md")
            .map(|(_, text)| *text)
            .expect("the terminal guide is served");
        for gone in [
            "## Quiet worker",
            "worker_quiet",
            "Yes, I trust this folder",
            "Only a startup screen counts.",
        ] {
            assert!(!guide.contains(gone), "still in guide/terminal.md: {gone}");
        }
    }
}
