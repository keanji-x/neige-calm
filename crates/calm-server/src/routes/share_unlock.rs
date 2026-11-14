//! Public identifier lookup and one-time-secret unlock. Neither operation uses
//! the ambient owner cookie, Bearer credentials, or development auto-login.
use crate::extract::{Json, JsonBody, Path};
use crate::{
    auth::{AuthState, LoginError, SessionIdentity, login_throttle::LoginAttempt},
    error::{CalmError, ErrorBody, Result},
    share_access::sessions::{self, Credential},
    state::{AppState, RouteState},
};
use axum::{
    Extension, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use calm_types::share_links::{ShareAccessStatus, UnlockShareLinkRequest, UnlockedShareLink};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use subtle::ConstantTimeEq;

pub fn router(auth: AuthState) -> Router<AppState> {
    Router::new()
        .route("/api/share-links/{id}/access", get(access))
        .route("/api/share-links/{id}/unlock", post(unlock))
        .layer(Extension(auth))
}

#[utoipa::path(get, path="/api/share-links/{id}/access", tag="share_links", security(()),
    params(("id"=String,Path)), responses(
        (status=200,description="Locked expiry or unlocked scope",body=ShareAccessStatus),
        (status=404,description="Share unavailable",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn access(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ShareAccessStatus>> {
    let (read, _) = crate::share_access::resolve_id(s.repo.as_ref(), &auth.config, &id).await?;
    let name = sessions::cookie_name(&id);
    let unlocked = sessions::cookies(&headers, &name).any(|token| {
        auth.shares.get(token).is_some_and(|c| c.link_id == id)
            && auth.sessions.get(token).is_some()
    });
    Ok(Json(if unlocked {
        ShareAccessStatus::Unlocked {
            scope: read.link.delegation.scope,
            expires_at: read.link.expires_at,
        }
    } else {
        ShareAccessStatus::Locked {
            expires_at: read.link.expires_at,
        }
    }))
}

#[utoipa::path(post, path="/api/share-links/{id}/unlock", tag="share_links", security(()),
    params(("id"=String,Path)), request_body=UnlockShareLinkRequest, responses(
        (status=200,description="Unlocked; isolated session cookie set",body=UnlockedShareLink),
        (status=400,description="Invalid request",body=ErrorBody),
        (status=401,description="Wrong token",body=ErrorBody),
        (status=403,description="Cross-origin unlock",body=ErrorBody),
        (status=404,description="Share unavailable",body=ErrorBody),
        (status=429,description="Unlock throttled",body=ErrorBody, headers(("Retry-After"=u64,description="Seconds until retry"))),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn unlock(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<UnlockShareLinkRequest>,
) -> std::result::Result<Response, LoginError> {
    crate::auth::check_origin(&auth, &headers)?;
    let (read, hash) = crate::share_access::resolve_id(s.repo.as_ref(), &auth.config, &id).await?;
    let given: [u8; 32] = Sha256::digest(body.token.as_bytes()).into();
    let want: [u8; 32] = hex::decode(&hash)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| CalmError::Internal("invalid token digest".into()))?;
    let ip = peer
        .map(|Extension(ConnectInfo(p))| p.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    match auth
        .login_throttle
        .attempt(ip, || bool::from(given.ct_eq(&want)))
    {
        LoginAttempt::Accepted => {}
        LoginAttempt::Rejected => return Err(CalmError::Unauthorized.into()),
        LoginAttempt::Throttled { retry_after } => {
            return Err(LoginError::Throttled {
                retry_after_secs: retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0),
            });
        }
    }
    let name = sessions::cookie_name(&id);
    for old in sessions::cookies(&headers, &name) {
        if auth.shares.get(old).is_some_and(|c| c.link_id == id) {
            auth.shares.remove(&auth.sessions, old)?;
        }
    }
    let token = auth.shares.create(
        &auth.sessions,
        SessionIdentity::owner(&auth.config),
        Credential {
            link_id: id,
            hash,
            expires_at: read.link.expires_at,
        },
    );
    // A revoke racing the mint cannot publish a usable credential.
    if crate::share_access::resolve_hash(
        s.repo.as_ref(),
        &auth.config,
        &auth.shares.get(&token).ok_or(CalmError::Unauthorized)?.hash,
        None,
    )
    .await
    .is_err()
    {
        auth.shares.remove(&auth.sessions, &token)?;
        return Err(CalmError::NotFound("share link".into()).into());
    }
    let mut cookie = crate::auth::build_session_cookie(&token);
    cookie.set_name(name);
    let mut response = Json(UnlockedShareLink {
        scope: read.link.delegation.scope,
        expires_at: read.link.expires_at,
    })
    .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie.to_string().parse().expect("cookie ascii"),
    );
    Ok(response)
}
