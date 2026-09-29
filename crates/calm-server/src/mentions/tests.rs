//! `GET /api/areas/{area_id}/mentions` through the real router: every `insert` resolves through the
//! `area/reports/` resolver `neige cat` / `neige find` use, only the caller's area is searched,
//! matching and ranking follow `rank`'s documented order, and every request reads afresh.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pulldown_cmark::{Event, Parser};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::rank::MAX_PER_GROUP;
use crate::area_reports::{self, AreaPath, Filter};
use crate::card_role_cache::CardRoleCache;
use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::event::{EditAuthor, EventBus};
use crate::ids::{ActorId, AreaId};
use crate::model::{NewArea, NewTrack, RequestTheme};
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::state::{AppState, CodexClient, DaemonClient, WriteContext};
use crate::track_area_cache::TrackAreaCache;
use crate::track_report::{TrackReportPayload, persist_report};

struct Fixture {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    areas: [AreaId; 2],
}

async fn boot() -> Fixture {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let mut areas = Vec::new();
    for name in ["mentions-a", "mentions-b"] {
        let area = repo
            .area_create(NewArea {
                name: name.into(),
                color: "#123456".into(),
                sort: None,
            })
            .await
            .unwrap();
        areas.push(area.id);
    }
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo_dyn.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-mentions-plugin-data"),
            Vec::new(),
            events,
            WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let app = crate::routes::router()
        .layer(axum::middleware::from_fn(crate::actor::actor_middleware))
        .with_state(state);
    Fixture {
        app,
        repo,
        areas: areas.try_into().unwrap(),
    }
}

