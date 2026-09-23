//! Global session gate: single-user owner login, in-memory sessions keyed by an unsigned `calm-session` cookie (HttpOnly, SameSite=Strict, dies on restart).
//! `dev_autologin` promotes every request to the owner without a cookie; production must never enable it.

use crate::config::Config;
use crate::error::{CalmError, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, Method, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Name of the session cookie. Frontend and backend MUST agree on this.
pub const SESSION_COOKIE: &str = "calm-session";

/// Owner principal id; every successful login lands on this exact string.
pub const OWNER_USER_ID: &str = "local-owner";

/// Display name used when no `auth.username` is configured.
pub const DEFAULT_DISPLAY_NAME: &str = "Owner";

/// Role string returned by `whoami`. Single-user model has exactly one role.
pub const OWNER_ROLE: &str = "owner";

/// Boot-time auth config, held in `AuthState` so requests never re-read env vars.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// `None` only allowed when `dev_autologin` is on; production boots panic otherwise.
    pub username: Option<String>,
    /// Same `None`-only-in-dev rule as `username`.
    pub password: Option<String>,
    /// Promote every request to the owner principal without any cookie. Off by default; explicit opt-in only.
    pub dev_autologin: bool,
    /// Display name surfaced via `whoami`; falls back to [`DEFAULT_DISPLAY_NAME`].
    pub display_name: String,
}

impl AuthConfig {
    /// Panics if auth is live (`dev_autologin = false`) but no password is configured: a boot-time misconfiguration, not a request-time 500.
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

/// One active in-memory session. Business access still resolves to the same
/// user principal; credential provenance separately limits local management.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub authority: SessionAuthority,
}

/// Required credential provenance: paired devices never gain local management
/// authority, even if a separate device-registry record is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAuthority {
    PasswordLogin,
    PairedDevice,
}

/// In-memory session store; a process restart wipes sessions.
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<String, Session>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh session, store it, and return the new id. Caller sets the cookie.
    pub fn create(&self, authority: SessionAuthority) -> String {
        let id = Uuid::new_v4().to_string();
        let session = Session {
            session_id: id.clone(),
            authority,
        };
        // Poisoned-mutex policy: log + recover, never take the whole server down for one bad in-flight request.
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(id.clone(), session);
        } else {
            tracing::error!("session store mutex poisoned on insert");
        }
        id
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        match self.inner.lock() {
            Ok(g) => g.get(id).cloned(),
            Err(_) => {
                tracing::error!("session store mutex poisoned on get");
                None
            }
        }
    }

    /// Idempotent — removing an unknown id is a no-op.
    pub fn remove(&self, id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.remove(id);
        }
    }
}

/// State the auth routes + middleware need; cloned into `AppState`.
#[derive(Debug, Clone)]
pub struct AuthState {
    pub config: Arc<AuthConfig>,
    pub sessions: SessionStore,
    pub mobile: crate::mobile_access::MobileAccess,
    /// `CALM_ALLOWED_ORIGIN`: the one foreign origin (dev frontend) trusted beside calm's own.
    pub allowed_origin: Option<String>,
}

impl AuthState {
    pub fn new(config: AuthConfig) -> Self {
        let sessions = SessionStore::new();
        Self {
            config: Arc::new(config),
            mobile: crate::mobile_access::MobileAccess::new(sessions.clone()),
            sessions,
            allowed_origin: None,
        }
    }

    /// Production wiring: auth config plus the configured `CALM_ALLOWED_ORIGIN`, if any.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let state = Self::new(AuthConfig::from_config(cfg)?);
        Ok(match cfg.allowed_origin.clone() {
            Some(origin) => state.with_allowed_origin(origin),
            None => state,
        })
    }

    /// `origin` must already be in [`normalize_origin`] form.
    pub fn with_allowed_origin(mut self, origin: String) -> Self {
        self.allowed_origin = Some(origin);
        self
    }
}

/// Canonical form of a configured or learned origin: `http(s)://host[:port]`, lowercase, no trailing `/`.
pub fn normalize_origin(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    let origin = lower.strip_suffix('/').unwrap_or(&lower);
    let host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    (!host.is_empty() && !host.contains('/')).then(|| origin.to_string())
}

