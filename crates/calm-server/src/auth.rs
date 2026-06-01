//! Global session gate (issue #189).
//!
//! Single-user owner auth: one configured username/password pair signs in
//! and obtains a session, every protected REST/WS endpoint then checks that
//! the request carries a valid `calm-session` cookie. No user table, no
//! registration, no permissions beyond the implicit `owner` role.
//!
//! ## Wire shape
//!
//! * `POST /api/auth/login` — body `{username, password}` → 200 with whoami
//!   payload + `Set-Cookie: calm-session=<id>; HttpOnly; SameSite=Strict;
//!   Path=/`. Wrong credentials → 401.
//! * `GET /api/auth/whoami` — 200 with `{userId, displayName, role,
//!   sessionId}` if the session cookie is valid (or dev_autologin is on);
//!   401 otherwise.
//! * `POST /api/auth/logout` — 200, drops the session and clears the cookie.
//!
//! 401 responses share the standard `{error: "unauthorized", code:
//! "unauthorized"}` body via `CalmError::Unauthorized`.
//!
//! ## Session storage
//!
//! In-memory `HashMap<session_id, Session>` behind an `Arc<Mutex<_>>`. We're
//! single-user single-process; persistence across restarts (the user would
//! have to log in again) is acceptable and the simplest possible thing.
//! Cookie value is a UUIDv4 string — high entropy, opaque on the wire.
//!
//! ## Dev autologin
//!
//! `CALM_DEV_AUTOLOGIN=true` (or `auth.dev_autologin = true`) skips the
//! whole flow: the middleware just promotes every request to the owner
//! principal without any cookie. Production must NEVER enable this — the
//! default is `false` and the boot path only opens it via explicit
//! env/config.
//!
//! ## Trust model
//!
//! Cookies are unsigned. Anyone with the `calm-session` value can act as
//! owner, which is fine because:
//!
//!   - cookies are `HttpOnly` (JS can't read them),
//!   - cookies are `SameSite=Strict` (cross-site requests can't carry them),
//!   - sessions live in memory and die on server restart.
//!
//! When we open neige-calm to external surfaces we'll layer signing /
//! rotation / idle expiry on top, but that's out of scope here.

use crate::config::Config;
use crate::error::{CalmError, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Name of the session cookie. Frontend and backend MUST agree on this —
/// see issue #189 acceptance criteria.
pub const SESSION_COOKIE: &str = "calm-session";

/// Owner principal id. Single-user model — every successful login lands on
/// this exact string. Surfaced via `whoami.userId`.
pub const OWNER_USER_ID: &str = "local-owner";

/// Default display name used when no `auth.username` is configured (e.g.
/// dev autologin without a credential set).
pub const DEFAULT_DISPLAY_NAME: &str = "Owner";

/// Role string returned by `whoami`. Single-user model has exactly one role.
pub const OWNER_ROLE: &str = "owner";

/// Boot-time auth config derived from the process `Config` + env. Held in
/// `AuthState` so the routes + middleware can consult it without re-reading
/// env vars at every request.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Configured owner username. `None` only allowed when `dev_autologin`
    /// is on; production boots panic otherwise (see `AuthConfig::from_env`).
    pub username: Option<String>,
    /// Configured owner password. Same `None`-only-in-dev rule as
    /// `username`.
    pub password: Option<String>,
    /// When true, every request is automatically promoted to the owner
    /// principal without any cookie / login flow. ALWAYS off by default;
    /// explicit env/config opt-in only.
    pub dev_autologin: bool,
    /// Display name surfaced via `whoami`. Falls back to
    /// [`DEFAULT_DISPLAY_NAME`] when `auth.username` isn't set (dev
    /// autologin).
    pub display_name: String,
}

impl AuthConfig {
    /// Derive auth config from the process `Config`. Panics if auth is
    /// "live" (`dev_autologin = false`) but no password is configured —
    /// that's a misconfiguration the operator MUST fix, not a request-
    /// time 500.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let username = cfg.auth_username.clone();
        let password = cfg.auth_password.clone();
        let dev_autologin = cfg.auth_dev_autologin;
        let display_name = username
            .clone()
            .unwrap_or_else(|| DEFAULT_DISPLAY_NAME.to_string());

        if !dev_autologin && password.is_none() {
            anyhow::bail!(
                "auth: missing owner credential — set CALM_AUTH_PASSWORD (and \
                 CALM_AUTH_USERNAME), or opt into CALM_DEV_AUTOLOGIN=true for \
                 local development"
            );
        }

        Ok(Self {
            username,
            password,
            dev_autologin,
            display_name,
        })
    }
}

/// One active session. Right now this is just a marker — single-user model
/// means every session resolves to the same owner principal — but holding
/// a struct keeps the door open for created-at timestamps / idle expiry /
/// rotation tags without another schema migration.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
}

