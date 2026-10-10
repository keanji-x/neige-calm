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
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::collections::HashMap;
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

impl ToolCallIdentity {
    /// The caller session is still the active authority of a Planner or Assistant card on
    /// `track`, read in the caller's write transaction (#2130 D16): the check a kernel-written
    /// effect of an agent's tool call re-runs where it commits.
    pub(crate) async fn session_is_active(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        track: &str,
    ) -> Result<bool, crate::error::CalmError> {
        use calm_truth::decision_gate::{GateDecision, PrincipalDecisionGate};
        let Some(principal) = self.to_principal() else {
            return Ok(false);
        };
        let decision = PrincipalDecisionGate::new(principal)
            .decide_recorder(tx, &TrackId::from(track.to_string()))
            .await?;
        Ok(matches!(decision, GateDecision::Allow))
    }
}

fn provider_session_actor(provider: &AgentProvider, session_id: WorkerSessionId) -> ActorId {
    match provider {
        // The released identity contract permits OpenCode only for managed Planner sessions.
        AgentProvider::OpenCode => ActorId::AiPlannerSession(session_id),
        AgentProvider::Codex => ActorId::AiCodexSession(session_id),
        AgentProvider::Claude => ActorId::AiClaudeSession(session_id),
    }
}

/// The soft role gate: the registry runs it on every kernel tool's declared
/// [`ToolDescriptor::roles`], and dispatch on every manifest plugin tool. The real boundary is
/// `role_gate::enforce_role` inside every eventized write. Both answer `-32403` (§5).
pub fn require_role_any(identity: &ToolCallIdentity, allowed: &[CardRole]) -> Result<(), RpcError> {
    if allowed.contains(&identity.role) {
        return Ok(());
    }
    Err(RpcError::forbidden(format!(
        "tool requires role in {allowed:?} got={got:?}",
        got = identity.role
    )))
}

/// Closed input (§4 of `docs/conventions/agent-commands.md`): the first key of `obj` outside
/// `valid` is refused with every valid key. `at` is the tool name, or `<tool>: <path>` for a
/// nested object. The registry runs it on every kernel tool's top-level arguments.
pub(crate) fn refuse_unknown_keys<S: AsRef<str>>(
    obj: &serde_json::Map<String, Value>,
    valid: &[S],
    at: &str,
) -> Result<(), RpcError> {
    let Some(key) = obj
        .keys()
        .find(|key| !valid.iter().any(|valid| valid.as_ref() == key.as_str()))
    else {
        return Ok(());
    };
    let mut names: Vec<&str> = valid.iter().map(AsRef::as_ref).collect();
    names.sort_unstable();
    let names = if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    };
    // A removed key keeps its pointer to what replaced it (principle 7).
    let retired = match key.as_str() {
        "lifecycle" => {
            "; `lifecycle` is removed: close with neige_track_close; ask with neige_user_ask"
        }
        _ => "",
    };
    Err(RpcError::invalid_params(format!(
        "{at}: unknown argument `{key}`; valid: {names}{retired}"
    )))
}

/// Closed input for a native plugin tool, from its manifest `inputSchema`: a schema that declares
/// `additionalProperties: false` refuses an unknown top-level key through [`refuse_unknown_keys`],
/// as the registry does for a kernel tool. An open or absent schema leaves the plugin to decide.
pub(crate) fn refuse_unknown_schema_keys(
    schema: Option<&Value>,
    args: &Value,
    at: &str,
) -> Result<(), RpcError> {
    let (Some(schema), Some(obj)) = (schema, args.as_object()) else {
        return Ok(());
    };
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Ok(());
    }
    refuse_unknown_keys(obj, &schema_keys(schema), at)
}

