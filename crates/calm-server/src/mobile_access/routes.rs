use super::MobileStatus;
use crate::auth::{AuthState, Principal};
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, JsonBody, Path};
use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use calm_types::mobile_access::{PairingClaim, PairingClaimed, PairingCreated, PairingRedeem};
use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MobileAction {}

pub(super) fn owner(auth: &AuthState, principal: &Principal) -> Result<()> {
    if auth.config.dev_autologin
        || !auth
            .sessions
            .get(&principal.session_id)
            .is_some_and(|session| {
                session.authority == crate::auth::SessionAuthority::PasswordLogin
            })
    {
        return Err(CalmError::Forbidden(
            "Mobile access requires a real owner login".into(),
        ));
    }
    Ok(())
}

pub fn management_router() -> Router<AuthState> {
    Router::new()
        .merge(super::enrollment_routes::management_router())
        .route(
            "/api/mobile/access",
            get(status).post(enable).delete(disable),
        )
        .route("/api/mobile/tailnet/login", post(tailnet_login))
        .route("/api/mobile/tailnet/logout", post(tailnet_logout))
        .route("/api/mobile/pairings", post(create))
        .route("/api/mobile/pairings/{id}/approve", post(approve))
        .route("/api/mobile/devices/{id}", axum::routing::delete(revoke))
        .layer(DefaultBodyLimit::max(4096))
}

pub fn public_router() -> Router<AuthState> {
    Router::new()
        .merge(super::enrollment_routes::public_router())
        .route("/api/mobile/pairings/claim", post(claim))
        .route("/api/mobile/pairings/redeem", post(redeem))
        .route("/mobile/pair", get(bootstrap))
        .route("/mobile/pair.js", get(bootstrap_js))
        .route("/mobile/pair.css", get(bootstrap_css))
        .layer(DefaultBodyLimit::max(4096))
}

pub(super) fn no_store(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response
}

#[utoipa::path(
    get, path = "/api/mobile/access", tag = "mobile",
    operation_id = "get_mobile_access_status",
    responses((status = 200, body = MobileStatus), (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 500, body = ErrorBody))
)]
pub async fn status(State(auth): State<AuthState>, principal: Principal) -> Result<Response> {
    owner(&auth, &principal)?;
    Ok(no_store(Json(auth.mobile.status().await?)))
}

#[utoipa::path(
    post, path = "/api/mobile/access", tag = "mobile", request_body = MobileAction,
    responses((status = 200, body = MobileStatus), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a real owner login"),
        (status = 500, body = ErrorBody))
)]
pub async fn enable(
    State(auth): State<AuthState>,
    principal: Principal,
    JsonBody(_body): JsonBody<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.enable().await?;
    Ok(no_store(Json(auth.mobile.status().await?)))
}

#[utoipa::path(
    delete, path = "/api/mobile/access", tag = "mobile", request_body = MobileAction,
    responses((status = 200, body = MobileStatus), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a real owner login"),
        (status = 500, body = ErrorBody))
)]
pub async fn disable(
    State(auth): State<AuthState>,
    principal: Principal,
    JsonBody(_body): JsonBody<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.disable().await?;
    Ok(no_store(Json(auth.mobile.status().await?)))
}

#[utoipa::path(
    post, path = "/api/mobile/pairings", tag = "mobile", request_body = MobileAction,
    operation_id = "create_mobile_pairing",
    responses(
        (status = 200, body = PairingCreated, description = "A new invitation; it replaces the earlier ones no phone has claimed, so a retry leaves one live ticket"),
        (status = 400, body = ErrorBody, description = "`bad_request`: the pairing limit is reached"),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a real owner login"),
        (status = 409, body = ErrorBody, description = "`conflict`: mobile access is off"),
        (status = 500, body = ErrorBody),
    )
)]
pub async fn create(
    State(auth): State<AuthState>,
    principal: Principal,
    JsonBody(_body): JsonBody<MobileAction>,
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

#[utoipa::path(
    post, path = "/api/mobile/pairings/{id}/approve", tag = "mobile", params(("id" = String, Path)), request_body = MobileAction,
    responses(
        (status = 204, description = "Approved, also when it already was or its phone already joined"),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a real owner login"),
        (status = 404, body = ErrorBody, description = "`not_found`: no pending request with this id (expired, never claimed, or unknown)"),
        (status = 409, body = ErrorBody, description = "`conflict`: mobile access is off"),
        (status = 500, body = ErrorBody),
    )
)]
pub async fn approve(
    State(auth): State<AuthState>,
    principal: Principal,
    Path(id): Path<String>,
    JsonBody(_body): JsonBody<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.lock()?.approve(&id)?;
    Ok(no_store(StatusCode::NO_CONTENT))
}