/// In-memory session store. `Arc<Mutex<...>>` is plenty for the single-user
/// case: lock contention is negligible (one login per browser tab) and a
/// process restart wipes sessions anyway.
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<String, Session>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh session, store it, and return the new id. Caller sets
    /// the cookie.
    pub fn create(&self) -> String {
        let id = Uuid::new_v4().to_string();
        let session = Session {
            session_id: id.clone(),
        };
        // Poisoned-mutex policy: log + recover. We never panic out of the
        // lock-poison branch because that would take the whole server down
        // for one bad request that's already in flight.
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(id.clone(), session);
        } else {
            tracing::error!("session store mutex poisoned on insert");
        }
        id
    }

    /// Look up a session by id. Returns `None` for unknown ids.
    pub fn get(&self, id: &str) -> Option<Session> {
        match self.inner.lock() {
            Ok(g) => g.get(id).cloned(),
            Err(_) => {
                tracing::error!("session store mutex poisoned on get");
                None
            }
        }
    }

    /// Remove a session by id. Idempotent — removing an unknown id is a
    /// no-op (matches what `POST /api/auth/logout` wants).
    pub fn remove(&self, id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(id);
        }
    }
}

/// State the auth routes + middleware need. Cloned into `AppState` so
/// handlers reach it via `State<AuthState>` extractors (same pattern as
/// the existing `AppState` shape).
#[derive(Debug, Clone)]
pub struct AuthState {
    pub config: Arc<AuthConfig>,
    pub sessions: SessionStore,
}

impl AuthState {
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config: Arc::new(config),
            sessions: SessionStore::new(),
        }
    }
}

/// Authenticated principal. Inserted into request extensions by
/// [`require_session`]; handlers that need to know "this came in
/// authenticated as owner" pluck it via `FromRequestParts`. Today every
/// authenticated principal is owner, so this carries the bare minimum
/// `session_id` for logout to know which session to drop.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub session_id: String,
}

impl Principal {
    /// Construct the standard owner principal from the auth config + a
    /// (possibly synthetic) session id.
    pub fn owner(cfg: &AuthConfig, session_id: String) -> Self {
        Self {
            user_id: OWNER_USER_ID.to_string(),
            display_name: cfg.display_name.clone(),
            role: OWNER_ROLE.to_string(),
            session_id,
        }
    }
}

impl<S> FromRequestParts<S> for Principal
where
    S: Send + Sync,
{
    type Rejection = CalmError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or(CalmError::Unauthorized)
    }
}

/// Read the `calm-session` cookie value from a request's headers, if any.
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let jar = CookieJar::from_headers(headers);
    jar.get(SESSION_COOKIE).map(|c| c.value().to_string())
}

/// Resolve a principal for the incoming request, honoring dev_autologin.
/// Returns `None` when there's no valid session AND dev_autologin is off
/// — caller decides whether to 401 or continue (whoami treats no-session
/// as 401 itself; the middleware treats it as block).
fn resolve_principal(state: &AuthState, headers: &HeaderMap) -> Option<Principal> {
    if state.config.dev_autologin {
        // Dev mode: synthesize a stable session id so whoami / logout etc.
        // behave consistently across requests. We don't write it back into
        // the store — there's no validation to do later, since the same
        // promotion happens on every request.
        return Some(Principal::owner(&state.config, "dev-autologin".to_string()));
    }
    let cookie = session_cookie(headers)?;
    let session = state.sessions.get(&cookie)?;
    Some(Principal::owner(&state.config, session.session_id))
}

