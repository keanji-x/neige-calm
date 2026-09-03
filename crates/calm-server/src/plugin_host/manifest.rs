//! Plugin manifest parsing and validation.
//!
//! Every plugin ships a `manifest.json` at the root of its install directory.
//! This module owns its typed shape, validation, and shared error surface.

use std::collections::HashMap;
use std::fmt;

use crate::mcp_server::tools::plan::key_is_valid;
use crate::validation::KERNEL_OVERLAY_PLUGIN_ID;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The wire key [`Manifest::config_schema`] serializes to.
///
/// It is the **error root path** every `config_schema` violation is reported
/// under (`config_schema.properties.theme.default: …`), which is a string an
/// operator reads next to their own `manifest.json`. Pinned to the real serde
/// output by `config_schema_key_matches_the_serialized_manifest`, so renaming
/// the field can not leave the diagnostics pointing at a key that no longer
/// exists on the wire.
///
/// (S1 review: nothing reads the schema out of the persisted blob any more —
/// `has_config` and the PATCH validator both go through the registry's typed
/// [`Manifest`]. See `routes::plugins::registry_manifest` for why.)
pub const CONFIG_SCHEMA_KEY: &str = "config_schema";

/// Top-level manifest blob loaded from `<install_path>/manifest.json`.
///
/// Unknown fields are tolerated (forwards compatibility). Missing optional
/// fields default; missing required fields fail in `parse`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Manifest {
    /// Which spelling of the template-binding array this file uses.
    ///
    /// * `1` — the array was spelled `workflows`. That spelling is **gone**
    ///   (see [`Manifest::reject_retired_workflows_key`]), so a v1 file is
    ///   still accepted only when it declares no bindings at all — nothing
    ///   about such a file changed in #1268 and it reads identically on either
    ///   kernel.
    /// * `2` — the array is spelled `templates` (#1268).
    /// * `3` — the manifest may declare a [`Self::config_schema`] with a
    ///   non-empty `required` (#1284). Same rollback argument as `2`, one
    ///   layer up: a pre-#1284 kernel ignores `config_schema` entirely, so a
    ///   plugin whose configuration is *mandatory* would come up on that
    ///   kernel with none of it — silently. Declaring `3` makes that old
    ///   kernel refuse the file by version instead (it accepts only `1..=2`),
    ///   which under `registry::load_from_dir` means the plugin **disappears
    ///   from the list** rather than running mis-configured. A
    ///   `config_schema` whose keys are all optional loses nothing on that
    ///   kernel — it degrades to "no configuration", which is precisely what
    ///   every default already means — so it stays at `2` and keeps loading.
    ///
    /// Any other value is rejected by [`Manifest::validate`].
    ///
    /// **Why a bump at all**, given the retired-key guard already covers the
    /// upgrade direction: it is the *rollback* direction that needs it. A
    /// `templates[]` manifest handed to a pre-#1268 kernel hits a parser that
    /// ignores unknown top-level keys, so the binding list silently defaults
    /// to empty and `issue-development` loses its `input_schema` with no error
    /// and no log. Declaring `2` makes that old kernel's own
    /// `manifest_version != 1` check refuse the file by version instead —
    /// which is why [`Manifest::validate`] *requires* `2` from any manifest
    /// that actually declares a binding.
    ///
    /// **And why the requirement is scoped to those manifests rather than to
    /// all of them.** Requiring `2` everywhere would refuse every existing
    /// binding-less manifest — and the boot loader turns a parse failure into
    /// `warn!` + skip (`registry::load_from_dir`), so those plugins would just
    /// disappear, which is the same silent-loss shape this bump exists to
    /// remove. A manifest with no bindings has nothing to lose on rollback: it
    /// reads identically on a pre-#1268 and a post-#1268 kernel, because the
    /// only thing the two disagree about is the name of an array it does not
    /// have. Scoping the requirement to manifests that *do* declare a binding
    /// is therefore what keeps binding-less manifests working across both
    /// kernels while still closing the rollback hazard completely — every
    /// manifest that could lose a binding is forced onto `2`, and every
    /// manifest that could not is left alone. This is not hypothetical: the
    /// plugin roots on real deployments hold connector manifests
    /// (`kind: "mcp-http"`, `cli-query`) that never declared one.
    pub manifest_version: u32,

    /// Reverse-DNS or slug, see `is_valid_plugin_id`. Stable across versions.
    pub id: String,

    /// Semver string. Validated; stored verbatim.
    pub version: String,

    /// Refuse to spawn if the running kernel is older than this. Validated as
    /// semver here; the actual comparison runs at spawn time (Slice B).
    pub min_kernel_version: String,

    pub display_name: String,

    /// #1164 §2.1 — connector kind. Absent ⇒ [`ConnectorKind::App`], which is
    /// exactly today's plugin semantics (the tree's only checked-in manifest,
    /// `plugins/git-forge`, carries no `kind` key). An *unknown* value is a
    /// hard parse error, never a silent downgrade to `app` — serde's
    /// `unknown variant` message names the accepted set.
    ///
    /// `#[serde(default)]` on the FIELD is load-bearing: deriving `Default`
    /// on the enum alone does not make a missing key legal.
    #[serde(default)]
    pub kind: ConnectorKind,

    /// Remote streamable-HTTP MCP server config. Present iff
    /// `kind == McpHttp` (enforced in [`Manifest::validate`]).
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

    /// How to launch the plugin process. **Required for
    /// [`ConnectorKind::App`], optional otherwise** (#1164 §2.1) — remote
    /// MCP servers and query CLIs have no kernel-supervised child process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Entrypoint>,

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

    /// Wave `template_input` contract (#891 / #1110 S2). One plugin, one
    /// input shape — sibling of `exposes_tools`, not of a template
    /// descriptor. Same JSON-Schema subset as `plugin_host::template_input`.
    /// Absent: the plugin does not accept `template_input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,

    /// #1284 §2.1 — the plugin's **user-configuration** contract: what an
    /// operator may set in Settings › Plugins, in the same JSON-Schema subset
    /// as [`Self::input_schema`] (`plugin_host::template_input`, error paths
    /// rooted at `config_schema…`).
    ///
    /// Unlike `input_schema` this is **not** app-only: all three kinds
    /// (`app` / `mcp-http` / `cli-query`) grow a consumer in S2/S3a/S3b, so
    /// [`Manifest::reject_app_only_surfaces`] deliberately leaves it alone.
    ///
    /// Absent ⇒ the plugin has **no** configurable surface, and
    /// `PATCH /api/plugins/{id}/config` refuses the write with a 400. "No
    /// config schema" and "config UI not built yet" must be two different
    /// things on screen; `PluginListItem::has_config` is the list-side
    /// projection of exactly this field's presence.
    ///
    /// Values declared here are **not** persisted at their defaults — see
    /// [`super::effective_config`]: `default` is applied on read, so changing
    /// a default in a later manifest version still reaches plugins whose
    /// operator never touched that key.
    ///
    /// Declaring a schema with a non-empty `required` forces
    /// `manifest_version >= 3`; see [`Manifest::manifest_version`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<Value>,

    /// Trusted forge plugins may claim **kernel wave-template ids**. Wave
    /// create binds `template_id` to one of these so the kernel can copy the
    /// owning plugin into `plugin_scope` and validate `template_input` against
    /// this Manifest's `input_schema` (#1110 S2/S5). Untrusted plugins'
    /// ids are ignored by the binding layer; the parser still checks id
    /// shape so broken entries fail close to the authoring point.
    ///
    /// **#1209 narrowed this capability, and this is the contract, not a
    /// note.** Declaring an id is *claiming an existing template*, never
    /// *creating* one. The ids the kernel knows are the wave-template roster
    /// (`crate::templates::TEMPLATES`: today
    /// `issue-development`, `small-change`, `investigation`), and
    /// `POST /api/waves` admits an id **iff it is in that roster** — plugin
    /// declarations do not widen the set. An id outside the roster is
    /// therefore inert: it is parsed, it is not rejected here, and it can
    /// never be bound **through `POST /api/waves`** — the only production
    /// writer of `waves.template_id` — because that create is a 400. The
    /// repo-layer `wave_create` takes `template_id` / `plugin_scope`
    /// verbatim and enforces nothing; its non-route callers are all test
    /// fixtures passing `None` today, and a future in-process writer that
    /// wanted this guarantee would have to call the admission itself. Before
    /// #1209 a running trusted plugin *could* make an arbitrary id creatable;
    /// that is the capability this field no longer has. Plugin-contributed
    /// templates (which would need a title, tasks and a report) are a separate
    /// piece of work — see `docs/architecture/1209-template-workflow-unify.md`
    /// §5, option C.
    #[serde(default)]
    pub templates: Vec<TemplateDescriptor>,

    /// Missing block treated as the most-restrictive permission set.
    #[serde(default)]
    pub permissions: Permissions,
}

