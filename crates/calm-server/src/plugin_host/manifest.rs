//! Plugin manifest parsing and validation: the typed shape of `manifest.json`, its
//! validation rules, and the shared error surface.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crate::mcp_server::tools::plan::key_is_valid;
use crate::validation::KERNEL_OVERLAY_PLUGIN_ID;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The wire key [`Manifest::config_schema`] serializes to; the error root path every `config_schema` violation is reported under.
pub const CONFIG_SCHEMA_KEY: &str = "config_schema";

/// Top-level manifest blob loaded from `<install_path>/manifest.json`. Unknown fields are tolerated.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Manifest {
    /// `1` — bindings spelled `workflows` (refused now, so only binding-less v1 files load); `2` —
    /// `templates`; `3` — a `config_schema` with non-empty `required`. The bump is what makes an older
    /// kernel refuse the file by version instead of silently ignoring the key.
    pub manifest_version: u32,

    /// Reverse-DNS or slug, see `is_valid_plugin_id`. Stable across versions.
    pub id: String,

    /// Semver string. Validated; stored verbatim.
    pub version: String,

    /// Refuse to spawn if the running kernel is older than this. Validated as semver here.
    pub min_kernel_version: String,

    pub display_name: String,

    /// Connector kind. Absent ⇒ [`ConnectorKind::App`]; an unknown value is a hard parse error.
    /// `#[serde(default)]` on the FIELD is load-bearing: deriving `Default` on the enum alone does not make a missing key legal.
    #[serde(default)]
    pub kind: ConnectorKind,

    /// Remote streamable-HTTP MCP server config. Present iff `kind == McpHttp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_http: Option<McpHttpBlock>,

    /// Read-only query CLI config. Present iff `kind == CliQuery`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_query: Option<CliQueryBlock>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub author: Option<Author>,

    #[serde(default)]
    pub license: Option<String>,

    #[serde(default)]
    pub homepage: Option<String>,

    /// How to launch the plugin process. Required for [`ConnectorKind::App`], optional otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Entrypoint>,

    /// An empty array is legal but such a plugin can never surface a card.
    #[serde(default)]
    pub views: Vec<View>,

    /// Worker-facing outbound tool allowlist; unrelated to iframe→kernel `permissions.tools`.
    #[serde(default)]
    pub exposes_tools: Vec<ExposedTool>,

    /// Track `template_input` contract. Absent: the plugin does not accept `template_input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,

    /// The plugin's user-configuration contract, in the same JSON-Schema subset as [`Self::input_schema`].
    /// Not app-only. Absent ⇒ no configurable surface (`PATCH …/config` is a 400). Defaults are applied
    /// on read, not persisted. A non-empty `required` forces `manifest_version >= 3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<Value>,

    /// Kernel track-template ids a trusted forge plugin claims. Claiming is never creating: `POST
    /// /api/tracks` admits an id iff it is in the kernel's template roster, so an id outside it is inert.
    #[serde(default)]
    pub templates: Vec<TemplateDescriptor>,

    /// Missing block treated as the most-restrictive permission set.
    #[serde(default)]
    pub permissions: Permissions,
}

/// What kind of external capability this manifest describes. Connectors have no child process, no `neige.*` inbound router, no plugin token.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectorKind {
    /// The kernel-supervised process-backed plugin.
    #[default]
    App,
    /// Remote streamable-HTTP MCP server.
    McpHttp,
    /// Read-only local query CLI.
    CliQuery,
}

impl ConnectorKind {
    /// Wire token, matching the serde `kebab-case` rename.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::McpHttp => "mcp-http",
            Self::CliQuery => "cli-query",
        }
    }

    /// `true` for the process-backed plugin.
    pub fn is_app(self) -> bool {
        matches!(self, Self::App)
    }
}

/// Where the API key rides on an outbound `mcp-http` request. `query:*` is refused unconditionally;
/// the rest of the set is enforced only when `api_key_secret` is set (a keyless connector sends no
/// credential anywhere). `Bearer` is not `header:Authorization`: upstreams reject the bare key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyIn {
    /// `Authorization: Bearer <credential>`.
    Bearer,
    /// `<name>: <credential>`, verbatim.
    Header(String),
}

/// What an operator must do when their manifest still says `query:<name>`.
pub const RETIRED_QUERY_HINT: &str = "`query:<name>` was retired (#1194): the credential must not ride in the \
     request URL, where transport errors and upstream echoes reproduce it. Use \
     `bearer` (sends `Authorization: Bearer <credential>`) or, for a server \
     that wants the bare credential under its own header name, \
     `header:<name>` (sends `<name>: <credential>` verbatim)";

impl ApiKeyIn {
    /// Parse the manifest's `api_key_in` string; `None` for anything outside the set, including the retired `query:` spelling.
    pub fn parse(s: &str) -> Option<Self> {
        if s == "bearer" {
            return Some(Self::Bearer);
        }
        let (scheme, name) = s.split_once(':')?;
        if name.trim().is_empty() {
            return None;
        }
        match scheme {
            "header" => Some(Self::Header(name.to_string())),
            _ => None,
        }
    }

    /// Does `s` name the retired query placement? Answered on the SCHEME, so `query:a=b` also gets the migration message.
    pub fn is_retired_query(s: &str) -> bool {
        s == "query" || s.starts_with("query:")
    }
}

/// Default per-request timeout for a steady-state `tools/call`. Deliberately NOT the bound that protects boot; may be raised without limit.
pub const MCP_HTTP_DEFAULT_TIMEOUT_MS: u64 = 10_000;

// Re-exported so parse-time validation names it here.
pub use calm_types::boot_budget::MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS;

/// `mcp_http` top-level block. Present iff `kind == "mcp-http"`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct McpHttpBlock {
    /// Absolute `http://` or `https://` endpoint.
    pub url: String,

    /// Name of the key in the connector's `secrets.json` holding the API key.
    /// Absent ⇒ unauthenticated requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret: Option<String>,

    /// `bearer` | `header:<name>`. Required when `api_key_secret` is set; `query:<name>` is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_in: Option<String>,

    /// Header name to secrets.json key. Values never live in the public manifest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub header_secrets: BTreeMap<String, String>,

    /// Discover and expose the upstream's complete catalog on every enable/reload. Explicit, so manifests written before this field keep exposing nothing.
    #[serde(default)]
    pub tools_all: bool,

    /// Strict allowlist of upstream tool names to expose; names the upstream does not serve are warned about and skipped.
    #[serde(default)]
    pub tools_allow: Vec<String>,

    /// Steady-state `tools/call` timeout. No upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,

    /// Bring-up (`initialize` + `tools/list`) timeout, capped at [`MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS`].
    /// Absent ⇒ `min(request_timeout_ms, ceiling)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bringup_timeout_ms: Option<u64>,
}

impl McpHttpBlock {
    /// The `tools/call` budget. Long by design; never used for bring-up.
    pub fn timeout_ms(&self) -> u64 {
        self.request_timeout_ms
            .filter(|ms| *ms > 0)
            .unwrap_or(MCP_HTTP_DEFAULT_TIMEOUT_MS)
    }

    /// The bring-up budget. The trailing `.min(...)` is what makes the bound total: the DERIVED default comes from an unbounded field.
    pub fn bringup_timeout_ms(&self) -> u64 {
        self.bringup_timeout_ms
            .filter(|ms| *ms > 0)
            .unwrap_or_else(|| self.timeout_ms())
            .min(MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS)
    }

    /// Parsed `api_key_in`, `None` when no key is configured.
    pub fn api_key_in_parsed(&self) -> Option<ApiKeyIn> {
        self.api_key_in.as_deref().and_then(ApiKeyIn::parse)
    }
}

pub const CLI_QUERY_DEFAULT_TIMEOUT_MS: u64 = 20_000;
pub const CLI_QUERY_DEFAULT_MAX_OUTPUT_BYTES: usize = 32_768;

/// Ceiling on the effective `cli_query.max_output_bytes`: one tool call's stdout, held whole in memory.
/// Clamped rather than refused at parse, so an existing manifest never silently vanishes at boot.
pub const CLI_QUERY_MAX_OUTPUT_BYTES_CEILING: usize = 8 * 1024 * 1024;

/// `cli_query` top-level block. Present iff `kind == "cli-query"`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CliQueryBlock {
    /// Bare name (resolved against the service PATH + `search_path_extra` at
    /// enable time) or an absolute path.
    pub command: String,

    /// Extra PATH entries used ONLY when resolving/executing this connector.
    #[serde(default)]
    pub search_path_extra: Vec<String>,

    /// Keys forwarded from the service environment. Default empty — the child
    /// gets `env_clear()` plus an explicit base set.
    #[serde(default)]
    pub env_allow: Vec<String>,

    /// Env keys whose values come from the connector's `secrets.json`.
    #[serde(default)]
    pub secret_env: Vec<String>,

    /// Env keys whose values come from the plugin's effective configuration. One entry is BOTH the child
    /// env key and the `config_schema` property it is valued from. Not subject to the `env_allow` credential
    /// denylist: the value is typed in by the operator for this one connector, so it escalates nothing.
    #[serde(default)]
    pub config_env: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<usize>,

    pub tools: Vec<CliQueryTool>,
}

impl CliQueryBlock {
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
            .filter(|ms| *ms > 0)
            .unwrap_or(CLI_QUERY_DEFAULT_TIMEOUT_MS)
    }

    /// The cap the runtime actually uses: defaulted when absent or zero, clamped to [`CLI_QUERY_MAX_OUTPUT_BYTES_CEILING`].
    pub fn max_output_bytes(&self) -> usize {
        let requested = self
            .max_output_bytes
            .filter(|n| *n > 0)
            .unwrap_or(CLI_QUERY_DEFAULT_MAX_OUTPUT_BYTES);
        if requested > CLI_QUERY_MAX_OUTPUT_BYTES_CEILING {
            tracing::warn!(
                requested,
                ceiling = CLI_QUERY_MAX_OUTPUT_BYTES_CEILING,
                "cli_query.max_output_bytes exceeds the ceiling and was clamped"
            );
            return CLI_QUERY_MAX_OUTPUT_BYTES_CEILING;
        }
        requested
    }
}

/// One hand-declared CLI tool. `args` is a fixed argv template: a `{{slot}}` element is replaced
/// wholesale by one argument — never re-split, but it can still be option-shaped (`--output=…`).
/// An author who wants positional-only values writes `"--"` into the template.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CliQueryTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    pub args: Vec<String>,
}

/// The `config.` namespace prefix inside a `{{…}}` argv slot.
pub const CONFIG_SLOT_PREFIX: &str = "config.";

/// What one `{{…}}` argv slot draws its value from — decided at parse time, never re-decided at
/// render time, so agent-supplied and operator-supplied values never share a namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgvSlot<'a> {
    /// `{{name}}` — valued from the agent's `tools/call` `arguments`, and only from there.
    Argument(&'a str),
    /// `{{config.key}}` — valued from the plugin's effective configuration, and only from there.
    Config(&'a str),
}

impl ArgvSlot<'_> {
    /// The bare name inside the slot, with the namespace prefix stripped.
    pub fn name(&self) -> &str {
        match self {
            Self::Argument(n) | Self::Config(n) => n,
        }
    }
}

/// If `s` is exactly `{{name}}` or `{{config.key}}`, classify it; partial occurrences do NOT match.
/// `{{config.}}` classifies as `Config("")` so the validator can refuse it by name.
pub fn argv_slot(s: &str) -> Option<ArgvSlot<'_>> {
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.is_empty() {
        return None;
    }
    Some(match inner.strip_prefix(CONFIG_SLOT_PREFIX) {
        Some(key) => ArgvSlot::Config(key),
        None => ArgvSlot::Argument(inner),
    })
}

/// Is `key` a legal POSIX-shaped environment variable name (`[A-Za-z_][A-Za-z0-9_]*`)?
pub fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Author {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// How to launch the plugin process. Kernel-injected env merges over this at spawn time.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Entrypoint {
    /// Relative to `install_path`.
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

    /// Closed set: `"card"` only.
    pub scope: String,

    #[serde(default)]
    pub default_size: Option<ViewSize>,

    /// Static-asset HTML rendered in the iframe. If absent, the HTTP layer proxies to the plugin process at `/views/<id>`.
    #[serde(default)]
    pub entry_html: Option<String>,

    /// MCP Apps `_meta.ui.csp` mirror, emitted under `_meta.ui` of the `resources/read` response. Absent → AppBridge's no-network default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csp: Option<CspBlock>,

    /// MCP Apps `_meta.ui.permissions` mirror. Only the `tools` slot is populated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<UiPermissions>,
}

/// `_meta.ui.csp` mirror — kept open-shape so unmodeled directives pass straight through to AppBridge.
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
    /// Unmodeled directives — forwarded verbatim.
    #[serde(flatten)]
    pub extras: HashMap<String, Vec<String>>,
}

/// `_meta.ui.permissions` mirror; only `tools` is modeled.
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
    /// Optional JSON Schema for the tool's MCP `inputSchema`. Absent ⇒ a permissive empty object schema, and a real agent then calls the tool with empty args.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// Optional MCP tool annotations (title/readOnlyHint/etc.) surfaced in tools/list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

/// Track-create handle that names a plugin-owned template id. Extra JSON keys are ignored.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TemplateDescriptor {
    pub id: String,
}

/// Permissions the plugin requests; enforced at the callback dispatch layer. Defaults grant nothing.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Permissions {
    /// Which `entity_kind` strings the plugin may overlay-write to. Empty = no overlay writes.
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

    /// Deprecated: the proposal channel was withdrawn; still parseable, intentionally ignored.
    #[serde(default)]
    pub proposals: Vec<String>,

    /// Per-plugin KV store cap in bytes. Slice C enforces; 0 = no KV access.
    #[serde(default)]
    pub kv_quota_bytes: u64,

    /// Future expansion (declared roots). Validated as a list of strings; no semantics.
    #[serde(default)]
    pub filesystem: Vec<String>,
}

