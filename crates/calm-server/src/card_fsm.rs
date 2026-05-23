//! Per-card FSM projector.
//!
//! A long-running background task subscribes to `EventBus` and projects
//! incoming events onto a per-card 6-state FSM:
//!
//!   `Starting / Idle / Working / AwaitingInput / Errored / Done`
//!
//! Whenever a card's state changes, the task writes a kernel-owned
//! `Overlay { plugin_id="kernel", entity_kind="card", entity_id=<card_id>,
//! kind="status", payload={ state } }`. The codex card head consumes this
//! overlay directly.
//!
//! Wave-level state is owned by the [`WaveLifecycle`](crate::model::WaveLifecycle)
//! enum stamped on the `waves` row (driven by the Spec Agent) — this projector
//! deliberately does NOT write wave-level overlays anymore, so there's a single
//! source of truth for "what's the wave doing right now".
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
//! Cards that don't have an entry in the FSM map are silently skipped —
//! the projector only owns the per-card overlay row.
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

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::overlay_upsert_tx;
use crate::db::{RepoEventWrite, write_with_event_typed};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, CardId};
use crate::model::NewOverlay;
use crate::validation::OVERLAY_STATUS_SCHEMA_VERSION;
use crate::wave_cove_cache::WaveCoveCache;

/// Actor stamped on every event the FSM produces. Kernel-internal
/// projector — distinct from [`ActorId::User`] / [`ActorId::Plugin`] /
/// [`ActorId::AiCodex`]. PR2 of #136 typed this from the legacy
/// `"kernel"` string.
const fn fsm_actor() -> ActorId {
    ActorId::Kernel
}

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
/// bottleneck and the wave-union should surface that. `PostToolUse` →
/// `Working` (not `Idle`): the agent is still active between tool calls
/// (reasoning about the next step), and only `stop` truly ends the turn.
/// Previously this was `Idle` with a 750ms debounce, which leaked through
/// whenever inter-tool reasoning took longer than the quiet window.
fn codex_kind_to_state(kind: &str) -> Option<State> {
    match kind {
        "hook.codex.session_start" => Some(State::Starting),
        "hook.codex.user_prompt_submit"
        | "hook.codex.pre_tool_use"
        | "hook.codex.post_tool_use" => Some(State::Working),
        "hook.codex.stop" => Some(State::AwaitingInput),
        "hook.codex.permission_request" => Some(State::AwaitingInput),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Background task entry point
// ---------------------------------------------------------------------------

/// Spawn the FSM task. Subscribes to `bus`, owns its own state map.
///
/// Takes the narrow `Arc<dyn RepoEventWrite>` rather than the full
/// `Arc<dyn Repo>` — the projector only does eventized writes (overlay
/// upserts via `write_with_event_typed`) plus reads (`card_get`,
/// `cards_by_wave`) inherited from the `RepoRead` supertrait. Raw
/// sync-domain writes like `overlay_upsert` / `card_update` are
/// deliberately unreachable here so a future contributor can't quietly
/// bypass the event-log invariant (PR #41).
pub fn spawn(
    repo: Arc<dyn RepoEventWrite>,
    bus: EventBus,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
) {
    let mut rx = bus.subscribe();
    let bus_clone = bus.clone();
    tokio::spawn(async move {
        let inner = Arc::new(Inner::new(
            repo,
            bus_clone,
            card_role_cache,
            wave_cove_cache,
        ));
        loop {
            match rx.recv().await {
                Ok(env) => inner.handle(env.event).await,
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
    repo: Arc<dyn RepoEventWrite>,
    bus: EventBus,
    /// PR3 (#136) role-gate cache. Threaded through every emit so
    /// `enforce_role` runs against the same map the rest of the server
    /// shares.
    card_role_cache: CardRoleCache,
    /// #234 — parallel wave→cove cache used by the role gate.
    wave_cove_cache: WaveCoveCache,
    /// `card_id → (committed_state, pending_downgrade_deadline)`.
    ///
    /// `pending_downgrade_deadline` is `Some(deadline)` only when a downgrade
    /// is being held. A landing upgrade clears it.
    map: Mutex<HashMap<CardId, CardEntry>>,
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
    fn new(
        repo: Arc<dyn RepoEventWrite>,
        bus: EventBus,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            bus,
            card_role_cache,
            wave_cove_cache,
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
    async fn observe(self: &Arc<Self>, card_id: CardId, target: State) {
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

    fn schedule_downgrade(self: Arc<Self>, card_id: CardId, deadline: Instant) {
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

    /// Commit a card state change: write the card-level overlay. Emits
    /// `Event::OverlaySet` so the WS bridge invalidates the right queries.
    /// Wave-level state lives on the kernel `WaveLifecycle` enum and is
    /// driven by the Spec Agent — this projector does not write wave-level
    /// overlays.
    async fn commit(&self, card_id: &CardId, state: State) {
        // Look up the owning wave so the audit row carries the full
        // ancestor chain.
        let card = match self.repo.card_get(card_id.as_ref()).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::debug!(card_id = %card_id, "card_fsm: card vanished mid-commit, skipping");
                return;
            }
            Err(e) => {
                tracing::warn!(card_id = %card_id, error = %e, "card_fsm: card_get failed");
                return;
            }
        };

        // 1. Card overlay. Goes through write_with_event so the overlay
        //    row and the events row land in the same transaction; the bus
        //    broadcast (with `_id` stamped) is emitted on commit success.
        // `schemaVersion` is the Tier A persistence contract from
        // `docs/upgrade-stability.md` — kernel-owned overlay payloads
        // stamp the version explicitly so an older binary can refuse a
        // v2 row from a newer one rather than silently mis-interpreting.
        let card_payload = json!({
            "schemaVersion": OVERLAY_STATUS_SCHEMA_VERSION,
            "state": state.wire_name(),
        });
        let new_overlay = NewOverlay {
            plugin_id: KERNEL_PLUGIN_ID.to_string(),
            entity_kind: "card".to_string(),
            entity_id: card_id.to_string(),
            kind: "status".to_string(),
            payload: card_payload,
        };
        // Resolve `wave → cove` so the audit row carries the full
        // ancestor chain (PR2 of #136). On lookup failure fall back to
        // `EventScope::System` — we'd rather emit a less-scoped event
        // than refuse the FSM commit, since the projection itself is
        // best-effort.
        let scope = match self.repo.wave_get(card.wave_id.as_str()).await {
            Ok(Some(w)) => EventScope::Card {
                card: card_id.clone(),
                wave: w.id,
                cove: w.cove_id,
            },
            _ => EventScope::System,
        };
        if let Err(e) = write_with_event_typed(
            self.repo.as_ref(),
            fsm_actor(),
            scope,
            None,
            &self.bus,
            &self.card_role_cache,
            &self.wave_cove_cache,
            move |tx| {
                Box::pin(async move {
                    let o = overlay_upsert_tx(tx, new_overlay).await?;
                    Ok(((), Event::OverlaySet(o)))
                })
            },
        )
        .await
        {
            tracing::warn!(card_id = %card_id, error = %e, "card_fsm: card overlay_upsert failed");
        }
        // Wave-level state is owned by `WaveLifecycle` (kernel column on
        // `waves`, driven by the Spec Agent). The projector deliberately
        // does NOT write a wave-level overlay so there's a single source
        // of truth.
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
            Some(State::Working)
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

    // Tests seed fixtures via raw sync-domain writes (`cove_create`,
    // `wave_create`, `card_create`), so they need the full `Repo`. Production
    // `spawn` takes the narrowed `Arc<dyn RepoEventWrite>` — the call below
    // relies on stable trait-object coercion at the function-argument site.
    use crate::db::Repo;
    use crate::db::sqlite::SqlxRepo;
    use crate::ids::WaveId;
    use crate::model::{NewCard, NewCove, NewWave};
    use serde_json::Value;
    use std::time::Duration as StdDuration;

    async fn setup() -> (Arc<dyn Repo>, EventBus, WaveId, CardId) {
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
        spawn(
            repo.clone(),
            bus.clone(),
            crate::card_role_cache::CardRoleCache::new(),
            crate::wave_cove_cache::WaveCoveCache::new(),
        );
        // Give the spawn a tick to subscribe.
        tokio::task::yield_now().await;

        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                payload: Value::Null,
            },
        );

        // Wait a beat for the async handler to land the overlay write.
        tokio::time::sleep(StdDuration::from_millis(100)).await;

        let card_overlays = repo.overlays_for("card", card_id.as_str()).await.unwrap();
        let s = card_overlays
            .iter()
            .find(|o| o.kind == "status")
            .expect("status overlay written");
        assert_eq!(s.payload["state"], "Working");

        // Regression: the projector must NOT write a wave-level overlay
        // anymore — wave state lives on `WaveLifecycle` and is driven by
        // the Spec Agent. A leftover write here would silently reintroduce
        // the dual-source-of-truth bug we just removed.
        let wave_overlays = repo.overlays_for("wave", wave_id.as_str()).await.unwrap();
        assert!(
            wave_overlays.iter().all(|o| o.kind != "status"),
            "card_fsm must not write wave-level status overlays; found: {:?}",
            wave_overlays.iter().map(|o| &o.kind).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn awaiting_input_beats_working() {
        let (repo, bus, _wave_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            crate::card_role_cache::CardRoleCache::new(),
            crate::wave_cove_cache::WaveCoveCache::new(),
        );
        tokio::task::yield_now().await;

        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(100)).await;

        let card_overlays = repo.overlays_for("card", card_id.as_str()).await.unwrap();
        let s = card_overlays
            .iter()
            .find(|o| o.kind == "status")
            .expect("status overlay");
        // BUG-FIX BASELINE: permission_request → AwaitingInput, not Idle.
        assert_eq!(s.payload["state"], "AwaitingInput");
    }

    #[tokio::test]
    async fn post_tool_use_stays_working() {
        // post_tool_use now maps to Working, not Idle: between tool calls
        // the agent is still actively reasoning, so the card should not
        // briefly flicker to Idle. Only `stop` truly ends the turn.
        let (repo, bus, _wave_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            crate::card_role_cache::CardRoleCache::new(),
            crate::wave_cove_cache::WaveCoveCache::new(),
        );
        tokio::task::yield_now().await;

        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.post_tool_use".into(),
                payload: Value::Null,
            },
        );

        // Past the old debounce window — still Working, never flickers.
        tokio::time::sleep(StdDuration::from_millis(900)).await;
        let card_overlays = repo.overlays_for("card", card_id.as_str()).await.unwrap();
        let s = card_overlays.iter().find(|o| o.kind == "status").unwrap();
        assert_eq!(s.payload["state"], "Working");

        // stop → AwaitingInput commits the turn boundary.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.stop".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        let card_overlays = repo.overlays_for("card", card_id.as_str()).await.unwrap();
        let s = card_overlays.iter().find(|o| o.kind == "status").unwrap();
        assert_eq!(s.payload["state"], "AwaitingInput");
    }
}