/// #1164 §2.1 — what kind of external capability this manifest describes.
///
/// `App` is the pre-#1164 plugin: a kernel-supervised child process speaking
/// stdio MCP. The other two variants are *connectors* — no child process, no
/// `neige.*` inbound router, no plugin token.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectorKind {
    /// Today's plugin. Semantics unchanged, byte for byte.
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

    /// `true` for the pre-#1164 process-backed plugin.
    pub fn is_app(self) -> bool {
        matches!(self, Self::App)
    }
}

/// Where the API key rides on an outbound `mcp-http` request. Closed set:
/// `query:<name>` or `header:<name>` (§2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyIn {
    Query(String),
    Header(String),
}

impl ApiKeyIn {
    /// Parse the manifest's `api_key_in` string. `None` for anything outside
    /// the closed set — the caller turns that into a validation error.
    pub fn parse(s: &str) -> Option<Self> {
        let (scheme, name) = s.split_once(':')?;
        if name.trim().is_empty() {
            return None;
        }
        match scheme {
            "query" => Some(Self::Query(name.to_string())),
            "header" => Some(Self::Header(name.to_string())),
            _ => None,
        }
    }
}

/// Default per-request timeout for a steady-state `tools/call` (§2.2).
///
/// **This value is deliberately NOT the bound that protects boot** — see
/// [`MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS`]. It may be raised without limit: a
/// report-generating MCP tool legitimately runs for minutes, and nothing on the
/// `tools/call` path is awaited by `AppState::new`.
pub const MCP_HTTP_DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// Hard ceiling on the bring-up (`initialize` + `tools/list`) timeout, enforced
/// here at manifest-parse time.
///
/// **Why a second knob exists at all.** `request_timeout_ms` used to govern two
/// things with opposite constraints, and every round of review patched the
/// arithmetic while the defect reappeared one level up:
///
/// * **bring-up** sits on the inline-awaited boot path (`AppState::new` →
///   `autospawn_enabled` → `spawn_admitted` → `tools/list`), so it must be
///   SHORT and hard-bounded — while it runs, the server does not serve;
/// * **steady-state `tools/call`** is not on the boot path at all and is
///   legitimately long.
///
/// One knob could satisfy neither: clamping it broke long tool calls, and
/// widening the boot budget to respect it made boot latency operator-controlled
/// and unbounded (`"request_timeout_ms": 600000` against a black-holed upstream
/// stalled boot for 20.5 minutes). Splitting them makes the boot bound hold *by
/// construction* for every manifest: `connector_bringup_budget` can never
/// exceed `2 × 15 s + slack`, whatever the manifest asks for.
///
/// (Same shape as `trusted_forge_plugin`, one bit that gates both "may hold a
/// wave scope" and "gets the forge credential passthrough". The general lesson:
/// when a constant needs adjusting for the third time, stop adjusting it and
/// look for the second constraint riding on it.)
///
/// 15 s is chosen as "generous for a TLS handshake plus a cold upstream's first
/// response, still short enough that a full slate of dead connectors cannot
/// push boot past [`super::CONNECTOR_AUTOSPAWN_BUDGET`]".
pub const MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS: u64 = 15_000;

/// `mcp_http` top-level block. Present iff `kind == "mcp-http"`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct McpHttpBlock {
    /// Absolute `http://` or `https://` endpoint.
    pub url: String,

    /// Name of the key in the connector's `secrets.json` holding the API key.
    /// Absent ⇒ unauthenticated requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret: Option<String>,

    /// `query:<name>` | `header:<name>`. Required when `api_key_secret` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_in: Option<String>,

    /// Hand-written allowlist of upstream tool names to expose. Names the
    /// upstream does not serve are warned about and skipped, not fatal (§2.2).
    #[serde(default)]
    pub tools_allow: Vec<String>,

    /// Steady-state `tools/call` timeout. Overrides
    /// [`MCP_HTTP_DEFAULT_TIMEOUT_MS`]. **No upper bound** — see that constant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,

    /// Bring-up (`initialize` + `tools/list`) timeout. Capped at
    /// [`MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS`], which `validate` refuses to let a
    /// manifest exceed.
    ///
    /// Absent ⇒ `min(request_timeout_ms, ceiling)`. Deriving the default from
    /// the call timeout keeps every manifest written before the split meaning
    /// what it meant (a small `request_timeout_ms` was, in practice, an
    /// operator asking for a fast bring-up), while the `min` is what makes the
    /// boot bound hold regardless of what that field says.
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

    /// The bring-up budget — the one on the inline-awaited boot path.
    ///
    /// The trailing `.min(...)` is not belt-and-braces with `validate`: it is
    /// what makes the bound total. `validate` refuses an explicit value over
    /// the ceiling, but the DERIVED default comes from an unbounded field, so
    /// without the clamp `"request_timeout_ms": 600000` would re-create exactly
    /// the 20.5-minute boot stall the split exists to remove.
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

