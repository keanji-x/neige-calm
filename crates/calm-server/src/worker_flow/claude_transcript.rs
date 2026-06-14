use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use calm_exec::flow::{FlowRowCtx, WorkerFlowItemSink, WorkerFlowSource};
use calm_types::error::CoreError;
use calm_types::runtime::{CardRuntime, RunStatus};
use calm_types::worker::{WorkerProviderKind, WorkerSession};
use calm_types::worker_flow::RawRef;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::sync::CancellationToken;

use crate::db::Repo;
use crate::model::now_ms;
use crate::worker_flow::claude_normalizer::{
    ClaudeNormalizerState, normalize_record_with_state, record_cwd, record_starts_turn,
    record_type, source_uuid,
};
use crate::worker_flow::cursor;

pub const CLAUDE_TRANSCRIPT_SOURCE_KIND: &str = "claude_transcript";

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_LAZY_RETRY_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_LAZY_RETRY_ATTEMPTS: usize = 30;
const DEFAULT_CURSOR_PERSIST_EVERY: u64 = 1;

#[derive(Clone, Debug)]
pub struct ClaudeTranscriptFlowSourceOptions {
    pub path_override: Option<PathBuf>,
    pub poll_interval: Duration,
    pub lazy_retry_delay: Duration,
    pub lazy_retry_attempts: usize,
    pub cursor_persist_every: u64,
}

impl Default for ClaudeTranscriptFlowSourceOptions {
    fn default() -> Self {
        Self {
            path_override: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            lazy_retry_delay: DEFAULT_LAZY_RETRY_DELAY,
            lazy_retry_attempts: DEFAULT_LAZY_RETRY_ATTEMPTS,
            cursor_persist_every: DEFAULT_CURSOR_PERSIST_EVERY,
        }
    }
}

pub struct ClaudeTranscriptFlowSource {
    repo: Arc<dyn Repo>,
    runtime: CardRuntime,
    card_cwd: String,
    stop: CancellationToken,
    options: ClaudeTranscriptFlowSourceOptions,
}

impl ClaudeTranscriptFlowSource {
    pub fn new(
        repo: Arc<dyn Repo>,
        runtime: CardRuntime,
        card_cwd: String,
        stop: CancellationToken,
    ) -> Self {
        Self::new_with_options(
            repo,
            runtime,
            card_cwd,
            stop,
            ClaudeTranscriptFlowSourceOptions::default(),
        )
    }

    pub fn new_with_options(
        repo: Arc<dyn Repo>,
        runtime: CardRuntime,
        card_cwd: String,
        stop: CancellationToken,
        options: ClaudeTranscriptFlowSourceOptions,
    ) -> Self {
        Self {
            repo,
            runtime,
            card_cwd,
            stop,
            options,
        }
    }

    async fn resolve_transcript_path(&self) -> Result<Option<PathBuf>, CoreError> {
        let expected_slug = slug_for_projects(&self.card_cwd);
        let path = if let Some(path) = &self.options.path_override {
            path.clone()
        } else {
            let Some(session_id) = self.runtime.session_id.as_deref() else {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    "worker-flow claude runtime has no session_id; skipping transcript attach"
                );
                return Ok(None);
            };
            let home = std::env::var("HOME")
                .map_err(|e| CoreError::Internal(format!("HOME not set: {e}")))?;
            PathBuf::from(home)
                .join(".claude/projects")
                .join(&expected_slug)
                .join(format!("{session_id}.jsonl"))
        };

