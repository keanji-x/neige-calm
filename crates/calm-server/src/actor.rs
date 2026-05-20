//! Scope G — declarative actor plumbing.
//!
//! Every REST write funnels through `Repo::write_with_event(actor, ...)` and
//! the `events` table records who did what. Pre–Scope G that "who" was
//! hardcoded to `"user"` in every handler, which made AI agent writes
//! indistinguishable from human writes in audit. This module closes that gap.
//!
//! The mechanism:
//!
//! 1. An axum middleware ([`actor_middleware`]) reads `X-Calm-Actor` from the
//!    incoming request headers, validates it, and injects an `Actor` into
//!    the request extensions. When the header is absent the default is
//!    `"user"` — preserving today's single-user local-host UX where no
//!    header is sent.
//!
//! 2. Handlers add `actor: Actor` to their signature; the `FromRequestParts`
//!    impl below plucks it from extensions. Handlers then pass `actor.0` as
//!    the actor argument to `write_with_event_typed`.
//!
//! 3. The middleware refuses to forward writes whose claimed actor is
//!    reserved for server-internal use (`kernel`, `plugin:*`). This stops
//!    REST callers from spoofing kernel writes or impersonating plugins.
//!    Server-internal sites (`card_fsm`, the codex hook ingest path, the
//!    plugin callback dispatcher) reach `write_with_event_typed` without
//!    going through the middleware, so those keep stamping `"kernel"` /
//!    `"plugin:<id>"` directly.
//!
//! **Not authenticated.** See `docs/sync-engine-design.md` §1.1 — the
//! `actor` field is a declared identity, not an authenticated one. In the
//! single-user local-host deployment that's adequate; if neige-calm ever
//! opens an externally-reachable surface, a separate auth layer must
//! precede any reliance on `actor` for security decisions. Today this
//! file is plumbing, not a security boundary.

use axum::{
    body::Body,
    extract::FromRequestParts,
    http::{HeaderMap, Request, request::Parts},
    middleware::Next,
    response::Response,
};

use crate::error::CalmError;

/// Declared identity of an event producer. Populated by
/// [`actor_middleware`] reading `X-Calm-Actor` from request headers; defaults
/// to `"user"` when absent (preserves single-user local-host UX where no
/// header is sent).
///
/// **Not authenticated** — this is a declared field. If neige-calm ever
/// opens an externally-reachable surface, a separate auth design must
/// gate writes before relying on this for security decisions. See design
/// doc §1.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor(pub String);

impl Actor {
    /// The default actor used when no `X-Calm-Actor` header is present.
    pub const DEFAULT: &'static str = "user";

    /// HTTP header carrying the declared actor.
    pub const HEADER: &'static str = "X-Calm-Actor";

    /// Borrow the underlying string slice — convenience for passing to
    /// `write_with_event_typed`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validation outcome for an actor string sourced from a request header.
///
/// Reserved actors (`kernel`, `plugin:*`) are rejected from the header —
/// they're populated server-side by the FSM projector and the plugin
/// callback dispatcher respectively. Allowing them via header would let any
/// REST caller spoof kernel writes or impersonate plugins.
fn validate_header_actor(raw: &str) -> Result<Actor, CalmError> {
    // Empty -> treat as missing, caller already collapses that case to the
    // default; this branch is defense in depth.
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

/// Axum middleware: read `X-Calm-Actor`, validate, stash an [`Actor`] in
/// request extensions for downstream handlers to pluck via the
/// [`FromRequestParts`] impl below.
///
/// On invalid headers we short-circuit with a 400 (via [`CalmError::BadRequest`])
/// — the handler never runs.
pub async fn actor_middleware(
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, CalmError> {
    // Header parsing: a non-UTF-8 byte sequence is treated the same as a
    // malformed value — 400, not silently default-to-user.
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
        // Empty string is treated as "missing" — defense in depth on top of
        // the middleware's header-absent branch.
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
        // Bare `plugin:` (no id) is rejected by the same arm — the
        // namespace itself is reserved, not just the id-bearing form.
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
