use super::MobileStatus;
use crate::auth::{AuthState, Principal, build_session_cookie};
use crate::error::{CalmError, ErrorBody, Result};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use calm_types::mobile_access::{PairingClaim, PairingClaimed, PairingCreated, PairingRedeem};
use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MobileAction {}

fn owner(auth: &AuthState, principal: &Principal) -> Result<()> {
    if auth.config.dev_autologin || auth.sessions.get(&principal.session_id).is_none() {
        return Err(CalmError::Forbidden(
            "Mobile access requires a real owner login".into(),
        ));
    }
    Ok(())
}

pub fn management_router() -> Router<AuthState> {
    Router::new()
        .route(
            "/api/mobile/access",
            get(status).post(enable).delete(disable),
        )
        .route("/api/mobile/pairings", post(create))
        .route("/api/mobile/pairings/{id}/approve", post(approve))
        .route("/api/mobile/devices/{id}", axum::routing::delete(revoke))
        .layer(DefaultBodyLimit::max(4096))
}

pub fn public_router() -> Router<AuthState> {
    Router::new()
        .route("/api/mobile/pairings/claim", post(claim))
        .route("/api/mobile/pairings/redeem", post(redeem))
        .route("/mobile/pair", get(bootstrap))
        .route("/mobile/pair.js", get(bootstrap_js))
        .route("/mobile/pair.css", get(bootstrap_css))
        .layer(DefaultBodyLimit::max(4096))
}

fn no_store(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response
}

#[utoipa::path(get, path = "/api/mobile/access", tag = "mobile", responses((status = 200, body = MobileStatus), (status = 401, body = ErrorBody)))]
pub async fn status(State(auth): State<AuthState>, principal: Principal) -> Result<Response> {
    owner(&auth, &principal)?;
    Ok(no_store(Json(auth.mobile.status().await?)))
}

#[utoipa::path(post, path = "/api/mobile/access", tag = "mobile", request_body = MobileAction, responses((status = 200, body = MobileStatus), (status = 400, body = ErrorBody)))]
pub async fn enable(
    State(auth): State<AuthState>,
    principal: Principal,
    Json(_body): Json<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.enable().await?;
    Ok(no_store(Json(auth.mobile.status().await?)))
}

#[utoipa::path(delete, path = "/api/mobile/access", tag = "mobile", request_body = MobileAction, responses((status = 200, body = MobileStatus), (status = 400, body = ErrorBody)))]
pub async fn disable(
    State(auth): State<AuthState>,
    principal: Principal,
    Json(_body): Json<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.disable().await?;
    Ok(no_store(Json(auth.mobile.status().await?)))
}

#[utoipa::path(post, path = "/api/mobile/pairings", tag = "mobile", request_body = MobileAction, responses((status = 200, body = PairingCreated), (status = 400, body = ErrorBody)))]
pub async fn create(
    State(auth): State<AuthState>,
    principal: Principal,
    Json(_body): Json<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    let (id, payload, expires) = auth.mobile.lock()?.invite()?;
    let qr = qrcode::QrCode::new(payload.as_bytes())
        .map_err(|_| CalmError::Internal("Unable to encode pairing QR".into()))?;
    let svg = qr
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(256, 256)
        .build();
    let image = format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    );
    Ok(no_store(Json(PairingCreated {
        id,
        qr_payload: payload,
        qr_image: image,
        expires_in_seconds: expires,
    })))
}

#[utoipa::path(post, path = "/api/mobile/pairings/{id}/approve", tag = "mobile", params(("id" = String, Path)), request_body = MobileAction, responses((status = 204), (status = 401, body = ErrorBody)))]
pub async fn approve(
    State(auth): State<AuthState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(_body): Json<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.lock()?.approve(&id)?;
    Ok(no_store(StatusCode::NO_CONTENT))
}

#[utoipa::path(delete, path = "/api/mobile/devices/{id}", tag = "mobile", params(("id" = String, Path)), request_body = MobileAction, responses((status = 204), (status = 401, body = ErrorBody)))]
pub async fn revoke(
    State(auth): State<AuthState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(_body): Json<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.lock()?.revoke(&id, &auth.sessions)?;
    Ok(no_store(StatusCode::NO_CONTENT))
}

#[utoipa::path(post, path = "/api/mobile/pairings/claim", tag = "mobile", request_body = PairingClaim, responses((status = 200, body = PairingClaimed), (status = 401, body = ErrorBody)))]
pub async fn claim(
    State(auth): State<AuthState>,
    Json(body): Json<PairingClaim>,
) -> Result<Response> {
    if auth.config.dev_autologin {
        return Err(CalmError::Unauthorized);
    }
    Ok(no_store(Json(auth.mobile.lock()?.claim(body)?)))
}

#[utoipa::path(post, path = "/api/mobile/pairings/redeem", tag = "mobile", request_body = PairingRedeem, responses((status = 204), (status = 202), (status = 401, body = ErrorBody)))]
pub async fn redeem(
    State(auth): State<AuthState>,
    Json(body): Json<PairingRedeem>,
) -> Result<Response> {
    if auth.config.dev_autologin {
        return Err(CalmError::Unauthorized);
    }
    let Some(session) = auth.mobile.lock()?.redeem(body, &auth.sessions)? else {
        return Ok(no_store(StatusCode::ACCEPTED));
    };
    let mut cookie = build_session_cookie(&session);
    cookie.set_secure(true);
    let mut response = no_store(StatusCode::NO_CONTENT);
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie.to_string().parse().expect("cookie ascii"),
    );
    Ok(response)
}

async fn bootstrap() -> Response {
    let mut response = no_store((
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("pair.html"),
    ));
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().expect("static header"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        "no-referrer".parse().expect("static header"),
    );
    response
}
async fn bootstrap_js() -> Response {
    no_store((
        [(header::CONTENT_TYPE, "text/javascript")],
        include_str!("pair.js"),
    ))
}
async fn bootstrap_css() -> Response {
    no_store((
        [(header::CONTENT_TYPE, "text/css")],
        include_str!("pair.css"),
    ))
}
