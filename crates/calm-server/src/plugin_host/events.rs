//! Event-subscription filter matching.
//!
//! Plugins subscribe via `neige.event.subscribe { filter }`. The filter is a
//! conjunction of optional clauses: event-name (glob), plugin id, entity kind,
//! entity id. This module owns the `matches()` predicate the per-subscription
//! bridge task runs on every broadcast.
//!
//! Wire shape (design doc §3.2 — `neige.events.subscribe`):
//! ```jsonc
//! {
//!   "filter": {
//!     "events":      ["card.added", "overlay.*"],   // empty = all
//!     "plugin_id":   "dev.example",                  // optional
//!     "entity_kind": "wave",                         // optional
//!     "entity_id":   "uuid"                          // optional
//!   }
//! }
//! ```
//!
//! Glob semantics on `events`: very narrow — we only support `"*"` (match
//! anything) and exact-name matches against the kernel's internal discriminant
//! strings (`"card.added"`, `"overlay.set"`, etc.). Trailing-`*` segment
//! globs (`"card.*"`) are also accepted because they cost ~3 lines and the
//! design doc explicitly mentions glob-style. We intentionally do NOT pull a
//! glob crate — the filter input arrives from plugin processes we don't
//! audit, and a regex DoS would be a Slice-C-shaped foot-gun.

use serde::Deserialize;

use super::glob::glob_matches;
use crate::event::Event;

/// The filter clause the plugin sends. All fields optional; missing == match.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionFilter {
    /// Event-name globs. Empty list = match every event. Each entry is matched
    /// independently — any match advances. Supported shapes: literal name,
    /// `"*"` (everything), `"<prefix>.*"` (one-segment wildcard suffix).
    #[serde(default)]
    pub events: Vec<String>,

    /// If set, only events carrying a plugin_id field equal to this value
    /// match. Applies to `overlay.*` and `plugin.state`; other events have
    /// no plugin_id and will fail the filter when this clause is present.
    #[serde(default)]
    pub plugin_id: Option<String>,

    /// If set, only events whose entity is of this kind match. Currently
    /// meaningful for `overlay.*` (carries `entity_kind` directly) and for
    /// `wave.*`/`card.*` (mapped to `"wave"` / `"card"` respectively).
    #[serde(default)]
    pub entity_kind: Option<String>,

    /// If set, only events touching this specific entity id match. Comparison
    /// is exact-string against the kernel's id columns.
    #[serde(default)]
    pub entity_id: Option<String>,
}

impl SubscriptionFilter {
    /// Predicate the bridge task runs on every broadcasted event.
    pub fn matches(&self, ev: &Event) -> bool {
        let name = event_name(ev);
        if !self.events.is_empty() && !self.events.iter().any(|g| glob_matches(g, name)) {
            return false;
        }
        if let Some(pid) = &self.plugin_id
            && event_plugin_id(ev).as_deref() != Some(pid.as_str())
        {
            return false;
        }
        if let Some(ek) = &self.entity_kind
            && event_entity_kind(ev).as_deref() != Some(ek.as_str())
        {
            return false;
        }
        if let Some(eid) = &self.entity_id
            && event_entity_id(ev).as_deref() != Some(eid.as_str())
        {
            return false;
        }
        true
    }
}

/// The dotted wire name for an event, identical to the `ev` field the WS
/// serializer emits (`crate::event::Event`'s `#[serde(tag = "ev")]` rename).
/// Delegates to [`Event::kind_tag`] so adding a new variant only needs one
/// site (the enum match in `event.rs`) — see PR4 of #136.
fn event_name(ev: &Event) -> &'static str {
    ev.kind_tag()
}

/// Plugin id carried by the event, if any. Only overlay events and plugin.state
/// carry one in the current vocabulary.
///
/// PR5 of #136 hazard H1: every variant of `Event` is enumerated explicitly
/// (no `_ =>` catch-all) so the compiler nags when a new variant lands and
/// forces the author of that variant to make a deliberate
/// has-a-plugin-id-or-not decision rather than silently inheriting `None`.
/// The 4 PR4 dispatcher/task-lifecycle variants (`codex.job_requested`,
/// `terminal.job_requested`, `task.completed`, `task.failed`) explicitly
/// return `None` — they're kernel-internal signals with no plugin attribution.
fn event_plugin_id(ev: &Event) -> Option<String> {
    match ev {
        Event::OverlaySet(o) => Some(o.plugin_id.clone()),
        Event::OverlayDeleted { plugin_id, .. } => Some(plugin_id.clone()),
        Event::PluginState { id, .. } => Some(id.clone()),

        // Events without a plugin attribution — explicit-arm so the
        // compiler flags any new variant for a deliberate decision.
        Event::CoveUpdated(_)
        | Event::CoveDeleted { .. }
        | Event::WaveUpdated(_)
        | Event::WaveDeleted { .. }
        | Event::WaveLifecycleChanged { .. }
        | Event::CardAdded(_)
        | Event::CardUpdated(_)
        | Event::CardDeleted { .. }
        | Event::WaveReportEdited { .. }
        | Event::TerminalDeleted { .. }
        | Event::CodexHook { .. }
        | Event::ClaudeHook { .. }
        | Event::CodexJobRequested { .. }
        | Event::TerminalJobRequested { .. }
        | Event::TaskCompleted { .. }
        | Event::TaskFailed { .. }
        // #318 INV-1 (b) — boot-takeover abandonment is a kernel-internal
        // signal, no plugin attribution.
        | Event::SpecPushAbandoned { .. } => None,
    }
}

