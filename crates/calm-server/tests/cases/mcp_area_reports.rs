//! `area/reports/` (#1838 S2) through the registered handlers (`neige.track.ls`, `neige.track.cat`,
//! `neige.report.find`): the Planner lists, finds and reads its own area's reports; another area's
//! reports are unreachable by any path; a Worker is Forbidden; names resolve exactly or refuse.

#![cfg(unix)]

use std::sync::Arc;

use calm_server::area_reports::MAX_REPORTS_PER_LISTING;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::area_reports::TOOL_REPORT_FIND;
use calm_server::mcp_server::tools::report_tag::TOOL_REPORT_TAG;
use calm_server::mcp_server::tools::track_file::{TOOL_TRACK_CAT, TOOL_TRACK_LS};
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_WRITE;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, TrackPatch, now_ms};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::track_report::TrackReportPayload;
use chrono::{Local, SecondsFormat, TimeZone};
use serde_json::{Value, json};

const FORBIDDEN: i64 = -32403;
const INVALID_PARAMS: i64 = -32602;

struct Side {
    area_id: AreaId,
    track_id: TrackId,
    planner: CardId,
    worker: CardId,
    report: CardId,
}

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    sqlx: Arc<SqlxRepo>,
    areas: [AreaId; 2],
    sides: Vec<Side>,
}

fn session_id(card: &CardId) -> String {
    format!("session-{card}")
}

