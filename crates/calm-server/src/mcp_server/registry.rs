//! Tool registry + per-connection app context for the kernel's MCP
//! server. PR7a (#136).
//!
//! ## What lives here
//!
//! [`ToolRegistry`] is a name -> handler map the transport consults on
//! every `tools/call`. Tool modules register their descriptors and handlers
//! here so the transport can route calls by name and expose role-filtered
//! discovery.
//! Each handler is `Send + Sync + 'static` and receives:
//!
//!   * an [`AppContext`] — repo, event bus, role cache, and the codex
//!     home parent (already on `AppState`, factored down to the minimum
//!     surface the MCP server needs so the registry doesn't take a
//!     full `AppState` clone);
//!   * a [`ToolCallIdentity`] — resolved for each `tools/call` either
//!     from `_meta.threadId` via runtime thread attribution or, for an
//!     explicitly [`ConnectionIdentity::CardBound`] connection with no
//!     thread metadata, from the bound card identity.

use crate::db::RouteRepo;
use crate::event::EventBus;
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::mcp_server::framing::RpcError;
use crate::model::CardRole;
use crate::session_projection_repo::AgentProvider;
use crate::state::WriteContext;
use calm_truth::wave_vcs_repo::WaveVcsRepo;
use calm_types::worker::{Principal, WorkerSessionId};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The card identity bound to a single MCP connection. Established at
/// handshake time from the presented MCP token's `card_mcp_tokens` row
/// and the card's row in `cards.role`.
///
/// `Clone` is cheap (small inline `CardId` newtype + a `Copy` role).
#[derive(Clone, Debug)]
pub struct CardIdentity {
    pub card_id: CardId,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: String,
    pub wave_id: Option<String>,
    pub cove_id: String,
}

impl CardIdentity {
    /// Convert to the `ActorId` the role gate will see for any
    /// `write_with_event` call this connection drives. MCP writes are
    /// keyed by worker session, not card id. Mapping:
    ///
    /// * `CardRole::Spec`       → [`ActorId::AiSpecSession`]
    /// * `CardRole::Worker`     → provider-specific AI session actor
    /// * `CardRole::ReportCard` → unreachable here too (report cards are
    ///   read-only kernel-projected payload and don't get an MCP token).
    ///   Mapped by provider for the same total-function reason — the role
    ///   gate refuses report-card actors as soon as they try to emit
    ///   `WaveUpdated`, so a token-row leak surfaces as a clear `Forbidden`
    ///   rather than a panic.
    pub fn to_actor_id(&self) -> ActorId {
        let session_id = WorkerSessionId::from(self.session_id.clone());
        match self.role {
            CardRole::Spec => ActorId::AiSpecSession(session_id),
            CardRole::Worker | CardRole::ReportCard => {
                provider_session_actor(&self.provider, session_id)
            }
        }
    }

    pub fn to_principal(&self) -> Option<Principal> {
        let wave_id = self.wave_id.as_ref()?;
        Some(Principal::Agent {
            session_id: WorkerSessionId::from(self.session_id.clone()),
            wave_id: WaveId::from(wave_id.clone()),
            cove_id: CoveId::from(self.cove_id.clone()),
        })
    }
}

/// Identity mode established once by the MCP `initialize` handshake.
///
/// Daemon-trust connections are not bound to any card and must provide a
/// resolvable `_meta.threadId` on each `tools/call`. Card-bound connections
/// are authenticated by a per-card MCP token; they may omit `threadId`, but
/// any supplied `threadId` must resolve back to the same card.
#[derive(Clone, Debug)]
pub enum ConnectionIdentity {
    DaemonTrust,
    CardBound(CardIdentity),
}

/// Identity resolved for one MCP `tools/call` from the request's
/// `_meta.threadId`, or from a card-bound connection when `_meta.threadId`
/// is absent. In that card-bound no-thread case `thread_id` is the literal
/// `"card-bound"` sentinel because no persisted runtime thread row was
/// involved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallIdentity {
    pub card_id: String,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: String,
    pub wave_id: Option<String>,
    pub cove_id: String,
    pub thread_id: String,
}

