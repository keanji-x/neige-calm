//! The background resolver: `enqueue` from a reader, one serial lane per plugin, `resolve` on the drain side.
//! The in-flight key is `(track, block)`; check and insert are one `HashSet::insert` under the lock, and the guard travels with the job so the key is released however the job ends. `enqueue` never waits on the plugin.
//! Negative route / scope outcomes never store a row; only permanent per-block outcomes store an `unavailable` row, without calling the plugin.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::db::write_in_tx_typed;
use crate::mcp_server::registry::AppContext;
use crate::mcp_server::tool_visibility::{TrackPluginScope, plugin_scope_for_track};
use crate::mcp_server::transport::{ToolEntry, plugin_tool_entry};
use crate::plugin_host::ConnectorClient;

use super::admission::admit;
use super::request::{Mode, SeriesRequest};
#[cfg(any(test, feature = "fixtures"))]
use super::seams::{Failpoints, Recorder};
use super::store::{NewRow, upsert_row_tx};
use super::summary::summarize;
use super::validate::validate_reply;
use super::{MAX_REASON_CHARS, SERIES_RESOLVE_TIMEOUT};

/// `(track_id, block_id)`.
pub type Key = (String, String);

type InflightSet = Arc<StdMutex<HashSet<Key>>>;

/// Holds one in-flight key; releases it on drop.
pub struct InflightGuard {
    set: InflightSet,
    key: Key,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

/// A queued resolution. Carries no request: the drain side derives it from
/// the block's payload at dequeue time.
pub struct Job {
    ctx: Arc<AppContext>,
    track_id: String,
    block_id: String,
    /// The plugin whose lane this job was queued on; a block re-pointed at a
    /// different source while queued is dropped and re-queued by its next
    /// read.
    lane_plugin: String,
    _guard: InflightGuard,
}

impl Job {
    pub fn track_id(&self) -> &str {
        &self.track_id
    }
    pub fn block_id(&self) -> &str {
        &self.block_id
    }
}

struct Lane {
    tx: mpsc::UnboundedSender<Job>,
    drain: JoinHandle<()>,
    /// Jobs sent and not yet received by the drain task.
    queued: Arc<AtomicUsize>,
}

impl Lane {
    /// `Err(job)` hands the job back when the drain task is gone.
    fn send(&self, job: Job) -> Result<(), Box<Job>> {
        self.queued.fetch_add(1, Ordering::SeqCst);
        match self.tx.send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::error::SendError(job)) => {
                self.queued.fetch_sub(1, Ordering::SeqCst);
                Err(Box::new(job))
            }
        }
    }
}

/// What a reader learns from `enqueue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enqueue {
    /// A job was queued on the plugin's lane (or recorded, in unstarted mode).
    Queued,
    /// The key was already in flight: a job is queued or running for this
    /// block.
    InFlight,
    /// Route pre-check or scope refused; nothing was queued, nothing stored.
    /// The reason is the `pending` reason for this read only.
    Miss(String),
}

/// How one `resolve` ended. Tests read it; production logs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// Admission, route, scope or client refused; no row was written.
    Dropped(String),
    /// A row was written (or the write was refused by a pinned row) with
    /// this status.
    Wrote { status: String, pinned: bool },
    /// The write itself failed (FK after a track delete, IO, failpoint).
    WriteFailed(String),
}

pub struct SeriesResolver {
    pool: Option<SqlitePool>,
    inflight: InflightSet,
    lanes: StdMutex<HashMap<String, Lane>>,
    resolve_timeout: Duration,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    #[cfg(any(test, feature = "fixtures"))]
    recorder: Option<Recorder>,
    #[cfg(any(test, feature = "fixtures"))]
    pub failpoints: Failpoints,
}

impl SeriesResolver {
    /// Production resolver: lanes are spawned on first use. `None` pool makes every job drop with a warning and every read answer `pending`.
    pub fn new(pool: Option<SqlitePool>) -> Self {
        Self {
            pool,
            inflight: Arc::new(StdMutex::new(HashSet::new())),
            lanes: StdMutex::new(HashMap::new()),
            resolve_timeout: SERIES_RESOLVE_TIMEOUT,
            now: Arc::new(calm_truth::model::now_ms),
            #[cfg(any(test, feature = "fixtures"))]
            recorder: None,
            #[cfg(any(test, feature = "fixtures"))]
            failpoints: Failpoints::default(),
        }
    }

