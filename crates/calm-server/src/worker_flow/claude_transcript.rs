use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use calm_exec::flow::{FlowRowCtx, WorkerFlowItemSink, WorkerFlowSource};
use calm_types::error::CoreError;
use calm_types::runtime::WorkerSessionProjection;
use calm_types::worker::{WorkerProviderKind, WorkerSession, WorkerSessionState};
use calm_types::worker_flow::RawRef;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::sync::CancellationToken;

use crate::db::Repo;
use crate::worker_flow::claude_normalizer::{
    ClaudeNormalizerState, normalize_record_with_state, record_cwd, record_starts_turn,
    record_type, source_uuid,
};
use crate::worker_flow::claude_transcript_lookup::{
    RuntimeAliveProbe, TranscriptLookup, wait_for_transcript_path,
};
use crate::worker_flow::cursor::{self, CursorWriter};

pub const CLAUDE_TRANSCRIPT_SOURCE_KIND: &str = "claude_transcript";

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_LAZY_RETRY_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_LAZY_RETRY_ATTEMPTS: usize = 30;

#[derive(Clone, Debug)]
pub struct ClaudeTranscriptFlowSourceOptions {
    pub path_override: Option<PathBuf>,
    pub poll_interval: Duration,
    pub lazy_retry_delay: Duration,
    pub lazy_retry_attempts: usize,
}

impl Default for ClaudeTranscriptFlowSourceOptions {
    fn default() -> Self {
        Self {
            path_override: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            lazy_retry_delay: DEFAULT_LAZY_RETRY_DELAY,
            lazy_retry_attempts: DEFAULT_LAZY_RETRY_ATTEMPTS,
        }
    }
}

pub struct ClaudeTranscriptFlowSource {
    repo: Arc<dyn Repo>,
    runtime: WorkerSessionProjection,
    card_cwd: String,
    stop: CancellationToken,
    options: ClaudeTranscriptFlowSourceOptions,
}

impl ClaudeTranscriptFlowSource {
    pub fn new(
        repo: Arc<dyn Repo>,
        runtime: WorkerSessionProjection,
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
        runtime: WorkerSessionProjection,
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

    async fn resolve_transcript_path(
        &self,
        session: &WorkerSession,
    ) -> Result<Option<PathBuf>, CoreError> {
        let mut lookup = if let Some(path) = &self.options.path_override {
            TranscriptLookup::Fixed(path.clone())
        } else {
            let Some(session_id) = self.runtime.session_id.clone() else {
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    "worker-flow claude runtime has no session_id; skipping transcript attach"
                );
                return Ok(None);
            };
            TranscriptLookup::hook(
                self.repo.clone(),
                session.track_id.as_str().to_string(),
                self.runtime.card_id.clone(),
                session_id,
            )
        };
        let mut runtime_alive = RepoRuntimeAlive(self);
        wait_for_transcript_path(
            &mut lookup,
            &self.stop,
            &self.options,
            &self.runtime,
            &mut runtime_alive,
        )
        .await
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
            CLAUDE_TRANSCRIPT_SOURCE_KIND,
        )
        .await?;
        let mut writer = CursorWriter::new(
            &self.runtime.card_id,
            CLAUDE_TRANSCRIPT_SOURCE_KIND,
            &source_path,
            stored.as_ref(),
            self.stop.clone(),
        );
        let mut cursor = stored
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
                return Ok(());
            }

            let read = match read_transcript_lines(&path, cursor.byte_offset, false).await {
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

            let mut lines = read.lines;
            let mut exit_after_batch = false;
            if lines.is_empty() {
                if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                    return Ok(());
                }
                if !self.runtime_is_alive().await {
                    let final_read = match read_transcript_lines(&path, cursor.byte_offset, true)
                        .await
                    {
                        Ok(read) => read,
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            tracing::info!(
                                card_id = %self.runtime.card_id,
                                runtime_id = %self.runtime.id,
                                "claude runtime reached terminal status; final drain complete, exiting tail"
                            );
                            return Ok(());
                        }
                        Err(err) => return Err(CoreError::Io(err)),
                    };
                    if final_read.offset_reset {
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
                    if final_read.invalid_unterminated_tail {
                        tracing::warn!(
                            source_path,
                            "claude transcript terminal drain found invalid unterminated tail; leaving cursor before tail"
                        );
                    }
                    lines = final_read.lines;
                    exit_after_batch = true;
                    if lines.is_empty() {
                        if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                            return Ok(());
                        }
                        tracing::info!(
                            card_id = %self.runtime.card_id,
                            runtime_id = %self.runtime.id,
                            "claude runtime reached terminal status; final drain complete, exiting tail"
                        );
                        return Ok(());
                    }
                } else {
                    sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
                    continue;
                }
            }

            for line in lines {
                if self.stop.is_cancelled() {
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
                        if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                            return Ok(());
                        }
                        continue;
                    }
                };

                if !cwd_checked && let Some(inband_cwd) = record_cwd(&parsed) {
                    cwd_checked = true;
                    if Path::new(inband_cwd) != Path::new(&self.card_cwd) {
                        tracing::warn!(
                            card_id = %self.runtime.card_id,
                            runtime_id = %self.runtime.id,
                            card_cwd = %self.card_cwd,
                            inband_cwd,
                            "claude transcript in-band cwd differs from card cwd; continuing because the hook-reported session path is the identity"
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
                position.seq = position.seq.saturating_add(items.len() as u64);

                cursor.last_source_uuid = source_uuid(&parsed);
                cursor.last_line_hash = Some(hash_line(&line.raw));
                cursor.record_index = cursor.record_index.saturating_add(1);
                cursor.byte_offset = line.offset_after;
                if !persist_cursor(&mut writer, sink, &ctx, &cursor, items).await? {
                    return Ok(());
                }
            }

            if !persist_cursor(&mut writer, sink, &ctx, &cursor, vec![]).await? {
                return Ok(());
            }
            if exit_after_batch {
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
                    "claude runtime liveness lookup failed; keeping tail alive"
                );
                true
            }
        }
    }
}

