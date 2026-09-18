//! #1722 §4 — the `kernel/track/activity` projector.
//!
//! One overlay row per track, `Overlay { plugin_id: "kernel", entity_kind:
//! "track", kind: "activity" }`, answering three questions the rail, the
//! Today page and the track page paint from ONE place: is anything moving
//! right now (`working`), does it need a person (`attention`: `input` or
//! `failed`), and when did the last completion-class result land
//! (`activity_at_ms`, a monotone high-water mark the read receipt is
//! compared against). `items[]` lists every attention source, `cards[]` the
//! per-card conclusion the card rows paint.
//!
//! Everything is recomputed from DURABLE rows on every wake-up (design
//! §4.2/§4.3): the current task attempts (the ONLY `working` source for
//! dispatched work — a shared-daemon thread rests at
//! `last_thread_status='active'` after its turn, F2.29, and the isolated
//! executor's `running` never clears, F2.33), the eligible sessions of the
//! track's cards (harness / current-attempt worker / never-task-bound
//! interactive card), the `kernel/card/status` FSM rows behind a LIVE
//! eligible session, and the track's lifecycle. Bus events are only
//! wake-ups: no time is taken from an event, and a 30 s tick over every
//! unarchived track repairs whatever a lost event, a lag or a crash left
//! behind (session exits and feeder stamps emit no events at all,
//! F2.25/F2.31/F2.32).
//!
//! Transaction shape (design §4.4, `deferred_write_tx_invariant`): every
//! read is an autocommit single statement (`track_activity::sql`); the
//! computed payload is compared with the stored one and only a CHANGE is
//! written, through `write_with_event_typed` (one IMMEDIATE transaction,
//! `Event::OverlaySet`).

pub mod sql;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use crate::db::sqlite::overlay_upsert_tx;
use crate::db::{Repo, write_with_event_typed};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use crate::harness::HarnessRegistry;
use crate::ids::{ActorId, TrackId};
use crate::model::NewOverlay;
use crate::state::WriteContext;
use calm_truth::validation::{KERNEL_OVERLAY_PLUGIN_ID, OVERLAY_ACTIVITY_SCHEMA_VERSION};
use sql::{CardStatusRow, SessionRow, TaskRow, TrackRow};

/// The overlay `kind` this projector owns.
pub const ACTIVITY_OVERLAY_KIND: &str = "activity";

/// Reconcile period (design §4.3): the convergence bound for every change
/// that emits no event is this plus one sweep.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Payload (design §4.1) — serialized field order is the wire order.
// ---------------------------------------------------------------------------

/// `attention` — the fold of `items[]` (`failed > input > none`), kept
/// redundantly so the rail need not scan the items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Attention {
    None,
    Input,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Input,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemSource {
    Card,
    Task,
    Session,
    Lifecycle,
}

/// One attention source. `id` is the card id / task key / session id /
/// track id by `source`; `card_id` is always present for `card` and
/// `session` items, the worker card (nullable) for `task` items, `null` for
/// `lifecycle`. `at_ms` is taken from the column the evidence lives in
/// (§4.1) so the sidebar's newest-first order is honest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ActivityItem {
    pub kind: ItemKind,
    pub source: ItemSource,
    pub id: String,
    pub card_id: Option<String>,
    pub at_ms: i64,
}

/// Per-card conclusion; the fold order is the derived `Ord`
/// (`working < input < failed`, §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardState {
    Working,
    Input,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardActivity {
    pub card_id: String,
    pub state: CardState,
}

/// The `kernel/track/activity` payload (design §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPayload {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub working: bool,
    pub attention: Attention,
    pub activity_at_ms: Option<i64>,
    pub items: Vec<ActivityItem>,
    pub cards: Vec<CardActivity>,
}

impl ActivityPayload {
    /// Field-wise equality of everything but the high-water mark, which the
    /// caller compares after taking the max (§4.4).
    fn same_conclusions(&self, other: &Self) -> bool {
        self.working == other.working
            && self.attention == other.attention
            && self.items == other.items
            && self.cards == other.cards
    }
}

// ---------------------------------------------------------------------------
// The pure fold (design §4.2) over the rows `sql` read.
// ---------------------------------------------------------------------------

/// Everything one recomputation read, in one place so the fold is a pure
/// function of it (and so a test can hand it rows directly).
#[derive(Debug, Clone)]
pub struct TrackRows {
    pub track: TrackRow,
    pub tasks: Vec<TaskRow>,
    pub sessions: Vec<SessionRow>,
    pub card_status: HashMap<String, CardStatusRow>,
    /// Worker session ids the in-process harness registry holds LIVE for
    /// this track — backend (i)'s `working` witness (§4.2 table).
    pub live_harness_sessions: Vec<String>,
}

