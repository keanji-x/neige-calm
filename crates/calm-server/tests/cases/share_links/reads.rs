//! Ordinary resource authorization and publication fences.
use super::*;
use calm_server::model::NewCard;
use calm_types::report_blocks::KIND_TASK;
use std::time::Duration;
async fn initial_report(fx: &Fixture) {
    fx.state
        .raw_repo()
        .card_create(NewCard {
            track_id: fx.track.clone().into(),
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
            title: None,
        })
        .await
        .unwrap();
}

async fn add_block(
    fx: &Fixture,
    kind: &str,
    payload: Value,
    markdown: Option<&str>,
    rev: u64,
) -> Value {
    let (status, response) = fx
        .request(
            "POST",
            &format!("/api/tracks/{}/report/blocks", fx.track),
            Some(&fx.cookie),
            json!({"kind":kind,"payload":payload,"markdown":markdown,"ifDocRev":rev}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    response
}

async fn report(fx: &Fixture) {
    initial_report(fx).await;
    let view: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/native-view-v1.json"
    )))
    .unwrap();
    let mut rev = add_block(
        fx,
        "prose",
        Value::Null,
        Some("# Public\n\nReadable prose"),
        0,
    )
    .await["docRev"]
        .as_u64()
        .unwrap();
    for (kind, payload) in [
        (
            KIND_TASK,
            json!({"key":"secret","kind":"terminal","command":"private command","ready":false,"declared_by":"user"}),
        ),
        (
            "table",
            json!({"columns":[{"key":"value","label":"Value"}],"rows":[{"value":42}]}),
        ),
        ("view", view["valid"].clone()),
    ] {
        rev = add_block(fx, kind, payload, None, rev).await["docRev"]
            .as_u64()
            .unwrap();
    }
}

#[tokio::test]
async fn all_report_owner_reads_require_current_area_access() {
    let fx = Fixture::new().await;
    report(&fx).await;
    fx.register("alice").await;
    let token = fx.auth.sessions.create(
        SessionIdentity {
            user_id: "alice".into(),
            display_name: "Alice".into(),
            role: "owner".into(),
        },
        SessionAuthority::PasswordLogin,
    );
    let cookie = format!("{SESSION_COOKIE}={token}");
    for suffix in [
        "report",
        "report/series/missing?rev=1",
        "sources",
        "sources/missing",
    ] {
        let (status, value) = fx
            .request(
                "GET",
                &format!("/api/tracks/{}/{suffix}", fx.track),
                Some(&cookie),
                json!(null),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{suffix}: {value}");
    }
}

#[tokio::test]
async fn owner_report_publication_rechecks_membership_after_assembly() {
    let fx = Fixture::new().await;
    report(&fx).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::tracks::install_track_detail_read_hook_for_test(
        &fx.track,
        calm_server::routes::tracks::TrackDetailReadHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let app = fx.app.clone();
    let cookie = fx.cookie.clone();
    let url = format!("/api/tracks/{}/report", fx.track);
    let read =
        tokio::spawn(async move { request(&app, "GET", &url, Some(&cookie), json!(null)).await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    // A registered user with no membership cannot publish an assembled Report.
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(
            sqlx::query("DELETE FROM area_members WHERE area_id=?1 AND user_id=?2")
                .bind(&fx.area)
                .bind(OWNER_USER_ID),
        )
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(read.await.unwrap().0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn scoped_report_publication_rechecks_revocation_after_assembly() {
    let fx = Fixture::new().await;
    report(&fx).await;
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::routes::tracks::install_track_detail_read_hook_for_test(
        &fx.track,
        calm_server::routes::tracks::TrackDetailReadHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let app = fx.app.clone();
    let url = format!("/api/tracks/{}/report?share_link={id}", fx.track);
    let read =
        tokio::spawn(async move { request(&app, "GET", &url, Some(&cookie), Value::Null).await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        fx.request(
            "DELETE",
            &format!("/api/share-links/{id}"),
            Some(&fx.cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    release.notify_one();
    let (status, body) = read.await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.to_string().contains("Readable prose"));
}
