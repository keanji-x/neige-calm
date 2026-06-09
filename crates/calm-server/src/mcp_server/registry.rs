//! Tool registry + per-connection app context for the kernel's MCP
//! server. PR7a (#136).
//!
//! ## What lives here
//!
//! [`ToolRegistry`] is a name -> handler map the transport consults on
//! every `tools/call`. PR7a registers the three emit handlers
//! (`calm.dispatch_request`, `calm.task_completed`, `calm.task_failed`);
//! PR7b registers `calm.update_wave_state` / `calm.get_wave_state` /
//! `calm.update_task_meta` and #229 PR B the three `calm.report.*` tools.
//! Each handler is `Send + Sync + 'static` and receives:
//!
//!   * an [`AppContext`] — repo, event bus, role cache, and the codex
//!     home parent (already on `AppState`, factored down to the minimum
//!     surface the MCP server needs so the registry doesn't take a
//!     full `AppState` clone);
//!   * a [`ToolCallIdentity`] — resolved for each `tools/call` from
//!     `_meta.threadId` via runtime thread attribution, with a token
//!     fallback for calls that have no thread metadata.

use crate::db::RouteRepo;
use crate::event::EventBus;
use crate::ids::{ActorId, CardId};
use crate::mcp_server::framing::RpcError;
use crate::model::CardRole;
use crate::state::WriteContext;
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
    pub wave_id: Option<String>,
}

impl CardIdentity {
    /// Convert to the `ActorId` the role gate will see for any
    /// `write_with_event` call this connection drives. Mapping:
    ///
    /// * `CardRole::Spec`       → [`ActorId::AiSpec`]
    /// * `CardRole::Worker`     → [`ActorId::AiCodex`]
    /// * `CardRole::Plain`      → unreachable here (Plain cards have no
    ///   token row by construction; see `card_with_codex_create_tx`).
    ///   We still map it to `AiCodex` for total-function-ness; the gate
    ///   itself denies the empty-CardId case, and a Plain card reaching
    ///   this code path would indicate a token-row leak we want surfaced
    ///   as a clear `Forbidden` rather than a panic.
    /// * `CardRole::ReportCard` → unreachable here too (report cards are
    ///   read-only kernel-projected payload and don't get an MCP token).
    ///   Mapped to `AiCodex` for the same total-function reason — the
    ///   role gate refuses report-card actors as soon as they try to
    ///   emit `WaveUpdated`, so a token-row leak surfaces as a clear
    ///   `Forbidden` rather than a panic.
    pub fn to_actor_id(&self) -> ActorId {
        match self.role {
            CardRole::Spec => ActorId::AiSpec(self.card_id.clone()),
            CardRole::Worker | CardRole::Plain | CardRole::ReportCard => {
                ActorId::AiCodex(self.card_id.clone())
            }
        }
    }
}

/// Identity resolved for one MCP `tools/call` from the request's
/// `_meta.threadId`. All fields except `wave_id` are required because
/// every authorized tool call must map to a concrete persisted thread
/// row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallIdentity {
    pub card_id: String,
    pub role: CardRole,
    pub wave_id: Option<String>,
    pub thread_id: String,
}

impl ToolCallIdentity {
    pub fn to_actor_id(&self) -> ActorId {
        let card_id = CardId::from(self.card_id.clone());
        match self.role {
            CardRole::Spec => ActorId::AiSpec(card_id),
            CardRole::Worker | CardRole::Plain | CardRole::ReportCard => ActorId::AiCodex(card_id),
        }
    }
}

/// PR7b (#136) — soft role gate for spec-only MCP tools.
///
/// The *real* boundary is [`crate::role_gate::enforce_role`], which runs
/// inside every eventized write and refuses any cross-role attempt with
/// a transactional rollback. This helper is purely UX: it short-circuits
/// at the MCP-tool entry so a worker card calling
/// `calm.update_wave_state` gets a deterministic
/// `-32602 spec-only tool` error code instead of the more opaque
/// `-32403 forbidden: only spec cards may emit wave.updated` that the
/// in-tx gate would otherwise produce after speculatively reading the
/// wave row.
///
/// Use at the top of every spec-only handler. `calm.get_wave_state` is
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
    /// Event bus for `write_with_event_typed` broadcasts.
    pub events: EventBus,
    /// #480 PR2 write-surface caches shared with REST/worker paths.
    pub write: WriteContext,
    /// Optional server-wide MCP daemon token hash. When present, the
    /// initialize handshake accepts this as daemon trust without binding a
    /// legacy per-card identity.
    pub daemon_token_hash: Option<String>,
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
}

pub fn read_only_annotations() -> Value {
    json!({ "readOnlyHint": true })
}

pub fn write_no_approval_annotations() -> Value {
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
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity_with_role(role: CardRole) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: "card-1".to_string(),
            role,
            wave_id: Some("wave-1".to_string()),
            thread_id: "thread-1".to_string(),
        }
    }

    #[test]
    fn require_role_any_accepts_any_allowed_role_and_rejects_others() {
        let allowed = [CardRole::Spec, CardRole::Worker];

        assert!(require_role_any(&identity_with_role(CardRole::Spec), &allowed).is_ok());
        assert!(require_role_any(&identity_with_role(CardRole::Worker), &allowed).is_ok());

        let err = require_role_any(&identity_with_role(CardRole::Plain), &allowed)
            .expect_err("plain must be denied");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(
            err.message.contains("Spec") && err.message.contains("Worker"),
            "error should mention allowed roles: {err:?}"
        );
        assert!(
            err.message.contains("Plain"),
            "error should mention actual role: {err:?}"
        );
    }
}