/// The conclusions of one fold, before the high-water mark is merged in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    pub working: bool,
    pub items: Vec<ActivityItem>,
    pub cards: Vec<CardActivity>,
    /// E3 — `MAX(finished_at_ms)` over the current attempts in `done` /
    /// `failed` (the user can only cancel, so `canceled` is not evidence).
    pub e3_task_settled: Option<i64>,
}

impl Fold {
    pub fn attention(&self) -> Attention {
        self.items
            .iter()
            .map(|i| match i.kind {
                ItemKind::Failed => Attention::Failed,
                ItemKind::Input => Attention::Input,
            })
            .max()
            .unwrap_or(Attention::None)
    }
}

const LIVE_STATES: [&str; 3] = ["starting", "running", "turn_pending"];

fn is_live(state: &str) -> bool {
    LIVE_STATES.contains(&state)
}

/// The † exception of design §4.2: a task-bound worker card whose current
/// attempts are AT LEAST ONE row and ALL `done`, and whose session was
/// minted no later than the last of those completions
/// (`ws.created_at_ms <= MAX(finished_at_ms)`), does not turn its
/// `state='failed'` into a `failed` item — the exit verdict belongs to
/// finished work (a signal-killed PTY, F2.31; a reaper exit after a
/// race-lost CAS). A session minted AFTER the completion is new work
/// (restart, F2.40) and stays red; a card with NO current row is NOT
/// suppressed (the empty set is not "all done" — v7 A-MIN3).
fn failed_session_is_finished_work(session: &SessionRow, tasks: &[TaskRow]) -> bool {
    let rows: Vec<&TaskRow> = tasks
        .iter()
        .filter(|t| t.worker_card_id.as_deref() == Some(session.card_id.as_str()))
        .collect();
    if rows.is_empty() || !rows.iter().all(|t| t.status == "done") {
        return false;
    }
    rows.iter()
        .filter_map(|t| t.finished_at_ms)
        .max()
        .is_some_and(|last_done| session.created_at_ms <= last_done)
}

