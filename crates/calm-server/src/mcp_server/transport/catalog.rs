//! Shared catalog production; connection identity and plugin admission remain authoritative.
use super::*;

/// One discovery owner for MCP listing and authenticated CLI lookup.
/// Bootstrap remains limited to the daemon's initial MCP discovery; CLI accepts card-bound callers only.
pub(crate) async fn tool_descriptors_for_connection(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    connection_identity: &ConnectionIdentity,
    thread_id: Option<&str>,
) -> Result<Vec<ToolDescriptor>, RpcError> {
    let descriptors = match connection_identity {
        ConnectionIdentity::DaemonTrust => match thread_id {
            Some(tid) => match resolve_thread_identity(ctx, Some(tid), "tools/list")
                .await
                .ok()
            {
                Some(identity) => tool_descriptors_for_identity(ctx, registry, &identity).await?,
                None => bootstrap_tool_descriptors(ctx, registry).await,
            },
            // Initial discovery may precede thread attribution. The catalog covers all
            // running plugins; tools/call still requires a live role and Track binding.
            None => bootstrap_tool_descriptors(ctx, registry).await,
        },
        ConnectionIdentity::CardBound(bound) => match thread_id {
            Some(tid) => match resolve_thread_identity(ctx, Some(tid), "tools/list")
                .await
                .ok()
            {
                Some(identity) if same_bound_session(&identity, bound) => {
                    tool_descriptors_for_identity(ctx, registry, &identity).await?
                }
                Some(identity) => {
                    warn_cross_session_reject(tid, &identity, bound);
                    Vec::new()
                }
                _ => Vec::new(),
            },
            None => {
                let identity = card_bound_identity(ctx, bound, "tools/list").await?;
                tool_descriptors_for_identity(ctx, registry, &identity).await?
            }
        },
    };
    Ok(descriptors)
}

/// The catalog digest in the kernel MCP entry's generation (#2014): a digest of exactly what a
/// bootstrap `tools/list` serves, sorted by name. Equal catalogs give equal values, so a restart
/// over an unchanged running plugin set rewrites nothing; any served change (a tool added or
/// removed, or a reload that changed a schema) gives a new one.
pub(crate) async fn bootstrap_catalog_digest(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
) -> String {
    let mut tools: Vec<Value> = bootstrap_tool_descriptors(ctx, registry)
        .await
        .into_iter()
        .map(ToolDescriptor::into_mcp_value)
        .collect();
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let canonical = serde_json::to_vec(&tools).expect("a JSON value always serializes");
    let digest = format!("{:x}", Sha256::digest(&canonical));
    digest[..32].to_string()
}

/// One production, two views (#2289 D2): what a resolved caller may call, and the subset its
/// `tools/list` shows. Both come from [`SessionCatalog::of`], so `listed` can never name a tool
/// the catalog does not.
pub(crate) struct SessionCatalog {
    pub(crate) role: CardRole,
    /// Every tool this caller may call, the managed-track restriction applied.
    callable: Vec<ToolDescriptor>,
    /// Kernel tools and running, in-scope natives whose declared `roles` exclude this role: served
    /// to someone, so naming the roles reveals nothing (kernel tool names are public, #2003 K7).
    /// A plugin tool outside the Track's scope or not running is in neither set: it stays
    /// undiscoverable, as at `tools/call`.
    role_refused: Vec<ToolDescriptor>,
}

impl SessionCatalog {
    /// Kernel tools and compiled natives whose `roles` hold the caller's role, natives filtered by
    /// running state and the Track's plugin scope; the scope's manifest tools for a
    /// [`PLUGIN_TOOL_ROLES`] role; then the managed-track restriction, as `tools/call` applies it.
    async fn of(
        ctx: &Arc<AppContext>,
        registry: &ToolRegistry,
        identity: &ToolCallIdentity,
    ) -> Result<Self, RpcError> {
        let scope = plugin_scope_for_track(ctx, identity.track_id.as_deref()).await;
        let mut served = registry.descriptors();
        extend_plugin_tool_descriptors_for_role(ctx, &mut served, identity.role, &scope).await;
        let (callable, role_refused): (Vec<_>, Vec<_>) = served
            .into_iter()
            .partition(|descriptor| descriptor.roles.contains(&identity.role));
        Ok(Self {
            role: identity.role,
            callable: filter_profile(ctx, &identity.card_id, callable).await?,
            role_refused,
        })
    }

