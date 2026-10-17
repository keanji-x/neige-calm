//! Ordinary preview reads use the real paired-owner credential, not management authority.
use super::*;

#[tokio::test]
async fn paired_owner_keeps_github_preview_and_device_revocation() {
    let _guard = crate::support::forge_env::FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let f = fixture(json!({})).await;
    let phone = pair(&f).await;
    let session = f
        .auth
        .sessions
        .get(phone.split_once('=').unwrap().1)
        .unwrap();
    assert_eq!(
        session.authority,
        calm_server::auth::SessionAuthority::PairedDevice
    );
    assert_eq!(session.identity.user_id, calm_server::auth::OWNER_USER_ID);
    // Pairing's real local fixture is established before the CLI-only PATH is installed.
    let cli = super::super::github_preview::github_cli();
    let uri = "/api/github/preview?owner=o&repo=r&kind=issue&number=1";
    let (status, headers, body) = request(&f.public, "GET", uri, Some(&phone), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(body["title"], "Issue title");
    assert_eq!(cli.calls().len(), 1);
    let (status, _, devices) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let revoke = format!(
        "/api/mobile/devices/{}",
        devices["devices"][0]["id"].as_str().unwrap()
    );
    assert_eq!(
        request(
            &f.local,
            "DELETE",
            &revoke,
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&f.public, "GET", uri, Some(&phone), json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        cli.calls().len(),
        1,
        "revoked device must not use host credentials"
    );
    drop(cli);
    f.auth.mobile.shutdown().await.unwrap();
}
