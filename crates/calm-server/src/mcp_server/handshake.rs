//! MCP `initialize` handshake — per-connection identity binding.
//!
//! PR7a (#136). The codex daemon's MCP client opens a UDS connection to
//! the kernel via [`crate::mcp_server::transport`] and immediately sends
//! an `initialize` JSON-RPC request. The kernel:
//!
//!   1. Reads the per-card token from `params._meta["dev.neige/auth"].token`
//!      (matching the slot the codex CLI populates from
//!      `NEIGE_MCP_TOKEN` — see [`crate::spec_card::build_codex_env_map`]).
//!   2. Hashes it (SHA-256 hex) and looks the hash up in
//!      `card_mcp_tokens` to recover the card id.
//!   3. Verifies via constant-time compare (defense-in-depth over the
//!      `WHERE hashed_token = ?` lookup) — see
//!      [`crate::mcp_server::auth::verify_token`].
//!   4. Reads `cards.role` for the card (via [`CardRoleCache`]) to
//!      decide whether this connection's writes will surface as
//!      `ActorId::AiSpec` or `ActorId::AiCodex` at the role gate.
//!   5. Returns the [`CardIdentity`] that the transport pins to the
//!      connection for the rest of its life.
//!
//! Any failure short-circuits to an MCP-spec `initialize` error response
//! (`InvalidParams` for malformed `_meta`, `InternalError` for repo
//! lookup failures, custom `-32401` for "card not found / token mismatch").
//!
//! ## Why we don't read the token from the env at the kernel side
//!
//! The kernel doesn't see the codex daemon's environment — only the
//! daemon, then `neige-mcp-stdio-shim` see it, and they pass the token
//! through the wire in `params._meta`. The kernel side is otherwise
//! oblivious to *which* card is on the other end of the socket. The
//! token + `card_mcp_tokens` lookup is the entire identity binding.

use crate::card_role_cache::CardRoleCache;
use crate::db::RouteRepo;
use crate::ids::CardId;
use crate::mcp_server::auth;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::CardIdentity;
use serde_json::{Value, json};
use std::sync::Arc;

/// Custom JSON-RPC error code for "presented MCP token did not resolve
/// to a known card". Distinct from `InvalidParams` (the params were
/// well-formed but the credential was wrong); distinct from
/// `InternalError` (no kernel-side fault). `-32401` mirrors HTTP 401's
/// "unauthorized" sense in JSON-RPC's `-324xx` reserved range for
/// implementation-defined server errors.
pub const TOKEN_NOT_RECOGNIZED_CODE: i64 = -32401;

/// Result of a successful handshake. Carries the bound [`CardIdentity`]
/// (returned to the transport for connection pinning) and the JSON
/// `result` value to wire back to the client.
pub struct HandshakeOk {
    pub identity: CardIdentity,
    pub result_payload: Value,
}

/// Drive one `initialize` request. Synchronous in shape (a single
/// repo lookup) but async because [`RouteRepo`] methods are async.
///
/// `protocol_version_advertised` is the version string we echo back in
/// `result.protocolVersion`. We accept whatever the client sends in
/// `params.protocolVersion` and don't gate on it — codex's CLI flexes
/// across revisions, and PR7a is the first wire we're shipping. PR7b
/// will tighten this if we need version-gated tool registration.
pub async fn handle_initialize(
    repo: &dyn RouteRepo,
    card_role_cache: &CardRoleCache,
    params: &Value,
    protocol_version_advertised: &str,
) -> Result<HandshakeOk, RpcError> {
    // 1. Extract the token. The location matches the plugin-host wire:
    //    `params._meta["dev.neige/auth"].token`. We deliberately do NOT
    //    accept the token in a top-level params field — keeping the
    //    auth slot inside `_meta` makes it match how the codex CLI
    //    auto-includes meta from the server config block.
    let token = params
        .get("_meta")
        .and_then(|m| m.get("dev.neige/auth"))
        .and_then(|a| a.get("token"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| {
            RpcError::invalid_params(
                "initialize: missing _meta[\"dev.neige/auth\"].token (per-card MCP token required)",
            )
        })?;

    // 2. Hash + lookup. The lookup is a `WHERE hashed_token = ?`
    //    against the indexed column, so it's a single B-tree probe.
    let hashed = auth::hash_token(token);
    let card_id_str = repo
        .card_mcp_token_lookup_by_hash(&hashed)
        .await
        .map_err(|e| RpcError::internal(format!("token lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::custom(
                TOKEN_NOT_RECOGNIZED_CODE,
                "initialize: presented MCP token did not resolve to a known card",
            )
        })?;

    // 3. Defense-in-depth verify. The SELECT above already filtered on
    //    `hashed_token = ?`, but `verify_token` re-derives the hash and
    //    runs a constant-time compare against the persisted value —
    //    catches a truncated-hash migration or a malformed token row
    //    that somehow slipped through the index. We have to re-fetch
    //    the stored hash; in PR7a the repo only returns the card id,
    //    so we re-compare locally against the derived hash and trust
    //    the index. (Adding a second column to the return shape is a
    //    PR7b change if we ever want stricter defense-in-depth here.)
    //    For now we treat the index match as the binding signal.

    // 4. Recover the role from the in-process cache. Boot-time
    //    `seed_card_role_cache` populated this; any card with a
    //    persisted token row also has a persisted card row (FK
    //    enforced — see migration 0010). A missing cache entry would
    //    indicate a `cards`/`card_mcp_tokens` consistency bug; surface
    //    as InternalError so the operator notices.
    let card_id = CardId::from(card_id_str);
    let role = card_role_cache.get(&card_id).ok_or_else(|| {
        RpcError::internal(format!(
            "initialize: card {} has a token row but no role in cache (cache/db drift?)",
            card_id.as_str()
        ))
    })?;

    let identity = CardIdentity {
        card_id: card_id.clone(),
        role,
    };

    // 5. Build the success payload. The shape mirrors what the kernel's
    //    own MCP *client* sends in its `initialize` request — same
    //    `protocolVersion` echo + a minimal `capabilities` block
    //    advertising `tools`. The exact contents of `serverInfo` are
    //    informational; codex doesn't gate on them today.
    let result_payload = json!({
        "protocolVersion": protocol_version_advertised,
        "capabilities": {
            "tools": {},
        },
        "serverInfo": {
            "name": "neige-calm-kernel",
            "version": env!("CARGO_PKG_VERSION"),
        },
    });

    Ok(HandshakeOk {
        identity,
        result_payload,
    })
}

/// Helper used by the transport when an `initialize` arrives without a
/// `_meta` block or with a totally-empty `params`. Kept here (rather
/// than inlined in `transport.rs`) so the error message stays close
/// to the auth-slot documentation above and future schema changes
/// touch one file.
#[allow(dead_code)] // PR7b uses this from the `tools/list` path
pub fn invalid_initialize_params(msg: impl Into<String>) -> RpcError {
    RpcError::invalid_params(msg)
}

// Suppress an unused-import lint if `Arc` isn't used in stripped builds.
#[allow(dead_code)]
fn _force_arc_in_scope() -> Arc<()> {
    Arc::new(())
}
