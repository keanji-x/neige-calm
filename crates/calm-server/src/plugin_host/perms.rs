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

use super::manifest::Permissions;

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
}