    /// The `tools/list` view: the callable tools that declare `listed_for` this role.
    pub(crate) fn listed(&self) -> impl Iterator<Item = &ToolDescriptor> {
        self.callable
            .iter()
            .filter(|descriptor| descriptor.listed_for.contains(&self.role))
    }

    pub(crate) fn callable(&self) -> &[ToolDescriptor] {
        &self.callable
    }

    /// The declared `roles` of `name` when it is served, but not to this role.
    pub(crate) fn roles_refusing(&self, name: &str) -> Option<&'static [CardRole]> {
        self.role_refused
            .iter()
            .find(|descriptor| descriptor.name == name)
            .map(|descriptor| descriptor.roles)
    }
}

/// The catalog of a card-bound session, resolved as its `tools/call` resolves it.
pub(crate) async fn card_bound_catalog(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    bound: &CardIdentity,
) -> Result<SessionCatalog, RpcError> {
    let identity = card_bound_identity(ctx, bound, "tools/list").await?;
    SessionCatalog::of(ctx, registry, &identity).await
}

/// What `tools/list` shows a resolved caller: the listed view of its catalog.
async fn tool_descriptors_for_identity(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: &ToolCallIdentity,
) -> Result<Vec<ToolDescriptor>, RpcError> {
    let catalog = SessionCatalog::of(ctx, registry, identity).await?;
    Ok(catalog.listed().cloned().collect())
}

/// The transport's `-32601` for a `tools/call` name this caller cannot reach. It lists the names
/// its `tools/list` shows, so the error is the same for an unknown name and an out-of-scope one:
/// it is not an existence oracle. A built-in plugin native refused by `require_bound` keeps its
/// bare `-32601` (#2003 KNOWN GAP K7). Hidden tools (callable, not in `tools/list`) are not listed.
pub(super) async fn unknown_tool_error(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: &ToolCallIdentity,
    name: &str,
) -> RpcError {
    let descriptors = match tool_descriptors_for_identity(ctx, registry, identity).await {
        Ok(descriptors) => descriptors,
        Err(error) => return error,
    };
    let mut visible: Vec<String> = descriptors
        .into_iter()
        .map(|descriptor| descriptor.name)
        .collect();
    visible.sort();
    RpcError::method_not_found(&format!(
        "tools/call: {name}; tools visible to this session: {}",
        visible.join(", ")
    ))
}

async fn filter_profile(
    ctx: &AppContext,
    card_id: &str,
    mut descriptors: Vec<ToolDescriptor>,
) -> Result<Vec<ToolDescriptor>, RpcError> {
    if crate::managed_track::reports_only_card(ctx, card_id).await? {
        descriptors
            .retain(|descriptor| crate::managed_track::report_planning_tool(&descriptor.name));
    }
    Ok(descriptors)
}

/// The plugin that serves a discovered tool: its id and the kind its manifest declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginOwner {
    pub(crate) id: String,
    pub(crate) kind: Option<ToolKind>,
}

/// Who serves `name`: `None` for a kernel tool. A built-in native answers by its compiled owner, a
/// minted name by [`plugin_tool_route`] in dispatch's order: the running plugins (fenced against
/// minting one name), then the installed ones, so a plugin that stops between listing and this
/// lookup still names itself. A name nobody serves is an error, never "kernel".
pub(crate) fn tool_owner(
    registry: &ToolRegistry,
    plugins: Option<&crate::plugin_host::PluginRegistry>,
    running_ids: &BTreeSet<String>,
    name: &str,
) -> Result<Option<PluginOwner>, RpcError> {
    if let Some(plugin) = crate::builtin_plugins::owner(name) {
        return Ok(Some(PluginOwner {
            id: plugin.manifest().id.clone(),
            kind: None,
        }));
    }
    if registry.lookup(name).is_some() {
        return Ok(None);
    }
    let route = match plugins {
        Some(plugins) => match plugin_tool_route(plugins, name, running_ids)? {
            Some(route) => Some(route),
            None => {
                let installed = plugins.list().into_iter().map(|m| m.id).collect();
                plugin_tool_route(plugins, name, &installed)?
            }
        },
        None => None,
    };
    route
        .map(|(id, _, kind)| Some(PluginOwner { id, kind }))
        .ok_or_else(|| {
            RpcError::internal(format!(
                "tool `{name}` has no owner; the plugin set changed while listing, retry"
            ))
        })
}