#[utoipa::path(
    delete, path = "/api/mobile/devices/{id}", tag = "mobile", params(("id" = String, Path)), request_body = MobileAction,
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a real owner login"),
        (status = 404, body = ErrorBody, description = "`not_found`: no paired device with this id; a repeated revoke is answered so"),
        (status = 500, body = ErrorBody),
    )
)]
pub async fn revoke(
    State(auth): State<AuthState>,
    principal: Principal,
    Path(id): Path<String>,
    JsonBody(_body): JsonBody<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.lock()?.revoke(&id, &auth.sessions)?;
    Ok(no_store(StatusCode::NO_CONTENT))
}

#[utoipa::path(
    post, path = "/api/mobile/pairings/claim", tag = "mobile", request_body = PairingClaim, security(()),
    operation_id = "claim_mobile_pairing",
    responses((status = 200, body = PairingClaimed),
        (status = 400, body = ErrorBody, description = "`bad_request`: the device name is not 1–80 printable bytes"),
        (status = 401, body = ErrorBody), (status = 500, body = ErrorBody))
)]
pub async fn claim(
    State(auth): State<AuthState>,
    JsonBody(body): JsonBody<PairingClaim>,
) -> Result<Response> {
    if auth.config.dev_autologin {
        return Err(CalmError::Unauthorized);
    }
    Ok(no_store(Json(auth.mobile.lock()?.claim(body)?)))
}

#[utoipa::path(
    post, path = "/api/mobile/pairings/redeem", tag = "mobile", request_body = PairingRedeem, security(()),
    operation_id = "redeem_mobile_pairing",
    responses((status = 204), (status = 202),
        (status = 400, body = ErrorBody, description = "`bad_request`: the device limit is reached"),
        (status = 401, body = ErrorBody), (status = 500, body = ErrorBody))
)]
pub async fn redeem(
    State(auth): State<AuthState>,
    JsonBody(body): JsonBody<PairingRedeem>,
) -> Result<Response> {
    if auth.config.dev_autologin {
        return Err(CalmError::Unauthorized);
    }
    let Some(session) = auth.mobile.lock()?.redeem(body, &auth.sessions)? else {
        return Ok(no_store(StatusCode::ACCEPTED));
    };
    let cookie = super::build_device_cookie(&session);
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

#[utoipa::path(
    post, path = "/api/mobile/tailnet/login", tag = "mobile", request_body = MobileAction,
    responses((status = 200, body = calm_types::tailnet::TailnetLogin), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a real owner login"),
        (status = 500, body = ErrorBody))
)]
pub async fn tailnet_login(
    State(auth): State<AuthState>,
    principal: Principal,
    JsonBody(_body): JsonBody<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    let response = auth
        .mobile
        .tailnet_action(calm_types::tailnet::TailnetAction::Login)
        .await?;
    let login_url = response.login_url.ok_or_else(|| {
        CalmError::BadRequest("No login request available; refresh node status".into())
    })?;
    Ok(no_store(Json(calm_types::tailnet::TailnetLogin {
        login_url,
        display_for_seconds: 120,
    })))
}

#[utoipa::path(
    post, path = "/api/mobile/tailnet/logout", tag = "mobile", request_body = MobileAction,
    responses((status = 200, body = MobileStatus), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody, description = "`forbidden`: not a real owner login"),
        (status = 500, body = ErrorBody))
)]
pub async fn tailnet_logout(
    State(auth): State<AuthState>,
    principal: Principal,
    JsonBody(_body): JsonBody<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.lock()?.disable(&auth.sessions)?;
    auth.mobile
        .tailnet_action(calm_types::tailnet::TailnetAction::Logout)
        .await?;
    Ok(no_store(Json(auth.mobile.status().await?)))
}