/// The top-level keys a tool's declared input schema accepts: the one source of its closed input.
fn schema_keys(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

/// The scheduler triggers a tool may fire after its commit (see [`AppContext::scheduler_poke`]).
pub trait SchedulerPokes: Send + Sync {
    /// Reap now the workers whose cleanup marker was just committed
    /// (`Scheduler::poke_worker_cleanups`).
    fn poke_worker_cleanups(&self);
}

/// A handle to the scheduler's triggers (see [`AppContext::scheduler_poke`]).
pub type SchedulerPoke = Arc<dyn SchedulerPokes>;

impl SchedulerPokes for Arc<crate::scheduler::Scheduler> {
    fn poke_worker_cleanups(&self) {
        crate::scheduler::Scheduler::poke_worker_cleanups(self);
    }
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
    /// The CONFIGURED gate-logs dir, so the `runs/<attempt_id>/gates/<N>.log` view reads the same directory
    /// the gate runner writes.
    pub gate_logs_dir: std::path::PathBuf,
    /// Late-bound: MCP server boot happens before plugin host construction.
    pub plugin_host: Arc<tokio::sync::OnceCell<Arc<crate::plugin_host::PluginHost>>>,
    /// Late-bound: MCP boot precedes runtime construction.
    pub operation_runtime: Arc<tokio::sync::OnceCell<Arc<crate::operation::OperationRuntime>>>,
    /// Late-bound: the keyed Track create `neige_track_add` runs, bound once by
    /// `AppState::bind_track_creator`. Weak: the creator holds the route state, which holds this
    /// context, so a strong handle here would keep the whole state alive; `AppState` owns it.
    pub track_creator: Arc<
        tokio::sync::OnceCell<
            std::sync::Weak<dyn crate::mcp_server::tools::track_add::TrackCreator>,
        >,
    >,
    /// Late-bound (the Dispatcher is spawned after the MCP context): the scheduler's triggers.
    /// A running-task cancel pokes the worker reap through it; unbound (fixtures without a
    /// Dispatcher) means the reconcile sweep reaps instead.
    pub scheduler_poke: Arc<tokio::sync::OnceCell<SchedulerPoke>>,
    /// The `chart.series` background resolver; `neige_report_read` enqueues into it.
    pub series_resolver: Arc<crate::report_series::SeriesResolver>,
    /// Transient ring of Planner plugin results `neige_source_capture` reads.
    pub plugin_results: Arc<crate::plugin_results::PluginResults>,
    /// What each session last read of a report: the anchors of the agent report writes (#1877, #1883).
    pub read_ledger: Arc<crate::report_read_ledger::ReadLedger>,
    /// #1780 preview gateway registrations; the gateway listeners read the same `Arc`.
    pub preview: Arc<crate::preview::PreviewRegistry>,
    /// The repo's sqlite pool for **read-only** statements; writes never go through this.
    /// `None` only for repos without sqlite (tests).
    pub sqlite_pool: Option<sqlx::SqlitePool>,
    /// How long one `neige_task_gate` call waits before it answers `running` (#2464 D3), from the
    /// configured worker idle window.
    pub gate_run_wait: crate::operation::task_gate_run::GateRunWait,
}

impl AppContext {
    /// The one production construction; the HTTP series route and the MCP tools share one
    /// `SeriesResolver`. The two late-bound cells are filled by `AppState::new` once those exist;
    /// `scheduler_poke` is bound there too, once the Dispatcher is spawned.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        daemon_token_hash: Option<String>,
        plugin_host: Arc<tokio::sync::OnceCell<Arc<crate::plugin_host::PluginHost>>>,
        operation_runtime: Arc<tokio::sync::OnceCell<Arc<crate::operation::OperationRuntime>>>,
        gate_logs_dir: std::path::PathBuf,
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
            plugin_host,
            operation_runtime,
            track_creator: Arc::new(tokio::sync::OnceCell::new()),
            scheduler_poke: Arc::new(tokio::sync::OnceCell::new()),
            series_resolver,
            plugin_results: Arc::new(crate::plugin_results::PluginResults::new()),
            read_ledger: Arc::new(crate::report_read_ledger::ReadLedger::new()),
            preview: Arc::new(crate::preview::PreviewRegistry::disabled()),
            sqlite_pool,
            gate_run_wait: crate::operation::task_gate_run::GateRunWait::DEFAULT,
        })
    }

    /// Boot's gate-run wait, from the configured worker idle window, set on the context
    /// [`Self::new`] just returned, before it is shared; every other construction keeps the bound
    /// of the default window.
    pub fn with_gate_run_wait(
        mut self: Arc<Self>,
        wait: crate::operation::task_gate_run::GateRunWait,
    ) -> Arc<Self> {
        Arc::get_mut(&mut self)
            .expect("with_gate_run_wait runs before the context is shared")
            .gate_run_wait = wait;
        self
    }

    /// Boot's preview registry, set on the context [`Self::new`] just returned, before it is
    /// shared; every other construction keeps the disabled one.
    pub fn with_preview(
        mut self: Arc<Self>,
        preview: Arc<crate::preview::PreviewRegistry>,
    ) -> Arc<Self> {
        Arc::get_mut(&mut self)
            .expect("with_preview runs before the context is shared")
            .preview = preview;
        self
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
    /// The roles that may call this tool. The registry refuses every other role before the
    /// handler runs (`-32403`), so a handler never re-checks the caller's role.
    pub roles: &'static [CardRole],
    /// The roles whose `tools/list` shows this tool, a subset of `roles`: a context-budget
    /// choice, never a grant. Explicit `&[]` for a tool served only by name (a CLI view).
    pub listed_for: &'static [CardRole],
}