/// Clap value parser for `CALM_ALLOWED_ORIGIN`.
pub fn parse_origin(raw: &str) -> std::result::Result<String, String> {
    normalize_origin(raw).ok_or_else(|| format!("`{raw}` is not an http(s) origin"))
}

/// Authenticated principal, inserted into request extensions by [`require_session`]; today every principal is owner.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub session_id: String,
}

impl Principal {
    /// The standard owner principal from the auth config + a (possibly synthetic) session id.
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

/// `None` when there is no valid session AND dev_autologin is off; the caller decides whether to 401.
fn resolve_principal(state: &AuthState, headers: &HeaderMap) -> Option<Principal> {
    if state.config.dev_autologin {
        // Dev mode: a stable synthetic session id so whoami / logout behave consistently; nothing is written to the store.
        return Some(Principal::owner(&state.config, "dev-autologin".to_string()));
    }
    let cookie = session_cookie(headers)?;
    let session = state.sessions.get(&cookie)?;
    Some(Principal::owner(&state.config, session.session_id))
}

/// #1780: `SameSite=Strict` still attaches the cookie to pages on another port of the same host,
/// so a write or WS upgrade that carries `Origin` must come from one of calm's own origins.
/// No `Origin` means a non-browser client (CLI, shim); browsers always send it on these requests.
fn check_origin(state: &AuthState, headers: &HeaderMap) -> Result<()> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let Ok(origin) = origin.to_str() else {
        return Err(CalmError::Forbidden(
            "cross-origin request rejected: Origin is not a valid header string".into(),
        ));
    };
    // Scheme-agnostic: one port speaks one protocol, and a TLS-terminating proxy keeps `Host`.
    // So an https-on-443 deployment also accepts `http://<same host>`; fine, this fence targets other ports.
    let same_host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|host| {
            ["http://", "https://"].iter().any(|scheme| {
                origin
                    .strip_prefix(scheme)
                    .is_some_and(|rest| rest.eq_ignore_ascii_case(host))
            })
        });
    if same_host
        || state.allowed_origin.as_deref() == Some(origin)
        || state
            .mobile
            .origin()
            .and_then(|o| normalize_origin(&o))
            .as_deref()
            == Some(origin)
    {
        return Ok(());
    }
    Err(CalmError::Forbidden(format!(
        "cross-origin request rejected: Origin `{origin}` is not an origin of this server"
    )))
}

