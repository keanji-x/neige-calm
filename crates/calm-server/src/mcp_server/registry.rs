//! Tool registry + per-connection app context for the kernel's MCP server: a name -> handler
//! map the transport consults on every `tools/call`, with role-filtered discovery.

use crate::db::{Repo, RouteRepo};
use crate::event::EventBus;
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::result::ToolResult;
use crate::model::CardRole;
use crate::session_projection_repo::AgentProvider;
use crate::state::WriteContext;
use calm_truth::track_vcs_repo::{SqlxTrackVcsRepo, TrackVcsRepo};
use calm_types::worker::{Principal, WorkerSessionId};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The card identity bound to a single MCP connection, established at handshake time.
#[derive(Clone, Debug)]
pub struct CardIdentity {
    pub card_id: CardId,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: String,
    pub track_id: Option<String>,
    pub area_id: String,
}

impl CardIdentity {
    /// The `ActorId` the role gate will see; MCP writes are keyed by worker session, not card id.
    /// `ReportCard` is mapped by provider as a total-function fallback — the role gate refuses it.
    pub fn to_actor_id(&self) -> ActorId {
        let session_id = WorkerSessionId::from(self.session_id.clone());
        match self.role {
            CardRole::Planner => ActorId::AiPlannerSession(session_id),
            CardRole::Worker | CardRole::ReportCard | CardRole::Assistant => {
                provider_session_actor(&self.provider, session_id)
            }
        }
    }

    pub fn to_principal(&self) -> Option<Principal> {
        let track_id = self.track_id.as_ref()?;
        Some(Principal::Agent {
            session_id: WorkerSessionId::from(self.session_id.clone()),
            track_id: TrackId::from(track_id.clone()),
            area_id: AreaId::from(self.area_id.clone()),
        })
    }
}

/// Identity mode established once by the MCP `initialize` handshake. Daemon-trust connections
/// must provide a resolvable `_meta.threadId` per call; card-bound ones may omit it, but any
/// supplied `threadId` must resolve back to the same card.
#[derive(Clone, Debug)]
pub enum ConnectionIdentity {
    DaemonTrust,
    CardBound(CardIdentity),
}

/// Identity resolved for one MCP `tools/call`. In the card-bound no-thread case `thread_id` is
/// the literal `"card-bound"` sentinel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallIdentity {
    pub card_id: String,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: String,
    pub track_id: Option<String>,
    pub area_id: String,
    pub thread_id: String,
}

impl ToolCallIdentity {
    /// The `ActorId` the role gate will see; MCP writes are keyed by worker session, not card id.
    pub fn to_actor_id(&self) -> ActorId {
        let session_id = WorkerSessionId::from(self.session_id.clone());
        match self.role {
            CardRole::Planner => ActorId::AiPlannerSession(session_id),
            CardRole::Worker | CardRole::ReportCard | CardRole::Assistant => {
                provider_session_actor(&self.provider, session_id)
            }
        }
    }

    pub fn to_principal(&self) -> Option<Principal> {
        let track_id = self.track_id.as_ref()?;
        Some(Principal::Agent {
            session_id: WorkerSessionId::from(self.session_id.clone()),
            track_id: TrackId::from(track_id.clone()),
            area_id: AreaId::from(self.area_id.clone()),
        })
    }
}

fn provider_session_actor(provider: &AgentProvider, session_id: WorkerSessionId) -> ActorId {
    match provider {
        AgentProvider::Codex => ActorId::AiCodexSession(session_id),
        AgentProvider::Claude => ActorId::AiClaudeSession(session_id),
    }
}

/// Soft role gate for planner-only MCP tools, purely UX: the real boundary is
/// `role_gate::enforce_role` inside every eventized write. This gives a deterministic
/// `-32602 planner-only tool` error instead of the in-tx `-32403`.
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

