//! MCP `initialize` handshake — daemon-level trust.
//!
//! PR7a (#136). The codex daemon's MCP client opens a UDS connection to
//! the kernel via [`crate::mcp_server::transport`] and immediately sends
//! an `initialize` JSON-RPC request. The kernel:
//!
//!   1. Reads the per-card token from `params._meta["dev.neige/auth"].token`
//!      (matching the slot the codex CLI populates from
//!      `NEIGE_MCP_TOKEN` — see [`crate::spec_card::build_codex_env_map`]).
//!   2. Hashes it (SHA-256 hex) and looks the hash up in
//!      `card_mcp_tokens`.
//!   3. Verifies via constant-time compare (defense-in-depth over the
//!      `WHERE hashed_token = ?` lookup) — see
//!      [`crate::mcp_server::auth::verify_token`].
//!   4. Returns a daemon-trust marker plus the token-bound card identity
//!      as a temporary fallback for legacy callers. Modern tools/call
//!      identity is still resolved first from `_meta.threadId`.
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
//! token + `card_mcp_tokens` lookup is only daemon trust in PR3b.

use crate::db::RouteRepo;
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

/// Result of a successful handshake. Carries daemon-level trust, a
/// temporary legacy card identity fallback, and the JSON `result` value
/// to wire back to the client.
pub struct HandshakeOk {
    pub daemon_trust: bool,
    pub legacy_identity: Option<CardIdentity>,
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
    //    PR7a.1 (#136 followup) — the repo returns `(card_id,
    //    stored_hash)` so step 3 can actually run the constant-time
    //    compare promised in the doc above.
    let hashed = auth::hash_token(token);
    let (card_id_str, stored_hash) = repo
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
    //    that somehow slipped through the index. PR7a.1 (#136 followup)
    //    wired this in: a mismatch returns the same `-32401`
    //    "TOKEN_NOT_RECOGNIZED" error code as the lookup miss so
    //    timing analysis can't distinguish "no row" from "row but
    //    hash drifted".
    if !auth::verify_token(token, &stored_hash) {
        return Err(RpcError::custom(
            TOKEN_NOT_RECOGNIZED_CODE,
            "initialize: presented MCP token did not resolve to a known card",
        ));
    }

    let legacy_identity = match repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("legacy card lookup: {e}")))?
    {
        Some(card) => {
            let role = repo
                .card_role_get(&card_id_str)
                .await
                .map_err(|e| RpcError::internal(format!("legacy card role lookup: {e}")))?
                .ok_or_else(|| RpcError::internal("legacy card role lookup: missing role"))?;
            Some(CardIdentity {
                card_id: card.id,
                role,
                wave_id: Some(card.wave_id.as_str().to_string()),
            })
        }
        None => None,
    };

    // 4. Build the success payload. The shape mirrors what the kernel's
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
        daemon_trust: true,
        legacy_identity,
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

#[cfg(test)]
mod tests {
    //! PR7a.1 (#136 followup) — defense-in-depth regression tests for
    //! the `verify_token` step in the handshake. These don't drive a
    //! real `RouteRepo`; they just pin the contract on
    //! `auth::verify_token` so a future refactor that swaps the hash
    //! algorithm without updating the handshake gets caught here.

    use crate::mcp_server::auth;

    #[test]
    fn verify_token_rejects_mismatched_stored_hash() {
        // Catches the regression PR7a.1 closed: the handshake used to
        // skip `verify_token` and trust the `WHERE hashed_token = ?`
        // index match. If a future migration ever stores something
        // other than `sha256(token)` in the column, this assertion
        // makes the drift loud at test time.
        let token = "deadbeefcafebabe".repeat(4); // 64-char hex stand-in
        let stored_hash = auth::hash_token("a-different-secret");
        assert!(
            !auth::verify_token(&token, &stored_hash),
            "verify_token must reject a stored hash that doesn't match the presented token"
        );
    }

    #[test]
    fn verify_token_accepts_matching_stored_hash() {
        let token = auth::CardMcpToken::generate();
        let stored_hash = auth::hash_token(token.as_str());
        assert!(
            auth::verify_token(token.as_str(), &stored_hash),
            "verify_token must accept the round-trip pair"
        );
    }
}
