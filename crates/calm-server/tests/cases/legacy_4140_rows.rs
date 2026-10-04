//! #1893 §3.3: the rows the 4140 database keeps after their code is deleted load as inert history.
//! Each slice that deletes a mechanism tightens the expectations below.
use crate::mcp_track_report::{Boot, boot, call_tool, seed_track_root_session};
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::model::CardRole;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::session_projection_repo::WorkerSessionKind;
use calm_types::worker::WorkerSessionId;
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../fixtures/legacy_4140_rows.sql");
const TRACKS: [(&str, &str); 3] = [
    ("legacy-a", "dispatch-a"),
    ("legacy-b", "dispatch-b"),
    ("legacy-c", "dispatch-c"),
];

async fn legacy_boot() -> Boot {
    let boot = boot().await;
    let fixture = FIXTURE
        .replace(
            "{planner_author}",
            calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
        )
        .replace(
            "{in_track_route}",
            calm_types::task_recovery::TASK_IN_TRACK_ROUTE,
        );
    sqlx::raw_sql(&fixture)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .expect("the fixture applies after every migration");
    boot.repo
        .seed_card_role_cache(&boot.card_role_cache)
        .await
        .unwrap();
    // Today's Planner reading each closed track; the fixture holds no live session.
    for (track, _) in TRACKS {
        let planner = planner(track);
        seed_track_root_session(
            boot.repo.as_ref(),
            &track.to_string().into(),
            &planner.card_id.into(),
            &planner.session_id,
        )
        .await;
    }
    boot
}

fn planner(track: &str) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: format!("planner-{}", &track["legacy-".len()..]),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: format!("planner-session-{}", &track["legacy-".len()..]),
        track_id: Some(track.into()),
        area_id: "legacy-area".into(),
        thread_id: "legacy-planner-thread".into(),
    }
}

