use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use calm_types::error::CoreError;
use calm_types::event::Event;
use calm_types::runtime::WorkerSessionProjection;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::db::Repo;
use crate::worker_flow::claude_transcript::{ClaudeTranscriptFlowSourceOptions, sleep_or_cancel};

const CLAUDE_HOOK_EVENT_KIND: &str = "claude.hook";

pub(super) trait RuntimeAliveProbe: Send {
    fn is_alive<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

/// Where the transcript comes from: a fixed path, or the `transcript_path` Claude reports in
/// its documented hook input for this session (persisted `claude.hook` events, so it survives
/// a server restart and a `--resume`). Only a regular file resolves.
pub(super) enum TranscriptLookup {
    Fixed(PathBuf),
    Hook(HookTranscriptPath),
}

pub(super) struct HookTranscriptPath {
    repo: Arc<dyn Repo>,
    track_id: String,
    card_id: String,
    session_id: String,
    since_id: Option<i64>,
    reported: Option<PathBuf>,
}

enum Probe {
    Missing,
    NotRegularFile(PathBuf),
    Found(PathBuf),
    Conflict,
}

impl Probe {
    fn is_waiting(&self) -> bool {
        matches!(self, Self::Missing | Self::NotRegularFile(_))
    }
}

impl TranscriptLookup {
    pub(super) fn hook(
        repo: Arc<dyn Repo>,
        track_id: String,
        card_id: String,
        session_id: String,
    ) -> Self {
        Self::Hook(HookTranscriptPath {
            repo,
            track_id,
            card_id,
            session_id,
            since_id: None,
            reported: None,
        })
    }

    async fn probe(&mut self) -> Result<Probe, CoreError> {
        let path = match self {
            Self::Fixed(path) => path,
            Self::Hook(hook) => {
                if !hook.read_new_hooks().await? {
                    return Ok(Probe::Conflict);
                }
                let Some(path) = &hook.reported else {
                    return Ok(Probe::Missing);
                };
                path
            }
        };
        match tokio::fs::metadata(path).await {
            Ok(metadata) if metadata.is_file() => Ok(Probe::Found(path.clone())),
            Ok(_) => Ok(Probe::NotRegularFile(path.clone())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Probe::Missing),
            Err(err) => Err(CoreError::Io(err)),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Fixed(path)
            | Self::Hook(HookTranscriptPath {
                reported: Some(path),
                ..
            }) => path.display().to_string(),
            Self::Hook(_) => "<no claude hook transcript_path yet>".to_string(),
        }
    }
}

impl HookTranscriptPath {
    /// Folds hook events past the cursor; returns false when two report different paths.
    /// This conflict policy covers the hooks seen before the path resolves: once tailing
    /// starts, later hooks are not consulted.
    async fn read_new_hooks(&mut self) -> Result<bool, CoreError> {
        let rows = self
            .repo
            .events_for_track(&self.track_id, &[CLAUDE_HOOK_EVENT_KIND], self.since_id)
            .await
            .map_err(|e| CoreError::Internal(format!("events_for_track claude hooks: {e}")))?;
        for row in rows {
            self.since_id = Some(row.id);
            let Event::ClaudeHook {
                card_id, payload, ..
            } = row.event
            else {
                continue;
            };
            if card_id.as_str() != self.card_id
                || payload.get("session_id").and_then(Value::as_str)
                    != Some(self.session_id.as_str())
            {
                continue;
            }
            let Some(path) = payload
                .get("transcript_path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
            else {
                continue;
            };
            match &self.reported {
                Some(reported) if *reported != path => {
                    tracing::warn!(
                        card_id = %self.card_id,
                        session_id = %self.session_id,
                        first_path = %reported.display(),
                        conflicting_path = %path.display(),
                        "claude hooks report conflicting transcript paths for one session; exiting source"
                    );
                    return Ok(false);
                }
                _ => self.reported = Some(path),
            }
        }
        Ok(true)
    }
}

pub(super) async fn wait_for_transcript_path(
    lookup: &mut TranscriptLookup,
    stop: &CancellationToken,
    options: &ClaudeTranscriptFlowSourceOptions,
    runtime: &WorkerSessionProjection,
    runtime_alive: &mut (dyn RuntimeAliveProbe + Send),
) -> Result<Option<PathBuf>, CoreError> {
    let warn_after = options.lazy_retry_attempts;
    let mut warned = false;
    let mut warned_not_regular_file = false;
    let mut attempt = 0_usize;
    loop {
        if stop.is_cancelled() {
            return Ok(None);
        }
        let mut probe = lookup.probe().await?;
        // Runtime shutdown can be what closes Claude's writer and flushes the transcript
        // path, so terminal liveness gets one final probe before the source exits.
        if probe.is_waiting() && !runtime_alive.is_alive().await {
            probe = lookup.probe().await?;
            if probe.is_waiting() {
                tracing::info!(
                    card_id = %runtime.card_id,
                    runtime_id = %runtime.id,
                    source_path = %lookup.describe(),
                    "claude runtime reached terminal status without creating a transcript; exiting source"
                );
                return Ok(None);
            }
        }
        match probe {
            Probe::Found(path) => return Ok(Some(path)),
            Probe::Conflict => return Ok(None),
            Probe::NotRegularFile(path) => {
                if !warned_not_regular_file {
                    warned_not_regular_file = true;
                    tracing::warn!(
                        card_id = %runtime.card_id,
                        source_path = %path.display(),
                        "claude transcript path is not a regular file; not tailing it"
                    );
                }
            }
            Probe::Missing => {}
        }
        if !warned && attempt >= warn_after {
            warned = true;
            tracing::warn!(
                card_id = %runtime.card_id,
                runtime_id = %runtime.id,
                source_path = %lookup.describe(),
                "claude transcript not present after lazy-retry budget; continuing to poll (claude creates file on first prompt)"
            );
        }
        let delay = if attempt < warn_after {
            options.lazy_retry_delay
        } else {
            Duration::from_secs(1)
        };
        sleep_or_cancel(delay, stop).await?;
        attempt = attempt.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::runtime::{AgentProvider, WorkerSessionKind};
    use calm_types::worker::WorkerSessionState;

    struct CreateTranscriptOnTerminal {
        path: PathBuf,
        calls: usize,
    }

    impl RuntimeAliveProbe for CreateTranscriptOnTerminal {
        fn is_alive<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move {
                self.calls = self.calls.saturating_add(1);
                tokio::fs::write(&self.path, b"{}\n").await.unwrap();
                false
            })
        }
    }

    #[tokio::test]
    async fn lazy_resolve_final_recheck_catches_file_created_during_terminal_liveness() {
        let transcript_dir = tempfile::tempdir().unwrap();
        let path = transcript_dir.path().join("session-lazy-race.jsonl");
        let stop = CancellationToken::new();
        let options = ClaudeTranscriptFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
        };
        let runtime = WorkerSessionProjection {
            id: "rt-lazy-race".into(),
            card_id: "card-lazy-race".into(),
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: None,
            session_id: Some("session-lazy-race".into()),
            active_turn_id: None,
            handle_state_json: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            completed_at_ms: None,
            last_turn_completed_ms: None,
        };
        let mut runtime_alive = CreateTranscriptOnTerminal {
            path: path.clone(),
            calls: 0,
        };

        let mut lookup = TranscriptLookup::Fixed(path.clone());
        let resolved =
            wait_for_transcript_path(&mut lookup, &stop, &options, &runtime, &mut runtime_alive)
                .await
                .unwrap();

        assert_eq!(resolved, Some(path));
        assert_eq!(runtime_alive.calls, 1);
    }
}