/// Axum middleware: gate every protected endpoint. Login, whoami, logout, version and openapi.json must NOT have this layer applied.
pub async fn require_session(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response> {
    let Some(principal) = resolve_principal(&auth, &headers) else {
        return Err(CalmError::Unauthorized);
    };
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        check_origin(&auth, &headers)?;
    }
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

/// Same as [`require_session`] but for the WS upgrade routes: every upgrade is origin-checked (WS has no CORS).
pub async fn require_session_ws(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response> {
    let Some(principal) = resolve_principal(&auth, &headers) else {
        return Err(CalmError::Unauthorized);
    };
    check_origin(&auth, &headers)?;
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

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

/// Mounted BEFORE the session gate: these routes must remain reachable without a prior login.
pub fn router() -> Router<AuthState> {
    session_router().route("/api/auth/login", post(login_handler))
}

/// The public mobile ingress offers pairing rather than password login.
pub fn session_router() -> Router<AuthState> {
    Router::new()
        .route("/api/auth/whoami", get(whoami_handler))
        .route("/api/auth/logout", post(logout_handler))
}

/// POST /api/auth/login — verify credentials, mint a session, set cookie.
async fn login_handler(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Response> {
    // Dev autologin: any login is a no-op success with a synthetic whoami; no cookie set, the middleware promotes every request anyway.
    if auth.config.dev_autologin {
        let principal = Principal::owner(&auth.config, "dev-autologin".to_string());
        return Ok(Json(WhoamiBody::from(&principal)).into_response());
    }

    let (Some(want_user), Some(want_pass)) = (
        auth.config.username.as_deref(),
        auth.config.password.as_deref(),
    ) else {
        // Impossible in practice (boot panics if password is unset + dev autologin is off); refuse rather than lock the user out by accident.
        return Err(CalmError::Unauthorized);
    };

    if body.username != want_user || body.password != want_pass {
        return Err(CalmError::Unauthorized);
    }

    // Tear down any previous session on this request so a successful login leaves no zombie sessions.
    if let Some(existing) = session_cookie(&headers) {
        auth.sessions.remove(&existing);
    }

    let new_id = auth.sessions.create(SessionAuthority::PasswordLogin);
    let principal = Principal::owner(&auth.config, new_id.clone());
    let cookie = build_session_cookie(&new_id);

    let mut resp = Json(WhoamiBody::from(&principal)).into_response();
    resp.headers_mut().append(
        header::SET_COOKIE,
        cookie.to_string().parse().expect("cookie ascii"),
    );
    Ok(resp)
}

/// GET /api/auth/whoami — NOT behind the session middleware (the frontend hits it before it knows whether it is logged in), so it checks inline.
async fn whoami_handler(State(auth): State<AuthState>, headers: HeaderMap) -> Result<Response> {
    let Some(principal) = resolve_principal(&auth, &headers) else {
        return Err(CalmError::Unauthorized);
    };
    Ok(Json(WhoamiBody::from(&principal)).into_response())
}

/// POST /api/auth/logout — always 200; idempotent.
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

/// `HttpOnly`, `SameSite=Strict`, path `/`. NOT `Secure`: that would silently break dev http on localhost (the cookie just would not be sent).
pub(crate) fn build_session_cookie(value: &str) -> Cookie<'static> {
    let mut c = Cookie::new(SESSION_COOKIE, value.to_string());
    c.set_http_only(true);
    c.set_same_site(SameSite::Strict);
    c.set_path("/");
    c
}

/// Removal cookie with the same `Path=/`: browsers key cookies by (name, domain, path), so a path mismatch would leave the original installed.
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
        let cfg = Config {
            emit_kernel_compatibility_json: false,
            listen: "127.0.0.1:0".into(),
            db_url: "mock".into(),
            data_dir: None,
            workspace_root: None,
            proc_supervisor_sock: None,
            allowed_origin: None,
            preview_ports: None,
            web_dist: None,
            fe_dist: None,
            plugins_dir: None,
            plugins_data_dir: None,
            plugins_disabled: vec![],
            templates_dir: None,
            codex_bin: "codex".into(),
            isolated_codex_config: None,
            claude_bin: "claude".into(),
            codex_bridge_bin: None,
            mcp_stdio_shim_bin: None,
            codex_ingest_url: None,
            auth_username: None,
            auth_password: None,
            auth_dev_autologin: false,
            mobile_access_config: None,
            private_tailnet_config: None,
            private_tailnet_unavailable: false,
            shared_codex_appserver_restart_initial_delay_ms: 250,
            shared_codex_appserver_restart_max_delay_ms: 10_000,
            shared_codex_appserver_start_timeout_secs: 120,
            shared_codex_appserver_stop_grace_secs: 60,
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
            workspace_root: None,
            proc_supervisor_sock: None,
            allowed_origin: None,
            preview_ports: None,
            web_dist: None,
            fe_dist: None,
            plugins_dir: None,
            plugins_data_dir: None,
            plugins_disabled: vec![],
            templates_dir: None,
            codex_bin: "codex".into(),
            isolated_codex_config: None,
            claude_bin: "claude".into(),
            codex_bridge_bin: None,
            mcp_stdio_shim_bin: None,
            codex_ingest_url: None,
            auth_username: None,
            auth_password: None,
            auth_dev_autologin: true,
            mobile_access_config: None,
            private_tailnet_config: None,
            private_tailnet_unavailable: false,
            shared_codex_appserver_restart_initial_delay_ms: 250,
            shared_codex_appserver_restart_max_delay_ms: 10_000,
            shared_codex_appserver_start_timeout_secs: 120,
            shared_codex_appserver_stop_grace_secs: 60,
            shared_codex_appserver_log_dir: None,
        };
        let auth = AuthConfig::from_config(&cfg).expect("dev autologin allows missing password");
        assert!(auth.dev_autologin);
        assert!(auth.password.is_none());
    }

    #[test]
    fn session_store_round_trip() {
        let store = SessionStore::new();
        let id = store.create(SessionAuthority::PasswordLogin);
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
        let id = auth.sessions.create(SessionAuthority::PasswordLogin);
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
