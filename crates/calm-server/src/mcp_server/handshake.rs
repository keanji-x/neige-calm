//! MCP `initialize` handshake — explicit per-connection identity.
//!
//! PR7a (#136). The codex daemon's MCP client opens a UDS connection to
//! the kernel via [`crate::mcp_server::transport`] and immediately sends
//! an `initialize` JSON-RPC request. The kernel:
//!
//!   1. Reads the per-session token from `params._meta["dev.neige/auth"].token`
//!      (matching the slot the codex CLI populates from
//!      `NEIGE_MCP_TOKEN`).
//!   2. Hashes it (SHA-256 hex) and looks the hash up in
//!      `worker_sessions`.
//!   3. Verifies via constant-time compare (defense-in-depth over the
//!      `WHERE hashed_token = ?` lookup) — see
//!      [`crate::mcp_server::auth::verify_token`].
//!   4. Returns a [`ConnectionIdentity`] that fixes the connection into
//!      one of two modes:
//!      * daemon token match → [`ConnectionIdentity::DaemonTrust`], which
//!        has no bound card and requires `_meta.threadId` per call;
//!      * per-session token match → [`ConnectionIdentity::CardBound`], which
//!        may omit `_meta.threadId` and otherwise must resolve it back to
//!        the bound session.
//!
//! Any failure short-circuits to an MCP-spec `initialize` error response
//! (`InvalidParams` for malformed `_meta`, `InternalError` for repo
//! lookup failures, custom `-32401` for "session not found / token mismatch").
//!
//! ## Why we don't read the token from the env at the kernel side
//!
//! The kernel doesn't see the codex daemon's environment — only the
//! daemon, then `neige-mcp-stdio-shim` see it, and they pass the token
//! through the wire in `params._meta`. The kernel side is otherwise
//! oblivious to *which* card is on the other end of the socket. The
//! token + active `worker_sessions` lookup is the connection credential.

use crate::db::RouteRepo;
use crate::mcp_server::auth;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{CardIdentity, ConnectionIdentity};
use calm_types::worker::{Principal, WorkerSessionId};
use serde_json::{Value, json};

/// Custom JSON-RPC error code for "presented MCP token did not resolve
/// to a known session". Distinct from `InvalidParams` (the params were
/// well-formed but the credential was wrong); distinct from
/// `InternalError` (no kernel-side fault). `-32401` mirrors HTTP 401's
/// "unauthorized" sense in JSON-RPC's `-324xx` reserved range for
/// implementation-defined server errors.
pub const TOKEN_NOT_RECOGNIZED_CODE: i64 = -32401;

/// Result of a successful handshake. Carries the explicit connection
/// identity and the JSON `result` value to wire back to the client.
pub struct HandshakeOk {
    pub connection_identity: ConnectionIdentity,
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
    daemon_token_hash: Option<&str>,
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
                "initialize: missing _meta[\"dev.neige/auth\"].token (per-session MCP token required)",
            )
        })?;

    let result_payload = initialize_result_payload(protocol_version_advertised);

    if let Some(stored_hash) = daemon_token_hash
        && auth::verify_token(token, stored_hash)
    {
        return Ok(HandshakeOk {
            connection_identity: ConnectionIdentity::DaemonTrust,
            result_payload,
        });
    }

    // 2. Hash + lookup. The lookup is a `WHERE mcp_token_hash = ?`
    //    against active worker sessions, so stale/exited/superseded
    //    sessions collapse to the same auth failure as an unknown token.
    let hashed = auth::hash_token(token);
    let session = repo
        .session_get_by_active_token_hash(&hashed)
        .await
        .map_err(|e| RpcError::internal(format!("token lookup: {e}")))?
        .ok_or_else(token_not_recognized)?;

    // 3. Defense-in-depth verify. The SELECT above already filtered on
    //    `mcp_token_hash = ?`, but `verify_token` re-derives the hash and
    //    runs a constant-time compare against the persisted value —
    //    catches a truncated-hash migration or a malformed token row
    //    that somehow slipped through the index. A mismatch returns the
    //    same `-32401` as a lookup miss so timing analysis can't
    //    distinguish "no row" from "row but hash drifted".
    let stored_hash = session
        .mcp_token_hash
        .as_deref()
        .ok_or_else(token_not_recognized)?;
    if !auth::verify_token(token, stored_hash) {
        return Err(token_not_recognized());
    }

    // 4. Recover the card-derived actor identity from the authenticated
    //    session. The persisted event actor remains card-shaped; the
    //    session Principal is threaded alongside it for the PR7 gate work.
    let card = repo
        .card_identity_get_by_session(session.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("session-bound card lookup: {e}")))?
        .ok_or_else(token_not_recognized)?;
    if card.wave_id != session.wave_id {
        return Err(token_not_recognized());
    }
    let principal = Principal::Agent {
        session_id: WorkerSessionId::from(session.id.as_str()),
        wave_id: session.wave_id.clone(),
        cove_id: card.cove_id.clone(),
    };
    let card_identity = CardIdentity {
        card_id: card.card_id,
        role: card.role,
        session_id: session.id.as_str().to_string(),
        wave_id: Some(session.wave_id.as_str().to_string()),
        cove_id: card.cove_id.as_str().to_string(),
    };
    debug_assert_eq!(card_identity.to_principal(), Some(principal));
    let connection_identity = ConnectionIdentity::CardBound(card_identity);

    // 5. Build the success payload. The shape mirrors what the kernel's
    //    own MCP *client* sends in its `initialize` request — same
    //    `protocolVersion` echo + a minimal `capabilities` block
    //    advertising `tools`. The exact contents of `serverInfo` are
    //    informational; codex doesn't gate on them today.
    Ok(HandshakeOk {
        connection_identity,
        result_payload,
    })
}

fn token_not_recognized() -> RpcError {
    RpcError::custom(
        TOKEN_NOT_RECOGNIZED_CODE,
        "initialize: presented MCP token did not resolve to a known session",
    )
}

fn initialize_result_payload(protocol_version_advertised: &str) -> Value {
    json!({
        "protocolVersion": protocol_version_advertised,
        "capabilities": {
            "tools": {},
        },
        "serverInfo": {
            "name": "neige-calm-kernel",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
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
