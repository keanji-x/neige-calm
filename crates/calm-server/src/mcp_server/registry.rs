//! Tool registry + per-connection app context for the kernel's MCP
//! server. PR7a (#136).
//!
//! ## What lives here
//!
//! [`ToolRegistry`] is a name -> handler map the transport consults on
//! every `tools/call`. Today PR7a registers three handlers
//! (`calm.dispatch_request`, `calm.task_completed`, `calm.task_failed`);
//! PR7b will register `calm.update_wave_state` / `calm.get_wave_state`
//! and PR8 will register `calm.wait_for_events`. Each handler is
//! `Send + Sync + 'static` and receives:
//!
//!   * an [`AppContext`] — repo, event bus, role cache, and the codex
//!     home parent (already on `AppState`, factored down to the minimum
//!     surface the MCP server needs so the registry doesn't take a
//!     full `AppState` clone);
//!   * a [`CardIdentity`] — *bound at the handshake*. The transport
//!     resolves which card minted the per-connection token in
//!     `handshake.rs`, and that identity rides on the connection for
//!     the rest of its lifetime. Tools never read the card id out of
//!     the JSON-RPC params; trying to "spoof" via params is a no-op
//!     because the registry shadow-overrides whatever the tool sees.
//!
//! ## Why identity is connection-level (not param-level)
//!
//! The codex daemon's MCP client multiplexes every tool call on one
//! socket, but a single daemon is bound to exactly one card (its
//! `NEIGE_CARD_ID` env). Threading a card_id through every tool param
//! would let a compromised plugin claim a different card identity by
//! editing the JSON it sends — a clear sandbox break. By resolving the
//! identity once at `initialize` (from the per-card MCP token row), we
//! make the binding cryptographic + per-connection: the token alone is
//! sufficient and any in-band `card_id` field is ignored.

use crate::card_role_cache::CardRoleCache;
use crate::db::RouteRepo;
use crate::event::EventBus;
use crate::event_cursor::EventCursorCache;
use crate::ids::{ActorId, CardId};
use crate::mcp_server::framing::RpcError;
use crate::model::CardRole;
use serde_json::Value;
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
/// metadata before reporting), so it skips this gate.
pub fn require_role(identity: &CardIdentity, required: CardRole) -> Result<(), RpcError> {
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
    /// Event bus for `write_with_event_typed` broadcasts. PR8's
    /// `calm.wait_for_events` will additionally `subscribe_filtered` on
    /// this same bus.
    pub events: EventBus,
    /// Role cache, threaded through to `write_with_event_typed` so the
    /// in-tx role gate runs without a DB lookup.
    pub card_role_cache: CardRoleCache,
    /// PR8 (#136) — per-card event cursor cache. Used by
    /// `calm.wait_for_events` (this module's `tools/wait.rs`) and by
    /// the `/internal/codex/pending_events` HTTP fallback so a wait
    /// call defaults `since` to wherever the last call left off for
    /// that card.
    pub event_cursor_cache: EventCursorCache,
}

/// Boxed future returned by a tool handler. Handlers are async fns;
/// `BoxFuture<'static, …>` keeps the registry's hash-map values
/// object-safe.
pub type ToolHandlerFuture =
    Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send + 'static>>;

/// One tool's invocation contract. The transport calls this with the
/// per-connection [`CardIdentity`] (immutable, established at
/// handshake) and the raw `arguments` JSON value from the
/// `tools/call` params. Handlers are responsible for shape-validating
/// `arguments` and translating internal errors into [`RpcError`]
/// (almost always `RpcError::invalid_params` / `RpcError::internal`).
pub type ToolHandler =
    Arc<dyn Fn(Arc<AppContext>, CardIdentity, Value) -> ToolHandlerFuture + Send + Sync>;

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
}

/// Map of tool name → handler + descriptor. Populated by
/// [`build_default_registry`] (PR7a wires three tools; PR7b/PR8 will
/// extend it).
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
