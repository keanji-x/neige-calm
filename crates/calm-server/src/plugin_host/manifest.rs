//! Plugin manifest parsing and validation.
//!
//! Every plugin ships a `manifest.json` at the root of its install directory.
//! This module owns the serde-typed shape, the validator (id / version / scope
//! rules locked in by §1 + §10 of `docs/m3-design.md`), and `ManifestError` —
//! the unified error surface install + boot paths report.
//!
//! M3 scope reminders (do not relax without re-reading the design doc):
//!
//!   * `manifest_version` must equal `1`.
//!   * `views[].scope` is the closed set `["card"]` — `"wave"` and `"cove"`
//!     are explicitly rejected per §10 #1 and #5.
//!   * Validation is hand-written (no `jsonschema` crate). The surface is
//!     small enough that pulling the dep is not worth it for Slice A.
//!
//! NOTE: This file is Slice A only. Slice B will read the parsed `Manifest`
//! to spawn the process; Slice C will consult `Permissions` on every callback.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::card_kind::CardKindRegistry;
use crate::mcp_server::tools::plan::{
    GateInput, PlanTaskInput, key_is_valid, validate_gate_shape, validate_new_plan_batch,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Public types — the on-disk JSON shape, 1:1 with §1.1 of the design doc.
// ---------------------------------------------------------------------------

/// Top-level manifest blob loaded from `<install_path>/manifest.json`.
///
/// Unknown fields are tolerated (forwards compatibility). Missing optional
/// fields default; missing required fields fail in `parse`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Manifest {
    /// Always `1` for M3. Other values are rejected by `parse`.
    pub manifest_version: u32,

    /// Reverse-DNS or slug, see `is_valid_plugin_id`. Stable across versions.
    pub id: String,

    /// Semver string. Validated; stored verbatim.
    pub version: String,

    /// Refuse to spawn if the running kernel is older than this. Validated as
    /// semver here; the actual comparison runs at spawn time (Slice B).
    pub min_kernel_version: String,

    pub display_name: String,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub author: Option<Author>,

    #[serde(default)]
    pub license: Option<String>,

    #[serde(default)]
    pub homepage: Option<String>,

    pub entrypoint: Entrypoint,

    /// At least one view recommended; an empty array is technically legal but
    /// such a plugin can never surface a card. We don't reject — the validator
    /// only enforces per-element rules. `AddPanel` will simply show nothing.
    #[serde(default)]
    pub views: Vec<View>,

    /// Worker-facing outbound tool allowlist (#760 slice 2). The kernel reads
    /// and enforces this for MCP `tools/list` discovery and `tools/call`
    /// routing; unrelated to iframe→kernel `permissions.tools`.
    #[serde(default)]
    pub exposes_tools: Vec<ExposedTool>,

    /// Trusted forge plugins may declare durable workflow descriptors. The
    /// registry/binding layer ignores this field for untrusted plugins; the
    /// manifest parser still validates the shape so broken descriptors fail
    /// close to the authoring point.
    #[serde(default)]
    pub workflows: Vec<WorkflowDescriptor>,

    /// Missing block treated as the most-restrictive permission set.
    #[serde(default)]
    pub permissions: Permissions,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Author {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// How to launch the plugin process. Kernel-injected env (token, sock, data
/// dir) merges over this at spawn time — that's Slice B.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Entrypoint {
    /// Relative to `install_path`. Slice B is responsible for sandboxing the
    /// path (no `../` escape); validation here only enforces non-emptiness.
    pub command: String,

    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

/// One plugin-rendered view. Each becomes a card-kind candidate in `AddPanel`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct View {
    pub view_id: String,
    pub title: String,

    #[serde(default)]
    pub icon: Option<String>,

    /// Closed set for M3: `"card"` only. The validator rejects anything else
    /// with an explicit error pointing at this field.
    pub scope: String,

    #[serde(default)]
    pub default_size: Option<ViewSize>,

    /// Static-asset HTML rendered in the iframe. Optional: if absent, Slice D's
    /// HTTP layer is expected to proxy to the plugin process at `/views/<id>`.
    #[serde(default)]
    pub entry_html: Option<String>,

    /// MCP Apps `_meta.ui.csp` mirror (migration doc §6/M3). When set, the
    /// kernel emits it under `_meta.ui` of the `resources/read` response so
    /// AppBridge's sandbox proxy can enforce the right Content-Security-Policy
    /// on the inner iframe. Absent → AppBridge falls back to its no-network
    /// default. M3 is intentionally loose about the inner shape; refinement
    /// (closed set of keys, glob validation) lands in M5 when we wire the
    /// transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csp: Option<CspBlock>,

    /// MCP Apps `_meta.ui.permissions` mirror. Today only the `tools` slot is
    /// populated (list of tool-name globs the iframe may call); the closed
    /// camera/microphone/etc. set in the upstream spec will land alongside
    /// AppBridge integration in M5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<UiPermissions>,
}

/// `_meta.ui.csp` mirror — kept open-shape so we can pass unmodeled directives
/// straight through to AppBridge without bumping the manifest schema.
///
/// The five named fields are the ones the spec calls out explicitly
/// (default_src, script_src, style_src, connect_src, img_src); everything
/// else flows through `extras` via `#[serde(flatten)]`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CspBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_src: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_src: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_src: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_src: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub img_src: Option<Vec<String>>,
    /// Unmodeled directives — forwarded verbatim. Keeps us forward-compatible
    /// with frame_src, font_src, worker_src, base_uri, etc. without a schema
    /// bump every time AppBridge gains support for one.
    #[serde(flatten)]
    pub extras: HashMap<String, Vec<String>>,
}

