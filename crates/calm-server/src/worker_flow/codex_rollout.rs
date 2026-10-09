use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use calm_exec::flow::{FlowRowCtx, WorkerFlowItemSink, WorkerFlowSource};
use calm_types::error::CoreError;
use calm_types::runtime::WorkerSessionProjection;
use calm_types::worker::{WorkerProviderKind, WorkerSession, WorkerSessionState};
use calm_types::worker_flow::RawRef;
use tokio_util::sync::CancellationToken;

use crate::db::Repo;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::worker_flow::codex_normalizer::{
    RolloutLine, is_turn_context, normalize_rollout_line, rollout_line_source_uuid,
    rollout_record_type, session_meta_id,
};
use crate::worker_flow::cursor::{self, CODEX_ROLLOUT_SOURCE_KIND, CursorWriter};

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_LAZY_RETRY_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_LAZY_RETRY_ATTEMPTS: usize = 30;

#[derive(Clone, Debug)]
pub struct CodexRolloutFlowSourceOptions {
    pub path_override: Option<PathBuf>,
    pub poll_interval: Duration,
    pub lazy_retry_delay: Duration,
    pub lazy_retry_attempts: usize,
}

impl Default for CodexRolloutFlowSourceOptions {
    fn default() -> Self {
        Self {
            path_override: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            lazy_retry_delay: DEFAULT_LAZY_RETRY_DELAY,
            lazy_retry_attempts: DEFAULT_LAZY_RETRY_ATTEMPTS,
        }
    }
}

pub struct CodexRolloutFlowSource {
    repo: Arc<dyn Repo>,
    runtime: WorkerSessionProjection,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    stop: CancellationToken,
    options: CodexRolloutFlowSourceOptions,
}

impl CodexRolloutFlowSource {
    pub fn new(
        repo: Arc<dyn Repo>,
        runtime: WorkerSessionProjection,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        stop: CancellationToken,
    ) -> Self {
        Self::new_with_options(
            repo,
            runtime,
            shared_codex_appserver,
            stop,
            CodexRolloutFlowSourceOptions::default(),
        )
    }

    pub fn new_with_options(
        repo: Arc<dyn Repo>,
        runtime: WorkerSessionProjection,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        stop: CancellationToken,
        options: CodexRolloutFlowSourceOptions,
    ) -> Self {
        Self {
            repo,
            runtime,
            shared_codex_appserver,
            stop,
            options,
        }
    }