impl ToolCallIdentity {
    /// Convert to the `ActorId` the role gate will see for this MCP tool
    /// call. MCP writes are keyed by worker session, not card id. Mapping:
    ///
    /// * `CardRole::Spec`       → [`ActorId::AiSpecSession`]
    /// * `CardRole::Worker`     → provider-specific AI session actor
    /// * `CardRole::ReportCard` → provider-specific total-function fallback;
    ///   write gates still reject report-card writes.
    pub fn to_actor_id(&self) -> ActorId {
        let session_id = WorkerSessionId::from(self.session_id.clone());
        match self.role {
            CardRole::Spec => ActorId::AiSpecSession(session_id),
            CardRole::Worker | CardRole::ReportCard => {
                provider_session_actor(&self.provider, session_id)
            }
        }
    }

    pub fn to_principal(&self) -> Option<Principal> {
        let wave_id = self.wave_id.as_ref()?;
        Some(Principal::Agent {
            session_id: WorkerSessionId::from(self.session_id.clone()),
            wave_id: WaveId::from(wave_id.clone()),
            cove_id: CoveId::from(self.cove_id.clone()),
        })
    }
}

fn provider_session_actor(provider: &AgentProvider, session_id: WorkerSessionId) -> ActorId {
    match provider {
        AgentProvider::Codex => ActorId::AiCodexSession(session_id),
        AgentProvider::Claude => ActorId::AiClaudeSession(session_id),
    }
}

/// PR7b (#136) — soft role gate for spec-only MCP tools.
///
/// The *real* boundary is [`crate::role_gate::enforce_role`], which runs
/// inside every eventized write and refuses any cross-role attempt with
/// a transactional rollback. This helper is purely UX: it short-circuits
/// at the MCP-tool entry so a worker card calling a spec-only tool such
/// as `calm.task.verdict` gets a deterministic `-32602 spec-only tool`
/// error code instead of the more opaque `-32403 forbidden` that the
/// in-tx gate would otherwise produce after speculatively reading rows.
///
/// Use at the top of every spec-only handler. `calm.wave.state` is
/// callable by both Spec and Worker (a worker may need to peek wave
/// metadata before reporting), so it uses [`require_role_any`] instead.
pub fn require_role(identity: &ToolCallIdentity, required: CardRole) -> Result<(), RpcError> {
    if identity.role != required {
        return Err(RpcError::custom(
            RpcError::INVALID_PARAMS,
            format!(
                "tool requires role={required:?} got={got:?}",
                got = identity.role
            ),
        ));
    }
    Ok(())
}

/// Variant of [`require_role`] for read-only tools shared by a small
/// fixed set of roles.
pub fn require_role_any(identity: &ToolCallIdentity, allowed: &[CardRole]) -> Result<(), RpcError> {
    if allowed.contains(&identity.role) {
        return Ok(());
    }
    Err(RpcError::custom(
        RpcError::INVALID_PARAMS,
        format!(
            "tool requires role in {allowed:?} got={got:?}",
            got = identity.role
        ),
    ))
}

