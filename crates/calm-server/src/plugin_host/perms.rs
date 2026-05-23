//! Per-plugin permission checks.
//!
//! Slice C consults these on every `neige.*` callback before the kernel runs
//! the side-effect (overlay write, card write, event subscribe, kv access).
//! The manifest's `permissions` blob is the policy; this module turns it into
//! per-call yes/no decisions.
//!
//! Rules summary (design doc §3 + §6):
//!
//!   * `can_overlay_write(entity_kind, overlay_kind)` — `entity_kind` must
//!     appear in `permissions.overlays_write`. `overlay_kind` is plugin-defined
//!     and not gated by the manifest; the kernel only enforces the entity-kind
//!     allow-list.
//!   * `can_card_create(kind, self_id)` — `kind` must be `"terminal"` (plugin-
//!     managed PTY) or start with `"plugin:<self_id>:"`. Plugins cannot create
//!     cards owned by other plugins. Additionally, the manifest's
//!     `permissions.cards_create` must be `true`.
//!   * `can_card_modify(card_kind, self_id)` / `can_card_delete(...)` — only
//!     cards whose `kind` starts with `"plugin:<self_id>:"`. Terminal cards
//!     and other plugins' cards are off-limits, even if the plugin created the
//!     terminal card in the first place.
//!   * `can_subscribe(ev_glob)` — `ev_glob` must match one of the entries in
//!     `permissions.events_subscribe`. `"*"` matches everything (the firehose
//!     pattern from `event.rs`'s topic grammar).
//!   * `kv_quota_bytes()` — manifest value if set; default 1 MiB.
//!   * `Manifest::can_call_tool(tool_name)` — iframe-initiated tool calls
//!     (`POST /api/plugins/:id/tool-call`) must appear in **some** view's
//!     `permissions.tools` allow-list. Empty / absent allow-lists deny by
//!     default — see `UiPermissions::can_call_tool` for the per-view logic
//!     and #198 (concern 5) for the design call.

use super::manifest::{Manifest, Permissions, UiPermissions};

/// Default per-plugin KV quota when the manifest doesn't pin one. Mirrors the
/// `permissions.kv_quota_bytes` field in the design doc's example (§1.1).
pub const DEFAULT_KV_QUOTA_BYTES: u64 = 1_048_576;

impl Permissions {
    /// May the plugin write an overlay on this entity kind?
    ///
    /// `entity_kind` is `"wave"` or `"card"` (the only two kinds in M3); the
    /// `overlay_kind` argument is the plugin-defined `kind` string and is
    /// **not** gated — design doc §6 only restricts entity kinds. We accept it
    /// here so future tightening (e.g. allow-listing specific overlay kinds)
    /// can land without a signature change.
    pub fn can_overlay_write(&self, entity_kind: &str, _overlay_kind: &str) -> bool {
        self.overlays_write.iter().any(|k| k == entity_kind)
    }

    /// May the plugin create a card with this `kind`?
    ///
    /// Two acceptable shapes:
    ///   * `"terminal"` — built-in PTY card. Plugin-managed terminals are a
    ///     real use case (a chat plugin spawning a PTY card it streams output
    ///     into), so we allow this independent of the `plugin:<self>:` prefix.
    ///   * `"plugin:<self_id>:<view>"` — the plugin's own namespaced card.
    ///
    /// Anything else (including another plugin's prefix) is denied.
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

    /// May the plugin mutate (update payload/sort) a card with this `kind`?
    ///
    /// **Strict ownership**: only cards whose `kind` starts with
    /// `"plugin:<self_id>:"`. Even terminal cards the plugin created itself
    /// are off-limits here — they're "owned" by the kernel's terminal layer,
    /// not the plugin that asked for one. This matches design §3's
    /// `neige.card.update` rule.
    pub fn can_card_modify(&self, card_kind: &str, self_id: &str) -> bool {
        let prefix = format!("plugin:{self_id}:");
        card_kind.starts_with(&prefix)
    }

    /// May the plugin delete this card? Same rule as `can_card_modify` —
    /// plugins can only delete cards they own.
    pub fn can_card_delete(&self, card_kind: &str, self_id: &str) -> bool {
        self.can_card_modify(card_kind, self_id)
    }

    /// May the plugin subscribe to events matching this glob?
    ///
    /// The match is conservative: we accept the exact glob requested if it
    /// appears in `events_subscribe`, or if the manifest lists `"*"` (firehose
    /// grant). We do NOT try to compute glob ⊆ glob inclusion — a plugin that
    /// asks for `"card:*"` must have `"card:*"` (or `"*"`) in its manifest.
    pub fn can_subscribe(&self, ev_glob: &str) -> bool {
        self.events_subscribe
            .iter()
            .any(|g| g == "*" || g == ev_glob)
    }

    /// Per-plugin KV byte budget. Manifest value if positive; otherwise the
    /// design-doc default of 1 MiB. A `0` value in the manifest is treated as
    /// "unset" — disabling KV is expressed by simply not granting any keys
    /// (Slice C's set-handler still consults this, so a hostile zero would
    /// effectively brick KV; we prefer to silently default rather than break).
    pub fn kv_quota_bytes(&self) -> u64 {
        if self.kv_quota_bytes == 0 {
            DEFAULT_KV_QUOTA_BYTES
        } else {
            self.kv_quota_bytes
        }
    }
}

