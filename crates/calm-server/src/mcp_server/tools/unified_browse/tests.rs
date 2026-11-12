use super::*;
use crate::daily_planner;
use crate::daily_planner::tests::{at, fixture, foreign_track};
use crate::mcp_server::{ToolCallIdentity, result::ToolResult};
use crate::model::CardRole;
use axum::extract::FromRef;

async fn identity(
    state: &crate::state::RouteState,
    track: &crate::model::Track,
) -> ToolCallIdentity {
    let card = state
        .repo
        .cards_by_track(track.id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind == "codex")
        .unwrap();
    ToolCallIdentity {
        card_id: card.id.to_string(),
        role: CardRole::Planner,
        provider: calm_types::runtime::AgentProvider::Codex,
        session_id: "reader-test".into(),
        track_id: Some(track.id.to_string()),
        area_id: track.area_id.to_string(),
        thread_id: "card-bound".into(),
    }
}
async fn call(
    route: &crate::state::RouteState,
    id: &ToolCallIdentity,
    tool: &str,
    args: Value,
) -> Result<Value, RpcError> {
    crate::mcp_server::build_default_registry()
        .lookup(tool)
        .unwrap()(route.mcp_context.clone(), id.clone(), args)
    .await
    .map(ToolResult::into_structured)
}
fn decoded(value: Value) -> Value {
    serde_json::from_str(value["content"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn unified_browse_report_only_system_and_identity_fences() {
    let (_tmp, _repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let foreign = foreign_track(&route).await;
    let own = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &own).await;
    let root = format!("/areas/{}/tracks/{}/", foreign.area_id, foreign.id);
    let index = decoded(
        call(
            &route,
            &id,
            "neige_cat",
            json!({"path":format!("{root}report.md"),"detail":"index"}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(index["track_id"], foreign.id.as_str());
    assert_eq!(index["area_id"], foreign.area_id.as_str());
    for tail in [
        "conversation.md",
        "track.json",
        "cards/index.json",
        "runs/index.json",
        "ops/x/result",
        "../report.md",
    ] {
        let error = call(
            &route,
            &id,
            "neige_cat",
            json!({"path":format!("{root}{tail}")}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32602, "{tail}");
    }
    let error = call(
        &route,
        &id,
        "neige_cat",
        json!({"path":format!("/areas/{}/tracks/{}/report.md",own.area_id,own.id)}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32403);
    call(
        &route,
        &id,
        "neige_cat",
        json!({"path":"report.md","detail":"index"}),
    )
    .await
    .unwrap();
    call(&route, &id, "neige_ls", json!({})).await.unwrap();
    for (path, code) in [
        (
            format!("/areas/{}/tracks/{}/report.md", foreign.area_id, own.id),
            -32403,
        ),
        (
            format!("/areas/{}/tracks/missing/report.md", foreign.area_id),
            -32404,
        ),
        (
            format!("/areas/missing/tracks/{}/report.md", foreign.id),
            -32404,
        ),
        ("/home/private/report.md".into(), -32602),
    ] {
        assert_eq!(
            call(&route, &id, "neige_cat", json!({"path":path}))
                .await
                .unwrap_err()
                .code,
            code
        );
    }
    let mut wrong_area = id.clone();
    wrong_area.area_id = foreign.area_id.to_string();
    let mut wrong_track = id.clone();
    wrong_track.track_id = Some(foreign.id.to_string());
    let mut wrong_role = id.clone();
    wrong_role.role = CardRole::Worker;
    for forged in [wrong_area, wrong_track, wrong_role] {
        assert_eq!(
            call(
                &route,
                &forged,
                "neige_cat",
                json!({"path":format!("{root}report.md")})
            )
            .await
            .unwrap_err()
            .code,
            -32403
        );
    }
}

#[tokio::test]
async fn unified_browse_dates_page_and_keep_watermark() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let edited = foreign_track(&route).await;
    let start = crate::workspace_reports::day_window(
        crate::workspace_reports::parse_date("2026-10-03").unwrap(),
        crate::workspace_reports::TIME_ZONE,
    )
    .unwrap()
    .0;
    let mut ids = Vec::new();
    for offset in 0..25 {
        ids.push(
            crate::workspace_reports::tests::event(repo.db(), &edited, start + offset, "a", "b")
                .await,
        );
    }
    for offset in 25..46 {
        let track = foreign_track(&route).await;
        crate::workspace_reports::tests::event(repo.db(), &track, start + offset, "a", "b").await;
    }
    let own = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &own).await;
    let first = decoded(
        call(
            &route,
            &id,
            "neige_cat",
            json!({"path":"/report-changes/2026-10-03.json"}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(first["changes"].as_array().unwrap().len(), 20);
    assert_eq!(first["timezone"], "Asia/Shanghai");
    let through = first["through_event_id"].clone();
    crate::workspace_reports::tests::event(repo.db(), &edited, start + 100, "b", "late").await;
    let rest = decoded(call(&route,&id,"neige_cat",json!({"path":"/report-changes/2026-10-03.json","cursor":first["next_cursor"],"through_event_id":through})).await.unwrap());
    assert_eq!(rest["changes"].as_array().unwrap().len(), 2);
    assert_eq!(rest["next_cursor"], Value::Null);
    let path = format!(
        "/areas/{}/tracks/{}/report-edits/2026-10-03.json",
        edited.area_id, edited.id
    );
    assert!(
        call(&route, &id, "neige_cat", json!({"path":path}))
            .await
            .is_err()
    );
    let first = decoded(
        call(
            &route,
            &id,
            "neige_cat",
            json!({"path":path,"through_event_id":through}),
        )
        .await
        .unwrap(),
    );
    let rest = decoded(
        call(
            &route,
            &id,
            "neige_cat",
            json!({"path":path,"through_event_id":through,"cursor":first["next_cursor"]}),
        )
        .await
        .unwrap(),
    );
    let seen: Vec<i64> = first["edits"]
        .as_array()
        .unwrap()
        .iter()
        .chain(rest["edits"].as_array().unwrap())
        .map(|e| {
            assert!(e["edited_at"].as_str().is_some());
            e["event_id"].as_i64().unwrap()
        })
        .collect();
    assert_eq!(seen, ids);
    assert_eq!(rest["next_cursor"], Value::Null);
    for file in ["2026-10-3.json", "2026-02-30.json", "YYYY-MM-DD.json"] {
        assert!(
            call(
                &route,
                &id,
                "neige_cat",
                json!({"path":format!("/report-changes/{file}")})
            )
            .await
            .is_err()
        );
    }
}
#[tokio::test]
async fn report_planning_profile_refuses_worker_terminal_lifecycle_and_plugin_writes() {
    let (_tmp, _repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &track).await;
    for tool in [
        "neige_track_close",
        "neige_task_accept",
        "neige_task_reject",
        "neige_terminal_open",
        "external-plugin.mutate",
    ] {
        let result =
            crate::managed_track::require_tool_allowed(&route.mcp_context, &id, tool).await;
        assert_eq!(result.unwrap_err().code, -32403, "{tool}");
    }
    for tool in ["neige_report_read", "neige_report_commit", "neige_cat"] {
        assert!(
            crate::managed_track::require_tool_allowed(&route.mcp_context, &id, tool)
                .await
                .is_ok()
        );
    }
    let close = crate::mcp_server::build_default_registry()
        .lookup("neige_track_close")
        .unwrap();
    assert_eq!(
        close(route.mcp_context.clone(), id, json!({"message":"done"}))
            .await
            .unwrap_err()
            .code,
        -32403
    );
    let foreign = foreign_track(&route).await;
    assert!(
        crate::managed_track::require_tool_allowed(
            &route.mcp_context,
            &identity(&route, &foreign).await,
            "external-plugin.mutate"
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn report_planning_profile_cannot_spawn_workers_through_report_task_blocks() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let (card, payload) =
        super::super::track_report::load_report_for_track(&route.mcp_context, &track)
            .await
            .unwrap();
    let task = calm_types::report_blocks::render_fence(
        "task",
        &json!({"key":"work","kind":"codex","declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,"goal":"Run a worker"}),
    );
    let next =
        crate::track_report::TrackReportPayload::new("plan", format!("{}\n{task}\n", payload.body));
    let result = crate::track_report::write::persist_report(
        route.repo.as_ref(),
        &route.events,
        &route.write,
        crate::ids::ActorId::Kernel,
        calm_types::event::EditAuthor::Planner,
        track.clone(),
        card.clone(),
        payload.clone(),
        next,
        payload.doc_rev,
        None,
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cannot declare or modify worker tasks")
    );
    assert_eq!(
        route
            .repo
            .card_get(card.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .payload,
        card.payload
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tasks WHERE track_id=?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn closed_daily_reports_are_read_only_to_agents_but_allow_human_corrections() {
    let (_tmp, _repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = daily_planner::reconcile(&route, at("2026-10-03T01:00:00Z"))
        .await
        .unwrap();
    daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let (card, payload) =
        super::super::track_report::load_report_for_track(&route.mcp_context, &track)
            .await
            .unwrap();
    let next = crate::track_report::TrackReportPayload::new(
        "correction",
        format!("{}\nA corrected detail.\n", payload.body),
    );
    let write = |author, actor| {
        crate::track_report::write::persist_report(
            route.repo.as_ref(),
            &route.events,
            &route.write,
            actor,
            author,
            track.clone(),
            card.clone(),
            payload.clone(),
            next.clone(),
            payload.doc_rev,
            None,
        )
    };
    assert!(
        write(
            calm_types::event::EditAuthor::Planner,
            crate::ids::ActorId::Kernel
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("may only read")
    );
    write(
        calm_types::event::EditAuthor::User,
        crate::ids::ActorId::User,
    )
    .await
    .unwrap();
    assert!(
        route
            .repo
            .track_get(track.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_some()
    );
}

#[tokio::test]
async fn unified_browse_directories_and_hidden_report_remain_distinct() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let foreign = foreign_track(&route).await;
    let own = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &own).await;
    let root = call(&route, &id, "neige_ls", json!({"path":"/"}))
        .await
        .unwrap();
    assert_eq!(
        root["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["/areas/", "/report-changes/"]
    );
    repo.db()
        .writer()
        .execute(
            sqlx::query("UPDATE areas SET name=? WHERE id=?")
                .bind("界".repeat(20_000))
                .bind(foreign.area_id.as_str()),
        )
        .await
        .unwrap();
    let mut areas = Vec::new();
    let mut cursor = None::<String>;
    loop {
        let mut args = json!({"path":"/areas/"});
        if let Some(cursor) = &cursor {
            args["cursor"] = json!(cursor);
        }
        let page = call(&route, &id, "neige_ls", args).await.unwrap();
        let rows = page["entries"].as_array().unwrap();
        if let Some(row) = rows
            .iter()
            .find(|row| row["area_id"] == foreign.area_id.as_str())
        {
            assert_eq!(rows.len(), 1, "an oversized first row occupies its page");
            assert!(serde_json::to_vec(row).unwrap().len() > paging::PAGE_BYTES);
        }
        areas.extend(rows.iter().cloned());
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let ids: Vec<_> = areas
        .iter()
        .map(|a| a["area_id"].as_str().unwrap())
        .collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "each Area appears exactly once");
    let expected_areas = route.repo.areas_list_user_visible().await.unwrap();
    let mut expected_ids: Vec<_> = expected_areas.iter().map(|a| a.id.as_str()).collect();
    expected_ids.sort();
    assert_eq!(
        ids, expected_ids,
        "paging retains all visible Areas in ID order"
    );
    assert!(ids.contains(&foreign.area_id.as_str()));
    assert!(!ids.contains(&own.area_id.as_str()));
    assert!(areas.iter().all(|a| a.get("default_cwd").is_none()));
    let path = format!("/areas/{}/tracks/", foreign.area_id);
    let tracks = call(&route, &id, "neige_ls", json!({"path":path}))
        .await
        .unwrap();
    assert_eq!(tracks["entries"][0]["track_id"], foreign.id.as_str());
    assert!(
        call(
            &route,
            &id,
            "neige_ls",
            json!({"path":path,"cursor":format!("/areas/{}/",foreign.area_id)})
        )
        .await
        .is_err()
    );
    let report_dir = format!("{path}{}/", foreign.id);
    let dir = call(&route, &id, "neige_ls", json!({"path":report_dir}))
        .await
        .unwrap();
    assert_eq!(dir["entries"].as_array().unwrap().len(), 2);
    for path in [
        "/report-changes/".to_string(),
        format!("{report_dir}report-edits/"),
    ] {
        let dir = call(&route, &id, "neige_ls", json!({"path":path}))
            .await
            .unwrap();
        assert_eq!(dir["entries"], json!([]));
        assert_eq!(dir["date_pattern"], "YYYY-MM-DD.json");
    }
    repo.db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET purpose=? WHERE id=?")
                .bind(crate::AREA_CHAT_PURPOSE)
                .bind(foreign.id.as_str()),
        )
        .await
        .unwrap();
    let hidden = call(&route, &id, "neige_ls", json!({"path":path}))
        .await
        .unwrap();
    assert_eq!(hidden["entries"], json!([]));
    assert_eq!(
        call(
            &route,
            &id,
            "neige_ls",
            json!({"path":path,"cursor":tracks["entries"][0]["path"]})
        )
        .await
        .unwrap_err()
        .code,
        -32602
    );
    call(
        &route,
        &id,
        "neige_cat",
        json!({"path":format!("{report_dir}report.md")}),
    )
    .await
    .unwrap();
}