        if should_check_transcript_slug(&path) {
            let actual_slug = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str());
            if actual_slug != Some(expected_slug.as_str()) {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    expected_slug = %expected_slug,
                    actual_slug = actual_slug.unwrap_or("<missing>"),
                    source_path = %path.display(),
                    "claude transcript slug mismatch (claude-code may use a different slug rule); exiting source"
                );
                return Ok(None);
            }
        }

        let warn_after = self.options.lazy_retry_attempts;
        let mut warned = false;
        let mut attempt = 0_usize;
        loop {
            if self.stop.is_cancelled() {
                return Ok(None);
            }
            if !self.runtime_is_alive().await {
                tracing::info!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    source_path = %path.display(),
                    "claude runtime reached terminal status before transcript appeared; exiting source"
                );
                return Ok(None);
            }
            match tokio::fs::metadata(&path).await {
                Ok(_) => return Ok(Some(path)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    if !warned && attempt >= warn_after {
                        warned = true;
                        tracing::warn!(
                            card_id = %self.runtime.card_id,
                            runtime_id = %self.runtime.id,
                            source_path = %path.display(),
                            "claude transcript not present after lazy-retry budget; continuing to poll (claude creates file on first prompt)"
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
                Err(err) => return Err(CoreError::Io(err)),
            }
        }
    }

    async fn run_tail(
        &self,
        session: &WorkerSession,
        sink: &dyn WorkerFlowItemSink,
        path: PathBuf,
    ) -> Result<(), CoreError> {
        let source_path = path.to_string_lossy().to_string();
        let mut cursor = cursor::get(
            self.repo.as_ref(),
            &self.runtime.card_id,
            CLAUDE_TRANSCRIPT_SOURCE_KIND,
        )
        .await?
        .filter(|c| c.source_path == source_path)
        .map(|c| CursorState {
            record_index: c.record_index.max(0) as u64,
            byte_offset: c.byte_offset.max(0) as u64,
            last_source_uuid: c.last_source_uuid,
            last_line_hash: c.last_line_hash,
        })
        .unwrap_or_default();
        let (mut position, mut state) = reconstruct_prefix_once(
            &path,
            cursor.byte_offset,
            &source_path,
            session,
        )
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(
                card_id = %self.runtime.card_id,
                runtime_id = %self.runtime.id,
                error = %err,
                "failed to reconstruct claude transcript prefix; starting at cursor seq zero"
            );
            (Position::default(), ClaudeNormalizerState::default())
        });
        let mut cwd_checked = false;

        loop {
            if self.stop.is_cancelled() {
                persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor).await?;
                return Ok(());
            }

            let read = match read_transcript_lines(&path, cursor.byte_offset).await {
                Ok(read) => read,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
                    continue;
                }
                Err(err) => return Err(CoreError::Io(err)),
            };
            if read.offset_reset {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    source_path,
                    byte_offset = cursor.byte_offset,
                    "claude transcript cursor passed EOF; resetting to start"
                );
                cursor = CursorState::default();
                position = Position::default();
                state = ClaudeNormalizerState::default();
            }
            if !read.has_terminator && read.saw_bytes {
                tracing::debug!(
                    source_path,
                    "claude transcript tail has an unterminated final line; deferring it"
                );
            }

            if read.lines.is_empty() {
                persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor).await?;
                if !self.runtime_is_alive().await {
                    tracing::info!(
                        card_id = %self.runtime.card_id,
                        runtime_id = %self.runtime.id,
                        "claude runtime reached terminal status; final drain complete, exiting tail"
                    );
                    return Ok(());
                }
                sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
                continue;
            }

            for line in read.lines {
                if self.stop.is_cancelled() {
                    persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor)
                        .await?;
                    return Ok(());
                }

                let parsed = match parse_line(&line.raw, cursor.record_index, &source_path) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            source_path,
                            line = cursor.record_index,
                            "skipping malformed claude transcript line"
                        );
                        cursor.last_source_uuid = None;
                        cursor.last_line_hash = Some(hash_line(&line.raw));
                        cursor.record_index = cursor.record_index.saturating_add(1);
                        cursor.byte_offset = line.offset_after;
                        continue;
                    }
                };

                if !cwd_checked && let Some(inband_cwd) = record_cwd(&parsed) {
                    cwd_checked = true;
                    let expected = slug_for_projects(&self.card_cwd);
                    let actual = slug_for_projects(inband_cwd);
                    if actual != expected {
                        tracing::warn!(
                            card_id = %self.runtime.card_id,
                            runtime_id = %self.runtime.id,
                            expected_slug = expected,
                            actual_slug = actual,
                            card_cwd = %self.card_cwd,
                            inband_cwd,
                            "claude transcript in-band cwd slug mismatch; continuing after path-time transcript slug matched"
                        );
                    }
                }

                if record_starts_turn(&parsed) {
                    position.turn = position.turn.saturating_add(1);
                }
                let raw_ref = RawRef {
                    provider: WorkerProviderKind::Claude,
                    source_path: Some(source_path.clone()),
                    line: Some(cursor.record_index),
                    record_type: Some(record_type(&parsed)),
                };
                let items = normalize_record_with_state(
                    &parsed,
                    position.seq,
                    position.turn,
                    &session.id,
                    raw_ref,
                    &mut state,
                );
                for item in items {
                    record_with_backpressure(sink, &row_ctx(session, &self.runtime), item).await?;
                    position.seq = position.seq.saturating_add(1);
                }

                cursor.last_source_uuid = source_uuid(&parsed);
                cursor.last_line_hash = Some(hash_line(&line.raw));
                cursor.record_index = cursor.record_index.saturating_add(1);
                cursor.byte_offset = line.offset_after;
                if cursor.record_index % self.options.cursor_persist_every.max(1) == 0 {
                    persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor)
                        .await?;
                }
            }

            persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor).await?;
            if !self.runtime_is_alive().await {
                tracing::info!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    "claude runtime reached terminal status; final drain complete, exiting tail"
                );
                return Ok(());
            }
            sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
        }
    }

    async fn runtime_is_alive(&self) -> bool {
        match self.repo.runtime_get_by_id(&self.runtime.id).await {
            Ok(Some(runtime)) => !matches!(
                runtime.status,
                RunStatus::Exited | RunStatus::Failed | RunStatus::Superseded
            ),
            Ok(None) => true,
            Err(err) => {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    error = %err,
                    "claude runtime liveness lookup failed; keeping tail alive"
                );
                true
            }
        }
    }
}

