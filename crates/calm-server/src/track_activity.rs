//! The `kernel/track/activity` projector: one overlay row per track (`working`, `attention`,
//! `activity_at_ms`), recomputed on every wake-up from durable rows plus one in-process value — the
//! renderer registry's last-output instant of each interactive PTY card; bus events and the
//! [`ActivityWake`] channel are only wake-ups.

pub mod notifications;
pub mod sql;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use crate::db::sqlite::overlay_upsert_tx;
use crate::db::sqlite::track_get_tx;
use crate::db::{Repo, write_with_events_typed};
use crate::error::CalmError;
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use crate::harness::HarnessRegistry;
use crate::ids::{ActorId, TrackId};
use crate::model::NewOverlay;
use crate::state::WriteContext;
use crate::terminal_renderer::TerminalRendererRegistry;
use calm_truth::validation::{KERNEL_OVERLAY_PLUGIN_ID, OVERLAY_ACTIVITY_SCHEMA_VERSION};
pub use notifications::{ActivityItem, NotificationSource};
use notifications::{NotificationRows, notifications};
use sql::{SessionRow, TaskRow, TrackRow};
use tokio::sync::mpsc;

/// The overlay `kind` this projector owns.
pub const ACTIVITY_OVERLAY_KIND: &str = "activity";

/// Reconcile period: the convergence bound for every change that emits no event is this plus one sweep.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// An interactive PTY card is `working` while its PTY is open and its last `Output` frame is younger
/// than this; the attach reader wakes the projector on a frame after at least this much quiet.
/// Output → quiet has no edge and is the tick's.
pub const INTERACTIVE_OUTPUT_WINDOW: Duration = Duration::from_secs(5);

/// `attention` — the fold of `items[]` (a `planner_down` item ⇒ `failed`, else an `ask` ⇒ `input`), kept
/// redundantly so the rail need not scan the items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Attention {
    None,
    Input,
    Failed,
}

/// Per-card conclusion; the fold order is the derived `Ord` (`working < input < failed`).
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

/// The `kernel/track/activity` payload.
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
    /// Field-wise equality of everything but the high-water mark, which the caller compares after taking the max.
    fn same_conclusions(&self, other: &Self) -> bool {
        self.working == other.working
            && self.attention == other.attention
            && self.items == other.items
            && self.cards == other.cards
    }
}

/// Everything one recomputation read, so the fold is a pure function of it.
#[derive(Debug, Clone)]
pub struct TrackRows {
    pub track: TrackRow,
    pub tasks: Vec<TaskRow>,
    pub sessions: Vec<SessionRow>,
    /// Worker session ids the in-process harness registry holds LIVE for this track.
    pub live_harness_sessions: Vec<String>,
    /// The renderer registry's last-output instant by card id, for every interactive PTY card whose
    /// PTY has a live renderer entry that received at least one frame; an in-process witness, never a row.
    pub output: HashMap<String, i64>,
    /// The instant the rows were read, for the output window.
    pub now_ms: i64,
    /// N0–N4 — what the two notification sources read.
    pub notifications: NotificationRows,
}

/// The conclusions of one fold, before the high-water mark is merged in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    pub working: bool,
    pub items: Vec<ActivityItem>,
    pub cards: Vec<CardActivity>,
    /// `MAX(finished_at_ms)` over the current attempts in `done` / `failed` (`canceled` is not evidence).
    pub e3_task_settled: Option<i64>,
    /// The newest last-output instant over the track's interactive PTY cards; folded into the
    /// high-water mark whether or not the card is still `working`, so the quiet after a burst (or the
    /// exit after it) reads as one unread.
    pub e8_interactive_output: Option<i64>,
}

impl Fold {
    pub fn attention(&self) -> Attention {
        self.items
            .iter()
            .map(|i| match i.source() {
                NotificationSource::PlannerDown => Attention::Failed,
                NotificationSource::Ask => Attention::Input,
            })
            .max()
            .unwrap_or(Attention::None)
    }
}