/// Entity kind ("wave" | "card") the event touches, if any. We map the
/// concrete event variants onto the two kinds the kernel knows about; events
/// that don't fit (e.g. cove.*, plugin.state) return None and therefore fail
/// an `entity_kind` clause when one is present.
///
/// PR5 of #136 hazard H1: explicit-arm every variant (no `_ =>` catch-all)
/// so the compiler flags when a new variant lands. The 4 PR4 variants are
/// dispatcher/task-lifecycle signals with no `wave`/`card` entity surface
/// — plugins that want to filter on those subscribe via the `events` glob
/// clause and omit `entity_kind` / `entity_id`. Matches the parallel
/// explicit-arm in [`event_entity_id`].
fn event_entity_kind(ev: &Event) -> Option<String> {
    match ev {
        Event::WaveUpdated(_) | Event::WaveDeleted { .. } | Event::WaveLifecycleChanged { .. } => {
            Some("wave".into())
        }
        // #318 INV-1 (b) — wave-scoped abandonment signal; routes through
        // plugin filters that select on `entity_kind="wave"`.
        Event::SpecPushAbandoned { .. } => Some("wave".into()),
        Event::CardAdded(_) | Event::CardUpdated(_) | Event::CardDeleted { .. } => {
            Some("card".into())
        }
        // Issue #247 PR2 — wave-report edit log is scoped to the report
        // card; surface "card" so plugin filters with
        // `entity_kind="card"` see structured edits alongside the
        // generic `card.updated` companion event.
        Event::WaveReportEdited { .. } => Some("card".into()),
        Event::OverlaySet(o) => Some(o.entity_kind.clone()),
        Event::OverlayDeleted { entity_kind, .. } => Some(entity_kind.clone()),
        Event::CodexHook { .. } | Event::ClaudeHook { .. } => Some("card".into()),

        // No entity-kind surface — explicit-arm so future variants force a
        // deliberate decision (cove updates don't surface "cove" because
        // plugin entity_kind today is exactly {"wave","card"}; the PR4
        // dispatcher variants don't have an entity at all).
        Event::CoveUpdated(_)
        | Event::CoveDeleted { .. }
        | Event::TerminalDeleted { .. }
        | Event::PluginState { .. }
        | Event::CodexJobRequested { .. }
        | Event::TerminalJobRequested { .. }
        | Event::TaskCompleted { .. }
        | Event::TaskFailed { .. } => None,
    }
}