pub fn fold(track_id: &str, rows: &TrackRows) -> Fold {
    let mut working = false;
    let mut items: Vec<ActivityItem> = Vec::new();
    let mut cards: BTreeMap<String, CardState> = BTreeMap::new();
    let mut e3: Option<i64> = None;

    fn raise(cards: &mut BTreeMap<String, CardState>, card_id: &str, state: CardState) {
        let slot = cards.entry(card_id.to_string()).or_insert(state);
        if state > *slot {
            *slot = state;
        }
    }

    // W — the task clause (§4.2 W, F2.22/F2.38/F2.39).
    for t in &rows.tasks {
        let is_child_track = t.child_track_id.is_some();
        match t.status.as_str() {
            "dispatched" | "running" if !is_child_track => {
                working = true;
                if let Some(wc) = &t.worker_card_id {
                    raise(&mut cards, wc, CardState::Working);
                }
            }
            // A sub-track row in flight: the worker is another track, whose
            // own overlay tells the truth (G20). Not working here.
            "dispatched" | "running" => {}
            // `verifying` is the PARENT's own gate run, child or not (F2.39).
            "verifying" => {
                working = true;
                if let Some(wc) = &t.worker_card_id {
                    raise(&mut cards, wc, CardState::Working);
                }
            }
            "failed" => {
                items.push(ActivityItem {
                    kind: ItemKind::Failed,
                    source: ItemSource::Task,
                    id: t.key.clone(),
                    card_id: t.worker_card_id.clone(),
                    at_ms: t.finished_at_ms.unwrap_or(t.updated_at_ms),
                });
                if let Some(wc) = &t.worker_card_id {
                    raise(&mut cards, wc, CardState::Failed);
                }
                e3 = e3.max(t.finished_at_ms);
            }
            "done" => {
                e3 = e3.max(t.finished_at_ms);
            }
            _ => {}
        }
    }

    // S — the per-backend session rules over the S0 result set (§4.2 table).
    for ws in &rows.sessions {
        let live = is_live(&ws.state);
        let thread_status = ws.last_thread_status.as_deref();
        let harness = ws.mode.as_deref() == Some(calm_types::harness::HARNESS_MODE);
        let session_failed =
            ws.state == "failed" && !failed_session_is_finished_work(ws, &rows.tasks);
        let session_item = |kind: ItemKind, at_ms: i64| ActivityItem {
            kind,
            source: ItemSource::Session,
            id: ws.id.clone(),
            card_id: Some(ws.card_id.clone()),
            at_ms,
        };
        // A `last_thread_status` item carries the feeder's stamp time; a
        // `state='failed'` item carries the exit writer's `updated_at_ms`
        // (§4.1, v8 A-MIN2).
        let stamp_at = ws.last_activity_ms.unwrap_or(ws.updated_at_ms);

        if harness {
            // (i) harness codex — planner + assistant.
            if ws.state == "turn_pending" && rows.live_harness_sessions.contains(&ws.id) {
                working = true;
                raise(&mut cards, &ws.card_id, CardState::Working);
            }
            if session_failed {
                items.push(session_item(ItemKind::Failed, ws.updated_at_ms));
                raise(&mut cards, &ws.card_id, CardState::Failed);
            }
            continue;
        }

        match ws.provider.as_str() {
            "codex" if ws.isolated => {
                // (iii) isolated executor — `working` only through W.
                if session_failed {
                    items.push(session_item(ItemKind::Failed, ws.updated_at_ms));
                    raise(&mut cards, &ws.card_id, CardState::Failed);
                }
            }
            "codex" => {
                // (ii) shared-daemon thread — interactive `codex-create`
                // card or `codex-worker`.
                if !ws.task_bound
                    && matches!(ws.state.as_str(), "running" | "turn_pending")
                    && thread_status == Some("active")
                {
                    working = true;
                    raise(&mut cards, &ws.card_id, CardState::Working);
                }
                if live
                    && matches!(
                        thread_status,
                        Some("waitingOnApproval" | "waitingOnUserInput")
                    )
                {
                    items.push(session_item(ItemKind::Input, stamp_at));
                    raise(&mut cards, &ws.card_id, CardState::Input);
                }
                if live && thread_status == Some("systemError") {
                    items.push(session_item(ItemKind::Failed, stamp_at));
                    raise(&mut cards, &ws.card_id, CardState::Failed);
                }
                if session_failed {
                    items.push(session_item(ItemKind::Failed, ws.updated_at_ms));
                    raise(&mut cards, &ws.card_id, CardState::Failed);
                }
            }
            "claude" => {
                // (iv) claude PTY — the FSM row counts only behind THIS live
                // session (the live-session gate).
                let fsm = rows.card_status.get(&ws.card_id);
                let fsm_state = fsm.map(|r| r.state.as_str());
                if !ws.task_bound && live && fsm_state == Some("Working") {
                    working = true;
                    raise(&mut cards, &ws.card_id, CardState::Working);
                }
                if let Some(row) = fsm.filter(|_| live) {
                    let card_item = |kind: ItemKind| ActivityItem {
                        kind,
                        source: ItemSource::Card,
                        id: ws.card_id.clone(),
                        card_id: Some(ws.card_id.clone()),
                        at_ms: row.updated_at,
                    };
                    match row.state.as_str() {
                        "AwaitingInput" => {
                            items.push(card_item(ItemKind::Input));
                            raise(&mut cards, &ws.card_id, CardState::Input);
                        }
                        "Errored" => {
                            items.push(card_item(ItemKind::Failed));
                            raise(&mut cards, &ws.card_id, CardState::Failed);
                        }
                        _ => {}
                    }
                }
                if session_failed {
                    items.push(session_item(ItemKind::Failed, ws.updated_at_ms));
                    raise(&mut cards, &ws.card_id, CardState::Failed);
                }
            }
            // (v) terminal — never from session signals (no turn concept,
            // hooks do not enter the FSM, F2.13). Unknown providers: nothing.
            _ => {}
        }
    }

    // Lifecycle (§4.2, last line).
    let lifecycle_item = |kind: ItemKind| ActivityItem {
        kind,
        source: ItemSource::Lifecycle,
        id: track_id.to_string(),
        card_id: None,
        at_ms: rows.track.updated_at,
    };
    match rows.track.lifecycle.as_str() {
        "blocked" | "reviewing" => items.push(lifecycle_item(ItemKind::Input)),
        "failed" => items.push(lifecycle_item(ItemKind::Failed)),
        _ => {}
    }

    // Deterministic order so the stored payload compares byte-stable:
    // newest first, then by source / id.
    items.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.cmp(b)));
    items.dedup();
    let cards = cards
        .into_iter()
        .map(|(card_id, state)| CardActivity { card_id, state })
        .collect();

    Fold {
        working,
        items,
        cards,
        e3_task_settled: e3,
    }
}