impl Permissions {
    /// `true` when this block grants literally nothing. `proposals` is excluded on purpose: it is the withdrawn, ignored compatibility field.
    pub fn grants_nothing(&self) -> bool {
        self.overlays_write.is_empty()
            && !self.cards_create
            && !self.cards_read_all
            && self.events_subscribe.is_empty()
            && self.kv_quota_bytes == 0
            && self.filesystem.is_empty()
    }
}

/// Manifest parse / validation failure.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// JSON syntax error.
    #[error("manifest JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// Field-level rule violation. `field` is a dotted path (e.g. `views[0].scope`).
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

impl Manifest {
    /// Parse a manifest from a JSON string and run every validation rule.
    pub fn parse(s: &str) -> Result<Manifest, ManifestError> {
        if s.trim().is_empty() {
            return Err(ManifestError::invalid("<root>", "manifest is empty"));
        }
        let m: Manifest = serde_json::from_str(s)?;
        // Raw-text guard first: "rename it to `templates`" is the actionable message, not "wrong version".
        Self::reject_retired_workflows_key(s)?;
        m.validate()?;
        Ok(m)
    }

    /// Refuse a manifest that still spells [`Self::templates`] as `workflows`. Runs on the raw text
    /// because `Manifest` tolerates unknown keys, so the struct has already discarded the evidence.
    /// Not conditioned on `manifest_version`.
    fn reject_retired_workflows_key(s: &str) -> Result<(), ManifestError> {
        let Ok(Value::Object(raw)) = serde_json::from_str::<Value>(s) else {
            // `from_str::<Manifest>` above already succeeded, so this branch is unreachable in practice.
            return Ok(());
        };
        if raw.contains_key("workflows") {
            return Err(ManifestError::invalid(
                "workflows",
                "renamed to `templates` in #1268; rename the key (its entries are \
                 unchanged: `{ \"id\": \"<kernel template id>\" }`)",
            ));
        }
        Ok(())
    }

