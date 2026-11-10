//! PR2: production public routes, finite publication fences and real sockets.
use super::*;
use calm_server::model::NewCard;
use calm_types::report_blocks::KIND_TASK;
use std::time::Duration;

pub(super) async fn track(fx: &Fixture, area: &str) -> String {
    fx.state
        .raw_repo()
        .track_create(NewTrack {
            area_id: area.into(),
            title: "future Track".into(),
            sort: None,
            cwd: "/private/workspace".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string()
}

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

pub(super) fn base(token: &str, track: &str) -> String {
    format!("/api/share/{token}/tracks/{track}")
}

#[tokio::test]
async fn anonymous_report_projects_listed_blocks_and_owner_keeps_tasks() {
    let fx = Fixture::new().await;
    report(&fx).await;
    for scope in [true, false] {
        let link = fx.create(scope).await;
        let token = link["token"].as_str().unwrap();
        let (status, target) = fx
            .request("GET", &format!("/api/share/{token}"), None, json!(null))
            .await;
        assert_eq!(status, StatusCode::OK, "{target}");
        assert_eq!(
            target["tracks"][0],
            json!({"id":fx.track,"title":"shared Track"})
        );
        let (status, shared) = fx
            .request(
                "GET",
                &format!("{}/report", base(token, &fx.track)),
                None,
                json!(null),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{shared}");
        assert!(shared["body"].as_str().unwrap().contains("Readable prose"));
        assert!(!shared.to_string().contains("private command"));
        assert!(
            !shared["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["kind"] == KIND_TASK)
        );
        assert_eq!(shared["taskDiagnostics"], json!([]));
        for kind in ["table", "view"] {
            assert!(
                shared["blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|block| block["kind"] == kind)
            );
        }
        assert!(!target.to_string().contains("cwd"));
        assert!(!target.to_string().contains("workspace"));
        assert!(!target.to_string().contains("approverUserId"));
    }
    let (status, owner) = fx
        .request(
            "GET",
            &format!("/api/tracks/{}/report", fx.track),
            Some(&fx.cookie),
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{owner}");
    assert!(
        owner["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["kind"] == KIND_TASK)
    );
}

#[tokio::test]
async fn anonymous_token_exact_scope_dynamic_area_and_hidden_tracks() {
    let fx = Fixture::new().await;
    let area_link = fx.create(false).await;
    let track_link = fx.create(true).await;
    let future = track(&fx, &fx.area).await;
    let hidden = track(&fx, &fx.area).await;
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET purpose='area-chat' WHERE id=?1").bind(&hidden))
        .await
        .unwrap();
    let foreign_area = fx
        .state
        .raw_repo()
        .area_create(
            OWNER_USER_ID,
            NewArea {
                name: "Unshared Area".into(),
                color: "#ffffff".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let foreign_track = track(&fx, foreign_area.id.as_str()).await;
    for (token, id, expected) in [
        ("unknown", &fx.track, StatusCode::NOT_FOUND),
        (
            track_link["token"].as_str().unwrap(),
            &future,
            StatusCode::NOT_FOUND,
        ),
        (
            area_link["token"].as_str().unwrap(),
            &future,
            StatusCode::OK,
        ),
        (
            area_link["token"].as_str().unwrap(),
            &hidden,
            StatusCode::NOT_FOUND,
        ),
        (
            area_link["token"].as_str().unwrap(),
            &foreign_track,
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, value) = fx
            .request(
                "GET",
                &format!("{}/sources", base(token, id)),
                None,
                json!(null),
            )
            .await;
        assert_eq!(status, expected, "{value}");
    }
    let (_, target) = fx
        .request(
            "GET",
            &format!("/api/share/{}", area_link["token"].as_str().unwrap()),
            None,
            json!(null),
        )
        .await;
    let ids: Vec<_> = target["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&future.as_str()));
    assert!(!ids.contains(&hidden.as_str()));
    // Current ownership is part of every publication, not merely link creation.
    fx.register("alice").await;
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(
            sqlx::query(
                "UPDATE area_members SET user_id='alice' WHERE area_id=?1 AND role='owner'",
            )
            .bind(&fx.area),
        )
        .await
        .unwrap();
    for link in [area_link, track_link] {
        assert_eq!(
            fx.request(
                "GET",
                &format!("/api/share/{}", link["token"].as_str().unwrap()),
                None,
                json!(null)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
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
async fn anonymous_routes_reject_revoked_links_and_mutations() {
    let fx = Fixture::new().await;
    let link = fx.create(true).await;
    let token = link["token"].as_str().unwrap();
    let root = base(token, &fx.track);
    for suffix in ["sources", "conversations"] {
        assert_eq!(
            fx.request("GET", &format!("{root}/{suffix}"), None, json!(null))
                .await
                .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        fx.request(
            "POST",
            &format!("{root}/conversations"),
            None,
            json!({"text":"forbidden"})
        )
        .await
        .0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    fx.request(
        "DELETE",
        &format!(
            "/api/share-links/{}",
            link["link"]["delegation"]["id"].as_str().unwrap()
        ),
        Some(&fx.cookie),
        json!(null),
    )
    .await;
    for suffix in [
        "report",
        "report/series?block_id=b_a&rev=1",
        "sources",
        "sources/src_a",
        "conversations",
        "conversations/missing/items",
        "conversations/missing/live",
        "conversations/missing/run",
    ] {
        assert_eq!(
            fx.request("GET", &format!("{root}/{suffix}"), None, json!(null))
                .await
                .0,
            StatusCode::NOT_FOUND,
            "{suffix}"
        );
    }
}

#[tokio::test]
async fn report_publication_rechecks_revocation_after_assembly() {
    let fx = Fixture::new().await;
    report(&fx).await;
    let link = fx.create(true).await;
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
    let url = format!(
        "{}/report",
        base(link["token"].as_str().unwrap(), &fx.track)
    );
    let read = tokio::spawn(async move { request(&app, "GET", &url, None, json!(null)).await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        fx.request(
            "DELETE",
            &format!(
                "/api/share-links/{}",
                link["link"]["delegation"]["id"].as_str().unwrap()
            ),
            Some(&fx.cookie),
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    release.notify_one();
    assert_eq!(read.await.unwrap().0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn shared_sources_preserve_body_and_recheck_publication() {
    use calm_server::report_sources::{NewSource, Origin, Provenance, store};
    let fx = Fixture::new().await;
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    let source_track = fx.track.clone();
    db.writer()
        .with_writer(move |session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                store::insert_tx(
                    &mut tx,
                    &source_track,
                    &NewSource {
                        source_id: "src_12345678".into(),
                        provenance: Provenance::Manual,
                        origin: Origin::Manual {
                            url: Some("https://example.com/article".into()),
                            content_id: None,
                        },
                        title: "Public source".into(),
                        published_at: None,
                        body: "Readable captured source".into(),
                        body_sha256: calm_server::report_sources::sha256_hex(
                            b"Readable captured source",
                        ),
                        captured_at: 1,
                        quotes: vec![],
                    },
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    let link = fx.create(true).await;
    let root = base(link["token"].as_str().unwrap(), &fx.track);
    let (status, list) = fx
        .request("GET", &format!("{root}/sources"), None, json!(null))
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["sources"][0]["title"], "Public source");
    assert!(list["sources"][0].get("body").is_none());
    let (status, detail) = fx
        .request(
            "GET",
            &format!("{root}/sources/src_12345678"),
            None,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["body"], "Readable captured source");
}

#[tokio::test]
async fn shared_chart_series_reads_existing_resolved_data() {
    use calm_server::report_series::{NewRow, SeriesRequest, store};
    let fx = Fixture::new().await;
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/market_series_reply.json")).unwrap();
    initial_report(&fx).await;
    add_block(&fx, "chart.series", fixture["block"].clone(), None, 0).await;
    let link = fx.create(true).await;
    let root = base(link["token"].as_str().unwrap(), &fx.track);
    let (status, report) = fx
        .request("GET", &format!("{root}/report"), None, json!(null))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let block = report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == "chart.series")
        .unwrap();
    let block_id = block["id"].as_str().unwrap().to_owned();
    let rev = block["rev"].as_u64().unwrap();
    let request = SeriesRequest::from_payload(&fixture["block"]).unwrap();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    let track = fx.track.clone();
    let stored_block = block_id.clone();
    let data = fixture["reply"].clone();
    let summary = fixture["resolved"].clone();
    db.writer()
        .with_writer(move |session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                store::upsert_row_tx(
                    &mut tx,
                    &track,
                    &stored_block,
                    &request.request_hash,
                    &NewRow {
                        status: "ok".into(),
                        reason: None,
                        as_of: "2026-09-10".into(),
                        resolved_at: 1,
                        pinned: true,
                        summary: Some(summary),
                        data: Some(data),
                    },
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    let url = format!("{root}/report/series?block_id={block_id}&rev={rev}");
    let (status, resolved) = fx.request("GET", &url, None, json!(null)).await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["status"], "ok", "{resolved}");
    assert_eq!(
        resolved["series"][0]["points"],
        fixture["reply"]["series"][0]["points"]
    );
    let (status, conflict) = fx
        .request(
            "GET",
            &format!("{root}/report/series?block_id={block_id}&rev=9999"),
            None,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict, json!({"current_rev": rev}));
}

#[tokio::test]
async fn shared_reads_require_delete_access_not_just_collaboration() {
    let fx = Fixture::new().await;
    let track_link = fx.create(true).await;
    let area_link = fx.create(false).await;
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(
            sqlx::query("UPDATE area_members SET role='member' WHERE area_id=?1 AND user_id=?2")
                .bind(&fx.area)
                .bind(OWNER_USER_ID),
        )
        .await
        .unwrap();
    for link in [track_link, area_link] {
        let (status, value) = fx
            .request(
                "GET",
                &format!("/api/share/{}", link["token"].as_str().unwrap()),
                None,
                json!(null),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{value}");
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
