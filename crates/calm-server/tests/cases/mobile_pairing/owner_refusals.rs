//! An owner-side refusal on `/api/mobile/*` is never a 401: the browser reads any 401 as a lost
//! session and signs the owner out (#2131 S3). Only the phone-side claim/redeem keep 401.
use super::*;

async fn owner(f: &Fixture, method: &str, path: &str) -> (StatusCode, Value) {
    let (status, _, body) = request(&f.local, method, path, Some(&f.owner_cookie), json!({})).await;
    (status, body)
}

fn refused(answer: (StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(answer.0, status, "{}", answer.1);
    assert_eq!(answer.1["code"], code, "{}", answer.1);
}

/// The owner's session survives the refusal and the mobile state reads as it did before.
async fn unchanged(f: &Fixture, before: &Value) {
    assert_eq!(
        owner(f, "GET", "/api/auth/whoami").await.0,
        StatusCode::OK,
        "a refusal must not end the owner's session"
    );
    assert_eq!(&owner(f, "GET", "/api/mobile/access").await.1, before);
}

async fn enabled(f: &Fixture) {
    assert_eq!(
        owner(f, "POST", "/api/mobile/access").await.0,
        StatusCode::OK
    );
}

async fn invite(f: &Fixture) -> (String, String) {
    let (status, invitation) = owner(f, "POST", "/api/mobile/pairings").await;
    assert_eq!(status, StatusCode::OK, "{invitation}");
    let ticket = invitation["qrPayload"].as_str().unwrap();
    (
        invitation["id"].as_str().unwrap().to_owned(),
        ticket.split("#v1.").nth(1).unwrap().to_owned(),
    )
}

async fn claim(f: &Fixture, ticket: &str) -> (StatusCode, Value) {
    let (status, _, body) = request(
        &f.public,
        "POST",
        "/api/mobile/pairings/claim",
        None,
        json!({"ticket":ticket,"deviceName":"Fixture phone"}),
    )
    .await;
    (status, body)
}

fn approve(id: &str) -> String {
    format!("/api/mobile/pairings/{id}/approve")
}

#[tokio::test]
async fn mobile_owner_stale_approve_is_not_found_or_conflict_and_changes_nothing() {
    let f = fixture(json!({})).await;
    enabled(&f).await;
    let (unclaimed, _) = invite(&f).await;
    let before = owner(&f, "GET", "/api/mobile/access").await.1;
    for id in ["no-such-pairing", unclaimed.as_str()] {
        refused(
            owner(&f, "POST", &approve(id)).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
        unchanged(&f, &before).await;
    }
    let (_, ticket) = invite(&f).await;
    let (status, claimed) = claim(&f, &ticket).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        owner(&f, "DELETE", "/api/mobile/access").await.0,
        StatusCode::OK
    );
    let before = owner(&f, "GET", "/api/mobile/access").await.1;
    refused(
        owner(&f, "POST", &approve(claimed["id"].as_str().unwrap())).await,
        StatusCode::CONFLICT,
        "conflict",
    );
    unchanged(&f, &before).await;
    f.auth.mobile.disable().await.unwrap();
}

#[tokio::test]
async fn mobile_owner_repeated_approve_and_revoke_answer_done_and_keep_the_session() {
    let f = fixture(json!({})).await;
    enabled(&f).await;
    let (_, ticket) = invite(&f).await;
    let (_, claimed) = claim(&f, &ticket).await;
    let path = approve(claimed["id"].as_str().unwrap());
    for _ in 0..2 {
        assert_eq!(owner(&f, "POST", &path).await.0, StatusCode::NO_CONTENT);
    }
    let (status, _, _) = request(
        &f.public,
        "POST",
        "/api/mobile/pairings/redeem",
        None,
        json!({"id":claimed["id"],"secret":claimed["secret"]}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        owner(&f, "POST", &path).await.0,
        StatusCode::NO_CONTENT,
        "approving a pairing that already joined is done"
    );
    let status = owner(&f, "GET", "/api/mobile/access").await.1;
    assert_eq!(status["devices"].as_array().unwrap().len(), 1);
    let revoke = format!(
        "/api/mobile/devices/{}",
        status["devices"][0]["id"].as_str().unwrap()
    );
    assert_eq!(owner(&f, "DELETE", &revoke).await.0, StatusCode::NO_CONTENT);
    let before = owner(&f, "GET", "/api/mobile/access").await.1;
    assert_eq!(before["devices"], json!([]));
    for path in [revoke.as_str(), "/api/mobile/devices/no-such-device"] {
        refused(
            owner(&f, "DELETE", path).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
        unchanged(&f, &before).await;
    }
    refused(
        owner(&f, "POST", &path).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    unchanged(&f, &before).await;
    f.auth.mobile.disable().await.unwrap();
}

#[tokio::test]
async fn mobile_invitation_create_retry_leaves_one_live_ticket() {
    let f = fixture(json!({})).await;
    enabled(&f).await;
    let (_, lost) = invite(&f).await;
    let (_, retried) = invite(&f).await;
    assert_eq!(
        claim(&f, &lost).await.0,
        StatusCode::UNAUTHORIZED,
        "the ticket of a superseded invitation must no longer pair a phone"
    );
    let (status, claimed) = claim(&f, &retried).await;
    assert_eq!(status, StatusCode::OK);
    let (_, unused) = invite(&f).await;
    assert_eq!(
        owner(&f, "POST", &approve(claimed["id"].as_str().unwrap()))
            .await
            .0,
        StatusCode::NO_CONTENT,
        "a new invitation keeps a phone that already claimed and waits for approval"
    );
    assert_eq!(claim(&f, &unused).await.0, StatusCode::OK);
    f.auth.mobile.disable().await.unwrap();
}

#[tokio::test]
async fn scan_enrollment_owner_writes_with_access_off_are_conflict_not_unauthorized() {
    let (f, _, control) = private_tailnet_fixture().await;
    let before = owner(&f, "GET", "/api/mobile/access").await.1;
    assert_eq!(before["publicUrl"], Value::Null);
    refused(
        owner(&f, "POST", "/api/mobile/enrollments").await,
        StatusCode::CONFLICT,
        "conflict",
    );
    unchanged(&f, &before).await;
    let (status, cleanup) = owner(&f, "DELETE", "/api/mobile/enrollments/stale-slot").await;
    assert_eq!(status, StatusCode::OK, "{cleanup}");
    unchanged(&f, &before).await;
    f.auth.mobile.shutdown().await.unwrap();
    control.abort();
}