impl ToolDescriptor {
    pub(crate) fn into_mcp_value(self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("name".into(), Value::String(self.name));
        object.insert("description".into(), Value::String(self.description));
        object.insert("inputSchema".into(), self.input_schema);
        if let Some(annotations) = self.annotations {
            object.insert("annotations".into(), annotations);
        }
        Value::Object(object)
    }
}

pub use calm_types::plugin::read_only_annotations;

/// Annotations telling codex not to insert a second approval prompt. Use ONLY for tools whose
/// declared `roles` the registry gates — the kernel's role gate is the actual authorization
/// boundary; a tool that writes outside the caller's track/area must keep approval ON.
pub use calm_types::plugin::role_gated_write_annotations;

/// A visibility fence a tool runs before its closed input: a built-in plugin's native tool is
/// unknown (`-32601`) to a caller whose Track does not bind the running plugin.
pub type ToolFence = Arc<
    dyn Fn(
            Arc<AppContext>,
            ToolCallIdentity,
        ) -> Pin<Box<dyn Future<Output = Result<(), RpcError>> + Send>>
        + Send
        + Sync,
>;

struct Entry {
    descriptor: ToolDescriptor,
    handler: ToolHandler,
    fence: Option<ToolFence>,
    keys: Arc<[String]>,
}

