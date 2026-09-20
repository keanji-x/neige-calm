//! Declared actor plumbing: `X-Calm-Actor` → middleware → `Actor` extension → `write_with_event_typed`.
//! Not authenticated — a declared identity, not a security boundary; `kernel` and `plugin:*` are refused from the header so REST callers cannot spoof server-internal writes.

use axum::{
    body::Body,
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, Request, request::Parts},
    middleware::Next,
    response::Response,
};
use std::net::SocketAddr;

use crate::error::CalmError;
use crate::ids::{ActorId, CardId};

/// Declared identity of an event producer; defaults to `"user"` when the header is absent. Not authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor(pub String);

impl Actor {
    /// The default actor used when no `X-Calm-Actor` header is present.
    pub const DEFAULT: &'static str = "user";

    /// HTTP header carrying the declared actor.
    pub const HEADER: &'static str = "X-Calm-Actor";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Map the header actor to a typed [`ActorId`]. Only `"user"` and `"ai:codex"` are header-reachable; anything else maps to `User` as defence in depth.
    pub fn to_actor_id(&self) -> ActorId {
        if self.0 == "user" {
            return ActorId::User;
        }
        if self.0 == "ai:codex" {
            // No card context at REST entry — the carried CardId stays empty; the write sites reattribute downstream once they have a card.
            return ActorId::AiCodex(CardId::from(""));
        }
        // Defensive default: attribute as User rather than synthesize a Kernel/Plugin identity from an attacker-controlled header.
        ActorId::User
    }
}

/// Reserved actors (`kernel`, `plugin:*`) are rejected from the header: allowing them would let any REST caller spoof kernel writes or impersonate plugins.
fn validate_header_actor(raw: &str) -> Result<Actor, CalmError> {
    // Empty -> missing; the caller already collapses that case, this is defense in depth.
    if raw.is_empty() {
        return Ok(Actor(Actor::DEFAULT.to_string()));
    }

    if raw == "user" {
        return Ok(Actor("user".to_string()));
    }

    if raw == "kernel" {
        return Err(CalmError::BadRequest(
            "X-Calm-Actor: `kernel` is reserved for server-internal writes".into(),
        ));
    }

    if let Some(id) = raw.strip_prefix("ai:") {
        if is_valid_actor_id(id) {
            return Ok(Actor(format!("ai:{id}")));
        }
        return Err(CalmError::BadRequest(format!(
            "X-Calm-Actor: invalid `ai:<id>` — id must be 1-64 chars matching [a-z0-9-], got `{id}`"
        )));
    }

    if raw.starts_with("plugin:") {
        return Err(CalmError::BadRequest(
            "X-Calm-Actor: `plugin:<id>` is reserved for the kernel's plugin callback dispatcher"
                .into(),
        ));
    }

    Err(CalmError::BadRequest(format!(
        "X-Calm-Actor: unrecognized actor `{raw}` — expected `user` or `ai:<id>`"
    )))
}

/// `[a-z0-9-]{1,64}` — kept tight on purpose. Headers carry attacker-controlled
/// bytes; the actor string lands verbatim in the `events.actor` column and is
/// echoed back over WS, so the smaller the alphabet the better.
fn is_valid_actor_id(id: &str) -> bool {
    let len = id.len();
    if !(1..=64).contains(&len) {
        return false;
    }
    id.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Axum middleware: read `X-Calm-Actor`, validate, stash an [`Actor`] in request extensions; invalid headers short-circuit with a 400.
pub async fn actor_middleware(
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, CalmError> {
    // A non-UTF-8 header is a malformed value — 400, not silently default-to-user.
    let raw = match headers.get(Actor::HEADER) {
        None => None,
        Some(v) => match v.to_str() {
            Ok(s) => Some(s.trim().to_string()),
            Err(_) => {
                return Err(CalmError::BadRequest(
                    "X-Calm-Actor: header must be valid UTF-8".into(),
                ));
            }
        },
    };

    let actor = match raw.as_deref() {
        None | Some("") => Actor(Actor::DEFAULT.to_string()),
        Some(s) => validate_header_actor(s)?,
    };

    request.extensions_mut().insert(actor);
    Ok(next.run(request).await)
}

/// Axum middleware: require the TCP peer to be loopback (internal worker hook routes are loopback callbacks, not user REST endpoints).
pub async fn require_loopback_connect_info(
    request: Request<Body>,
    next: Next,
) -> Result<Response, CalmError> {
    let Some(ConnectInfo(peer)) = request.extensions().get::<ConnectInfo<SocketAddr>>() else {
        return Err(CalmError::Forbidden(
            "internal hook requires loopback peer information".into(),
        ));
    };

    if !peer.ip().is_loopback() {
        return Err(CalmError::Forbidden(
            "internal hook requires loopback peer".into(),
        ));
    }

    Ok(next.run(request).await)
}

impl<S> FromRequestParts<S> for Actor
where
    S: Send + Sync,
{
    type Rejection = CalmError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Actor>()
            .cloned()
            .ok_or_else(|| CalmError::Internal("actor middleware not applied".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_when_empty() {
        let a = validate_header_actor("").unwrap();
        assert_eq!(a, Actor("user".into()));
    }

    #[test]
    fn user_passes() {
        let a = validate_header_actor("user").unwrap();
        assert_eq!(a, Actor("user".into()));
    }

    #[test]
    fn ai_with_valid_id_passes() {
        let a = validate_header_actor("ai:codex").unwrap();
        assert_eq!(a, Actor("ai:codex".into()));
        let a = validate_header_actor("ai:claude-3-5").unwrap();
        assert_eq!(a, Actor("ai:claude-3-5".into()));
    }

    #[test]
    fn ai_with_empty_id_rejected() {
        let err = validate_header_actor("ai:").unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn ai_with_uppercase_id_rejected() {
        let err = validate_header_actor("ai:UPPER").unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn ai_with_too_long_id_rejected() {
        let long = format!("ai:{}", "a".repeat(65));
        let err = validate_header_actor(&long).unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn kernel_rejected() {
        let err = validate_header_actor("kernel").unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn plugin_rejected() {
        let err = validate_header_actor("plugin:hello-world").unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        // Bare `plugin:` is rejected too — the namespace itself is reserved.
        let err = validate_header_actor("plugin:").unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }

    #[test]
    fn unrecognized_namespace_rejected() {
        let err = validate_header_actor("admin").unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
        let err = validate_header_actor("svc:foo").unwrap_err();
        assert!(matches!(err, CalmError::BadRequest(_)));
    }
}
