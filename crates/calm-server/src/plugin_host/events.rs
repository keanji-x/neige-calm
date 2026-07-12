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
        // Optional filters require metadata; skip derivation for the common
        // events-only subscriber path.
        if self.plugin_id.is_none() && self.entity_kind.is_none() && self.entity_id.is_none() {
            return true;
        }
        let meta = ev.metadata();
        if let Some(pid) = &self.plugin_id
            && meta.plugin_id.as_deref() != Some(pid.as_str())
        {
            return false;
        }
        if let Some(ek) = &self.entity_kind
            && meta.entity_kind.as_deref() != Some(ek.as_str())
        {
            return false;
        }
        if let Some(eid) = &self.entity_id
            && meta.entity_id.as_deref() != Some(eid.as_str())
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
            workflow_id: None,
            purpose: None,
            workflow_input: None,
            terminal_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn card(id: &str, wave_id: &str, kind: &str) -> Card {
        Card {
            id: id.into(),
            wave_id: wave_id.into(),
            title: None,
            kind: kind.into(),
            sort: 1.0,
            payload: json!({}),
            runtime: None,
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
            hook_idempotency_key: "hook-key".into(),
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
        assert!(
            !f.matches(&Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
                wave("w", "c"),
                None
            ),))
        );
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