#[async_trait]
impl WorkerFlowSource for ClaudeTranscriptFlowSource {
    fn provider(&self) -> WorkerProviderKind {
        WorkerProviderKind::Claude
    }

    async fn capture(
        &self,
        session: &WorkerSession,
        sink: &dyn WorkerFlowItemSink,
    ) -> Result<(), CoreError> {
        let Some(path) = self.resolve_transcript_path().await? else {
            tracing::info!(
                card_id = %self.runtime.card_id,
                runtime_id = %self.runtime.id,
                "claude transcript source exiting without resolved transcript path"
            );
            return Ok(());
        };
        self.run_tail(session, sink, path).await
    }
}

#[derive(Default)]
struct CursorState {
    record_index: u64,
    byte_offset: u64,
    last_source_uuid: Option<String>,
    last_line_hash: Option<String>,
}

#[derive(Default)]
struct Position {
    seq: u64,
    turn: u32,
}

fn row_ctx(session: &WorkerSession, runtime: &CardRuntime) -> FlowRowCtx {
    FlowRowCtx {
        session_id: session.id.clone(),
        wave_id: Some(session.wave_id.as_str().to_string()),
        card_id: Some(runtime.card_id.clone()),
    }
}

async fn record_with_backpressure(
    sink: &dyn WorkerFlowItemSink,
    ctx: &FlowRowCtx,
    item: calm_types::worker_flow::WorkerFlowItem,
) -> Result<(), CoreError> {
    loop {
        match sink.record(ctx, item.clone()).await {
            Ok(()) => return Ok(()),
            Err(CoreError::ServiceUnavailable(err)) => {
                tracing::warn!(
                    error = %err,
                    "worker-flow sink backpressure; retrying captured item"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn persist_cursor(
    repo: &dyn Repo,
    card_id: &str,
    source_path: &str,
    cursor: &CursorState,
) -> Result<(), CoreError> {
    cursor::upsert(
        repo,
        card_id,
        CLAUDE_TRANSCRIPT_SOURCE_KIND,
        source_path,
        cursor.record_index as i64,
        cursor.byte_offset as i64,
        cursor.last_source_uuid.as_deref(),
        cursor.last_line_hash.as_deref(),
        now_ms(),
    )
    .await
}

async fn reconstruct_prefix_once(
    path: &Path,
    byte_offset: u64,
    source_path: &str,
    session: &WorkerSession,
) -> Result<(Position, ClaudeNormalizerState), CoreError> {
    let mut state = ClaudeNormalizerState::default();
    if byte_offset == 0 {
        return Ok((Position::default(), state));
    }
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(byte_offset).read_to_end(&mut bytes).await?;
    let read = split_complete_lines(&bytes, 0)?;
    let mut position = Position::default();
    let mut record_index = 0_u64;
    for line in read.lines {
        let Ok(parsed) = parse_line(&line.raw, record_index, source_path) else {
            record_index = record_index.saturating_add(1);
            continue;
        };
        if record_starts_turn(&parsed) {
            position.turn = position.turn.saturating_add(1);
        }
        let raw_ref = RawRef {
            provider: WorkerProviderKind::Claude,
            source_path: Some(source_path.to_string()),
            line: Some(record_index),
            record_type: Some(record_type(&parsed)),
        };
        let items = normalize_record_with_state(
            &parsed,
            position.seq,
            position.turn,
            &session.id,
            raw_ref,
            &mut state,
        );
        position.seq = position.seq.saturating_add(items.len() as u64);
        record_index = record_index.saturating_add(1);
    }
    Ok((position, state))
}

fn should_check_transcript_slug(path: &Path) -> bool {
    let Some(projects_dir) = path.parent().and_then(|slug_dir| slug_dir.parent()) else {
        return false;
    };
    projects_dir.file_name().and_then(|name| name.to_str()) == Some("projects")
        && projects_dir
            .parent()
            .and_then(|claude_dir| claude_dir.file_name())
            .and_then(|name| name.to_str())
            == Some(".claude")
}

fn parse_line(raw: &str, line_index: u64, source_path: &str) -> Result<Value, CoreError> {
    serde_json::from_str(raw).map_err(|e| {
        CoreError::Internal(format!(
            "parse claude transcript line {source_path}:{line_index}: {e}"
        ))
    })
}

async fn sleep_or_cancel(duration: Duration, stop: &CancellationToken) -> Result<(), CoreError> {
    tokio::select! {
        _ = stop.cancelled() => Ok(()),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

struct TranscriptRead {
    lines: Vec<LineRead>,
    has_terminator: bool,
    saw_bytes: bool,
    offset_reset: bool,
}

struct LineRead {
    raw: String,
    offset_after: u64,
}

async fn read_transcript_lines(path: &Path, byte_offset: u64) -> io::Result<TranscriptRead> {
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let (offset, offset_reset) = if byte_offset > len {
        (0, true)
    } else {
        (byte_offset, false)
    };
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let mut read = split_complete_lines(&bytes, offset)?;
    read.offset_reset = offset_reset;
    Ok(read)
}

fn split_complete_lines(bytes: &[u8], base_offset: u64) -> io::Result<TranscriptRead> {
    let has_terminator = bytes.last().is_some_and(|byte| *byte == b'\n');
    let complete_len = if has_terminator {
        bytes.len()
    } else {
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|pos| pos + 1)
            .unwrap_or(0)
    };
    let mut lines = Vec::new();
    let mut start = 0_usize;
    while start < complete_len {
        let Some(relative_end) = bytes[start..complete_len]
            .iter()
            .position(|byte| *byte == b'\n')
        else {
            break;
        };
        let end = start + relative_end;
        let raw_bytes = bytes[start..end]
            .strip_suffix(b"\r")
            .unwrap_or(&bytes[start..end]);
        let raw = String::from_utf8(raw_bytes.to_vec()).map_err(io::Error::other)?;
        lines.push(LineRead {
            raw,
            offset_after: base_offset + end as u64 + 1,
        });
        start = end + 1;
    }
    Ok(TranscriptRead {
        lines,
        has_terminator,
        saw_bytes: !bytes.is_empty(),
        offset_reset: false,
    })
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

/// Mirrors Claude 2.1.170 project-directory slugging; cwd cross-checks catch drift.
pub fn slug_for_projects(cwd: &str) -> String {
    cwd.as_bytes()
        .iter()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => *byte as char,
            _ => '-',
        })
        .collect()
}
