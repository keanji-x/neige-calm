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
        "neige_task_verdict",
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