async fn attempts(boot: &Boot, track: &str, key: &str) -> Value {
    use axum::{Extension, body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let response = calm_server::routes::task_recovery::router()
        .with_state(crate::task_projection_acceptance::route_state(boot).await)
        .layer(Extension(crate::task_projection_acceptance::principal()))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .oneshot(
            Request::builder()
                .uri(format!("/api/tracks/{track}/tasks/{key}/attempts"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn legacy_4140_rows_load() {
    let boot = legacy_boot().await;
    let pool = boot.repo.sqlite_pool().unwrap();

    // Events: replay skips the kinds whose code is gone and keeps every live neighbour.
    let all: Vec<(i64, String)> = sqlx::query_as("SELECT id, kind FROM events ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let retired = [
        "task.file_publication_settled",
        "task.candidate_verification_settled",
        "task.execution_settled",
    ];
    assert_eq!(
        all.iter()
            .filter(|(_, kind)| retired.contains(&kind.as_str()))
            .count(),
        8,
        "anti-vacuity: the fixture holds the retired rows"
    );
    let live: Vec<i64> = all
        .iter()
        .filter(|(_, kind)| !retired.contains(&kind.as_str()))
        .map(|(id, _)| *id)
        .collect();
    let replayed = boot
        .repo
        .events_since(0, i64::MAX)
        .await
        .expect("replay over retired kinds is Ok");
    assert_eq!(
        replayed.iter().map(|(id, ..)| *id).collect::<Vec<_>>(),
        live
    );

    // Operations: every leftover row is terminal, so neither the driver nor boot recovery loads it.
    let state = crate::task_projection_acceptance::route_state(&boot).await;
    let plan = state
        .operation_runtime
        .recover_on_boot()
        .await
        .expect("boot recovery over retired kinds is Ok");
    assert!(plan.items.is_empty(), "{:?}", plan.items);
    state
        .operation_runtime
        .drive()
        .await
        .expect("drive over retired kinds is Ok");
    let phases: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, phase FROM operations ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(phases.len(), 11);
    assert!(
        phases
            .iter()
            .all(|(_, phase)| phase == "succeeded" || phase == "failed"),
        "{phases:?}"
    );

    // The recovery binding tables are gone with their code (S3); their rows were history.
    let bindings: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE name LIKE 'planner_recovery_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bindings, 0);
    // So is the Planner dispatch receipt table (S4).
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE name = 'planner_dispatch_receipts'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(receipts, 0);

    // Worker sessions and their cards.
    for (session, card) in [
        ("ws-op-a1", "worker-a1"),
        ("ws-op-a2", "worker-a2"),
        ("ws-op-b", "worker-b"),
        ("ws-op-c", "worker-c"),
    ] {
        let loaded = boot
            .repo
            .session_get_by_id(&WorkerSessionId::from(session))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("session {session}"));
        assert_eq!(loaded.card_id.as_ref().map(|id| id.as_str()), Some(card));
        assert!(boot.repo.card_get(card).await.unwrap().is_some(), "{card}");
    }
    // They carry a thread, a turn and a token like the 4140 rows, yet they are exited: boot thread
    // attribution and worker-flow boot selection pass over them.
    let legacy_thread = |thread: &str| thread.starts_with("legacy-thread-");
    let attributed =
        calm_server::session_projection_lookup::merge_active_shared_thread_attribution(
            boot.repo.as_ref(),
        )
        .await
        .unwrap();
    assert!(
        !attributed.values().any(|thread| legacy_thread(thread)),
        "{attributed:?}"
    );
    for kind in [WorkerSessionKind::CodexCard, WorkerSessionKind::ClaudeCard] {
        let selected = boot
            .repo
            .session_projection_active_for_kind(kind)
            .await
            .unwrap();
        assert!(
            !selected
                .iter()
                .any(|runtime| runtime.id.starts_with("ws-op-")),
            "{selected:?}"
        );
    }

    for (track, key) in TRACKS {
        // The Planner's plan reads return every current entry.
        let full = call_tool(&boot, "neige_plan_list", planner(track), json!({}))
            .await
            .unwrap_or_else(|error| panic!("{track}: plan.list full: {error:?}"));
        let keys: Vec<&str> = full["tasks"]
            .as_array()
            .unwrap_or_else(|| panic!("{track}: {full}"))
            .iter()
            .filter_map(|entry| entry["key"].as_str())
            .collect();
        assert_eq!(keys, [key], "{track}: {full}");
        // The failure shows as the failure only: nothing can be recovered, and no isolated
        // activity is read (S4).
        let entry = &full["tasks"][0];
        assert_eq!(entry["status"], "failed", "{track}: {full}");
        assert!(entry["status_detail"].is_string(), "{track}: {full}");
        for gone in ["recovery", "activity"] {
            assert!(entry.get(gone).is_none(), "{track}: {gone}: {full}");
        }
        let summary = call_tool(
            &boot,
            "neige_plan_list",
            planner(track),
            json!({"detail":"summary","key":key}),
        )
        .await
        .unwrap_or_else(|error| panic!("{track}: plan.list summary: {error:?}"));
        assert_eq!(summary["tasks"][0]["key"], key, "{track}: {summary}");

        // The attempt history keeps the recovery allocation, and says nothing about recovering.
        let history = attempts(&boot, track, key).await;
        let mut fields: Vec<&str> = history
            .as_object()
            .unwrap_or_else(|| panic!("{track}: {history}"))
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(fields, ["attempts", "current", "key"], "{track}: {history}");
        let generations = history["attempts"].as_array().unwrap().len();
        assert_eq!(generations, if track == "legacy-a" { 2 } else { 1 });

        // The report and its task block read.
        let report = call_tool(&boot, "neige_report_read", planner(track), json!({}))
            .await
            .unwrap_or_else(|error| panic!("{track}: report.read: {error:?}"));
        assert!(report.to_string().contains(key), "{track}: {report}");
        // Its isolated block is kept as history and never scheduled (S4).
        let retired: Vec<&Value> = report["taskDiagnostics"]
            .as_array()
            .unwrap_or_else(|| panic!("{track}: {report}"))
            .iter()
            .filter(|verdict| verdict["key"] == key)
            .flat_map(|verdict| verdict["diagnostics"].as_array().unwrap())
            .filter(|diagnostic| diagnostic["code"] == "neige_execution_retired")
            .collect();
        assert_eq!(retired.len(), 1, "{track}: {report}");

        // Track activity recomputes.
        let projector = calm_server::track_activity::TrackActivityProjector::new(
            boot.repo.clone(),
            state.events.clone(),
            state.write().clone(),
            state.harness.clone(),
            state.terminal_renderer.clone(),
        )
        .unwrap();
        projector
            .recompute_track(track)
            .await
            .unwrap_or_else(|error| panic!("{track}: activity: {error:?}"));
    }
}
