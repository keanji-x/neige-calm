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
//! Wave-level lifecycle is owned by the [`WaveLifecycle`](crate::model::WaveLifecycle)
//! enum stamped on the `waves` row (driven by the Spec Agent) — this projector
//! deliberately does NOT write that column. The two responsibilities stay split
//! cleanly: Spec Agent owns the wave's lifecycle stage, the FSM owns per-card
//! status, and the two get OR'd at the UI layer for the sidebar "Waiting on
//! you" grouping (see issue #254).
//!
//! On top of the per-card overlay, the projector ALSO writes a single
//! wave-scoped boolean overlay
//! `{kind:"any_card_needs_input", payload:{value: bool}}` whenever any card
//! under the wave is in `AwaitingInput` or `Errored`. This is **not** the old
//! `recompute_wave` projection that #248 deleted: that one re-projected the
//! whole 6-state FSM union onto the wave (a dual source of truth with
//! `WaveLifecycle`). This new overlay is one bool with explicit semantics —
//! "does any card under this wave currently need a human?" — and is
//! complementary to lifecycle, not redundant with it (#254).
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

use crate::db::sqlite::overlay_upsert_tx;
use crate::db::{RepoEventWrite, write_with_event_typed};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::NewOverlay;
use crate::state::WriteContext;
use crate::validation::{
    OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION, OVERLAY_STATUS_SCHEMA_VERSION,
};

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

pub(crate) struct CodexWorkerHook {
    /// PascalCase event name, used verbatim as the key in
    /// docker/codex-requirements.toml.
    pub event_name: &'static str,
    /// Worker FSM state this hook projects the card onto.
    pub state: State,
}

pub(crate) const CODEX_WORKER_HOOKS: &[CodexWorkerHook] = &[
    CodexWorkerHook {
        event_name: "SessionStart",
        state: State::Starting,
    },
    CodexWorkerHook {
        event_name: "UserPromptSubmit",
        state: State::Working,
    },
    CodexWorkerHook {
        event_name: "PreToolUse",
        state: State::Working,
    },
    CodexWorkerHook {
        event_name: "PostToolUse",
        state: State::Working,
    },
    CodexWorkerHook {
        event_name: "PermissionRequest",
        state: State::AwaitingInput,
    },
    CodexWorkerHook {
        event_name: "Stop",
        state: State::AwaitingInput,
    },
];

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
    let bare = kind.strip_prefix("hook.codex.")?;
    CODEX_WORKER_HOOKS
        .iter()
        .find(|h| crate::routes::codex::to_snake_case(h.event_name) == bare)
        .map(|h| h.state)
}

// ---------------------------------------------------------------------------
// Claude hook → State projection
// ---------------------------------------------------------------------------

/// Single source of truth for the Claude Code worker hooks the kernel
/// subscribes to and projects onto worker FSM state.
///
/// `build_claude_settings_json` (routes::claude_cards) iterates this to emit
/// the generated `--settings` file's `hooks` map, and `claude_kind_to_state`
/// projects each onto a worker `State`. Driving both paths from one table is
/// the #364 fix: previously the settings list and the projection match arms
/// were maintained separately and drifted — six hooks the FSM recognized
/// (`SubagentStart`/`SubagentStop`, `TaskCreated`/`TaskCompleted`,
/// `PermissionDenied`, `Elicitation`) were never registered, so Claude never
/// fired them and those transitions were unreachable.
///
/// Event names + matcher applicability verified against
/// https://code.claude.com/docs/en/hooks (2026-05).
pub(crate) struct ClaudeWorkerHook {
    /// PascalCase event name, used verbatim as the key in the settings
    /// `hooks` map (exactly what Claude Code reads).
    pub event_name: &'static str,
    /// Whether we register a `"matcher": "*"` for this hook. Mirrors the
    /// pre-existing convention: only the tool-name-scoped hooks (the
    /// PreToolUse family plus the two permission hooks) carry a matcher;
    /// subagent / task / elicitation / lifecycle hooks omit it. Omission is
    /// equivalent to match-all, which is what the FSM wants (every
    /// occurrence) — so this flag only keeps us faithful to how the existing
    /// settings were written; it never filters anything out.
    pub matcher: bool,
    /// Worker FSM state this hook projects the card onto.
    pub state: State,
}