/// Per-process context every tool handler reads from. Built once at
/// `McpServer::spawn` and `Arc`-cloned into each per-connection task.
///
/// Held by value-`Arc` everywhere — handler closures need
/// `Send + Sync + 'static`, and `Arc<AppContext>` satisfies that
/// without forcing each handler to clone every field individually.
#[derive(Clone)]
pub struct AppContext {
    /// Eventized writes route through this. Same `RouteRepo` upcast as
    /// `AppState::repo`, so the dyn-trait gate is preserved (no
    /// sync-domain raw writes reachable from a tool handler).
    pub repo: Arc<dyn RouteRepo>,
    /// Read-only wave-vcs drill-ins need the sqlite-backed audit tables.
    /// This is built from the full internal repo at MCP server spawn time
    /// instead of widening the route-facing repo trait.
    pub wave_vcs: Option<Arc<dyn WaveVcsRepo>>,
    /// Event bus for `write_with_event_typed` broadcasts.
    pub events: EventBus,
    /// #480 PR2 write-surface caches shared with REST/worker paths.
    pub write: WriteContext,
    /// Optional server-wide MCP daemon token hash. When present, the
    /// initialize handshake accepts this as daemon trust without binding a
    /// legacy per-card identity.
    pub daemon_token_hash: Option<String>,
    /// Issue #644 PR-C (PR #685 F3) — the CONFIGURED gate-logs dir
    /// (`Config::data_dir_resolved()/gate-logs`), threaded from
    /// `AppState::new` so the `plan/<key>/gate.log` view reads the same
    /// directory the gate runner writes — a `--data-dir` flag without
    /// `CALM_DATA_DIR` must not split the two.
    pub gate_logs_dir: std::path::PathBuf,
    /// Late-bound plugin host handle. MCP server boot intentionally happens
    /// before plugin host construction; plugin-tool discovery/routing reads
    /// this cell at dispatch time once `AppState::new` has populated it.
    pub plugin_host: Arc<tokio::sync::OnceCell<Arc<crate::plugin_host::PluginHost>>>,
    /// Late-bound operation runtime handle. Plugin forge-action tools need to
    /// submit durable operations, but MCP boot precedes runtime construction.
    pub operation_runtime: Arc<tokio::sync::OnceCell<Arc<crate::operation::OperationRuntime>>>,
}

/// Boxed future returned by a tool handler. Handlers are async fns;
/// `BoxFuture<'static, …>` keeps the registry's hash-map values
/// object-safe.
pub type ToolHandlerFuture =
    Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send + 'static>>;

/// One tool's invocation contract. The transport calls this with the
/// per-call [`ToolCallIdentity`] and the raw `arguments` JSON value from
/// the `tools/call` params. Handlers are responsible for shape-validating
/// `arguments` and translating internal errors into [`RpcError`].
pub type ToolHandler =
    Arc<dyn Fn(Arc<AppContext>, ToolCallIdentity, Value) -> ToolHandlerFuture + Send + Sync>;

/// `tools/list` descriptor — the JSON shape codex's MCP client expects.
/// We store the description + the JSON schema for `inputSchema` here
/// so a future `tools/list` request can fold the registry's contents
/// into a single response without a second registration pass.
#[derive(Clone)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    /// Pre-built JSON schema for the tool's `arguments` object. We
    /// store a `Value` (rather than a struct) because the MCP spec
    /// accepts the schema verbatim — no need to round-trip through a
    /// typed schema crate for three small handlers.
    pub input_schema: Value,
    /// Optional MCP `annotations` block, surfaced verbatim in `tools/list`.
    /// Codex 0.13x reads `readOnlyHint`/`destructiveHint`/`openWorldHint`
    /// from this to decide whether the tool needs explicit approval; missing
    /// annotations default to "approval required" (codex
    /// `mcp_tool_call.rs:1953`). Set explicitly per tool to avoid that
    /// default landing on every call.
    pub annotations: Option<Value>,
    /// Which roles see this tool in `tools/list`. Wire-level `tools/call`
    /// still routes by name regardless - this only controls discovery.
    /// Default to spec-only for write tools; explicit `&[]` for tools that
    /// must not appear in any role's tools/list (e.g. read tools served
    /// only via `neige` CLI, worker self-reports invoked only by `neige`).
    /// See issue #588.
    pub visible_to_roles: &'static [CardRole],
}

pub fn read_only_annotations() -> Value {
    json!({ "readOnlyHint": true })
}

