use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};

#[path = "browser_contract.rs"]
mod browser_contract;

pub(super) fn control_response(bytes: &[u8]) -> calm_types::enrollment::EnrollmentResponse {
    use calm_types::enrollment::{
        EnrollmentAction, EnrollmentRequest, EnrollmentResponse, EnrollmentResult,
    };
    let request: EnrollmentRequest = serde_json::from_slice(bytes).unwrap();
    let create = request.command.action == EnrollmentAction::Create;
    let now = chrono::Utc::now().timestamp_millis();
    EnrollmentResponse {
        version: 2,
        error: None,
        result: Some(EnrollmentResult {
            enrollment_id: request.command.enrollment_id,
            generation: request.command.generation,
            origin: if create {
                "https://fixture.example.ts.net".into()
            } else {
                String::new()
            },
            auth_key: if create {
                "tskey-auth-fixture-only".into()
            } else {
                String::new()
            },
            auth_key_expires_at: if create { now + 300_000 } else { 0 },
            pair_expires_at: if create { now + 180_000 } else { 0 },
            pending_cleanup: 0,
            detail: "Fixture cleanup complete".into(),
        }),
    }
}

async fn scan(f: &Fixture) -> (Value, Value) {
    assert_eq!(
        request(
            &f.local,
            "POST",
            "/api/mobile/access",
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, headers, created) = request(
        &f.local,
        "POST",
        "/api/mobile/enrollments",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let qr = created["qrPayload"].as_str().unwrap();
    assert!(qr.len() <= 2048);
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(qr.strip_prefix("neige-enroll:v2:").unwrap())
        .unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(envelope.as_object().unwrap().len(), 7);
    assert_eq!(envelope["version"], 2);
    assert_eq!(envelope["enrollmentId"], created["enrollmentId"]);
    assert_eq!(envelope["pairExpiresAt"], created["pairExpiresAt"]);
    assert_eq!(envelope["authKeyExpiresAt"], created["authKeyExpiresAt"]);
    (created, envelope)
}

#[tokio::test]
async fn scan_http_owner_create_claim_retry_redeem_cookie_and_revocation() {
    let (f, _, control) = private_tailnet_fixture().await;
    let (created, qr) = scan(&f).await;
    let claim = json!({"enrollmentId":created["enrollmentId"],"ticket":qr["pairTicket"],"deviceName":"Scan phone","attemptId":"client-attempt","attemptSecret":"a".repeat(64)});
    let (status, headers, first) = request(
        &f.public,
        "POST",
        "/api/mobile/enrollments/claim",
        None,
        claim.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let (_, _, again) = request(
        &f.public,
        "POST",
        "/api/mobile/enrollments/claim",
        None,
        claim.clone(),
    )
    .await;
    assert_eq!(first, again);
    assert_eq!(
        request(
            &f.public,
            "POST",
            "/api/mobile/pairings/claim",
            None,
            json!({"ticket":qr["pairTicket"],"deviceName":"No upgrade"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let mut other = claim;
    other["attemptId"] = json!("another-attempt");
    assert_eq!(
        request(
            &f.public,
            "POST",
            "/api/mobile/enrollments/claim",
            None,
            other
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let redeem = json!({"enrollmentId":created["enrollmentId"],"attemptId":"client-attempt","attemptSecret":"a".repeat(64)});
    let ((status, headers, body), (retry_status, retry_headers, retry)) = tokio::join!(
        request(
            &f.public,
            "POST",
            "/api/mobile/enrollments/redeem",
            None,
            redeem.clone()
        ),
        request(
            &f.public,
            "POST",
            "/api/mobile/enrollments/redeem",
            None,
            redeem.clone()
        )
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(body, retry);
    assert_eq!(
        headers[header::SET_COOKIE],
        retry_headers[header::SET_COOKIE]
    );
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("Secure"));
    assert!(cookie.contains("HttpOnly"));
    let cookie = cookie.split(';').next().unwrap();
    let (status, _, who) = request(
        &f.public,
        "GET",
        "/api/auth/whoami",
        Some(cookie),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["sessionFingerprint"],
        hex::encode(Sha256::digest(
            who["sessionId"].as_str().unwrap().as_bytes()
        ))
    );
    let (_, _, status) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(status["devices"].as_array().unwrap().len(), 1);
    assert_eq!(status["pending"], json!([]));
    let id = status["devices"][0]["id"].as_str().unwrap();
    assert_eq!(
        request(
            &f.local,
            "DELETE",
            &format!("/api/mobile/devices/{id}"),
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(
            &f.public,
            "POST",
            "/api/mobile/enrollments/redeem",
            None,
            redeem
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.public,
            "GET",
            "/api/auth/whoami",
            Some(cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}

#[tokio::test]
async fn scan_http_cancel_and_disable_invalidate_ticket_without_revival() {
    let (f, _, control) = private_tailnet_fixture().await;
    for cancel in [true, false] {
        let (created, qr) = scan(&f).await;
        if cancel {
            assert_eq!(
                request(
                    &f.local,
                    "DELETE",
                    &format!(
                        "/api/mobile/enrollments/{}",
                        created["enrollmentId"].as_str().unwrap()
                    ),
                    Some(&f.owner_cookie),
                    json!({})
                )
                .await
                .0,
                StatusCode::OK
            );
        } else {
            assert_eq!(
                request(
                    &f.local,
                    "DELETE",
                    "/api/mobile/access",
                    Some(&f.owner_cookie),
                    json!({})
                )
                .await
                .0,
                StatusCode::OK
            );
            assert_eq!(
                request(
                    &f.local,
                    "POST",
                    "/api/mobile/access",
                    Some(&f.owner_cookie),
                    json!({})
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        let claim = json!({"enrollmentId":created["enrollmentId"],"ticket":qr["pairTicket"],"deviceName":"Late phone","attemptId":"attempt","attemptSecret":"a".repeat(64)});
        let (status, headers, _) = request(
            &f.public,
            "POST",
            "/api/mobile/enrollments/claim",
            None,
            claim,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    }
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}

#[tokio::test]
async fn scan_http_strict_dto_and_dev_autologin_rejected() {
    let (f, _, control) = private_tailnet_fixture().await;
    assert_eq!(request(&f.public,"POST","/api/mobile/enrollments/redeem",None,json!({"enrollmentId":"id","attemptId":"attempt","attemptSecret":"a".repeat(64),"receipt":"forbidden"})).await.0,StatusCode::UNPROCESSABLE_ENTITY);
    let state = super::super::auth::fresh_state().await;
    let auth = AuthState::new(calm_server::auth::AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "Dev".into(),
    });
    let local = routes::application_router(state, auth);
    assert_eq!(
        request(&local, "POST", "/api/mobile/enrollments", None, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}
