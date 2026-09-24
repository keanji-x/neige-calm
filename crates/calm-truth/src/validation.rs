//! Per-kind payload validators and schema versions for the card and overlay kinds the kernel owns.
//! Plugin-defined kinds stay opaque.

use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use calm_types::claude_permissions::ClaudePermissionsSource;

use crate::db::RepoRead;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::model::Overlay;

/// `schemaVersion` for `Card.payload` when `kind == "terminal"`.
pub const TERMINAL_PAYLOAD_SCHEMA_VERSION: u32 = 1;
/// `Card.payload` key stamped `true` at creation only on terminals the Planner opened with hook
/// signals; a hook for such a card is advisory telemetry, never worker state.
pub const TERMINAL_SIGNALS_PAYLOAD_KEY: &str = "terminal_signals";
/// `Card.payload` key stamped at creation only on terminals the Planner opened with a
/// `claude_permissions` scope: the effective permissions block written to the terminal's settings file.
pub const TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY: &str = "claude_permissions";
/// `Card.payload` key stamped beside [`TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY`] with a
/// [`ClaudePermissionsSource`]; absent reads as `declared`.
pub const TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY: &str = "claude_permissions_source";
/// Creation-time template instructions, retained across Planner resets and
/// report edits. Only the track-create transaction may mint this snapshot.
pub const PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY: &str = "template_context";
/// The Planner card's backend (`AgentProvider` serde names), minted at track creation and
/// backfilled `"codex"` by migration 0117. Missing or unknown makes the card not a harness card.
pub const PLANNER_PROVIDER_PAYLOAD_KEY: &str = "planner_provider";

/// Kernel-owned card fields, refused at client boundaries and preserved by
/// `card_update_tx` even when a replacement payload omits them.
pub const SERVER_OWNED_CARD_PAYLOAD_KEYS: [&str; 5] = [
    TERMINAL_SIGNALS_PAYLOAD_KEY,
    TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY,
    TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY,
    PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY,
    PLANNER_PROVIDER_PAYLOAD_KEY,
];

/// Whether a stored value of a server-owned key is the shape the kernel mints (and so is kept
/// sticky by `card_update_tx`); the map form of a unit variant was never minted.
pub fn server_owned_value_is_sticky(key: &str, value: &Value) -> bool {
    match key {
        // Even corrupt snapshots stay protected: startup must report the
        // corruption, not erase it and silently drop the working method.
        PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY => true,
        // A Planner never changes backend; a corrupt value must survive so the card stays not-a-harness.
        PLANNER_PROVIDER_PAYLOAD_KEY => true,
        TERMINAL_SIGNALS_PAYLOAD_KEY => value.as_bool() == Some(true),
        TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY => value.is_object(),
        TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY => {
            value.is_string()
                && serde_json::from_value::<ClaudePermissionsSource>(value.clone()).is_ok()
        }
        _ => false,
    }
}