// ---------------------------------------------------------------------------
// The projector: reads, folds, compares, writes on change.
// ---------------------------------------------------------------------------

pub struct TrackActivityProjector {
    repo: Arc<dyn Repo>,
    pool: sqlx::SqlitePool,
    bus: EventBus,
    write: WriteContext,
    harness: HarnessRegistry,
}

/// What one recomputation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recompute {
    /// The track row is gone (deleted); nothing to project.
    NoTrack,
    /// The stored payload already said this; no write, no event.
    Unchanged(ActivityPayload),
    /// Written (and `overlay.set` emitted).
    Written(ActivityPayload),
}

impl Recompute {
    pub fn payload(&self) -> Option<&ActivityPayload> {
        match self {
            Recompute::NoTrack => None,
            Recompute::Unchanged(p) | Recompute::Written(p) => Some(p),
        }
    }
}

impl TrackActivityProjector {
    /// Fails only when the repo is not sqlite-backed — the projector reads
    /// the tables directly (design §4.4: autocommit statements, no
    /// transaction).
    pub fn new(
        repo: Arc<dyn Repo>,
        bus: EventBus,
        write: WriteContext,
        harness: HarnessRegistry,
    ) -> Option<Self> {
        let pool = repo.sqlite_pool()?;
        Some(Self {
            repo,
            pool,
            bus,
            write,
            harness,
        })
    }

    /// Read every durable input of one track. `None` when the track row is
    /// gone.
    pub async fn read_rows(&self, track_id: &str) -> crate::error::Result<Option<TrackRows>> {
        let Some(track) = sql::track_row(&self.pool, track_id).await? else {
            return Ok(None);
        };
        let tasks = sql::current_tasks(&self.pool, track_id).await?;
        let sessions = sql::eligible_sessions(&self.pool, track_id).await?;
        let card_status = sql::card_status_overlays(&self.pool, track_id).await?;
        let live_harness_sessions = self
            .harness
            .live_for_track(&TrackId::from(track_id.to_string()))
            .into_iter()
            .map(|(worker_session_id, _)| worker_session_id)
            .collect();
        Ok(Some(TrackRows {
            track,
            tasks,
            sessions,
            card_status,
            live_harness_sessions,
        }))
    }

    /// Recompute one track from its durable rows and write the overlay if
    /// (and only if) it changed. Errors are returned, not swallowed; the
    /// loop logs them and moves on.
    pub async fn recompute_track(&self, track_id: &str) -> crate::error::Result<Recompute> {
        let Some(rows) = self.read_rows(track_id).await? else {
            return Ok(Recompute::NoTrack);
        };
        let folded = fold(track_id, &rows);
        let evidence = sql::evidence(&self.pool, track_id).await?;
        let existing: Option<ActivityPayload> =
            sql::existing_activity_payload(&self.pool, track_id)
                .await?
                .and_then(|v| serde_json::from_value(v).ok());

        // `activity_at_ms` is a monotone high-water mark (M10): max of the
        // stored value and every persisted completion-class witness; seeded
        // on the first row, never lowered by a reconcile.
        let activity_at_ms = [
            existing.as_ref().and_then(|e| e.activity_at_ms),
            evidence.max(),
            folded.e3_task_settled,
        ]
        .into_iter()
        .flatten()
        .max();

        let next = ActivityPayload {
            schema_version: OVERLAY_ACTIVITY_SCHEMA_VERSION,
            working: folded.working,
            attention: folded.attention(),
            activity_at_ms,
            items: folded.items,
            cards: folded.cards,
        };
        if let Some(prev) = &existing
            && prev.schema_version == next.schema_version
            && prev.same_conclusions(&next)
            && prev.activity_at_ms == next.activity_at_ms
        {
            return Ok(Recompute::Unchanged(next));
        }
        self.write_overlay(track_id, &next).await?;
        Ok(Recompute::Written(next))
    }