/// Entity id the event touches. Same coverage logic as `event_entity_kind`.
fn event_entity_id(ev: &Event) -> Option<String> {
    match ev {
        Event::CoveUpdated(c) => Some(c.id.to_string()),
        Event::CoveDeleted { id } => Some(id.to_string()),
        Event::WaveUpdated(w) => Some(w.id.to_string()),
        Event::WaveDeleted { id, .. } => Some(id.to_string()),
        Event::WaveLifecycleChanged { id, .. } => Some(id.to_string()),
        // #318 INV-1 (b) — entity id is the abandoned wave's id, matching
        // the entity_kind="wave" arm above.
        Event::SpecPushAbandoned { wave_id, .. } => Some(wave_id.to_string()),
        Event::CardAdded(c) | Event::CardUpdated(c) => Some(c.id.to_string()),
        Event::CardDeleted { id, .. } => Some(id.to_string()),
        // Issue #247 PR2 — entity id is the report card's id, matching
        // the entity_kind="card" decision above.
        Event::WaveReportEdited { card_id, .. } => Some(card_id.to_string()),
        Event::OverlaySet(o) => Some(o.entity_id.clone()),
        Event::OverlayDeleted { entity_id, .. } => Some(entity_id.clone()),
        Event::PluginState { id, .. } => Some(id.clone()),
        // Codex hooks are scoped to a card_id, mirroring card.* events.
        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => {
            Some(card_id.to_string())
        }
        // Terminal-deleted: id IS the terminal id; we don't expose a
        // "terminal" entity_kind on the filter API, so this only matches
        // filters that omit `entity_kind` / `entity_id` or set them via
        // the `events` clause.
        Event::TerminalDeleted { id, .. } => Some(id.clone()),
        // PR4 of #136 — kernel-internal dispatcher / task-lifecycle
        // signals carry no entity id on the payload (the
        // BroadcastEnvelope holds the scope instead). Plugins that want
        // to subscribe to these must omit `entity_id` / `entity_kind`
        // and filter via the `events` glob clause.
        Event::CodexJobRequested { .. }
        | Event::TerminalJobRequested { .. }
        | Event::TaskCompleted { .. }
        | Event::TaskFailed { .. } => None,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Card, Cove, CoveKind, Overlay, Wave};
    use serde_json::json;

    fn cove(id: &str) -> Cove {
        Cove {
            id: id.into(),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: CoveKind::User,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn wave(id: &str, cove_id: &str) -> Wave {
        Wave {
            id: id.into(),
            cove_id: cove_id.into(),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: crate::model::WaveLifecycle::Draft,
            cwd: String::new(),
            terminal_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn card(id: &str, wave_id: &str, kind: &str) -> Card {
        Card {
            id: id.into(),
            wave_id: wave_id.into(),
            kind: kind.into(),
            sort: 1.0,
            payload: json!({}),
            deletable: true,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn overlay(plugin_id: &str, entity_kind: &str, entity_id: &str, kind: &str) -> Overlay {
        Overlay {
            id: "o1".into(),
            plugin_id: plugin_id.into(),
            entity_kind: entity_kind.into(),
            entity_id: entity_id.into(),
            kind: kind.into(),
            payload: json!({}),
            updated_at: 0,
        }
    }
    fn claude_hook(card_id: &str) -> Event {
        Event::ClaudeHook {
            card_id: card_id.into(),
            kind: "hook.claude.stop".into(),
            payload: json!({}),
        }
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = SubscriptionFilter::default();
        assert!(f.matches(&Event::CoveUpdated(cove("c"))));
        assert!(f.matches(&Event::CardAdded(card("k", "w", "terminal"))));
        assert!(f.matches(&Event::PluginState {
            id: "p".into(),
            state: "running".into(),
            last_error: None,
        }));
    }

    #[test]
    fn event_name_literal_match() {
        let f = SubscriptionFilter {
            events: vec!["card.added".into()],
            ..Default::default()
        };
        assert!(f.matches(&Event::CardAdded(card("k", "w", "terminal"))));
        assert!(!f.matches(&Event::CardUpdated(card("k", "w", "terminal"))));
    }

    #[test]
    fn event_name_glob_segment_wildcard() {
        let f = SubscriptionFilter {
            events: vec!["card.*".into()],
            ..Default::default()
        };
        assert!(f.matches(&Event::CardAdded(card("k", "w", "terminal"))));
        assert!(f.matches(&Event::CardUpdated(card("k", "w", "terminal"))));
        assert!(f.matches(&Event::CardDeleted {
            id: "k".into(),
            wave_id: "w".into(),
        }));
        assert!(!f.matches(&Event::WaveUpdated(wave("w", "c"))));
    }

    #[test]
    fn event_name_global_wildcard_matches_all() {
        let f = SubscriptionFilter {
            events: vec!["*".into()],
            ..Default::default()
        };
        assert!(f.matches(&Event::OverlaySet(overlay("p", "wave", "w", "status"))));
    }

    #[test]
    fn plugin_id_clause_gates_overlay() {
        let f = SubscriptionFilter {
            plugin_id: Some("p1".into()),
            ..Default::default()
        };
        assert!(f.matches(&Event::OverlaySet(overlay("p1", "wave", "w", "status"))));
        assert!(!f.matches(&Event::OverlaySet(overlay("p2", "wave", "w", "status"))));
        // Events that don't carry a plugin_id fail when this clause is present.
        assert!(!f.matches(&Event::CardAdded(card("k", "w", "terminal"))));
    }

    #[test]
    fn entity_kind_and_id_combine() {
        let f = SubscriptionFilter {
            entity_kind: Some("wave".into()),
            entity_id: Some("w-target".into()),
            ..Default::default()
        };
        assert!(f.matches(&Event::OverlaySet(overlay(
            "p", "wave", "w-target", "status"
        ))));
        // Same wave, different overlay kind on it — still matches (we don't gate kind).
        assert!(f.matches(&Event::OverlaySet(overlay(
            "p", "wave", "w-target", "progress"
        ))));
        // Wrong entity_id.
        assert!(!f.matches(&Event::OverlaySet(overlay(
            "p", "wave", "w-other", "status"
        ))));
        // Wrong entity_kind (overlay says "card", filter wants "wave").
        assert!(!f.matches(&Event::OverlaySet(overlay(
            "p", "card", "w-target", "status"
        ))));
    }

    #[test]
    fn claude_hook_maps_to_card_entity_without_plugin() {
        let ev = claude_hook("card-claude");

        let by_event = SubscriptionFilter {
            events: vec!["claude.hook".into()],
            ..Default::default()
        };
        assert!(by_event.matches(&ev));

        let by_card = SubscriptionFilter {
            entity_kind: Some("card".into()),
            entity_id: Some("card-claude".into()),
            ..Default::default()
        };
        assert!(by_card.matches(&ev));

        let by_plugin = SubscriptionFilter {
            plugin_id: Some("p".into()),
            ..Default::default()
        };
        assert!(!by_plugin.matches(&ev));
    }
}