/// Ceiling on the effective `cli_query.max_output_bytes` (#1164 P3 r2 G1).
///
/// **Why a ceiling at all.** Without one the value was unbounded, and
/// `usize::MAX` loaded — which made `cap + 1` in the capture path overflow: a
/// debug panic, and in release a wrap to `take(0)` that returned an EMPTY
/// stdout with `is_error: false`, i.e. silent data loss. The arithmetic is now
/// saturating regardless, but a manifest that can ask for an unbounded answer
/// is a memory bound nobody can reason about.
///
/// **Why 8 MiB.** This is ONE tool call's stdout, held whole in memory and then
/// embedded in a JSON text block (which roughly doubles it) on its way to the
/// agent — and an agent has to read it. Real query answers are kilobytes; the
/// default is 32 KiB. 8 MiB is ~250× the default, so no legitimate author is
/// squeezed, while the worst a manifest can cost per concurrent call stays in
/// the tens of megabytes rather than "whatever the child felt like writing".
///
/// **Why this CLAMPS rather than refusing to parse** (r3 H7). A parse-time
/// refusal is retroactive: `registry::load_from_dir` re-parses every installed
/// manifest at boot and merely `warn!`s past one that fails, so raising or
/// introducing a ceiling makes a connector that worked yesterday silently
/// vanish. This module already argues exactly that against widening the
/// `env_allow` denylist; adding the same hazard for a number would be
/// inconsistent. A credential denylist has no safe fallback — forwarding the
/// key anyway is the harm — whereas an over-large cap has an obviously correct
/// one: give the author the maximum and say so. The memory bound holds either
/// way, which is the only property that mattered.
pub const CLI_QUERY_MAX_OUTPUT_BYTES_CEILING: usize = 8 * 1024 * 1024;

/// `cli_query` top-level block. Present iff `kind == "cli-query"`.
///
/// #1164 P1 parses and validates this block; the execution runtime lands in a
/// later slice (§7 P3).
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

    /// The cap the runtime actually uses: the manifest's value, defaulted when
    /// absent or zero, and CLAMPED to [`CLI_QUERY_MAX_OUTPUT_BYTES_CEILING`].
    ///
    /// Clamped rather than refused so a manifest that used to load never stops
    /// loading — see the ceiling's doc for why that asymmetry with the
    /// `env_allow` denylist is deliberate.
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

/// One hand-declared CLI tool. `args` is a fixed argv template: a `{{slot}}`
/// element is replaced *wholesale* by one argument. No shell, no string
/// concatenation (§2.3).
///
/// # A value cannot become two arguments — but it can become a flag
///
/// Whole-element substitution means a value is never re-split and never
/// concatenated (partial forms like `--out={{x}}` are refused at manifest-parse
/// time), so shell metacharacters and whitespace are inert: `; rm -rf /` is one
/// literal argv element.
///
/// It can still be *option-shaped*: `{"path": "--output=/etc/cron.d/x"}` reaches
/// the child as the literal element `--output=/etc/cron.d/x`, and a CLI that
/// accepts options anywhere in its argv will read it as one. The kernel does
/// **not** refuse leading dashes (that would break legitimate values) and does
/// **not** insert `--` for you (many CLIs do not accept it).
///
/// **An author who wants positional-only values writes the separator into the
/// template**: `"args": ["quote", "--", "{{symbol}}"]`. A literal `--` element
/// passes validation like any other literal, and every CLI that follows the
/// convention treats what follows as positional.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CliQueryTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    pub args: Vec<String>,
}

/// If `s` is exactly `{{name}}`, return `name`. Partial occurrences (e.g.
/// `--sym={{symbol}}`) deliberately do NOT match: the template only supports
/// whole-argv substitution.
pub fn argv_slot(s: &str) -> Option<&str> {
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.is_empty() { None } else { Some(inner) }
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

/// Wave-create handle that names a plugin-owned template id.
///
/// #1110 S5 shrunk this to `{ id }`. Plan prose, gates, spec instructions,
/// and card kinds left the parser; `input_schema` lives on [`Manifest`].
/// Extra JSON keys are ignored (same forwards-compat as [`Manifest`]).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TemplateDescriptor {
    pub id: String,
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

    /// Deprecated compatibility field. The proposal channel was withdrawn;
    /// persisted manifests may still contain this Tier-A field, so it remains
    /// parseable but is intentionally ignored.
    #[serde(default)]
    pub proposals: Vec<String>,

    /// Per-plugin KV store cap in bytes. Slice C enforces; 0 = no KV access.
    #[serde(default)]
    pub kv_quota_bytes: u64,

    /// Future expansion (declared roots). Validated as a list of strings; no
    /// semantics in M3.
    #[serde(default)]
    pub filesystem: Vec<String>,
}