/// MCP `annotations` block for write tools whose access is already gated
/// by `require_role(...)` inside the kernel. Codex's
/// `requires_mcp_tool_approval()` (mcp_tool_call.rs:1953) short-circuits
/// on these three keys: `destructiveHint: false` + `openWorldHint: false`
/// makes it return false (no approval needed). Use this ONLY for tools
/// whose handler explicitly checks `CardRole` - the kernel's role gate
/// is the actual authorization boundary; this annotation just tells codex
/// not to insert a second approval prompt on top.
///
/// Do NOT slap this on every new write tool - re-evaluate whether the
/// handler enforces a real authorization gate first. If a tool ever
/// writes outside the wave/cove the caller owns (e.g. crosses cove
/// boundaries or touches global state), keep approval ON by using
/// `None` annotations or building a custom block with `destructiveHint:
/// true`.
pub fn role_gated_write_annotations() -> Value {
    json!({
        "readOnlyHint": false,
        "destructiveHint": false,
        "openWorldHint": false,
    })
}

/// Map of tool name → handler + descriptor. Populated by
/// [`build_default_registry`] (emit + wave-state + wave-report tools).
pub struct ToolRegistry {
    by_name: HashMap<String, (ToolDescriptor, ToolHandler)>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            by_name: HashMap::new(),
        }
    }

    pub fn register(&mut self, descriptor: ToolDescriptor, handler: ToolHandler) {
        self.by_name
            .insert(descriptor.name.clone(), (descriptor, handler));
    }

    /// Look up a handler by name. Returns the handler clone (an `Arc`,
    /// so cheap) or `None` if no tool with that name was registered.
    pub fn lookup(&self, name: &str) -> Option<ToolHandler> {
        self.by_name.get(name).map(|(_, h)| h.clone())
    }

    /// Snapshot of `tools/list` descriptors. Returns owned clones so
    /// the caller can serialize without holding a borrow on the
    /// registry across an await.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.by_name.values().map(|(d, _)| d.clone()).collect()
    }

    pub fn descriptors_for_role(&self, role: CardRole) -> Vec<ToolDescriptor> {
        self.by_name
            .values()
            .map(|(d, _)| d.clone())
            .filter(|d| d.visible_to_roles.contains(&role))
            .collect()
    }

    pub fn descriptors_visible_to_any_role(&self, roles: &[CardRole]) -> Vec<ToolDescriptor> {
        self.by_name
            .values()
            .filter(|d| roles.iter().any(|role| d.0.visible_to_roles.contains(role)))
            .map(|(d, _)| d.clone())
            .collect()
    }
}