/// Axum middleware: gate every protected endpoint. Routes excluded from
/// the gate (login, whoami, logout, version, openapi.json) must NOT have
/// this layer applied to them — see `main.rs` where the routing trees are
/// split. On success the resolved [`Principal`] lands in request
/// extensions for downstream handlers (none consume it today; we still
/// stash it so future code can reach `Principal::session_id` / `role`
/// without re-parsing the cookie).
pub async fn require_session(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response> {
    let Some(principal) = resolve_principal(&auth, &headers) else {
        return Err(CalmError::Unauthorized);
    };
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

/// Same as [`require_session`] but for the WS upgrade routes. Identical
/// semantics; pulled out so future divergence (e.g. relaxing cookie checks
/// for a token-in-query-param fallback) has a clean seam. Today it's a
/// thin wrapper.
pub async fn require_session_ws(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response> {
    let Some(principal) = resolve_principal(&auth, &headers) else {
        return Err(CalmError::Unauthorized);
    };
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhoamiBody {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub session_id: String,
}

impl From<&Principal> for WhoamiBody {
    fn from(p: &Principal) -> Self {
        Self {
            user_id: p.user_id.clone(),
            display_name: p.display_name.clone(),
            role: p.role.clone(),
            session_id: p.session_id.clone(),
        }
    }
}

/// Build the auth router. Mounted in `main.rs` BEFORE the session gate is
/// applied to the protected routes; the routes here must remain reachable
/// without a prior login (otherwise nobody could ever log in).
pub fn router() -> Router<AuthState> {
    Router::new()
        .route("/api/auth/login", post(login_handler))
        .route("/api/auth/whoami", get(whoami_handler))
        .route("/api/auth/logout", post(logout_handler))
}

/// POST /api/auth/login — verify credentials, mint a session, set cookie.
async fn login_handler(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Response> {
    // Dev autologin: any login is a no-op success — we still hand back a
    // synthetic whoami so the frontend's "login form" path stays usable
    // for screenshot/e2e flows. No cookie set; the middleware promotes
    // every request anyway.
    if auth.config.dev_autologin {
        let principal = Principal::owner(&auth.config, "dev-autologin".to_string());
        return Ok(Json(WhoamiBody::from(&principal)).into_response());
    }

    let (Some(want_user), Some(want_pass)) = (
        auth.config.username.as_deref(),
        auth.config.password.as_deref(),
    ) else {
        // Impossible in practice (boot panics if password is unset + dev
        // autologin is off), but defense-in-depth: if config is somehow
        // half-set, refuse rather than locking the user out by accident.
        return Err(CalmError::Unauthorized);
    };

    if body.username != want_user || body.password != want_pass {
        return Err(CalmError::Unauthorized);
    }

    // Tear down any previous session that might still be sitting on this
    // request — keeps a successful login from leaving zombie sessions
    // behind. Idempotent.
    if let Some(existing) = session_cookie(&headers) {
        auth.sessions.remove(&existing);
    }

    let new_id = auth.sessions.create();
    let principal = Principal::owner(&auth.config, new_id.clone());
    let cookie = build_session_cookie(&new_id);

    let mut resp = Json(WhoamiBody::from(&principal)).into_response();
    resp.headers_mut().append(
        header::SET_COOKIE,
        cookie.to_string().parse().expect("cookie ascii"),
    );
    Ok(resp)
}

/// GET /api/auth/whoami — returns owner whoami if authenticated (or
/// dev_autologin); 401 otherwise. NOT behind the session middleware (it's
/// the discovery endpoint the frontend hits *before* it knows whether
/// it's logged in), so it has to check inline.
async fn whoami_handler(State(auth): State<AuthState>, headers: HeaderMap) -> Result<Response> {
    let Some(principal) = resolve_principal(&auth, &headers) else {
        return Err(CalmError::Unauthorized);
    };
    Ok(Json(WhoamiBody::from(&principal)).into_response())
}

/// POST /api/auth/logout — drops the session id (if any) and clears the
/// cookie. Always 200; idempotent. Dev autologin: same response shape, no
/// store touch (there's nothing to drop).
async fn logout_handler(State(auth): State<AuthState>, headers: HeaderMap) -> Result<Response> {
    if let Some(id) = session_cookie(&headers) {
        auth.sessions.remove(&id);
    }
    let cookie = build_logout_cookie();
    let mut resp = Json(serde_json::json!({"ok": true})).into_response();
    resp.headers_mut().append(
        header::SET_COOKIE,
        cookie.to_string().parse().expect("cookie ascii"),
    );
    Ok(resp)
}

/// Build the cookie we send on successful login. `HttpOnly`, `SameSite=Strict`,
/// path `/`. We do NOT set `Secure` so that dev http on `localhost:5175` /
/// `localhost:4040` keeps working — production deployments (when they
/// happen) sit behind https terminators that can layer `Secure` on at the
/// proxy edge if needed. (Setting `Secure` here would silently break local
/// dev with no signal — the cookie just wouldn't be sent.)
fn build_session_cookie(value: &str) -> Cookie<'static> {
    let mut c = Cookie::new(SESSION_COOKIE, value.to_string());
    c.set_http_only(true);
    c.set_same_site(SameSite::Strict);
    c.set_path("/");
    c
}

/// Build the cookie used to clear the session on logout. Same attributes
/// as the live cookie but with `Cookie::make_removal()` which sets value
/// to empty + `Max-Age=0` so the browser drops the stored cookie. We
/// preserve `Path=/` on the removal so the browser matches the same
/// cookie scope as the original set; the cookie spec keys cookies by
/// (name, domain, path), and a path mismatch would leave the original
/// installed.
fn build_logout_cookie() -> Cookie<'static> {
    let mut c = Cookie::new(SESSION_COOKIE, "");
    c.set_http_only(true);
    c.set_same_site(SameSite::Strict);
    c.set_path("/");
    c.make_removal();
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_live() -> AuthConfig {
        AuthConfig {
            username: Some("owner".into()),
            password: Some("hunter2".into()),
            dev_autologin: false,
            display_name: "owner".into(),
        }
    }

    fn cfg_dev() -> AuthConfig {
        AuthConfig {
            username: None,
            password: None,
            dev_autologin: true,
            display_name: DEFAULT_DISPLAY_NAME.into(),
        }
    }

    #[test]
    fn auth_config_panics_without_password_in_prod_mode() {
        // Build a Config with no auth fields set and dev_autologin off.
        let cfg = Config {
            emit_kernel_compatibility_json: false,
            listen: "127.0.0.1:0".into(),
            db_url: "mock".into(),
            data_dir: None,
            proc_supervisor_sock: None,
            allowed_origin: "http://localhost".into(),
            web_dist: None,
            plugins_dir: None,
            plugins_data_dir: None,
            plugins_disabled: vec![],
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            codex_bridge_bin: None,
            mcp_stdio_shim_bin: None,
            codex_ingest_url: None,
            auth_username: None,
            auth_password: None,
            auth_dev_autologin: false,
            shared_codex_appserver_enabled: true,
            shared_codex_prompt_cards_enabled: false,
            shared_codex_empty_cards_enabled: false,
            shared_codex_appserver_restart_initial_delay_ms: 250,
            shared_codex_appserver_restart_max_delay_ms: 10_000,
            shared_codex_appserver_log_dir: None,
        };
        let err = AuthConfig::from_config(&cfg).unwrap_err();
        assert!(err.to_string().contains("owner credential"));
    }

    #[test]
    fn auth_config_allows_no_password_when_dev_autologin_on() {
        let cfg = Config {
            emit_kernel_compatibility_json: false,
            listen: "127.0.0.1:0".into(),
            db_url: "mock".into(),
            data_dir: None,
            proc_supervisor_sock: None,
            allowed_origin: "http://localhost".into(),
            web_dist: None,
            plugins_dir: None,
            plugins_data_dir: None,
            plugins_disabled: vec![],
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            codex_bridge_bin: None,
            mcp_stdio_shim_bin: None,
            codex_ingest_url: None,
            auth_username: None,
            auth_password: None,
            auth_dev_autologin: true,
            shared_codex_appserver_enabled: true,
            shared_codex_prompt_cards_enabled: false,
            shared_codex_empty_cards_enabled: false,
            shared_codex_appserver_restart_initial_delay_ms: 250,
            shared_codex_appserver_restart_max_delay_ms: 10_000,
            shared_codex_appserver_log_dir: None,
        };
        let auth = AuthConfig::from_config(&cfg).expect("dev autologin allows missing password");
        assert!(auth.dev_autologin);
        assert!(auth.password.is_none());
    }

    #[test]
    fn session_store_round_trip() {
        let store = SessionStore::new();
        let id = store.create();
        assert!(store.get(&id).is_some());
        store.remove(&id);
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn build_session_cookie_has_required_attrs() {
        let c = build_session_cookie("abc");
        assert_eq!(c.name(), SESSION_COOKIE);
        assert_eq!(c.value(), "abc");
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.same_site(), Some(SameSite::Strict));
        assert_eq!(c.path(), Some("/"));
    }

    #[test]
    fn resolve_principal_honors_dev_autologin_without_cookie() {
        let auth = AuthState::new(cfg_dev());
        let headers = HeaderMap::new();
        let p = resolve_principal(&auth, &headers).expect("dev autologin promotes");
        assert_eq!(p.user_id, OWNER_USER_ID);
        assert_eq!(p.role, OWNER_ROLE);
    }

    #[test]
    fn resolve_principal_blocks_without_cookie_in_prod_mode() {
        let auth = AuthState::new(cfg_live());
        let headers = HeaderMap::new();
        assert!(resolve_principal(&auth, &headers).is_none());
    }

    #[test]
    fn resolve_principal_accepts_valid_cookie() {
        let auth = AuthState::new(cfg_live());
        let id = auth.sessions.create();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{SESSION_COOKIE}={id}").parse().unwrap(),
        );
        let p = resolve_principal(&auth, &headers).expect("valid cookie resolves");
        assert_eq!(p.user_id, OWNER_USER_ID);
        assert_eq!(p.session_id, id);
    }

    #[test]
    fn resolve_principal_rejects_unknown_cookie() {
        let auth = AuthState::new(cfg_live());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{SESSION_COOKIE}=not-a-real-session")
                .parse()
                .unwrap(),
        );
        assert!(resolve_principal(&auth, &headers).is_none());
    }
}
