//! Per-plugin permission checks: the manifest's `permissions` blob turned into per-call yes/no decisions.

use super::glob::glob_matches;
use super::manifest::{Manifest, Permissions, UiPermissions};

/// Default per-plugin KV quota when the manifest doesn't pin one.
pub const DEFAULT_KV_QUOTA_BYTES: u64 = 1_048_576;

impl Permissions {
    /// May the plugin write an overlay on this entity kind? `overlay_kind` is plugin-defined and not gated.
    pub fn can_overlay_write(&self, entity_kind: &str, _overlay_kind: &str) -> bool {
        self.overlays_write.iter().any(|k| k == entity_kind)
    }

    /// May the plugin create a card with this `kind`? `"terminal"` or the plugin's own `plugin:<self_id>:` prefix.
    pub fn can_card_create(&self, kind: &str, self_id: &str) -> bool {
        if !self.cards_create {
            return false;
        }
        if kind == "terminal" {
            return true;
        }
        let prefix = format!("plugin:{self_id}:");
        kind.starts_with(&prefix)
    }

    /// Strict ownership: only `plugin:<self_id>:` cards, not even terminal cards the plugin created.
    pub fn can_card_modify(&self, card_kind: &str, self_id: &str) -> bool {
        let prefix = format!("plugin:{self_id}:");
        card_kind.starts_with(&prefix)
    }

    pub fn can_card_delete(&self, card_kind: &str, self_id: &str) -> bool {
        self.can_card_modify(card_kind, self_id)
    }

    /// Exact glob match or a `"*"` grant; no glob ⊆ glob inclusion is computed.
    pub fn can_subscribe(&self, ev_glob: &str) -> bool {
        self.events_subscribe
            .iter()
            .any(|g| g == "*" || g == ev_glob)
    }

    /// Manifest value if positive; a `0` is treated as unset rather than bricking KV.
    pub fn kv_quota_bytes(&self) -> u64 {
        if self.kv_quota_bytes == 0 {
            DEFAULT_KV_QUOTA_BYTES
        } else {
            self.kv_quota_bytes
        }
    }
}

impl UiPermissions {
    /// May an iframe call this `tool_name`? Same glob grammar as `events_subscribe`; an empty allow-list denies.
    pub fn can_call_tool(&self, tool_name: &str) -> bool {
        self.tools.iter().any(|p| glob_matches(p, tool_name))
    }
}