impl UiPermissions {
    /// May an iframe call this `tool_name` via `app.callServerTool`?
    ///
    /// Matches `tool_name` against each entry in `self.tools` using the same
    /// glob grammar as `events_subscribe`:
    ///
    ///   * `"*"` — matches every name (firehose grant).
    ///   * `"<prefix>.*"` — matches anything whose name starts with
    ///     `"<prefix>."` (e.g. `"neige.overlay.*"` matches `"neige.overlay.set"`
    ///     and `"neige.overlay.delete"` but not `"neige.overlayx"`).
    ///   * Anything else — literal equality.
    ///
    /// **Deny by default** (per #198 concern 5): an empty allow-list returns
    /// `false`, matching the comment on the struct field. Granting tool calls
    /// must be an explicit opt-in in the manifest — silent "everything goes"
    /// is exactly the failure mode the issue calls out.
    pub fn can_call_tool(&self, tool_name: &str) -> bool {
        self.tools.iter().any(|p| tool_glob_matches(p, tool_name))
    }
}

impl Manifest {
    /// Aggregate `can_call_tool` across every view's `permissions.tools` list.
    ///
    /// The iframe transport is per-view in the spec (each `_meta.ui.permissions`
    /// block is a property of a specific resource), but the kernel-side route
    /// `POST /api/plugins/:id/tool-call` is per-plugin. We resolve the mismatch
    /// conservatively by accepting the call if **any** view of the plugin would
    /// have allowed it — that's the strongest grant the manifest could express.
    /// Tightening this to per-view granularity requires the caller to thread the
    /// view_id through (M5 transport will), so this signature is forward-compat
    /// with that work.
    ///
    /// Deny-by-default: a plugin with no views, or with no view declaring this
    /// tool, returns `false`. See `UiPermissions::can_call_tool` for per-list
    /// glob semantics.
    pub fn can_call_tool(&self, tool_name: &str) -> bool {
        self.views
            .iter()
            .filter_map(|v| v.permissions.as_ref())
            .any(|p| p.can_call_tool(tool_name))
    }
}

/// Tool-name glob matcher. Mirrors `events.rs`'s `glob_matches`: literal
/// equality, full wildcard `"*"`, or dot-anchored prefix wildcard `"<x>.*"`.
fn tool_glob_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == name {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        let with_dot = format!("{prefix}.");
        return name.starts_with(&with_dot);
    }
    false
}

// ===========================================================================
// Tests — both allow and deny paths for every gate.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(json: &str) -> Permissions {
        serde_json::from_str(json).expect("valid perms json")
    }

    // ---- can_overlay_write --------------------------------------------------

    #[test]
    fn overlay_write_allows_listed_entity_kind() {
        let p = perms(r#"{ "overlays_write": ["wave", "card"] }"#);
        assert!(p.can_overlay_write("wave", "status"));
        assert!(p.can_overlay_write("card", "progress"));
    }

    #[test]
    fn overlay_write_denies_unlisted_entity_kind() {
        let p = perms(r#"{ "overlays_write": ["card"] }"#);
        assert!(!p.can_overlay_write("wave", "status"));
    }

    #[test]
    fn overlay_write_denies_empty_allowlist() {
        let p = perms("{}");
        assert!(!p.can_overlay_write("wave", "status"));
        assert!(!p.can_overlay_write("card", "status"));
    }

    // ---- can_card_create ----------------------------------------------------

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
        // cards_create=false (default) → even own-prefix is rejected.
        let p = perms("{}");
        assert!(!p.can_card_create("plugin:dev.example:notes", "dev.example"));
        assert!(!p.can_card_create("terminal", "dev.example"));
    }

    // ---- can_card_modify / delete ------------------------------------------

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

    // ---- can_subscribe ------------------------------------------------------

    #[test]
    fn subscribe_allows_exact_match() {
        let p = perms(r#"{ "events_subscribe": ["card:*", "wave:*"] }"#);
        assert!(p.can_subscribe("card:*"));
        assert!(p.can_subscribe("wave:*"));
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
        assert!(!p.can_subscribe("wave:*"));
        assert!(!p.can_subscribe("plugin:*"));
    }

    #[test]
    fn subscribe_denies_empty_allowlist() {
        let p = perms("{}");
        assert!(!p.can_subscribe("*"));
        assert!(!p.can_subscribe("card:*"));
    }

    // ---- kv_quota_bytes -----------------------------------------------------

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

    // ---- UiPermissions::can_call_tool --------------------------------------

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
        // The deny-by-default invariant from #198 concern 5: an empty (or
        // absent) `tools` array must not silently let calls through.
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
        // And `neige.card.*` is unrelated.
        assert!(!p.can_call_tool("neige.card.update"));
    }

    // ---- Manifest::can_call_tool -------------------------------------------

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
        // View exists but doesn't declare permissions at all → deny.
        let m = manifest_with_view_tools(None);
        assert!(!m.can_call_tool("neige.overlay.set"));
    }

    #[test]
    fn manifest_tool_call_denies_when_no_views_declared() {
        // Plugin with zero views can't grant any iframe tool calls — there's
        // no iframe to call from in the first place.
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
        // Two views, each granting a different tool. Both must be reachable.
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