    async fn resolve_rollout_path(&self) -> Result<Option<PathBuf>, CoreError> {
        if let Some(path) = &self.options.path_override {
            return Ok(Some(path.clone()));
        }
        let Some(thread_id) = self.runtime.thread_id.as_deref() else {
            tracing::warn!(
                card_id = %self.runtime.card_id,
                runtime_id = %self.runtime.id,
                "codex rollout source skipped runtime without thread_id"
            );
            return Ok(None);
        };

        // The rollout file is the one Codex names in `Thread.path` (`thread/read`; upstream marks it
        // `[UNSTABLE]`): known from `thread/start` on, before the first turn creates the file, and
        // after a daemon restart.
        let warn_after = self.options.lazy_retry_attempts;
        let mut warned = false;
        let mut attempt = 0_usize;
        let mut reported: Option<PathBuf> = None;
        let mut read_error: Option<String> = None;
        loop {
            if self.stop.is_cancelled() {
                return Ok(None);
            }
            if reported.is_none() {
                // Settlement joins this task under the driver's task lock: never wait out an RPC.
                let read = tokio::select! {
                    _ = self.stop.cancelled() => return Ok(None),
                    read = self.shared_codex_appserver.thread_path(thread_id) => read,
                };
                match read {
                    Ok(Some(path)) => reported = Some(path),
                    Ok(None) => {
                        tracing::warn!(
                            card_id = %self.runtime.card_id,
                            runtime_id = %self.runtime.id,
                            thread_id,
                            "codex app-server reported no rollout path (Thread.path is null); not capturing"
                        );
                        return Ok(None);
                    }
                    Err(err) => read_error = Some(err.to_string()),
                }
            }
            if let Some(path) = &reported
                && rollout_exists(path).await?
            {
                return Ok(Some(path.clone()));
            }

            if !self.runtime_is_alive().await {
                if let Some(path) = &reported
                    && rollout_exists(path).await?
                {
                    return Ok(Some(path.clone()));
                }
                // `source_path: None` means thread/read never answered; `error` says why.
                tracing::info!(
                    card_id = %self.runtime.card_id,
                    thread_id,
                    source_path = ?reported,
                    error = ?read_error,
                    "codex runtime reached terminal status with no rollout file to read; exiting source"
                );
                return Ok(None);
            }

            if !warned && attempt >= warn_after {
                warned = true;
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    thread_id,
                    source_path = ?reported,
                    error = ?read_error,
                    "codex rollout not readable after lazy-create retry budget; polling while runtime is alive"
                );
            }
            let delay = if attempt < warn_after {
                self.options.lazy_retry_delay
            } else {
                Duration::from_secs(1)
            };
            sleep_or_cancel(delay, &self.stop).await?;
            attempt = attempt.saturating_add(1);
        }
    }

    async fn run_tail(
        &self,
        session: &WorkerSession,
        sink: &dyn WorkerFlowItemSink,
        path: PathBuf,
    ) -> Result<(), CoreError> {
        let source_path = path.to_string_lossy().to_string();
        let ctx = row_ctx(session, &self.runtime);
        let stored = cursor::get(
            self.repo.as_ref(),
            &self.runtime.card_id,
            CODEX_ROLLOUT_SOURCE_KIND,
        )
        .await?;
        let mut writer = CursorWriter::new(
            &self.runtime.card_id,
            CODEX_ROLLOUT_SOURCE_KIND,
            &source_path,
            stored.as_ref(),
            self.stop.clone(),
        );
        let mut cursor = stored
            .filter(|c| c.source_path == source_path)
            .map(|c| CursorState {
                record_index: c.record_index.max(0) as u64,
                last_source_uuid: c.last_source_uuid,
                last_line_hash: c.last_line_hash,
            })
            .unwrap_or_default();
        let mut position: Option<Position> = None;

        loop {
            if self.stop.is_cancelled() {
                return Ok(());
            }

            let read = match read_rollout_lines(&path).await {
                Ok(read) => read,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
                    continue;
                }
                Err(err) => return Err(CoreError::Io(err)),
            };
            let RolloutRead {
                lines,
                has_terminator,
            } = read;
            if !has_terminator && !lines.is_empty() {
                tracing::debug!(
                    source_path,
                    "codex rollout tail has an unterminated final line; deferring it"
                );
            }

            if lines.is_empty() {
                if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                    return Ok(());
                }
                if !self.runtime_is_alive().await {
                    tracing::info!(
                        card_id = %self.runtime.card_id,
                        runtime_id = %self.runtime.id,
                        "codex runtime reached terminal status; final drain complete, exiting tail"
                    );
                    return Ok(());
                }
                sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
                continue;
            }

            let first = match parse_line(&lines[0], 0, &source_path) {
                Ok(line) => line,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        source_path,
                        "codex rollout first line is malformed; waiting for rewrite"
                    );
                    sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
                    continue;
                }
            };
            if let Some(thread_id) = self.runtime.thread_id.as_deref()
                && session_meta_id(&first) != Some(thread_id)
            {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    expected_thread_id = thread_id,
                    actual_thread_id = session_meta_id(&first).unwrap_or("<missing>"),
                    "codex rollout SessionMeta id mismatch; exiting source without consuming wrong file"
                );
                return Ok(());
            }

            if cursor.record_index > 0 && (cursor.record_index as usize) <= lines.len() {
                let prior_index = (cursor.record_index - 1) as usize;
                let prior_raw = &lines[prior_index];
                let uuid_mismatch = cursor
                    .last_source_uuid
                    .as_deref()
                    .map(|expected| {
                        let actual = parse_line(prior_raw, cursor.record_index - 1, &source_path)
                            .ok()
                            .and_then(|line| rollout_line_source_uuid(&line));
                        actual.as_deref() != Some(expected)
                    })
                    .unwrap_or(false);
                let hash_mismatch = cursor
                    .last_line_hash
                    .as_deref()
                    .map(|expected| hash_line(prior_raw) != expected)
                    .unwrap_or(false);

                if uuid_mismatch || hash_mismatch {
                    tracing::warn!(
                        card_id = %self.runtime.card_id,
                        runtime_id = %self.runtime.id,
                        source_path,
                        record_index = cursor.record_index,
                        "codex rollout prefix mismatch (uuid or hash); resetting cursor to re-ingest"
                    );
                    reset_cursor(&mut cursor, &mut position);
                }
            }

            if cursor.record_index as usize > lines.len() {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    source_path,
                    record_index = cursor.record_index,
                    lines = lines.len(),
                    "codex rollout cursor passed EOF after rewrite; resetting to start"
                );
                reset_cursor(&mut cursor, &mut position);
            }

            let position = position.get_or_insert_with(|| {
                // Perf invariant: reconstruct only on cold start/reset, then carry
                // seq/turn in memory so idle polls do not re-parse the consumed prefix.
                reconstruct_position(&lines, cursor.record_index, &source_path)
            });
            while (cursor.record_index as usize) < lines.len() {
                if self.stop.is_cancelled() {
                    return Ok(());
                }

                let line_index = cursor.record_index;
                let parsed = match parse_line(&lines[line_index as usize], line_index, &source_path)
                {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            source_path,
                            line = line_index,
                            "skipping malformed codex rollout line"
                        );
                        cursor.last_source_uuid = None;
                        cursor.last_line_hash = Some(hash_line(&lines[line_index as usize]));
                        cursor.record_index += 1;
                        if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                            return Ok(());
                        }
                        continue;
                    }
                };

                if is_turn_context(&parsed) {
                    position.turn = position.turn.saturating_add(1);
                    cursor.last_source_uuid = None;
                    cursor.last_line_hash = Some(hash_line(&lines[line_index as usize]));
                    cursor.record_index += 1;
                    if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                        return Ok(());
                    }
                    continue;
                }

                let raw_ref = RawRef {
                    provider: WorkerProviderKind::Codex,
                    source_path: Some(source_path.clone()),
                    line: Some(line_index),
                    record_type: Some(rollout_record_type(&parsed).to_string()),
                };
                let items: Vec<_> = normalize_rollout_line(
                    &parsed,
                    position.seq,
                    position.turn,
                    &session.id,
                    raw_ref,
                )
                .into_iter()
                .collect();
                position.seq = position.seq.saturating_add(items.len() as u64);

                cursor.last_source_uuid = rollout_line_source_uuid(&parsed);
                cursor.last_line_hash = Some(hash_line(&lines[line_index as usize]));
                cursor.record_index += 1;
                if !persist_cursor(&mut writer, sink, &ctx, &cursor, items).await? {
                    return Ok(());
                }
            }

            if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                return Ok(());
            }
            // TODO: session_projection_complete_for_terminal bypasses the event bus; canonicalize via Event emission.
            if !self.runtime_is_alive().await {
                tracing::info!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    "codex runtime reached terminal status; final drain complete, exiting tail"
                );
                return Ok(());
            }
            sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
        }
    }

    async fn runtime_is_alive(&self) -> bool {
        match self.repo.session_projection_by_id(&self.runtime.id).await {
            Ok(Some(runtime)) => !matches!(
                runtime.status,
                WorkerSessionState::Exited
                    | WorkerSessionState::Failed
                    | WorkerSessionState::Superseded
            ),
            Ok(None) => true,
            Err(err) => {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    error = %err,
                    "codex runtime liveness lookup failed; keeping tail alive"
                );
                true
            }
        }
    }
}