/// A track in `area` with a report card holding `body`: written through the production report writer
/// (CRDT plus stored block projection) when `crdt`, else left as a legacy row with no CRDT.
/// `updated_at` pins the report card's update time so recency order is deterministic.
async fn report(
    fx: &Fixture,
    area: &AreaId,
    title: &str,
    body: &str,
    crdt: bool,
    tags: &[&str],
    updated_at: i64,
) -> String {
    let track = fx
        .repo
        .track_create(NewTrack {
            area_id: area.clone(),
            title: title.into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card_id = format!("report-{}", track.id.as_str());
    let initial = if crdt { "" } else { body };
    sqlx::query(
        "INSERT INTO cards (id, track_id, kind, sort, payload, role, deletable, created_at, updated_at) \
         VALUES (?1, ?2, 'track-report', -1, ?3, 'reportcard', 0, 1, 1)",
    )
    .bind(&card_id)
    .bind(track.id.as_str())
    .bind(json!({ "schemaVersion": 4, "summary": "", "body": initial }).to_string())
    .execute(fx.repo.pool())
    .await
    .unwrap();
    if crdt {
        let card = fx.repo.card_get(&card_id).await.unwrap().unwrap();
        let current: TrackReportPayload = serde_json::from_value(card.payload.clone()).unwrap();
        persist_report(
            fx.repo.as_ref(),
            &EventBus::new(),
            &WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            ActorId::Kernel,
            EditAuthor::Kernel,
            track.clone(),
            card,
            current,
            TrackReportPayload::new(String::new(), body.to_string()),
            0,
            None,
        )
        .await
        .unwrap();
    }
    add_tags(fx, track.id.as_str(), tags).await;
    set_updated_at(fx, track.id.as_str(), updated_at).await;
    track.id.as_str().to_string()
}

/// Tags through the production tag writer, as `neige tag` applies them.
async fn add_tags(fx: &Fixture, track_id: &str, tags: &[&str]) {
    let add: Vec<String> = tags.iter().map(|tag| tag.to_string()).collect();
    let mut tx = fx.repo.pool().begin().await.unwrap();
    crate::report_tags::store::apply_tx(&mut tx, track_id, &add, &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn set_updated_at(fx: &Fixture, track_id: &str, updated_at: i64) {
    sqlx::query("UPDATE cards SET updated_at = ?1 WHERE track_id = ?2 AND kind = 'track-report'")
        .bind(updated_at)
        .bind(track_id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
}

async fn request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    };
    let response = app.clone().oneshot(request.unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

async fn mentions(fx: &Fixture, area: &AreaId, q: &str, track: Option<&str>) -> Value {
    let mut uri = format!("/api/areas/{}/mentions?q={}", area.as_str(), encode(q));
    if let Some(track) = track {
        uri.push_str(&format!("&track={}", encode(track)));
    }
    let (status, body) = request(&fx.app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

fn labels(body: &Value, group: &str) -> Vec<String> {
    body[group]
        .as_array()
        .unwrap_or_else(|| panic!("`{group}` is an array: {body}"))
        .iter()
        .map(|item| item["label"].as_str().expect("label").to_string())
        .collect()
}

/// The code span after `@`, parsed by a CommonMark parser rather than by the scheme that built it.
fn code_text(insert: &str) -> String {
    let rest = insert
        .strip_prefix('@')
        .unwrap_or_else(|| panic!("insert starts with `@`: {insert:?}"));
    let events: Vec<Event> = Parser::new(rest).collect();
    let code: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            Event::Code(text) => Some(text.to_string()),
            _ => None,
        })
        .collect();
    let texts = events
        .iter()
        .filter(|event| matches!(event, Event::Text(_)))
        .count();
    assert!(
        code.len() == 1 && texts == 0,
        "insert is exactly `@` + one code span: {insert:?} -> {events:?}"
    );
    code.into_iter().next().unwrap()
}

/// The `area/reports/` file a path names, through the classifier `neige cat` routes by.
fn report_file(path: &str) -> &str {
    match area_reports::classify(path) {
        Some(Ok(AreaPath::Report(file))) => file,
        other => panic!("{path:?} is not an area report path: {other:?}"),
    }
}

/// Resolves every `insert` of `body` in `area` through `area_reports` and checks it names the item's
/// own track (and block). Returns how many inserts were checked.
async fn assert_inserts_resolve(fx: &Fixture, area: &AreaId, body: &Value) -> usize {
    let pool = fx.repo.pool();
    let listing = area_reports::list(pool, area.as_str(), &Filter::default())
        .await
        .unwrap();
    let track_of = |path: &str| {
        listing
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.track_id.clone())
    };
    let mut checked = 0;
    for tag in body["tags"].as_array().unwrap() {
        let text = code_text(tag["insert"].as_str().unwrap());
        let name = text
            .strip_prefix("tag:")
            .expect("tag insert is `tag:<tag>`");
        assert_eq!(name, tag["label"].as_str().unwrap());
        let found = area_reports::list(
            pool,
            area.as_str(),
            &Filter {
                name: None,
                tag: Some(name.to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            found.len() as u64,
            tag["track_count"].as_u64().unwrap(),
            "{tag}"
        );
        checked += 1;
    }
    for track in body["tracks"].as_array().unwrap() {
        let path = code_text(track["insert"].as_str().unwrap());
        area_reports::read(pool, area.as_str(), report_file(&path))
            .await
            .unwrap_or_else(|e| panic!("{path:?} must resolve: {e:?}"));
        assert_eq!(
            track_of(&path).as_deref(),
            track["track_id"].as_str(),
            "{path:?}"
        );
        checked += 1;
    }
    for block in body["blocks"].as_array().unwrap() {
        let text = code_text(block["insert"].as_str().unwrap());
        let (path, id) = text
            .rsplit_once('#')
            .expect("block insert is `<path>#<id>`");
        assert_eq!(id, block["block_id"].as_str().unwrap());
        let blocks = area_reports::read_blocks(pool, area.as_str(), report_file(path))
            .await
            .unwrap_or_else(|e| panic!("{path:?} must resolve: {e:?}"));
        assert!(
            blocks.iter().any(|b| b.id == id),
            "{text:?}: block {id} is in the report the path resolves to"
        );
        assert_eq!(
            track_of(path).as_deref(),
            block["track_id"].as_str(),
            "{text:?}"
        );
        checked += 1;
    }
    checked
}

const TWO_BLOCKS: &str = "# Goal\n\nalpha\n\n# Result\n\nbeta\n";

#[tokio::test]
async fn every_insert_resolves_through_the_area_reports_resolver() {
    let fx = boot().await;
    let [a, b] = fx.areas.clone();
    let titles = [
        "认证 方案",
        "认证 方案",
        "a/b 100% ~x",
        ".hidden",
        "C# notes",
        "tick `x`",
        "`edge`",
    ];
    let mut tracks = Vec::new();
    for (i, title) in titles.iter().enumerate() {
        let tag = format!("t`{i}");
        tracks.push(
            report(
                &fx,
                &a,
                title,
                TWO_BLOCKS,
                true,
                &[&tag, "共享"],
                100 + i as i64,
            )
            .await,
        );
    }
    tracks.push(report(&fx, &a, "legacy 报告", TWO_BLOCKS, false, &["共享"], 50).await);
    // The same title in another area neither resolves here nor changes this area's names.
    report(&fx, &b, "认证 方案", TWO_BLOCKS, true, &["共享"], 999).await;

    let listing: Vec<String> = area_reports::list(fx.repo.pool(), a.as_str(), &Filter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    let body = mentions(&fx, &a, "", None).await;
    let paths: Vec<String> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| code_text(track["insert"].as_str().unwrap()))
        .collect();
    assert_eq!(
        paths, listing,
        "the tracks are exactly the `ls area/reports/` listing, in its order"
    );
    assert!(
        paths.iter().any(|p| p.contains('~')),
        "duplicate titles carry `~<id>`: {paths:?}"
    );
    let mut checked = assert_inserts_resolve(&fx, &a, &body).await;
    for track in &tracks {
        let body = mentions(&fx, &a, "", Some(track)).await;
        assert_eq!(body["blocks"].as_array().unwrap().len(), 2, "{body}");
        checked += assert_inserts_resolve(&fx, &a, &body).await;
    }
    let body = mentions(&fx, &a, "goal", None).await;
    assert_eq!(body["blocks"].as_array().unwrap().len(), MAX_PER_GROUP);
    checked += assert_inserts_resolve(&fx, &a, &body).await;
    // q="": 8 tags + 8 tracks; each of the 8 tracks' recommendations: those again + its 2 blocks;
    // q="goal": 8 blocks and nothing else.
    assert_eq!(checked, 16 + 8 * (16 + 2) + 8);
}

#[tokio::test]
async fn another_areas_tracks_tags_and_blocks_never_appear() {
    let fx = boot().await;
    let [a, b] = fx.areas.clone();
    report(
        &fx,
        &a,
        "Public plan",
        "# Public heading\n\nx\n",
        true,
        &["public"],
        10,
    )
    .await;
    let secret = report(
        &fx,
        &b,
        "Secret plan",
        "# Secret heading\n\ny\n",
        true,
        &["secret"],
        20,
    )
    .await;

    let everything = mentions(&fx, &a, "", Some(&secret)).await;
    assert_eq!(labels(&everything, "tracks"), ["Public plan"]);
    assert_eq!(labels(&everything, "tags"), ["public"]);
    assert_eq!(
        labels(&everything, "blocks"),
        Vec::<String>::new(),
        "another area's track lifts nothing"
    );
    for q in ["secret", "plan", "heading"] {
        let body = mentions(&fx, &a, q, Some(&secret)).await;
        let all: Vec<String> = ["tags", "tracks", "blocks"]
            .iter()
            .flat_map(|group| labels(&body, group))
            .collect();
        assert!(
            all.iter()
                .all(|label| !label.to_lowercase().contains("secret")),
            "q={q:?}: {body}"
        );
    }
    assert_eq!(
        labels(&mentions(&fx, &a, "heading", None).await, "blocks"),
        ["Public heading"]
    );

    let (status, _) = request(
        &fx.app,
        "GET",
        &format!("/api/areas/{}/mentions", a.as_str()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "`q` is required");
    let (status, body) = request(&fx.app, "GET", "/api/areas/no-such-area/mentions?q=", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("no-such-area")),
        "{body}"
    );
}

#[tokio::test]
async fn fuzzy_matches_subsequences_and_cjk_substrings_and_caps_each_group() {
    let fx = boot().await;
    let [a, _] = fx.areas.clone();
    report(
        &fx,
        &a,
        "Deployment runbook",
        "# Rollout checklist\n\nx\n",
        true,
        &["deploy"],
        30,
    )
    .await;
    report(
        &fx,
        &a,
        "新部署 方案",
        "# 部署 步骤\n\ny\n",
        true,
        &["新部署"],
        20,
    )
    .await;
    report(
        &fx,
        &a,
        "Unrelated",
        "# Nothing here\n\nz\n",
        true,
        &["misc"],
        10,
    )
    .await;

    let body = mentions(&fx, &a, "dplrnbk", None).await;
    assert_eq!(labels(&body, "tracks"), ["Deployment runbook"]);
    let body = mentions(&fx, &a, "rllt chk", None).await;
    assert_eq!(
        labels(&body, "blocks"),
        ["Rollout checklist"],
        "space-separated terms all match"
    );
    let body = mentions(&fx, &a, "部署", None).await;
    assert_eq!(labels(&body, "tracks"), ["新部署 方案"]);
    assert_eq!(labels(&body, "tags"), ["新部署"]);
    assert_eq!(labels(&body, "blocks"), ["部署 步骤"]);
    let body = mentions(&fx, &a, "qzx", None).await;
    for group in ["tags", "tracks", "blocks"] {
        assert!(labels(&body, group).is_empty(), "{group}: {body}");
    }

    for i in 0..(MAX_PER_GROUP + 3) {
        let title = format!("Common {i}");
        let tag = format!("common-{i}");
        report(
            &fx,
            &a,
            &title,
            "# Common a\n\nx\n\n# Common b\n\ny\n",
            true,
            &[&tag],
            100,
        )
        .await;
    }
    let body = mentions(&fx, &a, "common", None).await;
    for group in ["tags", "tracks", "blocks"] {
        assert_eq!(labels(&body, group).len(), MAX_PER_GROUP, "{group}: {body}");
    }
    let body = mentions(&fx, &a, "", None).await;
    for group in ["tags", "tracks"] {
        assert_eq!(labels(&body, group).len(), MAX_PER_GROUP, "{group}: {body}");
    }
}

#[tokio::test]
async fn an_empty_query_recommends_in_the_documented_order() {
    let fx = boot().await;
    let [a, _] = fx.areas.clone();
    report(&fx, &a, "old", "# o\n\nx\n", true, &["rare", "mid"], 10).await;
    let current = report(
        &fx,
        &a,
        "current",
        "# First\n\nx\n\n# Second\n\ny\n\n# Third\n\nz\n",
        true,
        &["mid", "often"],
        20,
    )
    .await;
    report(&fx, &a, "new", "# n\n\nx\n", true, &["often", "fresh"], 30).await;
    report(&fx, &a, "newest", "# m\n\nx\n", true, &["often"], 40).await;

    let body = mentions(&fx, &a, "", Some(&current)).await;
    assert_eq!(labels(&body, "tracks"), ["newest", "new", "current", "old"]);
    // Most-used first; equal use breaks by the newest report carrying the tag.
    assert_eq!(labels(&body, "tags"), ["often", "mid", "fresh", "rare"]);
    assert_eq!(
        labels(&body, "blocks"),
        ["First", "Second", "Third"],
        "the current track's blocks, in document order"
    );
    let counts: Vec<u64> = body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["track_count"].as_u64().unwrap())
        .collect();
    assert_eq!(counts, [3, 2, 1, 1]);
    assert_eq!(body["blocks"][0]["track_title"], "current");

    let body = mentions(&fx, &a, "  ", None).await;
    assert!(
        labels(&body, "blocks").is_empty(),
        "no track, no block recommendations: {body}"
    );
    assert_eq!(labels(&body, "tracks"), ["newest", "new", "current", "old"]);
    assert_eq!(body, mentions(&fx, &a, "  ", None).await, "deterministic");
}

#[tokio::test]
async fn the_track_parameter_lifts_that_tracks_blocks() {
    let fx = boot().await;
    let [a, _] = fx.areas.clone();
    let older = report(&fx, &a, "Older", "# Deploy steps\n\nx\n", true, &[], 1_000).await;
    let newer = report(&fx, &a, "Newer", "# Deploy steps\n\ny\n", true, &[], 2_000).await;
    let owners = |body: &Value| -> Vec<String> {
        body["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["track_title"].as_str().unwrap().to_string())
            .collect()
    };

    assert_eq!(
        owners(&mentions(&fx, &a, "deploy", None).await),
        ["Newer", "Older"],
        "newer report first"
    );
    assert_eq!(
        owners(&mentions(&fx, &a, "deploy", Some(&older)).await),
        ["Older", "Newer"],
        "the current track first"
    );
    assert_eq!(
        owners(&mentions(&fx, &a, "deploy", Some(&newer)).await),
        ["Newer", "Older"]
    );
}

#[tokio::test]
async fn a_new_tag_and_a_rename_show_on_the_next_request() {
    let fx = boot().await;
    let [a, _] = fx.areas.clone();
    let track = report(&fx, &a, "Before rename", TWO_BLOCKS, true, &["first"], 10).await;

    let before = mentions(&fx, &a, "", None).await;
    assert_eq!(labels(&before, "tracks"), ["Before rename"]);
    assert_eq!(labels(&before, "tags"), ["first"]);

    add_tags(&fx, &track, &["second"]).await;
    let (status, body) = request(
        &fx.app,
        "PATCH",
        &format!("/api/tracks/{track}"),
        Some(json!({ "title": "After rename" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let after = mentions(&fx, &a, "", Some(&track)).await;
    assert_eq!(labels(&after, "tracks"), ["After rename"]);
    let mut tags = labels(&after, "tags");
    tags.sort();
    assert_eq!(tags, ["first", "second"]);
    assert_eq!(after["blocks"][0]["track_title"], "After rename");
    assert_eq!(assert_inserts_resolve(&fx, &a, &after).await, 1 + 2 + 2);
}

#[test]
fn an_insert_is_one_code_span_whatever_backticks_the_text_holds() {
    for text in [
        "area/reports/a b.md",
        "tag:x`y",
        "a``b",
        "`lead",
        "trail`",
        "`",
    ] {
        assert_eq!(code_text(&super::mention_insert(text)), text, "{text:?}");
    }
}

/// Block ids of `body`'s `blocks` group, in order.
fn block_ids(body: &Value) -> Vec<String> {
    body["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["block_id"].as_str().unwrap().to_string())
        .collect()
}

async fn read_block_ids(fx: &Fixture, area: &AreaId, file: &str) -> Vec<String> {
    area_reports::read_blocks(fx.repo.pool(), area.as_str(), file)
        .await
        .unwrap()
        .into_iter()
        .map(|block| block.id)
        .collect()
}

async fn set_payload(fx: &Fixture, track_id: &str, payload: &str) {
    sqlx::query("UPDATE cards SET payload = ?1 WHERE track_id = ?2 AND kind = 'track-report'")
        .bind(payload)
        .bind(track_id)
        .execute(fx.repo.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_row_without_a_crdt_mentions_the_block_ids_neige_cat_reads() {
    let fx = boot().await;
    let [a, _] = fx.areas.clone();
    let body = "# Goal\n\nalpha\n\n```neige-block task\n\
                {\"key\":\"t1\",\"kind\":\"terminal\",\"goal\":\"printf ok\"}\n```\n";
    let track = report(&fx, &a, "legacy", body, false, &[], 10).await;
    // Stored block hints with other ids: a row with no CRDT reads as its body, ignoring them.
    let hints: Vec<Value> = crate::track_report_read::legacy_row_blocks(body)
        .into_iter()
        .enumerate()
        .map(|(i, block)| {
            json!({ "id": format!("b_hint{i}"), "kind": block.kind, "rev": block.rev, "payload": block.payload })
        })
        .collect();
    set_payload(
        &fx,
        &track,
        &json!({ "schemaVersion": 4, "summary": "", "body": body, "blocks": hints }).to_string(),
    )
    .await;

    let got = mentions(&fx, &a, "", Some(&track)).await;
    let ids = block_ids(&got);
    assert_eq!(ids, read_block_ids(&fx, &a, "legacy.md").await, "{got}");
    assert!(ids.iter().all(|id| !id.starts_with("b_hint")), "{ids:?}");
    assert!(
        labels(&got, "blocks").contains(&"task: command=printf ok".to_string()),
        "the legacy terminal task reads normalized: {got}"
    );
    assert_eq!(assert_inserts_resolve(&fx, &a, &got).await, 1 + 2);
}

#[tokio::test]
async fn a_crdt_row_mentions_its_stored_projection_without_loading_the_crdt() {
    let fx = boot().await;
    let [a, _] = fx.areas.clone();
    let track = report(&fx, &a, "projected", TWO_BLOCKS, true, &[], 10).await;
    let before = mentions(&fx, &a, "", Some(&track)).await;
    assert_eq!(
        block_ids(&before),
        read_block_ids(&fx, &a, "projected.md").await
    );
    assert_eq!(assert_inserts_resolve(&fx, &a, &before).await, 1 + 2);

    // A blob no loader could open: the mention read never touches it.
    sqlx::query("UPDATE cards SET body_crdt = x'00' WHERE track_id = ?1 AND kind = 'track-report'")
        .bind(&track)
        .execute(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(mentions(&fx, &a, "", Some(&track)).await, before);
}

#[tokio::test]
async fn a_crdt_row_without_a_projection_lists_its_report_without_blocks() {
    let fx = boot().await;
    let [a, _] = fx.areas.clone();
    let bare = report(&fx, &a, "bare", TWO_BLOCKS, true, &["kept"], 10).await;
    let other = report(&fx, &a, "other", TWO_BLOCKS, true, &[], 20).await;
    sqlx::query(
        "UPDATE cards SET payload = json_remove(payload, '$.blocks') \
         WHERE track_id = ?1 AND kind = 'track-report'",
    )
    .bind(&bare)
    .execute(fx.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        read_block_ids(&fx, &a, "bare.md").await.len(),
        2,
        "cat still reads it"
    );

    let got = mentions(&fx, &a, "", Some(&bare)).await;
    assert_eq!(labels(&got, "tracks"), ["other", "bare"]);
    assert_eq!(labels(&got, "tags"), ["kept"]);
    assert!(block_ids(&got).is_empty(), "{got}");
    let searched = mentions(&fx, &a, "goal", Some(&other)).await;
    assert_eq!(
        block_ids(&searched),
        read_block_ids(&fx, &a, "other.md").await[..1]
    );
    assert_eq!(assert_inserts_resolve(&fx, &a, &got).await, 1 + 2);
    assert_eq!(assert_inserts_resolve(&fx, &a, &searched).await, 1);
}
