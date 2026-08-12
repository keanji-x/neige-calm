use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, task_claim_pending_tx};
use calm_server::event::EventBus;
use calm_server::model::NewCard;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn boot() -> (AppState, String, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "policy".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "policy".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
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
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    (state, wave.id.to_string(), repo)
}

fn app(state: AppState) -> axum::Router {
    routes::waves::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn patch(
    state: AppState,
    wave_id: &str,
    actor: Option<&str>,
    body: Value,
) -> axum::http::Response<Body> {
    let mut request = Request::builder()
        .method("PATCH")
        .uri(format!("/api/waves/{wave_id}"))
        .header("content-type", "application/json");
    if let Some(actor) = actor {
        request = request.header("X-Calm-Actor", actor);
    }
    app(state)
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn columns(repo: &Arc<dyn Repo>, wave_id: &str) -> (Option<i64>, Option<String>) {
    sqlx::query_as("SELECT spec_task_ceiling, automation_policy FROM waves WHERE id = ?1")
        .bind(wave_id)
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

async fn task_row_snapshot(repo: &Arc<dyn Repo>, task_id: &str) -> String {
    sqlx::query_scalar(
        "SELECT json_array( \
           id,wave_id,key,kind,goal,context_json,acceptance_criteria,cwd,depends_on_json, \
           priority,gate_json,status,status_detail,worker_card_id,gate_result_json,gate_attempt, \
           gate_pid,gate_pid_starttime,gate_pid_boot_id,running_deadline_ms,context_stale_at_ms, \
           declared_by,spawn,created_at_ms,updated_at_ms,finished_at_ms) \
         FROM tasks WHERE id=?1",
    )
    .bind(task_id)
    .fetch_one(&repo.sqlite_pool().unwrap())
    .await
    .unwrap()
}

async fn wave_policy_snapshots(repo: &Arc<dyn Repo>, wave_ids: &[&str]) -> Vec<String> {
    let mut snapshots = Vec::with_capacity(wave_ids.len());
    for wave_id in wave_ids {
        snapshots.push(
            sqlx::query_scalar(
                "SELECT json_array(id,parent_wave_id,spec_task_ceiling,automation_policy,tree_task_budget) \
                 FROM waves WHERE id=?1",
            )
            .bind(wave_id)
            .fetch_one(&repo.sqlite_pool().unwrap())
            .await
            .unwrap(),
        );
    }
    snapshots
}

async fn seed_pending_report_task(repo: &Arc<dyn Repo>, wave_id: &str, key: &str) -> String {
    let block = calm_types::wave_report::ReportBlock {
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
    };
    let mut payload = calm_server::wave_report::WaveReportPayload::new(
        "",
        calm_types::report_blocks::flat_text(&block),
    );
    payload.blocks = Some(vec![block]);
    let report = repo
        .card_create(NewCard {
            wave_id: wave_id.to_owned().into(),
            title: None,
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::wave_report::WaveReportPayload::initial())
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

    let task_id = format!("{wave_id}:{key}");
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'codex',?4,'{}','[]',0,'pending','spec',0,0)")
        .bind(&task_id)
        .bind(wave_id)
        .bind(key)
        .bind(format!("{key} goal"))
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    task_id
}

#[tokio::test]
async fn policy_only_patch_is_not_short_circuited_and_clear_nulls_work() {
    let (state, wave_id, repo) = boot().await;
    assert_eq!(columns(&repo, &wave_id).await, (Some(32), None));
    let before = event_count(&repo).await;

    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"automation_policy": "declare-and-wait"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        columns(&repo, &wave_id).await,
        (Some(32), Some("declare-and-wait".into()))
    );
    assert_eq!(event_count(&repo).await, before + 1);

    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"spec_task_ceiling": 7}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &wave_id).await.0, Some(7));

    let response = patch(
        state,
        &wave_id,
        None,
        json!({"automation_policy": null, "spec_task_ceiling": null}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &wave_id).await, (None, None));
}

#[tokio::test]
async fn non_user_policy_patches_are_forbidden_without_rows_or_events() {
    let (state, wave_id, repo) = boot().await;
    let original = columns(&repo, &wave_id).await;
    let before = event_count(&repo).await;

    for body in [
        json!({"automation_policy": "auto-declare"}),
        json!({"spec_task_ceiling": 99}),
    ] {
        let response = patch(state.clone(), &wave_id, Some("ai:codex"), body).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(columns(&repo, &wave_id).await, original);
        assert_eq!(event_count(&repo).await, before);
    }
}