/// `_meta.ui.permissions` mirror. We only model `tools` for M3 (matches §1.2
/// of the migration doc — the closed set of host-feature permissions land
/// alongside AppBridge in M5).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct UiPermissions {
    /// Tool-name globs the iframe is allowed to invoke via
    /// `app.callServerTool`. Empty / absent → no iframe-initiated tool calls.
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ViewSize {
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub min_w: Option<u32>,
    #[serde(default)]
    pub min_h: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolKind {
    ForgeAction,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExposedTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<ToolKind>,
    /// Optional JSON Schema for the tool's MCP `inputSchema`. When absent the
    /// kernel falls back to a permissive empty object schema. Without this a
    /// real agent calls the tool with empty args (see #840 d1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// Optional MCP tool annotations (title/readOnlyHint/etc.) surfaced in tools/list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkflowDescriptor {
    pub id: String,
    #[serde(default)]
    pub plan_template: Vec<PlanTaskInput>,
    #[serde(default)]
    pub gates: Vec<GateInput>,
    #[serde(default)]
    pub spec_instructions: String,
    #[serde(default)]
    pub card_kinds: Vec<String>,
    /// #891 — optional JSON Schema (supported subset, see
    /// `plugin_host::workflow_input`) declaring the shape of the
    /// `workflow_input` a wave may carry when it binds this workflow.
    /// Absent (`None`): the workflow does not accept `workflow_input`.
    /// Field convention mirrors `ExposedTool::input_schema` above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

/// Permissions the plugin requests. Kernel enforces at the callback dispatch
/// layer (Slice C). Defaults are the most-restrictive (nothing granted).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Permissions {
    /// Which `entity_kind` strings the plugin may overlay-write to (subset of
    /// `["wave", "card"]`). Empty = no overlay writes.
    #[serde(default)]
    pub overlays_write: Vec<String>,

    /// May create cards under its own prefix (`plugin:<id>:<view>`).
    #[serde(default)]
    pub cards_create: bool,

    /// May read all cards (not just its own).
    #[serde(default)]
    pub cards_read_all: bool,

    /// Event-topic globs the plugin may subscribe to. Empty = no events.
    #[serde(default)]
    pub events_subscribe: Vec<String>,

    /// Per-plugin KV store cap in bytes. Slice C enforces; 0 = no KV access.
    #[serde(default)]
    pub kv_quota_bytes: u64,

    /// Future expansion (declared roots). Validated as a list of strings; no
    /// semantics in M3.
    #[serde(default)]
    pub filesystem: Vec<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Manifest parse / validation failure. The `Display` impl carries enough
/// detail (field path, expected shape) to be useful in HTTP 400 bodies and in
/// the `tracing::warn!` lines that the registry logs on skipped manifests.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// JSON syntax error. Wraps `serde_json::Error` so its line/col surface
    /// directly to the user.
    #[error("manifest JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// Field-level rule violation. `field` is a dotted path (e.g.
    /// `views[0].scope`), `reason` is a short human string.
    #[error("manifest validation failed at `{field}`: {reason}")]
    Invalid { field: String, reason: String },
}

impl ManifestError {
    fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing + validation
// ---------------------------------------------------------------------------

impl Manifest {
    /// Parse a manifest from a JSON string and run every validation rule. The
    /// returned `Manifest` is guaranteed shape-correct; semantic concerns
    /// (does the entrypoint binary exist, etc.) are deferred to Slice B.
    pub fn parse(s: &str) -> Result<Manifest, ManifestError> {
        // Reject empty input early — `serde_json` would already, but the error
        // message is friendlier this way.
        if s.trim().is_empty() {
            return Err(ManifestError::invalid("<root>", "manifest is empty"));
        }
        let m: Manifest = serde_json::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    /// Validate an already-deserialized manifest. Exposed publicly so callers
    /// holding a `Manifest` (e.g. after editing in-memory) can re-check it.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.manifest_version != 1 {
            return Err(ManifestError::invalid(
                "manifest_version",
                format!(
                    "M3 only accepts manifest_version=1, got {}",
                    self.manifest_version
                ),
            ));
        }

        if !is_valid_plugin_id(&self.id) {
            return Err(ManifestError::invalid(
                "id",
                "must match ^[a-z0-9][a-z0-9.-]{1,63}$ (reverse-DNS or slug, \
                 lowercase, 2–64 chars, alphanumerics plus '.' and '-')",
            ));
        }

        if Version::parse(&self.version).is_err() {
            return Err(ManifestError::invalid(
                "version",
                format!("`{}` is not a valid semver string", self.version),
            ));
        }

        if Version::parse(&self.min_kernel_version).is_err() {
            return Err(ManifestError::invalid(
                "min_kernel_version",
                format!("`{}` is not a valid semver string", self.min_kernel_version),
            ));
        }

        if self.display_name.trim().is_empty() {
            return Err(ManifestError::invalid("display_name", "must be non-empty"));
        }

        if self.entrypoint.command.trim().is_empty() {
            return Err(ManifestError::invalid(
                "entrypoint.command",
                "must be non-empty",
            ));
        }
        // Reject absolute paths and `..` escapes early — Slice B will also
        // re-check, but flagging here gives users a clearer error.
        if self.entrypoint.command.starts_with('/') || self.entrypoint.command.contains("..") {
            return Err(ManifestError::invalid(
                "entrypoint.command",
                "must be a relative path inside the plugin install dir \
                 (no leading `/`, no `..` segments)",
            ));
        }

        for (i, view) in self.views.iter().enumerate() {
            view.validate(i)?;
        }

        for (i, workflow) in self.workflows.iter().enumerate() {
            workflow.validate(i)?;
        }

        self.permissions.validate()?;

        Ok(())
    }
}

impl View {
    fn validate(&self, idx: usize) -> Result<(), ManifestError> {
        let path = |s: &str| format!("views[{idx}].{s}");

        if !is_valid_view_id(&self.view_id) {
            return Err(ManifestError::invalid(
                path("view_id"),
                "must match ^[a-z0-9][a-z0-9-]{0,31}$",
            ));
        }
        if self.title.trim().is_empty() {
            return Err(ManifestError::invalid(path("title"), "must be non-empty"));
        }
        // §10 #1 + #5: M3 scope enum is exactly `["card"]`. Be explicit about
        // rejecting "wave" and "cove" so the error message points at the
        // design doc, not just "unknown enum value".
        match self.scope.as_str() {
            "card" => {}
            "wave" => {
                return Err(ManifestError::invalid(
                    path("scope"),
                    "wave-scope views are deferred past M3 (design doc §10 #5); \
                     only \"card\" is accepted",
                ));
            }
            "cove" => {
                return Err(ManifestError::invalid(
                    path("scope"),
                    "cove-scope views are banned for M3 (design doc §10 #1); \
                     only \"card\" is accepted",
                ));
            }
            other => {
                return Err(ManifestError::invalid(
                    path("scope"),
                    format!("unknown scope `{other}`; expected \"card\""),
                ));
            }
        }
        Ok(())
    }
}

impl WorkflowDescriptor {
    fn validate(&self, idx: usize) -> Result<(), ManifestError> {
        let path = |s: &str| format!("workflows[{idx}].{s}");

        if !key_is_valid(&self.id) {
            return Err(ManifestError::invalid(
                path("id"),
                "must match ^[a-z0-9][a-z0-9._-]{0,63}$",
            ));
        }

        validate_new_plan_batch(&self.plan_template).map_err(|reason| {
            ManifestError::invalid(
                plan_template_error_field(idx, &self.plan_template, &reason),
                reason,
            )
        })?;

        for (gate_idx, gate) in self.gates.iter().enumerate() {
            validate_gate_shape(&self.id, gate).map_err(|reason| {
                ManifestError::invalid(
                    gate_error_field(path(&format!("gates[{gate_idx}]")), &reason),
                    reason,
                )
            })?;
        }

        if self.spec_instructions.len() > 8192 {
            return Err(ManifestError::invalid(
                path("spec_instructions"),
                "must be at most 8192 bytes",
            ));
        }
        if self
            .spec_instructions
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(ManifestError::invalid(
                path("spec_instructions"),
                "must not contain control characters other than newline or tab",
            ));
        }

        let builtins = CardKindRegistry::builtins();
        for (kind_idx, kind) in self.card_kinds.iter().enumerate() {
            if builtins.claims_kind(kind) {
                return Err(ManifestError::invalid(
                    path(&format!("card_kinds[{kind_idx}]")),
                    format!("card kind `{kind}` collides with a built-in card kind"),
                ));
            }
        }

        // #891 — `input_schema` must stay inside the supported subset so the
        // kernel-side instance validator executes every declared constraint
        // (fail-close at the authoring point, same spirit as plan_template).
        if let Some(schema) = self.input_schema.as_ref() {
            crate::plugin_host::workflow_input::validate_input_schema(schema)
                .map_err(|e| ManifestError::invalid(path(&e.path), e.reason))?;
        }

        Ok(())
    }
}

