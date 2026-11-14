//! Fail-closed admission and filtering for unlocked share credentials. Policy
//! belongs to share access; generic authentication only selects the credential.
use super::{
    ShareRead,
    sessions::{self, Credential},
};
use crate::{
    auth::{AuthState, Principal},
    db::RouteRepo,
    error::{CalmError, Result},
    extract::Path,
    state::AppState,
};
use axum::{
    RequestExt,
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};
use calm_types::{event::EventScope, share_links::ShareScope};
use std::collections::HashMap;

#[derive(Clone)]
pub(crate) struct ShareAccessScope {
    pub read: ShareRead,
    credential: Credential,
    auth: AuthState,
    session_id: String,
}
impl ShareAccessScope {
    /// Subscribe before validating so removal during an awaited scope read is
    /// still visible on entry to the connection's existing select loop.
    pub(crate) fn subscribe_session_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.auth.sessions.subscribe_changes()
    }

    /// The ordinary terminal pump and its writer consume the same live scope.
    pub(crate) fn terminal_input_scope(
        &self,
        state: &AppState,
        terminal: &str,
    ) -> crate::terminal_renderer::ClientInputScope {
        let (scope, repo, terminal) = (self.clone(), state.repo.clone(), terminal.to_owned());
        let check: std::sync::Arc<
            dyn Fn() -> futures::future::BoxFuture<'static, bool> + Send + Sync,
        > = std::sync::Arc::new(move || {
            let (scope, repo, terminal) = (scope.clone(), repo.clone(), terminal.clone());
            Box::pin(async move {
                if scope.validate(repo.as_ref()).await.is_err() {
                    return false;
                }
                let contained = match repo.terminal_get(&terminal).await {
                    Ok(Some(t)) => scope
                        .card(repo.as_ref(), t.card_id.as_str())
                        .await
                        .unwrap_or(false),
                    _ => false,
                };
                contained && scope.validate(repo.as_ref()).await.is_ok()
            })
        });
        crate::terminal_renderer::ClientInputScope::Bound {
            observe: check.clone(),
            control: check,
        }
    }

    pub(crate) fn input_source(&self) -> Result<crate::conversation_inputs::ShareInputSource> {
        let validated = crate::auth::ValidatedApplicationAuth::from_state(self.auth.clone())
            .map_err(|_| CalmError::Unauthorized)?;
        Ok(crate::conversation_inputs::ShareInputSource::new(
            validated.execution,
            self.read.link.delegation.clone(),
        ))
    }
    /// Transient ingress credential check. Never retain this in accepted execution.
    pub(crate) fn check_current(&self) -> Result<()> {
        if self.auth.sessions.get(&self.session_id).is_none()
            || self.auth.shares.get(&self.session_id).is_none()
        {
            return Err(CalmError::Unauthorized);
        }
        Ok(())
    }
    pub(crate) async fn authorize_ingress_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        area: &str,
        track: Option<&str>,
    ) -> Result<()> {
        self.check_current()?;
        let link = crate::db::sqlite::share_links::get_tx(tx, &self.credential.link_id).await?;
        if link != self.read.link
            || link.revoked_at.is_some()
            || link.expires_at <= crate::model::now_ms()
        {
            return Err(CalmError::Unauthorized);
        }
        let source = self.input_source()?;
        source.check_current()?;
        crate::db::sqlite::area_membership::area_require_access(
            tx,
            area,
            &link.delegation.approver_user_id,
            calm_types::organization::AreaAccess::Delete,
        )
        .await?;
        if area != self.area() {
            return Err(forbidden());
        }
        let contained = match (&link.delegation.scope, track) {
            (ShareScope::Area(id), _) => id == area,
            (ShareScope::Track(root), Some(track)) => {
                crate::db::sqlite::share_links::track_tree_contains(tx, root, track).await?
            }
            _ => false,
        };
        if !contained {
            return Err(forbidden());
        }
        self.check_current()
    }
    pub async fn validate(&self, repo: &dyn RouteRepo) -> Result<()> {
        self.check_current()?;
        let current = super::resolve_hash(repo, &self.auth.config, &self.credential.hash, None)
            .await
            .map_err(|_| CalmError::Unauthorized)?;
        if current != self.read {
            return Err(CalmError::Unauthorized);
        }
        self.check_current()
    }
    pub fn area(&self) -> &str {
        self.read.area.as_str()
    }
    pub async fn track(&self, repo: &dyn RouteRepo, id: &str) -> Result<bool> {
        let Some(track) = repo.track_get(id).await? else {
            return Ok(false);
        };
        Ok(track.purpose.as_deref() != Some(crate::AREA_CHAT_PURPOSE)
            && track.area_id.as_str() == self.area()
            && match &self.read.link.delegation.scope {
                ShareScope::Area(_) => true,
                ShareScope::Track(tid) => repo.share_track_tree_contains(tid, id).await?,
            })
    }
    pub async fn card(&self, repo: &dyn RouteRepo, id: &str) -> Result<bool> {
        match repo.card_track_id(id).await? {
            Some(track) => self.track(repo, &track).await,
            None => Ok(false),
        }
    }
    pub async fn entity(&self, repo: &dyn RouteRepo, kind: &str, id: &str) -> Result<bool> {
        match kind {
            "area" => Ok(id == self.area()),
            "track" => self.track(repo, id).await,
            "card" => self.card(repo, id).await,
            _ => Ok(false),
        }
    }
    pub async fn event(&self, repo: &dyn RouteRepo, scope: &EventScope) -> Result<bool> {
        self.validate(repo).await?;
        if let Some(track) = scope.track_id() {
            return self.track(repo, track.as_str()).await;
        }
        Ok(
            matches!(scope, EventScope::Area { area } if area.as_str() == self.area())
                && matches!(self.read.link.delegation.scope, ShareScope::Area(_)),
        )
    }
    pub async fn roots(&self, repo: &dyn RouteRepo) -> Result<Vec<std::path::PathBuf>> {
        let mut roots = Vec::new();
        for track in repo.tracks_by_area(self.area()).await? {
            if self.track(repo, track.id.as_str()).await?
                && let Ok(path) = tokio::fs::canonicalize(track.workspace.agent_cwd()).await
            {
                roots.push(path);
            }
        }
        roots.sort();
        roots.dedup();
        Ok(roots)
    }
}

