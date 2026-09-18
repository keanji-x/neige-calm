//! v2 is intentionally separate from the legacy approval endpoints.
use super::routes::{MobileAction, no_store, owner};
use crate::auth::{AuthState, Principal, build_session_cookie};
use crate::error::{CalmError, ErrorBody, Result};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::header,
    response::Response,
    routing::{delete, post},
};
use base64::Engine;
use calm_types::enrollment::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

pub fn management_router() -> Router<AuthState> {
    Router::new()
        .route("/api/mobile/enrollments", post(create).get(status))
        .route("/api/mobile/enrollments/{id}", delete(cancel))
        .layer(DefaultBodyLimit::max(4096))
        .layer(axum::middleware::map_response(|r: Response| async {
            no_store(r)
        }))
}

#[utoipa::path(get, path="/api/mobile/enrollments", tag="mobile", responses((status=200,body=EnrollmentCleanup),(status=403,body=ErrorBody)))]
pub async fn status(State(auth): State<AuthState>, principal: Principal) -> Result<Response> {
    owner(&auth, &principal)?;
    let result = auth
        .mobile
        .enrollment_control(
            EnrollmentAction::Status,
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
        .await?;
    Ok(no_store(Json(EnrollmentCleanup {
        pending_cleanup: result.pending_cleanup,
        detail: result.detail,
    })))
}
pub fn public_router() -> Router<AuthState> {
    Router::new()
        .route("/api/mobile/enrollments/claim", post(claim))
        .route("/api/mobile/enrollments/redeem", post(redeem))
        .layer(DefaultBodyLimit::max(4096))
        .layer(axum::middleware::map_response(|r: Response| async {
            no_store(r)
        }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanEnvelope<'a> {
    version: u32,
    enrollment_id: &'a str,
    origin: &'a str,
    auth_key: &'a str,
    auth_key_expires_at: i64,
    pair_ticket: &'a str,
    pair_expires_at: i64,
}

struct Reservation {
    mobile: super::MobileAccess,
    id: String,
    published: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.published {
            if let Ok(mut state) = self.mobile.lock() {
                state.cancel_scan(&self.id);
            }
            self.mobile.schedule_key_cleanup(self.id.clone());
        }
    }
}

#[utoipa::path(
    post, path="/api/mobile/enrollments", tag="mobile", request_body=MobileAction,
    responses((status=200, body=EnrollmentCreated),(status=400, body=ErrorBody),(status=403, body=ErrorBody))
)]
pub async fn create(
    State(auth): State<AuthState>,
    principal: Principal,
    Json(_body): Json<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    let _creating = auth
        .mobile
        .inner
        .enrollment_create
        .try_lock()
        .map_err(|_| {
            CalmError::BadRequest(
                "An enrollment creation is already in progress; do not retry an uncertain result"
                    .into(),
            )
        })?;
    let (id, generation, ticket, previous) = auth.mobile.lock()?.begin_scan()?;
    let mut reservation = Reservation {
        mobile: auth.mobile.clone(),
        id: id.clone(),
        published: false,
    };
    if let Some(previous) = previous {
        let _ = auth
            .mobile
            .enrollment_control(
                EnrollmentAction::Cancel,
                previous,
                uuid::Uuid::new_v4().to_string(),
            )
            .await;
    }
    let result = auth
        .mobile
        .enrollment_control(EnrollmentAction::Create, id.clone(), generation.clone())
        .await;
    let created = result.and_then(|key| {
        owner(&auth, &principal)?;
        let now = chrono::Utc::now().timestamp_millis();
        if key.pair_expires_at <= now
            || key.pair_expires_at > now + 180_000
            || key.pair_expires_at > key.auth_key_expires_at
            || key.auth_key_expires_at > now + 300_000
            || !key.auth_key.starts_with("tskey-auth-")
            || key.auth_key.len() > 1024
        {
            return Err(CalmError::BadRequest(
                "Issuer returned invalid key deadlines or capabilities".into(),
            ));
        }
        let envelope = ScanEnvelope {
            version: 2,
            enrollment_id: &id,
            origin: &key.origin,
            auth_key: &key.auth_key,
            auth_key_expires_at: key.auth_key_expires_at,
            pair_ticket: &ticket,
            pair_expires_at: key.pair_expires_at,
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|_| CalmError::Internal("Unable to encode scan envelope".into()))?;
        let payload = format!(
            "neige-enroll:v2:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        );
        if payload.len() > 2048 {
            return Err(CalmError::Internal(
                "Scan envelope exceeds size limit".into(),
            ));
        }
        let qr = qrcode::QrCode::new(payload.as_bytes())
            .map_err(|_| CalmError::Internal("Unable to encode scan QR".into()))?;
        let image = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(
                qr.render::<qrcode::render::svg::Color>()
                    .min_dimensions(256, 256)
                    .build()
            )
        );
        // Last grant transition shares the same lock as disable/cancel/revoke.
        let remaining = key.pair_expires_at - chrono::Utc::now().timestamp_millis();
        if remaining <= 0 {
            return Err(CalmError::Unauthorized);
        }
        auth.mobile.lock()?.finish_scan(
            &id,
            &generation,
            &key.origin,
            Duration::from_millis(remaining as u64),
        )?;
        Ok(EnrollmentCreated {
            enrollment_id: id.clone(),
            qr_payload: payload,
            qr_image: image,
            auth_key_expires_at: key.auth_key_expires_at,
            pair_expires_at: key.pair_expires_at,
        })
    });
    match created {
        Ok(created) => {
            reservation.published = true;
            Ok(no_store(Json(created)))
        }
        Err(error) => Err(error),
    }
}

