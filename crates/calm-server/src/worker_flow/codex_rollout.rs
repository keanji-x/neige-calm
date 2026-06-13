use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use calm_exec::flow::{FlowRowCtx, WorkerFlowItemSink, WorkerFlowSource};
use calm_types::error::CoreError;
use calm_types::runtime::CardRuntime;
use calm_types::worker::{WorkerProviderKind, WorkerSession};
use calm_types::worker_flow::RawRef;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::db::Repo;
use crate::model::now_ms;
use crate::worker_flow::codex_normalizer::{
    RolloutLine, is_turn_context, normalize_rollout_line, rollout_line_source_uuid,
    rollout_record_type, session_meta_id,
};
use crate::worker_flow::cursor::{self, CODEX_ROLLOUT_SOURCE_KIND};

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_LAZY_RETRY_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_LAZY_RETRY_ATTEMPTS: usize = 30;
const DEFAULT_CURSOR_PERSIST_EVERY: u64 = 20;

#[derive(Clone, Debug)]
pub struct CodexRolloutFlowSourceOptions {
    pub path_override: Option<PathBuf>,
    pub poll_interval: Duration,
    pub lazy_retry_delay: Duration,
    pub lazy_retry_attempts: usize,
    pub cursor_persist_every: u64,
}

impl Default for CodexRolloutFlowSourceOptions {
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

pub struct CodexRolloutFlowSource {
    repo: Arc<dyn Repo>,
    runtime: CardRuntime,
    codex_home: PathBuf,
    stop: CancellationToken,
    options: CodexRolloutFlowSourceOptions,
}

impl CodexRolloutFlowSource {
    pub fn new(
        repo: Arc<dyn Repo>,
        runtime: CardRuntime,
        codex_home: PathBuf,
        stop: CancellationToken,
    ) -> Self {
        Self::new_with_options(
            repo,
            runtime,
            codex_home,
            stop,
            CodexRolloutFlowSourceOptions::default(),
        )
    }

    pub fn new_with_options(
        repo: Arc<dyn Repo>,
        runtime: CardRuntime,
        codex_home: PathBuf,
        stop: CancellationToken,
        options: CodexRolloutFlowSourceOptions,
    ) -> Self {
        Self {
            repo,
            runtime,
            codex_home,
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

        let mut warned = false;
        loop {
            for _ in 0..self.options.lazy_retry_attempts {
                if self.stop.is_cancelled() {
                    return Ok(None);
                }
                match find_thread_path_by_id_str(&self.codex_home, thread_id).await {
                    Ok(Some(path)) => return Ok(Some(path)),
                    Ok(None) => {
                        sleep_or_cancel(self.options.lazy_retry_delay, &self.stop).await?;
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        sleep_or_cancel(self.options.lazy_retry_delay, &self.stop).await?;
                    }
                    Err(err) => return Err(CoreError::Io(err)),
                }
            }
            if !warned {
                warned = true;
                tracing::warn!(
                    card_id = %self.runtime.card_id,
                    runtime_id = %self.runtime.id,
                    thread_id,
                    "codex rollout file not found after lazy-create retry budget; continuing to probe"
                );
            }
            sleep_or_cancel(Duration::from_secs(1), &self.stop).await?;
        }
    }

    async fn run_tail(
        &self,
        session: &WorkerSession,
        sink: &dyn WorkerFlowItemSink,
        path: PathBuf,
    ) -> Result<(), CoreError> {
        let source_path = path.to_string_lossy().to_string();
        loop {
            if self.stop.is_cancelled() {
                return Ok(());
            }

            let mut cursor = cursor::get(
                self.repo.as_ref(),
                &self.runtime.card_id,
                CODEX_ROLLOUT_SOURCE_KIND,
            )
            .await?
            .filter(|c| c.source_path == source_path)
            .map(|c| CursorState {
                record_index: c.record_index.max(0) as u64,
                last_source_uuid: c.last_source_uuid,
            })
            .unwrap_or_default();

            let lines = match read_rollout_lines(&path).await {
                Ok(lines) => lines,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
                    continue;
                }
                Err(err) => return Err(CoreError::Io(err)),
            };

            if lines.is_empty() {
                persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor).await?;
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
                    "codex rollout SessionMeta id mismatch; resetting cursor and re-reading from start"
                );
                cursor.record_index = 0;
                cursor.last_source_uuid = None;
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
                cursor.record_index = 0;
                cursor.last_source_uuid = None;
            }