pub(crate) const CLAUDE_WORKER_HOOKS: &[ClaudeWorkerHook] = &[
    ClaudeWorkerHook {
        event_name: "SessionStart",
        matcher: false,
        state: State::Starting,
    },
    ClaudeWorkerHook {
        event_name: "UserPromptSubmit",
        matcher: false,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "PreToolUse",
        matcher: true,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "PostToolUse",
        matcher: true,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "PostToolUseFailure",
        matcher: true,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "SubagentStart",
        matcher: false,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "SubagentStop",
        matcher: false,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "TaskCreated",
        matcher: false,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "TaskCompleted",
        matcher: false,
        state: State::Working,
    },
    ClaudeWorkerHook {
        event_name: "PermissionRequest",
        matcher: true,
        state: State::AwaitingInput,
    },
    ClaudeWorkerHook {
        event_name: "PermissionDenied",
        matcher: true,
        state: State::AwaitingInput,
    },
    ClaudeWorkerHook {
        event_name: "Notification",
        matcher: false,
        state: State::AwaitingInput,
    },
    ClaudeWorkerHook {
        event_name: "Elicitation",
        matcher: false,
        state: State::AwaitingInput,
    },
    // Interactive Claude workers follow the same turn-boundary semantics as
    // codex foreground agents: `stop` means the worker is waiting for the
    // user's next prompt, so the wave surfaces "waiting on you" (#358/#367) —
    // not `Idle`.
    ClaudeWorkerHook {
        event_name: "Stop",
        matcher: false,
        state: State::AwaitingInput,
    },
    ClaudeWorkerHook {
        event_name: "StopFailure",
        matcher: false,
        state: State::Errored,
    },
    // Documented `SessionEnd.reason` values are `clear`, `resume`, `logout`,
    // `prompt_input_exit`, `bypass_permissions_disabled`, and `other`; none
    // indicates an error, so a session ending projects to `Done`, never
    // `Errored`. Verified against https://code.claude.com/docs/en/hooks.
    ClaudeWorkerHook {
        event_name: "SessionEnd",
        matcher: false,
        state: State::Done,
    },
];