#[async_trait]
impl WorkerFlowSource for CodexRolloutFlowSource {
    fn provider(&self) -> WorkerProviderKind {
        WorkerProviderKind::Codex
    }

    async fn capture(
        &self,
        session: &WorkerSession,
        sink: &dyn WorkerFlowItemSink,
    ) -> Result<(), CoreError> {
        let Some(path) = self.resolve_rollout_path().await? else {
            tracing::info!(
                card_id = %self.runtime.card_id,
                runtime_id = %self.runtime.id,
                "codex rollout source exiting without resolved rollout path"
            );
            return Ok(());
        };
        self.run_tail(session, sink, path).await
    }
}

#[derive(Default)]
struct CursorState {
    record_index: u64,
    last_source_uuid: Option<String>,
    last_line_hash: Option<String>,
}

#[derive(Default)]
struct Position {
    seq: u64,
    turn: u32,
}

fn reset_cursor(cursor: &mut CursorState, position: &mut Option<Position>) {
    cursor.record_index = 0;
    cursor.last_source_uuid = None;
    cursor.last_line_hash = None;
    *position = None;
}

fn row_ctx(session: &WorkerSession, runtime: &WorkerSessionProjection) -> FlowRowCtx {
    FlowRowCtx {
        session_id: session.id.clone(),
        track_id: Some(session.track_id.as_str().to_string()),
        card_id: Some(runtime.card_id.clone()),
    }
}