fn plan_template_error_field(workflow_idx: usize, tasks: &[PlanTaskInput], reason: &str) -> String {
    let path = |s: &str| format!("workflows[{workflow_idx}].{s}");

    if let Some(key) = backtick_value_after(reason, "invalid task key `") {
        let task_idx = tasks
            .iter()
            .position(|task| task.key == key)
            .or_else(|| tasks.iter().position(|task| !key_is_valid(&task.key)));
        if let Some(task_idx) = task_idx {
            return path(&format!("plan_template[{task_idx}].key"));
        }
    }

    if let Some(key) = backtick_value_after(reason, "duplicate key `") {
        let mut seen = HashSet::new();
        for (task_idx, task) in tasks.iter().enumerate() {
            let duplicate = !seen.insert(task.key.as_str());
            if duplicate && task.key == key {
                return path(&format!("plan_template[{task_idx}].key"));
            }
        }
    }

    let Some(key) = task_key_from_plan_error(reason) else {
        return path("plan_template");
    };
    let Some(task_idx) = tasks.iter().position(|task| task.key == key) else {
        return path("plan_template");
    };

    if reason.contains("unknown kind") {
        return path(&format!("plan_template[{task_idx}].kind"));
    }
    if reason.contains("`goal`") {
        return path(&format!("plan_template[{task_idx}].goal"));
    }
    if reason.contains("gate.") {
        return gate_error_field(path(&format!("plan_template[{task_idx}].gate")), reason);
    }
    if reason.contains("cwd") {
        return path(&format!("plan_template[{task_idx}].cwd"));
    }
    if reason.contains("unknown dependency")
        && let Some(dep) = backtick_value_after(reason, "unknown dependency `")
        && let Some(dep_idx) = tasks[task_idx]
            .depends_on
            .iter()
            .position(|candidate| candidate == dep)
    {
        return path(&format!("plan_template[{task_idx}].depends_on[{dep_idx}]"));
    }
    if reason.contains("`no_gate_reason`") {
        return path(&format!("plan_template[{task_idx}].no_gate_reason"));
    }
    if reason.contains("requires `context`") {
        return path(&format!("plan_template[{task_idx}].context"));
    }

    path(&format!("plan_template[{task_idx}]"))
}

fn task_key_from_plan_error(reason: &str) -> Option<&str> {
    reason
        .strip_prefix("task ")?
        .split_once(':')
        .map(|(key, _)| key)
}

fn backtick_value_after<'a>(reason: &'a str, prefix: &str) -> Option<&'a str> {
    let start = reason.find(prefix)? + prefix.len();
    reason[start..].split_once('`').map(|(value, _)| value)
}

fn gate_error_field(base: String, reason: &str) -> String {
    if reason.contains("gate.steps must be non-empty") {
        return format!("{base}.steps");
    }
    if let Some(step_idx) = indexed_field(reason, "gate.steps[") {
        if reason.contains(&format!("gate.steps[{step_idx}].name")) {
            return format!("{base}.steps[{step_idx}].name");
        }
        if reason.contains(&format!("gate.steps[{step_idx}].cmd")) {
            return format!("{base}.steps[{step_idx}].cmd");
        }
        return format!("{base}.steps[{step_idx}]");
    }
    if reason.contains("gate.timeout_secs") {
        return format!("{base}.timeout_secs");
    }
    if reason.contains("gate.cwd") {
        return format!("{base}.cwd");
    }
    base
}

fn indexed_field(reason: &str, needle: &str) -> Option<usize> {
    let start = reason.find(needle)? + needle.len();
    let end = reason[start..].find(']')?;
    reason[start..start + end].parse().ok()
}