    /// Validate an already-deserialized manifest.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !(1..=3).contains(&self.manifest_version) {
            return Err(ManifestError::invalid(
                "manifest_version",
                format!(
                    "only manifest_version 1, 2 or 3 is accepted, got {}",
                    self.manifest_version
                ),
            ));
        }

        // A manifest that actually declares a binding MUST say 2: a `templates[]` file read by an older
        // kernel parses clean and silently binds nothing. Binding-less manifests are left alone — the boot
        // loader turns a parse failure into `warn!` + skip, so breaking them would be the same silent loss.
        if !self.templates.is_empty() && self.manifest_version < 2 {
            return Err(ManifestError::invalid(
                "manifest_version",
                format!(
                    "must be 2 to declare `templates` (#1268 renamed the array; \
                     a v1 kernel would ignore it and silently bind nothing), got {}",
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

        // `kernel` is a reserved writer identity: the callback path writes `ctx.plugin_id` verbatim, so a
        // plugin named `kernel` would forge kernel-authored overlay rows. The regex admits it, so refuse it here.
        if self.id == KERNEL_OVERLAY_PLUGIN_ID {
            return Err(ManifestError::invalid(
                "id",
                format!(
                    "`{KERNEL_OVERLAY_PLUGIN_ID}` is reserved for kernel-authored rows \
                     and cannot be claimed by a plugin",
                ),
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

        // Exactly one connector block may be present, and it must be the one `kind` names.
        self.validate_connector_blocks()?;
        // App-only surfaces are refused at PARSE time for connectors.
        self.reject_app_only_surfaces()?;

        // `entrypoint` is required only for `app`.
        match self.entrypoint.as_ref() {
            Some(entrypoint) => {
                if entrypoint.command.trim().is_empty() {
                    return Err(ManifestError::invalid(
                        "entrypoint.command",
                        "must be non-empty",
                    ));
                }
                // Reject absolute paths and `..` escapes early; spawn re-checks.
                if entrypoint.command.starts_with('/') || entrypoint.command.contains("..") {
                    return Err(ManifestError::invalid(
                        "entrypoint.command",
                        "must be a relative path inside the plugin install dir \
                         (no leading `/`, no `..` segments)",
                    ));
                }
            }
            None if self.kind.is_app() => {
                return Err(ManifestError::invalid(
                    "entrypoint",
                    "required for `kind: \"app\"` manifests",
                ));
            }
            None => {}
        }

        for (i, view) in self.views.iter().enumerate() {
            view.validate(i)?;
        }

        // Track `template_input` lives on the Manifest; error paths are `input_schema…`.
        if let Some(schema) = self.input_schema.as_ref() {
            crate::plugin_host::template_input::validate_input_schema(schema)
                .map_err(|e| ManifestError::invalid(e.path, e.reason))?;
        }

        // Same subset, different field, different error root: a `config_schema` violation must say `config_schema…`.
        if let Some(schema) = self.config_schema.as_ref() {
            crate::plugin_host::template_input::validate_object_schema(CONFIG_SCHEMA_KEY, schema)
                .map_err(|e| ManifestError::invalid(e.path, e.reason))?;

            // The conditional version bump: only a schema with a non-empty `required` loses something real when an older kernel ignores the key.
            let has_required = schema
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|r| !r.is_empty());
            if has_required && self.manifest_version < 3 {
                return Err(ManifestError::invalid(
                    "manifest_version",
                    format!(
                        "must be 3 to declare a `config_schema` with `required` \
                         (#1284: a pre-#1284 kernel ignores `config_schema`, so the \
                         plugin would run with none of its mandatory configuration \
                         and no error), got {}",
                        self.manifest_version
                    ),
                ));
            }
        }

        for (i, template) in self.templates.iter().enumerate() {
            template.validate(i)?;
        }

        self.permissions.validate()?;

        Ok(())
    }
}

impl Manifest {
    /// The two connector blocks are mutually exclusive, and the present block must match `kind`.
    /// Deliberately not `#[serde(flatten)]` + an internally-tagged enum: the duplicate `kind` key is a round-trip hazard.
    fn validate_connector_blocks(&self) -> Result<(), ManifestError> {
        if self.mcp_http.is_some() && self.cli_query.is_some() {
            return Err(ManifestError::invalid(
                "mcp_http",
                "`mcp_http` and `cli_query` are mutually exclusive",
            ));
        }
        match self.kind {
            ConnectorKind::App => {
                if self.mcp_http.is_some() {
                    return Err(ManifestError::invalid(
                        "mcp_http",
                        "only allowed when `kind` is \"mcp-http\"",
                    ));
                }
                if self.cli_query.is_some() {
                    return Err(ManifestError::invalid(
                        "cli_query",
                        "only allowed when `kind` is \"cli-query\"",
                    ));
                }
            }
            ConnectorKind::McpHttp => {
                let block = self.mcp_http.as_ref().ok_or_else(|| {
                    ManifestError::invalid("mcp_http", "required when `kind` is \"mcp-http\"")
                })?;
                // The url's `{{config.*}}` slots are only checkable against the manifest's own `config_schema`.
                block.validate(self.config_schema.as_ref())?;
            }
            ConnectorKind::CliQuery => {
                let block = self.cli_query.as_ref().ok_or_else(|| {
                    ManifestError::invalid("cli_query", "required when `kind` is \"cli-query\"")
                })?;
                // The block's configuration-facing rules are cross-field: only checkable against the manifest's own `config_schema`.
                block.validate(self.config_schema.as_ref())?;
            }
        }
        Ok(())
    }

    /// Parse-time refusal of every `app`-only surface on a connector manifest. `Manifest::parse` is
    /// the single door every manifest enters through, so downstream readers never see one.
    fn reject_app_only_surfaces(&self) -> Result<(), ManifestError> {
        if self.kind.is_app() {
            return Ok(());
        }
        let kind = self.kind.wire_name();
        let only_app = |what: &str| {
            format!("only allowed for `kind: \"app\"` manifests; `kind: \"{kind}\"` {what}")
        };

        if self.entrypoint.is_some() {
            return Err(ManifestError::invalid(
                "entrypoint",
                only_app("has no kernel-supervised child process"),
            ));
        }
        if !self.views.is_empty() {
            return Err(ManifestError::invalid(
                "views",
                only_app("cannot serve a `ui://` resource, so a view could never render"),
            ));
        }
        if !self.templates.is_empty() {
            return Err(ManifestError::invalid(
                "templates",
                only_app("cannot own a track template"),
            ));
        }
        if self.input_schema.is_some() {
            return Err(ManifestError::invalid(
                "input_schema",
                only_app("declares no template, so there is no `template_input` to shape"),
            ));
        }
        if !self.permissions.grants_nothing() {
            return Err(ManifestError::invalid(
                "permissions",
                only_app(
                    "has no `neige.*` callback channel, so no permission it requests \
                     could ever be exercised",
                ),
            ));
        }
        // A forge action is dispatched with the forge credential passthrough, an `app`-only channel; refusing at parse time makes that durable.
        if let Some(tool) = self
            .exposes_tools
            .iter()
            .find(|t| t.kind == Some(ToolKind::ForgeAction))
        {
            return Err(ManifestError::invalid(
                "exposes_tools",
                format!(
                    "tool `{}` declares `kind: \"forge-action\"`, which is {}",
                    tool.name,
                    only_app("cannot receive the forge credential passthrough"),
                ),
            ));
        }
        Ok(())
    }
}

impl McpHttpBlock {
    fn validate(&self, config_schema: Option<&Value>) -> Result<(), ManifestError> {
        let raw = self.url.trim();
        // No slots: the full validator runs. Slots + a key: the origin is locked to the literal, so each slot
        // is replaced by [`ORIGIN_PROBE`] and the same validator runs on the result. Slots and no key: the
        // whole url is replaceable, so only slot well-formedness is checked and [`resolve_mcp_http_url`] is the gate.
        let slots = url_config_slots(raw)
            .map_err(|reason| ManifestError::invalid("mcp_http.url", reason))?;
        if slots.is_empty() {
            validate_mcp_http_url(raw)?;
        } else {
            for (_, key) in &slots {
                if !config_schema_declares(config_schema, key) {
                    return Err(ManifestError::invalid(
                        "mcp_http.url",
                        format!(
                            "config slot `{key}` is not a top-level property of this \
                             manifest's `config_schema`; a url slot is filled from the \
                             operator's configuration, so an undeclared one could never \
                             receive a value"
                        ),
                    ));
                }
            }
            if self.api_key_secret.is_some() || !self.header_secrets.is_empty() {
                probe_literal_url(raw)?;
            }
        }
        // Enforced at PARSE time so an operator learns at install rather than by watching boot stall.
        if let Some(ms) = self.bringup_timeout_ms
            && ms > MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS
        {
            return Err(ManifestError::invalid(
                "mcp_http.bringup_timeout_ms",
                format!(
                    "must be at most {MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS} ms — bring-up is \
                     awaited inline during server boot. Raise `request_timeout_ms` \
                     instead if a long-running `tools/call` is what you need."
                ),
            ));
        }
        // Refused BEFORE the `(secret, in)` match so the retired spelling is refused unconditionally, even
        // with no secret named. The `(None, _)` arm below stays open on purpose: a keyless connector sends
        // no credential whatever its `api_key_in` says, so there is no hole to close.
        if let Some(api_key_in) = self.api_key_in.as_deref()
            && ApiKeyIn::is_retired_query(api_key_in)
        {
            return Err(ManifestError::invalid(
                "mcp_http.api_key_in",
                RETIRED_QUERY_HINT,
            ));
        }
        match (self.api_key_secret.as_deref(), self.api_key_in.as_deref()) {
            (Some(secret), _) if secret.trim().is_empty() => {
                return Err(ManifestError::invalid(
                    "mcp_http.api_key_secret",
                    "must be non-empty when present",
                ));
            }
            (Some(_), None) => {
                return Err(ManifestError::invalid(
                    "mcp_http.api_key_in",
                    "required whenever `api_key_secret` is set",
                ));
            }
            (Some(_), Some(api_key_in)) => match ApiKeyIn::parse(api_key_in) {
                None => {
                    return Err(ManifestError::invalid(
                        "mcp_http.api_key_in",
                        "must be `bearer` or `header:<name>`",
                    ));
                }
                // An illegal header name would otherwise be rejected by the HTTP client at REQUEST time, once per call.
                Some(ApiKeyIn::Header(name)) if !is_http_field_name(&name) => {
                    return Err(ManifestError::invalid(
                        "mcp_http.api_key_in",
                        format!(
                            "`{name}` is not a legal HTTP header name \
                             (RFC 9110 token: alphanumerics and any of `!#$%&'*+-.^_`|~`)"
                        ),
                    ));
                }
                Some(_) => {}
            },
            (None, _) => {}
        }
        super::http_headers::validate_header_names(self.header_secrets.keys().map(String::as_str))
            .map_err(|why| ManifestError::invalid("mcp_http.header_secrets", why))?;
        if self.header_secrets.values().any(|key| key.is_empty()) {
            return Err(ManifestError::invalid(
                "mcp_http.header_secrets",
                "secret references must be non-empty",
            ));
        }
        if let Some(auth) = self
            .api_key_in_parsed()
            .filter(|_| self.api_key_secret.is_some())
        {
            let auth_name = match auth {
                ApiKeyIn::Bearer => "authorization".to_string(),
                ApiKeyIn::Header(name) => name,
            };
            if self
                .header_secrets
                .keys()
                .any(|name| name.eq_ignore_ascii_case(&auth_name))
            {
                return Err(ManifestError::invalid(
                    "mcp_http.header_secrets",
                    "cannot override the API key header",
                ));
            }
        }
        if self.tools_all && !self.tools_allow.is_empty() {
            return Err(ManifestError::invalid(
                "mcp_http.tools_all",
                "cannot be true when `mcp_http.tools_allow` names tools; choose all tools or a strict allowlist",
            ));
        }
        for (i, name) in self.tools_allow.iter().enumerate() {
            validate_connector_tool_name(name, &format!("mcp_http.tools_allow[{i}]"))?;
        }
        Ok(())
    }
}

impl CliQueryBlock {
    fn validate(&self, config_schema: Option<&Value>) -> Result<(), ManifestError> {
        if self.command.trim().is_empty() {
            return Err(ManifestError::invalid(
                "cli_query.command",
                "must be non-empty",
            ));
        }
        if self.tools.is_empty() {
            return Err(ManifestError::invalid(
                "cli_query.tools",
                "must declare at least one tool",
            ));
        }
        // `env_allow` forwards values out of the SERVICE environment, so a forge credential key here would
        // hand an agent-callable connector the operator's git identity. The denylist is the CREDENTIAL subset
        // only; `secret_env` values come from the connector's own `secrets.json` and escalate nothing.
        for (i, key) in self.env_allow.iter().enumerate() {
            if crate::operation::forge_action_adapter::FORGE_CREDENTIAL_ENV_KEYS
                .contains(&key.as_str())
            {
                return Err(ManifestError::invalid(
                    format!("cli_query.env_allow[{i}]"),
                    format!(
                        "`{key}` is a forge CREDENTIAL and may never be forwarded to a \
                         cli-query connector: a query connector is authored in a manifest \
                         and callable by any agent that can see its tools, so it must not \
                         hold the operator's forge identity"
                    ),
                ));
            }
        }
        // A `config_env` entry names BOTH a child env key and the `config_schema` property it draws from, so it has to be legal as both.
        for (i, key) in self.config_env.iter().enumerate() {
            let path = format!("cli_query.config_env[{i}]");
            if !is_valid_env_key(key) {
                return Err(ManifestError::invalid(
                    path,
                    format!(
                        "`{key}` is not a legal environment variable name \
                         ([A-Za-z_][A-Za-z0-9_]*); a name no program can look up \
                         would be a silent no-op rather than a configuration"
                    ),
                ));
            }
            if !config_schema_declares(config_schema, key) {
                return Err(ManifestError::invalid(
                    path,
                    format!(
                        "`{key}` is not a top-level property of this manifest's \
                         `config_schema`; a `config_env` key names the configuration \
                         property its value comes from, so an undeclared one could \
                         never receive a value"
                    ),
                ));
            }
        }

        // Three sources write into ONE child environment; a duplicate target would make the winning value
        // depend on injection order. This is retroactive for the `env_allow` ∩ `secret_env` pair.
        let mut seen: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
        for (source, keys) in [
            ("cli_query.env_allow", &self.env_allow),
            ("cli_query.secret_env", &self.secret_env),
            ("cli_query.config_env", &self.config_env),
        ] {
            for (i, key) in keys.iter().enumerate() {
                if let Some(first) = seen.insert(key.as_str(), source) {
                    return Err(ManifestError::invalid(
                        format!("{source}[{i}]"),
                        format!(
                            "env key `{key}` is already declared by `{first}`; \
                             `env_allow`, `secret_env` and `config_env` all write the \
                             same child environment, so a duplicate target would make \
                             the winning value depend on injection order"
                        ),
                    ));
                }
            }
        }

        for (i, tool) in self.tools.iter().enumerate() {
            tool.validate(i, config_schema)?;
        }
        Ok(())
    }
}

/// Is `key` a top-level property of this manifest's `config_schema`? Absent schema ⇒ nothing is declared.
fn config_schema_declares(config_schema: Option<&Value>, key: &str) -> bool {
    config_schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|p| p.contains_key(key))
}

impl CliQueryTool {
    fn validate(&self, idx: usize, config_schema: Option<&Value>) -> Result<(), ManifestError> {
        let path = |s: &str| format!("cli_query.tools[{idx}].{s}");
        validate_connector_tool_name(&self.name, &path("name"))?;

        // Slot names must be declared top-level keys of `input_schema`.
        let properties = self
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object());

        // The namespace has to be reserved on BOTH sides: an input property literally named `config.x`
        // would let one `tools/call` supply the operator's configuration value. Retroactive by design.
        if let Some(props) = properties {
            for key in props.keys() {
                if key.starts_with(CONFIG_SLOT_PREFIX) {
                    return Err(ManifestError::invalid(
                        path(&format!("input_schema.properties.{key}")),
                        format!(
                            "`{CONFIG_SLOT_PREFIX}` is reserved for operator \
                             configuration slots (`{{{{config.<key>}}}}`) and may not \
                             prefix a tool input property: an agent-supplied argument \
                             must never be able to occupy a configuration name"
                        ),
                    ));
                }
            }
        }

        for (i, arg) in self.args.iter().enumerate() {
            let Some(slot) = argv_slot(arg) else {
                // A literal argv element. Reject stray braces so a typo like `--sym={{symbol}}` fails at authoring time.
                if arg.contains("{{") || arg.contains("}}") {
                    return Err(ManifestError::invalid(
                        path(&format!("args[{i}]")),
                        "a `{{slot}}` template must occupy the whole argv element \
                         (no string concatenation, no shell)",
                    ));
                }
                continue;
            };
            match slot {
                ArgvSlot::Argument(name) => {
                    if !properties.is_some_and(|p| p.contains_key(name)) {
                        return Err(ManifestError::invalid(
                            path(&format!("args[{i}]")),
                            format!(
                                "slot `{name}` is not a top-level property of this tool's \
                                 input_schema"
                            ),
                        ));
                    }
                }
                ArgvSlot::Config(key) => {
                    if key.is_empty() {
                        return Err(ManifestError::invalid(
                            path(&format!("args[{i}]")),
                            format!("`{{{{{CONFIG_SLOT_PREFIX}}}}}` names no configuration key"),
                        ));
                    }
                    if !config_schema_declares(config_schema, key) {
                        return Err(ManifestError::invalid(
                            path(&format!("args[{i}]")),
                            format!(
                                "config slot `{key}` is not a top-level property of this \
                                 manifest's `config_schema`; a configuration slot is filled \
                                 from the operator's configuration, never from the agent's \
                                 arguments, so an undeclared one could never receive a value"
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Real parse of `mcp_http.url`. A fragment is refused because it is never sent to a server, so an
/// endpoint written with one does not address what its author wrote.
fn validate_mcp_http_url(raw: &str) -> Result<(), ManifestError> {
    let field = "mcp_http.url";
    // WHATWG normalization turns `https:///mcp` into host `mcp`; require a non-empty authority in the RAW text first.
    match raw.split_once("://") {
        Some((_, rest)) if !rest.split(['/', '?', '#']).next().unwrap_or("").is_empty() => {}
        _ => {
            return Err(ManifestError::invalid(
                field,
                "must be `http://<host>[…]` or `https://<host>[…]` with a non-empty authority",
            ));
        }
    }
    // WHATWG treats a BACKSLASH as a path separator and STRIPS tabs and newlines, so `https://\evil.example/mcp`
    // would be retargeted while `log_target` (which splits the raw string) reports the host the author wrote.
    if let Some(bad) = raw
        .chars()
        .find(|c| *c == '\\' || c.is_ascii_control() || *c == '\u{7f}')
    {
        return Err(ManifestError::invalid(
            field,
            format!(
                "must not contain backslashes or ASCII control characters \
                 (found {bad:?}): WHATWG URL parsing treats them as authority/path \
                 delimiters or strips them, which retargets the request"
            ),
        ));
    }
    let parsed = url::Url::parse(raw)
        .map_err(|e| ManifestError::invalid(field, format!("not a valid absolute URL: {e}")))?;
    // Scheme FIRST: `FILE://x/mcp` is non-canonical AND unsupported, and the scheme is the useful error.
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ManifestError::invalid(
            field,
            format!(
                "scheme must be `http` or `https`, got `{}`",
                parsed.scheme()
            ),
        ));
    }
    // The manifest must be written in canonical form: ureq re-parses the string and `log_target` does not, and the two must agree.
    if parsed.as_str() != raw {
        return Err(ManifestError::invalid(
            field,
            format!(
                "must be written in canonical form; `{raw}` normalizes to `{}`. \
                 Use the normalized spelling so the URL we contact and the URL \
                 we log are provably the same string.",
                parsed.as_str()
            ),
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(ManifestError::invalid(field, "must carry a host"));
    }
    if parsed.fragment().is_some() {
        return Err(ManifestError::invalid(
            field,
            "must not carry a `#fragment`: a fragment is never sent to the \
             server, so the endpoint contacted is not the one written here",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ManifestError::invalid(
            field,
            "must not embed userinfo credentials; use `api_key_secret` + \
             `api_key_in` so the value lives in `secrets.json`",
        ));
    }
    Ok(())
}

/// The stand-in substituted for a slot when the url's manifest-literal origin is needed. A legal host
/// label AND a legal path/query byte, so the real parser decides where the slot sits.
const ORIGIN_PROBE: &str = "neige-config-slot";

/// The manifest-literal url of a templated `mcp_http.url`: every slot replaced by [`ORIGIN_PROBE`],
/// held to [`validate_mcp_http_url`], and parsed. Only sound for the keyed tier, where the origin is locked.
fn probe_literal_url(raw: &str) -> Result<url::Url, ManifestError> {
    let field = "mcp_http.url";
    let bad = |reason: String| ManifestError::invalid(field, reason);
    let probe = render_url_slots(raw, |_| Ok(ORIGIN_PROBE.to_string())).map_err(&bad)?;
    validate_mcp_http_url(&probe).map_err(|e| {
        let detail = match &e {
            ManifestError::Invalid { reason, .. } => reason.clone(),
            other => other.to_string(),
        };
        bad(format!(
            "with each configuration slot replaced by a stand-in this url is refused by \
             the manifest's own validator, and no configured value could repair it: \
             {detail}"
        ))
    })?;
    url::Url::parse(&probe).map_err(|e| bad(format!("not a valid absolute URL: {e}")))
}

/// A `mcp_http.url` rendered against a plugin's effective configuration, re-validated, and — when the
/// connector holds an API key — checked to still point at the manifest's own origin. [`resolve_mcp_http_url`] is the only constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMcpUrl(String);

impl ResolvedMcpUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Every `{{…}}` occurrence in a `mcp_http.url`, as `(byte range, key)`. Only `{{config.<key>}}` is
/// a slot: a url has no agent-supplied arguments, so the bare `{{name}}` form is refused by name.
fn url_config_slots(raw: &str) -> Result<Vec<(std::ops::Range<usize>, &str)>, String> {
    let stray = |what: &str| {
        format!(
            "stray `{what}` in `mcp_http.url`: a configuration slot is written `{{{{{CONFIG_SLOT_PREFIX}<key>}}}}`"
        )
    };
    let mut out: Vec<(std::ops::Range<usize>, &str)> = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = raw[i..].find("{{") {
        let start = i + rel;
        if raw[i..start].contains("}}") {
            return Err(stray("}}"));
        }
        let body = start + 2;
        let Some(erel) = raw[body..].find("}}") else {
            return Err("unterminated `{{` in `mcp_http.url`".to_string());
        };
        let inner = &raw[body..body + erel];
        if inner.contains("{{") {
            return Err(stray("{{"));
        }
        let key = inner
            .strip_prefix(CONFIG_SLOT_PREFIX)
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                format!(
                    "`{{{{{inner}}}}}` is not a configuration slot: an `mcp_http.url` \
                     placeholder must be written `{{{{{CONFIG_SLOT_PREFIX}<key>}}}}`. A url is \
                     rendered at bring-up, where no agent argument exists"
                )
            })?;
        out.push((start..body + erel + 2, key));
        i = body + erel + 2;
    }
    if raw[i..].contains("}}") {
        return Err(stray("}}"));
    }
    Ok(out)
}

/// Substitute every slot in `raw` with what `value_for` returns for its key.
fn render_url_slots(
    raw: &str,
    mut value_for: impl FnMut(&str) -> Result<String, String>,
) -> Result<String, String> {
    let slots = url_config_slots(raw)?;
    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    for (range, key) in slots {
        out.push_str(&raw[cursor..range.start]);
        out.push_str(&value_for(key)?);
        cursor = range.end;
    }
    out.push_str(&raw[cursor..]);
    Ok(out)
}

/// One effective-configuration value as it goes into a url. Not percent-encoded on purpose: the
/// canonical-form rule in [`validate_mcp_http_url`] refuses anything the parser would have to re-spell.
fn config_url_value(
    key: &str,
    effective: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    match effective.get(key) {
        None | Some(Value::Null) => Err(format!(
            "`mcp_http.url` has a `{{{{{CONFIG_SLOT_PREFIX}{key}}}}}` slot but no value is in \
             force for `{key}`. Set it under Settings › Plugins, then start the \
             connector again."
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        Some(Value::Bool(b)) => Ok(b.to_string()),
        Some(other) => Err(format!(
            "configuration key `{key}` holds {}, which has no rendering inside a url \
             (`config_schema` can only declare string, integer, number and boolean)",
            json_value_type_name(other)
        )),
    }
}

fn json_value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Render `mcp_http.url`'s configuration slots and decide whether the result may be contacted. With
/// `api_key_secret` the origin is locked to the manifest literal — a UI session must not be able to
/// redirect the credential; the zero-redirect agent in `HttpMcpClient::new` is the other half.
pub fn resolve_mcp_http_url(
    block: &McpHttpBlock,
    effective: &serde_json::Map<String, Value>,
) -> Result<ResolvedMcpUrl, String> {
    let raw = block.url.trim();
    let rendered = render_url_slots(raw, |key| config_url_value(key, effective))?;

    // THE validator, called — not restated.
    validate_mcp_http_url(&rendered).map_err(|e| e.to_string())?;

    if block.api_key_secret.is_some() || !block.header_secrets.is_empty() {
        lock_origin(raw, &rendered)?;
    }
    Ok(ResolvedMcpUrl(rendered))
}

/// The keyed tier's extra check: `rendered`'s origin must be the manifest's.
fn lock_origin(raw: &str, rendered: &str) -> Result<(), String> {
    let refusal = |detail: String| {
        format!(
            "`mcp_http.url` origin is locked because this connector holds an API key \
             (`mcp_http.api_key_secret`): {detail}. A configuration slot may fill the \
             path or the query, never the scheme, host or port — otherwise configuring \
             the plugin would be enough to send the credential somewhere else."
        )
    };

    // The literal origin: every slot replaced by a probe, held to the manifest's own url validator.
    let literal = probe_literal_url(raw).map_err(|e| {
        refusal(format!(
            "the manifest url does not survive its own validator once its slots are \
             accounted for ({e})"
        ))
    })?;
    // The probe may appear in the path or the query and nowhere else. Userinfo is a real position:
    // `https://user{{config.x}}@h.example/mcp` probes clean, then a value of `.evil.example/` moves the host.
    // The validator already refuses userinfo, so those arms cannot fire today; kept so a relaxation cannot reopen it.
    if literal.scheme().contains(ORIGIN_PROBE)
        || literal.host_str().is_some_and(|h| h.contains(ORIGIN_PROBE))
        || literal.username().contains(ORIGIN_PROBE)
        || literal.password().is_some_and(|p| p.contains(ORIGIN_PROBE))
    {
        return Err(refusal(
            "a slot sits inside the origin of the manifest url".to_string(),
        ));
    }

    // The rendered `(scheme, host, port)` compared with the manifest literal's, item by item. Kept with
    // the slot-in-origin refusal above: without it a host slot's "literal origin" would be the operator's own value.
    let got = url::Url::parse(rendered)
        .map_err(|e| refusal(format!("the rendered url does not parse ({e})")))?;
    let origin_of = |u: &url::Url| {
        format!(
            "{}://{}:{}",
            u.scheme(),
            u.host_str().unwrap_or(""),
            u.port_or_known_default()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string())
        )
    };
    if got.scheme() != literal.scheme()
        || got.host_str() != literal.host_str()
        || got.port_or_known_default() != literal.port_or_known_default()
    {
        return Err(refusal(format!(
            "the manifest pins `{}` but the configured value resolves to `{}`",
            origin_of(&literal),
            origin_of(&got)
        )));
    }
    Ok(())
}

/// RFC 9110 `field-name` = `token`.
fn is_http_field_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// Shared name check for connector-supplied tools; materialization and manifest parsing both route through it.
pub fn validate_connector_tool_name(name: &str, field: &str) -> Result<(), ManifestError> {
    if name.trim().is_empty() {
        return Err(ManifestError::invalid(field, "tool name must be non-empty"));
    }
    if name != name.trim() {
        return Err(ManifestError::invalid(
            field,
            "tool name must not have leading/trailing whitespace",
        ));
    }
    if name.contains(char::is_whitespace) {
        return Err(ManifestError::invalid(
            field,
            "tool name must not contain whitespace",
        ));
    }
    Ok(())
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
        // Scope enum is exactly `["card"]`; "track" and "area" get explicit errors.
        match self.scope.as_str() {
            "card" => {}
            "track" => {
                return Err(ManifestError::invalid(
                    path("scope"),
                    "track-scope views are deferred past M3 (design doc §10 #5); \
                     only \"card\" is accepted",
                ));
            }
            "area" => {
                return Err(ManifestError::invalid(
                    path("scope"),
                    "area-scope views are banned for M3 (design doc §10 #1); \
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

impl TemplateDescriptor {
    fn validate(&self, idx: usize) -> Result<(), ManifestError> {
        if !key_is_valid(&self.id) {
            return Err(ManifestError::invalid(
                format!("templates[{idx}].id"),
                "must match ^[a-z0-9][a-z0-9._-]{0,63}$",
            ));
        }
        Ok(())
    }
}

impl Permissions {
    fn validate(&self) -> Result<(), ManifestError> {
        // No other entity kinds exist in the kernel today.
        for (i, kind) in self.overlays_write.iter().enumerate() {
            if kind != "track" && kind != "card" {
                return Err(ManifestError::invalid(
                    format!("permissions.overlays_write[{i}]"),
                    format!(
                        "must be \"track\" or \"card\"; got `{kind}` \
                         (kernel knows no other entity kinds)"
                    ),
                ));
            }
        }
        // Globs are validated by the event bus, not here; only empty strings are rejected.
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

impl Manifest {
    /// Render the validated manifest back to a JSON `Value`.
    pub fn to_json(&self) -> Value {
        // Every field type is serde-derived from data that already round-tripped through `from_str`.
        serde_json::to_value(self).expect("Manifest serializable")
    }
}

impl fmt::Display for Manifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} v{} ({})", self.id, self.version, self.display_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ISSUE_DEVELOPMENT_RENDERED_PROMPT_GOLDEN: &str =
        include_str!("../../tests/goldens/issue_development_planner_prompt.txt");

    fn assert_full_golden_eq(expected: &str, actual: &str) {
        assert!(
            !expected.is_empty(),
            "full golden degenerate state: expected golden must not be empty"
        );
        assert!(
            !actual.is_empty(),
            "full golden degenerate state: rendered output must not be empty"
        );
        if expected == actual {
            return;
        }

        let first_difference = expected
            .bytes()
            .zip(actual.bytes())
            .position(|(expected, actual)| expected != actual)
            .unwrap_or_else(|| expected.len().min(actual.len()));
        let mut context_offset = first_difference;
        while !expected.is_char_boundary(context_offset) || !actual.is_char_boundary(context_offset)
        {
            context_offset -= 1;
        }

        fn line_context(text: &str, byte_offset: usize) -> String {
            let line_start = text[..byte_offset].rfind('\n').map_or(0, |index| index + 1);
            let line_end = text[byte_offset..]
                .find('\n')
                .map_or(text.len(), |index| byte_offset + index);
            let line_number = text[..line_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            let column = text[line_start..byte_offset].chars().count() + 1;
            format!(
                "line {line_number}, column {column}: {:?}",
                &text[line_start..line_end]
            )
        }

        panic!(
            "full golden mismatch at byte {first_difference} (expected {} bytes, actual {} bytes)\n\
             expected next {:?}; {}\n  actual next {:?}; {}",
            expected.len(),
            actual.len(),
            expected[context_offset..].chars().next(),
            line_context(expected, context_offset),
            actual[context_offset..].chars().next(),
            line_context(actual, context_offset)
        );
    }

    #[test]
    #[should_panic(expected = "full golden degenerate state")]
    fn full_golden_equality_rejects_empty_expected_and_actual() {
        assert_full_golden_eq("", "");
    }

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
                "overlays_write": ["track", "card"],
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
        assert!(!m.permissions.cards_create);
        assert!(m.permissions.overlays_write.is_empty());
    }

    fn template_manifest_value() -> Value {
        json!({
            "manifest_version": 2,
            "id": "dev.neige.template-test",
            "version": "1.0.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Template Test",
            "entrypoint": { "command": "bin/template-test" },
            "templates": [
                { "id": "issue-development" }
            ],
            "permissions": {}
        })
    }

    fn parse_manifest_value(v: Value) -> Result<Manifest, ManifestError> {
        Manifest::parse(&serde_json::to_string(&v).expect("serialize manifest value"))
    }

    #[test]
    fn parses_template_descriptor() {
        let m = parse_manifest_value(template_manifest_value()).expect("template manifest");
        assert_eq!(m.templates.len(), 1);
        assert_eq!(m.templates[0].id, "issue-development");
    }

    #[test]
    fn a_manifest_still_spelling_the_array_workflows_is_refused_by_name() {
        let mut v = template_manifest_value();
        let entries = v["templates"].take();
        v.as_object_mut()
            .expect("manifest fixture is an object")
            .remove("templates");
        v["workflows"] = entries;

        let err = parse_manifest_value(v).expect_err("the retired key must not parse silently");
        let ManifestError::Invalid { field, reason } = &err else {
            panic!("expected a field-level Invalid, got {err:?}");
        };
        assert_eq!(field, "workflows");
        assert!(
            reason.contains("templates"),
            "the refusal must name the new key, got {reason:?}"
        );
    }

    #[test]
    fn declaring_templates_at_version_1_is_refused_naming_the_version() {
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(1);
        let err =
            parse_manifest_value(v).expect_err("a v1 file must not be allowed to declare bindings");
        let ManifestError::Invalid { field, reason } = &err else {
            panic!("expected a field-level Invalid, got {err:?}");
        };
        assert_eq!(field, "manifest_version");
        assert!(
            reason.contains('2'),
            "the refusal must say which version to declare, got {reason:?}"
        );
    }

    #[test]
    fn a_binding_less_manifest_still_loads_at_version_1() {
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(1);
        v.as_object_mut()
            .expect("manifest fixture is an object")
            .remove("templates");
        let m = parse_manifest_value(v).expect("a v1 manifest without bindings is still valid");
        assert_eq!(m.manifest_version, 1);
        assert!(m.templates.is_empty());

        // An explicitly empty array has nothing to lose on rollback.
        let mut empty = template_manifest_value();
        empty["manifest_version"] = json!(1);
        empty["templates"] = json!([]);
        parse_manifest_value(empty).expect("an empty `templates` array declares no binding");
    }

    #[test]
    fn version_2_with_templates_parses() {
        let m = parse_manifest_value(template_manifest_value()).expect("v2 manifest");
        assert_eq!(m.manifest_version, 2);
        assert_eq!(m.templates[0].id, "issue-development");
    }

    #[test]
    fn the_shipped_git_forge_manifest_declares_version_2() {
        let m = Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
            .expect("shipped git-forge manifest");
        assert_eq!(m.manifest_version, 2);
        assert!(!m.templates.is_empty());
    }

    #[test]
    fn an_unrelated_unknown_top_level_key_still_parses() {
        let mut v = template_manifest_value();
        v["some_future_field"] = json!({ "anything": true });
        let m = parse_manifest_value(v).expect("unknown top-level keys stay forwards-compatible");
        assert_eq!(m.templates[0].id, "issue-development");
    }

    #[test]
    fn extra_template_descriptor_fields_are_ignored() {
        let mut v = template_manifest_value();
        v["templates"][0]["plan_template"] = json!([]);
        v["templates"][0]["gates"] = json!([]);
        v["templates"][0]["planner_instructions"] = json!("leftover");
        v["templates"][0]["card_kinds"] = json!(["terminal"]);
        v["templates"][0]["input_schema"] = json!({"type": "object"});
        let m = parse_manifest_value(v).expect("S5 ignores retired descriptor fields");
        assert_eq!(m.templates[0].id, "issue-development");
    }

    #[test]
    fn parses_shipped_issue_development_descriptor() {
        let m = Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
            .expect("shipped git-forge manifest");
        let template = m
            .templates
            .iter()
            .find(|template| template.id == "issue-development")
            .expect("issue-development template");
        assert_eq!(m.templates.len(), 1);
        assert_eq!(template.id, "issue-development");

        let schema = m
            .input_schema
            .as_ref()
            .expect("git-forge declares Manifest.input_schema");
        assert_eq!(schema["type"], "object");
        assert_eq!(
            schema["required"],
            serde_json::json!(["issue_url", "repo", "issue_number"])
        );
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        assert_eq!(schema["properties"]["issue_url"]["type"], "string");
        assert_eq!(schema["properties"]["repo"]["type"], "string");
        // The type must be the strict "integer".
        assert_eq!(schema["properties"]["issue_number"]["type"], "integer");
        assert_eq!(schema["properties"]["merge_policy"]["type"], "string");
        assert_eq!(
            schema["properties"]["merge_policy"]["enum"],
            serde_json::json!(["hold-for-ratify", "auto-merge"])
        );
        assert_eq!(
            schema["properties"]["merge_policy"]["default"],
            "hold-for-ratify"
        );
        assert_eq!(schema["properties"]["notes"]["type"], "string");
    }

    #[test]
    fn shipped_git_forge_give_up_uses_retained_lifecycle_tool() {
        Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
            .expect("shipped git-forge manifest");
        let descriptor = crate::mcp_server::build_default_registry()
            .descriptors()
            .into_iter()
            .find(|descriptor| descriptor.name == "calm.report.write")
            .expect("retained GIVE-UP tool descriptor");
        assert!(
            descriptor.input_schema["properties"]
                .get("lifecycle")
                .is_some(),
            "GIVE-UP tool must carry lifecycle: {}",
            descriptor.input_schema
        );

        let template = TemplateDescriptor {
            id: "issue-development".into(),
        };
        let rendered =
            crate::operation::planner_harness_start_adapter::render_planner_developer_instructions(
                "track-give-up",
                Some(&template),
                None,
            );
        crate::planner_card::validate_planner_prompt_contract(&rendered)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            !rendered.contains("If n == cap and the round is non-approving"),
            "S5 descriptor has no planner_instructions to inject"
        );
    }

    #[test]
    fn shipped_issue_development_rendered_prompt_matches_full_golden() {
        let manifest = Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
            .expect("shipped git-forge manifest");
        let template = manifest
            .templates
            .iter()
            .find(|template| template.id == "issue-development")
            .expect("issue-development template");

        // A legal final state for the shipped schema, with every required and optional field populated.
        let template_input = json!({
            "issue_url": "https://github.com/neige-calm/neige-calm/issues/985",
            "repo": "neige-calm/neige-calm",
            "issue_number": 985,
            "merge_policy": "auto-merge",
            "notes": "Full golden fixture covers every shipped template input field."
        });
        crate::plugin_host::template_input::validate_template_input(
            manifest
                .input_schema
                .as_ref()
                .expect("shipped git-forge Manifest.input_schema"),
            &template_input,
        )
        .expect("full golden template_input satisfies the shipped schema");
        let rendered =
            crate::operation::planner_harness_start_adapter::render_planner_developer_instructions(
                "track-golden-985",
                Some(template),
                Some(&template_input),
            );

        if std::env::var_os("REGEN_PLANNER_PROMPT_GOLDEN").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/goldens/issue_development_planner_prompt.txt");
            // Write back `rendered + "\n"`: the assertion side does
            // `strip_suffix('\n')`, so omitting it panics on the very next run.
            std::fs::write(&path, format!("{rendered}\n")).expect("write regenerated golden");
            panic!(
                "issue_development_planner_prompt.txt regenerated from the current prompt; \
                 hand-verify the diff, commit, and re-run without REGEN_PLANNER_PROMPT_GOLDEN"
            );
        }

        let expected = ISSUE_DEVELOPMENT_RENDERED_PROMPT_GOLDEN
            .strip_suffix('\n')
            .expect("text fixture has its repository newline");
        assert_full_golden_eq(expected, &rendered);
    }

    /// The last three rows: the descriptor alphabet has no `/`, so a plugin cannot claim `site/<stem>` or the reserved `plugin/…` key.
    #[test]
    fn template_descriptor_rejects_invalid_shapes() {
        let cases: Vec<(&str, Value, &str)> = vec![
            ("empty id", json!(""), "templates[0].id"),
            ("bad id", json!("Bad Id"), "templates[0].id"),
            ("site prefix", json!("site/x"), "templates[0].id"),
            ("plugin prefix", json!("plugin/x"), "templates[0].id"),
            ("any slash", json!("a/b"), "templates[0].id"),
        ];
        for (label, id, field) in cases {
            let mut v = template_manifest_value();
            v["templates"][0]["id"] = id;
            let err = parse_manifest_value(v).expect_err(label);
            assert!(
                matches!(err, ManifestError::Invalid { field: ref actual, .. } if actual == field),
                "{label}: got {err:?}"
            );
        }
    }

    fn subset_input_schema() -> Value {
        json!({
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
        })
    }

    #[test]
    fn manifest_accepts_subset_input_schema_and_defaults_to_none() {
        let manifest = parse_manifest_value(template_manifest_value()).expect("valid manifest");
        assert!(manifest.input_schema.is_none());

        let mut v = template_manifest_value();
        v["input_schema"] = subset_input_schema();
        let manifest = parse_manifest_value(v).expect("subset input_schema accepted");
        assert!(manifest.input_schema.is_some());
    }

    #[test]
    fn manifest_rejects_out_of_subset_input_schema() {
        let cases: [(&str, Value, &str); 5] = [
            (
                "hostile $ref keyword",
                json!({
                    "type": "object",
                    "$ref": "#/defs/x",
                    "additionalProperties": false
                }),
                "input_schema.$ref",
            ),
            (
                "hostile property keyword (pattern)",
                json!({
                    "type": "object",
                    "properties": { "u": { "type": "string", "pattern": ".*" } },
                    "additionalProperties": false
                }),
                "input_schema.properties.u.pattern",
            ),
            (
                "missing additionalProperties: false",
                json!({ "type": "object", "properties": {} }),
                "input_schema.additionalProperties",
            ),
            (
                "required key not declared",
                json!({
                    "type": "object",
                    "properties": {},
                    "required": ["ghost"],
                    "additionalProperties": false
                }),
                "input_schema.required[0]",
            ),
            (
                "enum riding a non-string type",
                json!({
                    "type": "object",
                    "properties": { "n": { "type": "integer", "enum": [1] } },
                    "additionalProperties": false
                }),
                "input_schema.properties.n.enum",
            ),
        ];
        for (label, schema, expected_field) in cases {
            let mut v = template_manifest_value();
            v["input_schema"] = schema;
            let err = parse_manifest_value(v).expect_err(label);
            assert!(
                matches!(&err, ManifestError::Invalid { field, .. } if field == expected_field),
                "{label}: got {err:?}"
            );
        }
    }

    /// All-optional config schema: legal at `manifest_version: 2`.
    fn optional_config_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "theme": {
                    "type": "string",
                    "enum": ["dark", "light"],
                    "default": "dark",
                    "description": "Card chrome"
                },
                "retries": { "type": "integer", "default": 3 }
            },
            "additionalProperties": false
        })
    }

    #[test]
    fn manifest_accepts_subset_config_schema_and_defaults_to_none() {
        let manifest = parse_manifest_value(template_manifest_value()).expect("valid manifest");
        assert!(manifest.config_schema.is_none(), "absent ⇒ None");

        let mut v = template_manifest_value();
        v["config_schema"] = optional_config_schema();
        let manifest = parse_manifest_value(v).expect("subset config_schema accepted");
        assert_eq!(
            manifest.config_schema.as_ref(),
            Some(&optional_config_schema())
        );
    }

    #[test]
    fn manifest_rejects_out_of_subset_config_schema_under_its_own_root() {
        let cases: [(&str, Value, &str); 6] = [
            (
                "hostile $ref keyword",
                json!({
                    "type": "object",
                    "$ref": "#/defs/x",
                    "additionalProperties": false
                }),
                "config_schema.$ref",
            ),
            (
                "hostile property keyword (pattern)",
                json!({
                    "type": "object",
                    "properties": { "u": { "type": "string", "pattern": ".*" } },
                    "additionalProperties": false
                }),
                "config_schema.properties.u.pattern",
            ),
            (
                "missing additionalProperties: false",
                json!({ "type": "object", "properties": {} }),
                "config_schema.additionalProperties",
            ),
            (
                "required key not declared",
                json!({
                    "type": "object",
                    "properties": {},
                    "required": ["ghost"],
                    "additionalProperties": false
                }),
                "config_schema.required[0]",
            ),
            (
                "enum riding a non-string type",
                json!({
                    "type": "object",
                    "properties": { "n": { "type": "integer", "enum": [1] } },
                    "additionalProperties": false
                }),
                "config_schema.properties.n.enum",
            ),
            (
                "default outside its own enum",
                json!({
                    "type": "object",
                    "properties": {
                        "theme": { "type": "string", "enum": ["dark"], "default": "neon" }
                    },
                    "additionalProperties": false
                }),
                "config_schema.properties.theme.default",
            ),
        ];
        for (label, schema, expected_field) in cases {
            let mut v = template_manifest_value();
            v["manifest_version"] = json!(3);
            v["config_schema"] = schema;
            let err = parse_manifest_value(v).expect_err(label);
            assert!(
                matches!(&err, ManifestError::Invalid { field, .. } if field == expected_field),
                "{label}: got {err:?}"
            );
        }
    }

    #[test]
    fn config_schema_with_required_demands_manifest_version_3() {
        let mut required_schema = optional_config_schema();
        required_schema["required"] = json!(["theme"]);

        // (a) required + version 2 ⇒ rejected, naming the VERSION.
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(2);
        v["config_schema"] = required_schema.clone();
        let err = parse_manifest_value(v).expect_err("required config at v2 must be refused");
        match &err {
            ManifestError::Invalid { field, reason } => {
                assert_eq!(field, "manifest_version");
                assert!(reason.contains("config_schema"), "got {reason}");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // (b) required + version 3 ⇒ accepted.
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(3);
        v["config_schema"] = required_schema;
        let m = parse_manifest_value(v).expect("required config at v3 is accepted");
        assert_eq!(m.manifest_version, 3);

        // (c) all-optional + version 2 ⇒ accepted.
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(2);
        v["config_schema"] = optional_config_schema();
        let m = parse_manifest_value(v).expect("optional-only config at v2 is accepted");
        assert_eq!(m.manifest_version, 2);
    }

    /// An empty `required: []` is not a required key.
    #[test]
    fn an_empty_required_array_does_not_demand_version_3() {
        let mut schema = optional_config_schema();
        schema["required"] = json!([]);
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(2);
        v["config_schema"] = schema;
        parse_manifest_value(v).expect("`required: []` has nothing to lose on an old kernel");
    }

    #[test]
    fn config_schema_key_matches_the_serialized_manifest() {
        let mut v = template_manifest_value();
        v["config_schema"] = optional_config_schema();
        let blob = parse_manifest_value(v).expect("valid").to_json();
        assert_eq!(
            blob.get(CONFIG_SCHEMA_KEY),
            Some(&optional_config_schema()),
            "blob keys: {:?}",
            blob.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );

        // Absence really is absence (skip_serializing_if).
        let blob = parse_manifest_value(template_manifest_value())
            .expect("valid")
            .to_json();
        assert!(blob.get(CONFIG_SCHEMA_KEY).is_none());
    }

    #[test]
    fn missing_required_field_fails() {
        // `display_name` missing: serde rejects it before any validator runs.
        let json = r#"{
            "manifest_version": 1,
            "id": "a.b",
            "version": "1.0.0",
            "min_kernel_version": "0.1.0",
            "entrypoint": { "command": "bin/run" }
        }"#;
        let err = Manifest::parse(json).expect_err("missing display_name");
        assert!(matches!(err, ManifestError::Json(_)), "got {err:?}");
    }

    #[test]
    fn missing_entrypoint_is_a_validation_error_for_app_manifests() {
        let json = r#"{
            "manifest_version": 1,
            "id": "a.b",
            "version": "1.0.0",
            "min_kernel_version": "0.1.0",
            "display_name": "X"
        }"#;
        let err = Manifest::parse(json).expect_err("missing entrypoint");
        match &err {
            ManifestError::Invalid { field, .. } => assert_eq!(field, "entrypoint"),
            other => panic!("expected an entrypoint validation error, got {other:?}"),
        }
    }

    #[test]
    fn empty_string_fails() {
        let err = Manifest::parse("").unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { .. }));
    }

    #[test]
    fn bad_manifest_version_fails() {
        // Probed on both sides of the accepted range: a single sample above it would stay green under `>= 1`.
        for version in ["0", "4", "99"] {
            let json = format!(
                r#"{{
            "manifest_version": {version},
            "id": "a.b",
            "version": "1.0.0",
            "min_kernel_version": "0.1.0",
            "display_name": "X",
            "entrypoint": {{ "command": "bin/x" }}
        }}"#
            );
            let err = Manifest::parse(&json).unwrap_err();
            match err {
                ManifestError::Invalid { field, .. } => {
                    assert_eq!(field, "manifest_version", "version {version}")
                }
                other => panic!("version {version}: wrong variant: {other:?}"),
            }
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
    fn reserved_kernel_id_rejected() {
        let json = hello_world().replace("dev.neige.hello-world", KERNEL_OVERLAY_PLUGIN_ID);
        let err = Manifest::parse(&json).unwrap_err();
        match err {
            ManifestError::Invalid { field, reason } => {
                assert_eq!(field, "id");
                assert!(reason.contains("reserved"), "reason={reason}");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// The neighbouring id is fine — the refusal is exact, not a prefix ban.
    #[test]
    fn kernel_prefixed_id_still_allowed() {
        let json = hello_world().replace("dev.neige.hello-world", "kernel-helper");
        Manifest::parse(&json).expect("`kernel-helper` is not the reserved id");
    }

    #[test]
    fn scope_track_rejected() {
        let json = hello_world().replace("\"scope\": \"card\"", "\"scope\": \"track\"");
        let err = Manifest::parse(&json).unwrap_err();
        match err {
            ManifestError::Invalid { field, reason } => {
                assert_eq!(field, "views[0].scope");
                assert!(reason.contains("track"), "reason: {reason}");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn scope_area_rejected() {
        let json = hello_world().replace("\"scope\": \"card\"", "\"scope\": \"area\"");
        let err = Manifest::parse(&json).unwrap_err();
        match err {
            ManifestError::Invalid { field, reason } => {
                assert_eq!(field, "views[0].scope");
                assert!(reason.contains("area"), "reason: {reason}");
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
        let json = hello_world().replace("[\"track\", \"card\"]", "[\"track\", \"area\"]");
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

    #[test]
    fn view_without_csp_or_permissions_round_trips_as_none() {
        // hello_world() declares no CSP / permissions.
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

#[cfg(test)]
mod connector_kind_tests {
    use super::*;
    use serde_json::json;

    fn base(extra: Value) -> String {
        let mut m = json!({
            "manifest_version": 2,
            "id": "conn-x",
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Conn",
        });
        let obj = m.as_object_mut().unwrap();
        for (k, v) in extra.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        m.to_string()
    }

    fn mcp_http_block() -> Value {
        json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "WISBURG_API_KEY",
            "api_key_in": "bearer",
            "tools_allow": ["list_reports"],
            "request_timeout_ms": 10000,
        })
    }

    /// `expect_err` with the case identity in the panic message, so a table row that silently passed is named.
    fn expect_reject(res: Result<Manifest, ManifestError>, ctx: &str) -> ManifestError {
        match res {
            Ok(_) => panic!("`{ctx}` must be rejected, but it parsed"),
            Err(e) => e,
        }
    }

    fn cli_query_block() -> Value {
        json!({
            "command": "longbridge",
            "tools": [{
                "name": "quote",
                "description": "Get a quote",
                "input_schema": {
                    "type": "object",
                    "properties": { "symbol": { "type": "string" } },
                    "required": ["symbol"],
                    "additionalProperties": false
                },
                "args": ["quote", "{{symbol}}"]
            }]
        })
    }

    /// `config_schema` is deliberately NOT on the app-only list; paired with the `input_schema` half, which is.
    #[test]
    fn connectors_may_declare_config_schema_but_still_not_input_schema() {
        let schema = json!({
            "type": "object",
            "properties": { "endpoint": { "type": "string", "default": "https://a.example" } },
            "additionalProperties": false
        });

        Manifest::parse(&base(json!({
            "kind": "cli-query",
            "cli_query": cli_query_block(),
            "config_schema": schema,
        })))
        .expect("a connector may declare config_schema");

        Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": mcp_http_block(),
            "config_schema": schema,
        })))
        .expect("an mcp-http connector may declare config_schema");

        let err = expect_reject(
            Manifest::parse(&base(json!({
                "kind": "cli-query",
                "cli_query": cli_query_block(),
                "input_schema": schema,
            }))),
            "input_schema on a connector",
        );
        assert!(
            matches!(&err, ManifestError::Invalid { field, .. } if field == "input_schema"),
            "got {err:?}"
        );
    }

    #[test]
    fn absent_kind_defaults_to_app() {
        let m = Manifest::parse(&base(json!({ "entrypoint": { "command": "bin/run" } }))).unwrap();
        assert_eq!(m.kind, ConnectorKind::App);
        assert!(m.mcp_http.is_none());
        assert!(m.cli_query.is_none());
    }

    #[test]
    fn shipped_git_forge_manifest_still_parses_as_app() {
        let text = include_str!("../../../../plugins/git-forge/manifest.json");
        let m = Manifest::parse(text).expect("shipped manifest must keep parsing");
        assert_eq!(m.kind, ConnectorKind::App);
        assert!(m.entrypoint.is_some());
    }

    #[test]
    fn unknown_kind_is_a_parse_error_not_a_silent_app() {
        let err = Manifest::parse(&base(json!({
            "kind": "sql-query",
            "entrypoint": { "command": "bin/run" }
        })))
        .expect_err("unknown kind must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("sql-query"),
            "error must name the offending value: {msg}"
        );
        assert!(matches!(err, ManifestError::Json(_)), "got {err:?}");
    }

    #[test]
    fn kind_round_trips_through_to_json() {
        let m = Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": mcp_http_block(),
        })))
        .unwrap();
        let re: Manifest = serde_json::from_value(m.to_json()).expect("re-parse");
        assert_eq!(re.kind, ConnectorKind::McpHttp);
        assert_eq!(
            re.mcp_http.as_ref().unwrap().url,
            "https://mcp.example.com/mcp"
        );
        // Exactly one `kind` key on the wire.
        assert_eq!(
            m.to_json()
                .as_object()
                .unwrap()
                .keys()
                .filter(|k| k.as_str() == "kind")
                .count(),
            1
        );
    }

    #[test]
    fn app_without_entrypoint_is_rejected() {
        let err = Manifest::parse(&base(json!({}))).expect_err("app needs an entrypoint");
        assert!(err.to_string().contains("entrypoint"), "{err}");
    }

    #[test]
    fn connectors_do_not_need_an_entrypoint() {
        Manifest::parse(&base(
            json!({ "kind": "mcp-http", "mcp_http": mcp_http_block() }),
        ))
        .expect("mcp-http needs no entrypoint");
        Manifest::parse(&base(
            json!({ "kind": "cli-query", "cli_query": cli_query_block() }),
        ))
        .expect("cli-query needs no entrypoint");
    }

    #[test]
    fn kind_and_block_must_agree() {
        for (kind, block_key, block) in [
            ("mcp-http", "cli_query", cli_query_block()),
            ("cli-query", "mcp_http", mcp_http_block()),
        ] {
            let err = Manifest::parse(&base(json!({ "kind": kind, block_key: block })))
                .expect_err("mismatched block must be rejected");
            assert!(err.to_string().contains("required when"), "{err}");
        }
    }

    #[test]
    fn both_blocks_present_is_rejected() {
        let err = Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": mcp_http_block(),
            "cli_query": cli_query_block(),
        })))
        .expect_err("blocks are mutually exclusive");
        assert!(err.to_string().contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn app_may_not_carry_a_connector_block() {
        for (key, block) in [
            ("mcp_http", mcp_http_block()),
            ("cli_query", cli_query_block()),
        ] {
            let err = Manifest::parse(&base(json!({
                "entrypoint": { "command": "bin/run" },
                key: block,
            })))
            .expect_err("app must not carry a connector block");
            assert!(err.to_string().contains("only allowed when"), "{err}");
        }
    }

    /// One case per field, and the error must NAME the field.
    #[test]
    fn connector_app_only_surface_errors_name_the_field() {
        let cases: Vec<(&str, Value)> = vec![
            ("entrypoint", json!({ "command": "bin/run" })),
            (
                "views",
                json!([{ "view_id": "main", "title": "Main", "scope": "card" }]),
            ),
            ("templates", json!([{ "id": "wf.build" }])),
            (
                "input_schema",
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ),
            ("permissions", json!({ "cards_create": true })),
            ("permissions", json!({ "kv_quota_bytes": 1 })),
            ("permissions", json!({ "events_subscribe": ["*"] })),
            ("permissions", json!({ "overlays_write": ["card"] })),
            ("permissions", json!({ "cards_read_all": true })),
            ("permissions", json!({ "filesystem": ["/tmp"] })),
        ];
        for (kind, block_key, block) in [
            ("mcp-http", "mcp_http", mcp_http_block()),
            ("cli-query", "cli_query", cli_query_block()),
        ] {
            for (field, value) in &cases {
                let mut manifest = json!({ "kind": kind, block_key: block.clone() });
                manifest
                    .as_object_mut()
                    .unwrap()
                    .insert((*field).to_string(), value.clone());
                let err = expect_reject(
                    Manifest::parse(&base(manifest)),
                    &format!("{kind}/{field}={value}"),
                );
                let msg = err.to_string();
                assert!(
                    msg.contains(field),
                    "{kind}: error must name `{field}`: {msg}"
                );
                assert!(
                    msg.contains(kind),
                    "{kind}: error must name the kind: {msg}"
                );
            }
        }
    }

    /// Otherwise the test above would pass for the wrong reason.
    #[test]
    fn a_connector_without_app_only_surfaces_parses() {
        for (kind, block_key, block) in [
            ("mcp-http", "mcp_http", mcp_http_block()),
            ("cli-query", "cli_query", cli_query_block()),
        ] {
            Manifest::parse(&base(json!({ "kind": kind, block_key: block })))
                .unwrap_or_else(|e| panic!("{kind} must parse: {e}"));
        }
        // An explicitly-present but all-default `permissions` block requests nothing.
        Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": mcp_http_block(),
            "permissions": { "proposals": ["legacy"] },
        })))
        .expect("an all-default permissions block grants nothing");
    }

    #[test]
    fn app_manifests_keep_every_surface() {
        Manifest::parse(&base(json!({
            "entrypoint": { "command": "bin/run" },
            "views": [{ "view_id": "main", "title": "Main", "scope": "card" }],
            "templates": [{ "id": "wf.build" }],
            "input_schema": { "type": "object", "properties": {}, "additionalProperties": false },
            "permissions": { "cards_create": true, "kv_quota_bytes": 4096 },
        })))
        .expect("an app manifest may declare all of these");
    }

    #[test]
    fn all_tools_is_explicit_and_legacy_empty_allowlists_stay_empty() {
        for explicit_empty in [false, true] {
            let mut block = mcp_http_block();
            let object = block.as_object_mut().unwrap();
            if explicit_empty {
                object.insert("tools_allow".into(), json!([]));
            } else {
                object.remove("tools_allow");
            }
            let manifest = Manifest::parse(&base(json!({
                "kind": "mcp-http",
                "mcp_http": block,
            })))
            .expect("legacy connector still parses");
            let parsed = manifest.mcp_http.unwrap();
            assert!(!parsed.tools_all, "absence must never be promoted to all");
            assert!(parsed.tools_allow.is_empty());
        }

        let mut block = mcp_http_block();
        block.as_object_mut().unwrap().remove("tools_allow");
        block["tools_all"] = json!(true);
        let manifest = Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": block,
        })))
        .expect("explicit all-tools connector parses");
        assert!(manifest.mcp_http.unwrap().tools_all);
    }

    #[test]
    fn a_manifest_cannot_combine_all_tools_with_a_named_allowlist() {
        let mut block = mcp_http_block();
        block["tools_all"] = json!(true);
        let error = Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": block,
        })))
        .expect_err("two authority modes must be refused");
        assert!(error.to_string().contains("tools_all"), "{error}");
    }

    #[test]
    fn mcp_http_url_must_be_absolute_http() {
        let mut block = mcp_http_block();
        block["url"] = json!("mcp.example.com/mcp");
        let err = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block })))
            .expect_err("relative url rejected");
        assert!(err.to_string().contains("mcp_http.url"), "{err}");
    }

    #[test]
    fn mcp_http_url_is_really_parsed() {
        for bad in [
            "https://",
            "http://",
            "https:///mcp",
            "https://mcp.example.com/mcp#frag",
            "https://user:pw@mcp.example.com/mcp",
            "ftp://mcp.example.com/mcp",
            "https://exa mple.com/mcp",
        ] {
            let mut block = mcp_http_block();
            block["url"] = json!(bad);
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                bad,
            );
            assert!(err.to_string().contains("mcp_http.url"), "`{bad}`: {err}");
        }
        for good in [
            "https://mcp.example.com/mcp",
            "http://127.0.0.1:8931/mcp",
            "https://mcp.example.com/mcp?v=1",
        ] {
            let mut block = mcp_http_block();
            block["url"] = json!(good);
            Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block })))
                .unwrap_or_else(|e| panic!("`{good}` must be accepted: {e}"));
        }
    }

    /// WHATWG's other authority terminators parse to a host the author did not write, while `log_target` reports the host they did.
    #[test]
    fn mcp_http_url_rejects_whatwg_retargeting() {
        // Each entry is `(raw, host the parser actually resolves it to)`.
        for (raw, retargeted_host) in [
            (r"https://\evil.example/mcp", "evil.example"),
            (r"https:/\evil.example/mcp", "evil.example"),
            // Tab/CR/LF are STRIPPED, not treated as delimiters: the two labels fuse into one host.
            (
                "https://good.example\t.evil.example/mcp",
                "good.example.evil.example",
            ),
            (
                "https://good.example\n.evil.example/mcp",
                "good.example.evil.example",
            ),
            (
                "https://good.example\r.evil.example/mcp",
                "good.example.evil.example",
            ),
        ] {
            // The premise: `url` really does resolve this somewhere other than the literal authority.
            if let Ok(parsed) = url::Url::parse(raw) {
                assert_eq!(
                    parsed.host_str(),
                    Some(retargeted_host),
                    "fixture assumption broken for {raw:?}"
                );
            }
            let mut block = mcp_http_block();
            block["url"] = json!(raw);
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                raw,
            );
            assert!(err.to_string().contains("mcp_http.url"), "{raw:?}: {err}");
        }
    }

    /// A `config_schema` declaring the keys the url fixtures below fill.
    fn url_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "rev": { "type": "string" },
                "endpoint": { "type": "string" },
                "port": { "type": "string" },
                "count": { "type": "integer" }
            },
            "additionalProperties": false
        })
    }

    /// Parse an `mcp-http` manifest whose url is `url`, with [`url_schema`]
    /// declared and the API key present or absent as asked.
    fn http_with_url(url: &str, keyed: bool) -> Result<Manifest, ManifestError> {
        let mut block = mcp_http_block();
        block["url"] = json!(url);
        if !keyed {
            block.as_object_mut().unwrap().remove("api_key_secret");
            block.as_object_mut().unwrap().remove("api_key_in");
        }
        Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": block,
            "config_schema": url_schema(),
        })))
    }

    fn resolve(url: &str, keyed: bool, config: Value) -> Result<ResolvedMcpUrl, String> {
        let m = http_with_url(url, keyed).expect("fixture manifest must parse");
        let effective = effective_config_for_test(&m, &config);
        resolve_mcp_http_url(m.mcp_http.as_ref().unwrap(), &effective)
    }

    /// Like [`resolve`], but a manifest that does not parse is a REFUSAL rather than a broken fixture:
    /// which placement refuses is not the property.
    fn refuse_or_resolve(url: &str, keyed: bool, config: Value) -> Result<ResolvedMcpUrl, String> {
        match http_with_url(url, keyed) {
            Err(e) => Err(e.to_string()),
            Ok(m) => {
                let effective = effective_config_for_test(&m, &config);
                resolve_mcp_http_url(m.mcp_http.as_ref().unwrap(), &effective)
            }
        }
    }

    /// Go through the REAL merge, so these fixtures cannot disagree with what a
    /// spawn would actually see.
    fn effective_config_for_test(m: &Manifest, user: &Value) -> serde_json::Map<String, Value> {
        crate::plugin_host::config::effective_config(m, user)
    }

    #[test]
    fn a_url_slot_fills_the_path_and_the_query() {
        let resolved = resolve(
            "https://mcp.example.com/{{config.path}}?rev={{config.rev}}",
            true,
            json!({ "path": "v2/mcp", "rev": "7" }),
        )
        .expect("path and query slots are what the keyed tier allows");
        assert_eq!(resolved.as_str(), "https://mcp.example.com/v2/mcp?rev=7");
    }

    /// A non-string scalar is still one value; a container is not.
    #[test]
    fn a_url_slot_renders_scalars_and_refuses_containers() {
        let resolved = resolve(
            "https://mcp.example.com/mcp?n={{config.count}}",
            true,
            json!({ "count": 12 }),
        )
        .expect("an integer has a rendering");
        assert_eq!(resolved.as_str(), "https://mcp.example.com/mcp?n=12");

        let err = resolve(
            "https://mcp.example.com/mcp?n={{config.count}}",
            true,
            json!({ "count": [1, 2] }),
        )
        .expect_err("an array has no rendering inside a url");
        assert!(err.contains("count"), "{err}");
    }

    /// With an API key in play, a slot that could move the origin is refused — the class `validate_mcp_http_url` does NOT catch.
    #[test]
    fn a_keyed_connector_refuses_a_slot_anywhere_in_the_origin() {
        let moves_origin = json!({ "endpoint": "evil.example", "port": "8443" });
        for (url, config) in [
            ("https://{{config.endpoint}}/mcp", moves_origin.clone()),
            ("https://mcp.{{config.endpoint}}/mcp", moves_origin.clone()),
            (
                "{{config.endpoint}}://mcp.example.com/mcp",
                moves_origin.clone(),
            ),
            (
                "https://mcp.example.com:{{config.port}}/mcp",
                moves_origin.clone(),
            ),
            // No path at all: the slot abuts the authority.
            (
                "https://mcp.example.com{{config.endpoint}}",
                moves_origin.clone(),
            ),
            // Userinfo: the probe lands in the USERNAME, and these values then end the authority early, moving the host.
            (
                "https://user{{config.endpoint}}@h.example/mcp",
                json!({ "endpoint": ".evil.example/" }),
            ),
            (
                "https://user{{config.endpoint}}@h.example/mcp",
                json!({ "endpoint": "/" }),
            ),
            // The password half of the same position.
            (
                "https://user:pw{{config.endpoint}}@h.example/mcp",
                json!({ "endpoint": ".evil.example/" }),
            ),
        ] {
            match refuse_or_resolve(url, true, config) {
                // Any of the refusals may be the one that fires: the template-level validator names the field, the origin lock names the rule.
                Err(err) => assert!(
                    err.contains("origin is locked") || err.contains("mcp_http.url"),
                    "{url}: {err}"
                ),
                Ok(resolved) => panic!(
                    "{url} resolved to {} — a keyed connector's origin is not configurable",
                    resolved.as_str()
                ),
            }
        }
    }

    /// The parse-time placement of the keyed template check. The unkeyed half of each pair shows this is the tier talking, not a new global rule.
    #[test]
    fn a_keyed_url_template_is_refused_at_install_time() {
        for url in [
            // Userinfo.
            "https://user{{config.endpoint}}@h.example/mcp",
            // A port is digits; the probe render does not parse at all.
            "https://mcp.example.com:{{config.port}}/mcp",
            // Scheme slot: the probe render is not `http(s)`.
            "{{config.endpoint}}://mcp.example.com/mcp",
            // WHATWG retargeting written into the template itself.
            r"https://mcp.example.com\{{config.path}}",
            "https://mcp.example.com/mcp#{{config.rev}}",
        ] {
            let err = http_with_url(url, true)
                .expect_err("a keyed template must be refused at manifest-parse time");
            let err = err.to_string();
            assert!(err.contains("mcp_http.url"), "{url}: {err}");

            // Unkeyed: the same template installs; `resolve_mcp_http_url` is the only gate.
            http_with_url(url, false).unwrap_or_else(|e| {
                panic!("unkeyed connectors keep their url wide open: {url}: {e}")
            });
        }
    }

    /// The render-time placement, reached the only way it can be: an [`McpHttpBlock`] that never went through [`Manifest::parse`].
    #[test]
    fn a_hand_built_block_is_still_refused_at_render_time() {
        let block = McpHttpBlock {
            url: "https://user{{config.endpoint}}@h.example/mcp".to_string(),
            api_key_secret: Some("API_KEY".to_string()),
            api_key_in: Some("bearer".to_string()),
            header_secrets: BTreeMap::new(),
            tools_all: false,
            tools_allow: vec!["quote".to_string()],
            request_timeout_ms: None,
            bringup_timeout_ms: None,
        };
        let effective: serde_json::Map<String, Value> =
            serde_json::from_value(json!({ "endpoint": ".evil.example/" })).unwrap();
        let err = resolve_mcp_http_url(&block, &effective)
            .expect_err("a userinfo slot moves the rendered host to the operator's value");
        assert!(
            err.contains("origin is locked") && err.contains("mcp_http.url"),
            "{err}"
        );
    }

    /// Same fixture, key removed: the tier flips and the whole url — host included — becomes configurable.
    #[test]
    fn an_unkeyed_connector_may_have_its_entire_url_configured() {
        let resolved = resolve(
            "{{config.endpoint}}",
            false,
            json!({ "endpoint": "http://192.168.1.9:8931/mcp" }),
        )
        .expect("with no credential to divert there is nothing to lock");
        assert_eq!(resolved.as_str(), "http://192.168.1.9:8931/mcp");

        // The identical manifest WITH a key refuses it, so the difference is the tier and not the fixture.
        let err = refuse_or_resolve(
            "{{config.endpoint}}",
            true,
            json!({ "endpoint": "http://192.168.1.9:8931/mcp" }),
        )
        .expect_err("a keyed connector may not have its whole url configured");
        assert!(
            err.contains("origin is locked") || err.contains("mcp_http.url"),
            "{err}"
        );
    }

    /// The WHATWG retargeting cases must be refused just as hard when they arrive as a configuration value.
    #[test]
    fn a_configured_value_is_refused_by_the_real_url_validator() {
        for value in [
            r"mcp\evil.example",
            "mcp\t.evil.example",
            "mcp\n.evil.example",
            "mcp#fragment",
            "mcp\u{7f}",
        ] {
            let res = resolve(
                "https://mcp.example.com/{{config.path}}",
                true,
                json!({ "path": value }),
            );
            let err = res
                .err()
                .unwrap_or_else(|| panic!("{value:?} must be refused"));
            assert!(
                err.contains("mcp_http.url"),
                "{value:?} must be refused BY the url validator, naming the field: {err}"
            );
        }
    }

    /// A value that would have to be re-spelled to be a URL is refused rather
    /// than silently percent-encoded into a different target.
    #[test]
    fn a_configured_value_needing_encoding_is_refused_not_encoded() {
        let err = resolve(
            "https://mcp.example.com/{{config.path}}",
            true,
            json!({ "path": "a b" }),
        )
        .expect_err("a space would have to be encoded");
        assert!(err.contains("canonical"), "{err}");
    }

    /// The slot with nothing in force names the key.
    #[test]
    fn a_url_slot_with_no_value_in_force_is_refused_by_name() {
        let err = resolve("https://mcp.example.com/{{config.path}}", true, json!({}))
            .expect_err("an unfillable url slot must not be contacted");
        assert!(
            err.contains("`path`") && err.contains("Settings › Plugins"),
            "{err}"
        );
    }

    /// A slot must name a declared `config_schema` property, and the bare `{{name}}` form has no meaning in a url.
    #[test]
    fn a_url_slot_must_be_a_declared_config_key() {
        let err = expect_reject(
            http_with_url("https://mcp.example.com/{{config.nope}}", true),
            "an undeclared url slot",
        );
        assert!(err.to_string().contains("config slot `nope`"), "{err}");

        let err = expect_reject(
            http_with_url("https://mcp.example.com/{{path}}", true),
            "a bare argument slot in a url",
        );
        assert!(
            err.to_string().contains("must be written"),
            "a url has no agent arguments: {err}"
        );

        let err = expect_reject(
            http_with_url("https://mcp.example.com/{{config.}}", true),
            "an empty config slot",
        );
        assert!(err.to_string().contains("mcp_http.url"), "{err}");

        let err = expect_reject(
            http_with_url("https://mcp.example.com/x}}", true),
            "a stray closing brace",
        );
        assert!(err.to_string().contains("stray"), "{err}");
    }

    /// A url with no slots: validated at parse time, resolvable with no configuration at all.
    #[test]
    fn an_unslotted_url_is_still_validated_at_parse_time() {
        let err = expect_reject(
            http_with_url("https://mcp.example.com/mcp#frag", true),
            "a literal url with a fragment",
        );
        assert!(err.to_string().contains("mcp_http.url"), "{err}");

        let resolved = resolve("https://mcp.example.com/mcp", true, json!({}))
            .expect("a literal url resolves with no configuration");
        assert_eq!(resolved.as_str(), "https://mcp.example.com/mcp");
    }

    /// Whatever the parser normalizes, the manifest must have been written that way.
    #[test]
    fn mcp_http_url_must_be_written_in_canonical_form() {
        for noncanonical in [
            "https://mcp.example.com",          // → `.../` (empty path added)
            "https://MCP.Example.COM/mcp",      // → lowercased host
            "https://mcp.example.com:443/mcp",  // → default port dropped
            "https://mcp.example.com/a/../mcp", // → path normalized
        ] {
            let mut block = mcp_http_block();
            block["url"] = json!(noncanonical);
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                noncanonical,
            );
            assert!(
                err.to_string().contains("canonical"),
                "{noncanonical}: {err}"
            );
        }
        // The canonical spellings are accepted: the rule is "write it canonically", not "we reject these hosts".
        for good in [
            "https://mcp.example.com/",
            "https://mcp.example.com/mcp",
            "https://mcp.example.com:8443/mcp",
        ] {
            let mut block = mcp_http_block();
            block["url"] = json!(good);
            Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block })))
                .unwrap_or_else(|e| panic!("`{good}` must be accepted: {e}"));
        }
    }

    /// `FILE://x/mcp` is non-canonical AND unsupported; the author must be told the scheme is wrong.
    #[test]
    fn an_unsupported_scheme_is_named_even_when_it_is_also_non_canonical() {
        for bad in ["FILE://x/mcp", "FTP://mcp.example.com/mcp"] {
            let mut block = mcp_http_block();
            block["url"] = json!(bad);
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                bad,
            );
            assert!(err.to_string().contains("scheme must be"), "`{bad}`: {err}");
            assert!(
                !err.to_string().contains("canonical"),
                "`{bad}` must not be reported as a formatting problem: {err}"
            );
        }
    }

    #[test]
    fn a_connector_may_not_declare_a_forge_action_tool() {
        for (kind, block_key, block) in [
            ("mcp-http", "mcp_http", mcp_http_block()),
            ("cli-query", "cli_query", cli_query_block()),
        ] {
            let err = Manifest::parse(&base(json!({
                "kind": kind,
                block_key: block,
                "exposes_tools": [
                    { "name": "ok_tool" },
                    { "name": "forge_it", "kind": "forge-action" },
                ],
            })))
            .expect_err("{kind}: forge-action on a connector must be refused");
            assert!(err.to_string().contains("exposes_tools"), "{kind}: {err}");
            assert!(err.to_string().contains("forge_it"), "{kind}: {err}");
        }
        // The same manifest without the forge-action tool parses.
        Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": mcp_http_block(),
            "exposes_tools": [{ "name": "ok_tool" }],
        })))
        .expect("a plain connector tool list is fine");
        // And `app` plugins keep the capability.
        Manifest::parse(&base(json!({
            "entrypoint": { "command": "bin/run" },
            "exposes_tools": [{ "name": "forge_it", "kind": "forge-action" }],
        })))
        .expect("app plugins may still declare forge actions");
    }

    #[test]
    fn header_api_key_name_must_be_a_legal_field_name() {
        for bad in ["x api key", "x:key", "x\nkey", "x=key", "(key)"] {
            let mut block = mcp_http_block();
            block["api_key_in"] = json!(format!("header:{bad}"));
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                bad,
            );
            assert!(err.to_string().contains("api_key_in"), "`{bad}`: {err}");
        }
        for good in ["x-api-key", "Authorization", "X_Api_Key1"] {
            let mut block = mcp_http_block();
            block["api_key_in"] = json!(format!("header:{good}"));
            Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block })))
                .unwrap_or_else(|e| panic!("`{good}` must be accepted: {e}"));
        }
    }

    /// Every spelling of the `query` scheme lands on the migration message, not the generic closed-set one.
    #[test]
    fn a_retired_query_placement_is_rejected_with_a_migration_message() {
        for retired in [
            "query:api_key",
            "query:key",
            "query:",
            "query",
            "query:a=b",
            "query:a b",
        ] {
            let mut block = mcp_http_block();
            block["api_key_in"] = json!(retired);
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                retired,
            )
            .to_string();
            assert!(err.contains("api_key_in"), "`{retired}`: {err}");
            assert!(
                err.contains("retired"),
                "`{retired}` must be refused AS retired, not as an unknown scheme: {err}"
            );
            assert!(
                err.contains("`bearer`") && err.contains("`header:<name>`"),
                "`{retired}` must name both replacements: {err}"
            );
        }
    }

    /// Proves the unconditional refusal of the retired value only; a keyless `cookie:k` still parses, deliberately.
    #[test]
    fn a_retired_query_placement_is_rejected_even_without_a_credential() {
        for retired in ["query:api_key", "query:", "query"] {
            let mut block = mcp_http_block();
            block.as_object_mut().unwrap().remove("api_key_secret");
            block["api_key_in"] = json!(retired);
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                &format!("keyless `{retired}`"),
            )
            .to_string();
            assert!(err.contains("mcp_http.api_key_in"), "`{retired}`: {err}");
            assert!(err.contains("retired"), "`{retired}`: {err}");
        }

        // Positive control: a keyless manifest that says nothing about `api_key_in` still parses.
        let mut ok = mcp_http_block();
        let obj = ok.as_object_mut().unwrap();
        obj.remove("api_key_secret");
        obj.remove("api_key_in");
        Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": ok })))
            .expect("a keyless connector with no api_key_in must still parse");

        // A keyless manifest naming a SURVIVING form parses too; it sends nothing.
        let mut ok2 = mcp_http_block();
        ok2.as_object_mut().unwrap().remove("api_key_secret");
        ok2["api_key_in"] = json!("bearer");
        Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": ok2 })))
            .expect("a keyless `bearer` is not what this rule is about");
    }

    #[test]
    fn api_key_in_is_a_closed_set_and_required_with_a_secret() {
        let mut block = mcp_http_block();
        block["api_key_in"] = json!("body:token");
        let err = Manifest::parse(&base(
            json!({ "kind": "mcp-http", "mcp_http": block.clone() }),
        ))
        .expect_err("unknown api_key_in location rejected");
        assert!(err.to_string().contains("api_key_in"), "{err}");

        let mut block = mcp_http_block();
        block.as_object_mut().unwrap().remove("api_key_in");
        let err = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block })))
            .expect_err("api_key_in required when a secret is named");
        assert!(err.to_string().contains("api_key_in"), "{err}");
    }

    #[test]
    fn api_key_in_parses_both_surviving_forms() {
        assert_eq!(ApiKeyIn::parse("bearer"), Some(ApiKeyIn::Bearer));
        assert_eq!(
            ApiKeyIn::parse("header:x-api-key"),
            Some(ApiKeyIn::Header("x-api-key".into()))
        );
        // `bearer` takes no argument: it is a value SHAPE, not a location.
        assert_eq!(ApiKeyIn::parse("bearer:x"), None);
        assert_eq!(ApiKeyIn::parse("Bearer"), None);
        assert_eq!(ApiKeyIn::parse("header:"), None);
        assert_eq!(ApiKeyIn::parse("cookie:k"), None);
        assert_eq!(ApiKeyIn::parse("api_key"), None);
        // Retired; the validator owns the migration message, not `parse`.
        assert_eq!(ApiKeyIn::parse("query:api_key"), None);
        assert!(ApiKeyIn::is_retired_query("query:api_key"));
        assert!(ApiKeyIn::is_retired_query("query"));
        assert!(!ApiKeyIn::is_retired_query("queryish:x"));
        assert!(!ApiKeyIn::is_retired_query("header:x"));
    }

    #[test]
    fn timeout_defaults_and_overrides() {
        let m = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": {
            "url": "https://x.example/mcp"
        }})))
        .unwrap();
        assert_eq!(
            m.mcp_http.unwrap().timeout_ms(),
            MCP_HTTP_DEFAULT_TIMEOUT_MS
        );

        let m = Manifest::parse(&base(
            json!({ "kind": "mcp-http", "mcp_http": mcp_http_block() }),
        ))
        .unwrap();
        assert_eq!(m.mcp_http.unwrap().timeout_ms(), 10_000);
    }

    /// The bring-up budget is bounded by construction, including when derived from the unbounded call timeout.
    #[test]
    fn bringup_timeout_is_capped_however_the_call_timeout_is_configured() {
        // Derived default tracks a modest call timeout verbatim…
        let m = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": {
            "url": "https://x.example/mcp",
            "request_timeout_ms": 2_000,
        }})))
        .unwrap();
        let block = m.mcp_http.unwrap();
        assert_eq!(block.timeout_ms(), 2_000);
        assert_eq!(block.bringup_timeout_ms(), 2_000);

        // …and is clamped as soon as that timeout stops being a sane boot budget.
        let m = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": {
            "url": "https://x.example/mcp",
            "request_timeout_ms": 600_000,
        }})))
        .unwrap();
        let block = m.mcp_http.unwrap();
        assert_eq!(
            block.timeout_ms(),
            600_000,
            "a long tools/call budget must stay long"
        );
        assert_eq!(
            block.bringup_timeout_ms(),
            MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS,
            "bring-up must not inherit an unbounded call budget"
        );

        // An explicit bring-up override under the ceiling is honoured…
        let m = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": {
            "url": "https://x.example/mcp",
            "request_timeout_ms": 600_000,
            "bringup_timeout_ms": 750,
        }})))
        .unwrap();
        let block = m.mcp_http.unwrap();
        assert_eq!(block.bringup_timeout_ms(), 750);
        assert_eq!(block.timeout_ms(), 600_000);
    }

    /// …and one over the ceiling is refused at PARSE time.
    #[test]
    fn a_bringup_timeout_over_the_ceiling_is_a_manifest_error() {
        let err = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": {
            "url": "https://x.example/mcp",
            "bringup_timeout_ms": MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS + 1,
        }})))
        .expect_err("a bring-up budget over the ceiling must not load");
        assert!(err.to_string().contains("bringup_timeout_ms"), "{err}");

        // Exactly at the ceiling is fine — the bound is `>`, not `>=`.
        Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": {
            "url": "https://x.example/mcp",
            "bringup_timeout_ms": MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS,
        }})))
        .expect("exactly the ceiling must load");
    }

    #[test]
    fn cli_query_argv_slot_must_be_a_whole_element() {
        let mut block = cli_query_block();
        block["tools"][0]["args"] = json!(["quote", "--sym={{symbol}}"]);
        let err = Manifest::parse(&base(json!({ "kind": "cli-query", "cli_query": block })))
            .expect_err("partial substitution must be rejected");
        assert!(err.to_string().contains("whole argv element"), "{err}");
    }

    #[test]
    fn cli_query_slot_must_be_a_declared_schema_property() {
        let mut block = cli_query_block();
        block["tools"][0]["args"] = json!(["quote", "{{ticker}}"]);
        let err = Manifest::parse(&base(json!({ "kind": "cli-query", "cli_query": block })))
            .expect_err("undeclared slot must be rejected");
        assert!(err.to_string().contains("ticker"), "{err}");
    }

    /// The `config_schema` these tests configure against. Driving `Manifest::parse` is deliberate: the rules are CROSS-FIELD (block ↔ `config_schema`).
    fn cli_config_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "endpoint": { "type": "string", "default": "https://api.example" },
                "LB_ACCOUNT": { "type": "string" }
            },
            "additionalProperties": false
        })
    }

    fn cli_with_config(block: Value) -> Result<Manifest, ManifestError> {
        Manifest::parse(&base(json!({
            "kind": "cli-query",
            "cli_query": block,
            "config_schema": cli_config_schema(),
        })))
    }

    /// The positive half: without it every assertion below is satisfiable by a validator that refuses everything.
    #[test]
    fn a_config_slot_and_config_env_are_accepted_when_declared() {
        let mut block = cli_query_block();
        block["config_env"] = json!(["LB_ACCOUNT"]);
        block["tools"][0]["args"] = json!(["quote", "{{symbol}}", "--url", "{{config.endpoint}}"]);
        let m = cli_with_config(block).expect("a declared config slot + config_env must load");
        let block = m.cli_query.unwrap();
        assert_eq!(block.config_env, vec!["LB_ACCOUNT".to_string()]);
        assert_eq!(
            argv_slot(&block.tools[0].args[3]),
            Some(ArgvSlot::Config("endpoint"))
        );
    }

    /// The parse-time half: a tool may not declare an input property inside the reserved namespace.
    #[test]
    fn an_input_schema_property_may_not_claim_the_config_namespace() {
        let mut block = cli_query_block();
        block["tools"][0]["input_schema"]["properties"]["config.endpoint"] =
            json!({ "type": "string" });
        let err = cli_with_config(block)
            .expect_err("a tool must not declare an input property in the config namespace");
        let err = err.to_string();
        assert!(err.contains("config.endpoint"), "{err}");
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn a_config_slot_must_name_a_declared_config_schema_property() {
        let mut block = cli_query_block();
        block["tools"][0]["args"] = json!(["quote", "{{config.ghost}}"]);
        let err = cli_with_config(block).expect_err("an undeclared config slot must be refused");
        assert!(err.to_string().contains("ghost"), "{err}");

        // …and with no `config_schema` at all, nothing is declared.
        let mut block = cli_query_block();
        block["tools"][0]["args"] = json!(["quote", "{{config.endpoint}}"]);
        let err = Manifest::parse(&base(json!({ "kind": "cli-query", "cli_query": block })))
            .expect_err("no config_schema declares no config keys");
        assert!(err.to_string().contains("endpoint"), "{err}");

        // The degenerate form is refused by name, not as stray braces.
        let mut block = cli_query_block();
        block["tools"][0]["args"] = json!(["quote", "{{config.}}"]);
        let err = cli_with_config(block).expect_err("`{{config.}}` names no key");
        assert!(
            err.to_string().contains("names no configuration key"),
            "{err}"
        );
    }

    /// Rule (i). No credential denylist here: a `config_env` value is typed in by the operator, so it escalates nothing.
    #[test]
    fn a_config_env_key_must_be_a_legal_env_name_and_a_declared_property() {
        let mut block = cli_query_block();
        block["config_env"] = json!(["2BAD"]);
        let err = cli_with_config(block).expect_err("an illegal env name must be refused");
        let err = err.to_string();
        assert!(err.contains("2BAD"), "{err}");
        assert!(err.contains("environment variable name"), "{err}");

        let mut block = cli_query_block();
        block["config_env"] = json!(["NOT_DECLARED"]);
        let err = cli_with_config(block).expect_err("an undeclared config_env key must be refused");
        assert!(err.to_string().contains("NOT_DECLARED"), "{err}");

        // A forge CREDENTIAL name is accepted here, unlike in `env_allow`.
        let mut block = cli_query_block();
        block["config_env"] = json!(["GH_TOKEN"]);
        let mut schema = cli_config_schema();
        schema["properties"]["GH_TOKEN"] = json!({ "type": "string" });
        Manifest::parse(&base(json!({
            "kind": "cli-query",
            "cli_query": block,
            "config_schema": schema,
        })))
        .expect("config_env is not subject to the env_allow credential denylist");
    }

    /// Rule (ii), at all three pairings.
    #[test]
    fn the_three_env_sources_may_not_name_the_same_target_key() {
        let mut schema = cli_config_schema();
        schema["properties"]["LB_TOKEN"] = json!({ "type": "string" });
        schema["properties"]["NO_PROXY"] = json!({ "type": "string" });
        let parse = |block: Value| {
            Manifest::parse(&base(json!({
                "kind": "cli-query",
                "cli_query": block,
                "config_schema": schema,
            })))
        };

        // config_env ∩ secret_env
        let mut block = cli_query_block();
        block["secret_env"] = json!(["LB_TOKEN"]);
        block["config_env"] = json!(["LB_TOKEN"]);
        let err = parse(block).expect_err("config_env ∩ secret_env must be refused");
        let err = err.to_string();
        assert!(err.contains("LB_TOKEN"), "{err}");
        assert!(err.contains("secret_env"), "{err}");

        // config_env ∩ env_allow
        let mut block = cli_query_block();
        block["env_allow"] = json!(["NO_PROXY"]);
        block["config_env"] = json!(["NO_PROXY"]);
        let err = parse(block).expect_err("config_env ∩ env_allow must be refused");
        assert!(err.to_string().contains("NO_PROXY"), "{err}");

        // env_allow ∩ secret_env
        let mut block = cli_query_block();
        block["env_allow"] = json!(["LB_TOKEN"]);
        block["secret_env"] = json!(["LB_TOKEN"]);
        let err = parse(block).expect_err("env_allow ∩ secret_env must be refused");
        assert!(err.to_string().contains("LB_TOKEN"), "{err}");

        // A repeat WITHIN one list is the same collision.
        let mut block = cli_query_block();
        block["config_env"] = json!(["LB_TOKEN", "LB_TOKEN"]);
        let err = parse(block).expect_err("a repeated key is still one target");
        assert!(err.to_string().contains("LB_TOKEN"), "{err}");

        // …and three DISTINCT keys across the three sources are fine.
        let mut block = cli_query_block();
        block["env_allow"] = json!(["NO_PROXY"]);
        block["secret_env"] = json!(["LB_TOKEN"]);
        block["config_env"] = json!(["LB_ACCOUNT"]);
        parse(block).expect("distinct targets must load");
    }

    #[test]
    fn cli_query_requires_a_command_and_at_least_one_tool() {
        let mut block = cli_query_block();
        block["command"] = json!("   ");
        assert!(
            Manifest::parse(&base(json!({ "kind": "cli-query", "cli_query": block }))).is_err()
        );

        let mut block = cli_query_block();
        block["tools"] = json!([]);
        assert!(
            Manifest::parse(&base(json!({ "kind": "cli-query", "cli_query": block }))).is_err()
        );
    }

    #[test]
    fn cli_query_defaults() {
        let m = Manifest::parse(&base(
            json!({ "kind": "cli-query", "cli_query": cli_query_block() }),
        ))
        .unwrap();
        let block = m.cli_query.unwrap();
        assert_eq!(block.timeout_ms(), CLI_QUERY_DEFAULT_TIMEOUT_MS);
        assert_eq!(block.max_output_bytes(), CLI_QUERY_DEFAULT_MAX_OUTPUT_BYTES);
        assert!(block.env_allow.is_empty());
        assert!(block.search_path_extra.is_empty());
    }

    #[test]
    fn argv_slot_matches_only_whole_elements() {
        assert_eq!(argv_slot("{{symbol}}"), Some(ArgvSlot::Argument("symbol")));
        assert_eq!(argv_slot("--sym={{symbol}}"), None);
        assert_eq!(argv_slot("{{}}"), None);
        assert_eq!(argv_slot("quote"), None);
    }

    /// `config.` is a namespace: `{{myconfig.x}}` and `{{config}}` must stay argument slots.
    #[test]
    fn argv_slot_classifies_the_config_namespace() {
        assert_eq!(
            argv_slot("{{config.endpoint}}"),
            Some(ArgvSlot::Config("endpoint"))
        );
        assert_eq!(argv_slot("{{config.a.b}}"), Some(ArgvSlot::Config("a.b")));
        // Not the namespace: no separator, or the prefix is not at the start.
        assert_eq!(argv_slot("{{config}}"), Some(ArgvSlot::Argument("config")));
        assert_eq!(
            argv_slot("{{myconfig.x}}"),
            Some(ArgvSlot::Argument("myconfig.x"))
        );
        // The degenerate namespace form is a slot with an empty name, so the validator can refuse it by name.
        assert_eq!(argv_slot("{{config.}}"), Some(ArgvSlot::Config("")));
        assert_eq!(argv_slot("{{config.endpoint}}").unwrap().name(), "endpoint");
    }

    #[test]
    fn env_key_names_follow_the_posix_shape() {
        for ok in ["PATH", "_X", "LB_ACCOUNT_2", "a"] {
            assert!(is_valid_env_key(ok), "{ok} must be accepted");
        }
        for bad in [
            "", "2FOO", "FOO-BAR", "FOO BAR", "FOO=BAR", "FOO.BAR", "föö",
        ] {
            assert!(!is_valid_env_key(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn connector_tool_names_reject_empty_and_whitespace() {
        assert!(validate_connector_tool_name("get_report", "f").is_ok());
        assert!(validate_connector_tool_name("", "f").is_err());
        assert!(validate_connector_tool_name("  ", "f").is_err());
        assert!(validate_connector_tool_name("two words", "f").is_err());
        assert!(validate_connector_tool_name(" pad ", "f").is_err());
    }
}