/// Map of tool name → handler + descriptor. [`Self::lookup`] returns the guarded handler, the one
/// order every kernel tool call runs in: the managed-track grant, the tool's fence, closed input
/// (keys from the declared schema), the declared `roles`, then the handler (its arguments and
/// state). Every refusal is led by the tool name (§5).
pub struct ToolRegistry {
    by_name: HashMap<String, Entry>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            by_name: HashMap::new(),
        }
    }

    pub fn register(&mut self, descriptor: ToolDescriptor, handler: ToolHandler) {
        self.register_fenced(descriptor, None, handler);
    }

    pub fn register_fenced(
        &mut self,
        descriptor: ToolDescriptor,
        fence: Option<ToolFence>,
        handler: ToolHandler,
    ) {
        let keys = schema_keys(&descriptor.input_schema).into();
        self.by_name.insert(
            descriptor.name.clone(),
            Entry {
                descriptor,
                handler,
                fence,
                keys,
            },
        );
    }

    pub fn lookup(&self, name: &str) -> Option<ToolHandler> {
        let entry = self.by_name.get(name)?;
        let name = entry.descriptor.name.clone();
        let roles = entry.descriptor.roles;
        let handler = entry.handler.clone();
        let fence = entry.fence.clone();
        let keys = entry.keys.clone();
        Some(Arc::new(move |ctx, identity, args| {
            let (name, handler, fence, keys) =
                (name.clone(), handler.clone(), fence.clone(), keys.clone());
            Box::pin(async move {
                let result = async {
                    crate::managed_track::require_tool_allowed(&ctx, &identity, &name).await?;
                    if let Some(fence) = fence {
                        fence(ctx.clone(), identity.clone()).await?;
                    }
                    if let Some(obj) = args.as_object() {
                        refuse_unknown_keys(obj, &keys, &name)?;
                    }
                    require_role_any(&identity, roles)?;
                    handler(ctx, identity, args).await
                }
                .await;
                result.map_err(|error| error.for_tool(&name))
            })
        }))
    }

    /// The handler as registered, without the guard: for re-registering a built-in plugin's
    /// native tool into the kernel registry, which guards it there.
    pub(crate) fn unguarded(&self, name: &str) -> Option<ToolHandler> {
        self.by_name.get(name).map(|entry| entry.handler.clone())
    }

    /// Owned clones so the caller can serialize without holding a borrow across an await.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.by_name
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect()
    }

    /// The tools `role`'s `tools/list` shows (`listed_for`), not every tool it may call.
    pub fn descriptors_listed_for(&self, role: CardRole) -> Vec<ToolDescriptor> {
        self.descriptors_listed_for_any(&[role])
    }

    pub fn descriptors_listed_for_any(&self, roles: &[CardRole]) -> Vec<ToolDescriptor> {
        self.by_name
            .values()
            .filter(|entry| {
                roles
                    .iter()
                    .any(|role| entry.descriptor.listed_for.contains(role))
            })
            .map(|entry| entry.descriptor.clone())
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(err.code, RpcError::FORBIDDEN);
        assert!(
            err.message.contains("Planner") && err.message.contains("ReportCard"),
            "error should mention allowed roles: {err:?}"
        );
        assert!(
            err.message.contains("Worker"),
            "error should mention actual role: {err:?}"
        );
    }

    fn fake_descriptor(name: &str, listed_for: &'static [CardRole]) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: "fake".to_string(),
            input_schema: json!({ "type": "object" }),
            annotations: None,
            roles: listed_for,
            listed_for,
        }
    }

    fn fake_handler(who: &'static str) -> ToolHandler {
        Arc::new(move |_ctx, _identity, _args| {
            Box::pin(async move { Ok(ToolResult::structured(json!({ "who": who }))) })
        })
    }

    #[test]
    fn descriptors_listed_for_any_returns_union_without_hidden_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(
            fake_descriptor("neige_planner_only", &[CardRole::Planner]),
            fake_handler("planner"),
        );
        registry.register(
            fake_descriptor("neige_worker_only", &[CardRole::Worker]),
            fake_handler("worker"),
        );
        registry.register(
            fake_descriptor("neige_shared_both", &[CardRole::Planner, CardRole::Worker]),
            fake_handler("shared"),
        );
        registry.register(
            fake_descriptor("neige_report_only", &[CardRole::ReportCard]),
            fake_handler("report"),
        );
        registry.register(
            fake_descriptor("neige_hidden_none", &[]),
            fake_handler("hidden"),
        );

        let mut names = registry
            .descriptors_listed_for_any(&[CardRole::Planner, CardRole::Worker])
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(
            names,
            vec![
                "neige_planner_only",
                "neige_shared_both",
                "neige_worker_only"
            ]
        );
    }

    #[test]
    fn track_history_drill_ins_are_hidden_but_registered() {
        let mut registry = ToolRegistry::new();
        crate::mcp_server::tools::register_default_tools(&mut registry);
        let hidden = [
            crate::mcp_server::tools::track_history::TOOL_TRACK_DIFF,
            crate::mcp_server::tools::track_history::TOOL_TRACK_SHOW,
            crate::mcp_server::tools::track_history::TOOL_TRACK_LOG,
            crate::mcp_server::tools::admin::TOOL_ADMIN_GC,
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
                    .descriptors_listed_for(role)
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

    /// Listing is never a grant (§7): every registered tool, kernel and compiled plugin native
    /// alike, is listed only for roles that may call it, and every tool may be called by someone.
    #[test]
    fn every_tool_is_listed_only_for_roles_that_may_call_it() {
        let descriptors = crate::mcp_server::build_default_registry().descriptors();
        assert!(
            descriptors.len() >= 40,
            "anti-vacuity: {} tools registered",
            descriptors.len()
        );
        assert!(
            descriptors
                .iter()
                .any(|d| crate::builtin_plugins::owner(&d.name).is_some()),
            "anti-vacuity: the compiled plugin natives are registered"
        );
        for descriptor in &descriptors {
            assert!(
                !descriptor.roles.is_empty(),
                "{}: no role may call it",
                descriptor.name
            );
            let unauthorized: Vec<_> = descriptor
                .listed_for
                .iter()
                .filter(|role| !descriptor.roles.contains(role))
                .collect();
            assert!(
                unauthorized.is_empty(),
                "{}: listed for {unauthorized:?}, which its roles {:?} do not admit",
                descriptor.name,
                descriptor.roles
            );
        }
    }
}