async fn new_track(sqlx: &SqlxRepo, area: &AreaId, title: &str) -> TrackId {
    sqlx.track_create(NewTrack {
        template_input: None,
        area_id: area.clone(),
        title: title.into(),
        sort: None,
        cwd: String::new(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .unwrap()
    .id
}

async fn new_card(sqlx: &SqlxRepo, track: &TrackId, kind: &str, payload: Value) -> CardId {
    sqlx.card_create(NewCard {
        track_id: track.clone(),
        title: None,
        kind: kind.into(),
        sort: None,
        payload,
    })
    .await
    .unwrap()
    .id
}

/// A track with a report card, a Planner (with the live session the write gate resolves) and a Worker.
async fn side(sqlx: &SqlxRepo, cache: &CardRoleCache, area: &AreaId, title: &str) -> Side {
    let track = new_track(sqlx, area, title).await;
    let planner = new_card(sqlx, &track, "codex", json!({ "role": "planner" })).await;
    let worker = new_card(sqlx, &track, "codex", json!({ "task": "local" })).await;
    let report = new_card(
        sqlx,
        &track,
        "track-report",
        serde_json::to_value(TrackReportPayload::initial()).unwrap(),
    )
    .await;
    cache.insert(planner.clone(), CardRole::Planner, track.clone());
    cache.insert(worker.clone(), CardRole::Worker, track.clone());
    cache.insert(report.clone(), CardRole::ReportCard, track.clone());
    crate::support::mcp::set_persisted_card_role(sqlx, planner.as_str(), CardRole::Planner).await;
    let mut tx = sqlx.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session_id(&planner),
            card_id: planner.as_str().to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(format!("thread-{planner}")),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    Side {
        area_id: area.clone(),
        track_id: track,
        planner,
        worker,
        report,
    }
}

/// One side per `(area index, title)`, all minted before the context so its caches see them.
async fn boot(specs: &[(usize, &str)]) -> Boot {
    let sqlx = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let mut areas = Vec::new();
    for name in ["area-a", "area-b"] {
        let area = sqlx
            .area_create(NewArea {
                name: name.into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        areas.push(area.id);
    }
    let cache = CardRoleCache::new();
    let mut sides = Vec::new();
    for (area, title) in specs {
        sides.push(side(&sqlx, &cache, &areas[*area], title).await);
    }
    let repo: Arc<dyn Repo> = sqlx.clone();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let ctx = AppContext::new(
        repo,
        EventBus::new(),
        calm_server::state::WriteContext::new(cache, track_area_cache),
        None,
        Arc::new(tokio::sync::OnceCell::new()),
        Arc::new(tokio::sync::OnceCell::new()),
        std::env::temp_dir().join("neige-area-reports-gate-logs"),
    );
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    Boot {
        ctx,
        registry: Arc::new(registry),
        sqlx,
        areas: areas.try_into().unwrap(),
        sides,
    }
}

fn who(side: &Side, role: CardRole) -> ToolCallIdentity {
    let card = match role {
        CardRole::Worker => &side.worker,
        _ => &side.planner,
    };
    ToolCallIdentity {
        card_id: card.as_str().to_string(),
        role,
        provider: AgentProvider::Codex,
        session_id: session_id(card),
        track_id: Some(side.track_id.as_str().to_string()),
        area_id: side.area_id.as_str().to_string(),
        thread_id: format!("thread-{card}"),
    }
}

fn planner(side: &Side) -> ToolCallIdentity {
    who(side, CardRole::Planner)
}

async fn call(
    boot: &Boot,
    tool: &str,
    who: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot.registry.lookup(tool).expect("registered");
    handler(boot.ctx.clone(), who, args)
        .await
        .map(calm_server::mcp_server::result::ToolResult::into_structured)
}

async fn ls(boot: &Boot, who: ToolCallIdentity, path: &str) -> Result<Value, RpcError> {
    call(boot, TOOL_TRACK_LS, who, json!({ "path": path })).await
}

async fn cat(boot: &Boot, who: ToolCallIdentity, path: &str) -> Result<String, RpcError> {
    let value = call(boot, TOOL_TRACK_CAT, who, json!({ "path": path })).await?;
    Ok(value["content"].as_str().expect("content").to_string())
}

async fn find(boot: &Boot, who: ToolCallIdentity, args: Value) -> Result<Vec<String>, RpcError> {
    let value = call(boot, TOOL_REPORT_FIND, who, args).await?;
    Ok(paths(&value))
}

fn paths(listing: &Value) -> Vec<String> {
    listing
        .as_array()
        .expect("listing array")
        .iter()
        .map(|entry| entry["path"].as_str().expect("path").to_string())
        .collect()
}

async fn write_body(boot: &Boot, side: &Side, body: &str) {
    call(boot, TOOL_REPORT_READ, planner(side), json!({}))
        .await
        .expect("read");
    call(
        boot,
        TOOL_REPORT_WRITE,
        planner(side),
        json!({ "body": body, "message": "write" }),
    )
    .await
    .expect("neige.report.write");
}

async fn tag(boot: &Boot, side: &Side, add: &[&str]) {
    call(
        boot,
        TOOL_REPORT_TAG,
        planner(side),
        json!({ "path": "report.md", "add": add }),
    )
    .await
    .expect("neige.report.tag");
}

async fn set_report_updated_at(boot: &Boot, side: &Side, at: i64) {
    sqlx::query("UPDATE cards SET updated_at = ?1 WHERE id = ?2")
        .bind(at)
        .bind(side.report.as_str())
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
}

fn rfc3339(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, false)
}

fn assert_refused(result: Result<impl std::fmt::Debug, RpcError>, code: i64, needle: &str) {
    let err = result.expect_err(needle);
    assert_eq!(err.code, code, "{err:?}");
    assert!(err.message.contains(needle), "want {needle:?}: {err:?}");
}

#[tokio::test]
async fn planner_lists_finds_and_reads_every_report_of_its_area() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查"), (1, "机密")]).await;
    let (own, other) = (&boot.sides[0], &boot.sides[1]);
    tag(&boot, own, &["认证", "架构"]).await;
    tag(&boot, other, &["认证", "排障"]).await;
    write_body(&boot, other, "# 登录 排查\n\n结论：会话 过期。\n").await;
    set_report_updated_at(&boot, own, 2_000_000).await;
    set_report_updated_at(&boot, other, 1_000_000).await;

    assert_eq!(
        ls(&boot, planner(own), "area/").await.unwrap(),
        json!([{ "name": "reports/", "kind": "dir" }])
    );
    let listing = ls(&boot, planner(own), "/area/reports/").await.unwrap();
    assert_eq!(
        listing,
        json!([
            { "path": "area/reports/认证 方案.md", "title": "认证 方案",
              "trackId": own.track_id.as_str(), "tags": ["认证", "架构"], "updatedAt": rfc3339(2_000_000) },
            { "path": "area/reports/登录 排查.md", "title": "登录 排查",
              "trackId": other.track_id.as_str(), "tags": ["认证", "排障"], "updatedAt": rfc3339(1_000_000) }
        ]),
        "own track included, newest report first"
    );
    let root = ls(&boot, planner(own), "/").await.unwrap();
    assert!(
        !root.to_string().contains("area"),
        "the track root does not list area/: {root}"
    );

    let p = || planner(own);
    let both = vec!["area/reports/认证 方案.md", "area/reports/登录 排查.md"];
    for (args, want) in [
        (json!({ "path": "area/reports/" }), both.clone()),
        (
            json!({ "path": "area/reports", "name": "*认证*" }),
            vec![both[0]],
        ),
        (
            json!({ "path": "area/reports", "tag": "认证" }),
            both.clone(),
        ),
        (
            json!({ "path": "area/reports", "tag": "排障" }),
            vec![both[1]],
        ),
        (
            json!({ "path": "area/reports", "name": "认证 ?案.md", "tag": "架构" }),
            vec![both[0]],
        ),
        (
            json!({ "path": "area/reports", "name": "*认证*", "tag": "排障" }),
            vec![],
        ),
        (json!({ "path": "area/reports", "name": "认证" }), vec![]),
        (json!({ "path": "area/reports", "tag": "认" }), vec![]),
    ] {
        assert_eq!(
            find(&boot, p(), args.clone()).await.unwrap(),
            want,
            "{args}"
        );
    }

    assert_eq!(
        cat(&boot, p(), "area/reports/登录 排查.md").await.unwrap(),
        "# 登录 排查\n\n结论：会话 过期。\n"
    );
    assert_eq!(
        cat(&boot, p(), "area/reports/认证 方案.md").await.unwrap(),
        cat(&boot, p(), "report.md").await.unwrap(),
        "the own report reads exactly as report.md"
    );
    assert_eq!(
        cat(&boot, planner(other), "report.md").await.unwrap(),
        "# 登录 排查\n\n结论：会话 过期。\n",
        "report.md keeps serving the caller's own track"
    );
}

#[tokio::test]
async fn another_areas_reports_are_invisible_and_unreadable() {
    let boot = boot(&[(0, "认证 方案"), (1, "认证 方案"), (1, "机密")]).await;
    let (own, twin, secret) = (&boot.sides[0], &boot.sides[1], &boot.sides[2]);
    write_body(&boot, twin, "twin body\n").await;
    write_body(&boot, secret, "secret body\n").await;
    tag(&boot, secret, &["认证"]).await;

    let listing = ls(&boot, planner(own), "area/reports").await.unwrap();
    assert_eq!(
        paths(&listing),
        vec!["area/reports/认证 方案.md"],
        "no suffix: the twin is elsewhere"
    );
    for args in [
        json!({ "path": "area/reports" }),
        json!({ "path": "area/reports", "tag": "认证" }),
        json!({ "path": "area/reports", "name": "*" }),
    ] {
        let found = find(&boot, planner(own), args.clone()).await.unwrap();
        assert!(
            found.iter().all(|path| path == "area/reports/认证 方案.md"),
            "{args}: {found:?}"
        );
    }
    assert_ne!(
        cat(&boot, planner(own), "area/reports/认证 方案.md")
            .await
            .unwrap(),
        "twin body\n"
    );
    for path in [
        "area/reports/机密.md".to_string(),
        format!("area/reports/认证 方案~{}.md", twin.track_id.as_str()),
        format!("area/reports/机密~{}.md", &secret.track_id.as_str()[..8]),
        format!("area/reports/~{}.md", secret.track_id.as_str()),
    ] {
        assert_refused(
            cat(&boot, planner(own), &path).await,
            INVALID_PARAMS,
            "no report at",
        );
    }
    // The other area's own Planner sees exactly its own two reports.
    let theirs = ls(&boot, planner(twin), "area/reports").await.unwrap();
    assert_eq!(
        paths(&theirs),
        vec!["area/reports/机密.md", "area/reports/认证 方案.md"]
    );
    let _ = &boot.areas;
}

#[tokio::test]
async fn a_worker_is_forbidden_every_area_path_and_find() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查")]).await;
    let side = &boot.sides[0];
    let worker = || who(side, CardRole::Worker);
    for path in [
        "area",
        "area/reports/",
        "area/reports/登录 排查.md",
        "area/reports/../x",
    ] {
        assert_refused(ls(&boot, worker(), path).await, FORBIDDEN, "Planner's view");
        assert_refused(
            cat(&boot, worker(), path).await,
            FORBIDDEN,
            "Planner's view",
        );
    }
    assert_refused(
        find(
            &boot,
            worker(),
            json!({ "path": "area/reports/", "tag": "认证" }),
        )
        .await,
        FORBIDDEN,
        "Planner's view",
    );
    let mut assistant = planner(side);
    assistant.role = CardRole::Assistant;
    assert_refused(
        find(&boot, assistant, json!({ "path": "area/reports/" })).await,
        INVALID_PARAMS,
        "tool requires role",
    );
    // The Worker's own-track views are unchanged.
    assert_eq!(
        cat(&boot, worker(), "report.md").await.unwrap(),
        cat(&boot, planner(side), "report.md").await.unwrap()
    );
}

#[tokio::test]
async fn traversal_and_malformed_paths_are_refused() {
    let boot = boot(&[(0, "认证 方案"), (0, "a/b")]).await;
    let p = || planner(&boot.sides[0]);
    for path in [
        "area/reports/../report.md",
        "area/reports/../../x.md",
        "area/reports/a/b.md",
        "area/reports//认证 方案.md",
        "area/other",
        "area/../report.md",
    ] {
        assert_refused(cat(&boot, p(), path).await, INVALID_PARAMS, "not available");
        assert_refused(ls(&boot, p(), path).await, INVALID_PARAMS, "not available");
    }
    for (path, needle) in [
        ("area/reports/..%2F..%2Fx.md", "not a listed name"),
        ("area/reports/a%2fb.md", "malformed escape"),
        ("area/reports/.md", "untitled report"),
        ("area/reports/认证 方案", "end in `.md`"),
        ("area/reports/认证 方案~.md", "invalid `~` ID suffix"),
    ] {
        assert_refused(cat(&boot, p(), path).await, INVALID_PARAMS, needle);
    }
    assert_eq!(
        cat(&boot, p(), "area/reports/a%2Fb.md").await.unwrap(),
        cat(&boot, planner(&boot.sides[1]), "report.md")
            .await
            .unwrap(),
        "an escaped `/` resolves to its title"
    );
    assert_refused(
        cat(&boot, p(), "area/reports/").await,
        INVALID_PARAMS,
        "is a directory",
    );
    assert_refused(
        cat(&boot, p(), "area").await,
        INVALID_PARAMS,
        "is a directory",
    );
    assert_refused(
        ls(&boot, p(), "area/reports/认证 方案.md").await,
        INVALID_PARAMS,
        "not a directory",
    );
    for path in ["report.md", "area", "area/reports/x.md", "/"] {
        assert_refused(
            find(&boot, p(), json!({ "path": path })).await,
            INVALID_PARAMS,
            "only `area/reports/` can be searched",
        );
    }
    assert_refused(
        find(
            &boot,
            p(),
            json!({ "path": "area/reports", "area_id": boot.areas[1].as_str() }),
        )
        .await,
        INVALID_PARAMS,
        "unknown argument `area_id`",
    );
}

#[tokio::test]
async fn shared_titles_list_suffixed_and_a_bare_name_is_ambiguous() {
    let boot = boot(&[(0, "认证 方案"), (0, "认证 方案"), (0, "登录 排查")]).await;
    let (first, second) = (&boot.sides[0], &boot.sides[1]);
    write_body(&boot, first, "first\n").await;
    write_body(&boot, second, "second\n").await;
    let p = || planner(&boot.sides[2]);

    let listing = paths(&ls(&boot, p(), "area/reports").await.unwrap());
    let suffixed = |side: &Side| {
        listing
            .iter()
            .find(|path| path.contains(&side.track_id.as_str()[..8]))
            .cloned()
            .expect("listed with its id suffix")
    };
    let (first_path, second_path) = (suffixed(first), suffixed(second));
    for path in [&first_path, &second_path] {
        assert!(path.starts_with("area/reports/认证 方案~"), "{path}");
    }
    assert!(listing.contains(&"area/reports/登录 排查.md".to_string()));
    assert_eq!(cat(&boot, p(), &first_path).await.unwrap(), "first\n");
    assert_eq!(cat(&boot, p(), &second_path).await.unwrap(), "second\n");

    let err = cat(&boot, p(), "area/reports/认证 方案.md")
        .await
        .unwrap_err();
    assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
    assert!(err.message.contains("names 2 reports"), "{err:?}");
    for path in [&first_path, &second_path] {
        assert!(err.message.contains(path.as_str()), "{err:?} lists {path}");
    }

    // Renamed apart, the bare name is unique again and the old suffixed path still resolves.
    boot.sqlx
        .track_update(
            second.track_id.as_str(),
            TrackPatch {
                title: Some("认证 方案 第二版".into()),
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        cat(&boot, p(), "area/reports/认证 方案.md").await.unwrap(),
        "first\n"
    );
    assert_eq!(cat(&boot, p(), &first_path).await.unwrap(), "first\n");
    assert_eq!(
        cat(&boot, p(), "area/reports/认证 方案 第二版.md")
            .await
            .unwrap(),
        "second\n"
    );
    assert_refused(
        cat(&boot, p(), &second_path).await,
        INVALID_PARAMS,
        "no report at",
    );
}

#[tokio::test]
async fn a_body_write_is_read_back_and_moves_the_report_time() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查")]).await;
    let (own, other) = (&boot.sides[0], &boot.sides[1]);
    set_report_updated_at(&boot, other, 1_000).await;
    write_body(&boot, other, "v1\n").await;
    assert_eq!(
        cat(&boot, planner(own), "area/reports/登录 排查.md")
            .await
            .unwrap(),
        "v1\n"
    );
    write_body(&boot, other, "v2\n").await;
    assert_eq!(
        cat(&boot, planner(own), "area/reports/登录 排查.md")
            .await
            .unwrap(),
        "v2\n"
    );
    let listing = find(
        &boot,
        planner(own),
        json!({ "path": "area/reports", "name": "登*" }),
    )
    .await;
    assert_eq!(listing.unwrap(), vec!["area/reports/登录 排查.md"]);
    let value = call(
        &boot,
        TOOL_REPORT_FIND,
        planner(own),
        json!({ "path": "area/reports", "name": "登*" }),
    )
    .await
    .unwrap();
    assert_ne!(
        value[0]["updatedAt"],
        json!(rfc3339(1_000)),
        "a body write moves the report time"
    );
}

/// The time is the report card's own: a track change that is not a report change leaves it alone.
#[tokio::test]
async fn report_time_ignores_other_track_activity_and_follows_tag_changes() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查")]).await;
    let (own, other) = (&boot.sides[0], &boot.sides[1]);
    set_report_updated_at(&boot, other, 1_000).await;
    sqlx::query("UPDATE tracks SET updated_at = 1000 WHERE id = ?1")
        .bind(other.track_id.as_str())
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
    let moved = boot
        .sqlx
        .track_update(
            other.track_id.as_str(),
            TrackPatch {
                sort: Some(42.0),
                pinned_at: Some(Some(now_ms())),
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    assert_ne!(
        moved.updated_at, 1_000,
        "the fixture's activity must move the track"
    );

    let time = || async {
        let value = call(
            &boot,
            TOOL_REPORT_FIND,
            planner(own),
            json!({ "path": "area/reports", "name": "登*" }),
        )
        .await
        .unwrap();
        value[0]["updatedAt"].clone()
    };
    assert_eq!(time().await, json!(rfc3339(1_000)));
    tag(&boot, other, &["排障"]).await;
    assert_ne!(
        time().await,
        json!(rfc3339(1_000)),
        "a tag change is a report change"
    );
}

#[tokio::test]
async fn more_reports_than_the_cap_are_refused_not_truncated() {
    let boot = boot(&[(0, "认证 方案")]).await;
    let area = &boot.sides[0].area_id;
    for index in 0..MAX_REPORTS_PER_LISTING {
        let track = new_track(&boot.sqlx, area, &format!("r{index:03}")).await;
        new_card(
            &boot.sqlx,
            &track,
            "track-report",
            serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        )
        .await;
    }
    let p = || planner(&boot.sides[0]);
    let total = MAX_REPORTS_PER_LISTING + 1;
    assert_refused(
        ls(&boot, p(), "area/reports").await,
        INVALID_PARAMS,
        &format!("holds {total} reports, more than the {MAX_REPORTS_PER_LISTING}"),
    );
    assert_refused(
        find(&boot, p(), json!({ "path": "area/reports", "name": "*" })).await,
        INVALID_PARAMS,
        &format!("{total} reports match"),
    );
    let narrowed = find(&boot, p(), json!({ "path": "area/reports", "name": "r*" }))
        .await
        .unwrap();
    assert_eq!(
        narrowed.len(),
        MAX_REPORTS_PER_LISTING,
        "exactly the cap is served"
    );
    assert_eq!(
        cat(&boot, p(), "area/reports/r499.md").await.unwrap(),
        TrackReportPayload::initial().body,
        "reading is not capped"
    );
}

#[path = "mcp_area_report_blocks.rs"]
mod blocks;
