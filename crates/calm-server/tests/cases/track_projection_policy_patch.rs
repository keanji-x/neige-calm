use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, task_claim_pending_tx};
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::NewCard;
use calm_server::model::{NewArea, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use calm_server::track_report::{TrackReportPayload, persist_report};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn boot() -> (AppState, String, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "policy".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "policy".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-policy-patch-plugins"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    (state, track.id.to_string(), repo)
}

fn app(state: AppState) -> axum::Router {
    routes::tracks::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn patch(
    state: AppState,
    track_id: &str,
    actor: Option<&str>,
    body: Value,
) -> axum::http::Response<Body> {
    let mut request = Request::builder()
        .method("PATCH")
        .uri(format!("/api/tracks/{track_id}"))
        .header("content-type", "application/json");
    if let Some(actor) = actor {
        request = request.header("X-Calm-Actor", actor);
    }
    app(state)
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn columns(repo: &Arc<dyn Repo>, track_id: &str) -> (Option<i64>, Option<String>) {
    sqlx::query_as("SELECT spec_task_ceiling, automation_policy FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

async fn event_count(repo: &Arc<dyn Repo>) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

const TASK_PERSISTENT_COLUMNS: &[&str] = &[
    "id",
    "track_id",
    "key",
    "kind",
    "goal",
    "context_json",
    "acceptance_criteria",
    "cwd",
    "depends_on_json",
    "priority",
    "gate_json",
    "status",
    "status_detail",
    "worker_card_id",
    "gate_result_json",
    "gate_attempt",
    "gate_pid",
    "gate_pid_starttime",
    "gate_pid_boot_id",
    "running_deadline_ms",
    "created_at_ms",
    "updated_at_ms",
    "finished_at_ms",
    "claim_context_json",
    "context_stale_at_ms",
    "context_closure_truncated",
    "declared_by",
    "decl_ready",
    "decl_released_by_user",
    "context_verify_failures",
    "spawn",
    "child_track_id",
];

const TRACK_PERSISTENT_COLUMNS: &[&str] = &[
    "id",
    "area_id",
    "title",
    "sort",
    "archived_at",
    "created_at",
    "updated_at",
    "lifecycle",
    "terminal_at",
    "pinned_at",
    "task_budget",
    "require_task_gates",
    "root_session_id",
    "template_id",
    "template_input",
    "purpose",
    "spec_task_ceiling",
    "automation_policy",
    "parent_track_id",
    "tree_task_budget",
    "plugin_scope",
    // #1147 S1 — migration 0077 replaced `cwd` (dropped, see above) with the
    // typed workspace.
    "workspace_kind",
    "workspace_path",
    "workspace_frozen_at",
];

type PersistedTrackEvent = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

async fn persistent_row_snapshot(
    repo: &Arc<dyn Repo>,
    table: &str,
    expected_columns: &[&str],
    id: &str,
) -> String {
    let actual_columns: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
    ))
    .fetch_all(&repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(
        actual_columns, expected_columns,
        "{table} persistent-row snapshot column list drifted from the HEAD schema"
    );

    sqlx::query_scalar(&format!(
        "SELECT json_array({}) FROM {table} WHERE id=?1",
        expected_columns.join(",")
    ))
    .bind(id)
    .fetch_one(&repo.sqlite_pool().unwrap())
    .await
    .unwrap()
}

async fn task_row_snapshot(repo: &Arc<dyn Repo>, task_id: &str) -> String {
    persistent_row_snapshot(repo, "tasks", TASK_PERSISTENT_COLUMNS, task_id).await
}

async fn track_policy_snapshots(repo: &Arc<dyn Repo>, track_ids: &[&str]) -> Vec<String> {
    let mut snapshots = Vec::with_capacity(track_ids.len());
    for track_id in track_ids {
        snapshots.push(
            persistent_row_snapshot(repo, "tracks", TRACK_PERSISTENT_COLUMNS, track_id).await,
        );
    }
    snapshots
}

fn assert_track_columns_unchanged_except(before: &str, after: &str, except: &[&str]) {
    let before: Vec<Value> = serde_json::from_str(before).unwrap();
    let after: Vec<Value> = serde_json::from_str(after).unwrap();
    assert_eq!(before.len(), TRACK_PERSISTENT_COLUMNS.len());
    assert_eq!(after.len(), TRACK_PERSISTENT_COLUMNS.len());
    for (index, column) in TRACK_PERSISTENT_COLUMNS.iter().enumerate() {
        if !except.contains(column) {
            assert_eq!(
                after[index], before[index],
                "successful ceiling PATCH unexpectedly changed tracks.{column}"
            );
        }
    }
}

async fn event_cursor(repo: &Arc<dyn Repo>) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM events")
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

fn report_task_block(key: &str) -> calm_types::track_report::ReportBlock {
    calm_types::track_report::ReportBlock {
        id: format!("b_{key}"),
        rev: 1,
        kind: "task".into(),
        payload: json!({
            "key": key,
            "kind": "codex",
            "goal": format!("{key} goal"),
            "acceptance": "done",
            "no_gate_reason": "not needed",
            "declared_by": "spec",
            "ready": true
        }),
    }
}

async fn seed_pending_report_task(repo: &Arc<dyn Repo>, track_id: &str, key: &str) -> String {
    let existing: Option<(String, String)> =
        sqlx::query_as("SELECT id,payload FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(track_id)
            .fetch_optional(&repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    let (report_id, mut payload) = if let Some((report_id, payload)) = existing {
        (
            report_id,
            serde_json::from_str::<TrackReportPayload>(&payload).unwrap(),
        )
    } else {
        let report = repo
            .card_create(NewCard {
                track_id: track_id.to_owned().into(),
                title: None,
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
            })
            .await
            .unwrap();
        (report.id.to_string(), TrackReportPayload::new("", ""))
    };
    let blocks = payload.blocks.get_or_insert_default();
    blocks.push(report_task_block(key));
    payload.body = blocks
        .iter()
        .map(calm_types::report_blocks::flat_text)
        .collect::<Vec<_>>()
        .join("\n");
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(&report_id)
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    let task_id = format!("{track_id}:{key}");
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'codex',?4,'{}','[]',0,'pending','spec',0,0)")
        .bind(&task_id)
        .bind(track_id)
        .bind(key)
        .bind(format!("{key} goal"))
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    task_id
}

async fn write_report_task(
    state: &AppState,
    repo: &Arc<dyn Repo>,
    track_id: &str,
    key: &str,
) -> String {
    let track = repo.track_get(track_id).await.unwrap().unwrap();
    let report = if let Some(report) = repo
        .cards_by_track(track_id)
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "track-report")
    {
        report
    } else {
        repo.card_create(NewCard {
            track_id: track_id.to_owned().into(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap()
    };
    let current: TrackReportPayload = serde_json::from_value(report.payload.clone()).unwrap();
    let mut blocks = current.blocks.clone().unwrap_or_default();
    blocks.push(report_task_block(key));
    let body = blocks
        .iter()
        .map(calm_types::report_blocks::flat_text)
        .collect::<Vec<_>>()
        .join("\n");
    let next = TrackReportPayload::new(current.summary.clone(), body);
    let if_doc_rev = current.doc_rev;
    persist_report(
        state.repo.as_ref(),
        &state.events,
        state.write(),
        ActorId::Kernel,
        EditAuthor::Spec,
        track,
        report,
        current,
        next,
        if_doc_rev,
        None,
        None,
        false,
    )
    .await
    .expect("production report write commits task declaration");

    format!("{track_id}:{key}")
}

#[tokio::test]
async fn policy_only_patch_is_not_short_circuited_and_clear_nulls_work() {
    let (state, track_id, repo) = boot().await;
    assert_eq!(columns(&repo, &track_id).await, (Some(32), None));
    let before = event_count(&repo).await;

    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"automation_policy": "declare-and-wait"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        columns(&repo, &track_id).await,
        (Some(32), Some("declare-and-wait".into()))
    );
    assert_eq!(event_count(&repo).await, before + 1);

    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"spec_task_ceiling": 7}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &track_id).await.0, Some(7));

    let response = patch(
        state,
        &track_id,
        None,
        json!({"automation_policy": null, "spec_task_ceiling": null}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &track_id).await, (None, None));
}

#[tokio::test]
async fn non_user_policy_patches_are_forbidden_without_rows_or_events() {
    let (state, track_id, repo) = boot().await;
    let original = columns(&repo, &track_id).await;
    let before = event_count(&repo).await;

    for body in [
        json!({"automation_policy": "auto-declare"}),
        json!({"spec_task_ceiling": 99}),
    ] {
        let response = patch(state.clone(), &track_id, Some("ai:codex"), body).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(columns(&repo, &track_id).await, original);
        assert_eq!(event_count(&repo).await, before);
    }
}

#[tokio::test]
async fn invalid_policy_values_are_rejected() {
    let (state, track_id, repo) = boot().await;
    for body in [
        json!({"automation_policy": "unsafe"}),
        json!({"spec_task_ceiling": -1}),
    ] {
        let response = patch(state.clone(), &track_id, None, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(columns(&repo, &track_id).await, (Some(32), None));
}

async fn tree_budget(repo: &Arc<dyn Repo>, track_id: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT tree_task_budget FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

/// #985 slice 6 PR-B — `tree_task_budget` reaches the column through the SAME
/// production PATCH surface as `spec_task_ceiling`, with the same four
/// behaviors (write, present-null reset, non-user 403, negative 400) plus the
/// root-only rule. Without a write surface the column would be permanently
/// pinned at the kernel default and every test would have to poke it with raw
/// SQL — a fixture bypassing the production route.
#[tokio::test]
async fn tree_task_budget_patch_matches_the_spec_task_ceiling_surface() {
    let (state, track_id, repo) = boot().await;
    assert_eq!(tree_budget(&repo, &track_id).await, None);
    let before = event_count(&repo).await;

    // Non-user actors are refused, with no row and no event. The REASON is
    // asserted, not just the status: an `ai:codex` REST request is also
    // refused further downstream (its legacy header path carries an empty
    // card id), so a bare 403 assertion here would stay green with the
    // user-only gate deleted.
    let response = patch(
        state.clone(),
        &track_id,
        Some("ai:codex"),
        json!({"tree_task_budget": 8}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("tree_task_budget") && body.contains("user-only"),
        "403 must come from the user-only policy gate, got {body}"
    );
    assert_eq!(tree_budget(&repo, &track_id).await, None);
    assert_eq!(event_count(&repo).await, before);

    // Negative values are refused.
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"tree_task_budget": -1}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(tree_budget(&repo, &track_id).await, None);

    // The whole-tree rebuild runs in the write transaction. Its configured
    // budget (and therefore N, because member admission requires N<=B) has a
    // fixed production ceiling rather than allowing an unbounded writer hold.
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"tree_task_budget": 65}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(tree_budget(&repo, &track_id).await, None);

    // A budget-only patch is NOT short-circuited as an empty patch.
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"tree_task_budget": 8}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(tree_budget(&repo, &track_id).await, Some(8));
    assert_eq!(event_count(&repo).await, before + 1);

    // Present-null resets to the kernel default.
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"tree_task_budget": null}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(tree_budget(&repo, &track_id).await, None);

    // Root-only: a child track refuses the patch and keeps its NULL.
    let child = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: repo
                .track_get(&track_id)
                .await
                .unwrap()
                .unwrap()
                .area_id
                .clone(),
            title: "child".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let child_id = child.id.to_string();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(&track_id)
        .bind(&child_id)
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let response = patch(state, &child_id, None, json!({"tree_task_budget": 4})).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(tree_budget(&repo, &child_id).await, None);
}

#[tokio::test]
async fn tightening_policy_immediately_deletes_pending_projection_and_emits_plan_updated() {
    let (state, track_id, repo) = boot().await;
    let block = calm_types::track_report::ReportBlock {
        id: "b_policy".into(),
        rev: 1,
        kind: "task".into(),
        payload: json!({"key":"queued","kind":"codex","goal":"queued goal",
            "acceptance":"done","no_gate_reason":"not needed","declared_by":"spec","ready":true}),
    };
    let mut payload = calm_server::track_report::TrackReportPayload::new(
        "",
        calm_types::report_blocks::flat_text(&block),
    );
    payload.blocks = Some(vec![block]);
    let report = repo
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(report.id.as_str())
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'queued','codex','queued goal','{}','[]',0,'pending','spec',0,0)")
        .bind(format!("{track_id}:queued")).bind(&track_id).execute(&repo.sqlite_pool().unwrap()).await.unwrap();

    let response = patch(
        state,
        &track_id,
        None,
        json!({"automation_policy":"declare-and-wait"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1")
        .bind(&track_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let plan_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='plan.updated'")
            .fetch_one(&repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(plan_events, 1);
}

/// `tree_task_budget` is an input to schedulability even for an N=1 root when
/// explicitly configured. Tightening it must use the same in-transaction
/// reprojection seam as the other policy fields.
#[tokio::test]
async fn tightening_tree_budget_immediately_deletes_pending_projection_and_emits_plan_updated() {
    let (state, track_id, repo) = boot().await;
    let block = calm_types::track_report::ReportBlock {
        id: "b_tree_budget".into(),
        rev: 1,
        kind: "task".into(),
        payload: json!({"key":"queued","kind":"codex","goal":"queued goal",
            "acceptance":"done","no_gate_reason":"not needed","declared_by":"spec","ready":true}),
    };
    let mut payload = calm_server::track_report::TrackReportPayload::new(
        "",
        calm_types::report_blocks::flat_text(&block),
    );
    payload.blocks = Some(vec![block]);
    let report = repo
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(report.id.as_str())
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'queued','codex','queued goal','{}','[]',0,'pending','spec',0,0)")
        .bind(format!("{track_id}:queued")).bind(&track_id).execute(&repo.sqlite_pool().unwrap()).await.unwrap();

    let response = patch(state, &track_id, None, json!({"tree_task_budget": 0})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE track_id=?1")
        .bind(&track_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let plan_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='plan.updated'")
            .fetch_one(&repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(plan_events, 1);
}

/// The root budget determines every descendant's share. The PATCH transaction
/// must therefore invalidate descendant projections too; otherwise this old
/// pending row remains directly claimable without any later child edit.
#[tokio::test]
async fn tightening_root_tree_budget_culls_descendant_pending_before_it_can_be_claimed() {
    let (state, root_id, repo) = boot().await;
    let root = repo.track_get(&root_id).await.unwrap().unwrap();
    let child = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: root.area_id,
            title: "child".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(&root_id)
        .bind(child.id.as_str())
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let task_id = seed_pending_report_task(&repo, child.id.as_str(), "child-queued").await;

    let response = patch(state, &root_id, None, json!({"tree_task_budget": 0})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE id=?1")
        .bind(&task_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(pending, 0, "the descendant's stale projection survived");

    let mut tx = repo.sqlite_pool().unwrap().begin().await.unwrap();
    assert_eq!(
        task_claim_pending_tx(&mut tx, &task_id, 1, &[], false)
            .await
            .unwrap(),
        0,
        "a task admitted under the old descendant share remained claimable"
    );
    tx.rollback().await.unwrap();
}

/// Pending overage is removable, but already-dispatched work is not. A budget
/// PATCH that would make an in-flight member exceed its new share must roll
/// back instead of publishing a root budget smaller than the live inventory.
#[tokio::test]
async fn tightening_root_tree_budget_below_inflight_inventory_is_rejected_atomically() {
    let (state, root_id, repo) = boot().await;
    let root = repo.track_get(&root_id).await.unwrap().unwrap();
    let child = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: root.area_id,
            title: "child".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(&root_id)
        .bind(child.id.as_str())
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let task_id = seed_pending_report_task(&repo, child.id.as_str(), "inflight").await;
    let mut tx = repo.sqlite_pool().unwrap().begin().await.unwrap();
    assert_eq!(
        task_claim_pending_tx(&mut tx, &task_id, 1, &[], false)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();

    let before_events = event_count(&repo).await;
    let before_tracks = track_policy_snapshots(&repo, &[&root_id, child.id.as_str()]).await;
    let before_task = task_row_snapshot(&repo, &task_id).await;
    let response = patch(state, &root_id, None, json!({"tree_task_budget": 0})).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        track_policy_snapshots(&repo, &[&root_id, child.id.as_str()]).await,
        before_tracks,
        "a rejected tree-budget PATCH must not alter either affected track"
    );
    assert_eq!(
        task_row_snapshot(&repo, &task_id).await,
        before_task,
        "a rejected tree-budget PATCH must not alter the in-flight task"
    );
    assert_eq!(
        event_count(&repo).await,
        before_events,
        "a rejected tree-budget PATCH must not append events"
    );
}

/// Unlike a tree-budget change, a per-track ceiling may be committed below
/// immutable in-flight occupancy. Every persistent task column is preserved,
/// and new admission remains frozen until occupancy converges to the ceiling.
#[tokio::test]
async fn tightening_spec_ceiling_below_inflight_inventory_commits_degraded_state() {
    let (state, track_id, repo) = boot().await;
    let task_id = seed_pending_report_task(&repo, &track_id, "inflight").await;
    let mut tx = repo.sqlite_pool().unwrap().begin().await.unwrap();
    assert_eq!(
        task_claim_pending_tx(&mut tx, &task_id, 1, &[], false)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();

    let before_task = task_row_snapshot(&repo, &task_id).await;
    let before_track = track_policy_snapshots(&repo, &[&track_id])
        .await
        .pop()
        .unwrap();
    let before_events = event_cursor(&repo).await;
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"spec_task_ceiling": 0}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &track_id).await.0, Some(0));
    let after_track = track_policy_snapshots(&repo, &[&track_id])
        .await
        .pop()
        .unwrap();
    assert_track_columns_unchanged_except(
        &before_track,
        &after_track,
        &["updated_at", "spec_task_ceiling"],
    );
    assert_eq!(
        task_row_snapshot(&repo, &task_id).await,
        before_task,
        "ceiling degradation must not edit any persistent in-flight task column"
    );

    let new_events: Vec<PersistedTrackEvent> = sqlx::query_as(
        "SELECT kind,actor,scope_kind,scope_area,scope_track,scope_card,payload \
             FROM events WHERE id>?1 ORDER BY id",
    )
    .bind(before_events)
    .fetch_all(&repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(
        new_events.len(),
        1,
        "ceiling PATCH must append exactly one event envelope"
    );
    let (kind, actor, scope_kind, scope_area, scope_track, scope_card, payload) = &new_events[0];
    assert_eq!(kind, "track.updated");
    assert_eq!(
        serde_json::from_str::<Value>(actor).unwrap(),
        json!({"kind": "User"})
    );
    assert_eq!(scope_kind, "track");
    let track = repo.track_get(&track_id).await.unwrap().unwrap();
    assert_eq!(scope_area.as_deref(), Some(track.area_id.as_str()));
    assert_eq!(scope_track.as_deref(), Some(track_id.as_str()));
    assert_eq!(scope_card, &None);
    let payload: Value = serde_json::from_str(payload).unwrap();
    assert_eq!(
        payload.get("id").and_then(Value::as_str),
        Some(track_id.as_str())
    );

    let blocked_task = write_report_task(&state, &repo, &track_id, "blocked-after-tighten").await;
    let blocked_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE id=?1")
        .bind(&blocked_task)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(
        blocked_rows, 0,
        "ceiling degradation must freeze admission of a newly declared task"
    );
    let mut tx = repo.sqlite_pool().unwrap().begin().await.unwrap();
    assert_eq!(
        task_claim_pending_tx(&mut tx, &blocked_task, 2, &[], false)
            .await
            .unwrap(),
        0,
        "a task declared after tightening must not remain claimable"
    );
    tx.rollback().await.unwrap();
}
