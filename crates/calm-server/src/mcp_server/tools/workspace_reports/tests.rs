use super::*;
use crate::daily_planner;
use crate::daily_planner::tests::{at, fixture, foreign_track};
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
        .find(|card| card.kind == "codex")
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

#[tokio::test]
async fn granted_planner_reads_foreign_reports_without_write_authority() {
    let (_tmp, _repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = foreign_track(&route).await;
    let next = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    assert_ne!(track.area_id, next.area_id);
    let id = identity(&route, &next).await;
    let state_tool = crate::mcp_server::build_default_registry()
        .lookup("neige_track_status")
        .unwrap();
    let state = state_tool(route.mcp_context.clone(), id.clone(), json!({}))
        .await
        .unwrap()
        .into_structured();
    assert_eq!(state["creation_identity"]["identity"], "2026-10-04");
    // The stored `report_time_zone` is served as `timezone` (§4).
    assert_eq!(
        state["creation_identity"],
        json!({"owner": "daily-planner", "identity": "2026-10-04", "timezone": "Asia/Shanghai"})
    );
    let registry = crate::mcp_server::build_default_registry();
    let read = registry.lookup("neige_workspace_cat").unwrap();
    let result = read(
        route.mcp_context.clone(),
        id.clone(),
        json!({"track_id":track.id}),
    )
    .await
    .unwrap()
    .into_structured();
    assert_eq!(result["track_id"], track.id.as_str());
    assert!(result["body"].as_str().unwrap().contains("概要"));
    let list = registry.lookup("neige_workspace_ls").unwrap();
    assert_eq!(
        list(route.mcp_context.clone(), id.clone(), json!({}))
            .await
            .unwrap()
            .into_structured()["reports"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // The ordinary write tool still resolves only the bound Track and requires its own marker read.
    let write = registry.lookup("neige_report_write").unwrap();
    let result = write(
        route.mcp_context.clone(),
        id,
        json!({"message":"attempt","body":result["body"]}),
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .message
            .contains("has not read the whole report")
    );
    let foreign_report = route
        .repo
        .cards_by_track(track.id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
        .unwrap();
    assert!(
        route
            .mcp_context
            .read_ledger
            .last_read("reader-test", foreign_report.id.as_str())
            .is_none()
    );
}

#[tokio::test]
async fn ungranted_planner_cannot_read_workspace_reports() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &track).await;
    sqlx::query("UPDATE managed_track_identities SET report_read_scope='area' WHERE track_id=?1")
        .bind(track.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    for (tool, args) in [
        ("neige_workspace_ls", json!({})),
        ("neige_workspace_cat", json!({"track_id":track.id})),
        ("neige_workspace_diff", json!({"date":"2026-10-03"})),
        (
            "neige_workspace_log",
            json!({"date":"2026-10-03","track_id":track.id,"through_event_id":0}),
        ),
    ] {
        let handler = crate::mcp_server::build_default_registry()
            .lookup(tool)
            .unwrap();
        let error = handler(route.mcp_context.clone(), id.clone(), args)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32403, "{tool}");
    }
}

#[tokio::test]
async fn workers_assistants_and_forged_bindings_are_refused() {
    let (_tmp, _repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &track).await;
    for role in [CardRole::Worker, CardRole::Assistant, CardRole::ReportCard] {
        assert!(
            dispatch(
                route.mcp_context.clone(),
                ToolCallIdentity { role, ..id.clone() },
                json!({}),
                "neige_workspace_ls"
            )
            .await
            .is_err()
        );
    }
    assert!(
        dispatch(
            route.mcp_context.clone(),
            ToolCallIdentity {
                track_id: Some("wrong-track".into()),
                ..id
            },
            json!({}),
            "neige_workspace_ls"
        )
        .await
        .is_err()
    );
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
    for tool in [
        "neige_report_read",
        "neige_report_commit",
        "neige_workspace_diff",
    ] {
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

async fn call(
    route: &crate::state::RouteState,
    id: &ToolCallIdentity,
    tool: &str,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = crate::mcp_server::build_default_registry()
        .lookup(tool)
        .unwrap();
    handler(route.mcp_context.clone(), id.clone(), args)
        .await
        .map(ToolResult::into_structured)
}

/// #2087 B2: every workspace list pages with `cursor` / `next_cursor`, an opaque string, and
/// carries `next_cursor: null` on its last page; the edits cursor is the event id in decimal.
#[tokio::test]
async fn workspace_lists_page_with_a_string_cursor_through_the_tools() {
    let (_tmp, repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let (start, _) = crate::workspace_reports::day_window(
        crate::workspace_reports::parse_date("2026-10-03").unwrap(),
        crate::daily_planner::TIME_ZONE,
    )
    .unwrap();
    let edited = foreign_track(&route).await;
    let mut ids = Vec::new();
    for offset in 0..25 {
        ids.push(
            crate::workspace_reports::tests::event(
                repo.pool(),
                &edited,
                start + offset,
                "old",
                "new",
            )
            .await,
        );
    }
    for offset in 25..46 {
        let track = foreign_track(&route).await;
        crate::workspace_reports::tests::event(repo.pool(), &track, start + offset, "a", "b").await;
    }
    let planner = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &planner).await;

    let diff = call(
        &route,
        &id,
        "neige_workspace_diff",
        json!({"date":"2026-10-03"}),
    )
    .await
    .unwrap();
    assert_eq!(diff["timezone"], "Asia/Shanghai");
    assert!(diff.get("time_zone").is_none(), "{diff}");
    assert_eq!(diff["changes"].as_array().unwrap().len(), 20);
    let cursor = diff["next_cursor"].as_str().expect("a string cursor");
    let through = diff["through_event_id"].clone();
    let rest = call(
        &route,
        &id,
        "neige_workspace_diff",
        json!({"date":"2026-10-03","cursor":cursor,"through_event_id":through}),
    )
    .await
    .unwrap();
    assert_eq!(rest["changes"].as_array().unwrap().len(), 2);
    assert_eq!(rest["next_cursor"], Value::Null, "the last page says so");

    let args = |cursor: Option<&str>| {
        let mut args = json!({"date":"2026-10-03","track_id":edited.id,"through_event_id":through});
        if let Some(cursor) = cursor {
            args["cursor"] = json!(cursor);
        }
        args
    };
    let first = call(&route, &id, "neige_workspace_log", args(None))
        .await
        .unwrap();
    assert_eq!(first["edits"].as_array().unwrap().len(), 20);
    assert_eq!(first["next_cursor"], json!(ids[19].to_string()));
    let second = call(
        &route,
        &id,
        "neige_workspace_log",
        args(first["next_cursor"].as_str()),
    )
    .await
    .unwrap();
    let seen: Vec<i64> = first["edits"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["edits"].as_array().unwrap())
        .map(|entry| entry["event_id"].as_i64().unwrap())
        .collect();
    assert_eq!(seen, ids);
    assert_eq!(second["next_cursor"], Value::Null);
    for bad in ["abc", "-1", "+5", "1.5", ""] {
        let error = call(&route, &id, "neige_workspace_log", args(Some(bad)))
            .await
            .unwrap_err();
        assert_eq!(error.code, RpcError::INVALID_PARAMS, "{bad}");
        assert!(
            error.message.contains(&format!("cursor `{bad}`")),
            "{bad}: {}",
            error.message
        );
    }

    let list = call(&route, &id, "neige_workspace_ls", json!({}))
        .await
        .unwrap();
    assert_eq!(list["timezone"], "Asia/Shanghai");
    let cursor = list["next_cursor"].as_str().expect("22 reports page");
    let rest = call(&route, &id, "neige_workspace_ls", json!({"cursor":cursor}))
        .await
        .unwrap();
    assert_eq!(
        list["reports"].as_array().unwrap().len() + rest["reports"].as_array().unwrap().len(),
        22
    );
    assert_eq!(rest["next_cursor"], Value::Null);
}

/// #2087 B2: the retired `after` is refused with the tool's valid keys, never ignored.
#[tokio::test]
async fn workspace_tools_refuse_the_retired_after_with_their_valid_keys() {
    let (_tmp, _repo, state) = fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let track = daily_planner::reconcile(&route, at("2026-10-04T01:00:00Z"))
        .await
        .unwrap();
    let id = identity(&route, &track).await;
    for (tool, args, valid) in [
        ("neige_workspace_ls", json!({"after":"t"}), "valid: cursor"),
        (
            "neige_workspace_diff",
            json!({"date":"2026-10-03","after":"t"}),
            "valid: cursor, date, through_event_id",
        ),
        (
            "neige_workspace_log",
            json!({"date":"2026-10-03","track_id":track.id,"through_event_id":0,"after":0}),
            "valid: cursor, date, through_event_id, track_id",
        ),
    ] {
        let error = call(&route, &id, tool, args).await.unwrap_err();
        assert_eq!(error.code, RpcError::INVALID_PARAMS, "{tool}");
        assert_eq!(
            error.message,
            format!("{tool}: unknown argument `after`; {valid}"),
            "{tool}"
        );
    }
}