impl Permissions {
    fn validate(&self) -> Result<(), ManifestError> {
        // overlays_write: each entry must be either "wave" or "card".
        // No other entity kinds exist in the kernel today.
        for (i, kind) in self.overlays_write.iter().enumerate() {
            if kind != "wave" && kind != "card" {
                return Err(ManifestError::invalid(
                    format!("permissions.overlays_write[{i}]"),
                    format!(
                        "must be \"wave\" or \"card\"; got `{kind}` \
                         (kernel knows no other entity kinds)"
                    ),
                ));
            }
        }
        // events_subscribe: globs are validated by the event bus, not here.
        // We only reject empty strings (almost certainly a typo).
        for (i, topic) in self.events_subscribe.iter().enumerate() {
            if topic.trim().is_empty() {
                return Err(ManifestError::invalid(
                    format!("permissions.events_subscribe[{i}]"),
                    "topic glob must be non-empty",
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Validators — hand-rolled instead of pulling `regex` for two tiny patterns.
// ---------------------------------------------------------------------------

/// `^[a-z0-9][a-z0-9.-]{1,63}$` — total 2..=64 chars; head is alphanumeric.
fn is_valid_plugin_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes.len() > 64 {
        return false;
    }
    if !is_lower_alnum(bytes[0]) {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|&b| is_lower_alnum(b) || b == b'.' || b == b'-')
}

/// `^[a-z0-9][a-z0-9-]{0,31}$` — total 1..=32 chars; head is alphanumeric.
fn is_valid_view_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !is_lower_alnum(bytes[0]) {
        return false;
    }
    bytes[1..].iter().all(|&b| is_lower_alnum(b) || b == b'-')
}

fn is_lower_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

// ---------------------------------------------------------------------------
// Public-API conveniences
// ---------------------------------------------------------------------------

impl Manifest {
    /// Render the validated manifest back to a JSON `Value`. Useful when
    /// persisting into the `plugins.manifest` column without re-reading the
    /// file from disk.
    pub fn to_json(&self) -> Value {
        // `unwrap` here is fine: every field type is serde-derived from data
        // that already round-tripped through `serde_json::from_str`.
        serde_json::to_value(self).expect("Manifest serializable")
    }
}

impl fmt::Display for Manifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} v{} ({})", self.id, self.version, self.display_name)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hello_world() -> &'static str {
        r#"{
            "manifest_version": 1,
            "id": "dev.neige.hello-world",
            "version": "0.1.0",
            "min_kernel_version": "0.3.0",
            "display_name": "Hello World",
            "description": "Reference plugin.",
            "author": { "name": "Neige", "url": "https://neige.dev" },
            "license": "MIT",
            "entrypoint": {
                "command": "bin/hello-world",
                "args": ["--serve"],
                "env": { "RUST_LOG": "info" }
            },
            "views": [
                {
                    "view_id": "status",
                    "title": "Hello status",
                    "scope": "card",
                    "default_size": { "w": 4, "h": 5, "min_w": 3, "min_h": 3 },
                    "entry_html": "views/status.html"
                }
            ],
            "exposes_tools": [
                { "name": "hello.ping", "description": "Returns 'pong'" },
                {
                    "name": "hello.forge",
                    "description": "Returns a lowered forge-action payload",
                    "kind": "forge-action"
                }
            ],
            "permissions": {
                "overlays_write": ["wave", "card"],
                "cards_create": true,
                "cards_read_all": true,
                "events_subscribe": ["*"],
                "kv_quota_bytes": 1048576,
                "filesystem": []
            }
        }"#
    }

