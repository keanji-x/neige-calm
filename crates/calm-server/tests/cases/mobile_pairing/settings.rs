//! Settings uses deployment identity, not mobile-management credential provenance.
use super::*;

#[tokio::test]
async fn paired_owner_settings_access_ends_when_the_actual_device_is_revoked() {
    let f = fixture(json!({})).await;
    let phone = pair(&f).await;
    assert_eq!(
        request(&f.public, "GET", "/api/settings", Some(&phone), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, _, expected) = request(&f.public, "PUT", "/api/settings", Some(&phone),
        json!({"settings":{"paired_owner_setting":"allowed","planner_max_turn_duration_minutes":"0030"}})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        expected["settings"]["planner_max_turn_duration_minutes"],
        "30"
    );
    assert_eq!(
        request(&f.public, "GET", "/api/settings", Some(&phone), json!({}))
            .await
            .2,
        expected
    );
    let (_, _, access) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    let devices = access["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    let revoke = format!("/api/mobile/devices/{}", devices[0]["id"].as_str().unwrap());
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
    for method in ["GET", "PUT"] {
        assert_eq!(request(&f.public, method, "/api/settings", Some(&phone),
            json!({"settings":{"http_proxy":"http://revoked.invalid:3128","paired_owner_setting":"must not change"}})).await.0,
            StatusCode::UNAUTHORIZED);
    }
    let (status, _, actual) = request(
        &f.local,
        "GET",
        "/api/settings",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(actual, expected);
    f.auth.mobile.disable().await.unwrap();
}