/// Project a Claude hook `kind` (e.g. `hook.claude.pre_tool_use`) onto the
/// worker-card FSM. This intentionally stays separate from
/// `codex_kind_to_state`, but interactive Claude workers follow the same
/// turn-boundary semantics as codex foreground agents: `stop` means the
/// worker is waiting for the user's next input, so the UI should surface it
/// as `AwaitingInput`.
///
/// Claude Code hook names verified against https://code.claude.com/docs/en/hooks.
fn claude_kind_to_state(kind: &str, _payload: &serde_json::Value) -> Option<State> {
    let bare = kind.strip_prefix("hook.claude.")?;
    CLAUDE_WORKER_HOOKS
        .iter()
        .find(|h| crate::routes::codex::to_snake_case(h.event_name) == bare)
        .map(|h| h.state)
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
pub fn spawn(repo: Arc<dyn RepoEventWrite>, bus: EventBus, write: WriteContext) {
    let mut rx = bus.subscribe();
    let bus_clone = bus.clone();
    tokio::spawn(async move {
        let inner = Arc::new(Inner::new(repo, bus_clone, write));
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
    write: WriteContext,
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
    fn new(repo: Arc<dyn RepoEventWrite>, bus: EventBus, write: WriteContext) -> Self {
        Self {
            repo,
            bus,
            write,
            map: Mutex::new(HashMap::new()),
        }
    }

    async fn handle(self: &Arc<Self>, ev: Event) {
        match ev {
            Event::CodexHook { card_id, kind, .. } => {
                let Some(target) = codex_kind_to_state(&kind) else {
                    return;
                };
                self.observe(card_id, target).await;
            }
            Event::ClaudeHook {
                card_id,
                kind,
                payload,
            } => {
                let Some(target) = claude_kind_to_state(&kind, &payload) else {
                    return;
                };
                self.observe(card_id, target).await;
            }
            _ => {}
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

    /// Commit a card state change: write the card-level overlay and
    /// recompute the wave-scoped `any_card_needs_input` aggregate. Both
    /// writes emit `Event::OverlaySet` so the WS bridge invalidates the
    /// right queries. The wave-level `WaveLifecycle` column is owned by
    /// the Spec Agent — this projector still does not touch it.
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
            &self.write,
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

        // 2. Wave-scoped `any_card_needs_input` aggregate. Issue #254 —
        //    OR'd at the UI layer with `WaveLifecycle` for the sidebar
        //    "Waiting on you" grouping. Does NOT touch the lifecycle
        //    column, so Spec Agent stays the single source of truth for
        //    wave-level state.
        self.recompute_wave_needs_input(&card.wave_id).await;
    }

    /// Aggregate every card under `wave_id` into a single boolean
    /// `any_card_needs_input` overlay on the wave. Idempotent: if the
    /// computed value matches what's already on disk, no write fires
    /// (and no event is emitted). Issue #254.
    ///
    /// Pulls the canonical card set from `repo.cards_by_wave` rather
    /// than scanning the global FSM map. Two reasons:
    ///   - **No lock-across-IO.** The previous shape held `self.map.lock()`
    ///     across a `card_get` round-trip per entry, blocking every other
    ///     FSM handler for the duration. Reading the wave's cards out of
    ///     the repo first, then taking the map lock once for an in-memory
    ///     lookup, keeps the critical section sub-microsecond.
    ///   - **Future-proof for phase 2.** Once terminal / plugin cards
    ///     start populating the FSM map they'll be scoped to *their* wave
    ///     automatically — the aggregator can't accidentally pick up a
    ///     card from a sibling wave that happens to share the map.
    ///
    /// Concurrent `commit()` callers can race the read-then-write
    /// idempotency check below; the final write resolves via
    /// `overlay_upsert_tx`'s `ON CONFLICT DO UPDATE` (last writer wins,
    /// no lost writes — see `db/sqlite.rs::overlay_upsert_tx`).
    async fn recompute_wave_needs_input(&self, wave_id: &WaveId) {
        // 1. Snapshot the canonical card set for this wave. This is the
        //    source of truth for "what cards belong to this wave" — the
        //    FSM map is just our live state cache for those cards.
        let cards = match self.repo.cards_by_wave(wave_id.as_str()).await {
            Ok(cs) => cs,
            Err(e) => {
                tracing::warn!(
                    wave_id = %wave_id,
                    error = %e,
                    "card_fsm: cards_by_wave failed during needs_input recompute"
                );
                return;
            }
        };

        // 2. Lock the map briefly for an in-memory lookup only — no awaits
        //    inside the critical section. Cards that don't have an entry
        //    in the FSM map (terminal/plugin cards in phase 1, or codex
        //    cards we haven't yet observed an event for) contribute
        //    nothing — they can't be in `AwaitingInput` / `Errored`
        //    without first showing up in the map.
        let needs_input = {
            let map = self.map.lock().await;
            cards.iter().any(|c| {
                matches!(
                    map.get(&c.id),
                    Some(entry)
                        if matches!(entry.committed, State::AwaitingInput | State::Errored)
                )
            })
        };

        // 3. Idempotency: read the existing wave overlay and skip the write
        //    when the boolean is unchanged. Without this the projector
        //    would churn an overlay event on every per-card transition,
        //    even when the wave-level answer didn't move.
        let existing = match self.repo.overlays_for("wave", wave_id.as_str()).await {
            Ok(rows) => rows
                .into_iter()
                .find(|o| o.kind == "any_card_needs_input" && o.plugin_id == KERNEL_PLUGIN_ID),
            Err(e) => {
                tracing::warn!(
                    wave_id = %wave_id,
                    error = %e,
                    "card_fsm: overlays_for(wave) failed during needs_input recompute"
                );
                return;
            }
        };
        if let Some(prev) = &existing
            && prev.payload.get("value").and_then(|v| v.as_bool()) == Some(needs_input)
        {
            return; // unchanged — skip the write
        }

        // Resolve cove for the event scope. On failure, fall back to
        // `EventScope::System` — same defensive policy as the per-card
        // overlay write above.
        let scope = match self.repo.wave_get(wave_id.as_str()).await {
            Ok(Some(w)) => EventScope::Wave {
                wave: w.id,
                cove: w.cove_id,
            },
            _ => EventScope::System,
        };
        let payload = json!({
            "schemaVersion": OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION,
            "value": needs_input,
        });
        let new_overlay = NewOverlay {
            plugin_id: KERNEL_PLUGIN_ID.to_string(),
            entity_kind: "wave".to_string(),
            entity_id: wave_id.to_string(),
            kind: "any_card_needs_input".to_string(),
            payload,
        };
        if let Err(e) = write_with_event_typed(
            self.repo.as_ref(),
            fsm_actor(),
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
        .await
        {
            tracing::warn!(
                wave_id = %wave_id,
                error = %e,
                "card_fsm: any_card_needs_input overlay_upsert failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX_REQUIREMENTS_TOML: &str = include_str!("../../../docker/codex-requirements.toml");

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
    fn every_codex_worker_hook_is_registered_in_requirements_toml() {
        for hook in CODEX_WORKER_HOOKS {
            let needle = format!("[[hooks.{}]]", hook.event_name);
            assert!(
                CODEX_REQUIREMENTS_TOML.contains(&needle),
                "docker/codex-requirements.toml is missing registration for {needle}; \
                 FSM projects this hook to {:?} but codex CLI never fires it. \
                 See #372.",
                hook.state,
            );
        }
    }

    #[test]
    fn claude_kind_mapping_is_worker_specific() {
        assert_eq!(
            claude_kind_to_state("hook.claude.session_start", &Value::Null),
            Some(State::Starting)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.pre_tool_use", &Value::Null),
            Some(State::Working)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.subagent_start", &Value::Null),
            Some(State::Working)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.permission_request", &Value::Null),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.notification", &Value::Null),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.permission_denied", &Value::Null),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.elicitation", &Value::Null),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.teammate_idle", &Value::Null),
            None
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.stop", &Value::Null),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            codex_kind_to_state("hook.codex.stop"),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.stop_failure", &Value::Null),
            Some(State::Errored)
        );
        assert_eq!(
            claude_kind_to_state(
                "hook.claude.session_end",
                &json!({ "reason": "prompt_input_exit" })
            ),
            Some(State::Done)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.session_end", &json!({ "reason": "fatal" })),
            Some(State::Done)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.pre_compact", &Value::Null),
            None,
            "real but unmapped Claude hooks are no-ops"
        );
        assert_eq!(
            claude_kind_to_state("hook.codex.stop", &Value::Null),
            None,
            "Claude mapping only strips the Claude prefix"
        );
    }

    #[test]
    fn every_registered_hook_projects_to_its_table_state() {
        for h in CLAUDE_WORKER_HOOKS {
            let kind = format!(
                "hook.claude.{}",
                crate::routes::codex::to_snake_case(h.event_name)
            );
            assert_eq!(
                claude_kind_to_state(&kind, &serde_json::Value::Null),
                Some(h.state),
                "hook {} (kind {kind}) must project to {:?}",
                h.event_name,
                h.state
            );
        }
        // The six hooks #364 added must specifically be projected now.
        for (name, want) in [
            ("SubagentStart", State::Working),
            ("SubagentStop", State::Working),
            ("TaskCreated", State::Working),
            ("TaskCompleted", State::Working),
            ("PermissionDenied", State::AwaitingInput),
            ("Elicitation", State::AwaitingInput),
        ] {
            let kind = format!("hook.claude.{}", crate::routes::codex::to_snake_case(name));
            assert_eq!(
                claude_kind_to_state(&kind, &serde_json::Value::Null),
                Some(want)
            );
        }
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
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
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
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::wave_cove_cache::WaveCoveCache::new(),
            ),
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

        // Regression guard from #248: the per-card FSM commit must not
        // write a wave-level overlay of `kind == "status"` (the old union
        // projection that #248 deleted — it was a dual source of truth
        // with `WaveLifecycle`, owned by the Spec Agent). PR #260 (issue
        // #254) re-introduces a narrower wave-level overlay
        // `kind == "any_card_needs_input"`, which is expected here — this
        // test asserts only that the deleted `"status"` projection has
        // not silently returned.
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
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::wave_cove_cache::WaveCoveCache::new(),
            ),
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
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::wave_cove_cache::WaveCoveCache::new(),
            ),
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

    // ----- #254 wave-scoped `any_card_needs_input` aggregator ----------------

    #[tokio::test]
    async fn needs_input_overlay_fires_on_awaiting_input() {
        let (repo, bus, wave_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::wave_cove_cache::WaveCoveCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(150)).await;

        let wave_overlays = repo.overlays_for("wave", wave_id.as_str()).await.unwrap();
        let o = wave_overlays
            .iter()
            .find(|o| o.kind == "any_card_needs_input")
            .expect("any_card_needs_input overlay written");
        assert_eq!(o.payload["value"], Value::Bool(true));
        assert_eq!(o.plugin_id, KERNEL_PLUGIN_ID);
    }

    #[tokio::test]
    async fn needs_input_overlay_clears_on_working() {
        let (repo, bus, wave_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::wave_cove_cache::WaveCoveCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        // Drive into AwaitingInput first.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        // Sanity: overlay is true.
        let v = repo
            .overlays_for("wave", wave_id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "any_card_needs_input")
            .unwrap()
            .payload["value"]
            .clone();
        assert_eq!(v, Value::Bool(true));

        // pre_tool_use is an upgrade from AwaitingInput? No — Working
        // has lower severity than AwaitingInput. The 750ms downgrade
        // window holds it. Sleep past the window.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(1100)).await;

        let v = repo
            .overlays_for("wave", wave_id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "any_card_needs_input")
            .unwrap()
            .payload["value"]
            .clone();
        assert_eq!(v, Value::Bool(false));
    }

    #[tokio::test]
    async fn needs_input_overlay_is_idempotent() {
        let (repo, bus, wave_id, card_id) = setup().await;
        // Subscribe BEFORE spawn so we capture every overlay event from
        // the moment the FSM is live.
        let mut rx = bus.subscribe();
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::wave_cove_cache::WaveCoveCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        // First emit → card goes AwaitingInput, wave overlay flips to true.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(150)).await;

        // Second emit → STILL AwaitingInput (same severity, no actual
        // transition), wave aggregate is unchanged. The idempotency
        // guard should suppress the second overlay write.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(150)).await;

        // Drain the receiver and count wave-scoped `any_card_needs_input`
        // OverlaySet events. There should be exactly ONE.
        let mut wave_overlay_writes = 0usize;
        while let Ok(env) = rx.try_recv() {
            if let Event::OverlaySet(ref o) = env.event
                && o.kind == "any_card_needs_input"
                && o.entity_kind == "wave"
                && o.entity_id == wave_id.to_string()
            {
                wave_overlay_writes += 1;
            }
        }
        assert_eq!(
            wave_overlay_writes, 1,
            "expected exactly one any_card_needs_input write (idempotent), got {wave_overlay_writes}"
        );
    }

    #[tokio::test]
    async fn needs_input_overlay_ors_multiple_cards() {
        // Two codex cards under the same wave. Driving ONE to
        // AwaitingInput should light up the wave overlay even while the
        // other stays Working; flipping both to Working should clear it.
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
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card_a = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .unwrap();
        let card_b = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .unwrap();
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::wave_cove_cache::WaveCoveCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        // A → Working, B → AwaitingInput. Wave overlay should be true.
        bus.emit(
            ActorId::AiCodex(card_a.id.clone()),
            Event::CodexHook {
                card_id: card_a.id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                payload: Value::Null,
            },
        );
        bus.emit(
            ActorId::AiCodex(card_b.id.clone()),
            Event::CodexHook {
                card_id: card_b.id.clone(),
                kind: "hook.codex.permission_request".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(200)).await;
        let v = repo
            .overlays_for("wave", wave.id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "any_card_needs_input")
            .expect("overlay present")
            .payload["value"]
            .clone();
        assert_eq!(v, Value::Bool(true));

        // Flip B back to Working — past the 750ms downgrade window.
        bus.emit(
            ActorId::AiCodex(card_b.id.clone()),
            Event::CodexHook {
                card_id: card_b.id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                payload: Value::Null,
            },
        );
        tokio::time::sleep(StdDuration::from_millis(1100)).await;
        let v = repo
            .overlays_for("wave", wave.id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "any_card_needs_input")
            .unwrap()
            .payload["value"]
            .clone();
        assert_eq!(v, Value::Bool(false));
    }
}
