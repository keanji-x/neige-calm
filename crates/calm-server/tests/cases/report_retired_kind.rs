//! The retired `view` overlay kind (#2021 S4) at every REST write-end family: block upsert,
//! Replace, recipe ingress and fork each refuse it, while a report that still stores one stays
//! readable and its block stays deletable.

use axum::http::StatusCode;
use calm_server::auth::{self, AuthConfig, AuthState};
use calm_server::track_report::TrackReportPayload;
use calm_types::report_blocks::render_fence;
use serde_json::{Value, json};

use crate::track_recipe_instantiate::{Boot, boot, create_recipe, create_track_body, send};

const RETIRED_KIND: &str = "view.live";

/// What every fence-validating write end says about the retired kind.
fn unknown_kind_text() -> String {
    format!(
        "unknown block kind `{RETIRED_KIND}` — known data kinds: chart.candles, chart.series, \
         table, app, task, preview, view"
    )
}

fn retired_fence() -> String {
    render_fence(
        RETIRED_KIND,
        &json!({"source": "neige://plugin/operations/health", "version": 1}),
    )
}

/// The production router of [`boot`] behind a session layer, which the report block routes need.
fn app(boot: &Boot) -> axum::Router {
    let auth_state = AuthState::new(AuthConfig {
        username: None,
        password: None,
        dev_autologin: true,
        display_name: "owner".into(),
    });
    boot.app.clone().layer(axum::middleware::from_fn_with_state(
        auth_state,
        auth::require_session,
    ))
}

fn error_text(body: &Value) -> &str {
    body["error"].as_str().unwrap_or_default()
}

