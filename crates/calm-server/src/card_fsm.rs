//! Per-card FSM projector: a background task subscribes to `EventBus` and projects hook events onto a per-card 6-state FSM (`Starting / Idle / Working / AwaitingInput / Errored / Done`),
//! writing a kernel-owned card `status` overlay and a track-scoped `any_card_needs_input` boolean overlay. It never writes `TrackLifecycle` (owned by the Planner Agent).
//! Upgrades commit immediately, downgrades are held for `DOWNGRADE_QUIET_MS`. In-memory only: the map starts empty on restart and the first hook per card re-populates it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};

use crate::db::sqlite::overlay_upsert_tx;
use crate::db::{RepoEventWrite, write_with_event_typed};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::NewOverlay;
use crate::state::WriteContext;
use crate::validation::{
    OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION, OVERLAY_STATUS_SCHEMA_VERSION,
};

/// Actor stamped on every event the FSM produces.
const fn fsm_actor() -> ActorId {
    ActorId::Kernel
}

/// Plugin id stamped on the FSM-authored overlays; plugins cannot write under the reserved `"kernel"` namespace.
const KERNEL_PLUGIN_ID: &str = "kernel";

/// How long to hold a downgrade before committing it; upgrades are emitted immediately.
const DOWNGRADE_QUIET_MS: u64 = 750;

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

    /// Severity ordering: `AwaitingInput > Errored > Working > Starting > Idle > Done`.
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

pub(crate) struct CodexWorkerHook {
    /// PascalCase event name, used verbatim as the key in docker/codex-requirements.toml.
    pub event_name: &'static str,
    /// `None` keeps the row as vocabulary only: registered and recognised, but the FSM leaves the card alone.
    pub state: Option<State>,
}

pub(crate) const CODEX_WORKER_HOOKS: &[CodexWorkerHook] = &[
    CodexWorkerHook {
        event_name: "SessionStart",
        state: Some(State::Starting),
    },
    CodexWorkerHook {
        event_name: "UserPromptSubmit",
        state: Some(State::Working),
    },
    CodexWorkerHook {
        event_name: "PreToolUse",
        state: Some(State::Working),
    },
    CodexWorkerHook {
        event_name: "PostToolUse",
        state: Some(State::Working),
    },
    CodexWorkerHook {
        event_name: "PermissionRequest",
        state: Some(State::AwaitingInput),
    },
    CodexWorkerHook {
        event_name: "Stop",
        state: Some(State::Idle),
    },
];

/// Returns `None` for hooks we don't model — the FSM leaves the card's state alone.
fn codex_kind_to_state(kind: &str) -> Option<State> {
    let bare = kind.strip_prefix("hook.codex.")?;
    CODEX_WORKER_HOOKS
        .iter()
        .find(|h| crate::routes::codex::to_snake_case(h.event_name) == bare)
        .and_then(|h| h.state)
}

/// Single source of truth for the Claude Code worker hooks: `build_claude_settings_json` emits the settings `hooks` map from it and `claude_kind_to_state` projects from it, so the two cannot drift.
/// Event names + matcher applicability verified against https://code.claude.com/docs/en/hooks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClaudeWorkerHook {
    /// PascalCase event name, used verbatim as the key in the settings `hooks` map.
    pub event_name: &'static str,
    /// Whether we register a `"matcher": "*"` for this hook. Omission is equivalent to match-all, so this only keeps the generated settings faithful to convention; it never filters anything out.
    pub matcher: bool,
    /// `None` keeps the row as vocabulary only (still registered, still a legal terminal signal). `Notification` additionally gates on the payload.
    pub state: Option<State>,
}