/// Per-process context every tool handler reads from, `Arc`-cloned into each connection task.
#[derive(Clone)]
pub struct AppContext {
    pub terminal_interaction:
        Arc<tokio::sync::OnceCell<Arc<crate::terminal_interaction::TerminalInteraction>>>,
    /// Eventized writes route through this; the dyn-trait gate keeps sync-domain raw writes
    /// unreachable from a tool handler.
    pub repo: Arc<dyn RouteRepo>,
    /// Read-only track-vcs drill-ins need the sqlite-backed audit tables.
    pub track_vcs: Option<Arc<dyn TrackVcsRepo>>,
    pub events: EventBus,
    /// Write-surface caches shared with REST/worker paths.
    pub write: WriteContext,
    /// Optional server-wide MCP daemon token hash; the handshake accepts it as daemon trust.
    pub daemon_token_hash: Option<String>,
    /// The CONFIGURED gate-logs dir, so the `plan/<key>/gate.log` view reads the same directory
    /// the gate runner writes.
    pub gate_logs_dir: std::path::PathBuf,
    /// Same boot-resolved value used by scheduler admission. Report reads use
    /// it to explain effective budgets without consulting process-global env.
    pub task_budget_default: i64,
    /// Late-bound: MCP server boot happens before plugin host construction.
    pub plugin_host: Arc<tokio::sync::OnceCell<Arc<crate::plugin_host::PluginHost>>>,
    /// Late-bound: MCP boot precedes runtime construction.
    pub operation_runtime: Arc<tokio::sync::OnceCell<Arc<crate::operation::OperationRuntime>>>,
    /// The `chart.series` background resolver; `calm.report.read` enqueues into it.
    pub series_resolver: Arc<crate::report_series::SeriesResolver>,
    /// Transient ring of Planner plugin results `calm.source.capture` reads.
    pub plugin_results: Arc<crate::plugin_results::PluginResults>,
    /// The repo's sqlite pool for **read-only** statements; writes never go through this.
    /// `None` only for repos without sqlite (tests).
    pub sqlite_pool: Option<sqlx::SqlitePool>,
}

impl AppContext {
    /// The one production construction; the HTTP series route and the MCP tools share one
    /// `SeriesResolver`. The two late-bound cells are filled by `AppState::new` once those exist.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        daemon_token_hash: Option<String>,
        plugin_host: Arc<tokio::sync::OnceCell<Arc<crate::plugin_host::PluginHost>>>,
        operation_runtime: Arc<tokio::sync::OnceCell<Arc<crate::operation::OperationRuntime>>>,
        gate_logs_dir: std::path::PathBuf,
        task_budget_default: i64,
    ) -> Arc<Self> {
        let sqlite_pool = repo.sqlite_pool();
        let track_vcs = sqlite_pool.clone().map(SqlxTrackVcsRepo::shared);
        let series_resolver = Arc::new(crate::report_series::SeriesResolver::new(
            sqlite_pool.clone(),
        ));
        let route_repo: Arc<dyn RouteRepo> = repo;
        Arc::new(Self {
            terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
            repo: route_repo,
            track_vcs,
            events,
            write,
            daemon_token_hash,
            gate_logs_dir,
            task_budget_default,
            plugin_host,
            operation_runtime,
            series_resolver,
            plugin_results: Arc::new(crate::plugin_results::PluginResults::new()),
            sqlite_pool,
        })
    }
}

/// Boxed future returned by a tool handler, keeping the registry's map values object-safe.
pub type ToolHandlerFuture =
    Pin<Box<dyn Future<Output = Result<ToolResult, RpcError>> + Send + 'static>>;

/// One tool's invocation contract; handlers shape-validate `arguments` and translate
/// internal errors into [`RpcError`].
pub type ToolHandler =
    Arc<dyn Fn(Arc<AppContext>, ToolCallIdentity, Value) -> ToolHandlerFuture + Send + Sync>;

/// `tools/list` descriptor — the JSON shape codex's MCP client expects.
#[derive(Clone)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    /// Pre-built JSON schema for the tool's `arguments` object, stored verbatim.
    pub input_schema: Value,
    /// Optional MCP `annotations` block. Codex reads `readOnlyHint`/`destructiveHint`/`openWorldHint`
    /// to decide whether the tool needs explicit approval; missing annotations default to
    /// "approval required".
    pub annotations: Option<Value>,
    /// Which roles see this tool in `tools/list`; `tools/call` still routes by name regardless.
    /// Explicit `&[]` for tools that must not appear in any role's list.
    pub visible_to_roles: &'static [CardRole],
}