/// A `claude` / `codex` / `terminal` session that is not a harness row and was never bound to a
/// task. Its `working` comes from PTY output alone; a task-bound card's
/// comes from the task clause alone.
pub fn interactive_pty_card(ws: &SessionRow) -> bool {
    ws.mode.as_deref() != Some(calm_types::harness::HARNESS_MODE)
        && !ws.task_bound
        && matches!(ws.provider.as_str(), "claude" | "codex" | "terminal")
}

/// A task-bound worker card whose current attempts are AT LEAST ONE row and ALL `done`, and whose
/// session was minted no later than the last completion, does not turn `state='failed'` into a
/// `failed` card verdict: the exit verdict belongs to finished work. A card with NO current row is NOT suppressed.
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
    let mut cards: BTreeMap<String, CardState> = BTreeMap::new();
    // The cards with `working` evidence, kept apart from the max-collapsed `cards` slots: a `failed` / `input`
    // verdict out-ranks `working` in the slot, and the terminal-phase filter needs the working evidence back once those go.
    let mut working_cards: BTreeSet<String> = BTreeSet::new();
    let mut e3: Option<i64> = None;
    let mut e8: Option<i64> = None;
    let output_window_ms = INTERACTIVE_OUTPUT_WINDOW.as_millis() as i64;

    fn raise(cards: &mut BTreeMap<String, CardState>, card_id: &str, state: CardState) {
        let slot = cards.entry(card_id.to_string()).or_insert(state);
        if state > *slot {
            *slot = state;
        }
    }
    fn raise_working(
        cards: &mut BTreeMap<String, CardState>,
        working_cards: &mut BTreeSet<String>,
        card_id: &str,
    ) {
        working_cards.insert(card_id.to_string());
        raise(cards, card_id, CardState::Working);
    }

    // W — the task clause.
    for t in &rows.tasks {
        let is_child_track = t.child_track_id.is_some();
        match t.status.as_str() {
            "dispatched" | "running" if !is_child_track => {
                working = true;
                if let Some(wc) = &t.worker_card_id {
                    raise_working(&mut cards, &mut working_cards, wc);
                }
            }
            // A sub-track row in flight: the worker is another track, whose own overlay tells the truth.
            "dispatched" | "running" => {}
            // `verifying` is the PARENT's own gate run, child or not.
            "verifying" => {
                working = true;
                if let Some(wc) = &t.worker_card_id {
                    raise_working(&mut cards, &mut working_cards, wc);
                }
            }
            // A failed attempt is a card verdict, not a notification: the Planner handles it.
            "failed" => {
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

    // S — the per-backend session rules.
    for ws in &rows.sessions {
        let harness = ws.mode.as_deref() == Some(calm_types::harness::HARNESS_MODE);
        let pty_backed = matches!(ws.provider.as_str(), "codex" | "claude" | "terminal");
        if harness {
            // (i) harness codex — planner + assistant.
            if ws.state == "turn_pending" && rows.live_harness_sessions.contains(&ws.id) {
                working = true;
                raise_working(&mut cards, &mut working_cards, &ws.card_id);
            }
        } else if pty_backed && interactive_pty_card(ws) {
            // (ii) any PTY-backed card: working iff its PTY is open (the registry keeps the last
            // stamp after the reader's `Exited` arm, so the exit gate is the row) and it printed
            // within the window. No thread status, no hook, no `input` state.
            let last_output = rows.output.get(&ws.card_id).copied();
            e8 = e8.max(last_output);
            if ws.pty_open && last_output.is_some_and(|at| rows.now_ms - at < output_window_ms) {
                working = true;
                raise_working(&mut cards, &mut working_cards, &ws.card_id);
            }
        }
        // `failed` ⇔ `ws.state = 'failed'` (the exit writer's verdict on an ephemeral session, the
        // reaper's on a harness one): a card verdict, not a notification. A signal-killed
        // codex TUI leaves its resumable row `running` and is NOT failed. Unknown providers: nothing.
        if (harness || pty_backed)
            && ws.state == "failed"
            && !failed_session_is_finished_work(ws, &rows.tasks)
        {
            raise(&mut cards, &ws.card_id, CardState::Failed);
        }
    }

    // The notification items, newest first then by key so the stored payload compares byte-stable.
    let items = notifications(track_id, &rows.notifications);

    // Closed-track filter, `cards[]` only: on a closed track the per-card `input` / `failed`
    // verdicts go; `working` stays (the sweeper ends it) and `cards` is rebuilt from the working evidence.
    // The items are not filtered: an open ask or planner down is still addressed to the user.
    if rows.track.closed_at.is_some() {
        cards = working_cards
            .iter()
            .map(|card_id| (card_id.clone(), CardState::Working))
            .collect();
    }

    let cards = cards
        .into_iter()
        .map(|(card_id, state)| CardActivity { card_id, state })
        .collect();

    Fold {
        working,
        items,
        cards,
        e3_task_settled: e3,
        e8_interactive_output: e8,
    }
}

/// The in-process wake-up for a write that emits no event: a Dismiss (#1829) sends its track id and
/// the projector recomputes that track. Best effort: a send nobody receives (no projector runs) is
/// dropped, and the 30 s tick still converges.
#[derive(Debug, Clone)]
pub struct ActivityWake(mpsc::UnboundedSender<String>);

impl ActivityWake {
    /// A wake no projector receives: the repo is not sqlite-backed, or the state was built without
    /// a projector. Every send is dropped.
    pub fn detached() -> Self {
        Self(mpsc::unbounded_channel().0)
    }

    pub fn wake(&self, track_id: &str) {
        let _ = self.0.send(track_id.to_string());
    }
}

pub struct TrackActivityProjector {
    repo: Arc<dyn Repo>,
    pool: sqlx::SqlitePool,
    bus: EventBus,
    write: WriteContext,
    harness: HarnessRegistry,
    /// The in-process carrier of every interactive PTY card's last output; built before the
    /// projector is spawned.
    renderer: Arc<TerminalRendererRegistry>,
    /// The sending half of [`Self::wake`]'s channel; held so the receive arm never sees it closed.
    wake: ActivityWake,
    wake_rx: mpsc::UnboundedReceiver<String>,
}

/// What one recomputation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recompute {
    /// The track row is gone — before the reads or between the reads and the write; nothing written, nothing emitted.
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

/// What the write transaction found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    TrackGone,
}

impl TrackActivityProjector {
    /// Fails only when the repo is not sqlite-backed — the projector reads the tables directly.
    pub fn new(
        repo: Arc<dyn Repo>,
        bus: EventBus,
        write: WriteContext,
        harness: HarnessRegistry,
        renderer: Arc<TerminalRendererRegistry>,
    ) -> Option<Self> {
        let pool = repo.sqlite_pool()?;
        let (wake_tx, wake_rx) = mpsc::unbounded_channel();
        Some(Self {
            repo,
            pool,
            bus,
            write,
            harness,
            renderer,
            wake: ActivityWake(wake_tx),
            wake_rx,
        })
    }

    /// The handle a writer that emits no event uses to wake this projector's loop.
    pub fn wake(&self) -> ActivityWake {
        self.wake.clone()
    }

    /// Read every input of one track: the durable rows plus the two in-process witnesses (live harness
    /// handles, interactive PTY cards' last output). `None` when the track row is gone.
    pub async fn read_rows(&self, track_id: &str) -> crate::error::Result<Option<TrackRows>> {
        let Some(track) = sql::track_row(&self.pool, track_id).await? else {
            return Ok(None);
        };
        let tasks = sql::current_tasks(&self.pool, track_id).await?;
        let sessions = sql::eligible_sessions(&self.pool, track_id).await?;
        let notifications = sql::notification_rows(&self.pool, track_id).await?;
        let live_harness_sessions = self
            .harness
            .live_for_track(&TrackId::from(track_id.to_string()))
            .into_iter()
            .map(|(worker_session_id, _)| worker_session_id)
            .collect();
        let output = sessions
            .iter()
            .filter(|ws| interactive_pty_card(ws))
            .filter_map(|ws| {
                let terminal_id = ws.terminal_run_id.as_deref()?;
                let at = self.renderer.last_output_ms(terminal_id)?;
                Some((ws.card_id.clone(), at))
            })
            .collect();
        Ok(Some(TrackRows {
            track,
            tasks,
            sessions,
            live_harness_sessions,
            output,
            now_ms: crate::model::now_ms(),
            notifications,
        }))
    }

    /// Recompute one track from its durable rows and write the overlay only if it changed.
    pub async fn recompute_track(&self, track_id: &str) -> crate::error::Result<Recompute> {
        let Some(rows) = self.read_rows(track_id).await? else {
            return Ok(Recompute::NoTrack);
        };
        let folded = fold(track_id, &rows);
        let evidence = sql::evidence(&self.pool, track_id).await?;
        let stored = sql::existing_activity_payload(&self.pool, track_id).await?;
        // The high-water mark is read from the raw JSON, independently of the struct parse: a payload
        // another binary version wrote must not re-seed the mark and light a spurious unread.
        let stored_mark = stored
            .as_ref()
            .and_then(|v| v.get("activity_at_ms"))
            .and_then(Value::as_i64);
        let existing: Option<ActivityPayload> = match stored {
            None => None,
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!(
                        track_id = %track_id,
                        error = %e,
                        "track_activity: stored payload does not parse; conclusions \
                         recomputed, high-water mark kept from the raw row"
                    );
                    None
                }
            },
        };

        // `activity_at_ms` is a monotone high-water mark: max of the stored value, every persisted
        // completion-class witness and the in-process last-output instant (what a crash loses is the
        // part not yet folded); never lowered by a reconcile.
        let activity_at_ms = [
            stored_mark,
            evidence.max(),
            folded.e3_task_settled,
            folded.e8_interactive_output,
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
        match self.write_overlay(track_id, &next).await? {
            WriteOutcome::Written => Ok(Recompute::Written(next)),
            WriteOutcome::TrackGone => Ok(Recompute::NoTrack),
        }
    }

    /// ONE IMMEDIATE transaction that re-reads the track row, upserts the overlay and appends the
    /// `overlay.set` event. A track deleted since the reads aborts with no row and no event: the table
    /// has no FK and the reconcile enumerates live tracks only, so an orphan row would be permanent.
    pub async fn write_overlay(
        &self,
        track_id: &str,
        payload: &ActivityPayload,
    ) -> crate::error::Result<WriteOutcome> {
        let new_overlay = NewOverlay {
            plugin_id: KERNEL_OVERLAY_PLUGIN_ID.to_string(),
            entity_kind: "track".to_string(),
            entity_id: track_id.to_string(),
            kind: ACTIVITY_OVERLAY_KIND.to_string(),
            payload: serde_json::to_value(payload)?,
        };
        let track_gone = Arc::new(AtomicBool::new(false));
        let gone_in_tx = Arc::clone(&track_gone);
        let track = TrackId::from(track_id.to_string());
        let result = write_with_events_typed(
            self.repo.as_ref(),
            ActorId::Kernel,
            None,
            &self.bus,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let row = match track_get_tx(tx, &track).await {
                        Ok(row) => row,
                        Err(CalmError::NotFound(m)) => {
                            gone_in_tx.store(true, Ordering::SeqCst);
                            return Err(CalmError::NotFound(m));
                        }
                        Err(e) => return Err(e),
                    };
                    let o = overlay_upsert_tx(tx, new_overlay).await?;
                    let scope = EventScope::Track {
                        track: row.id,
                        area: row.area_id,
                    };
                    Ok(((), vec![(scope, Event::OverlaySet(o))]))
                })
            },
        )
        .await;
        match result {
            Ok(_) => Ok(WriteOutcome::Written),
            Err(_) if track_gone.load(Ordering::SeqCst) => {
                tracing::debug!(
                    track_id = %track_id,
                    "track_activity: track deleted between the reads and the write; \
                     overlay not written"
                );
                Ok(WriteOutcome::TrackGone)
            }
            Err(e) => Err(e),
        }
    }

    /// The boot sweep and the 30 s tick: every unarchived track, serially.
    pub async fn reconcile_all(&self) {
        let ids = match sql::track_ids(&self.pool).await {
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

    /// Which track a bus event wakes; `None` for everything else. The projector's own `overlay.set`
    /// row must not wake it; the two PTY edges arrive on the registry's wake channel, not on the bus.
    pub async fn track_for_event(&self, env: &BroadcastEnvelope) -> Option<String> {
        match &env.event {
            Event::HarnessPhaseChanged { track_id, .. } => Some(track_id.as_str().to_string()),
            // Every turn end: a codex system error persists its failed turn row AFTER the phase
            // event, so only this event carries it.
            Event::HarnessItemAdded {
                track_id, method, ..
            } if method == "turn/completed" => Some(track_id.as_str().to_string()),
            // A user's reply to the Planner closes its asks.
            Event::HarnessUserMessageEnqueued { track_id, .. } => {
                Some(track_id.as_str().to_string())
            }
            Event::WorkerSessionStarted { card_id, .. }
            | Event::WorkerSessionStatusChanged { card_id, .. }
            | Event::WorkerSessionSuperseded { card_id, .. } => self.card_track(card_id).await,
            Event::TaskDispatched { .. }
            | Event::TaskCompleted { .. }
            | Event::TaskFailed { .. }
            | Event::TaskGateResult { .. } => env.scope.track_id().map(|t| t.as_str().to_string()),
            // A question opens an ask; its answer or its withdrawal closes it.
            Event::AskRequested { track_id, .. }
            | Event::AskAnswered { track_id, .. }
            | Event::AskWithdrawn { track_id, .. } => Some(track_id.as_str().to_string()),
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

    /// Which track a PTY edge wakes: the terminal's card's track, `None` when the terminal row (or
    /// its card) is gone.
    async fn track_for_terminal(&self, terminal_id: &str) -> Option<String> {
        match self.repo.terminal_get(terminal_id).await {
            Ok(Some(terminal)) => self.card_track(terminal.card_id.as_str()).await,
            _ => None,
        }
    }

    /// The projector loop: a boot sweep, then bus wake-ups, the two PTY edges, the [`ActivityWake`]
    /// sends and the tick in one `select!` (serial; both wake channels are unbounded FIFOs, so a
    /// wake-up that lands during a recomputation queues instead of being lost).
    pub async fn run(mut self) {
        let mut rx = self.bus.subscribe();
        // This clone stays alive for the loop's lifetime so the receive arm can never observe a
        // closed channel.
        let (wake_tx, mut wake_rx) = mpsc::unbounded_channel::<String>();
        self.renderer.set_output_wake(wake_tx.clone());
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
                Some(terminal_id) = wake_rx.recv() => {
                    if let Some(track_id) = self.track_for_terminal(&terminal_id).await
                        && let Err(e) = self.recompute_track(&track_id).await
                    {
                        tracing::warn!(
                            track_id = %track_id,
                            terminal_id = %terminal_id,
                            error = %e,
                            "track_activity: PTY-edge recompute failed"
                        );
                    }
                }
                Some(track_id) = self.wake_rx.recv() => {
                    if let Err(e) = self.recompute_track(&track_id).await {
                        tracing::warn!(
                            track_id = %track_id,
                            error = %e,
                            "track_activity: in-process wake recompute failed"
                        );
                    }
                }
            }
        }
    }
}

/// Spawn the projector task and return its [`ActivityWake`]. At the boot sweep the harness registry
/// is still empty (run loops are installed by `boot_harnesses` later), so harness rows read
/// `working=false` on the first pass, and the renderer registry is empty until a card's WS
/// reattaches its PTY.
pub fn spawn(
    repo: Arc<dyn Repo>,
    bus: EventBus,
    write: WriteContext,
    harness: HarnessRegistry,
    renderer: Arc<TerminalRendererRegistry>,
) -> ActivityWake {
    let Some(projector) = TrackActivityProjector::new(repo, bus, write, harness, renderer) else {
        tracing::warn!("track_activity: repo is not sqlite-backed; projector not started");
        return ActivityWake::detached();
    };
    let wake = projector.wake();
    tokio::spawn(projector.run());
    wake
}
