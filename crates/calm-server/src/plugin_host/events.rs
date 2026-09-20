//! Event-subscription filter matching: a conjunction of optional clauses (event-name glob, plugin id, entity kind, entity id).
//! Globs are deliberately narrow — literal names, `"*"`, and a trailing `.*` segment — with no glob crate, since the input comes from unaudited plugin processes.

use serde::Deserialize;

use super::glob::glob_matches;
use crate::event::Event;

/// The filter clause the plugin sends. All fields optional; missing == match.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionFilter {
    /// Event-name globs; empty = match every event. Supported shapes: literal name, `"*"`, `"<prefix>.*"`.
    #[serde(default)]
    pub events: Vec<String>,

    /// Only events carrying a matching plugin_id (`overlay.*`, `plugin.state`); other events fail the filter when this clause is present.
    #[serde(default)]
    pub plugin_id: Option<String>,

    /// `overlay.*` carries `entity_kind` directly; `track.*`/`card.*` map to `"track"` / `"card"`.
    #[serde(default)]
    pub entity_kind: Option<String>,

    #[serde(default)]
    pub entity_id: Option<String>,
}

impl SubscriptionFilter {
    pub fn matches(&self, ev: &Event) -> bool {
        let name = event_name(ev);
        if !self.events.is_empty() && !self.events.iter().any(|g| glob_matches(g, name)) {
            return false;
        }
        // Optional filters require metadata; skip derivation for the common events-only subscriber path.
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

/// The dotted wire name for an event, identical to the `ev` field the WS serializer emits.
fn event_name(ev: &Event) -> &'static str {
    ev.kind_tag()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Area, AreaKind, Card, Overlay, Track};
    use serde_json::json;

    fn area(id: &str) -> Area {
        Area {
            id: id.into(),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: AreaKind::User,
            default_template_id: None,
            default_cwd: None,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn track(id: &str, area_id: &str) -> Track {
        Track {
            id: id.into(),
            area_id: area_id.into(),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: crate::model::TrackLifecycle::Draft,
            cwd_wire_alias: String::new(),
            template_id: None,
            plugin_scope: None,
            purpose: None,
            template_input: None,
            terminal_at: None,
            recipe_id: None,
            recipe_revision: None,
            claude_permissions_policy: None,
            workspace: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }
    fn card(id: &str, track_id: &str, kind: &str) -> Card {
        Card {
            id: id.into(),
            track_id: track_id.into(),
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
        assert!(f.matches(&Event::AreaUpdated(area("c"))));
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
            track_id: "w".into(),
        }));
        assert!(!f.matches(&Event::TrackUpdated(
            crate::event::TrackUpdatedPayload::new(track("w", "c"), None),
        )));
    }

    #[test]
    fn event_name_global_wildcard_matches_all() {
        let f = SubscriptionFilter {
            events: vec!["*".into()],
            ..Default::default()
        };
        assert!(f.matches(&Event::OverlaySet(overlay("p", "track", "w", "status"))));
    }

    #[test]
    fn plugin_id_clause_gates_overlay() {
        let f = SubscriptionFilter {
            plugin_id: Some("p1".into()),
            ..Default::default()
        };
        assert!(f.matches(&Event::OverlaySet(overlay("p1", "track", "w", "status"))));
        assert!(!f.matches(&Event::OverlaySet(overlay("p2", "track", "w", "status"))));
        // Events that don't carry a plugin_id fail when this clause is present.
        assert!(!f.matches(&Event::CardAdded(card("k", "w", "terminal"))));
    }

    #[test]
    fn entity_kind_and_id_combine() {
        let f = SubscriptionFilter {
            entity_kind: Some("track".into()),
            entity_id: Some("w-target".into()),
            ..Default::default()
        };
        assert!(f.matches(&Event::OverlaySet(overlay(
            "p", "track", "w-target", "status"
        ))));
        // Same track, different overlay kind on it — still matches (we don't gate kind).
        assert!(f.matches(&Event::OverlaySet(overlay(
            "p", "track", "w-target", "progress"
        ))));
        assert!(!f.matches(&Event::OverlaySet(overlay(
            "p", "track", "w-other", "status"
        ))));
        // Wrong entity_kind (overlay says "card", filter wants "track").
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