pub(crate) const CLAUDE_WORKER_HOOKS: &[ClaudeWorkerHook] = &[
    ClaudeWorkerHook {
        event_name: "SessionStart",
        matcher: false,
        state: Some(State::Starting),
    },
    ClaudeWorkerHook {
        event_name: "UserPromptSubmit",
        matcher: false,
        state: Some(State::Working),
    },
    ClaudeWorkerHook {
        event_name: "PreToolUse",
        matcher: true,
        state: Some(State::Working),
    },
    ClaudeWorkerHook {
        event_name: "PostToolUse",
        matcher: true,
        state: Some(State::Working),
    },
    ClaudeWorkerHook {
        event_name: "PostToolUseFailure",
        matcher: true,
        state: Some(State::Working),
    },
    // The four sub-agent / task hooks are vocabulary only: after `Stop` they would lift the card back to `Working`. They stay registered, and `terminal_hooks` names `SubagentStop` as a Planner terminal signal.
    ClaudeWorkerHook {
        event_name: "SubagentStart",
        matcher: false,
        state: None,
    },
    ClaudeWorkerHook {
        event_name: "SubagentStop",
        matcher: false,
        state: None,
    },
    ClaudeWorkerHook {
        event_name: "TaskCreated",
        matcher: false,
        state: None,
    },
    ClaudeWorkerHook {
        event_name: "TaskCompleted",
        matcher: false,
        state: None,
    },
    ClaudeWorkerHook {
        event_name: "PermissionRequest",
        matcher: true,
        state: Some(State::AwaitingInput),
    },
    ClaudeWorkerHook {
        event_name: "PermissionDenied",
        matcher: true,
        state: Some(State::AwaitingInput),
    },
    // Projects only when the payload's `notification_type` is in `NOTIFICATION_NEEDS_INPUT_TYPES`; every other subtype — `idle_prompt` above all — is a no-op.
    ClaudeWorkerHook {
        event_name: "Notification",
        matcher: false,
        state: Some(State::AwaitingInput),
    },
    ClaudeWorkerHook {
        event_name: "Elicitation",
        matcher: false,
        state: Some(State::AwaitingInput),
    },
    ClaudeWorkerHook {
        event_name: "Stop",
        matcher: false,
        state: Some(State::Idle),
    },
    ClaudeWorkerHook {
        event_name: "StopFailure",
        matcher: false,
        state: Some(State::Errored),
    },
    // No documented `SessionEnd.reason` indicates an error, so a session ending projects to `Done`, never `Errored`.
    ClaudeWorkerHook {
        event_name: "SessionEnd",
        matcher: false,
        state: Some(State::Done),
    },
];

/// The `Notification.notification_type` values that mean the worker is blocked on a human. Every other or missing value is NOT attention:
/// `idle_prompt` in particular arrives ~60 s after every `Stop`, and projecting it would undo `Stop → Idle`.
const NOTIFICATION_NEEDS_INPUT_TYPES: &[&str] = &[
    "permission_prompt",
    "elicitation_dialog",
    "elicitation_url_dialog",
    "agent_needs_input",
];