async fn report(boot: &Boot, track_id: &str) -> Value {
    let (status, report) = send(
        app(boot),
        "GET",
        &format!("/api/tracks/{track_id}/report"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    report
}

fn retired_block(report: &Value) -> &Value {
    report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == RETIRED_KIND)
        .expect("the stored retired block")
}

/// A Track whose report was persisted before S4. Seeded directly because no write end accepts
/// the kind any more. The first ordinary block write beside it migrates it into the CRDT; the
/// second loads that CRDT, the state a 4140 report is in.
async fn track_with_stored_retired_block(boot: &Boot, title: &str) -> String {
    let (status, created) = send(
        app(boot),
        "POST",
        "/api/tracks",
        Some(create_track_body(&boot.area_id, title, json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let track_id = created["id"].as_str().unwrap().to_string();
    let body = format!("# Performance\n\n{}", retired_fence());
    let payload = serde_json::to_string(&TrackReportPayload::new("legacy", body)).unwrap();
    let track = track_id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE cards SET payload = ?1, body_crdt = NULL \
                 WHERE track_id = ?2 AND kind = 'track-report'",
            )
            .bind(payload)
            .bind(track)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    for markdown in [
        "# Conclusion\n\nunchanged\n",
        "# Review\n\nstill writable\n",
    ] {
        let doc_rev = report(boot, &track_id).await["docRev"].clone();
        let (status, written) = send(
            app(boot),
            "POST",
            &format!("/api/tracks/{track_id}/report/blocks"),
            Some(json!({"kind": "prose", "markdown": markdown, "ifDocRev": doc_rev})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a write beside the stored block: {written}"
        );
        let has_crdt: bool = sqlx::query_scalar(
            "SELECT body_crdt IS NOT NULL FROM cards WHERE track_id = ?1 AND kind = 'track-report'",
        )
        .bind(&track_id)
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        assert!(has_crdt, "the report is CRDT-backed after the write");
        retired_block(&report(boot, &track_id).await);
    }
    track_id
}

#[tokio::test]
async fn retired_view_kind_is_refused_by_rest_block_upsert() {
    let boot = boot().await;
    let track_id = track_with_stored_retired_block(&boot, "retired-upsert").await;
    let before = report(&boot, &track_id).await;
    let stored = retired_block(&before);
    let payload = stored["payload"].clone();
    for (method, uri, body) in [
        (
            "POST",
            format!("/api/tracks/{track_id}/report/blocks"),
            json!({"kind": RETIRED_KIND, "payload": payload, "ifDocRev": before["docRev"]}),
        ),
        (
            "PATCH",
            format!(
                "/api/tracks/{track_id}/report/blocks/{}",
                stored["id"].as_str().unwrap()
            ),
            json!({"kind": RETIRED_KIND, "payload": payload, "ifBlockRev": stored["rev"]}),
        ),
    ] {
        let (status, error) = send(app(&boot), method, &uri, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method}: {error}");
        assert_eq!(
            error_text(&error),
            format!(
                "bad request: invalid `{RETIRED_KIND}` payload: {}",
                unknown_kind_text()
            ),
            "{method}"
        );
    }
    assert_eq!(report(&boot, &track_id).await["docRev"], before["docRev"]);
}

#[tokio::test]
async fn retired_view_kind_is_refused_by_rest_replace() {
    let boot = boot().await;
    let track_id = track_with_stored_retired_block(&boot, "retired-replace").await;
    let before = report(&boot, &track_id).await;
    // The current body verbatim: the stored block may not be written back.
    let (status, error) = send(
        app(&boot),
        "POST",
        &format!("/api/tracks/{track_id}/report"),
        Some(
            json!({"ifDocRev": before["docRev"], "summary": before["summary"],
                    "body": before["body"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(
        error_text(&error),
        format!(
            "bad request: invalid `{RETIRED_KIND}` block payload: {} (see neige_report_kinds)",
            unknown_kind_text()
        )
    );
    assert_eq!(report(&boot, &track_id).await["docRev"], before["docRev"]);
}

#[tokio::test]
async fn retired_view_kind_is_refused_by_recipe_ingress() {
    let boot = boot().await;
    let body = format!("# Performance\n\n{}", retired_fence());
    // `BadRequest` nests: the recipe prefix wraps the fence validator's own `bad request:`.
    let expected = format!(
        "bad request: track recipe body: bad request: invalid `{RETIRED_KIND}` block payload: {} \
         (see neige_report_kinds)",
        unknown_kind_text()
    );
    let (status, error) = send(
        app(&boot),
        "POST",
        "/api/track-recipes",
        Some(json!({"title": "old", "body": body})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error_text(&error), expected);

    let recipe = create_recipe(app(&boot), "current", "# Performance\n\nprose\n").await;
    let uri = format!("/api/track-recipes/{}", recipe["id"].as_str().unwrap());
    let (status, error) = send(
        app(&boot),
        "PUT",
        &uri,
        Some(json!({"title": recipe["title"], "body": body, "if_revision": recipe["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error_text(&error), expected);
    let (status, unchanged) = send(app(&boot), "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unchanged, recipe);
}

#[tokio::test]
async fn retired_view_kind_is_refused_by_fork() {
    let boot = boot().await;
    let source = track_with_stored_retired_block(&boot, "retired-fork-source").await;
    let stored = retired_block(&report(&boot, &source).await)["id"]
        .as_str()
        .unwrap()
        .to_string();
    let tracks_before = boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len();
    let (status, error) = send(
        app(&boot),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "retired-fork-target",
            json!({"fork_report_from": source}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(
        error_text(&error),
        format!(
            "bad request: track create: invalid forked report block {stored}: {}",
            unknown_kind_text()
        )
    );
    assert_eq!(
        boot.repo.tracks_by_area(&boot.area_id).await.unwrap().len(),
        tracks_before,
        "a refused fork creates no Track"
    );
}

#[tokio::test]
async fn a_stored_retired_view_kind_is_listed_and_still_deletable() {
    let boot = boot().await;
    let track_id = track_with_stored_retired_block(&boot, "retired-read").await;
    let before = report(&boot, &track_id).await;
    let stored = retired_block(&before);
    assert!(before["body"].as_str().unwrap().contains(&retired_fence()));
    let (status, deleted) = send(
        app(&boot),
        "DELETE",
        &format!(
            "/api/tracks/{track_id}/report/blocks/{}",
            stored["id"].as_str().unwrap()
        ),
        Some(json!({"ifBlockRev": stored["rev"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let after = report(&boot, &track_id).await;
    assert!(
        after["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["kind"] != RETIRED_KIND),
        "{after}"
    );
}
