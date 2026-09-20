//! MCP `initialize` handshake — explicit per-connection identity. The token in
//! `params._meta["dev.neige/auth"].token` plus an active `worker_sessions` lookup is the
//! connection credential; the kernel never sees the codex daemon's environment.

use crate::db::RouteRepo;
use crate::mcp_server::auth;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{CardIdentity, ConnectionIdentity};
use crate::session_projection_repo::AgentProvider;
use calm_types::worker::{Principal, WorkerProviderKind, WorkerSessionId};
use serde_json::{Value, json};

/// Custom JSON-RPC error code for "presented MCP token did not resolve to a known session";
/// `-32401` mirrors HTTP 401 in JSON-RPC's implementation-defined range.
pub const TOKEN_NOT_RECOGNIZED_CODE: i64 = -32401;

/// Result of a successful handshake.
pub struct HandshakeOk {
    pub connection_identity: ConnectionIdentity,
    pub result_payload: Value,
}

/// Drive one `initialize` request. `params.protocolVersion` is accepted without gating;
/// `protocol_version_advertised` is echoed back in `result.protocolVersion`.
pub async fn handle_initialize(
    repo: &dyn RouteRepo,
    daemon_token_hash: Option<&str>,
    params: &Value,
    protocol_version_advertised: &str,
) -> Result<HandshakeOk, RpcError> {
    // 1. Extract the token from `params._meta["dev.neige/auth"].token`; a top-level params field
    //    is deliberately NOT accepted.
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

    // 2. Hash + lookup against ACTIVE worker sessions, so stale/exited/superseded sessions
    //    collapse to the same auth failure as an unknown token.
    let hashed = auth::hash_token(token);
    let session = repo
        .session_get_by_active_token_hash(&hashed)
        .await
        .map_err(|e| RpcError::internal(format!("token lookup: {e}")))?
        .ok_or_else(token_not_recognized)?;

    // 3. Defense-in-depth constant-time verify; a mismatch returns the same `-32401` as a lookup
    //    miss so timing analysis can't distinguish the two.
    let stored_hash = session
        .mcp_token_hash
        .as_deref()
        .ok_or_else(token_not_recognized)?;
    if !auth::verify_token(token, stored_hash) {
        return Err(token_not_recognized());
    }

    // 4. Recover the card/session binding metadata for `require_role` gating, `Principal`, and
    //    track/card resolution — not for actor minting.
    let card = repo
        .card_identity_get_by_session(session.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("session-bound card lookup: {e}")))?
        .ok_or_else(token_not_recognized)?;
    if card.track_id != session.track_id {
        return Err(token_not_recognized());
    }
    let principal = Principal::Agent {
        session_id: WorkerSessionId::from(session.id.as_str()),
        track_id: session.track_id.clone(),
        area_id: card.area_id.clone(),
    };
    let card_identity = CardIdentity {
        card_id: card.card_id,
        role: card.role,
        provider: agent_provider_from_worker_provider(session.provider),
        session_id: session.id.as_str().to_string(),
        track_id: Some(session.track_id.as_str().to_string()),
        area_id: card.area_id.as_str().to_string(),
    };
    debug_assert_eq!(card_identity.to_principal(), Some(principal));
    let connection_identity = ConnectionIdentity::CardBound(card_identity);

    // 5. Build the success payload; `serverInfo` is informational.
    Ok(HandshakeOk {
        connection_identity,
        result_payload,
    })
}

fn agent_provider_from_worker_provider(provider: WorkerProviderKind) -> AgentProvider {
    match provider {
        WorkerProviderKind::Claude => AgentProvider::Claude,
        WorkerProviderKind::Codex | WorkerProviderKind::Terminal => AgentProvider::Codex,
    }
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
    //! Pins the `verify_token` contract so a refactor that swaps the hash algorithm without
    //! updating the handshake gets caught here.

    use crate::mcp_server::auth;

    #[test]
    fn verify_token_rejects_mismatched_stored_hash() {
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
