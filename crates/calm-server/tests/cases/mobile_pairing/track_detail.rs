use super::*;
use calm_server::auth::{OWNER_USER_ID, SessionAuthority};
use calm_server::model::{NewArea, NewTrack};

#[tokio::test]
async fn paired_owner_reads_track_detail_and_revocation_denies_the_next_read() {
    let f = fixture(json!({})).await;
    let repo = f.state.raw_repo();
    let area = repo
        .area_create(
            OWNER_USER_ID,
            NewArea {
                name: "Phone Track".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            area_id: area.id,
            title: "Phone Track".into(),
            sort: None,
            cwd: "/fixture/phone-track".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let phone = pair(&f).await;
    let token = phone.strip_prefix(&format!("{SESSION_COOKIE}=")).unwrap();
    let session = f.auth.sessions.get(token).unwrap();
    assert_eq!(session.identity.user_id, OWNER_USER_ID);
    assert_eq!(session.authority, SessionAuthority::PairedDevice);
    let path = format!("/api/tracks/{}", track.id);
    let (status, _, body) = request(&f.public, "GET", &path, Some(&phone), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["track"]["id"], track.id.as_str());
    let (_, _, status) = request(
        &f.local,
        "GET",
        "/api/mobile/access",
        Some(&f.owner_cookie),
        json!({}),
    )
    .await;
    let device = status["devices"][0]["id"].as_str().unwrap();
    assert_eq!(
        request(
            &f.local,
            "DELETE",
            &format!("/api/mobile/devices/{device}"),
            Some(&f.owner_cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(f.auth.sessions.get(token).is_none());
    assert_eq!(
        request(&f.public, "GET", &path, Some(&phone), json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    f.auth.mobile.disable().await.unwrap();
}