/// Reads the hook body's top-level `notification_type` (the same field `terminal_hooks::parse_terminal_signal` reads); absent or non-string ⇒ `false`.
fn notification_needs_input(payload: &serde_json::Value) -> bool {
    payload
        .get("notification_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|t| NOTIFICATION_NEEDS_INPUT_TYPES.contains(&t))
}

/// Project a Claude hook `kind` onto the worker-card FSM; intentionally separate from `codex_kind_to_state`.
fn claude_kind_to_state(kind: &str, payload: &serde_json::Value) -> Option<State> {
    let bare = kind.strip_prefix("hook.claude.")?;
    let hook = CLAUDE_WORKER_HOOKS
        .iter()
        .find(|h| crate::routes::codex::to_snake_case(h.event_name) == bare)?;
    if hook.event_name == "Notification" && !notification_needs_input(payload) {
        return None;
    }
    hook.state
}

/// Spawn the FSM task. Takes the narrow `Arc<dyn RepoEventWrite>` so raw sync-domain writes are unreachable here and the event-log invariant cannot be quietly bypassed.
pub fn spawn(repo: Arc<dyn RepoEventWrite>, bus: EventBus, write: WriteContext) {
    let mut rx = bus.subscribe();
    let bus_clone = bus.clone();
    tokio::spawn(async move {
        let inner = Arc::new(Inner::new(repo, bus_clone, write));
        loop {
            match rx.recv().await {
                Ok(env) => inner.handle(env).await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "card_fsm event subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

struct Inner {
    repo: Arc<dyn RepoEventWrite>,
    bus: EventBus,
    write: WriteContext,
    /// `card_id → (committed_state, pending_downgrade_deadline)`; a landing upgrade clears the pending deadline.
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

    async fn handle(self: &Arc<Self>, env: BroadcastEnvelope) {
        let BroadcastEnvelope { actor, event, .. } = env;
        match event {
            Event::CodexHook { card_id, kind, .. } => {
                let Some(target) = codex_kind_to_state(&kind) else {
                    return;
                };
                if self.hook_is_provably_stale(&card_id, &actor).await {
                    return;
                }
                self.observe(card_id, target).await;
            }
            Event::ClaudeHook {
                card_id,
                kind,
                payload,
                ..
            } => {
                let Some(target) = claude_kind_to_state(&kind, &payload) else {
                    return;
                };
                if self.hook_is_provably_stale(&card_id, &actor).await {
                    return;
                }
                self.observe(card_id, target).await;
            }
            _ => {}
        }
    }

    /// The narrow stale-session fence: a hook stamped with a session actor whose session is not the card's current `cards.session_id` is provably stale and must not move the FSM.
    /// Card-level actors are deliberately NOT stale: after a Claude `/clear` every hook from the still-running worker degrades to the card-level actor, and dropping those would hide every later permission prompt. A lookup error also projects.
    async fn hook_is_provably_stale(&self, card_id: &CardId, actor: &ActorId) -> bool {
        let session_id = match actor {
            ActorId::AiCodexSession(ws) | ActorId::AiClaudeSession(ws) => ws,
            _ => return false,
        };
        match self
            .repo
            .card_identity_get_by_session(session_id.as_str())
            .await
        {
            Ok(Some(identity)) if identity.card_id == *card_id => false,
            Ok(current) => {
                tracing::debug!(
                    card_id = %card_id,
                    session_id = %session_id,
                    current_card = ?current.map(|c| c.card_id),
                    "card_fsm: hook from a session that is not the card's current one; dropped as stale"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    card_id = %card_id,
                    session_id = %session_id,
                    error = %e,
                    "card_fsm: card_identity_get_by_session failed; projecting the hook unfenced"
                );
                false
            }
        }
    }

    /// Mutating entry: a new event says "card X wants to be in state Y now".
    /// Decides upgrade-vs-downgrade and (synchronously) commits or schedules.
    async fn observe(self: &Arc<Self>, card_id: CardId, target: State) {
        let mut map = self.map.lock().await;
        // The first observation of a card MUST commit (no prior overlay row), even if it happens to be `Idle`.
        let (cur, first_observation) = match map.get(&card_id) {
            Some(e) => (e.committed, false),
            None => (State::Done, true), // placeholder; severity-floor so anything is an upgrade
        };

        if first_observation || target.severity() >= cur.severity() {
            // Upgrade, same, or first observation: commit immediately and drop any pending downgrade.
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

    /// Commit a card state change: write the card-level overlay and recompute the track-scoped `any_card_needs_input` aggregate; never touches `TrackLifecycle`.
    async fn commit(&self, card_id: &CardId, state: State) {
        // Look up the owning track so the audit row carries the full ancestor chain.
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

        // Card overlay through write_with_event so the overlay row and the events row land in one transaction; `schemaVersion` is stamped so an older binary can refuse a newer row.
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
        // On lookup failure fall back to `EventScope::System`: a less-scoped event beats refusing a best-effort projection.
        let scope = match self.repo.track_get(card.track_id.as_str()).await {
            Ok(Some(w)) => EventScope::Card {
                card: card_id.clone(),
                track: w.id,
                area: w.area_id,
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

        // Track-scoped `any_card_needs_input` aggregate, OR'd at the UI layer with `TrackLifecycle`; the lifecycle column itself is untouched.
        self.recompute_track_needs_input(&card.track_id).await;
    }

    /// Aggregate every card under `track_id` into one boolean `any_card_needs_input` overlay; idempotent (no write, no event when unchanged).
    /// Cards come from `repo.cards_by_track`, and the map lock is taken once for an in-memory lookup — never across IO. Concurrent commits race the idempotency check; `overlay_upsert_tx`'s `ON CONFLICT DO UPDATE` resolves it (last writer wins).
    async fn recompute_track_needs_input(&self, track_id: &TrackId) {
        // 1. The canonical card set for this track; the FSM map is only a live cache for those cards.
        let cards = match self.repo.cards_by_track(track_id.as_str()).await {
            Ok(cs) => cs,
            Err(e) => {
                tracing::warn!(
                    track_id = %track_id,
                    error = %e,
                    "card_fsm: cards_by_track failed during needs_input recompute"
                );
                return;
            }
        };

        // 2. Lock the map briefly for an in-memory lookup only — no awaits inside the critical section. Cards without an entry contribute nothing.
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

        // 3. Idempotency: skip the write when the boolean is unchanged, or every per-card transition would churn a track overlay event.
        let existing = match self.repo.overlays_for("track", track_id.as_str()).await {
            Ok(rows) => rows
                .into_iter()
                .find(|o| o.kind == "any_card_needs_input" && o.plugin_id == KERNEL_PLUGIN_ID),
            Err(e) => {
                tracing::warn!(
                    track_id = %track_id,
                    error = %e,
                    "card_fsm: overlays_for(track) failed during needs_input recompute"
                );
                return;
            }
        };
        if let Some(prev) = &existing
            && prev.payload.get("value").and_then(|v| v.as_bool()) == Some(needs_input)
        {
            return; // unchanged — skip the write
        }

        // On failure fall back to `EventScope::System`, as for the per-card overlay write.
        let scope = match self.repo.track_get(track_id.as_str()).await {
            Ok(Some(w)) => EventScope::Track {
                track: w.id,
                area: w.area_id,
            },
            _ => EventScope::System,
        };
        let payload = json!({
            "schemaVersion": OVERLAY_ANY_CARD_NEEDS_INPUT_SCHEMA_VERSION,
            "value": needs_input,
        });
        let new_overlay = NewOverlay {
            plugin_id: KERNEL_PLUGIN_ID.to_string(),
            entity_kind: "track".to_string(),
            entity_id: track_id.to_string(),
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
                track_id = %track_id,
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
        // A finished turn is quiet, not "waiting on you".
        assert_eq!(codex_kind_to_state("hook.codex.stop"), Some(State::Idle));
        assert_ne!(
            codex_kind_to_state("hook.codex.stop"),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            codex_kind_to_state("hook.codex.permission_request"),
            Some(State::AwaitingInput)
        );
        assert_eq!(codex_kind_to_state("hook.codex.something_else"), None);
    }

    /// `Stop` is `Idle` in BOTH tables; only the permission / elicitation hooks (and whitelisted notifications) are attention.
    #[test]
    fn claude_stop_is_idle_not_attention() {
        assert_eq!(
            claude_kind_to_state("hook.claude.stop", &Value::Null),
            Some(State::Idle)
        );
        assert_ne!(
            claude_kind_to_state("hook.claude.stop", &Value::Null),
            Some(State::AwaitingInput)
        );
        assert_eq!(codex_kind_to_state("hook.codex.stop"), Some(State::Idle));
        let stop = CLAUDE_WORKER_HOOKS
            .iter()
            .find(|h| h.event_name == "Stop")
            .expect("Stop row");
        assert_eq!(stop.state, Some(State::Idle));
        let codex_stop = CODEX_WORKER_HOOKS
            .iter()
            .find(|h| h.event_name == "Stop")
            .expect("codex Stop row");
        assert_eq!(codex_stop.state, Some(State::Idle));
    }

    #[test]
    fn notification_permission_prompt_is_awaiting_input() {
        for t in [
            "permission_prompt",
            "elicitation_dialog",
            "elicitation_url_dialog",
            "agent_needs_input",
        ] {
            assert_eq!(
                claude_kind_to_state(
                    "hook.claude.notification",
                    &json!({ "hook_event_name": "Notification", "notification_type": t })
                ),
                Some(State::AwaitingInput),
                "notification_type={t}"
            );
        }
        for t in [
            "idle_prompt",
            "auth_success",
            "elicitation_complete",
            "elicitation_response",
            "agent_completed",
            "quota_auto_resume_fired",
            "quota_auto_resume_stale",
            "quota_auto_resume_disabled",
            "something_new",
        ] {
            assert_eq!(
                claude_kind_to_state(
                    "hook.claude.notification",
                    &json!({ "hook_event_name": "Notification", "notification_type": t })
                ),
                None,
                "notification_type={t} must not project"
            );
        }
        assert_eq!(
            claude_kind_to_state("hook.claude.notification", &Value::Null),
            None,
            "absent payload"
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.notification", &json!({ "message": "x" })),
            None,
            "absent notification_type"
        );
        assert_eq!(
            claude_kind_to_state(
                "hook.claude.notification",
                &json!({ "notification_type": ["permission_prompt"] })
            ),
            None,
            "non-string notification_type"
        );
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
            None,
            "#1722: sub-agent / task hooks are vocabulary only"
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.permission_request", &Value::Null),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            claude_kind_to_state(
                "hook.claude.notification",
                &json!({ "notification_type": "permission_prompt" })
            ),
            Some(State::AwaitingInput)
        );
        assert_eq!(
            claude_kind_to_state("hook.claude.notification", &Value::Null),
            None,
            "#1722: a Notification without a whitelisted subtype is a no-op"
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
            Some(State::Idle)
        );
        assert_eq!(codex_kind_to_state("hook.codex.stop"), Some(State::Idle));
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

    /// Every table row projects exactly its `state`, and exactly the four sub-agent / task rows are `None`.
    #[test]
    fn every_registered_hook_projects_to_its_table_state() {
        let mut none_rows: Vec<&str> = Vec::new();
        for h in CLAUDE_WORKER_HOOKS {
            let kind = format!(
                "hook.claude.{}",
                crate::routes::codex::to_snake_case(h.event_name)
            );
            let payload = if h.event_name == "Notification" {
                json!({ "notification_type": "permission_prompt" })
            } else {
                Value::Null
            };
            assert_eq!(
                claude_kind_to_state(&kind, &payload),
                h.state,
                "hook {} (kind {kind}) must project to {:?}",
                h.event_name,
                h.state
            );
            if h.state.is_none() {
                none_rows.push(h.event_name);
            }
        }
        assert_eq!(
            none_rows,
            [
                "SubagentStart",
                "SubagentStop",
                "TaskCreated",
                "TaskCompleted"
            ],
            "exactly the four sub-agent / task rows are vocabulary-only"
        );
        for h in CODEX_WORKER_HOOKS {
            let kind = format!(
                "hook.codex.{}",
                crate::routes::codex::to_snake_case(h.event_name)
            );
            assert_eq!(codex_kind_to_state(&kind), h.state);
            assert!(h.state.is_some(), "codex row {} projects", h.event_name);
        }
        // The two attention hooks are still projected.
        for name in ["PermissionDenied", "Elicitation"] {
            let kind = format!("hook.claude.{}", crate::routes::codex::to_snake_case(name));
            assert_eq!(
                claude_kind_to_state(&kind, &serde_json::Value::Null),
                Some(State::AwaitingInput)
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

    // Tests seed via raw sync-domain writes, so they need the full `Repo`; `spawn` takes the narrowed `Arc<dyn RepoEventWrite>` via trait-object coercion.
    use crate::db::Repo;
    use crate::db::sqlite::SqlxRepo;
    use crate::ids::TrackId;
    use crate::model::{NewArea, NewCard, NewTrack};
    use serde_json::Value;
    use std::time::Duration as StdDuration;

    async fn setup() -> (Arc<dyn Repo>, EventBus, TrackId, CardId) {
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let bus = EventBus::new();
        let area = repo
            .area_create(NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .unwrap();
        (repo, bus, track.id, card.id)
    }

    // Overlay-poll ceiling, ~15s: a downgrade is held `DOWNGRADE_QUIET_MS` on a detached timer before its commit even starts, so a short budget flakes under CI starvation. The poll returns as soon as the value matches.
    const OVERLAY_POLL_ATTEMPTS: usize = 600;

    async fn wait_for_track_needs_input(repo: &Arc<dyn Repo>, track_id: &TrackId, want: bool) {
        for _ in 0..OVERLAY_POLL_ATTEMPTS {
            let got = repo
                .overlays_for("track", track_id.as_str())
                .await
                .unwrap()
                .into_iter()
                .find(|o| o.kind == "any_card_needs_input")
                .and_then(|o| o.payload.get("value").cloned());
            if got == Some(Value::Bool(want)) {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }
        let rows = repo.overlays_for("track", track_id.as_str()).await.unwrap();
        panic!("timed out waiting for any_card_needs_input={want}; overlays={rows:?}");
    }

    async fn wait_for_card_status(repo: &Arc<dyn Repo>, card_id: &CardId, want: &str) {
        for _ in 0..OVERLAY_POLL_ATTEMPTS {
            let got = repo
                .overlays_for("card", card_id.as_str())
                .await
                .unwrap()
                .into_iter()
                .find(|o| o.kind == "status")
                .and_then(|o| o.payload.get("state").cloned());
            if got == Some(Value::String(want.to_string())) {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }
        let rows = repo.overlays_for("card", card_id.as_str()).await.unwrap();
        panic!("timed out waiting for card status {want}; overlays={rows:?}");
    }

    #[tokio::test]
    async fn upgrade_commits_immediately() {
        let (repo, bus, track_id, card_id) = setup().await;
        // A sentinel card driven to Working AFTER the card under test, as an order-based barrier: the FSM handles events strictly in order, so the sentinel cannot be handled until the card under test is handled in full.
        let card_b = repo
            .card_create(NewCard {
                track_id: track_id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .unwrap();
        // Subscribe BEFORE spawn so we observe every overlay event in order.
        let mut rx = bus.subscribe();
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
        // Give the spawn a tick to subscribe.
        tokio::task::yield_now().await;

        for c in [&card_id, &card_b.id] {
            bus.emit(
                ActorId::AiCodex(c.clone()),
                Event::CodexHook {
                    card_id: c.clone(),
                    kind: "hook.codex.pre_tool_use".into(),
                    hook_idempotency_key: "hook-key".into(),
                    payload: Value::Null,
                },
            );
        }

        // Read overlay events IN ORDER until the sentinel's status (no wall-clock budget): card A's `status=Working` must arrive BEFORE the sentinel's (an inline commit, not the downgrade timer),
        // and no track-level `kind == "status"` overlay may appear. Only an asymmetric deferral is caught; a uniform one preserves order (a paused clock is incompatible with the sqlx pool).
        let timed = tokio::time::timeout(StdDuration::from_secs(15), async {
            let mut seen_a_working = false;
            loop {
                let env = match rx.recv().await {
                    Ok(env) => env,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        panic!("overlay event receiver lagged ({n} frames) before sentinel");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        panic!("bus closed before sentinel overlay observed");
                    }
                };
                match &env.event {
                    Event::OverlaySet(o) if o.kind == "status" && o.entity_kind == "track" => {
                        panic!(
                            "card_fsm must not write track-level status overlays; got {:?}",
                            o
                        );
                    }
                    Event::OverlaySet(o)
                        if o.kind == "status"
                            && o.entity_kind == "card"
                            && o.entity_id == card_id.to_string() =>
                    {
                        assert_eq!(
                            o.payload.get("state").and_then(|v| v.as_str()),
                            Some("Working")
                        );
                        seen_a_working = true;
                    }
                    Event::OverlaySet(o)
                        if o.kind == "status"
                            && o.entity_kind == "card"
                            && o.entity_id == card_b.id.to_string() =>
                    {
                        assert!(
                            seen_a_working,
                            "upgrade was not immediate: sentinel committed before the card \
                             under test's Working status (a first-observation upgrade must \
                             commit inline, not behind the downgrade timer)"
                        );
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await;
        timed.expect("timed out waiting for sentinel (card B Working) overlay");
    }

    #[tokio::test]
    async fn awaiting_input_beats_working() {
        let (repo, bus, _track_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        wait_for_card_status(&repo, &card_id, "Working").await;
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        // permission_request → AwaitingInput, not Idle.
        wait_for_card_status(&repo, &card_id, "AwaitingInput").await;
    }

    #[tokio::test]
    async fn post_tool_use_stays_working() {
        // post_tool_use maps to Working: between tool calls the agent is still reasoning, so the card must not flicker to Idle.
        let (repo, bus, _track_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        wait_for_card_status(&repo, &card_id, "Working").await;
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.post_tool_use".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );

        // Past the debounce window — still Working, never flickers.
        tokio::time::sleep(StdDuration::from_millis(900)).await;
        wait_for_card_status(&repo, &card_id, "Working").await;

        // stop → Idle commits the turn boundary (a downgrade from Working,
        // held DOWNGRADE_QUIET_MS; the poll ceiling covers it).
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.stop".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        wait_for_card_status(&repo, &card_id, "Idle").await;
    }

    #[tokio::test]
    async fn needs_input_overlay_fires_on_awaiting_input() {
        let (repo, bus, track_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );

        wait_for_track_needs_input(&repo, &track_id, true).await;
        let o = repo
            .overlays_for("track", track_id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.kind == "any_card_needs_input")
            .expect("any_card_needs_input overlay written");
        assert_eq!(o.payload["value"], Value::Bool(true));
        assert_eq!(o.plugin_id, KERNEL_PLUGIN_ID);
    }

    #[tokio::test]
    async fn needs_input_overlay_clears_on_working() {
        let (repo, bus, track_id, card_id) = setup().await;
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        // Drive into AwaitingInput first.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        // Sanity: overlay is true.
        wait_for_track_needs_input(&repo, &track_id, true).await;

        // Working has lower severity than AwaitingInput, so the 750ms downgrade window holds it.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        wait_for_track_needs_input(&repo, &track_id, false).await;
    }

    #[tokio::test]
    async fn needs_input_overlay_is_idempotent() {
        let (repo, bus, track_id, card_id) = setup().await;
        // Two more codex cards as deterministic sentinels, created BEFORE the FSM spawns: B exercises the recompute idempotency path (true → true writes nothing),
        // and C is the terminal marker — by in-order processing its status overlay cannot appear until B has been handled in full, recompute included.
        let new_codex_card = || NewCard {
            track_id: track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        };
        let card_b = repo.card_create(new_codex_card()).await.unwrap();
        let card_c = repo.card_create(new_codex_card()).await.unwrap();
        // Subscribe BEFORE spawn so every overlay event is captured.
        let mut rx = bus.subscribe();
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        // First emit → card A AwaitingInput, track overlay flips false → true: exactly ONE track write.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );

        // Second emit → same state, track aggregate unchanged (true → true): the idempotency guard must suppress the track write.
        bus.emit(
            ActorId::AiCodex(card_id.clone()),
            Event::CodexHook {
                card_id: card_id.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );

        // Sentinels: drive B then C to AwaitingInput. Neither is handled until A's two emits are fully processed, and C not until B is.
        // The count is finalized on C's status, not B's: `commit()` writes the card status BEFORE the recompute, so B's recompute has run only once C's status is seen.
        for sentinel in [&card_b, &card_c] {
            bus.emit(
                ActorId::AiCodex(sentinel.id.clone()),
                Event::CodexHook {
                    card_id: sentinel.id.clone(),
                    kind: "hook.codex.permission_request".into(),
                    hook_idempotency_key: "hook-key".into(),
                    payload: Value::Null,
                },
            );
        }

        // Count track-scoped `any_card_needs_input` writes until card C's status overlay; there must be EXACTLY ONE. Bounded by an outer timeout so a broken assumption panics instead of hanging.
        let track_overlay_writes = tokio::time::timeout(StdDuration::from_secs(15), async {
            let mut writes = 0usize;
            loop {
                let env = match rx.recv().await {
                    Ok(env) => env,
                    // A lagged receiver dropped frames, so the count would be unreliable; fail loudly.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        panic!("overlay event receiver lagged ({n} frames) before sentinel");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        panic!("bus closed before sentinel overlay observed");
                    }
                };
                match &env.event {
                    Event::OverlaySet(o)
                        if o.kind == "any_card_needs_input"
                            && o.entity_kind == "track"
                            && o.entity_id == track_id.to_string() =>
                    {
                        writes += 1;
                    }
                    // Terminal marker: card C's status overlay; the count already includes anything B's recompute emitted.
                    Event::OverlaySet(o)
                        if o.kind == "status"
                            && o.entity_kind == "card"
                            && o.entity_id == card_c.id.to_string()
                            && o.payload.get("state").and_then(|v| v.as_str())
                                == Some("AwaitingInput") =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
            writes
        })
        .await
        .expect("timed out waiting for terminal marker (card C AwaitingInput) overlay");
        assert_eq!(
            track_overlay_writes, 1,
            "expected exactly one any_card_needs_input write (idempotent), got {track_overlay_writes}"
        );
    }

    #[tokio::test]
    async fn needs_input_overlay_ors_multiple_cards() {
        // Driving ONE of two cards to AwaitingInput lights the track overlay while the other stays Working; both Working clears it.
        let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let bus = EventBus::new();
        let area = repo
            .area_create(NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card_a = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            })
            .await
            .unwrap();
        let card_b = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
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
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
        tokio::task::yield_now().await;

        // A → Working, B → AwaitingInput. Track overlay should be true.
        bus.emit(
            ActorId::AiCodex(card_a.id.clone()),
            Event::CodexHook {
                card_id: card_a.id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        bus.emit(
            ActorId::AiCodex(card_b.id.clone()),
            Event::CodexHook {
                card_id: card_b.id.clone(),
                kind: "hook.codex.permission_request".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        wait_for_track_needs_input(&repo, &track.id, true).await;

        // Flip B back to Working — past the 750ms downgrade window.
        bus.emit(
            ActorId::AiCodex(card_b.id.clone()),
            Event::CodexHook {
                card_id: card_b.id.clone(),
                kind: "hook.codex.pre_tool_use".into(),
                hook_idempotency_key: "hook-key".into(),
                payload: Value::Null,
            },
        );
        wait_for_track_needs_input(&repo, &track.id, false).await;
    }

    fn spawn_fsm(repo: &Arc<dyn Repo>, bus: &EventBus) {
        spawn(
            repo.clone(),
            bus.clone(),
            WriteContext::new(
                crate::card_role_cache::CardRoleCache::new(),
                crate::track_area_cache::TrackAreaCache::new(),
            ),
        );
    }

    fn claude_hook(card_id: &CardId, bare: &str, payload: Value) -> Event {
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: format!("hook.claude.{bare}"),
            hook_idempotency_key: format!("hook-key-{bare}"),
            payload,
        }
    }

    /// Card-`status` overlay states for `card_id` off `rx`, in order, until `until` is observed (15 s outer timeout); returns every state seen, `until` included.
    async fn card_status_sequence_until(
        rx: &mut tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
        card_id: &CardId,
        until: &str,
    ) -> Vec<String> {
        tokio::time::timeout(StdDuration::from_secs(15), async {
            let mut seen = Vec::new();
            loop {
                let env = match rx.recv().await {
                    Ok(env) => env,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        panic!("overlay event receiver lagged ({n} frames)");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        panic!("bus closed before `{until}` was observed");
                    }
                };
                if let Event::OverlaySet(o) = &env.event
                    && o.kind == "status"
                    && o.entity_kind == "card"
                    && o.entity_id == card_id.to_string()
                {
                    let state = o.payload["state"].as_str().unwrap_or("?").to_string();
                    seen.push(state.clone());
                    if state == until {
                        return seen;
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for card status `{until}`"))
    }

    /// `user_prompt_submit → stop → notification{idle_prompt} → subagent_stop` ends `Idle` and never lifts the card back to `Working` or `AwaitingInput` along the way.
    #[tokio::test]
    async fn stop_then_idle_prompt_then_subagent_stop_ends_idle() {
        let (repo, bus, _track_id, card_id) = setup().await;
        let mut rx = bus.subscribe();
        spawn_fsm(&repo, &bus);
        tokio::task::yield_now().await;

        let actor = ActorId::AiClaude(card_id.clone());
        bus.emit(
            actor.clone(),
            claude_hook(&card_id, "user_prompt_submit", Value::Null),
        );
        bus.emit(actor.clone(), claude_hook(&card_id, "stop", Value::Null));
        bus.emit(
            actor.clone(),
            claude_hook(
                &card_id,
                "notification",
                json!({ "hook_event_name": "Notification", "notification_type": "idle_prompt" }),
            ),
        );
        bus.emit(actor, claude_hook(&card_id, "subagent_stop", Value::Null));

        // `Idle` is a downgrade from `Working`, so it lands only after DOWNGRADE_QUIET_MS; the sequence read proves nothing else was committed in between.
        let seen = card_status_sequence_until(&mut rx, &card_id, "Idle").await;
        assert_eq!(seen, ["Working", "Idle"], "card status sequence");
        wait_for_card_status(&repo, &card_id, "Idle").await;
    }

    fn claude_session(
        id: &str,
        track_id: &TrackId,
        card_id: &CardId,
        state: calm_types::worker::WorkerSessionState,
    ) -> calm_types::worker::WorkerSession {
        use calm_types::worker::{
            LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
            WorkerSessionId,
        };
        WorkerSession {
            id: WorkerSessionId::from(id),
            track_id: track_id.clone(),
            provider: WorkerProviderKind::Claude,
            mode: SessionMode::Ephemeral,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state,
            mcp_token_hash: None,
            thread_id: None,
            agent_session_id: Some(format!("native-{id}")),
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(card_id.clone()),
            handle_state_json: None,
            liveness: LivenessTag::Unknown,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        }
    }

    /// Insert `session` and, when `link` is set, point `cards.session_id` at it.
    async fn insert_session(
        repo: &SqlxRepo,
        session: calm_types::worker::WorkerSession,
        link: bool,
    ) {
        use crate::db::sqlite::{begin_immediate_tx, session_insert_tx};
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let session_id = session.id.clone();
        let card_id = session.card_id.clone().expect("fixture session has a card");
        session_insert_tx(&mut tx, session).await.unwrap();
        if link {
            sqlx::query("UPDATE cards SET session_id = ?1 WHERE id = ?2")
                .bind(session_id.as_str())
                .bind(card_id.as_str())
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    }

    /// Two claude worker cards under one track. The card under test has
    /// `cards.session_id = s2` (active) and an exited predecessor `s0`; the
    /// sibling card owns the other active session `s1`.
    struct FenceFixture {
        repo: Arc<dyn Repo>,
        bus: EventBus,
        card: CardId,
    }

    async fn fence_fixture() -> FenceFixture {
        use calm_types::worker::WorkerSessionState;
        let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let repo: Arc<dyn Repo> = sqlx_repo.clone();
        let bus = EventBus::new();
        let area = repo
            .area_create(NewArea {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let new_claude_card = || NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "claude".into(),
            sort: None,
            payload: Value::Null,
        };
        let card = repo.card_create(new_claude_card()).await.unwrap();
        let sibling = repo.card_create(new_claude_card()).await.unwrap();
        // s0: the card's exited predecessor (restart shape, F2.26).
        insert_session(
            &sqlx_repo,
            claude_session("s0", &track.id, &card.id, WorkerSessionState::Exited),
            false,
        )
        .await;
        // s2: the card's current active session.
        insert_session(
            &sqlx_repo,
            claude_session("s2", &track.id, &card.id, WorkerSessionState::Running),
            true,
        )
        .await;
        // s1: the sibling card's active session.
        insert_session(
            &sqlx_repo,
            claude_session("s1", &track.id, &sibling.id, WorkerSessionState::Running),
            true,
        )
        .await;
        FenceFixture {
            repo,
            bus,
            card: card.id,
        }
    }

    /// A hook whose actor names an active session that is NOT the card's current one, or the card's exited predecessor, produces no overlay; the card-level `stop` sentinel must be the FIRST status overlay.
    #[tokio::test]
    async fn hook_from_other_active_session_is_ignored() {
        use calm_types::worker::WorkerSessionId;
        let f = fence_fixture().await;
        let mut rx = f.bus.subscribe();
        spawn_fsm(&f.repo, &f.bus);
        tokio::task::yield_now().await;

        f.bus.emit(
            ActorId::AiClaudeSession(WorkerSessionId::from("s1")),
            claude_hook(&f.card, "pre_tool_use", Value::Null),
        );
        f.bus.emit(
            ActorId::AiClaudeSession(WorkerSessionId::from("s0")),
            claude_hook(&f.card, "permission_request", Value::Null),
        );
        // Sentinel: card-level actor, first observation ⇒ commits inline.
        f.bus.emit(
            ActorId::AiClaude(f.card.clone()),
            claude_hook(&f.card, "stop", Value::Null),
        );

        let seen = card_status_sequence_until(&mut rx, &f.card, "Idle").await;
        assert_eq!(
            seen,
            ["Idle"],
            "stale-session hooks must not have written a status overlay before the sentinel"
        );
    }

    /// The card-level fallback actor still projects — dropping it would hide permission prompts after a `/clear` rotates the native session id.
    #[tokio::test]
    async fn card_level_hook_still_projects() {
        let f = fence_fixture().await;
        spawn_fsm(&f.repo, &f.bus);
        tokio::task::yield_now().await;

        f.bus.emit(
            ActorId::AiClaude(f.card.clone()),
            claude_hook(&f.card, "permission_request", Value::Null),
        );
        wait_for_card_status(&f.repo, &f.card, "AwaitingInput").await;
    }

    /// The card's CURRENT session passes the fence.
    #[tokio::test]
    async fn hook_from_current_session_projects() {
        use calm_types::worker::WorkerSessionId;
        let f = fence_fixture().await;
        spawn_fsm(&f.repo, &f.bus);
        tokio::task::yield_now().await;

        f.bus.emit(
            ActorId::AiClaudeSession(WorkerSessionId::from("s2")),
            claude_hook(&f.card, "permission_request", Value::Null),
        );
        wait_for_card_status(&f.repo, &f.card, "AwaitingInput").await;
    }
}