/// Register `old_name` as a hidden alias for an already-registered tool
/// (`new_name`). On invocation, logs a `warn!` and delegates to the new
/// handler. Hidden from every role's `tools/list` (`visible_to_roles: &[]`).
///
/// MUST be called AFTER the real handler is registered. The real descriptor
/// stays untouched.
pub fn register_deprecated_alias(
    registry: &mut ToolRegistry,
    old_name: &'static str,
    new_name: &'static str,
) {
    let real = registry
        .lookup(new_name)
        .unwrap_or_else(|| panic!("register_deprecated_alias: {new_name} not registered yet"));
    let new_for_log = new_name;
    let old_for_log = old_name;
    let handler: ToolHandler = Arc::new(move |ctx, identity, args| {
        tracing::warn!(
            target: "mcp_alias",
            card_id = %identity.card_id,
            old_name = old_for_log,
            new_name = new_for_log,
            "deprecated MCP tool name; please migrate"
        );
        real(ctx, identity, args)
    });
    let alias_descriptor = ToolDescriptor {
        name: old_name.into(),
        description: format!("[deprecated] Use `{new_name}` instead. Hidden from tools/list."),
        input_schema: json!({ "type": "object", "additionalProperties": true }),
        annotations: None,
        visible_to_roles: &[],
    };
    registry.register(alias_descriptor, handler);
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::state::WriteContext;
    use crate::wave_cove_cache::WaveCoveCache;

    fn identity_with_role_and_provider(
        role: CardRole,
        provider: AgentProvider,
    ) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: "card-1".to_string(),
            role,
            provider,
            session_id: "session-1".to_string(),
            wave_id: Some("wave-1".to_string()),
            cove_id: "cove-1".to_string(),
            thread_id: "thread-1".to_string(),
        }
    }

    fn identity_with_role(role: CardRole) -> ToolCallIdentity {
        identity_with_role_and_provider(role, AgentProvider::Codex)
    }

    fn card_identity_with_role_and_provider(
        role: CardRole,
        provider: AgentProvider,
    ) -> CardIdentity {
        CardIdentity {
            card_id: CardId::from("card-1"),
            role,
            provider,
            session_id: "session-1".to_string(),
            wave_id: Some("wave-1".to_string()),
            cove_id: "cove-1".to_string(),
        }
    }

    fn card_identity_with_role(role: CardRole) -> CardIdentity {
        card_identity_with_role_and_provider(role, AgentProvider::Codex)
    }

    #[test]
    fn card_identity_to_actor_id_uses_session_actor_for_each_role() {
        assert_eq!(
            card_identity_with_role(CardRole::Spec).to_actor_id(),
            ActorId::AiSpecSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            card_identity_with_role(CardRole::Worker).to_actor_id(),
            ActorId::AiCodexSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            card_identity_with_role_and_provider(CardRole::Worker, AgentProvider::Claude)
                .to_actor_id(),
            ActorId::AiClaudeSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            card_identity_with_role(CardRole::ReportCard).to_actor_id(),
            ActorId::AiCodexSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            card_identity_with_role_and_provider(CardRole::ReportCard, AgentProvider::Claude)
                .to_actor_id(),
            ActorId::AiClaudeSession(WorkerSessionId::from("session-1"))
        );
    }

    #[test]
    fn tool_call_identity_to_actor_id_uses_session_actor_for_each_role() {
        assert_eq!(
            identity_with_role(CardRole::Spec).to_actor_id(),
            ActorId::AiSpecSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            identity_with_role(CardRole::Worker).to_actor_id(),
            ActorId::AiCodexSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            identity_with_role_and_provider(CardRole::Worker, AgentProvider::Claude).to_actor_id(),
            ActorId::AiClaudeSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            identity_with_role(CardRole::ReportCard).to_actor_id(),
            ActorId::AiCodexSession(WorkerSessionId::from("session-1"))
        );
        assert_eq!(
            identity_with_role_and_provider(CardRole::ReportCard, AgentProvider::Claude)
                .to_actor_id(),
            ActorId::AiClaudeSession(WorkerSessionId::from("session-1"))
        );
    }

    #[test]
    fn require_role_any_accepts_any_allowed_role_and_rejects_others() {
        let allowed = [CardRole::Spec, CardRole::ReportCard];

        assert!(require_role_any(&identity_with_role(CardRole::Spec), &allowed).is_ok());
        assert!(require_role_any(&identity_with_role(CardRole::ReportCard), &allowed).is_ok());

        let err = require_role_any(&identity_with_role(CardRole::Worker), &allowed)
            .expect_err("worker must be denied");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("Spec") && err.message.contains("ReportCard"),
            "error should mention allowed roles: {err:?}"
        );
        assert!(
            err.message.contains("Worker"),
            "error should mention actual role: {err:?}"
        );
    }

    fn fake_descriptor(name: &str, visible_to_roles: &'static [CardRole]) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: "fake".to_string(),
            input_schema: json!({ "type": "object" }),
            annotations: None,
            visible_to_roles,
        }
    }

    fn fake_handler(who: &'static str) -> ToolHandler {
        Arc::new(move |_ctx, _identity, _args| Box::pin(async move { Ok(json!({ "who": who })) }))
    }

    async fn fake_context() -> Arc<AppContext> {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let route_repo: Arc<dyn RouteRepo> = repo;
        Arc::new(AppContext {
            repo: route_repo,
            wave_vcs: None,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: std::env::temp_dir().join("neige-registry-test-gate-logs"),
            plugin_host: Arc::new(tokio::sync::OnceCell::new()),
            operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
        })
    }

    #[tokio::test]
    async fn deprecated_alias_forwards_to_real_handler() {
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("calm.foo.bar", &[CardRole::Spec]),
            fake_handler("real"),
        );
        register_deprecated_alias(&mut registry, "calm.foo_bar", "calm.foo.bar");

        let handler = registry
            .lookup("calm.foo_bar")
            .expect("alias handler registered");
        let out = handler(
            fake_context().await,
            identity_with_role(CardRole::Spec),
            json!({ "anything": true }),
        )
        .await
        .expect("alias forwards to real handler");

        assert_eq!(out, json!({ "who": "real" }));
    }

    #[test]
    fn deprecated_alias_is_hidden_from_tools_list() {
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("calm.foo.bar", &[CardRole::Spec]),
            fake_handler("real"),
        );
        register_deprecated_alias(&mut registry, "calm.foo_bar", "calm.foo.bar");

        let names = registry
            .descriptors_for_role(CardRole::Spec)
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"calm.foo.bar".to_string()));
        assert!(!names.contains(&"calm.foo_bar".to_string()));
    }

    #[test]
    fn descriptors_visible_to_any_role_returns_union_without_hidden_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("calm.spec.only", &[CardRole::Spec]),
            fake_handler("spec"),
        );
        registry.register(
            fake_descriptor("calm.worker.only", &[CardRole::Worker]),
            fake_handler("worker"),
        );
        registry.register(
            fake_descriptor("calm.shared", &[CardRole::Spec, CardRole::Worker]),
            fake_handler("shared"),
        );
        registry.register(
            fake_descriptor("calm.report.only", &[CardRole::ReportCard]),
            fake_handler("report"),
        );
        registry.register(fake_descriptor("calm.hidden", &[]), fake_handler("hidden"));

        let mut names = registry
            .descriptors_visible_to_any_role(&[CardRole::Spec, CardRole::Worker])
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(
            names,
            vec!["calm.shared", "calm.spec.only", "calm.worker.only"]
        );
    }

    #[tokio::test]
    async fn deprecated_alias_does_not_overwrite_real_name() {
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("calm.foo.bar", &[CardRole::Spec]),
            fake_handler("real"),
        );
        register_deprecated_alias(&mut registry, "calm.foo_bar", "calm.foo.bar");

        let handler = registry
            .lookup("calm.foo.bar")
            .expect("real handler still registered");
        let out = handler(
            fake_context().await,
            identity_with_role(CardRole::Spec),
            json!({}),
        )
        .await
        .expect("real handler still callable");

        assert_eq!(out, json!({ "who": "real" }));
    }

    #[test]
    fn wave_history_drill_ins_are_hidden_but_registered() {
        let mut registry = ToolRegistry::new();
        crate::mcp_server::tools::register_default_tools(&mut registry);
        let hidden = [
            crate::mcp_server::tools::wave_history::TOOL_WAVE_DIFF,
            crate::mcp_server::tools::wave_history::TOOL_WAVE_CAT_AT,
            crate::mcp_server::tools::wave_history::TOOL_WAVE_LOG,
            crate::mcp_server::tools::admin::TOOL_ADMIN_WAVE_GC,
            crate::mcp_server::tools::admin::TOOL_ADMIN_VACUUM,
        ];

        for name in hidden {
            assert!(registry.lookup(name).is_some(), "{name} handler registered");
            for role in [CardRole::Spec, CardRole::Worker, CardRole::ReportCard] {
                let names = registry
                    .descriptors_for_role(role)
                    .into_iter()
                    .map(|descriptor| descriptor.name)
                    .collect::<Vec<_>>();
                assert!(
                    !names.iter().any(|visible| visible == name),
                    "{name} must be hidden from {role:?} tools/list: {names:?}"
                );
            }
        }
    }
}