    async fn write_overlay(
        &self,
        track_id: &str,
        payload: &ActivityPayload,
    ) -> crate::error::Result<()> {
        // Resolve the area for the event scope; on lookup failure fall back
        // to `System` rather than refuse the write (same posture as
        // `card_fsm`).
        let scope = match self.repo.track_get(track_id).await {
            Ok(Some(t)) => EventScope::Track {
                track: t.id,
                area: t.area_id,
            },
            _ => EventScope::System,
        };
        let new_overlay = NewOverlay {
            plugin_id: KERNEL_OVERLAY_PLUGIN_ID.to_string(),
            entity_kind: "track".to_string(),
            entity_id: track_id.to_string(),
            kind: ACTIVITY_OVERLAY_KIND.to_string(),
            payload: serde_json::to_value(payload)?,
        };
        write_with_event_typed(
            self.repo.as_ref(),
            ActorId::Kernel,
            scope,
            None,
            &self.bus,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let o = overlay_upsert_tx(tx, new_overlay).await?;
                    Ok(((), Event::OverlaySet(o)))
                })
            },
        )
        .await?;
        Ok(())
    }

    /// The boot sweep and the 30 s tick: every unarchived track, serially.
    pub async fn reconcile_all(&self) {
        let ids = match sql::unarchived_track_ids(&self.pool).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "track_activity: track enumeration failed");
                return;
            }
        };
        for id in ids {
            if let Err(e) = self.recompute_track(&id).await {
                tracing::warn!(track_id = %id, error = %e, "track_activity: reconcile failed");
            }
        }
    }

    /// Which track a bus event wakes (design §4.3 wake-up table); `None`
    /// for everything else. Only `overlay.set` needs a lookup, and only when
    /// the FSM's commit degraded its scope to `System`.
    pub async fn track_for_event(&self, env: &BroadcastEnvelope) -> Option<String> {
        match &env.event {
            Event::OverlaySet(o)
                if o.plugin_id == KERNEL_OVERLAY_PLUGIN_ID
                    && o.entity_kind == "card"
                    && o.kind == "status" =>
            {
                if let Some(track) = env.scope.track_id() {
                    return Some(track.as_str().to_string());
                }
                self.card_track(&o.entity_id).await
            }
            Event::HarnessPhaseChanged { track_id, .. } => Some(track_id.as_str().to_string()),
            Event::HarnessItemAdded {
                track_id,
                item_type,
                ..
            } if item_type.as_deref() == Some("mcpToolCall") => Some(track_id.as_str().to_string()),
            Event::WorkerSessionStarted { card_id, .. }
            | Event::WorkerSessionStatusChanged { card_id, .. }
            | Event::WorkerSessionSuperseded { card_id, .. } => self.card_track(card_id).await,
            Event::TaskDispatched { .. }
            | Event::TaskCompleted { .. }
            | Event::TaskFailed { .. }
            | Event::TaskExecutionSettled { .. }
            | Event::TaskGateResult { .. } => env.scope.track_id().map(|t| t.as_str().to_string()),
            Event::TrackLifecycleChanged { id, .. } => Some(id.as_str().to_string()),
            Event::TrackReportEdited { track_id, .. } => Some(track_id.as_str().to_string()),
            Event::TrackUpdated(payload) => Some(payload.track.id.as_str().to_string()),
            _ => None,
        }
    }

    async fn card_track(&self, card_id: &str) -> Option<String> {
        match self.repo.card_get(card_id).await {
            Ok(Some(card)) => Some(card.track_id.as_str().to_string()),
            _ => None,
        }
    }

    /// The projector loop: a boot sweep, then bus wake-ups and the tick in
    /// one `select!` (serial — a wake-up and a sweep never interleave).
    pub async fn run(self) {
        let mut rx = self.bus.subscribe();
        let mut tick = tokio::time::interval(RECONCILE_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    // The first tick completes immediately: the boot sweep.
                    self.reconcile_all().await;
                }
                received = rx.recv() => match received {
                    Ok(env) => {
                        if let Some(track_id) = self.track_for_event(&env).await
                            && let Err(e) = self.recompute_track(&track_id).await
                        {
                            tracing::warn!(
                                track_id = %track_id,
                                error = %e,
                                "track_activity: event-driven recompute failed"
                            );
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "track_activity event subscriber lagged");
                    }
                    Err(RecvError::Closed) => break,
                },
            }
        }
    }
}

/// Spawn the projector task. Called from `AppState::new` right after
/// `HarnessRegistry::new()` — the registry is an `Arc` clone, so the task
/// does not wait for `AppState`; at the boot sweep it is still empty (run
/// loops are installed by `boot_harnesses` later), so harness rows read
/// `working=false` on the first pass (design §4.2, §9 G15).
pub fn spawn(repo: Arc<dyn Repo>, bus: EventBus, write: WriteContext, harness: HarnessRegistry) {
    let Some(projector) = TrackActivityProjector::new(repo, bus, write, harness) else {
        tracing::warn!("track_activity: repo is not sqlite-backed; projector not started");
        return;
    };
    tokio::spawn(projector.run());
}
