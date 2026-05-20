//! Per-card FSM + wave-union state machine.
//!
//! A long-running background task subscribes to `EventBus` and projects
//! incoming events onto a per-card 6-state FSM:
//!
//!   `Starting / Idle / Working / AwaitingInput / Errored / Done`
//!
//! Whenever a card's state changes, the task:
//!   1. Writes a kernel-owned `Overlay { plugin_id="kernel", entity_kind="card",
//!      entity_id=<card_id>, kind="status", payload={ state } }`.
//!   2. Recomputes the owning wave's union state (most-severe of its cards'
//!      states) and writes a wave-level overlay `{ state, counts }`.
//!
//! ## Scope (phase 1)
//!
//! Only **codex cards** participate. Two-line rationale:
//!   - **Terminal cards** have no event surface that maps cleanly onto the
//!     six FSM states; that needs a new daemon-side event and is deferred to
//!     phase 2.
//!   - **Plugin cards** can't be driven directly from `Event::PluginState`
//!     because `plugin_id` is independent of `card_id` — a plugin may back
//!     zero or many cards. Driving plugin-card FSM correctly needs either
//!     callback-level signal or a registry walk; also phase 2.
//!
//! Cards that don't have an entry in the FSM map don't contribute to wave
//! union (they're treated as "not tracked", i.e. they don't drag the wave
//! out of Idle).
//!
//! ## Throttle / debounce
//!
//! State changes are filtered through a tiny debouncer:
//!   - **Upgrades** (more severe) emit immediately.
//!   - **Downgrades** (less severe) are held for `DOWNGRADE_QUIET_MS`. If a
//!     new event lands inside the window, the timer resets (or the upgrade
//!     fires through immediately, replacing the pending downgrade).
//!
//! ## In-memory only
//!
//! The map is `Mutex<HashMap<card_id, State>>`. On restart it starts empty;
//! the first hook event for each codex card re-populates it. Persisted state
//! lives on the overlay rows the FSM writes, so the UI is correct as soon as
//! cards re-attach.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};

use crate::db::Repo;
use crate::event::{Event, EventBus};
use crate::model::NewOverlay;

/// Plugin id stamped on the FSM-authored overlays. The kernel uses a
/// reserved `"kernel"` namespace — plugins can't write under it (their
/// callback path requires `plugin_id == self.id`), so these rows are
/// unambiguously kernel-owned.
const KERNEL_PLUGIN_ID: &str = "kernel";

/// How long to hold a downgrade (less-severe → more-calm) before committing
/// it. Upgrades are emitted immediately. Matches design doc §"Throttle".
const DOWNGRADE_QUIET_MS: u64 = 750;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Starting,
    Idle,
    Working,
    AwaitingInput,
    Errored,
    Done,
}