fn forbidden() -> CalmError {
    CalmError::Forbidden("outside share scope".into())
}
fn query(uri: &axum::http::Uri) -> HashMap<String, String> {
    url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

async fn allowed(
    scope: &ShareAccessScope,
    repo: &dyn RouteRepo,
    method: &Method,
    route: &str,
    params: &HashMap<String, String>,
    uri: &axum::http::Uri,
) -> Result<bool> {
    let parts: Vec<_> = route.trim_start_matches('/').split('/').collect();
    let get = *method == Method::GET;
    let id = |names: &[&str]| {
        names
            .iter()
            .find_map(|key| params.get(*key))
            .map(String::as_str)
            .unwrap_or("")
    };
    if parts.get(1) == Some(&"tracks") {
        if parts.len() == 2 && *method == Method::POST {
            return Ok(matches!(
                scope.read.link.delegation.scope,
                ShareScope::Area(_)
            ));
        }
        if parts.len() < 3
            || parts.contains(&"share-links")
            || (parts.len() == 3 && !matches!(*method, Method::GET | Method::PATCH))
        {
            return Ok(false);
        }
        return scope.track(repo, id(&["track_id", "id"])).await;
    }
    if parts.get(1) == Some(&"cards") {
        return scope.card(repo, id(&["card_id", "id"])).await;
    }
    if parts.get(1) == Some(&"terminals") {
        let Some(terminal) = repo.terminal_get(id(&["id"])).await? else {
            return Ok(false);
        };
        return scope.card(repo, terminal.card_id.as_str()).await;
    }
    match parts.as_slice() {
        ["api", "events"] => Ok(get),
        ["api", "areas"] => Ok(get),
        ["api", "areas", _, "tracks"] => Ok(id(&["area_id", "id"]) == scope.area()
            && (get
                || (*method == Method::POST
                    && matches!(&scope.read.link.delegation.scope, ShareScope::Area(_))))),
        ["api", "areas", _, "recipes" | "mentions" | "folders"] => {
            Ok(get && id(&["area_id", "id"]) == scope.area())
        }
        ["api", "overlays" | "settings" | "track-recipes"] | ["api", "track-recipes", _] => Ok(get),
        ["api", "read-receipts"] => Ok(get),
        ["api", "read-receipts", _, _, "read" | "unread"] => {
            if *method != Method::POST {
                return Ok(false);
            }
            match id(&["kind"]) {
                "track" => scope.track(repo, id(&["target_id"])).await,
                "conversation" => scope.card(repo, id(&["target_id"])).await,
                _ => Err(CalmError::BadRequest("invalid receipt kind".into())),
            }
        }
        ["api", "fs", "roots"] => Ok(get),
        [
            "api",
            "fs",
            "listdir" | "readfile" | "readfile-raw" | "gitstatus" | "gitdiff",
        ] => {
            // The filesystem owner binds the allowed roots and the actual read
            // handles. Middleware authorizes this route, never a discarded path.
            Ok(get)
        }
        ["api", "models"] => {
            if !get {
                return Ok(false);
            }
            match query(uri).get("card_id") {
                Some(id) => scope.card(repo, id).await,
                None => Ok(true),
            }
        }
        ["api", "agent-providers"]
        | ["api", "agent-providers", "managed"]
        | ["api", "claude", "installed-version"]
        | ["api", "track-templates"]
        | ["api", "github", "preview"] => Ok(get),
        _ => Ok(false),
    }
}

pub(crate) async fn share_scope_middleware(
    State((state, auth)): State<(AppState, AuthState)>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let principal = request
        .extensions()
        .get::<Principal>()
        .ok_or(CalmError::Unauthorized)?;
    let source = request
        .extensions()
        .get::<crate::auth::HumanInputSource>()
        .ok_or(CalmError::Unauthorized)?;
    if !source.is_share_link() {
        return Ok(next.run(request).await);
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("share-scope-before-access", &principal.session_id).await;
    let credential = auth
        .shares
        .get(&principal.session_id)
        .ok_or(CalmError::Unauthorized)?;
    let read = super::resolve_hash(state.repo.as_ref(), &auth.config, &credential.hash, None)
        .await
        .map_err(|_| CalmError::Unauthorized)?;
    let scope = ShareAccessScope {
        read,
        credential,
        auth,
        session_id: principal.session_id.clone(),
    };
    scope.validate(state.repo.as_ref()).await?;
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .ok_or_else(forbidden)?;
    let params = if route.contains('{') {
        let Path(params) = request
            .extract_parts::<Path<HashMap<String, String>>>()
            .await
            .map_err(|_| forbidden())?;
        params
    } else {
        HashMap::new()
    };
    if !allowed(
        &scope,
        state.repo.as_ref(),
        request.method(),
        &route,
        &params,
        request.uri(),
    )
    .await?
    {
        return Err(forbidden());
    }
    let method = request.method().clone();
    let uri = request.uri().clone();
    if route.starts_with("/api/fs/") && route != "/api/fs/roots" {
        let access =
            crate::routes::fs::share_read::ShareRead::bind(&scope, state.repo.as_ref()).await?;
        request.extensions_mut().insert(std::sync::Arc::new(access));
    }
    request.extensions_mut().insert(scope.clone());
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("share-scope-admitted", &scope.session_id).await;
    let response = next.run(request).await;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("share-scope-response", &scope.session_id).await;
    scope.validate(state.repo.as_ref()).await?;
    if !allowed(&scope, state.repo.as_ref(), &method, &route, &params, &uri).await? {
        return Err(forbidden());
    }
    Ok(response)
}

/// Markdown images cannot add a header. The only ambient-share fallback is an
/// exact Track workspace read or card attachment, and only without owner auth.
pub(crate) async fn subresource_auth(
    State((state, auth)): State<(AppState, AuthState)>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    if sessions::selector(request.headers(), request.uri()).is_none()
        && crate::auth::resolve_principal(&auth, request.headers()).is_none()
        && *request.method() == Method::GET
    {
        let route = request
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .map(|p| p.as_str())
            .unwrap_or("");
        let workspace = route == "/api/tracks/{track_id}/workspace/readfile-raw";
        let attachment = route == "/api/cards/{id}/planner/attachments/{attachment_id}";
        if workspace || attachment {
            let Path(params) = request
                .extract_parts::<Path<HashMap<String, String>>>()
                .await
                .map_err(|_| forbidden())?;
            let names: Vec<String> = request
                .headers()
                .get_all(axum::http::header::COOKIE)
                .iter()
                .filter_map(|h| h.to_str().ok())
                .flat_map(|h| h.split(';'))
                .filter_map(|p| p.trim().split_once('='))
                .filter_map(|(name, _)| name.strip_prefix("calm-share-session-").map(str::to_owned))
                .collect();
            for id in names {
                let name = sessions::cookie_name(&id);
                let found = sessions::cookies(request.headers(), &name)
                    .find_map(|s| auth.shares.get(s).map(|c| (s.to_owned(), c)));
                let Some((session_id, credential)) = found else {
                    continue;
                };
                let Ok(read) =
                    super::resolve_hash(state.repo.as_ref(), &auth.config, &credential.hash, None)
                        .await
                else {
                    continue;
                };
                let scope = ShareAccessScope {
                    read,
                    credential,
                    auth: auth.clone(),
                    session_id,
                };
                let covers = if workspace {
                    scope
                        .track(
                            state.repo.as_ref(),
                            params.get("track_id").map(String::as_str).unwrap_or(""),
                        )
                        .await?
                } else {
                    scope
                        .card(
                            state.repo.as_ref(),
                            params.get("id").map(String::as_str).unwrap_or(""),
                        )
                        .await?
                };
                if covers && scope.validate(state.repo.as_ref()).await.is_ok() {
                    request
                        .headers_mut()
                        .insert("x-calm-share-link", id.parse().map_err(|_| forbidden())?);
                    break;
                }
            }
        }
    }
    Ok(next.run(request).await)
}
