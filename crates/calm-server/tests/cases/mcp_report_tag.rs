//! `calm.report.tag` (#1838 S1) through the registered handler: the tagged report is always the
//! bound card's own track; `report.md` is the only path; the Planner changes tags, a Worker lists
//! them; tags round-trip in insertion order.

#![cfg(unix)]

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::ids::{CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::report_tag::TOOL_REPORT_TAG;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, now_ms};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::track_report::TrackReportPayload;
use serde_json::{Value, json};

const INVALID_PARAMS: i64 = -32602;

struct Side {
    track_id: TrackId,
    planner: CardId,
    worker: CardId,
    report: CardId,
}

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    sqlx: Arc<SqlxRepo>,
    area_id: String,
    /// The caller's track.
    a: Side,
    /// Another track in the SAME area: same-area neighbours still cannot write each other's tags.
    b: Side,
}

fn session_id(card: &CardId) -> String {
    format!("session-{card}")
}

async fn side(sqlx: &SqlxRepo, cache: &CardRoleCache, area: &calm_server::ids::AreaId) -> Side {
    let track = sqlx
        .track_create(NewTrack {
            template_input: None,
            area_id: area.clone(),
            title: "tagged".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = |kind: &str, payload: Value| NewCard {
        track_id: track.id.clone(),
        title: None,
        kind: kind.into(),
        sort: None,
        payload,
    };
    let planner = sqlx
        .card_create(card("codex", json!({ "role": "planner" })))
        .await
        .unwrap();
    let worker = sqlx
        .card_create(card("codex", json!({ "task": "local" })))
        .await
        .unwrap();
    let report = sqlx
        .card_create(card(
            "track-report",
            serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        ))
        .await
        .unwrap();
    cache.insert(planner.id.clone(), CardRole::Planner, track.id.clone());
    cache.insert(worker.id.clone(), CardRole::Worker, track.id.clone());
    cache.insert(report.id.clone(), CardRole::ReportCard, track.id.clone());
    crate::support::mcp::set_persisted_card_role(sqlx, planner.id.as_str(), CardRole::Planner)
        .await;
    // The write gate resolves a planner actor through its live session row.
    let mut tx = sqlx.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session_id(&planner.id),
            card_id: planner.id.as_str().to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(format!("thread-{}", planner.id)),
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
        track_id: track.id,
        planner: planner.id,
        worker: worker.id,
        report: report.id,
    }
}

async fn boot() -> Boot {
    let sqlx = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = sqlx
        .area_create(NewArea {
            name: "report-tag".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let cache = CardRoleCache::new();
    let a = side(&sqlx, &cache, &area.id).await;
    let b = side(&sqlx, &cache, &area.id).await;
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
        std::env::temp_dir().join("neige-report-tag-gate-logs"),
    );
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    Boot {
        ctx,
        registry: Arc::new(registry),
        sqlx,
        area_id: area.id.as_str().to_string(),
        a,
        b,
    }
}

fn identity(boot: &Boot, side: &Side, role: CardRole) -> ToolCallIdentity {
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
        area_id: boot.area_id.clone(),
        thread_id: format!("thread-{card}"),
    }
}

async fn tag(boot: &Boot, who: ToolCallIdentity, args: Value) -> Result<Vec<String>, RpcError> {
    let handler = boot.registry.lookup(TOOL_REPORT_TAG).expect("registered");
    let value = handler(boot.ctx.clone(), who, args)
        .await
        .map(calm_server::mcp_server::result::ToolResult::into_structured)?;
    Ok(serde_json::from_value(value["tags"].clone()).expect("tags array"))
}

async fn stored(boot: &Boot, side: &Side) -> Vec<String> {
    sqlx::query_scalar("SELECT tag FROM report_tags WHERE track_id = ?1 ORDER BY ordinal")
        .bind(side.track_id.as_str())
        .fetch_all(boot.sqlx.pool())
        .await
        .unwrap()
}

async fn report_updated_at(boot: &Boot, side: &Side) -> i64 {
    sqlx::query_scalar("SELECT updated_at FROM cards WHERE id = ?1")
        .bind(side.report.as_str())
        .fetch_one(boot.sqlx.pool())
        .await
        .unwrap()
}

async fn backdate_report(boot: &Boot, side: &Side) {
    sqlx::query("UPDATE cards SET updated_at = 1 WHERE id = ?1")
        .bind(side.report.as_str())
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
}

fn strs(tags: &[&str]) -> Vec<String> {
    tags.iter().map(|t| t.to_string()).collect()
}

#[tokio::test]
async fn planner_tags_its_own_report_and_the_worker_reads_the_same_tags() {
    let boot = boot().await;
    let planner = || identity(&boot, &boot.a, CardRole::Planner);
    let worker = || identity(&boot, &boot.a, CardRole::Worker);
    assert!(
        tag(&boot, worker(), json!({ "path": "report.md" }))
            .await
            .unwrap()
            .is_empty(),
        "an untagged report lists empty"
    );

    backdate_report(&boot, &boot.a).await;
    let tags = tag(
        &boot,
        planner(),
        json!({ "path": "report.md", "add": ["认证", "架构"] }),
    )
    .await
    .unwrap();
    assert_eq!(tags, strs(&["认证", "架构"]));
    assert!(
        report_updated_at(&boot, &boot.a).await > 1,
        "a tag change bumps the report"
    );
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind = 'card.updated' AND scope_card = ?1",
    )
    .bind(boot.a.report.as_str())
    .fetch_one(boot.sqlx.pool())
    .await
    .unwrap();
    assert_eq!(
        events, 1,
        "the bump is announced as the report card's update"
    );

    let tags = tag(
        &boot,
        planner(),
        json!({ "path": "/report.md", "add": [" 排障 "] }),
    )
    .await
    .unwrap();
    assert_eq!(
        tags,
        strs(&["认证", "架构", "排障"]),
        "insertion order, trimmed"
    );
    assert_eq!(
        tag(&boot, worker(), json!({ "path": "report.md" }))
            .await
            .unwrap(),
        tags
    );

    let tags = tag(
        &boot,
        planner(),
        json!({ "path": "report.md", "remove": ["排障", "absent"] }),
    )
    .await
    .unwrap();
    assert_eq!(tags, strs(&["认证", "架构"]));
    assert_eq!(stored(&boot, &boot.a).await, tags);
}