/// Refuse a client-supplied `Card.payload` carrying any server-owned key, whatever the card kind:
/// the hook ingest route reads the marker from the payload, never from the patchable `kind`.
pub fn reject_client_supplied_server_owned_keys(payload: &Value) -> Result<()> {
    for key in SERVER_OWNED_CARD_PAYLOAD_KEYS {
        if payload.get(key).is_some() {
            return Err(CalmError::BadRequest(format!(
                "`{key}` is server-owned and cannot be written through the API"
            )));
        }
    }
    Ok(())
}
/// `schemaVersion` for `Card.payload` when `kind == "codex"`.
pub const CODEX_PAYLOAD_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Card.payload` when `kind == "claude"`.
pub const CLAUDE_PAYLOAD_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Card.payload` when `kind == "track-report"`; mirrors
/// `TrackReportPayload::SCHEMA_VERSION`. No SQL migration: older rows are upgraded on their next write.
pub const TRACK_REPORT_PAYLOAD_SCHEMA_VERSION: u32 = 4;
/// `schemaVersion` for `Overlay.payload` when `kind == "progress"`.
pub const OVERLAY_PROGRESS_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "eta"`.
pub const OVERLAY_ETA_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "now"`.
pub const OVERLAY_NOW_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "layout"`.
pub const OVERLAY_LAYOUT_SCHEMA_VERSION: u32 = 1;
/// The reserved `plugin_id` namespace for overlay rows the kernel authors itself; nothing outside
/// the process may write it.
pub const KERNEL_OVERLAY_PLUGIN_ID: &str = "kernel";
/// `schemaVersion` for `Overlay.payload` when `kind == "file-viewer-nav"`.
pub const OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION: u32 = 1;
/// `schemaVersion` for `Overlay.payload` when `kind == "activity"`.
pub const OVERLAY_ACTIVITY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy)]
pub struct OverlayKindEntry {
    pub kind: &'static str,
    pub validate: fn(&Value) -> Result<()>,
    pub max_schema_version: u32,
}

pub struct OverlayKindRegistry {
    entries: &'static [OverlayKindEntry],
}

impl OverlayKindRegistry {
    pub const fn new(entries: &'static [OverlayKindEntry]) -> Self {
        Self { entries }
    }

    pub fn lookup(&self, kind: &str) -> Option<&'static OverlayKindEntry> {
        self.entries.iter().find(|entry| entry.kind == kind)
    }

    pub fn validate(&self, kind: &str, payload: &Value) -> Result<()> {
        let Some(entry) = self.lookup(kind) else {
            return Ok(());
        };
        (entry.validate)(payload)
    }

    pub fn max_supported_schema_version(&self, kind: &str) -> Option<u32> {
        self.lookup(kind).map(|entry| entry.max_schema_version)
    }
}

fn validate_as<T>(kind: &str, max_version: u32, payload: &Value) -> Result<()>
where
    T: DeserializeOwned,
{
    check_schema_version(kind, payload, max_version)?;
    serde_json::from_value::<T>(payload.clone())
        .map(|_| ())
        .map_err(|e| CalmError::BadRequest(format!("invalid {kind} payload: {e}")))
}

macro_rules! simple_overlay {
    ($fn_name:ident, $shape_name:ident, $kind:literal, $version:expr, {
        $field:ident: $field_ty:ty
    }) => {
        fn $fn_name(payload: &Value) -> Result<()> {
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct $shape_name {
                $field: $field_ty,
            }
            validate_as::<$shape_name>($kind, $version, payload)
        }
    };
}

simple_overlay!(
    validate_progress_overlay_payload,
    ProgressPayload,
    "progress",
    OVERLAY_PROGRESS_SCHEMA_VERSION,
    { value: f64 }
);
simple_overlay!(
    validate_eta_overlay_payload,
    EtaPayload,
    "eta",
    OVERLAY_ETA_SCHEMA_VERSION,
    { text: String }
);
simple_overlay!(
    validate_now_overlay_payload,
    NowPayload,
    "now",
    OVERLAY_NOW_SCHEMA_VERSION,
    { text: String }
);

fn validate_layout_overlay_payload(payload: &Value) -> Result<()> {
    check_schema_version("layout", payload, OVERLAY_LAYOUT_SCHEMA_VERSION)?;
    validate_layout_payload(payload)
}

fn validate_file_viewer_nav_overlay_payload(payload: &Value) -> Result<()> {
    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    enum FileViewerTab {
        Code,
        Diff,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct FileViewerNavPayload {
        #[serde(default)]
        schema_version: Option<u32>,
        tab: FileViewerTab,
        folder_path: String,
        selected_path: Option<String>,
        diff_selected: Option<String>,
    }

    validate_as::<FileViewerNavPayload>(
        "file-viewer-nav",
        OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION,
        payload,
    )
}

/// The `kernel/track/activity` payload; mirrors `calm_server::track_activity::ActivityPayload`
/// field for field and is closed against unknown fields.
fn validate_activity_overlay_payload(payload: &Value) -> Result<()> {
    /// A nullable field that must still be PRESENT: serde treats a missing `Option` as `None`.
    fn present<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
    where
        D: serde::Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Option::<T>::deserialize(d)
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    enum Attention {
        None,
        Input,
        Failed,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    enum ItemKind {
        Input,
        Failed,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    enum ItemSource {
        Card,
        Task,
        Session,
        Lifecycle,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct Item {
        kind: ItemKind,
        source: ItemSource,
        id: String,
        #[serde(deserialize_with = "present")]
        card_id: Option<String>,
        at_ms: i64,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "lowercase")]
    enum CardState {
        Working,
        Input,
        Failed,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct CardEntry {
        card_id: String,
        state: CardState,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct ActivityPayload {
        #[serde(default)]
        #[serde(rename = "schemaVersion")]
        schema_version: Option<u32>,
        working: bool,
        attention: Attention,
        #[serde(deserialize_with = "present")]
        activity_at_ms: Option<i64>,
        items: Vec<Item>,
        cards: Vec<CardEntry>,
    }

    validate_as::<ActivityPayload>("activity", OVERLAY_ACTIVITY_SCHEMA_VERSION, payload)
}

/// The kernel-owned overlay kinds. The two FSM-era kinds (the per-card state row, the track-scoped
/// needs-input boolean) are deliberately absent: rows an older kernel left behind pass the read-side
/// version filter untouched, like any plugin kind.
pub static OVERLAY_KIND_REGISTRY: OverlayKindRegistry = OverlayKindRegistry::new(&[
    OverlayKindEntry {
        kind: "progress",
        validate: validate_progress_overlay_payload,
        max_schema_version: OVERLAY_PROGRESS_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "eta",
        validate: validate_eta_overlay_payload,
        max_schema_version: OVERLAY_ETA_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "now",
        validate: validate_now_overlay_payload,
        max_schema_version: OVERLAY_NOW_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "layout",
        validate: validate_layout_overlay_payload,
        max_schema_version: OVERLAY_LAYOUT_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "file-viewer-nav",
        validate: validate_file_viewer_nav_overlay_payload,
        max_schema_version: OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION,
    },
    OverlayKindEntry {
        kind: "activity",
        validate: validate_activity_overlay_payload,
        max_schema_version: OVERLAY_ACTIVITY_SCHEMA_VERSION,
    },
]);

pub type OverlayScopeFuture<'a> = Pin<Box<dyn Future<Output = Result<EventScope>> + Send + 'a>>;
pub type OverlayRouteScopeFn = for<'a> fn(&'a dyn RepoRead, &'a str) -> OverlayScopeFuture<'a>;

pub struct OverlayEntityScopeEntry {
    pub kind: &'static str,
    pub route_scope_fn: OverlayRouteScopeFn,
    /// May a writer outside the kernel process attach overlays to this entity kind? `false` marks a
    /// kernel-reserved namespace whose rows the kernel reads back as fact.
    pub externally_writable: bool,
}

pub struct OverlayEntityScopeRegistry {
    entries: &'static [OverlayEntityScopeEntry],
}

impl OverlayEntityScopeRegistry {
    pub const fn new(entries: &'static [OverlayEntityScopeEntry]) -> Self {
        Self { entries }
    }

    pub fn lookup(&self, kind: &str) -> Option<&'static OverlayEntityScopeEntry> {
        self.entries.iter().find(|entry| entry.kind == kind)
    }

    pub async fn route_scope(
        &self,
        repo: &dyn RepoRead,
        kind: &str,
        id: &str,
    ) -> Result<EventScope> {
        match self.lookup(kind) {
            Some(entry) => (entry.route_scope_fn)(repo, id).await,
            None => Ok(EventScope::System),
        }
    }

    pub fn externally_writable(&self, kind: &str) -> bool {
        self.lookup(kind)
            .map(|entry| entry.externally_writable)
            .unwrap_or(false)
    }

    pub fn externally_writable_kinds(&self) -> Vec<&'static str> {
        self.entries
            .iter()
            .filter(|entry| entry.externally_writable)
            .map(|entry| entry.kind)
            .collect()
    }
}

fn card_overlay_scope<'a>(repo: &'a dyn RepoRead, id: &'a str) -> OverlayScopeFuture<'a> {
    Box::pin(async move {
        let card = match repo.card_get(id).await? {
            Some(card) => card,
            None => return Ok(EventScope::System),
        };
        let track = match repo.track_get(card.track_id.as_str()).await? {
            Some(track) => track,
            None => return Ok(EventScope::System),
        };
        Ok(EventScope::Card {
            card: card.id,
            track: track.id,
            area: track.area_id,
        })
    })
}

fn track_overlay_scope<'a>(repo: &'a dyn RepoRead, id: &'a str) -> OverlayScopeFuture<'a> {
    Box::pin(async move {
        let track = match repo.track_get(id).await? {
            Some(track) => track,
            None => return Ok(EventScope::System),
        };
        Ok(EventScope::Track {
            track: track.id,
            area: track.area_id,
        })
    })
}

fn system_overlay_scope<'a>(_repo: &'a dyn RepoRead, _id: &'a str) -> OverlayScopeFuture<'a> {
    Box::pin(async { Ok(EventScope::System) })
}

pub static OVERLAY_ENTITY_SCOPE_REGISTRY: OverlayEntityScopeRegistry =
    OverlayEntityScopeRegistry::new(&[
        OverlayEntityScopeEntry {
            kind: "card",
            route_scope_fn: card_overlay_scope,
            externally_writable: true,
        },
        OverlayEntityScopeEntry {
            kind: "track",
            route_scope_fn: track_overlay_scope,
            externally_writable: true,
        },
        OverlayEntityScopeEntry {
            kind: "view",
            route_scope_fn: system_overlay_scope,
            externally_writable: false,
        },
        OverlayEntityScopeEntry {
            kind: "system",
            route_scope_fn: system_overlay_scope,
            externally_writable: false,
        },
    ]);

/// Maximum `schemaVersion` this kernel interprets for an overlay `kind`; `None` for plugin-defined
/// kinds. Backs the read-side guard against rows a newer binary wrote into the same DB.
pub fn max_supported_overlay_schema_version(kind: &str) -> Option<u32> {
    OVERLAY_KIND_REGISTRY.max_supported_schema_version(kind)
}

/// Read the `schemaVersion` field from a payload, defaulting to `1` when absent or unparsable.
pub fn payload_schema_version(payload: &Value) -> u32 {
    payload
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(1)
}

/// Read-side guard: `true` if the overlay row carries a `schemaVersion` above this binary's max for
/// its kind and must be dropped before reaching a client.
pub fn should_skip_overlay(overlay: &Overlay) -> bool {
    let Some(max) = max_supported_overlay_schema_version(&overlay.kind) else {
        return false;
    };
    let version = payload_schema_version(&overlay.payload);
    if version > max {
        tracing::warn!(
            overlay_id = %overlay.id,
            kind = %overlay.kind,
            schema_version = version,
            max_supported = max,
            entity_kind = %overlay.entity_kind,
            entity_id = %overlay.entity_id,
            "dropping overlay with unsupported schemaVersion on read \
             (kernel-owned kind, future version); upgrade this binary \
             or rewrite the row to the supported version",
        );
        true
    } else {
        false
    }
}

/// [`should_skip_overlay`] for the WS surface: only `Event::OverlaySet` ships a full overlay payload.
pub fn should_skip_event_for_overlay_version(event: &Event) -> bool {
    match event {
        Event::OverlaySet(overlay) => should_skip_overlay(overlay),
        _ => false,
    }
}

/// Enforce the `schemaVersion` rule for a kernel-owned kind: absent or `== expected` accepts,
/// anything else is `BadRequest`.
fn check_schema_version(kind: &str, payload: &Value, expected: u32) -> Result<()> {
    // Non-object payloads can't carry `schemaVersion`; the kind-specific validator decides on them.
    if !payload.is_object() {
        return Ok(());
    }
    let Some(raw) = payload.get("schemaVersion") else {
        return Ok(());
    };
    let Some(version) = raw.as_u64() else {
        return Err(CalmError::BadRequest(format!(
            "invalid schemaVersion for kind `{kind}`: expected u32, got {raw}"
        )));
    };
    if version as u32 == expected {
        Ok(())
    } else {
        Err(CalmError::BadRequest(format!(
            "unsupported schemaVersion {version} for kind `{kind}`; this kernel supports {expected}"
        )))
    }
}

/// Validate an `Overlay.payload` for a given `kind`; unknown / plugin-specific kinds are `Ok(())`.
pub fn validate_overlay_payload(kind: &str, payload: &Value) -> Result<()> {
    OVERLAY_KIND_REGISTRY.validate(kind, payload)
}

/// Grid column count — must move in lock-step with `web/src/TrackGrid.tsx::COLS`.
const LAYOUT_GRID_COLS: u32 = 12;

/// Validate a `layout` overlay payload: strict shape, `w, h >= 1`, `x + w <= LAYOUT_GRID_COLS`,
/// non-empty card-id keys.
fn validate_layout_payload(payload: &Value) -> Result<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LayoutPayload {
        positions: std::collections::BTreeMap<String, LayoutPos>,
        #[serde(default, rename = "schemaVersion")]
        schema_version: Option<u32>,
    }

    // `y` has no upper bound: RGL has no max-rows concept, cards just keep stacking down.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct LayoutPos {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    }

    let parsed: LayoutPayload = serde_json::from_value(payload.clone())
        .map_err(|e| CalmError::BadRequest(format!("invalid layout payload: {e}")))?;

    for (card_id, pos) in &parsed.positions {
        if card_id.is_empty() {
            return Err(CalmError::BadRequest(
                "invalid layout payload: positions key must be a non-empty card id",
            ));
        }
        if pos.w < 1 {
            return Err(CalmError::BadRequest(format!(
                "invalid layout payload: positions.{card_id}.w must be >= 1, got {}",
                pos.w
            )));
        }
        if pos.h < 1 {
            return Err(CalmError::BadRequest(format!(
                "invalid layout payload: positions.{card_id}.h must be >= 1, got {}",
                pos.h
            )));
        }
        // `checked_add` so an overflowed sum can't wrap under `LAYOUT_GRID_COLS`.
        match pos.x.checked_add(pos.w) {
            Some(sum) if sum <= LAYOUT_GRID_COLS => {}
            _ => {
                return Err(CalmError::BadRequest(format!(
                    "invalid layout payload: positions.{card_id}.x + w must be <= {} (grid columns), got x={} w={}",
                    LAYOUT_GRID_COLS, pos.x, pos.w
                )));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_kind::CardKindRegistry;
    use crate::db::sqlite::SqlxRepo;
    use serde_json::json;

    fn validate_builtin_card(kind: &str, payload: &Value) -> Result<()> {
        CardKindRegistry::builtins()
            .validate_payload(kind, payload)
            .map_err(CalmError::from)
    }

    fn bad_request_message(err: &CalmError) -> Option<&str> {
        match err {
            CalmError::Core(calm_types::error::CoreError::BadRequest(message)) => Some(message),
            _ => None,
        }
    }

    fn is_bad_request(err: &CalmError) -> bool {
        bad_request_message(err).is_some()
    }

    #[test]
    fn overlay_entity_scope_registry_4_entries() {
        let kinds: Vec<_> = OVERLAY_ENTITY_SCOPE_REGISTRY
            .entries
            .iter()
            .map(|entry| entry.kind)
            .collect();
        assert_eq!(kinds, vec!["card", "track", "view", "system"]);
        assert!(OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable("card"));
        assert!(OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable("track"));
        assert!(!OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable("view"));
        assert!(!OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable("system"));
        assert_eq!(
            OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable_kinds(),
            vec!["card", "track"]
        );
    }

    #[tokio::test]
    async fn overlay_entity_scope_registry_reserved_kinds_scope_to_system() {
        let repo = SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo");
        for kind in ["view", "system"] {
            let scope = OVERLAY_ENTITY_SCOPE_REGISTRY
                .route_scope(&repo, kind, "any-id")
                .await
                .unwrap();
            assert_eq!(scope, EventScope::System, "kind={kind}");
            assert!(
                !OVERLAY_ENTITY_SCOPE_REGISTRY.externally_writable(kind),
                "kind={kind} must stay kernel-reserved"
            );
        }
    }

    #[tokio::test]
    async fn overlay_entity_scope_registry_unknown_kind_falls_back_to_system() {
        let repo = SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo");
        let scope = OVERLAY_ENTITY_SCOPE_REGISTRY
            .route_scope(&repo, "weird", "x")
            .await
            .unwrap();
        assert_eq!(scope, EventScope::System);
    }

    #[test]
    fn server_owned_source_is_sticky_only_as_a_string() {
        let sticky = |value: Value| {
            server_owned_value_is_sticky(TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY, &value)
        };
        assert!(sticky(json!("declared")));
        assert!(sticky(json!("declared_within_policy")));
        assert!(
            !sticky(json!({"declared": null})),
            "the map form of a unit variant is not minted"
        );
        assert!(!sticky(json!("policy")));
        assert!(!sticky(Value::Null));
    }

    #[test]
    fn terminal_happy_with_id() {
        validate_builtin_card("terminal", &json!({ "terminal_id": "t1" })).unwrap();
    }

    #[test]
    fn terminal_happy_without_id() {
        validate_builtin_card("terminal", &json!({})).unwrap();
    }

    #[test]
    fn terminal_happy_null() {
        validate_builtin_card("terminal", &Value::Null).unwrap();
    }

    #[test]
    fn terminal_extra_fields_tolerated() {
        validate_builtin_card("terminal", &json!({ "terminal_id": "t1", "extra": "ok" })).unwrap();
    }

    #[test]
    fn terminal_rejects_wrong_type() {
        let err = validate_builtin_card("terminal", &json!({ "terminal_id": 42 })).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn terminal_rejects_array_root() {
        let err = validate_builtin_card("terminal", &json!([1, 2, 3])).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn ui_prefixed_card_accepts_anything() {
        validate_builtin_card("ui://example/view", &json!({ "junk": "ok" })).unwrap();
        validate_builtin_card("ui://example/view", &json!([1, 2, 3])).unwrap();
        validate_builtin_card("ui://example/view", &Value::Null).unwrap();
    }

    #[test]
    fn plugin_prefixed_card_accepts_anything() {
        validate_builtin_card("plugin:foo:bar", &json!({ "whatever": true })).unwrap();
    }

    #[test]
    fn overlay_kind_registry_lookup_known_kinds() {
        let expected = [
            ("progress", OVERLAY_PROGRESS_SCHEMA_VERSION),
            ("eta", OVERLAY_ETA_SCHEMA_VERSION),
            ("now", OVERLAY_NOW_SCHEMA_VERSION),
            ("layout", OVERLAY_LAYOUT_SCHEMA_VERSION),
            ("file-viewer-nav", OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION),
            ("activity", OVERLAY_ACTIVITY_SCHEMA_VERSION),
        ];

        for (kind, max_schema_version) in expected {
            let entry = OVERLAY_KIND_REGISTRY
                .lookup(kind)
                .unwrap_or_else(|| panic!("missing registry entry for {kind}"));
            assert_eq!(entry.kind, kind);
            assert_eq!(entry.max_schema_version, max_schema_version);
            assert_eq!(
                OVERLAY_KIND_REGISTRY.max_supported_schema_version(kind),
                Some(max_schema_version)
            );
        }
    }

    #[test]
    fn overlay_kind_registry_lookup_unknown_kind() {
        assert!(OVERLAY_KIND_REGISTRY.lookup("plugin:foo").is_none());
        assert!(OVERLAY_KIND_REGISTRY.lookup("").is_none());
    }

    /// Pins the registry only (no lookup, no version ceiling, opaque at the external write gates),
    /// not the absence of readers.
    #[test]
    fn status_and_any_card_needs_input_are_not_registered() {
        for kind in ["status", "any_card_needs_input"] {
            assert!(
                OVERLAY_KIND_REGISTRY.lookup(kind).is_none(),
                "{kind} is retired"
            );
            assert_eq!(
                max_supported_overlay_schema_version(kind),
                None,
                "{kind} has no version ceiling"
            );
            OVERLAY_KIND_REGISTRY
                .validate(kind, &json!({ "schemaVersion": 999, "anything": true }))
                .unwrap();
        }
    }

    #[test]
    fn overlay_kind_registry_validate_plugin_kind_opaque() {
        OVERLAY_KIND_REGISTRY
            .validate("plugin:foo", &json!({ "junk": true }))
            .unwrap();
        OVERLAY_KIND_REGISTRY
            .validate("plugin:foo", &json!({ "schemaVersion": 999 }))
            .unwrap();
    }

    #[test]
    fn overlay_kind_registry_validate_known_kinds_accept_and_reject() {
        let cases = [
            (
                "progress",
                json!({ "value": 0.5 }),
                json!({ "value": "fast" }),
            ),
            ("eta", json!({ "text": "5m" }), json!({ "text": null })),
            ("now", json!({ "text": "writing" }), json!({ "text": 7 })),
            (
                "layout",
                json!({ "positions": { "c": { "x": 0, "y": 0, "w": 4, "h": 3 } } }),
                json!({ "positions": { "c": { "x": 10, "y": 0, "w": 4, "h": 3 } } }),
            ),
            (
                "file-viewer-nav",
                json!({
                    "tab": "code",
                    "folderPath": "/repo/src",
                    "selectedPath": null,
                    "diffSelected": null
                }),
                json!({
                    "tab": "history",
                    "folderPath": "/repo/src",
                    "selectedPath": null,
                    "diffSelected": null
                }),
            ),
            (
                "activity",
                activity_payload_fixture(),
                json!({
                    "working": true,
                    "attention": "urgent",
                    "activity_at_ms": null,
                    "items": [],
                    "cards": []
                }),
            ),
        ];

        for (kind, valid, invalid) in cases {
            OVERLAY_KIND_REGISTRY.validate(kind, &valid).unwrap();
            let err = OVERLAY_KIND_REGISTRY
                .validate(kind, &invalid)
                .expect_err("invalid overlay payload must fail");
            assert!(is_bad_request(&err));
        }
    }

    #[test]
    fn overlay_kind_registry_rejects_unknown_schema_version_per_kind() {
        let cases = [
            ("progress", json!({ "schemaVersion": 99, "value": 0.5 })),
            ("eta", json!({ "schemaVersion": 99, "text": "5m" })),
            ("now", json!({ "schemaVersion": 99, "text": "writing" })),
            (
                "layout",
                json!({ "schemaVersion": 99, "positions": { "c": { "x": 0, "y": 0, "w": 1, "h": 1 } } }),
            ),
            (
                "file-viewer-nav",
                json!({
                    "schemaVersion": 99,
                    "tab": "code",
                    "folderPath": "/",
                    "selectedPath": null,
                    "diffSelected": null
                }),
            ),
            ("activity", {
                let mut payload = activity_payload_fixture();
                payload["schemaVersion"] = json!(99);
                payload
            }),
        ];

        for (kind, payload) in cases {
            let err = OVERLAY_KIND_REGISTRY.validate(kind, &payload).unwrap_err();
            assert!(is_bad_request(&err), "kind={kind}");
        }
    }

    /// A complete, valid `kernel/track/activity` payload.
    fn activity_payload_fixture() -> Value {
        json!({
            "schemaVersion": OVERLAY_ACTIVITY_SCHEMA_VERSION,
            "working": true,
            "attention": "input",
            "activity_at_ms": 1789460968837_i64,
            "items": [
                { "kind": "input", "source": "card", "id": "card-1",
                  "card_id": "card-1", "at_ms": 1789460968837_i64 },
                { "kind": "failed", "source": "task", "id": "build",
                  "card_id": null, "at_ms": 1789460968000_i64 },
                { "kind": "failed", "source": "lifecycle", "id": "track-1",
                  "card_id": null, "at_ms": 1789460960000_i64 }
            ],
            "cards": [
                { "card_id": "card-1", "state": "input" },
                { "card_id": "card-2", "state": "working" }
            ]
        })
    }

    #[test]
    fn activity_overlay_payload_is_closed_to_the_design_shape() {
        OVERLAY_KIND_REGISTRY
            .validate("activity", &activity_payload_fixture())
            .unwrap();
        OVERLAY_KIND_REGISTRY
            .validate(
                "activity",
                &json!({
                    "working": false, "attention": "none",
                    "activity_at_ms": null, "items": [], "cards": []
                }),
            )
            .unwrap();

        let rejected = [
            // missing required (nullable) field
            json!({ "working": false, "attention": "none", "items": [], "cards": [] }),
            // an item without its (nullable) card_id
            {
                let mut p = activity_payload_fixture();
                p["items"][0].as_object_mut().unwrap().remove("card_id");
                p
            },
            // unknown top-level field
            {
                let mut p = activity_payload_fixture();
                p["unread"] = json!(true);
                p
            },
            // unknown item field
            {
                let mut p = activity_payload_fixture();
                p["items"][0]["severity"] = json!(3);
                p
            },
            // unknown card field
            {
                let mut p = activity_payload_fixture();
                p["cards"][0]["reason"] = json!("x");
                p
            },
            // enum values outside the design vocabulary
            {
                let mut p = activity_payload_fixture();
                p["attention"] = json!("working");
                p
            },
            {
                let mut p = activity_payload_fixture();
                p["items"][0]["kind"] = json!("working");
                p
            },
            {
                let mut p = activity_payload_fixture();
                p["items"][0]["source"] = json!("overlay");
                p
            },
            {
                let mut p = activity_payload_fixture();
                p["cards"][0]["state"] = json!("unread");
                p
            },
            // wrong types
            {
                let mut p = activity_payload_fixture();
                p["working"] = json!("yes");
                p
            },
            {
                let mut p = activity_payload_fixture();
                p["items"][0]["at_ms"] = json!("now");
                p
            },
        ];
        for (i, payload) in rejected.iter().enumerate() {
            let Err(err) = OVERLAY_KIND_REGISTRY.validate("activity", payload) else {
                panic!("case {i} must fail: {payload}");
            };
            assert!(is_bad_request(&err), "case {i}: {err}");
        }
    }

    #[test]
    fn progress_happy() {
        validate_overlay_payload("progress", &json!({ "value": 0.42 })).unwrap();
    }

    #[test]
    fn progress_happy_integer() {
        validate_overlay_payload("progress", &json!({ "value": 1 })).unwrap();
    }

    #[test]
    fn progress_rejects_missing_value() {
        let err = validate_overlay_payload("progress", &json!({})).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn progress_rejects_string_value() {
        let err = validate_overlay_payload("progress", &json!({ "value": "fast" })).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn eta_happy() {
        validate_overlay_payload("eta", &json!({ "text": "5m" })).unwrap();
    }

    #[test]
    fn eta_rejects_missing_text() {
        let err = validate_overlay_payload("eta", &json!({})).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn eta_rejects_wrong_type() {
        let err = validate_overlay_payload("eta", &json!({ "text": 5 })).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn now_happy() {
        validate_overlay_payload("now", &json!({ "text": "writing tests" })).unwrap();
    }

    #[test]
    fn now_rejects_missing_text() {
        let err = validate_overlay_payload("now", &json!({})).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn now_rejects_wrong_type() {
        let err = validate_overlay_payload("now", &json!({ "text": null })).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn file_viewer_nav_happy_code_with_nulls() {
        validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap();
    }

    #[test]
    fn file_viewer_nav_happy_diff_with_paths() {
        validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "diff",
                "folderPath": "/repo/src",
                "selectedPath": "/repo/src/main.ts",
                "diffSelected": "src/main.ts"
            }),
        )
        .unwrap();
    }

    #[test]
    fn file_viewer_nav_rejects_missing_folder_path() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn file_viewer_nav_rejects_unknown_tab() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "history",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn file_viewer_nav_rejects_wrong_selected_path_type() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": 42,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn file_viewer_nav_rejects_unknown_field() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 1,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null,
                "extra": true
            }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn unknown_overlay_kind_accepts_anything() {
        validate_overlay_payload("custom-plugin-kind", &json!({ "anything": true })).unwrap();
        validate_overlay_payload("custom-plugin-kind", &json!([])).unwrap();
        validate_overlay_payload("custom-plugin-kind", &Value::Null).unwrap();
    }

    #[test]
    fn layout_happy_empty_positions() {
        validate_overlay_payload("layout", &json!({ "positions": {} })).unwrap();
    }

    #[test]
    fn layout_happy_one_card() {
        validate_overlay_payload(
            "layout",
            &json!({ "positions": { "card-1": { "x": 0, "y": 0, "w": 4, "h": 3 } } }),
        )
        .unwrap();
    }

    #[test]
    fn layout_happy_card_at_right_edge() {
        validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 8, "y": 0, "w": 4, "h": 2 } } }),
        )
        .unwrap();
    }

    #[test]
    fn layout_rejects_missing_positions() {
        let err = validate_overlay_payload("layout", &json!({})).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn layout_rejects_positions_not_object() {
        let err = validate_overlay_payload("layout", &json!({ "positions": [] })).unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn layout_rejects_x_plus_w_over_cols() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 10, "y": 0, "w": 4, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("grid columns")));
    }

    #[test]
    fn layout_rejects_w_zero() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 0, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("w must be >= 1")));
    }

    #[test]
    fn layout_rejects_h_zero() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 2, "h": 0 } } }),
        )
        .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("h must be >= 1")));
    }

    #[test]
    fn layout_rejects_negative_x() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": -1, "y": 0, "w": 2, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn layout_rejects_negative_y() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": -1, "w": 2, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn layout_rejects_missing_position_field() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 2 } } }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn layout_rejects_unknown_root_field() {
        let err = validate_overlay_payload("layout", &json!({ "positions": {}, "extra": 1 }))
            .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn layout_rejects_unknown_position_field() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "c": { "x": 0, "y": 0, "w": 2, "h": 2, "z": 9 } } }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn layout_rejects_empty_card_id_key() {
        let err = validate_overlay_payload(
            "layout",
            &json!({ "positions": { "": { "x": 0, "y": 0, "w": 2, "h": 2 } } }),
        )
        .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("non-empty card id")));
    }

    #[test]
    fn payload_schema_version_defaults_to_one_when_absent() {
        assert_eq!(payload_schema_version(&json!({})), 1);
        assert_eq!(payload_schema_version(&json!({ "other": "field" })), 1);
        assert_eq!(payload_schema_version(&Value::Null), 1);
    }

    #[test]
    fn payload_schema_version_returns_value_when_present() {
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": 1 })), 1);
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": 7 })), 7);
    }

    #[test]
    fn payload_schema_version_defaults_when_wrong_type() {
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": "1" })), 1);
        assert_eq!(payload_schema_version(&json!({ "schemaVersion": null })), 1);
    }

    #[test]
    fn terminal_accepts_missing_schema_version() {
        validate_builtin_card("terminal", &json!({ "terminal_id": "t1" })).unwrap();
    }

    #[test]
    fn terminal_accepts_matching_schema_version() {
        validate_builtin_card(
            "terminal",
            &json!({ "schemaVersion": 1, "terminal_id": "t1" }),
        )
        .unwrap();
    }

    #[test]
    fn terminal_rejects_unknown_schema_version() {
        let err = validate_builtin_card(
            "terminal",
            &json!({ "schemaVersion": 2, "terminal_id": "t1" }),
        )
        .unwrap_err();
        let Some(msg) = bad_request_message(&err) else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("schemaVersion"), "msg = {msg}");
        assert!(msg.contains("terminal"), "msg = {msg}");
        assert!(msg.contains('2'), "msg = {msg}");
    }

    #[test]
    fn codex_accepts_missing_schema_version() {
        validate_builtin_card("codex", &json!({ "any": "thing" })).unwrap();
    }

    #[test]
    fn codex_accepts_matching_schema_version() {
        validate_builtin_card("codex", &json!({ "schemaVersion": 1, "any": "thing" })).unwrap();
    }

    #[test]
    fn codex_rejects_unknown_schema_version() {
        let err = validate_builtin_card("codex", &json!({ "schemaVersion": 99, "any": "thing" }))
            .unwrap_err();
        let Some(msg) = bad_request_message(&err) else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("codex"), "msg = {msg}");
    }

    #[test]
    fn track_report_happy() {
        validate_builtin_card(
            "track-report",
            &json!({ "schemaVersion": 4, "docRev": 1, "summary": "", "body": "# Goal\n" }),
        )
        .unwrap();
    }

    #[test]
    fn track_report_accepts_missing_schema_version() {
        validate_builtin_card(
            "track-report",
            &json!({ "summary": "hi", "body": "# Done\n" }),
        )
        .unwrap();
    }

    #[test]
    fn track_report_v4_rejects_missing_doc_rev() {
        let err = validate_builtin_card(
            "track-report",
            &json!({ "schemaVersion": 4, "summary": "", "body": "# Goal\n" }),
        )
        .unwrap_err();
        let Some(msg) = bad_request_message(&err) else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("docRev"), "msg = {msg}");
    }

    #[test]
    fn track_report_rejects_missing_summary() {
        let err = validate_builtin_card(
            "track-report",
            &json!({ "schemaVersion": 4, "body": "# Goal" }),
        )
        .unwrap_err();
        let Some(msg) = bad_request_message(&err) else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("summary"), "msg = {msg}");
    }

    #[test]
    fn track_report_rejects_missing_body() {
        let err = validate_builtin_card(
            "track-report",
            &json!({ "schemaVersion": 4, "summary": "x" }),
        )
        .unwrap_err();
        let Some(msg) = bad_request_message(&err) else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("body"), "msg = {msg}");
    }

    #[test]
    fn track_report_rejects_wrong_field_type() {
        let err = validate_builtin_card(
            "track-report",
            &json!({ "schemaVersion": 4, "summary": 42, "body": "x" }),
        )
        .unwrap_err();
        assert!(is_bad_request(&err));
    }

    #[test]
    fn track_report_rejects_unknown_schema_version() {
        let err = validate_builtin_card(
            "track-report",
            &json!({ "schemaVersion": 5, "summary": "", "body": "" }),
        )
        .unwrap_err();
        let Some(msg) = bad_request_message(&err) else {
            panic!("expected BadRequest");
        };
        assert!(msg.contains("track-report"), "msg = {msg}");
        assert!(msg.contains('5'), "msg = {msg}");
    }

    #[test]
    fn track_report_rejects_declared_legacy_schema_versions() {
        for version in [1, 2, 3] {
            let err = validate_builtin_card(
                "track-report",
                &json!({ "schemaVersion": version, "summary": "", "body": "" }),
            )
            .unwrap_err();
            let Some(msg) = bad_request_message(&err) else {
                panic!("expected BadRequest");
            };
            assert!(msg.contains(&version.to_string()), "msg = {msg}");
            assert!(msg.contains('4'), "msg = {msg}");
        }
    }

    #[test]
    fn track_report_tolerates_unknown_fields() {
        validate_builtin_card(
            "track-report",
            &json!({
                "schemaVersion": 4,
                "docRev": 0,
                "summary": "",
                "body": "x",
                "futureField": "tolerated"
            }),
        )
        .unwrap();
    }

    #[test]
    fn progress_accepts_matching_schema_version() {
        validate_overlay_payload("progress", &json!({ "schemaVersion": 1, "value": 0.5 })).unwrap();
    }

    #[test]
    fn progress_rejects_unknown_schema_version() {
        let err =
            validate_overlay_payload("progress", &json!({ "schemaVersion": 2, "value": 0.5 }))
                .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("schemaVersion")));
    }

    #[test]
    fn eta_accepts_matching_schema_version() {
        validate_overlay_payload("eta", &json!({ "schemaVersion": 1, "text": "5m" })).unwrap();
    }

    #[test]
    fn now_accepts_matching_schema_version() {
        validate_overlay_payload("now", &json!({ "schemaVersion": 1, "text": "writing" })).unwrap();
    }

    #[test]
    fn layout_accepts_matching_schema_version() {
        validate_overlay_payload(
            "layout",
            &json!({
                "schemaVersion": 1,
                "positions": { "c": { "x": 0, "y": 0, "w": 4, "h": 3 } }
            }),
        )
        .unwrap();
    }

    #[test]
    fn layout_rejects_unknown_schema_version() {
        let err =
            validate_overlay_payload("layout", &json!({ "schemaVersion": 9, "positions": {} }))
                .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("schemaVersion")));
    }

    #[test]
    fn file_viewer_nav_rejects_unknown_schema_version() {
        let err = validate_overlay_payload(
            "file-viewer-nav",
            &json!({
                "schemaVersion": 9,
                "tab": "code",
                "folderPath": "/repo/src",
                "selectedPath": null,
                "diffSelected": null
            }),
        )
        .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("schemaVersion")));
    }

    #[test]
    fn plugin_overlay_passthrough_with_arbitrary_schema_version() {
        validate_overlay_payload(
            "custom-plugin-kind",
            &json!({ "schemaVersion": 999, "anything": true }),
        )
        .unwrap();
        validate_overlay_payload(
            "ui://example/view",
            &json!({ "schemaVersion": "totally a string", "x": 1 }),
        )
        .unwrap();
    }

    #[test]
    fn rejects_non_integer_schema_version_on_kernel_kinds() {
        let err = validate_overlay_payload("eta", &json!({ "schemaVersion": "1", "text": "5m" }))
            .unwrap_err();
        assert!(bad_request_message(&err).is_some_and(|m| m.contains("schemaVersion")));
    }

    #[test]
    fn max_supported_overlay_schema_version_kernel_kinds() {
        assert_eq!(
            max_supported_overlay_schema_version("progress"),
            Some(OVERLAY_PROGRESS_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("eta"),
            Some(OVERLAY_ETA_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("now"),
            Some(OVERLAY_NOW_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("layout"),
            Some(OVERLAY_LAYOUT_SCHEMA_VERSION)
        );
        assert_eq!(
            max_supported_overlay_schema_version("file-viewer-nav"),
            Some(OVERLAY_FILE_VIEWER_NAV_SCHEMA_VERSION)
        );
    }

    #[test]
    fn max_supported_overlay_schema_version_plugin_kinds_return_none() {
        assert_eq!(max_supported_overlay_schema_version("custom-badge"), None);
        assert_eq!(
            max_supported_overlay_schema_version("ui://example/view"),
            None
        );
        assert_eq!(max_supported_overlay_schema_version(""), None);
    }
}