impl Manifest {
    /// Allow if **any** view's `permissions.tools` would allow it: the route is per-plugin while the spec's permissions are per-view.
    pub fn can_call_tool(&self, tool_name: &str) -> bool {
        self.views
            .iter()
            .filter_map(|v| v.permissions.as_ref())
            .any(|p| p.can_call_tool(tool_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(json: &str) -> Permissions {
        serde_json::from_str(json).expect("valid perms json")
    }

    #[test]
    fn overlay_write_allows_listed_entity_kind() {
        let p = perms(r#"{ "overlays_write": ["track", "card"] }"#);
        assert!(p.can_overlay_write("track", "status"));
        assert!(p.can_overlay_write("card", "progress"));
    }

    #[test]
    fn overlay_write_denies_unlisted_entity_kind() {
        let p = perms(r#"{ "overlays_write": ["card"] }"#);
        assert!(!p.can_overlay_write("track", "status"));
    }

    #[test]
    fn overlay_write_denies_empty_allowlist() {
        let p = perms("{}");
        assert!(!p.can_overlay_write("track", "status"));
        assert!(!p.can_overlay_write("card", "status"));
    }

    #[test]
    fn card_create_allows_terminal_when_granted() {
        let p = perms(r#"{ "cards_create": true }"#);
        assert!(p.can_card_create("terminal", "dev.example"));
    }

    #[test]
    fn card_create_allows_own_prefix() {
        let p = perms(r#"{ "cards_create": true }"#);
        assert!(p.can_card_create("plugin:dev.example:notes", "dev.example"));
    }

    #[test]
    fn card_create_denies_other_plugin_prefix() {
        let p = perms(r#"{ "cards_create": true }"#);
        assert!(!p.can_card_create("plugin:other.plugin:notes", "dev.example"));
    }

    #[test]
    fn card_create_denies_bare_kind() {
        let p = perms(r#"{ "cards_create": true }"#);
        assert!(!p.can_card_create("doc", "dev.example"));
    }

    #[test]
    fn card_create_denies_without_grant() {
        let p = perms("{}");
        assert!(!p.can_card_create("plugin:dev.example:notes", "dev.example"));
        assert!(!p.can_card_create("terminal", "dev.example"));
    }

    #[test]
    fn card_modify_allows_own_prefix() {
        let p = perms("{}");
        assert!(p.can_card_modify("plugin:dev.example:notes", "dev.example"));
        assert!(p.can_card_delete("plugin:dev.example:notes", "dev.example"));
    }

    #[test]
    fn card_modify_denies_terminal_kind() {
        let p = perms("{}");
        assert!(!p.can_card_modify("terminal", "dev.example"));
        assert!(!p.can_card_delete("terminal", "dev.example"));
    }

    #[test]
    fn card_modify_denies_other_plugin_kind() {
        let p = perms("{}");
        assert!(!p.can_card_modify("plugin:other.plugin:notes", "dev.example"));
        assert!(!p.can_card_delete("plugin:other.plugin:notes", "dev.example"));
    }

    #[test]
    fn subscribe_allows_exact_match() {
        let p = perms(r#"{ "events_subscribe": ["card:*", "track:*"] }"#);
        assert!(p.can_subscribe("card:*"));
        assert!(p.can_subscribe("track:*"));
    }

    #[test]
    fn subscribe_allows_wildcard_grant() {
        let p = perms(r#"{ "events_subscribe": ["*"] }"#);
        assert!(p.can_subscribe("anything"));
        assert!(p.can_subscribe("card:added"));
    }

    #[test]
    fn subscribe_denies_unlisted_glob() {
        let p = perms(r#"{ "events_subscribe": ["card:*"] }"#);
        assert!(!p.can_subscribe("track:*"));
        assert!(!p.can_subscribe("plugin:*"));
    }

    #[test]
    fn subscribe_denies_empty_allowlist() {
        let p = perms("{}");
        assert!(!p.can_subscribe("*"));
        assert!(!p.can_subscribe("card:*"));
    }

    #[test]
    fn manifest_accepts_deprecated_proposals_field() {
        let json = r#"{
            "manifest_version": 1,
            "id": "dev.example",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "X",
            "entrypoint": { "command": "bin/x" },
            "permissions": { "proposals": ["report", "legacy-kind"] }
        }"#;
        let m = Manifest::parse(json).expect("valid manifest");
        assert_eq!(m.permissions.proposals, ["report", "legacy-kind"]);
    }

    #[test]
    fn kv_quota_default_when_unset() {
        let p = perms("{}");
        assert_eq!(p.kv_quota_bytes(), DEFAULT_KV_QUOTA_BYTES);
    }

    #[test]
    fn kv_quota_honors_manifest_value() {
        let p = perms(r#"{ "kv_quota_bytes": 4096 }"#);
        assert_eq!(p.kv_quota_bytes(), 4096);
    }

    #[test]
    fn kv_quota_treats_zero_as_default() {
        let p = perms(r#"{ "kv_quota_bytes": 0 }"#);
        assert_eq!(p.kv_quota_bytes(), DEFAULT_KV_QUOTA_BYTES);
    }

    fn ui_perms(json: &str) -> UiPermissions {
        serde_json::from_str(json).expect("valid ui-perms json")
    }

    #[test]
    fn tool_call_allows_listed_name() {
        let p = ui_perms(r#"{ "tools": ["neige.overlay.set", "neige.card.update"] }"#);
        assert!(p.can_call_tool("neige.overlay.set"));
        assert!(p.can_call_tool("neige.card.update"));
    }

    #[test]
    fn tool_call_denies_unlisted_name() {
        let p = ui_perms(r#"{ "tools": ["neige.overlay.set"] }"#);
        assert!(!p.can_call_tool("neige.card.update"));
        assert!(!p.can_call_tool("neige.overlay.delete"));
    }

    #[test]
    fn tool_call_denies_empty_allowlist() {
        let p = ui_perms(r#"{ "tools": [] }"#);
        assert!(!p.can_call_tool("neige.overlay.set"));
        let p2 = ui_perms("{}");
        assert!(!p2.can_call_tool("neige.overlay.set"));
    }

    #[test]
    fn tool_call_supports_wildcard_grant() {
        let p = ui_perms(r#"{ "tools": ["*"] }"#);
        assert!(p.can_call_tool("neige.overlay.set"));
        assert!(p.can_call_tool("anything.at.all"));
    }

    #[test]
    fn tool_call_supports_prefix_glob() {
        let p = ui_perms(r#"{ "tools": ["neige.overlay.*"] }"#);
        assert!(p.can_call_tool("neige.overlay.set"));
        assert!(p.can_call_tool("neige.overlay.delete"));
        // Prefix glob is dot-anchored: `neige.overlayx` must not slip through.
        assert!(!p.can_call_tool("neige.overlayx"));
        assert!(!p.can_call_tool("neige.card.update"));
    }

    fn manifest_with_view_tools(view_tools: Option<&[&str]>) -> Manifest {
        let perms_json = match view_tools {
            Some(list) => {
                let arr: Vec<_> = list.iter().map(|s| format!("\"{s}\"")).collect();
                format!(", \"permissions\": {{ \"tools\": [{}] }}", arr.join(","))
            }
            None => String::new(),
        };
        let json = format!(
            r#"{{
                "manifest_version": 1,
                "id": "dev.example",
                "version": "0.1.0",
                "min_kernel_version": "0.0.1",
                "display_name": "X",
                "entrypoint": {{ "command": "bin/x" }},
                "views": [
                    {{
                        "view_id": "main",
                        "title": "Main",
                        "scope": "card"{perms_json}
                    }}
                ]
            }}"#
        );
        Manifest::parse(&json).expect("valid manifest")
    }

    #[test]
    fn manifest_tool_call_allows_when_any_view_grants() {
        let m = manifest_with_view_tools(Some(&["neige.overlay.set"]));
        assert!(m.can_call_tool("neige.overlay.set"));
        assert!(!m.can_call_tool("neige.card.update"));
    }

    #[test]
    fn manifest_tool_call_denies_when_view_has_no_permissions_block() {
        let m = manifest_with_view_tools(None);
        assert!(!m.can_call_tool("neige.overlay.set"));
    }

    #[test]
    fn manifest_tool_call_denies_when_no_views_declared() {
        let json = r#"{
            "manifest_version": 1,
            "id": "dev.headless",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Headless",
            "entrypoint": { "command": "bin/x" }
        }"#;
        let m = Manifest::parse(json).expect("valid");
        assert!(!m.can_call_tool("neige.overlay.set"));
    }

    #[test]
    fn manifest_tool_call_unions_across_views() {
        let json = r#"{
            "manifest_version": 1,
            "id": "dev.example",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "X",
            "entrypoint": { "command": "bin/x" },
            "views": [
                {
                    "view_id": "a",
                    "title": "A",
                    "scope": "card",
                    "permissions": { "tools": ["neige.overlay.set"] }
                },
                {
                    "view_id": "b",
                    "title": "B",
                    "scope": "card",
                    "permissions": { "tools": ["neige.card.update"] }
                }
            ]
        }"#;
        let m = Manifest::parse(json).expect("valid");
        assert!(m.can_call_tool("neige.overlay.set"));
        assert!(m.can_call_tool("neige.card.update"));
        assert!(!m.can_call_tool("neige.overlay.delete"));
    }
}