#[utoipa::path(delete, path="/api/mobile/enrollments/{id}", tag="mobile", params(("id"=String,Path)), request_body=MobileAction, responses((status=200,body=EnrollmentCleanup),(status=403,body=ErrorBody)))]
pub async fn cancel(
    State(auth): State<AuthState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(_body): Json<MobileAction>,
) -> Result<Response> {
    owner(&auth, &principal)?;
    auth.mobile.lock()?.cancel_scan(&id);
    let result = auth
        .mobile
        .enrollment_control(
            EnrollmentAction::Cancel,
            id,
            uuid::Uuid::new_v4().to_string(),
        )
        .await?;
    Ok(no_store(Json(EnrollmentCleanup {
        pending_cleanup: result.pending_cleanup,
        detail: result.detail,
    })))
}

#[utoipa::path(post, path="/api/mobile/enrollments/claim", tag="mobile", request_body=EnrollmentClaim, responses((status=200,body=EnrollmentClaimed),(status=401,body=ErrorBody)))]
pub async fn claim(
    State(auth): State<AuthState>,
    Json(body): Json<EnrollmentClaim>,
) -> Result<Response> {
    if auth.config.dev_autologin {
        return Err(CalmError::Unauthorized);
    }
    Ok(no_store(Json(auth.mobile.lock()?.claim_scan(body)?)))
}

#[utoipa::path(post, path="/api/mobile/enrollments/redeem", tag="mobile", request_body=EnrollmentRedeem, responses((status=200,body=EnrollmentRedeemed),(status=401,body=ErrorBody)))]
pub async fn redeem(
    State(auth): State<AuthState>,
    Json(body): Json<EnrollmentRedeem>,
) -> Result<Response> {
    if auth.config.dev_autologin {
        return Err(CalmError::Unauthorized);
    }
    let session = auth.mobile.lock()?.redeem_scan(&body, &auth.sessions)?;
    auth.mobile.schedule_key_cleanup(body.enrollment_id.clone());
    let mut cookie = build_session_cookie(&session);
    cookie.set_secure(true);
    let mut response = no_store(Json(EnrollmentRedeemed {
        enrollment_id: body.enrollment_id,
        attempt_id: body.attempt_id,
        session_fingerprint: hex::encode(Sha256::digest(session.as_bytes())),
    }));
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie.to_string().parse().expect("cookie ascii"),
    );
    Ok(response)
}