impl State {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Idle => "Idle",
            Self::Working => "Working",
            Self::AwaitingInput => "AwaitingInput",
            Self::Errored => "Errored",
            Self::Done => "Done",
        }
    }

    /// Severity ordering used both for "is this an upgrade?" and for the
    /// wave-union pick.
    ///
    /// `AwaitingInput > Errored > Working > Starting > Idle > Done`
    fn severity(self) -> u8 {
        match self {
            Self::AwaitingInput => 5,
            Self::Errored => 4,
            Self::Working => 3,
            Self::Starting => 2,
            Self::Idle => 1,
            Self::Done => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Codex hook → State projection
// ---------------------------------------------------------------------------

/// Project a codex hook `kind` (e.g. `hook.codex.pre_tool_use`) onto the FSM
/// transition target. Returns `None` for hooks we don't model — the FSM
/// leaves the card's state alone in that case.
///
/// `Stop` → `AwaitingInput` (not `Idle`): when codex's agent loop stops it
/// is genuinely waiting for the next user prompt, so the user is the
/// bottleneck and the wave-union should surface that.  `PostToolUse` stays
/// `Idle` — the agent loop is still active between tool calls and the next
/// `PreToolUse` typically follows within ms.
fn codex_kind_to_state(kind: &str) -> Option<State> {
    match kind {
        "hook.codex.session_start" => Some(State::Starting),
        "hook.codex.user_prompt_submit" | "hook.codex.pre_tool_use" => Some(State::Working),
        "hook.codex.post_tool_use" => Some(State::Idle),
        "hook.codex.stop" => Some(State::AwaitingInput),
        "hook.codex.permission_request" => Some(State::AwaitingInput),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Background task entry point
// ---------------------------------------------------------------------------

/// Spawn the FSM task. Subscribes to `bus`, owns its own state map.
pub fn spawn(repo: Arc<dyn Repo>, bus: EventBus) {
    let mut rx = bus.subscribe();
    let bus_clone = bus.clone();
    tokio::spawn(async move {
        let inner = Arc::new(Inner::new(repo, bus_clone));
        loop {
            match rx.recv().await {
                Ok(ev) => inner.handle(ev).await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "card_fsm event subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Inner — shared mutable state
// ---------------------------------------------------------------------------

struct Inner {
    repo: Arc<dyn Repo>,
    bus: EventBus,
    /// `card_id → (committed_state, pending_downgrade_deadline)`.
    ///
    /// `pending_downgrade_deadline` is `Some(deadline)` only when a downgrade
    /// is being held. A landing upgrade clears it.
    map: Mutex<HashMap<String, CardEntry>>,
}

#[derive(Debug, Clone)]
struct CardEntry {
    /// What we've already written as the card's overlay.
    committed: State,
    /// What we'd commit if the quiet window expires. `None` means there's
    /// no pending downgrade.
    pending: Option<PendingDowngrade>,
}

#[derive(Debug, Clone)]
struct PendingDowngrade {
    target: State,
    deadline: Instant,
}

impl Inner {
    fn new(repo: Arc<dyn Repo>, bus: EventBus) -> Self {
        Self {
            repo,
            bus,
            map: Mutex::new(HashMap::new()),
        }
    }

    async fn handle(self: &Arc<Self>, ev: Event) {
        // Only codex hooks drive FSM in phase 1.
        if let Event::CodexHook { card_id, kind, .. } = ev {
            let Some(target) = codex_kind_to_state(&kind) else {
                return;
            };
            self.observe(card_id, target).await;
        }
    }

    /// Mutating entry: a new event says "card X wants to be in state Y now".
    /// Decides upgrade-vs-downgrade and (synchronously) commits or schedules.
    async fn observe(self: &Arc<Self>, card_id: String, target: State) {
        let mut map = self.map.lock().await;
        // Distinguish "first observation of this card" from "already-tracked
        // card landing in its current state again". The first observation
        // MUST commit (we have no prior overlay row to derive state from),
        // even if it happens to be `Idle`.
        let (cur, first_observation) = match map.get(&card_id) {
            Some(e) => (e.committed, false),
            None => (State::Done, true), // placeholder; severity-floor so anything is an upgrade
        };

        if first_observation || target.severity() >= cur.severity() {
            // Upgrade, same, or first observation: commit immediately. Drop
            // any pending downgrade.
            let changed = first_observation || target != cur;
            map.insert(
                card_id.clone(),
                CardEntry {
                    committed: target,
                    pending: None,
                },
            );
            drop(map);
            if changed {
                self.commit(&card_id, target).await;
            }
        } else {
            // Downgrade: schedule.
            let deadline = Instant::now() + Duration::from_millis(DOWNGRADE_QUIET_MS);
            if let Some(entry) = map.get_mut(&card_id) {
                entry.pending = Some(PendingDowngrade { target, deadline });
            }
            drop(map);
            self.clone().schedule_downgrade(card_id, deadline);
        }
    }

    fn schedule_downgrade(self: Arc<Self>, card_id: String, deadline: Instant) {
        tokio::spawn(async move {
            sleep_until(deadline).await;
            // Re-read state at fire time: the pending may have been replaced
            // (upgrade landed, or a newer downgrade pushed the deadline).
            let mut map = self.map.lock().await;
            let Some(entry) = map.get_mut(&card_id) else {
                return;
            };
            let Some(pending) = entry.pending.clone() else {
                return; // upgrade cleared it
            };
            if pending.deadline > Instant::now() {
                // A newer downgrade pushed the deadline further out. The
                // newer spawn will handle it.
                return;
            }
            let target = pending.target;
            if target == entry.committed {
                entry.pending = None;
                return;
            }
            entry.committed = target;
            entry.pending = None;
            drop(map);
            self.commit(&card_id, target).await;
        });
    }

    /// Commit a card state change: write the card-level overlay AND recompute
    /// + write the wave-level union overlay. Emits `Event::OverlaySet` for
    /// both so the WS bridge invalidates the right queries.
    async fn commit(&self, card_id: &str, state: State) {
        // Look up the owning wave; we need it for the union step.
        let card = match self.repo.card_get(card_id).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::debug!(card_id, "card_fsm: card vanished mid-commit, skipping");
                return;
            }
            Err(e) => {
                tracing::warn!(card_id, error = %e, "card_fsm: card_get failed");
                return;
            }
        };

        // 1. Card overlay.
        let card_payload = json!({ "state": state.wire_name() });
        match self
            .repo
            .overlay_upsert(NewOverlay {
                plugin_id: KERNEL_PLUGIN_ID.to_string(),
                entity_kind: "card".to_string(),
                entity_id: card_id.to_string(),
                kind: "status".to_string(),
                payload: card_payload,
            })
            .await
        {
            Ok(o) => self.bus.emit(Event::OverlaySet(o)),
            Err(e) => {
                tracing::warn!(card_id, error = %e, "card_fsm: card overlay_upsert failed");
                return;
            }
        }

        // 2. Wave union.
        self.recompute_wave(&card.wave_id).await;
    }

    /// Recompute the wave's union state by walking every card in the wave's
    /// FSM map entry (we only know about tracked cards). Writes a wave-level
    /// overlay `{ state, counts }`.
    async fn recompute_wave(&self, wave_id: &str) {
        // Find every card in this wave so we can intersect with the FSM map.
        let cards = match self.repo.cards_by_wave(wave_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(wave_id, error = %e, "card_fsm: cards_by_wave failed");
                return;
            }
        };

        let map = self.map.lock().await;
        let mut union: Option<State> = None;
        let mut working = 0u32;
        let mut awaiting = 0u32;
        let mut errored = 0u32;
        for card in &cards {
            let Some(entry) = map.get(&card.id) else {
                continue;
            };
            let s = entry.committed;
            match s {
                State::Working => working += 1,
                State::AwaitingInput => awaiting += 1,
                State::Errored => errored += 1,
                _ => {}
            }
            union = Some(match union {
                None => s,
                Some(cur) if s.severity() > cur.severity() => s,
                Some(cur) => cur,
            });
        }
        drop(map);

        let final_state = union.unwrap_or(State::Idle);
        let payload = json!({
            "state": final_state.wire_name(),
            "counts": {
                "working": working,
                "awaiting": awaiting,
                "errored": errored,
            }
        });

        match self
            .repo
            .overlay_upsert(NewOverlay {
                plugin_id: KERNEL_PLUGIN_ID.to_string(),
                entity_kind: "wave".to_string(),
                entity_id: wave_id.to_string(),
                kind: "status".to_string(),
                payload,
            })
            .await
        {
            Ok(o) => self.bus.emit(Event::OverlaySet(o)),
            Err(e) => {
                tracing::warn!(wave_id, error = %e, "card_fsm: wave overlay_upsert failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_kind_mapping() {
        assert_eq!(
            codex_kind_to_state("hook.codex.session_start"),
            Some(State::Starting)
        );
        assert_eq!(
            codex_kind_to_state("hook.codex.user_prompt_submit"),
            Some(State::Working)
        );
        assert_eq!(
            codex_kind_to_state("hook.codex.pre_tool_use"),
            Some(State::Working)
        );
        assert_eq!(
            codex_kind_to_state("hook.codex.post_tool_use"),
            Some(State::Idle)
        );
        assert_eq!(
            codex_kind_to_state("hook.codex.stop"),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            codex_kind_to_state("hook.codex.permission_request"),
            Some(State::AwaitingInput)
        );
        assert_eq!(codex_kind_to_state("hook.codex.something_else"), None);
    }

    #[test]
    fn severity_ordering() {
        // AwaitingInput > Errored > Working > Starting > Idle > Done
        assert!(State::AwaitingInput.severity() > State::Errored.severity());
        assert!(State::Errored.severity() > State::Working.severity());
        assert!(State::Working.severity() > State::Starting.severity());
        assert!(State::Starting.severity() > State::Idle.severity());
        assert!(State::Idle.severity() > State::Done.severity());
    }

    #[test]
    fn wire_names_pinned() {
        assert_eq!(State::Starting.wire_name(), "Starting");
        assert_eq!(State::Idle.wire_name(), "Idle");
        assert_eq!(State::Working.wire_name(), "Working");
        assert_eq!(State::AwaitingInput.wire_name(), "AwaitingInput");
        assert_eq!(State::Errored.wire_name(), "Errored");
        assert_eq!(State::Done.wire_name(), "Done");
    }

    // ----- end-to-end behavior tests against an in-memory repo --------------

    use crate::db::sqlite::SqlxRepo;
    use crate::model::{NewCard, NewCove, NewWave};
    use serde_json::Value;
    use std::time::Duration as StdDuration;

    async fn setup() -> (Arc<dyn Repo>, EventBus, String, String) {
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let bus = EventBus::new();
        let cove = repo
            .cove_create(NewCove {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "w".into(),
                sort: None,
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .unwrap();
        (repo, bus, wave.id, card.id)
    }

    #[tokio::test]
    async fn upgrade_commits_immediately() {
        let (repo, bus, wave_id, card_id) = setup().await;
        spawn(repo.clone(), bus.clone());
        // Give the spawn a tick to subscribe.
        tokio::task::yield_now().await;

        bus.emit(Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.pre_tool_use".into(),
            payload: Value::Null,
        });

        // Wait a beat for the async handler to land the overlay write.
        tokio::time::sleep(StdDuration::from_millis(100)).await;

        let card_overlays = repo.overlays_for("card", &card_id).await.unwrap();
        let s = card_overlays
            .iter()
            .find(|o| o.kind == "status")
            .expect("status overlay written");
        assert_eq!(s.payload["state"], "Working");

        let wave_overlays = repo.overlays_for("wave", &wave_id).await.unwrap();
        let ws = wave_overlays.iter().find(|o| o.kind == "status").unwrap();
        assert_eq!(ws.payload["state"], "Working");
        assert_eq!(ws.payload["counts"]["working"], 1);
    }

    #[tokio::test]
    async fn awaiting_input_beats_working() {
        let (repo, bus, _wave_id, card_id) = setup().await;
        spawn(repo.clone(), bus.clone());
        tokio::task::yield_now().await;

        bus.emit(Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.pre_tool_use".into(),
            payload: Value::Null,
        });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        bus.emit(Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.permission_request".into(),
            payload: Value::Null,
        });
        tokio::time::sleep(StdDuration::from_millis(100)).await;

        let card_overlays = repo.overlays_for("card", &card_id).await.unwrap();
        let s = card_overlays
            .iter()
            .find(|o| o.kind == "status")
            .expect("status overlay");
        // BUG-FIX BASELINE: permission_request → AwaitingInput, not Idle.
        assert_eq!(s.payload["state"], "AwaitingInput");
    }

    #[tokio::test]
    async fn downgrade_is_debounced() {
        let (repo, bus, _wave_id, card_id) = setup().await;
        spawn(repo.clone(), bus.clone());
        tokio::task::yield_now().await;

        bus.emit(Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.pre_tool_use".into(),
            payload: Value::Null,
        });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        bus.emit(Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.post_tool_use".into(),
            payload: Value::Null,
        });
        // Within the quiet window: still "Working".
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        let card_overlays = repo.overlays_for("card", &card_id).await.unwrap();
        let s = card_overlays.iter().find(|o| o.kind == "status").unwrap();
        assert_eq!(s.payload["state"], "Working");

        // After the window: settles to "Idle".
        tokio::time::sleep(StdDuration::from_millis(800)).await;
        let card_overlays = repo.overlays_for("card", &card_id).await.unwrap();
        let s = card_overlays.iter().find(|o| o.kind == "status").unwrap();
        assert_eq!(s.payload["state"], "Idle");
    }
}