    /// Test seam: same in-flight insert and pre-check, but records the job instead of spawning a lane.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn new_unstarted(pool: Option<SqlitePool>) -> Self {
        Self {
            recorder: Some(Recorder::default()),
            ..Self::new(pool)
        }
    }

    pub fn with_resolve_timeout(mut self, timeout: Duration) -> Self {
        self.resolve_timeout = timeout;
        self
    }

    pub fn with_now(mut self, now: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        self.now = now;
        self
    }

    pub fn now_ms(&self) -> i64 {
        (self.now)()
    }

    pub fn pool(&self) -> Option<&SqlitePool> {
        self.pool.as_ref()
    }

    pub fn resolve_timeout(&self) -> Duration {
        self.resolve_timeout
    }

    /// Queued jobs + running jobs (each lane runs at most one).
    pub fn inflight_len(&self) -> usize {
        self.inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn recorded_enqueue_calls(&self) -> usize {
        self.recorder
            .as_ref()
            .map(|r| r.enqueue_calls.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn recorded_outcomes(&self) -> Vec<Enqueue> {
        self.recorder
            .as_ref()
            .map(|r| {
                r.outcomes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
            .unwrap_or_default()
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn take_recorded_jobs(&self) -> Vec<Job> {
        self.recorder
            .as_ref()
            .map(|r| std::mem::take(&mut *r.jobs.lock().unwrap_or_else(PoisonError::into_inner)))
            .unwrap_or_default()
    }

    /// `true` iff the `lanes` lock could be taken right now; the witness that a rebuild holds the lock.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn lanes_try_lock(&self) -> bool {
        self.lanes.try_lock().is_ok()
    }

    /// Queued (not yet dequeued) jobs on a plugin's lane.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn lane_queue_len(&self, plugin_id: &str) -> usize {
        self.lanes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(plugin_id)
            .map(|lane| lane.queued.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// `Some(true)` once the plugin's drain task has ended.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn lane_is_finished(&self, plugin_id: &str) -> Option<bool> {
        self.lanes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(plugin_id)
            .map(|lane| lane.drain.is_finished())
    }

    /// Reader entry point: resolve the track's plugin scope, then `enqueue_scoped`.
    pub async fn enqueue(
        &self,
        ctx: &Arc<AppContext>,
        track_id: &str,
        block_id: &str,
        request: &SeriesRequest,
    ) -> Enqueue {
        let scope = plugin_scope_for_track(ctx, Some(track_id)).await;
        self.enqueue_scoped(ctx, track_id, block_id, request, &scope)
            .await
    }

    pub(crate) async fn enqueue_scoped(
        &self,
        ctx: &Arc<AppContext>,
        track_id: &str,
        block_id: &str,
        request: &SeriesRequest,
        scope: &TrackPluginScope,
    ) -> Enqueue {
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(recorder) = &self.recorder {
            recorder.enqueue_calls.fetch_add(1, Ordering::SeqCst);
        }
        let outcome = self
            .enqueue_inner(ctx, track_id, block_id, request, scope)
            .await;
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(recorder) = &self.recorder {
            recorder
                .outcomes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(outcome.clone());
        }
        outcome
    }

    async fn enqueue_inner(
        &self,
        ctx: &Arc<AppContext>,
        track_id: &str,
        block_id: &str,
        request: &SeriesRequest,
        scope: &TrackPluginScope,
    ) -> Enqueue {
        let key: Key = (track_id.to_string(), block_id.to_string());
        // Check-and-insert is ONE call under the lock: two readers racing on
        // the same block cannot both pass.
        let inserted = self
            .inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone());
        if !inserted {
            return Enqueue::InFlight;
        }
        let guard = InflightGuard {
            set: self.inflight.clone(),
            key,
        };

        #[cfg(any(test, feature = "fixtures"))]
        if self
            .failpoints
            .hold_in_precheck
            .swap(false, Ordering::SeqCst)
        {
            self.failpoints
                .precheck_entered
                .fetch_add(1, Ordering::SeqCst);
            self.failpoints.precheck_release.notified().await;
        }

        // Route pre-check + scope. A miss drops `guard` on return, releasing
        // the key; nothing is spawned or stored.
        let Some(plugin_host) = ctx.plugin_host.get().cloned() else {
            return Enqueue::Miss(format!("plugin {} is not installed", request.plugin_id));
        };
        let running = plugin_host.running_plugin_ids().await;
        let entry = plugin_tool_entry(
            plugin_host.registry(),
            &running,
            &request.plugin_id,
            &request.tool,
        );
        if let Some(reason) = entry.miss_reason(&request.plugin_id, &request.tool) {
            return Enqueue::Miss(reason);
        }
        if *scope == TrackPluginScope::None {
            return Enqueue::Miss("track owner plugin unavailable".to_string());
        }

        let job = Job {
            ctx: ctx.clone(),
            track_id: track_id.to_string(),
            block_id: block_id.to_string(),
            lane_plugin: request.plugin_id.clone(),
            _guard: guard,
        };

        #[cfg(any(test, feature = "fixtures"))]
        if let Some(recorder) = &self.recorder {
            recorder
                .jobs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(job);
            return Enqueue::Queued;
        }

        self.submit(job);
        Enqueue::Queued
    }

    /// Check-rebuild-send under the `lanes` lock, no `.await` inside.
    fn submit(&self, job: Job) {
        let plugin_id = job.lane_plugin.clone();
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        let dead = lanes
            .get(&plugin_id)
            .is_some_and(|lane| lane.drain.is_finished());
        if dead {
            self.rebuild_lane(&mut lanes, &plugin_id);
        }
        let lane = lanes
            .entry(plugin_id.clone())
            .or_insert_with(|| self.spawn_lane(&plugin_id));
        if let Err(job) = lane.send(job) {
            let lane = self.rebuild_lane(&mut lanes, &plugin_id);
            if lane.send(*job).is_err() {
                tracing::warn!(
                    plugin_id,
                    "report_series: lane refused a job twice; dropping (next read re-queues)"
                );
            }
        }
    }

    /// Replace a dead lane. Takes the guard, not the map: it can only be called by whoever holds the `lanes` lock.
    fn rebuild_lane<'a>(
        &self,
        lanes: &'a mut MutexGuard<'_, HashMap<String, Lane>>,
        plugin_id: &str,
    ) -> &'a mut Lane {
        #[cfg(any(test, feature = "fixtures"))]
        {
            self.failpoints
                .rebuild_entered
                .fetch_add(1, Ordering::SeqCst);
            if self
                .failpoints
                .hold_in_rebuild
                .swap(false, Ordering::SeqCst)
            {
                let mut released = self
                    .failpoints
                    .rebuild_released
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                while !*released {
                    released = self
                        .failpoints
                        .rebuild_condvar
                        .wait(released)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
        tracing::warn!(plugin_id, "report_series: rebuilding a dead lane");
        let lane = self.spawn_lane(plugin_id);
        lanes.insert(plugin_id.to_string(), lane);
        lanes
            .get_mut(plugin_id)
            .expect("lane was just inserted under this lock")
    }

    fn spawn_lane(&self, plugin_id: &str) -> Lane {
        #[cfg(any(test, feature = "fixtures"))]
        self.failpoints.drain_spawned.fetch_add(1, Ordering::SeqCst);
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let plugin_id = plugin_id.to_string();
        let queued = Arc::new(AtomicUsize::new(0));
        let drain_queued = queued.clone();
        let drain = tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                drain_queued.fetch_sub(1, Ordering::SeqCst);
                let resolver = job.ctx.series_resolver.clone();
                let outcome = resolver.resolve(job).await;
                tracing::debug!(plugin_id, ?outcome, "report_series: job resolved");
            }
        });
        Lane { tx, drain, queued }
    }

    /// Run one job to completion: admission → route → scope → client → call
    /// under timeout → checklist → write. The job's guard is released when
    /// this function returns, whichever way.
    pub async fn resolve(&self, job: Job) -> ResolveOutcome {
        #[cfg(any(test, feature = "fixtures"))]
        if self
            .failpoints
            .panic_drain_once
            .swap(false, Ordering::SeqCst)
        {
            panic!("report_series: panic_drain_once failpoint");
        }
        let Job {
            ctx,
            track_id,
            block_id,
            lane_plugin,
            _guard,
        } = job;
        let now = (self.now)();

        // 1. Admission.
        let Some(pool) = self.pool.as_ref() else {
            tracing::warn!(
                track_id,
                block_id,
                "report_series: no sqlite pool; dropping job without a row"
            );
            return ResolveOutcome::Dropped("no sqlite pool".to_string());
        };
        let (request, window) = match admit(pool, &track_id, &block_id, &lane_plugin, now).await {
            Ok(Ok(admitted)) => admitted,
            Ok(Err(reason)) => return ResolveOutcome::Dropped(reason),
            Err(error) => {
                tracing::warn!(
                    track_id,
                    block_id,
                    error = %error,
                    "report_series: admission read failed; dropping job"
                );
                return ResolveOutcome::Dropped(format!("admission read failed: {error}"));
            }
        };
        let as_of = window.as_of_text();

        // 2. Route (again — the plugin may have stopped while queued).
        let Some(plugin_host) = ctx.plugin_host.get().cloned() else {
            return ResolveOutcome::Dropped("no plugin host".to_string());
        };
        let running = plugin_host.running_plugin_ids().await;
        let entry = match plugin_tool_entry(
            plugin_host.registry(),
            &running,
            &request.plugin_id,
            &request.tool,
        ) {
            ToolEntry::Found(entry) => entry,
            negative => {
                let reason = negative
                    .miss_reason(&request.plugin_id, &request.tool)
                    .unwrap_or_default();
                return ResolveOutcome::Dropped(reason);
            }
        };
        let read_only = entry
            .annotations
            .as_ref()
            .and_then(|a| a.get("readOnlyHint"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        if entry.kind.is_some() || !read_only {
            return self
                .write_unavailable(
                    &ctx,
                    &track_id,
                    &block_id,
                    &request.request_hash,
                    &as_of,
                    "tool is not an ordinary read-only tool",
                )
                .await;
        }

        // 3. Scope.
        match plugin_scope_for_track(&ctx, Some(&track_id)).await {
            TrackPluginScope::None => {
                return ResolveOutcome::Dropped("track owner plugin unavailable".to_string());
            }
            TrackPluginScope::Only(owner) if owner != request.plugin_id => {
                let reason = format!(
                    "plugin {} is outside this track's plugin scope",
                    request.plugin_id
                );
                return self
                    .write_unavailable(
                        &ctx,
                        &track_id,
                        &block_id,
                        &request.request_hash,
                        &as_of,
                        &reason,
                    )
                    .await;
            }
            TrackPluginScope::All | TrackPluginScope::Only(_) => {}
        }

        // 4. Client. Only local variants are series sources: a remote connector must not be driven by document content.
        let Some(client) = plugin_host.connector_client(&request.plugin_id).await else {
            return ResolveOutcome::Dropped(format!("plugin {} is not running", request.plugin_id));
        };
        if matches!(client, ConnectorClient::Http(_)) {
            return self
                .write_unavailable(
                    &ctx,
                    &track_id,
                    &block_id,
                    &request.request_hash,
                    &as_of,
                    "remote connectors are not series sources",
                )
                .await;
        }
        let deadline_ms = now + self.resolve_timeout.as_millis() as i64;
        let arguments = request.tool_arguments(&window, deadline_ms);
        let call = async {
            match &client {
                ConnectorClient::Stdio(c) => {
                    c.tools_call(&request.tool, arguments, Some(&track_id))
                        .await
                }
                ConnectorClient::Cli(c) => c.tools_call(&request.tool, arguments).await,
                ConnectorClient::Http(_) => Err(crate::plugin_host::mcp::RpcError::internal(
                    "remote connectors are not series sources",
                )),
            }
        };

        // 5. Call under timeout. A timeout drops the call future, and with it
        // the `McpClient` responder slot.
        let verdict = match tokio::time::timeout(self.resolve_timeout, call).await {
            Err(_) => Err(format!(
                "plugin call timed out after {} ms",
                self.resolve_timeout.as_millis()
            )),
            Ok(Err(rpc)) => Err(format!("plugin call failed: {}", rpc.message)),
            // 6. Checklist.
            Ok(Ok(result)) => validate_reply(&result, &request, &window),
        };

        // 7. Write.
        let resolved_at = (self.now)();
        let row = match verdict {
            Ok(reply) => {
                let summary = summarize(&reply, &request.fields);
                let pinned =
                    request.mode() == Mode::Frozen && reply.all_complete_past(window.as_of);
                NewRow {
                    status: "ok".to_string(),
                    reason: None,
                    as_of,
                    resolved_at,
                    pinned,
                    summary: Some(serde_json::to_value(summary).unwrap_or_default()),
                    data: Some(reply.data_json()),
                }
            }
            Err(reason) => NewRow {
                status: "unavailable".to_string(),
                reason: Some(cap_reason(&reason)),
                as_of,
                resolved_at,
                pinned: false,
                summary: None,
                data: None,
            },
        };
        self.write_row(&ctx, &track_id, &block_id, &request.request_hash, row)
            .await
    }

    async fn write_unavailable(
        &self,
        ctx: &Arc<AppContext>,
        track_id: &str,
        block_id: &str,
        request_hash: &str,
        as_of: &str,
        reason: &str,
    ) -> ResolveOutcome {
        let row = NewRow {
            status: "unavailable".to_string(),
            reason: Some(cap_reason(reason)),
            as_of: as_of.to_string(),
            resolved_at: (self.now)(),
            pinned: false,
            summary: None,
            data: None,
        };
        self.write_row(ctx, track_id, block_id, request_hash, row)
            .await
    }

    async fn write_row(
        &self,
        ctx: &Arc<AppContext>,
        track_id: &str,
        block_id: &str,
        request_hash: &str,
        row: NewRow,
    ) -> ResolveOutcome {
        #[cfg(any(test, feature = "fixtures"))]
        if self
            .failpoints
            .fail_write_once
            .swap(false, Ordering::SeqCst)
        {
            tracing::warn!(
                track_id,
                block_id,
                "report_series: fail_write_once failpoint"
            );
            return ResolveOutcome::WriteFailed("fail_write_once failpoint".to_string());
        }
        #[cfg(any(test, feature = "fixtures"))]
        if self
            .failpoints
            .hold_before_write
            .swap(false, Ordering::SeqCst)
        {
            self.failpoints.write_held.fetch_add(1, Ordering::SeqCst);
            self.failpoints.write_release.notified().await;
        }
        let status = row.status.clone();
        let pinned = row.pinned;
        let key = (
            track_id.to_string(),
            block_id.to_string(),
            request_hash.to_string(),
        );
        let written = write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
            Box::pin(async move { upsert_row_tx(tx, &key.0, &key.1, &key.2, &row).await })
        })
        .await;
        match written {
            Ok(affected) => {
                if affected == 0 {
                    tracing::debug!(
                        track_id,
                        block_id,
                        "report_series: row is pinned; write refused by the DB"
                    );
                }
                ResolveOutcome::Wrote { status, pinned }
            }
            Err(error) => {
                tracing::warn!(
                    track_id,
                    block_id,
                    error = %error,
                    "report_series: row write failed; key released, next read re-queues"
                );
                ResolveOutcome::WriteFailed(error.to_string())
            }
        }
    }
}

/// Truncate a reason to `MAX_REASON_CHARS` characters before storing it.
pub fn cap_reason(reason: &str) -> String {
    reason.chars().take(MAX_REASON_CHARS).collect()
}