impl Permissions {
    /// `true` when this block grants literally nothing — i.e. it is
    /// indistinguishable from an absent `permissions` key.
    ///
    /// #1164 §3 uses this to refuse a connector manifest that *requests*
    /// anything: connectors have no `neige.*` channel, so a granted permission
    /// would be a claim the kernel could never honour. `proposals` is excluded
    /// on purpose — it is the withdrawn, ignored Tier-A compatibility field, so
    /// a persisted manifest carrying it must not become unparseable.
    pub fn grants_nothing(&self) -> bool {
        self.overlays_write.is_empty()
            && !self.cards_create
            && !self.cards_read_all
            && self.events_subscribe.is_empty()
            && self.kv_quota_bytes == 0
            && self.filesystem.is_empty()
    }
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
        // Raw-text guard first: when a file carries the retired key, "rename it
        // to `templates`" is the actionable message, not "wrong version".
        Self::reject_retired_workflows_key(s)?;
        m.validate()?;
        Ok(m)
    }

    /// #1268 — refuse a manifest that still spells [`Self::templates`] the old
    /// way (`workflows`).
    ///
    /// `Manifest` deliberately tolerates unknown top-level keys, so without
    /// this the rename would be a **silent** contract break: an old manifest
    /// would parse, declare zero bindings, and `issue-development` would
    /// quietly lose its `input_schema` — every `POST /api/waves` carrying
    /// `template_input` would then 400 with nothing pointing at the cause.
    /// Naming the new key in the error costs one extra `Value` parse of a file
    /// that is at most a few KB and is read once per install/reload.
    ///
    /// This runs on the *raw text*, not on the deserialized struct, precisely
    /// because the struct is where the evidence has already been discarded —
    /// hence an associated fn with no `&self`: there is nothing in the parsed
    /// value for it to consult, and a `&self` receiver would invite exactly the
    /// misreading this paragraph exists to prevent.
    ///
    /// Deliberately **not** conditioned on `manifest_version`: the retired key
    /// is refused at every declared version, so "v1 said `workflows`" is never
    /// a way back in.
    fn reject_retired_workflows_key(s: &str) -> Result<(), ManifestError> {
        let Ok(Value::Object(raw)) = serde_json::from_str::<Value>(s) else {
            // Not an object, or not valid JSON — `serde_json::from_str::<Manifest>`
            // above already succeeded, so this branch is unreachable in practice;
            // there is nothing to check either way.
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

    /// Validate an already-deserialized manifest. Exposed publicly so callers
    /// holding a `Manifest` (e.g. after editing in-memory) can re-check it.
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

        // #1268 — a manifest that actually declares a binding MUST say 2.
        //
        // This is the whole point of the bump, and it is deliberately scoped to
        // files that have something to lose. The hazard is rollback: a
        // `templates[]` file read by a pre-#1268 kernel parses clean (unknown
        // top-level keys are ignored), declares no binding, and silently drops
        // `issue-development`'s `input_schema`. Declaring 2 turns that into the
        // old kernel's own `manifest_version != 1` refusal.
        //
        // A manifest with no bindings has no such exposure — it reads
        // identically on both kernels — so it is left alone rather than broken
        // for symmetry. That matters in practice: the plugin install root on a
        // real deployment holds connector manifests (`kind: "mcp-http"`,
        // `cli-query`) that never declared a binding, and the boot loader
        // treats a parse failure as `warn!` + skip (`registry.rs`), i.e. losing
        // them would be quiet — the exact failure shape this rule exists to
        // remove.
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

        // #1297: `kernel` is a reserved writer identity, not merely a naming
        // convention. `card_fsm` stamps it on the overlay rows the scheduler
        // and spec-harness admission read back as fact, and the callback path
        // writes `ctx.plugin_id` verbatim — so a plugin that simply *named
        // itself* `kernel` would forge that authorship without touching any
        // of the guards on the REST side. The regex above admits it, so the
        // refusal has to be explicit and it has to be here, at the only place
        // a plugin id enters the system.
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

        // #1164 §2.1 — kind ↔ block consistency. Exactly one connector block
        // may be present, and it must be the one the `kind` names.
        self.validate_connector_blocks()?;
        // #1164 §3 — and the app-only surfaces are refused at PARSE time for
        // connectors, which is what §4's interception table rests on.
        self.reject_app_only_surfaces()?;

        // `entrypoint` is required only for `app`. Non-app kinds have no
        // kernel-supervised child, so demanding a binary path there would be
        // pure ceremony (and would force fake values into the manifest).
        match self.entrypoint.as_ref() {
            Some(entrypoint) => {
                if entrypoint.command.trim().is_empty() {
                    return Err(ManifestError::invalid(
                        "entrypoint.command",
                        "must be non-empty",
                    ));
                }
                // Reject absolute paths and `..` escapes early — Slice B will
                // also re-check, but flagging here gives users a clearer error.
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

        // #1110 S2 — wave `template_input` lives on the Manifest, not a
        // template descriptor. Error paths are `input_schema…` (no
        // `templates[i].` prefix).
        if let Some(schema) = self.input_schema.as_ref() {
            crate::plugin_host::template_input::validate_input_schema(schema)
                .map_err(|e| ManifestError::invalid(e.path, e.reason))?;
        }

        // #1284 §2.1 — same subset, different Manifest field, and therefore a
        // different error root: a `config_schema` violation must say
        // `config_schema…`, never `input_schema…`. That is the whole reason
        // `validate_object_schema` takes a root path.
        if let Some(schema) = self.config_schema.as_ref() {
            crate::plugin_host::template_input::validate_object_schema(CONFIG_SCHEMA_KEY, schema)
                .map_err(|e| ManifestError::invalid(e.path, e.reason))?;

            // …and the conditional version bump. Scoped to schemas that carry
            // a non-empty `required` for the reason spelled out on
            // `manifest_version`: only those lose something real when a
            // pre-#1284 kernel ignores the key. `required` is already known to
            // be a non-empty array of declared property names at this point —
            // the subset validator above ran first.
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

// ---------------------------------------------------------------------------
// #1164 — connector-block validation
// ---------------------------------------------------------------------------

impl Manifest {
    /// Enforce the §2.1 contract: the two connector blocks are mutually
    /// exclusive, and the present block must match `kind`. We deliberately do
    /// NOT use `#[serde(flatten)]` + an internally-tagged enum — the duplicate
    /// `kind` key that shape produces is a round-trip hazard.
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
                block.validate()?;
            }
            ConnectorKind::CliQuery => {
                let block = self.cli_query.as_ref().ok_or_else(|| {
                    ManifestError::invalid("cli_query", "required when `kind` is \"cli-query\"")
                })?;
                block.validate()?;
            }
        }
        Ok(())
    }

    /// #1164 §3 — **parse-time** refusal of every `app`-only surface on a
    /// connector manifest.
    ///
    /// §3 lists "渲染 `ui://` 或绑 `templates[]`（parse 期拒绝）" as a channel
    /// that *does not exist* for connectors, and §4's interception table is
    /// only sound if the manifest can never declare one. Enforcing it here —
    /// rather than by hoping no downstream reader ever looks — is what makes
    /// the negative durable: `Manifest::parse` is the single door every
    /// manifest enters through (`registry::load_from_dir`, the install route,
    /// `/reload`), so a connector manifest that reaches any reader is already
    /// known to carry none of these.
    ///
    /// Each field gets its own error naming the field, because "your connector
    /// manifest is invalid" is useless to whoever authored it.
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
                only_app("cannot own a wave template"),
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
        // D6 — a forge action is dispatched with the forge credential
        // passthrough, which is an `app`-plugin-only channel. It is not
        // exploitable in P1 (every reader gates on `running_plugin_ids`, and a
        // successful `mcp-http` spawn REPLACES `exposes_tools` wholesale with
        // `materialize_http_tools`, which hard-codes `kind: None`) — but "not
        // reachable today" is exactly the invariant P3's `cli-query` executor
        // would silently make live. Refusing at parse time makes it durable
        // rather than incidental.
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
    fn validate(&self) -> Result<(), ManifestError> {
        validate_mcp_http_url(self.url.trim())?;
        // The ceiling is enforced where the manifest is PARSED, not where the
        // spawn reads it: a connector whose bring-up budget would stall boot
        // must fail to load, so an operator learns at install time rather than
        // by watching the server take minutes to answer its first request.
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
            (Some(_), Some(spec)) => match ApiKeyIn::parse(spec) {
                None => {
                    return Err(ManifestError::invalid(
                        "mcp_http.api_key_in",
                        "must be `query:<name>` or `header:<name>`",
                    ));
                }
                // A header name that is not an RFC 9110 field-name would be
                // rejected by the HTTP client at REQUEST time — i.e. once per
                // call, as a transport error, long after the operator could
                // connect it to the manifest they wrote.
                Some(ApiKeyIn::Header(name)) if !is_http_field_name(&name) => {
                    return Err(ManifestError::invalid(
                        "mcp_http.api_key_in",
                        format!(
                            "`{name}` is not a legal HTTP header name \
                             (RFC 9110 token: alphanumerics and any of `!#$%&'*+-.^_`|~`)"
                        ),
                    ));
                }
                // A query parameter name with characters that would have to be
                // percent-encoded is legal but confusing; `=` and `&` would
                // actively re-shape the query, so refuse them outright.
                Some(ApiKeyIn::Query(name))
                    if name.contains(['=', '&', '#', '?'])
                        || name.contains(char::is_whitespace) =>
                {
                    return Err(ManifestError::invalid(
                        "mcp_http.api_key_in",
                        format!(
                            "query parameter name `{name}` must not contain \
                             whitespace or any of `=`, `&`, `#`, `?`"
                        ),
                    ));
                }
                Some(_) => {}
            },
            (None, _) => {}
        }
        for (i, name) in self.tools_allow.iter().enumerate() {
            validate_connector_tool_name(name, &format!("mcp_http.tools_allow[{i}]"))?;
        }
        Ok(())
    }
}

impl CliQueryBlock {
    fn validate(&self) -> Result<(), ManifestError> {
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
        // #1164 P3 F1 — `env_allow` is a passthrough from the SERVICE
        // environment, so a manifest that names a forge credential key would
        // hand a manifest-authored, agent-callable connector the operator's git
        // identity. Refused at parse time, which is the earliest and loudest
        // place: install and reload both go through here, so such a manifest
        // never becomes an enabled connector at all. `build_child_env` keeps a
        // fail-closed filter for anything that reaches the runtime by another
        // route.
        //
        // The denylist is the CREDENTIAL subset only (r2 G4). The wider forge
        // passthrough set also carries `GH_HOST`/`NO_PROXY`/`no_proxy`, which
        // grant nothing: refusing them made `"env_allow": ["no_proxy"]` — an
        // ordinary need for a query CLI behind a proxy — a hard install failure
        // whose reason falsely called it a credential, while `HTTP_PROXY` sailed
        // through. Since `registry::load_from_dir` re-parses on boot, every key
        // in this set can also retroactively invalidate an installed manifest,
        // which is a cost only a real credential is worth paying.
        //
        // `secret_env` is deliberately NOT subject to this list either: those
        // values come from the connector's own `secrets.json`, which the
        // operator authored for this connector. Naming `GH_TOKEN` there sets it
        // to whatever the operator put in that file — there is no escalation
        // from the service identity, which is the thing this denylist protects.
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
        for (i, tool) in self.tools.iter().enumerate() {
            tool.validate(i)?;
        }
        Ok(())
    }
}

impl CliQueryTool {
    fn validate(&self, idx: usize) -> Result<(), ManifestError> {
        let path = |s: &str| format!("cli_query.tools[{idx}].{s}");
        validate_connector_tool_name(&self.name, &path("name"))?;

        // Slot names must be declared top-level keys of `input_schema`.
        // `input_schema` is the same JSON-Schema subset the rest of the
        // manifest uses; here we only need its top-level property names.
        let properties = self
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object());
        for (i, arg) in self.args.iter().enumerate() {
            let Some(slot) = argv_slot(arg) else {
                // A literal argv element. Reject stray braces so a typo like
                // `--sym={{symbol}}` fails at authoring time instead of being
                // silently passed through as a literal.
                if arg.contains("{{") || arg.contains("}}") {
                    return Err(ManifestError::invalid(
                        path(&format!("args[{i}]")),
                        "a `{{slot}}` template must occupy the whole argv element \
                         (no string concatenation, no shell)",
                    ));
                }
                continue;
            };
            let known = properties.is_some_and(|p| p.contains_key(slot));
            if !known {
                return Err(ManifestError::invalid(
                    path(&format!("args[{i}]")),
                    format!(
                        "slot `{slot}` is not a top-level property of this tool's input_schema"
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// #1164 §2.2 — real parse of `mcp_http.url`.
///
/// The previous check was `starts_with("http://") || starts_with("https://")`,
/// which accepted a bare `https://`, a malformed authority, and — the one that
/// is actively harmful — a **fragment**. `HttpMcpClient::new` appends the query
/// auth AFTER whatever the manifest said, so `https://h/mcp#x` becomes
/// `https://h/mcp#x?api_key=…`: the key lands inside the fragment and is never
/// transmitted, and the connector fails authentication with no hint why.
///
/// Rejecting at manifest-parse time means the failure names the field.
fn validate_mcp_http_url(raw: &str) -> Result<(), ManifestError> {
    let field = "mcp_http.url";
    // WHATWG normalization is too forgiving for a *manifest*: it turns
    // `https:///mcp` into host `mcp`, silently retargeting the request at a
    // host the author never wrote. Require a non-empty authority in the RAW
    // text before handing it to the parser.
    match raw.split_once("://") {
        Some((_, rest)) if !rest.split(['/', '?', '#']).next().unwrap_or("").is_empty() => {}
        _ => {
            return Err(ManifestError::invalid(
                field,
                "must be `http://<host>[…]` or `https://<host>[…]` with a non-empty authority",
            ));
        }
    }
    // The authority pre-check above only knows `/`, `?` and `#` as delimiters,
    // but WHATWG treats a BACKSLASH as a path separator and STRIPS ASCII tabs
    // and newlines before parsing. So `https://\evil.example/mcp` and
    // `https://good.example\t.evil.example/mcp` both sail past it and are then
    // normalized into a different textual target — while `HttpMcpClient`'s
    // `log_target` splits the UNNORMALIZED raw string and would report the host
    // the author wrote rather than the one we would actually contact.
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
    // Scheme FIRST. WHATWG lower-cases the scheme, so `FILE://x/mcp` is
    // non-canonical *and* unsupported — and reporting "must be written in
    // canonical form" would send the author off to fix the capitalisation of a
    // scheme this connector will never accept.
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ManifestError::invalid(
            field,
            format!(
                "scheme must be `http` or `https`, got `{}`",
                parsed.scheme()
            ),
        ));
    }
    // The robust form of the same rule: whatever else the parser did to this
    // string, the manifest must have been written in canonical form. Anything
    // that re-serializes differently is a URL whose textual target is not the
    // one the author wrote — and this crate has TWO consumers of the string
    // (ureq, which re-parses it, and `log_target`, which does not), so a
    // manifest where those disagree is exactly what must not exist.
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
            "must not carry a `#fragment`: the API key is appended to the query \
             string after it, so it would never be transmitted",
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

/// RFC 9110 `field-name` = `token`. Used to reject an `api_key_in:
/// header:<name>` the HTTP client would refuse at request time.
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

/// Shared name check for connector-supplied tools. There is no
/// `ExposedTool::validate` in the tree (§2.7), so materialization and manifest
/// parsing both route through this one predicate.
///
/// `_` is rejected because `plugin.<id>_<tool>` uses the FIRST `_` as the
/// id↔tool boundary only by virtue of plugin ids excluding `_`; a tool name
/// containing `_` is fine, but an EMPTY or whitespace name would synthesize an
/// unroutable descriptor. `.` is fine in tool names.
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

    const ISSUE_DEVELOPMENT_RENDERED_PROMPT_GOLDEN: &str =
        include_str!("../../tests/goldens/issue_development_spec_prompt.txt");

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

    /// #1268 — the rename's one silent failure mode, made loud.
    ///
    /// `Manifest` tolerates unknown top-level keys (see
    /// `extra_template_descriptor_fields_are_ignored` for the *descriptor*
    /// half of the same forwards-compat rule), so a manifest still spelling
    /// the array `workflows` would otherwise parse into
    /// `templates: []` — the plugin would declare no binding at all, and the
    /// only symptom would be `issue-development` losing its `input_schema`
    /// and every `template_input` create 400-ing far from the cause.
    ///
    /// Both halves are asserted: the old key is refused **and** the error
    /// names the new one, because "invalid manifest" alone would not tell the
    /// author what to type. Deleting `reject_retired_workflows_key` turns the
    /// first assertion red; weakening its message turns the second red.
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

    /// #1268 — the **rollback** direction, which the retired-key guard cannot
    /// reach.
    ///
    /// The guard runs in *this* kernel, so it protects an operator moving
    /// forward. Moving backward, a `templates[]` manifest is handed to a
    /// pre-#1268 parser that ignores unknown top-level keys: it parses clean,
    /// binds nothing, and `issue-development` loses its `input_schema` with no
    /// error and no log. The only thing an old kernel *will* refuse on its own
    /// is a `manifest_version` it does not know — so a manifest that declares a
    /// binding is required to say `2`, and that requirement is what this pins.
    ///
    /// The error names the field rather than the array, because the fix is to
    /// the version line.
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

    /// The scope of that rule, stated as a test rather than a comment: a
    /// manifest with **no** bindings is untouched by #1268 and keeps loading at
    /// version 1.
    ///
    /// This is not symmetry for its own sake. The plugin install root on a real
    /// deployment holds connector manifests that never declared a binding, and
    /// the boot loader turns a parse failure into `warn!` + skip — so
    /// tightening the rule to "every manifest must say 2" would silently drop
    /// working plugins, which is the same failure shape #1268 exists to remove.
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

        // An explicitly empty array is the same case: nothing to lose on
        // rollback, so it must not be treated as "declares a binding".
        let mut empty = template_manifest_value();
        empty["manifest_version"] = json!(1);
        empty["templates"] = json!([]);
        parse_manifest_value(empty).expect("an empty `templates` array declares no binding");
    }

    /// Version 2 is the current epoch and parses with its bindings intact.
    #[test]
    fn version_2_with_templates_parses() {
        let m = parse_manifest_value(template_manifest_value()).expect("v2 manifest");
        assert_eq!(m.manifest_version, 2);
        assert_eq!(m.templates[0].id, "issue-development");
    }

    /// The shipped plugin is on the current epoch — otherwise the rule above
    /// would be pinned only by hand-built fixtures while the one manifest that
    /// actually ships stayed on the retired one.
    #[test]
    fn the_shipped_git_forge_manifest_declares_version_2() {
        let m = Manifest::parse(include_str!("../../../../plugins/git-forge/manifest.json"))
            .expect("shipped git-forge manifest");
        assert_eq!(m.manifest_version, 2);
        assert!(!m.templates.is_empty());
    }

    /// The other direction: nothing about the check makes an ordinary unknown
    /// top-level key fatal. Only the one retired spelling is.
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
        v["templates"][0]["spec_instructions"] = json!("leftover");
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

        // #1110 S2 — the shipped plugin's input contract lives on the
        // Manifest, not the template descriptor. Parsing via
        // `Manifest::parse` already ran `validate()`, so reaching here
        // proves the schema passes the subset validator.
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
        // F8: integer-encoded only — the type must be the strict "integer".
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
            crate::operation::spec_harness_start_adapter::render_spec_developer_instructions(
                "wave-give-up",
                Some(&template),
                None,
            );
        crate::spec_card::validate_spec_prompt_contract(&rendered)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            !rendered.contains("If n == cap and the round is non-approving"),
            "S5 descriptor has no spec_instructions to inject"
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

        // The fixed fixture id is the explicit normalization rule for the
        // per-wave substitution performed by the production renderer.
        // This independent, fully populated fixture is a legal final state for
        // the shipped schema and keeps every required and optional field in the
        // full-prompt contract.
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
            crate::operation::spec_harness_start_adapter::render_spec_developer_instructions(
                "wave-golden-985",
                Some(template),
                Some(&template_input),
            );

        if std::env::var_os("REGEN_SPEC_PROMPT_GOLDEN").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/goldens/issue_development_spec_prompt.txt");
            // Write back `rendered + "\n"`: the assertion side does
            // `strip_suffix('\n')`, so omitting it panics on the very next run.
            std::fs::write(&path, format!("{rendered}\n")).expect("write regenerated golden");
            panic!(
                "issue_development_spec_prompt.txt regenerated from the current prompt; \
                 hand-verify the diff, commit, and re-run without REGEN_SPEC_PROMPT_GOLDEN"
            );
        }

        let expected = ISSUE_DEVELOPMENT_RENDERED_PROMPT_GOLDEN
            .strip_suffix('\n')
            .expect("text fixture has its repository newline");
        assert_full_golden_eq(expected, &rendered);
    }

    #[test]
    fn template_descriptor_rejects_invalid_shapes() {
        let cases: Vec<(&str, Value, &str)> = vec![
            ("empty id", json!(""), "templates[0].id"),
            ("bad id", json!("Bad Id"), "templates[0].id"),
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

    /// #891 / #1110 S2 — the subset validator runs at manifest parse;
    /// exhaustive keyword/coherence coverage lives in
    /// `plugin_host::template_input` (this pins the top-level
    /// `input_schema…` field-path wiring).
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

    // -----------------------------------------------------------------------
    // #1284 S1 — `config_schema`
    // -----------------------------------------------------------------------

    /// All-optional config schema: nothing is lost on a pre-#1284 kernel, so
    /// it stays legal at `manifest_version: 2`.
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

    /// The reason `validate_object_schema` had to take a root path (#1284 §2.1
    /// / F8): every one of these violations used to be reportable only as
    /// `input_schema…`, i.e. against a field this manifest does not have.
    ///
    /// Paired with `manifest_rejects_out_of_subset_input_schema` above, which
    /// keeps proving the *other* root still says `input_schema`.
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

    /// #1284 §2.1 — the conditional bump, in all three cells that decide it.
    ///
    /// A pre-#1284 kernel ignores `config_schema` outright. For an all-optional
    /// schema that is a faithful degradation (every key falls back to what its
    /// default already meant), so `2` keeps working. For a schema with
    /// `required` it is not: the plugin would run with none of its mandatory
    /// configuration and say nothing, so `3` is demanded — and on the old
    /// kernel the file is refused by version and the plugin disappears loudly.
    #[test]
    fn config_schema_with_required_demands_manifest_version_3() {
        let mut required_schema = optional_config_schema();
        required_schema["required"] = json!(["theme"]);

        // (a) required + version 2 ⇒ rejected, and it is the VERSION that is
        // named, not the schema.
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

        // (c) all-optional + version 2 ⇒ accepted, i.e. the rule really is
        // conditional and not "config_schema ⇒ 3".
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(2);
        v["config_schema"] = optional_config_schema();
        let m = parse_manifest_value(v).expect("optional-only config at v2 is accepted");
        assert_eq!(m.manifest_version, 2);
    }

    /// An empty `required: []` is not a required key. Pinned separately
    /// because "declares `required`" and "has required keys" are the kind of
    /// pair that quietly becomes the same predicate.
    #[test]
    fn an_empty_required_array_does_not_demand_version_3() {
        let mut schema = optional_config_schema();
        schema["required"] = json!([]);
        let mut v = template_manifest_value();
        v["manifest_version"] = json!(2);
        v["config_schema"] = schema;
        parse_manifest_value(v).expect("`required: []` has nothing to lose on an old kernel");
    }

    /// [`CONFIG_SCHEMA_KEY`] is the root every `config_schema` diagnostic is
    /// reported under, so it has to name a key that really exists on the wire:
    /// renaming the serde field without renaming the constant would leave
    /// operators reading error paths for a field their manifest does not have.
    /// Pinned to what a real `Manifest` actually serializes to — in both
    /// directions, because `skip_serializing_if` means absence is also
    /// observable (that is the shape `PluginDetail.manifest` publishes).
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

        // …and absence really is absence (skip_serializing_if), which is what
        // `has_config: false` reads.
        let blob = parse_manifest_value(template_manifest_value())
            .expect("valid")
            .to_json();
        assert!(blob.get(CONFIG_SCHEMA_KEY).is_none());
    }

    #[test]
    fn missing_required_field_fails() {
        // `display_name` missing entirely — still an unconditionally required
        // field, so serde rejects it before any validator runs.
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

    /// #1164 §2.1 moved `entrypoint` from "unconditionally required" (a serde
    /// error) to "required for `kind: app`" (a validator error). It is still
    /// rejected for an app manifest — only the error variant changed.
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
        // #1268 widened the accepted set to {1, 2} and #1284 to {1, 2, 3}, so
        // the "unknown epoch" case has to be probed on both sides of it — a
        // single sample above the range would stay green if the check were
        // rewritten as `>= 1`. (`3` moved from this list to
        // `config_schema_with_required_demands_manifest_version_3` when #1284
        // made it a real version.)
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

    /// #1297: `kernel` satisfies the id regex, so without an explicit refusal
    /// a plugin could register under it and — since the callback path writes
    /// `ctx.plugin_id` verbatim — mint rows indistinguishable from the ones
    /// `card_fsm` authors. The REST gate cannot see this route at all.
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

// ===========================================================================
// #1164 §2.1 — connector kind + mutually-exclusive blocks
// ===========================================================================

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
            "api_key_in": "query:api_key",
            "tools_allow": ["list_reports"],
            "request_timeout_ms": 10000,
        })
    }

    /// `expect_err` with the case identity in the panic message — with a dozen
    /// table rows, "called `Result::unwrap_err()` on an `Ok` value" alone does
    /// not say WHICH row silently passed.
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

    /// #1284 — `config_schema` is deliberately **not** on the app-only list:
    /// S2/S3a/S3b give all three kinds a consumer, and the connector kinds are
    /// where operator-supplied configuration is most obviously needed
    /// (endpoints, argv values, env). Paired with the `input_schema` half,
    /// which stays app-only — without that half this test would still pass if
    /// `reject_app_only_surfaces` were deleted outright.
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

    // ---- kind defaulting -------------------------------------------------

    /// The tree's only checked-in manifest carries no `kind` key. Absent must
    /// mean `app`, with zero change to app semantics.
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

    /// Risk R7: an unknown `kind` must be a loud parse error, never a silent
    /// downgrade to `app` (which would spawn a process for a manifest that
    /// describes something else entirely).
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
        // Exactly one `kind` key on the wire — the reason we did not use
        // `#[serde(flatten)]` with an internally-tagged enum (D4).
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

    // ---- entrypoint conditionality ---------------------------------------

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

    // ---- kind ↔ block consistency ----------------------------------------

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

    // ---- §3: app-only surfaces are refused at PARSE time ------------------

    /// Every field in this table is a channel §3 says does not exist for a
    /// connector. The interception table in §4 is only sound if a connector
    /// manifest can never declare one, and `Manifest::parse` is the single
    /// door every manifest enters through.
    ///
    /// One case per field, and the error must NAME the field — "your manifest
    /// is invalid" is useless to whoever authored it.
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

    /// The same manifest MINUS the offending field must parse — otherwise the
    /// test above would pass for the wrong reason.
    #[test]
    fn a_connector_without_app_only_surfaces_parses() {
        for (kind, block_key, block) in [
            ("mcp-http", "mcp_http", mcp_http_block()),
            ("cli-query", "cli_query", cli_query_block()),
        ] {
            Manifest::parse(&base(json!({ "kind": kind, block_key: block })))
                .unwrap_or_else(|e| panic!("{kind} must parse: {e}"));
        }
        // An explicitly-present but all-default `permissions` block is not a
        // request for anything, so it must NOT be refused.
        Manifest::parse(&base(json!({
            "kind": "mcp-http",
            "mcp_http": mcp_http_block(),
            "permissions": { "proposals": ["legacy"] },
        })))
        .expect("an all-default permissions block grants nothing");
    }

    /// `app` manifests are untouched by §3 — the anti-regression half.
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

    // ---- mcp_http block --------------------------------------------------

    #[test]
    fn mcp_http_url_must_be_absolute_http() {
        let mut block = mcp_http_block();
        block["url"] = json!("mcp.example.com/mcp");
        let err = Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block })))
            .expect_err("relative url rejected");
        assert!(err.to_string().contains("mcp_http.url"), "{err}");
    }

    /// Prefix matching on `http://` accepted all of these. The FRAGMENT case
    /// is the one that is actively harmful: `HttpMcpClient::new` appends
    /// `?api_key=…` after whatever the manifest said, so the credential lands
    /// inside the fragment and is never transmitted.
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

    /// Round-2 finding: the raw-authority pre-check only knows `/`, `?` and
    /// `#` as delimiters, so WHATWG's OTHER authority terminators walked past
    /// it. Each of these parses to a host the manifest author did not write,
    /// while `HttpMcpClient::log_target` — which splits the UNNORMALIZED raw
    /// string — would report the host they did.
    #[test]
    fn mcp_http_url_rejects_whatwg_retargeting() {
        // Each entry is `(raw, host the parser actually resolves it to)`, so
        // the fixture proves the retargeting is real rather than asserting a
        // refusal that might be firing for an unrelated reason.
        for (raw, retargeted_host) in [
            (r"https://\evil.example/mcp", "evil.example"),
            (r"https:/\evil.example/mcp", "evil.example"),
            // Tab/CR/LF are STRIPPED, not treated as delimiters: the two
            // labels fuse into one host that appears nowhere in the manifest.
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
            // The premise: `url` really does resolve this somewhere other than
            // the literal authority a naive reader (and `log_target`) sees.
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

    /// Canonical-form equality is the robust half of the same rule: whatever
    /// the parser normalizes, the manifest must have been written that way, so
    /// the string we contact and the string we log are provably the same.
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
        // …and the canonical spellings of the same URLs are accepted, so the
        // rule is "write it canonically", not "we reject these hosts".
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

    /// Round-3 finding: the canonical-form check used to run BEFORE the scheme
    /// check, so an unsupported scheme spelled in upper case was reported as a
    /// formatting problem. `FILE://x/mcp` normalizes to `file://x/mcp`, which
    /// is non-canonical *and* unsupported — the author must be told the scheme
    /// is wrong, not that they capitalised it wrong.
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

    /// D6 — a forge action rides the forge credential passthrough, an
    /// `app`-only channel. Refused at parse time so P3's `cli-query` executor
    /// cannot quietly make it live.
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
        // The same manifest without the forge-action tool parses — otherwise
        // the assertion above could be passing for an unrelated reason.
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

    /// An illegal header name would otherwise fail at REQUEST time, once per
    /// call, as an opaque transport error.
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

    #[test]
    fn query_api_key_name_must_not_reshape_the_query() {
        for bad in ["a=b", "a&b", "a?b", "a#b", "a b"] {
            let mut block = mcp_http_block();
            block["api_key_in"] = json!(format!("query:{bad}"));
            let err = expect_reject(
                Manifest::parse(&base(json!({ "kind": "mcp-http", "mcp_http": block }))),
                bad,
            );
            assert!(err.to_string().contains("api_key_in"), "`{bad}`: {err}");
        }
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
    fn api_key_in_parses_both_locations() {
        assert_eq!(
            ApiKeyIn::parse("query:api_key"),
            Some(ApiKeyIn::Query("api_key".into()))
        );
        assert_eq!(
            ApiKeyIn::parse("header:x-api-key"),
            Some(ApiKeyIn::Header("x-api-key".into()))
        );
        assert_eq!(ApiKeyIn::parse("query:"), None);
        assert_eq!(ApiKeyIn::parse("cookie:k"), None);
        assert_eq!(ApiKeyIn::parse("api_key"), None);
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

    /// The A-fix: the two budgets are separate, and the bring-up one is bounded
    /// **by construction** — including when it is derived from the unbounded
    /// call timeout.
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

        // …and is clamped as soon as that timeout stops being a sane boot
        // budget. This is the case that used to stall `AppState::new` for
        // 20.5 minutes.
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

    /// …and one over the ceiling is refused at PARSE time, so the operator
    /// learns at install rather than by watching boot crawl.
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

    // ---- cli_query block -------------------------------------------------

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
        assert_eq!(argv_slot("{{symbol}}"), Some("symbol"));
        assert_eq!(argv_slot("--sym={{symbol}}"), None);
        assert_eq!(argv_slot("{{}}"), None);
        assert_eq!(argv_slot("quote"), None);
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