pub fn read_only_annotations() -> Value {
    json!({ "readOnlyHint": true })
}

/// Annotations telling codex not to insert a second approval prompt. Use ONLY for tools whose
/// handler explicitly checks `CardRole` — the kernel's role gate is the actual authorization
/// boundary; a tool that writes outside the caller's track/area must keep approval ON.
pub fn role_gated_write_annotations() -> Value {
    json!({
        "readOnlyHint": false,
        "destructiveHint": false,
        "openWorldHint": false,
    })
}

/// Map of tool name → handler + descriptor.
pub struct ToolRegistry {
    by_name: HashMap<String, (ToolDescriptor, ToolHandler)>,
    /// Names registered through [`register_deprecated_alias`] and not since re-registered as real
    /// tools; the one kind of descriptor without a `prompts/tools/<name>.md` source.
    deprecated_aliases: BTreeSet<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            by_name: HashMap::new(),
            deprecated_aliases: BTreeSet::new(),
        }
    }

    /// A real tool registered over a former alias name is a real tool: the
    /// name leaves [`deprecated_alias_names`](Self::deprecated_alias_names).
    pub fn register(&mut self, descriptor: ToolDescriptor, handler: ToolHandler) {
        self.deprecated_aliases.remove(&descriptor.name);
        self.by_name
            .insert(descriptor.name.clone(), (descriptor, handler));
    }

    pub fn lookup(&self, name: &str) -> Option<ToolHandler> {
        self.by_name.get(name).map(|(_, h)| h.clone())
    }

    /// Owned clones so the caller can serialize without holding a borrow across an await.
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

    pub fn deprecated_alias_names(&self) -> &BTreeSet<String> {
        &self.deprecated_aliases
    }
}