#[tokio::test]
async fn invalid_policy_values_are_rejected() {
    let (state, wave_id, repo) = boot().await;
    for body in [
        json!({"automation_policy": "unsafe"}),
        json!({"spec_task_ceiling": -1}),
    ] {
        let response = patch(state.clone(), &wave_id, None, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(columns(&repo, &wave_id).await, (Some(32), None));
}

async fn tree_budget(repo: &Arc<dyn Repo>, wave_id: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT tree_task_budget FROM waves WHERE id = ?1")
        .bind(wave_id)
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
    let (state, wave_id, repo) = boot().await;
    assert_eq!(tree_budget(&repo, &wave_id).await, None);
    let before = event_count(&repo).await;

    // Non-user actors are refused, with no row and no event. The REASON is
    // asserted, not just the status: an `ai:codex` REST request is also
    // refused further downstream (its legacy header path carries an empty
    // card id), so a bare 403 assertion here would stay green with the
    // user-only gate deleted.
    let response = patch(
        state.clone(),
        &wave_id,
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
    assert_eq!(tree_budget(&repo, &wave_id).await, None);
    assert_eq!(event_count(&repo).await, before);

    // Negative values are refused.
    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"tree_task_budget": -1}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(tree_budget(&repo, &wave_id).await, None);

    // The whole-tree rebuild runs in the write transaction. Its configured
    // budget (and therefore N, because member admission requires N<=B) has a
    // fixed production ceiling rather than allowing an unbounded writer hold.
    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"tree_task_budget": 65}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(tree_budget(&repo, &wave_id).await, None);

    // A budget-only patch is NOT short-circuited as an empty patch.
    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"tree_task_budget": 8}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(tree_budget(&repo, &wave_id).await, Some(8));
    assert_eq!(event_count(&repo).await, before + 1);

    // Present-null resets to the kernel default.
    let response = patch(
        state.clone(),
        &wave_id,
        None,
        json!({"tree_task_budget": null}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(tree_budget(&repo, &wave_id).await, None);

    // Root-only: a child wave refuses the patch and keeps its NULL.
    let child = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: repo
                .wave_get(&wave_id)
                .await
                .unwrap()
                .unwrap()
                .cove_id
                .clone(),
            title: "child".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let child_id = child.id.to_string();
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
        .bind(&wave_id)
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
    let (state, wave_id, repo) = boot().await;
    let block = calm_types::wave_report::ReportBlock {
        id: "b_policy".into(),
        rev: 1,
        kind: "task".into(),
        payload: json!({"key":"queued","kind":"codex","goal":"queued goal",
            "acceptance":"done","no_gate_reason":"not needed","declared_by":"spec","ready":true}),
    };
    let mut payload = calm_server::wave_report::WaveReportPayload::new(
        "",
        calm_types::report_blocks::flat_text(&block),
    );
    payload.blocks = Some(vec![block]);
    let report = repo
        .card_create(NewCard {
            wave_id: wave_id.clone().into(),
            title: None,
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::wave_report::WaveReportPayload::initial())
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
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'queued','codex','queued goal','{}','[]',0,'pending','spec',0,0)")
        .bind(format!("{wave_id}:queued")).bind(&wave_id).execute(&repo.sqlite_pool().unwrap()).await.unwrap();

    let response = patch(
        state,
        &wave_id,
        None,
        json!({"automation_policy":"declare-and-wait"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1")
        .bind(&wave_id)
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
    let (state, wave_id, repo) = boot().await;
    let block = calm_types::wave_report::ReportBlock {
        id: "b_tree_budget".into(),
        rev: 1,
        kind: "task".into(),
        payload: json!({"key":"queued","kind":"codex","goal":"queued goal",
            "acceptance":"done","no_gate_reason":"not needed","declared_by":"spec","ready":true}),
    };
    let mut payload = calm_server::wave_report::WaveReportPayload::new(
        "",
        calm_types::report_blocks::flat_text(&block),
    );
    payload.blocks = Some(vec![block]);
    let report = repo
        .card_create(NewCard {
            wave_id: wave_id.clone().into(),
            title: None,
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::wave_report::WaveReportPayload::initial())
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
    sqlx::query("INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,'queued','codex','queued goal','{}','[]',0,'pending','spec',0,0)")
        .bind(format!("{wave_id}:queued")).bind(&wave_id).execute(&repo.sqlite_pool().unwrap()).await.unwrap();

    let response = patch(state, &wave_id, None, json!({"tree_task_budget": 0})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE wave_id=?1")
        .bind(&wave_id)
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
    let root = repo.wave_get(&root_id).await.unwrap().unwrap();
    let child = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: root.cove_id,
            title: "child".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
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
    let root = repo.wave_get(&root_id).await.unwrap().unwrap();
    let child = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: root.cove_id,
            title: "child".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
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
    let before_waves = wave_policy_snapshots(&repo, &[&root_id, child.id.as_str()]).await;
    let before_task = task_row_snapshot(&repo, &task_id).await;
    let response = patch(state, &root_id, None, json!({"tree_task_budget": 0})).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        wave_policy_snapshots(&repo, &[&root_id, child.id.as_str()]).await,
        before_waves,
        "a rejected tree-budget PATCH must not alter either affected wave"
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

/// Unlike a tree-budget change, a per-wave ceiling may be committed below
/// immutable in-flight occupancy. The task is preserved byte-for-byte and new
/// admission remains frozen until occupancy converges to the new ceiling.
#[tokio::test]
async fn tightening_spec_ceiling_below_inflight_inventory_commits_degraded_state() {
    let (state, wave_id, repo) = boot().await;
    let task_id = seed_pending_report_task(&repo, &wave_id, "inflight").await;
    let mut tx = repo.sqlite_pool().unwrap().begin().await.unwrap();
    assert_eq!(
        task_claim_pending_tx(&mut tx, &task_id, 1, &[], false)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();

    let before_task = task_row_snapshot(&repo, &task_id).await;
    let before_events = event_count(&repo).await;
    let response = patch(state, &wave_id, None, json!({"spec_task_ceiling": 0})).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(columns(&repo, &wave_id).await.0, Some(0));
    assert_eq!(
        task_row_snapshot(&repo, &task_id).await,
        before_task,
        "ceiling degradation must not edit immutable in-flight task bytes"
    );
    assert_eq!(event_count(&repo).await, before_events + 1);
}