#[tokio::test]
async fn adding_a_present_tag_is_idempotent() {
    let boot = boot().await;
    let planner = identity(&boot, &boot.a, CardRole::Planner);
    let args = json!({ "path": "report.md", "add": ["认证", "认证"] });
    assert_eq!(
        tag(&boot, planner.clone(), args.clone()).await.unwrap(),
        strs(&["认证"])
    );
    backdate_report(&boot, &boot.a).await;
    assert_eq!(tag(&boot, planner, args).await.unwrap(), strs(&["认证"]));
    assert_eq!(
        stored(&boot, &boot.a).await,
        strs(&["认证"]),
        "no duplicate row"
    );
    assert_eq!(
        report_updated_at(&boot, &boot.a).await,
        1,
        "a no-op leaves the report time"
    );
}

#[tokio::test]
async fn only_report_md_takes_tags() {
    let boot = boot().await;
    let other_report = format!("../{}/report.md", boot.b.track_id.as_str());
    for path in [
        "track.json",
        "index.md",
        "",
        "report.md/x",
        "area/reports/tagged.md",
        other_report.as_str(),
    ] {
        let err = tag(
            &boot,
            identity(&boot, &boot.a, CardRole::Planner),
            json!({ "path": path, "add": ["x"] }),
        )
        .await
        .expect_err(path);
        assert_eq!(err.code, INVALID_PARAMS, "{path}: {err:?}");
        assert!(err.message.contains("only `report.md`"), "{path}: {err:?}");
    }
    assert!(stored(&boot, &boot.a).await.is_empty());
    assert!(stored(&boot, &boot.b).await.is_empty());
}

#[tokio::test]
async fn a_caller_cannot_affect_another_tracks_tags() {
    let boot = boot().await;
    tag(
        &boot,
        identity(&boot, &boot.b, CardRole::Planner),
        json!({ "path": "report.md", "add": ["theirs"] }),
    )
    .await
    .unwrap();
    backdate_report(&boot, &boot.b).await;

    for role in [CardRole::Planner, CardRole::Worker] {
        let err = tag(
            &boot,
            identity(&boot, &boot.a, role),
            json!({ "path": "report.md", "track_id": boot.b.track_id.as_str(), "remove": ["theirs"] }),
        )
        .await
        .expect_err("a smuggled track id is refused");
        assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
        assert!(
            err.message.contains("unknown argument `track_id`"),
            "{err:?}"
        );
        assert!(
            tag(
                &boot,
                identity(&boot, &boot.a, role),
                json!({ "path": "report.md" })
            )
            .await
            .unwrap()
            .is_empty(),
            "{role:?} lists only its own track's tags"
        );
    }
    // The same tag name on the caller's own report touches only the caller's rows.
    let tags = tag(
        &boot,
        identity(&boot, &boot.a, CardRole::Planner),
        json!({ "path": "report.md", "add": ["theirs", "mine"], "remove": ["theirs"] }),
    )
    .await
    .unwrap();
    assert_eq!(tags, strs(&["mine"]));
    assert_eq!(stored(&boot, &boot.b).await, strs(&["theirs"]));
    assert_eq!(report_updated_at(&boot, &boot.b).await, 1);
}

#[tokio::test]
async fn invalid_tags_and_non_planner_writes_are_refused_before_any_write() {
    let boot = boot().await;
    let planner = || identity(&boot, &boot.a, CardRole::Planner);
    let too_many: Vec<String> = (0..33).map(|i| format!("t{i}")).collect();
    for (args, needle) in [
        (
            json!({ "path": "report.md", "add": ["ok", "a b"] }),
            "no whitespace",
        ),
        (
            json!({ "path": "report.md", "add": ["a,b"] }),
            "no whitespace",
        ),
        (
            json!({ "path": "report.md", "remove": ["  "] }),
            "must not be empty",
        ),
        (
            json!({ "path": "report.md", "add": "solo" }),
            "must be an array of strings",
        ),
        (
            json!({ "path": "report.md", "add": [1] }),
            "must be an array of strings",
        ),
        (
            json!({ "path": "report.md", "add": too_many }),
            "at most 32",
        ),
        (json!({ "add": ["x"] }), "missing `path`"),
    ] {
        let err = tag(&boot, planner(), args.clone())
            .await
            .expect_err("refused");
        assert_eq!(err.code, INVALID_PARAMS, "{args}: {err:?}");
        assert!(err.message.contains(needle), "{args}: {err:?}");
    }

    let err = tag(
        &boot,
        identity(&boot, &boot.a, CardRole::Worker),
        json!({ "path": "report.md", "add": ["x"] }),
    )
    .await
    .expect_err("a worker lists tags but does not change them");
    assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
    assert!(
        err.message.contains("only the Planner changes report tags"),
        "{err:?}"
    );
    let mut assistant = planner();
    assistant.role = CardRole::Assistant;
    tag(&boot, assistant, json!({ "path": "report.md" }))
        .await
        .expect_err("assistant is not a tagging role");
    assert!(stored(&boot, &boot.a).await.is_empty());
}