    #[test]
    fn parses_valid_hello_world_manifest() {
        let m = Manifest::parse(hello_world()).expect("valid manifest");
        assert_eq!(m.id, "dev.neige.hello-world");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.views.len(), 1);
        assert_eq!(m.views[0].scope, "card");
        assert_eq!(m.exposes_tools.len(), 2);
        assert!(m.permissions.cards_create);
        assert_eq!(m.permissions.kv_quota_bytes, 1_048_576);
    }

    #[test]
    fn parses_minimal_manifest_with_defaults() {
        let json = r#"{
            "manifest_version": 1,
            "id": "x.y",
            "version": "1.0.0",
            "min_kernel_version": "0.0.1",
            "display_name": "X",
            "entrypoint": { "command": "bin/x" }
        }"#;
        let m = Manifest::parse(json).expect("minimal");
        assert!(m.views.is_empty());
        assert!(m.exposes_tools.is_empty());
        // Missing permissions block → default Permissions (no grants).
        assert!(!m.permissions.cards_create);
        assert!(m.permissions.overlays_write.is_empty());
    }

    fn workflow_manifest_value() -> Value {
        json!({
            "manifest_version": 1,
            "id": "dev.neige.workflow-test",
            "version": "1.0.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Workflow Test",
            "entrypoint": { "command": "bin/workflow-test" },
            "workflows": [
                {
                    "id": "issue-development",
                    "plan_template": [
                        {
                            "key": "inspect",
                            "kind": "codex",
                            "goal": "Inspect the issue.",
                            "depends_on": [],
                            "gate": {
                                "steps": [
                                    { "name": "test", "cmd": "cargo test" }
                                ]
                            }
                        },
                        {
                            "key": "implement",
                            "kind": "claude",
                            "goal": "Implement the change.",
                            "depends_on": ["inspect"]
                        }
                    ],
                    "gates": [
                        {
                            "steps": [
                                { "name": "fmt", "cmd": "cargo fmt --all --check" }
                            ]
                        }
                    ],
                    "spec_instructions": "Use the workflow descriptor for wave {wave_id}.\nKeep it concise.",
                    "card_kinds": ["plugin:dev.neige.workflow-test:custom"]
                }
            ],
            "permissions": {}
        })
    }

    fn parse_manifest_value(v: Value) -> Result<Manifest, ManifestError> {
        Manifest::parse(&serde_json::to_string(&v).expect("serialize manifest value"))
    }

    fn context_str<'a>(task: &'a PlanTaskInput, key: &str) -> &'a str {
        task.context
            .as_ref()
            .and_then(|context| context.get(key))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("task {} missing context string {key}", task.key))
    }

    fn task<'a>(tasks: &'a HashMap<&str, &PlanTaskInput>, key: &str) -> &'a PlanTaskInput {
        tasks
            .get(key)
            .copied()
            .unwrap_or_else(|| panic!("missing task {key}"))
    }

    fn depends_on(task: &PlanTaskInput) -> Vec<&str> {
        task.depends_on.iter().map(String::as_str).collect()
    }

    #[test]
    fn parses_workflow_descriptor() {
        let m = parse_manifest_value(workflow_manifest_value()).expect("workflow manifest");
        assert_eq!(m.workflows.len(), 1);
        assert_eq!(m.workflows[0].id, "issue-development");
        assert_eq!(m.workflows[0].plan_template.len(), 2);
        assert_eq!(m.workflows[0].gates.len(), 1);
    }

    #[test]
    fn parses_shipped_issue_development_descriptor() {
        let m = Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
            .expect("shipped git-forge manifest");
        let workflow = m
            .workflows
            .iter()
            .find(|workflow| workflow.id == "issue-development")
            .expect("issue-development workflow");
        let tasks = workflow
            .plan_template
            .iter()
            .map(|task| (task.key.as_str(), task))
            .collect::<HashMap<_, _>>();

        assert_eq!(workflow.plan_template.len(), 8);
        assert_eq!(
            depends_on(task(&tasks, "review-design-a")),
            vec!["inspect-issue"]
        );
        assert_eq!(
            depends_on(task(&tasks, "review-design-b")),
            vec!["inspect-issue"]
        );
        assert_eq!(
            depends_on(task(&tasks, "implement-change")),
            vec!["review-design-a", "review-design-b"]
        );
        assert_eq!(
            depends_on(task(&tasks, "open-pr")),
            vec!["implement-change"]
        );
        assert_eq!(depends_on(task(&tasks, "review-pr-a")), vec!["open-pr"]);
        assert_eq!(depends_on(task(&tasks, "review-pr-b")), vec!["open-pr"]);
        assert_eq!(
            depends_on(task(&tasks, "merge")),
            vec!["review-pr-a", "review-pr-b"]
        );
        assert_eq!(
            context_str(task(&tasks, "review-design-a"), "reviewer_role"),
            "design-correctness"
        );
        assert_eq!(
            context_str(task(&tasks, "review-design-b"), "reviewer_role"),
            "design-failure-path"
        );
        assert_ne!(
            context_str(task(&tasks, "review-design-a"), "reviewer_role"),
            context_str(task(&tasks, "review-design-b"), "reviewer_role")
        );
        assert_eq!(
            context_str(task(&tasks, "review-pr-a"), "reviewer_role"),
            "pr-correctness"
        );
        assert_eq!(
            context_str(task(&tasks, "review-pr-b"), "reviewer_role"),
            "pr-failure-path"
        );
        assert_ne!(
            context_str(task(&tasks, "review-pr-a"), "reviewer_role"),
            context_str(task(&tasks, "review-pr-b"), "reviewer_role")
        );
        assert_eq!(context_str(task(&tasks, "review-design-a"), "channel"), "a");
        assert_eq!(context_str(task(&tasks, "review-design-b"), "channel"), "b");
        assert_eq!(context_str(task(&tasks, "review-pr-a"), "channel"), "a");
        assert_eq!(context_str(task(&tasks, "review-pr-b"), "channel"), "b");
        assert!(workflow.spec_instructions.len() <= 8192);
        assert!(
            !workflow
                .spec_instructions
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        );
        for needle in [
            "calm.review.round",
            "approved",
            "changes_requested",
            "cap is the fixed policy constant 8",
            // #891 slice ② review fix — cap+2 is scoped to cap-exhaustion
            // grants; a merge_hold/repo_mismatch grant licenses no raise.
            "after a cap-exhaustion ratify grant it is the previous cap plus exactly 2",
            "cap = previous cap + 2",
            "Always re-review",
            "expected_head_sha",
            "calm.ratify.request",
            "reason:\"cap_exhausted\"",
            "root_cause",
            // #891 slice ② — workflow-input ingest / repo cross-check /
            // merge_policy semantics live in spec_instructions (agent
            // policy, deliberately not kernel-enforced); pin their presence.
            "Bound Workflow Input",
            "gh.issue.view",
            // Reason strings are a single free-form field on the kernel
            // side; the descriptor prescribes prefix + detail encoding.
            "reason:\"repo_mismatch: input.repo=",
            "reason:\"merge_hold: pr #",
            "also the semantics whenever merge_policy is absent",
            // merge_hold lifecycle dance: ratify.request 400s unless the
            // wave is `working`, so the hold must route through it.
            "green checks, then move reviewing->working",
            // #891 slice ② r2 — post-grant half of the merge_policy
            // contract: the grant covers the already-converged head (no
            // extra review round), and the wave resumes working->reviewing
            // before merging per fence F4.
            "no fresh review round is required for the hold itself",
            "resume working->reviewing and call gh.pr.merge per fence F4",
        ] {
            assert!(
                workflow.spec_instructions.contains(needle),
                "spec_instructions missing {needle}"
            );
        }

        // #891 slice ② — the shipped descriptor's input contract. Parsing
        // via `Manifest::parse` already ran `validate()`, so reaching here
        // proves the schema passes the slice-① subset validator.
        let schema = workflow
            .input_schema
            .as_ref()
            .expect("issue-development declares input_schema");
        assert_eq!(schema["type"], "object");
        assert_eq!(
            schema["required"],
            serde_json::json!(["issue_url", "repo", "issue_number"])
        );
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        assert_eq!(schema["properties"]["issue_url"]["type"], "string");
        assert_eq!(schema["properties"]["repo"]["type"], "string");
        // F8: integer-encoded only — the type must be the strict "integer".
        assert_eq!(schema["properties"]["issue_number"]["type"], "integer");
        assert_eq!(schema["properties"]["merge_policy"]["type"], "string");
        assert_eq!(
            schema["properties"]["merge_policy"]["enum"],
            serde_json::json!(["hold-for-ratify", "auto-merge"])
        );
        // Documentation-only default (kernel never applies it; absent ⇒
        // hold-for-ratify semantics are spelled out in spec_instructions).
        assert_eq!(
            schema["properties"]["merge_policy"]["default"],
            "hold-for-ratify"
        );
        assert_eq!(schema["properties"]["notes"]["type"], "string");

        // Plan-template tweak (§2.3): inspect-issue references the bound
        // input instead of assuming a pasted goal.
        let inspect = task(&tasks, "inspect-issue");
        assert!(inspect.goal.contains("bound workflow input"));
        assert!(inspect.goal.contains("gh.issue.view"));
        assert!(inspect.goal.contains("input.repo"));
        let acceptance = inspect.acceptance_criteria.as_deref().unwrap_or("");
        assert!(acceptance.contains("origin remote matches input.repo"));

        // #891 slice ② review fix — the merge task is policy-conditional:
        // its goal must not push agents past the hold-for-ratify gate, and
        // parking at the merge_hold ratify request is an accepted outcome.
        let merge = task(&tasks, "merge");
        assert!(merge.goal.contains("merge_policy-required ratify grant"));
        assert!(merge.goal.contains("park at the merge_hold ratify request"));
        let merge_acceptance = merge.acceptance_criteria.as_deref().unwrap_or("");
        assert!(merge_acceptance.contains("policy-required ratify grant"));
        assert!(merge_acceptance.contains("no merge performed"));
    }

    #[test]
    fn workflow_descriptor_rejects_invalid_shapes() {
        let cases: Vec<(&str, Value, &str)> = vec![
            ("empty id", json!(""), "workflows[0].id"),
            ("bad id", json!("Bad Id"), "workflows[0].id"),
        ];
        for (label, id, field) in cases {
            let mut v = workflow_manifest_value();
            v["workflows"][0]["id"] = id;
            let err = parse_manifest_value(v).expect_err(label);
            assert!(
                matches!(err, ManifestError::Invalid { field: ref actual, .. } if actual == field),
                "{label}: got {err:?}"
            );
        }

        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["key"] = json!("Bad Key");
        let err = parse_manifest_value(v).expect_err("bad plan key");
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "workflows[0].plan_template[0].key")
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][1]["depends_on"] = json!(["missing"]);
        let err = parse_manifest_value(v).expect_err("missing dependency");
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "workflows[0].plan_template[1].depends_on[0]")
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["kind"] = json!("worker");
        let err = parse_manifest_value(v).expect_err("unknown task kind");
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "workflows[0].plan_template[0].kind")
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["gates"][0]["steps"] = json!([]);
        let err = parse_manifest_value(v).expect_err("empty workflow gate");
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "workflows[0].gates[0].steps")
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["gate"]["steps"] = json!([]);
        let err = parse_manifest_value(v).expect_err("empty task gate");
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "workflows[0].plan_template[0].gate.steps")
        );
    }

    #[test]
    fn workflow_descriptor_rejects_plan_template_cycles() {
        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["depends_on"] = json!(["implement"]);
        v["workflows"][0]["plan_template"][1]["depends_on"] = json!(["inspect"]);
        let err = parse_manifest_value(v).expect_err("cycle");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].plan_template" && reason.contains("dependency cycle")),
            "got {err:?}"
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["depends_on"] = json!(["inspect"]);
        let err = parse_manifest_value(v).expect_err("self dependency");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].plan_template" && reason.contains("dependency cycle: inspect -> inspect")),
            "got {err:?}"
        );
    }

    #[test]
    fn workflow_descriptor_rejects_plan_gate_content_like_plan_upsert() {
        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["gate"]["steps"][0]["cmd"] = json!("  ");
        let err = parse_manifest_value(v).expect_err("blank task gate cmd");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].plan_template[0].gate.steps[0].cmd" && reason.contains("cmd must be non-empty")),
            "got {err:?}"
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["gate"]["steps"][0]["name"] = json!("");
        let err = parse_manifest_value(v).expect_err("blank task gate name");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].plan_template[0].gate.steps[0].name" && reason.contains("name must be non-empty")),
            "got {err:?}"
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["plan_template"][0]["gate"]["timeout_secs"] = json!(7201);
        let err = parse_manifest_value(v).expect_err("task gate timeout too high");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].plan_template[0].gate.timeout_secs" && reason.contains("1..=7200")),
            "got {err:?}"
        );
    }

    #[test]
    fn workflow_descriptor_rejects_workflow_gate_content_like_plan_upsert() {
        let mut v = workflow_manifest_value();
        v["workflows"][0]["gates"][0]["steps"][0]["cmd"] = json!("  ");
        let err = parse_manifest_value(v).expect_err("blank workflow gate cmd");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].gates[0].steps[0].cmd" && reason.contains("cmd must be non-empty")),
            "got {err:?}"
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["gates"][0]["steps"][0]["name"] = json!("");
        let err = parse_manifest_value(v).expect_err("blank workflow gate name");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].gates[0].steps[0].name" && reason.contains("name must be non-empty")),
            "got {err:?}"
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["gates"][0]["timeout_secs"] = json!(7201);
        let err = parse_manifest_value(v).expect_err("workflow gate timeout too high");
        assert!(
            matches!(&err, ManifestError::Invalid { field, reason } if field == "workflows[0].gates[0].timeout_secs" && reason.contains("1..=7200")),
            "got {err:?}"
        );
    }

    #[test]
    fn workflow_descriptor_rejects_bad_spec_instructions() {
        let mut v = workflow_manifest_value();
        v["workflows"][0]["spec_instructions"] = json!("x".repeat(8193));
        let err = parse_manifest_value(v).expect_err("oversized spec instructions");
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "workflows[0].spec_instructions")
        );

        let mut v = workflow_manifest_value();
        v["workflows"][0]["spec_instructions"] = json!("bad\u{0007}");
        let err = parse_manifest_value(v).expect_err("control char spec instructions");
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "workflows[0].spec_instructions")
        );
    }

    #[test]
    fn workflow_descriptor_accepts_subset_input_schema_and_defaults_to_none() {
        // No input_schema (today's shipped manifests) → parses to None.
        let manifest = parse_manifest_value(workflow_manifest_value()).expect("valid manifest");
        assert!(manifest.workflows[0].input_schema.is_none());

        let mut v = workflow_manifest_value();
        v["workflows"][0]["input_schema"] = json!({
            "type": "object",
            "properties": {
                "issue_url": { "type": "string", "description": "Canonical issue URL" },
                "merge_policy": {
                    "type": "string",
                    "enum": ["hold-for-ratify", "auto-merge"],
                    "default": "hold-for-ratify"
                }
            },
            "required": ["issue_url"],
            "additionalProperties": false
        });
        let manifest = parse_manifest_value(v).expect("subset input_schema accepted");
        assert!(manifest.workflows[0].input_schema.is_some());
    }

    /// #891 — the subset validator runs at manifest parse; exhaustive
    /// keyword/coherence coverage lives in `plugin_host::workflow_input`
    /// (this pins the `workflows[i].input_schema…` field-path wiring).
    #[test]
    fn workflow_descriptor_rejects_out_of_subset_input_schema() {
        let cases: [(&str, Value, &str); 5] = [
            (
                "hostile $ref keyword",
                json!({
                    "type": "object",
                    "$ref": "#/defs/x",
                    "additionalProperties": false
                }),
                "workflows[0].input_schema.$ref",
            ),
            (
                "hostile property keyword (pattern)",
                json!({
                    "type": "object",
                    "properties": { "u": { "type": "string", "pattern": ".*" } },
                    "additionalProperties": false
                }),
                "workflows[0].input_schema.properties.u.pattern",
            ),
            (
                "missing additionalProperties: false",
                json!({ "type": "object", "properties": {} }),
                "workflows[0].input_schema.additionalProperties",
            ),
            (
                "required key not declared",
                json!({
                    "type": "object",
                    "properties": {},
                    "required": ["ghost"],
                    "additionalProperties": false
                }),
                "workflows[0].input_schema.required[0]",
            ),
            (
                "enum riding a non-string type",
                json!({
                    "type": "object",
                    "properties": { "n": { "type": "integer", "enum": [1] } },
                    "additionalProperties": false
                }),
                "workflows[0].input_schema.properties.n.enum",
            ),
        ];
        for (label, schema, expected_field) in cases {
            let mut v = workflow_manifest_value();
            v["workflows"][0]["input_schema"] = schema;
            let err = parse_manifest_value(v).expect_err(label);
            assert!(
                matches!(&err, ManifestError::Invalid { field, .. } if field == expected_field),
                "{label}: got {err:?}"
            );
        }
    }

    #[test]
    fn workflow_descriptor_rejects_builtin_card_kind_collision() {
        for (label, kind) in [
            ("exact built-in", "terminal"),
            ("builtin prefix", "ui://dev.neige.workflow-test/custom"),
        ] {
            let mut v = workflow_manifest_value();
            v["workflows"][0]["card_kinds"] = json!([kind]);
            let err = parse_manifest_value(v).expect_err(label);
            assert!(
                matches!(err, ManifestError::Invalid { ref field, .. } if field == "workflows[0].card_kinds[0]"),
                "{label}: got {err:?}"
            );
        }
    }

    #[test]
    fn missing_required_field_fails() {
        // `entrypoint` missing entirely.
        let json = r#"{
            "manifest_version": 1,
            "id": "a.b",
            "version": "1.0.0",
            "min_kernel_version": "0.1.0",
            "display_name": "X"
        }"#;
        let err = Manifest::parse(json).expect_err("missing entrypoint");
        assert!(matches!(err, ManifestError::Json(_)), "got {err:?}");
    }

    #[test]
    fn empty_string_fails() {
        let err = Manifest::parse("").unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { .. }));
    }

    #[test]
    fn bad_manifest_version_fails() {
        let json = r#"{
            "manifest_version": 2,
            "id": "a.b",
            "version": "1.0.0",
            "min_kernel_version": "0.1.0",
            "display_name": "X",
            "entrypoint": { "command": "bin/x" }
        }"#;
        let err = Manifest::parse(json).unwrap_err();
        match err {
            ManifestError::Invalid { field, .. } => assert_eq!(field, "manifest_version"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn bad_id_rejected_uppercase() {
        let json = hello_world().replace("dev.neige.hello-world", "Dev.Neige.HelloWorld");
        let err = Manifest::parse(&json).unwrap_err();
        match err {
            ManifestError::Invalid { field, .. } => assert_eq!(field, "id"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn bad_id_rejected_too_short() {
        let json = hello_world().replace("dev.neige.hello-world", "a");
        let err = Manifest::parse(&json).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { field, .. } if field == "id"));
    }

    #[test]
    fn bad_id_rejected_illegal_char() {
        // underscore not allowed.
        let json = hello_world().replace("dev.neige.hello-world", "dev_neige");
        let err = Manifest::parse(&json).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { field, .. } if field == "id"));
    }

    #[test]
    fn scope_wave_rejected() {
        let json = hello_world().replace("\"scope\": \"card\"", "\"scope\": \"wave\"");
        let err = Manifest::parse(&json).unwrap_err();
        match err {
            ManifestError::Invalid { field, reason } => {
                assert_eq!(field, "views[0].scope");
                assert!(reason.contains("wave"), "reason: {reason}");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn scope_cove_rejected() {
        let json = hello_world().replace("\"scope\": \"card\"", "\"scope\": \"cove\"");
        let err = Manifest::parse(&json).unwrap_err();
        match err {
            ManifestError::Invalid { field, reason } => {
                assert_eq!(field, "views[0].scope");
                assert!(reason.contains("cove"), "reason: {reason}");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn scope_unknown_rejected() {
        let json = hello_world().replace("\"scope\": \"card\"", "\"scope\": \"sidebar\"");
        let err = Manifest::parse(&json).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { field, .. } if field == "views[0].scope"));
    }

    #[test]
    fn bad_semver_rejected_version() {
        let json =
            hello_world().replace("\"version\": \"0.1.0\"", "\"version\": \"not-a-version\"");
        let err = Manifest::parse(&json).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { field, .. } if field == "version"));
    }

    #[test]
    fn bad_semver_rejected_min_kernel() {
        let json = hello_world().replace(
            "\"min_kernel_version\": \"0.3.0\"",
            "\"min_kernel_version\": \"v3\"",
        );
        let err = Manifest::parse(&json).unwrap_err();
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "min_kernel_version")
        );
    }

    #[test]
    fn empty_entrypoint_command_rejected() {
        let json = hello_world().replace("\"command\": \"bin/hello-world\"", "\"command\": \"\"");
        let err = Manifest::parse(&json).unwrap_err();
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "entrypoint.command")
        );
    }

    #[test]
    fn absolute_entrypoint_command_rejected() {
        let json = hello_world().replace(
            "\"command\": \"bin/hello-world\"",
            "\"command\": \"/usr/bin/evil\"",
        );
        let err = Manifest::parse(&json).unwrap_err();
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "entrypoint.command")
        );
    }

    #[test]
    fn parent_dir_entrypoint_command_rejected() {
        let json = hello_world().replace(
            "\"command\": \"bin/hello-world\"",
            "\"command\": \"../escape\"",
        );
        let err = Manifest::parse(&json).unwrap_err();
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "entrypoint.command")
        );
    }

    #[test]
    fn bad_view_id_rejected() {
        let json = hello_world().replace("\"view_id\": \"status\"", "\"view_id\": \"Has-Caps\"");
        let err = Manifest::parse(&json).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { field, .. } if field == "views[0].view_id"));
    }

    #[test]
    fn bad_overlay_kind_rejected() {
        let json = hello_world().replace("[\"wave\", \"card\"]", "[\"wave\", \"cove\"]");
        let err = Manifest::parse(&json).unwrap_err();
        match err {
            ManifestError::Invalid { field, .. } => {
                assert_eq!(field, "permissions.overlays_write[1]");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn empty_event_topic_rejected() {
        let json = hello_world().replace("[\"*\"]", "[\"\"]");
        let err = Manifest::parse(&json).unwrap_err();
        assert!(
            matches!(err, ManifestError::Invalid { field, .. } if field == "permissions.events_subscribe[0]")
        );
    }

    #[test]
    fn json_syntax_error_surfaces_as_json_variant() {
        let err = Manifest::parse("{not json").unwrap_err();
        assert!(matches!(err, ManifestError::Json(_)));
    }

    #[test]
    fn id_validator_boundaries() {
        // 2 chars minimum.
        assert!(is_valid_plugin_id("ab"));
        assert!(!is_valid_plugin_id("a"));
        // Head must be alnum.
        assert!(!is_valid_plugin_id(".a"));
        assert!(!is_valid_plugin_id("-a"));
        // 64 chars max.
        let s64: String = "a".repeat(64);
        assert!(is_valid_plugin_id(&s64));
        let s65: String = "a".repeat(65);
        assert!(!is_valid_plugin_id(&s65));
    }

    #[test]
    fn view_id_validator_boundaries() {
        assert!(is_valid_view_id("a"));
        assert!(is_valid_view_id("status-view"));
        assert!(!is_valid_view_id(""));
        assert!(!is_valid_view_id("UPPER"));
        let s32: String = "a".repeat(32);
        assert!(is_valid_view_id(&s32));
        let s33: String = "a".repeat(33);
        assert!(!is_valid_view_id(&s33));
    }

    #[test]
    fn round_trip_to_json_preserves_fields() {
        let m = Manifest::parse(hello_world()).unwrap();
        let v = m.to_json();
        let re_parsed: Manifest = serde_json::from_value(v).expect("re-parse from serialized json");
        assert_eq!(re_parsed.id, m.id);
        assert_eq!(re_parsed.views.len(), m.views.len());
    }

    #[test]
    fn exposed_tool_kind_round_trips_and_legacy_defaults_to_none() {
        let m = Manifest::parse(hello_world()).unwrap();
        assert_eq!(m.exposes_tools[0].kind, None);
        assert_eq!(m.exposes_tools[1].kind, Some(ToolKind::ForgeAction));

        let v = m.to_json();
        let re_parsed: Manifest = serde_json::from_value(v).expect("re-parse manifest JSON");
        assert_eq!(re_parsed.exposes_tools[0].kind, None);
        assert_eq!(re_parsed.exposes_tools[1].kind, Some(ToolKind::ForgeAction));

        let legacy = r#"{
            "manifest_version": 1,
            "id": "dev.neige.legacy-tool",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Legacy tool",
            "entrypoint": { "command": "bin/x" },
            "exposes_tools": [{ "name": "legacy.run" }]
        }"#;
        let legacy = Manifest::parse(legacy).expect("legacy manifest parses");
        assert_eq!(legacy.exposes_tools[0].kind, None);
    }

    // ----- M3: view-level CSP / permissions -------------------------------

    #[test]
    fn view_without_csp_or_permissions_round_trips_as_none() {
        // hello_world() declares no CSP / permissions; ensure they parse as
        // None and the serialized form omits both keys.
        let m = Manifest::parse(hello_world()).unwrap();
        assert!(m.views[0].csp.is_none());
        assert!(m.views[0].permissions.is_none());
        let v = m.to_json();
        let view_obj = v["views"][0].as_object().expect("views[0] is object");
        assert!(
            !view_obj.contains_key("csp"),
            "absent csp must not serialize"
        );
        assert!(
            !view_obj.contains_key("permissions"),
            "absent permissions must not serialize"
        );
    }

    #[test]
    fn view_with_csp_populates_struct() {
        let json = r#"{
            "manifest_version": 1,
            "id": "dev.neige.csp",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "CSP demo",
            "entrypoint": { "command": "bin/x" },
            "views": [
                {
                    "view_id": "main",
                    "title": "Main",
                    "scope": "card",
                    "csp": {
                        "default_src": ["'self'"],
                        "script_src": ["'self'", "'unsafe-inline'"],
                        "style_src": ["'self'"],
                        "connect_src": ["https://api.example.com"],
                        "img_src": ["'self'", "data:"],
                        "frame_src": ["'none'"],
                        "font_src": ["'self'", "https://fonts.gstatic.com"]
                    },
                    "permissions": {
                        "tools": ["neige.overlay.set", "neige.card.update"]
                    }
                }
            ]
        }"#;
        let m = Manifest::parse(json).expect("valid manifest");
        let view = &m.views[0];
        let csp = view.csp.as_ref().expect("csp set");
        assert_eq!(
            csp.default_src.as_deref(),
            Some(&["'self'".to_string()][..])
        );
        assert_eq!(
            csp.script_src.as_deref(),
            Some(&["'self'".to_string(), "'unsafe-inline'".to_string()][..])
        );
        assert_eq!(
            csp.connect_src.as_deref(),
            Some(&["https://api.example.com".to_string()][..])
        );
        assert_eq!(
            csp.img_src.as_deref(),
            Some(&["'self'".to_string(), "data:".to_string()][..])
        );
        // Unmodeled directives go through the catch-all extras.
        assert_eq!(
            csp.extras.get("frame_src"),
            Some(&vec!["'none'".to_string()])
        );
        assert_eq!(
            csp.extras.get("font_src"),
            Some(&vec![
                "'self'".to_string(),
                "https://fonts.gstatic.com".to_string()
            ])
        );

        let perms = view.permissions.as_ref().expect("permissions set");
        assert_eq!(
            perms.tools,
            vec![
                "neige.overlay.set".to_string(),
                "neige.card.update".to_string()
            ]
        );
    }

    #[test]
    fn view_csp_round_trip_preserves_extras() {
        let json = r#"{
            "manifest_version": 1,
            "id": "dev.neige.csprt",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "CSP RT",
            "entrypoint": { "command": "bin/x" },
            "views": [
                {
                    "view_id": "main",
                    "title": "Main",
                    "scope": "card",
                    "csp": {
                        "default_src": ["'self'"],
                        "worker_src": ["blob:"]
                    }
                }
            ]
        }"#;
        let m = Manifest::parse(json).unwrap();
        let v = m.to_json();
        let re_parsed: Manifest = serde_json::from_value(v).expect("re-parse");
        let csp = re_parsed.views[0].csp.as_ref().expect("csp");
        assert_eq!(
            csp.default_src.as_deref(),
            Some(&["'self'".to_string()][..])
        );
        assert_eq!(
            csp.extras.get("worker_src"),
            Some(&vec!["blob:".to_string()])
        );
    }
}