/// Register `old_name` as a hidden alias that warns and delegates to `new_name`'s handler.
/// MUST be called AFTER the real handler is registered.
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
    registry.deprecated_aliases.insert(old_name.to_string());
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
    use crate::track_area_cache::TrackAreaCache;

    fn identity_with_role_and_provider(
        role: CardRole,
        provider: AgentProvider,
    ) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: "card-1".to_string(),
            role,
            provider,
            session_id: "session-1".to_string(),
            track_id: Some("track-1".to_string()),
            area_id: "area-1".to_string(),
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
            track_id: Some("track-1".to_string()),
            area_id: "area-1".to_string(),
        }
    }

    fn card_identity_with_role(role: CardRole) -> CardIdentity {
        card_identity_with_role_and_provider(role, AgentProvider::Codex)
    }

    #[test]
    fn card_identity_to_actor_id_uses_session_actor_for_each_role() {
        assert_eq!(
            card_identity_with_role(CardRole::Planner).to_actor_id(),
            ActorId::AiPlannerSession(WorkerSessionId::from("session-1"))
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
            identity_with_role(CardRole::Planner).to_actor_id(),
            ActorId::AiPlannerSession(WorkerSessionId::from("session-1"))
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
        let allowed = [CardRole::Planner, CardRole::ReportCard];

        assert!(require_role_any(&identity_with_role(CardRole::Planner), &allowed).is_ok());
        assert!(require_role_any(&identity_with_role(CardRole::ReportCard), &allowed).is_ok());

        let err = require_role_any(&identity_with_role(CardRole::Worker), &allowed)
            .expect_err("worker must be denied");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("Planner") && err.message.contains("ReportCard"),
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
        Arc::new(move |_ctx, _identity, _args| {
            Box::pin(async move { Ok(ToolResult::structured(json!({ "who": who }))) })
        })
    }

    async fn fake_context() -> Arc<AppContext> {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let sqlite_pool = repo.sqlite_pool();
        let route_repo: Arc<dyn RouteRepo> = repo;
        Arc::new(AppContext {
            terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
            repo: route_repo,
            track_vcs: None,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: std::env::temp_dir().join("neige-registry-test-gate-logs"),
            task_budget_default: crate::scheduler::DEFAULT_TRACK_TASK_BUDGET,
            plugin_host: Arc::new(tokio::sync::OnceCell::new()),
            operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
            series_resolver: Arc::new(crate::report_series::SeriesResolver::new_unstarted(None)),
            plugin_results: Arc::new(crate::plugin_results::PluginResults::new()),
            sqlite_pool,
        })
    }

    #[tokio::test]
    async fn deprecated_alias_forwards_to_real_handler() {
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("calm.foo.bar", &[CardRole::Planner]),
            fake_handler("real"),
        );
        register_deprecated_alias(&mut registry, "calm.foo_bar", "calm.foo.bar");

        let handler = registry
            .lookup("calm.foo_bar")
            .expect("alias handler registered");
        let out = handler(
            fake_context().await,
            identity_with_role(CardRole::Planner),
            json!({ "anything": true }),
        )
        .await
        .expect("alias forwards to real handler");

        assert_eq!(out.into_structured(), json!({ "who": "real" }));
    }

    #[test]
    fn deprecated_alias_is_hidden_from_tools_list() {
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("calm.foo.bar", &[CardRole::Planner]),
            fake_handler("real"),
        );
        register_deprecated_alias(&mut registry, "calm.foo_bar", "calm.foo.bar");

        let names = registry
            .descriptors_for_role(CardRole::Planner)
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
            fake_descriptor("calm.spec.only", &[CardRole::Planner]),
            fake_handler("planner"),
        );
        registry.register(
            fake_descriptor("calm.worker.only", &[CardRole::Worker]),
            fake_handler("worker"),
        );
        registry.register(
            fake_descriptor("calm.shared", &[CardRole::Planner, CardRole::Worker]),
            fake_handler("shared"),
        );
        registry.register(
            fake_descriptor("calm.report.only", &[CardRole::ReportCard]),
            fake_handler("report"),
        );
        registry.register(fake_descriptor("calm.hidden", &[]), fake_handler("hidden"));

        let mut names = registry
            .descriptors_visible_to_any_role(&[CardRole::Planner, CardRole::Worker])
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
            fake_descriptor("calm.foo.bar", &[CardRole::Planner]),
            fake_handler("real"),
        );
        register_deprecated_alias(&mut registry, "calm.foo_bar", "calm.foo.bar");

        let handler = registry
            .lookup("calm.foo.bar")
            .expect("real handler still registered");
        let out = handler(
            fake_context().await,
            identity_with_role(CardRole::Planner),
            json!({}),
        )
        .await
        .expect("real handler still callable");

        assert_eq!(out.into_structured(), json!({ "who": "real" }));
    }

    #[test]
    fn deprecated_alias_names_track_registration_order() {
        // real then alias: the alias name is an alias.
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("calm.foo.bar", &[CardRole::Planner]),
            fake_handler("real"),
        );
        register_deprecated_alias(&mut registry, "calm.foo_bar", "calm.foo.bar");
        assert!(registry.deprecated_alias_names().contains("calm.foo_bar"));
        assert!(!registry.deprecated_alias_names().contains("calm.foo.bar"));

        // alias then real over the same name: it is a real tool now.
        registry.register(
            fake_descriptor("calm.foo_bar", &[CardRole::Planner]),
            fake_handler("real-again"),
        );
        assert!(
            !registry.deprecated_alias_names().contains("calm.foo_bar"),
            "a real tool registered over a former alias name is a real tool: {:?}",
            registry.deprecated_alias_names()
        );
        assert!(registry.lookup("calm.foo_bar").is_some());
    }

    #[test]
    fn track_history_drill_ins_are_hidden_but_registered() {
        let mut registry = ToolRegistry::new();
        crate::mcp_server::tools::register_default_tools(&mut registry);
        let hidden = [
            crate::mcp_server::tools::track_history::TOOL_TRACK_DIFF,
            crate::mcp_server::tools::track_history::TOOL_TRACK_CAT_AT,
            crate::mcp_server::tools::track_history::TOOL_TRACK_LOG,
            crate::mcp_server::tools::admin::TOOL_ADMIN_TRACK_GC,
            crate::mcp_server::tools::admin::TOOL_ADMIN_VACUUM,
        ];

        for name in hidden {
            assert!(registry.lookup(name).is_some(), "{name} handler registered");
            for role in [
                CardRole::Planner,
                CardRole::Worker,
                CardRole::ReportCard,
                CardRole::Assistant,
            ] {
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