struct RepoRuntimeAlive<'a>(&'a ClaudeTranscriptFlowSource);

impl RuntimeAliveProbe for RepoRuntimeAlive<'_> {
    fn is_alive<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(self.0.runtime_is_alive())
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
        let Some(path) = self.resolve_transcript_path(session).await? else {
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
            cursor.byte_offset as i64,
            cursor.last_source_uuid.as_deref(),
            cursor.last_line_hash.as_deref(),
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
    let read = split_complete_lines(&bytes, 0, false)?;
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

fn parse_line(raw: &str, line_index: u64, source_path: &str) -> Result<Value, CoreError> {
    serde_json::from_str(raw).map_err(|e| {
        CoreError::Internal(format!(
            "parse claude transcript line {source_path}:{line_index}: {e}"
        ))
    })
}

pub(super) async fn sleep_or_cancel(
    duration: Duration,
    stop: &CancellationToken,
) -> Result<(), CoreError> {
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
    invalid_unterminated_tail: bool,
}

struct LineRead {
    raw: String,
    offset_after: u64,
}

async fn read_transcript_lines(
    path: &Path,
    byte_offset: u64,
    allow_unterminated: bool,
) -> io::Result<TranscriptRead> {
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
    let mut read = split_complete_lines(&bytes, offset, allow_unterminated)?;
    read.offset_reset = offset_reset;
    Ok(read)
}

fn split_complete_lines(
    bytes: &[u8],
    base_offset: u64,
    allow_unterminated: bool,
) -> io::Result<TranscriptRead> {
    let has_terminator = bytes.last().is_some_and(|byte| *byte == b'\n');
    let last_newline = bytes.iter().rposition(|byte| *byte == b'\n');
    let mut invalid_unterminated_tail = false;
    let complete_len = if has_terminator {
        bytes.len()
    } else {
        let terminated_len = last_newline.map(|pos| pos + 1).unwrap_or(0);
        if allow_unterminated {
            let tail = &bytes[terminated_len..];
            if tail.is_empty() {
                terminated_len
            } else if serde_json::from_slice::<Value>(tail).is_ok() {
                bytes.len()
            } else {
                invalid_unterminated_tail = true;
                terminated_len
            }
        } else {
            terminated_len
        }
    };
    let mut lines = Vec::new();
    let mut start = 0_usize;
    while start < complete_len {
        let (end, offset_after) = match bytes[start..complete_len]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            Some(relative_end) => {
                let end = start + relative_end;
                (end, base_offset + end as u64 + 1)
            }
            None if allow_unterminated && complete_len == bytes.len() => {
                (complete_len, base_offset + complete_len as u64)
            }
            None => break,
        };
        let raw_bytes = bytes[start..end]
            .strip_suffix(b"\r")
            .unwrap_or(&bytes[start..end]);
        let raw = String::from_utf8(raw_bytes.to_vec()).map_err(io::Error::other)?;
        lines.push(LineRead { raw, offset_after });
        start = if end < complete_len && bytes[end] == b'\n' {
            end + 1
        } else {
            end
        };
    }
    Ok(TranscriptRead {
        lines,
        has_terminator,
        saw_bytes: !bytes.is_empty(),
        offset_reset: false,
        invalid_unterminated_tail,
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