async fn persist_cursor(
    writer: &mut CursorWriter,
    sink: &dyn WorkerFlowItemSink,
    ctx: &FlowRowCtx,
    cursor: &CursorState,
    items: Vec<calm_types::worker_flow::WorkerFlowItem>,
) -> Result<bool, CoreError> {
    writer
        .persist(
            sink,
            ctx,
            items,
            cursor.record_index as i64,
            0,
            cursor.last_source_uuid.as_deref(),
            cursor.last_line_hash.as_deref(),
        )
        .await
}

fn hash_line(raw: &str) -> String {
    use std::fmt::Write as _;

    let digest = blake3::hash(raw.as_bytes());
    let mut hash = String::with_capacity(16);
    for byte in &digest.as_bytes()[..8] {
        write!(&mut hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    hash
}

fn reconstruct_position(lines: &[String], record_index: u64, source_path: &str) -> Position {
    let mut position = Position::default();
    for (idx, raw) in lines.iter().take(record_index as usize).enumerate() {
        let Ok(line) = parse_line(raw, idx as u64, source_path) else {
            continue;
        };
        if is_turn_context(&line) {
            position.turn = position.turn.saturating_add(1);
            continue;
        }
        let raw_ref = RawRef {
            provider: WorkerProviderKind::Codex,
            source_path: Some(source_path.to_string()),
            line: Some(idx as u64),
            record_type: Some(rollout_record_type(&line).to_string()),
        };
        if normalize_rollout_line(
            &line,
            position.seq,
            position.turn,
            &calm_types::worker::WorkerSessionId::from("reconstruct"),
            raw_ref,
        )
        .is_some()
        {
            position.seq = position.seq.saturating_add(1);
        }
    }
    position
}

fn parse_line(raw: &str, line_index: u64, source_path: &str) -> Result<RolloutLine, CoreError> {
    serde_json::from_str(raw).map_err(|e| {
        CoreError::Internal(format!(
            "parse codex rollout line {source_path}:{line_index}: {e}"
        ))
    })
}

async fn sleep_or_cancel(duration: Duration, stop: &CancellationToken) -> Result<(), CoreError> {
    tokio::select! {
        _ = stop.cancelled() => Ok(()),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

struct RolloutRead {
    lines: Vec<String>,
    has_terminator: bool,
}

async fn read_rollout_lines(path: &Path) -> io::Result<RolloutRead> {
    if path.extension().and_then(|s| s.to_str()) == Some("zst") {
        return read_zstd_lines(path).await;
    }
    split_complete_lines(tokio::fs::read(path).await?)
}

async fn read_zstd_lines(path: &Path) -> io::Result<RolloutRead> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("zstd")
            .arg("-dc")
            .arg(&path)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "zstd -dc failed for {} with status {}",
                path.display(),
                output.status
            )));
        }
        split_complete_lines(output.stdout)
    })
    .await
    .map_err(io::Error::other)?
}

fn split_complete_lines(bytes: Vec<u8>) -> io::Result<RolloutRead> {
    let has_terminator = bytes.last().is_some_and(|byte| *byte == b'\n');
    let complete_bytes = if has_terminator {
        bytes.as_slice()
    } else {
        match bytes.iter().rposition(|byte| *byte == b'\n') {
            Some(pos) => &bytes[..=pos],
            None => &[],
        }
    };
    let text = String::from_utf8(complete_bytes.to_vec()).map_err(io::Error::other)?;
    let lines = text
        .split_terminator('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    Ok(RolloutRead {
        lines,
        has_terminator,
    })
}

async fn rollout_exists(path: &Path) -> Result<bool, CoreError> {
    tokio::fs::try_exists(path).await.map_err(CoreError::Io)
}
