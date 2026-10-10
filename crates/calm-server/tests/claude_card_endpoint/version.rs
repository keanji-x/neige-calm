//! #2516: the Claude version a card's child started on, and the installed version the card's
//! Update compares it with. The CLI is a stand-in that answers `--version` from a file.

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::live_restart::{eventually, live_card, starts};
use super::{Boot, ENV_LOCK, post_restart, response_json};

const BUDGET: Duration = Duration::from_secs(60);

fn install(dir: &Path, version: &str) {
    std::fs::write(dir.join("version"), format!("{version} (Claude Code)\n")).unwrap();
}

async fn call(boot: &Boot, method: &str, uri: &str) -> (StatusCode, Value) {
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    response_json(resp).await
}

async fn installed(boot: &Boot) -> Value {
    let (status, body) = call(boot, "GET", "/api/claude/installed-version").await;
    assert_eq!(status, StatusCode::OK, "body={body:?}");
    body
}

async fn refresh(boot: &Boot) {
    let (status, body) = call(boot, "POST", "/api/claude/installed-version/refresh").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body:?}");
}

/// The card's runtime as `GET /api/tracks/{id}/cards` projects it.
async fn listed_runtime(boot: &Boot, card_id: &str) -> Value {
    let (status, cards) = call(boot, "GET", &format!("/api/tracks/{}/cards", boot.track_id)).await;
    assert_eq!(status, StatusCode::OK, "body={cards:?}");
    cards
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"] == card_id)
        .unwrap_or_else(|| panic!("card {card_id} listed: {cards}"))["runtime"]
        .clone()
}

/// A card's child records the version it started on; an Update after Claude was upgraded runs
/// the new version, records it, and reads the installed version again.
#[tokio::test]
async fn an_update_records_the_version_its_new_child_started_on() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let boot = &live.boot;
    let dir = live.log.parent().unwrap();
    assert_eq!(
        listed_runtime(boot, &live.card_id).await["claude_version"],
        "2.1.259"
    );

    install(dir, "2.1.280");
    let (status, card) = post_restart(boot.app.clone(), &live.card_id).await;
    assert_eq!(status, StatusCode::OK, "body={card:?}");
    assert_eq!(card["runtime"]["claude_version"], "2.1.280", "{card}");
    eventually("the replacement starts", || starts(&live.log).len() == 2).await;
    assert_eq!(
        listed_runtime(boot, &live.card_id).await["claude_version"],
        "2.1.280"
    );

    let deadline = tokio::time::Instant::now() + BUDGET;
    while installed(boot).await != json!({ "version": "2.1.280" }) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the Update never refreshed the installed version"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// The read serves the cache and never runs the CLI; the refresh does, at most once a minute.
#[tokio::test]
async fn the_installed_version_route_serves_what_the_last_refresh_read() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let boot = &live.boot;
    let dir = live.log.parent().unwrap();
    assert_eq!(
        installed(boot).await,
        json!({ "version": null }),
        "no refresh has run yet"
    );

    refresh(boot).await;
    assert_eq!(installed(boot).await, json!({ "version": "2.1.259" }));

    install(dir, "2.1.280");
    assert_eq!(
        installed(boot).await,
        json!({ "version": "2.1.259" }),
        "a read runs nothing"
    );
    refresh(boot).await;
    assert_eq!(
        installed(boot).await,
        json!({ "version": "2.1.259" }),
        "a second refresh within the minute is skipped"
    );

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}