            let mut position = reconstruct_position(&lines, cursor.record_index, &source_path);
            while (cursor.record_index as usize) < lines.len() {
                if self.stop.is_cancelled() {
                    persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor)
                        .await?;
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
                        cursor.record_index += 1;
                        continue;
                    }
                };

                if is_turn_context(&parsed) {
                    position.turn = position.turn.saturating_add(1);
                    cursor.record_index += 1;
                    continue;
                }

                let raw_ref = RawRef {
                    provider: WorkerProviderKind::Codex,
                    source_path: Some(source_path.clone()),
                    line: Some(line_index),
                    record_type: Some(rollout_record_type(&parsed).to_string()),
                };
                if let Some(item) = normalize_rollout_line(
                    &parsed,
                    position.seq,
                    position.turn,
                    &session.id,
                    raw_ref,
                ) {
                    record_with_backpressure(sink, &row_ctx(session, &self.runtime), item).await?;
                    position.seq = position.seq.saturating_add(1);
                }

                cursor.last_source_uuid = rollout_line_source_uuid(&parsed);
                cursor.record_index += 1;
                if cursor.record_index % self.options.cursor_persist_every.max(1) == 0 {
                    persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor)
                        .await?;
                }
            }

            persist_cursor(&*self.repo, &self.runtime.card_id, &source_path, &cursor).await?;
            sleep_or_cancel(self.options.poll_interval, &self.stop).await?;
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
            return Ok(());
        };
        self.run_tail(session, sink, path).await
    }
}

#[derive(Default)]
struct CursorState {
    record_index: u64,
    last_source_uuid: Option<String>,
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
        CODEX_ROLLOUT_SOURCE_KIND,
        source_path,
        cursor.record_index as i64,
        0,
        cursor.last_source_uuid.as_deref(),
        now_ms(),
    )
    .await
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

async fn read_rollout_lines(path: &Path) -> io::Result<Vec<String>> {
    if path.extension().and_then(|s| s.to_str()) == Some("zst") {
        return read_zstd_lines(path).await;
    }
    let file = tokio::fs::File::open(path).await?;
    let mut lines = BufReader::new(file).lines();
    let mut out = Vec::new();
    while let Some(line) = lines.next_line().await? {
        out.push(line);
    }
    Ok(out)
}

async fn read_zstd_lines(path: &Path) -> io::Result<Vec<String>> {
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
        let text = String::from_utf8(output.stdout).map_err(io::Error::other)?;
        Ok(text.lines().map(str::to_string).collect())
    })
    .await
    .map_err(io::Error::other)?
}

async fn find_thread_path_by_id_str(
    codex_home: &Path,
    thread_id: &str,
) -> io::Result<Option<PathBuf>> {
    let root = codex_home.join("sessions");
    let thread_id = thread_id.to_string();
    tokio::task::spawn_blocking(move || find_thread_path_blocking(&root, &thread_id))
        .await
        .map_err(io::Error::other)?
}

fn find_thread_path_blocking(root: &Path, thread_id: &str) -> io::Result<Option<PathBuf>> {
    if !root.exists() {
        return Ok(None);
    }
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_dir() {
                queue.push_back(path);
                continue;
            }
            if !ty.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let plain_name = name
                .strip_suffix(".zst")
                .unwrap_or(name)
                .strip_suffix(".jsonl")
                .unwrap_or(name);
            if plain_name.starts_with("rollout-") && plain_name.ends_with(thread_id) {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_rollout_file_by_thread_id() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions/2026/06/13");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("rollout-2026-06-13T00-00-00-abc-123.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        let found = find_thread_path_blocking(&dir.path().join("sessions"), "abc-123")
            .unwrap()
            .unwrap();
        assert_eq!(found, path);
    }
}
