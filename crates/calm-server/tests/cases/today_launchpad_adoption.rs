//! Real Today routes over local fake peers. Adoption must preserve persisted facts.
use crate::planner_first_start::{AppServer, app_state, router};
use crate::planner_repoint_restart_lock::request;
use axum::http::StatusCode;
use calm_server::db::sqlite::SqlxRepo;
use calm_truth::db::RepoRead;
use serde_json::{Value, json};

async fn ensure(app: axum::Router) -> (StatusCode, Value) {
    request(app, "POST", "/api/today/launchpad/ensure".into(), json!({})).await
}

#[path = "today_launchpad_snapshot.rs"]
mod snapshot;
pub(crate) use snapshot::database;

async fn legacy(repo: &SqlxRepo, dto: &Value, historical: bool) {
    let track = dto["track_id"].as_str().unwrap();
    let card = dto["planner_card_id"].as_str().unwrap();
    sqlx::query("UPDATE tracks SET purpose=NULL,template_id='small-change',template_input='{\"issue\":2843}',plugin_scope='legacy' WHERE id=?1")
        .bind(track).execute(repo.pool()).await.unwrap();
    let mut frame: Value = sqlx::query_scalar::<_, String>(
        "SELECT ws.handle_state_json FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id WHERE c.id=?1",
    ).bind(card).fetch_one(repo.pool()).await.map(|s| serde_json::from_str(&s).unwrap()).unwrap();
    if historical {
        frame = json!({"schema_version":1,"mode":"harness","phase":"idle", "push_watermark":73,
            "pending_queue":[],"last_thread_id":"native-today", "last_turn_id":"old-turn"});
    } else {
        frame["push_watermark"] = json!(73);
        frame["last_thread_id"] = json!("native-today");
        frame["last_turn_id"] = json!("old-turn");
    }
    sqlx::query(
        "UPDATE worker_sessions SET thread_id='native-today',handle_state_json=?2 \
        WHERE id=(SELECT session_id FROM cards WHERE id=?1)",
    )
    .bind(card)
    .bind(frame.to_string())
    .execute(repo.pool())
    .await
    .unwrap();
    // Both a native row and an unrelated historical raw row must survive exactly.
    for (thread, method, params) in [
        ("native-today", "item/completed", "{\"id\":1}"),
        ("older-native", "unknown/raw", "{ \"opaque\": true }"),
    ] {
        repo.fixture_transcript_insert(
            "old-runtime",
            card,
            track,
            thread,
            Some("old-turn"),
            Some(method),
            Some("agent_message"),
            method,
            params,
            None,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn u11_canonical_today_concurrent_adoption_preserves_both_formats_and_native_identity() {
    for historical in [true, false] {
        let (_tmp, repo, state) = app_state(AppServer::Down).await;
        let app = router(&state).await;
        let (status, original) = ensure(app.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{original}");
        legacy(&repo, &original, historical).await;
        let allowed = [
            "purpose",
            "template_id",
            "plugin_scope",
            "template_input",
            "updated_at",
        ];
        let before = snapshot::database_except(&repo, &allowed).await;
        let (a, b) = tokio::join!(ensure(app.clone()), ensure(app.clone()));
        let mut statuses = [a.0, b.0];
        statuses.sort();
        assert_eq!(statuses, [StatusCode::OK, StatusCode::CREATED]);
        assert_eq!(a.1, original);
        assert_eq!(b.1, original);
        assert_eq!(snapshot::database_except(&repo, &allowed).await, before);
        let track = repo
            .track_get(original["track_id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(track.purpose.as_deref(), Some("launchpad"));
        assert!(
            track.template_id.is_none()
                && track.template_input.is_none()
                && track.plugin_scope.is_none()
        );
        let unchanged = database(&repo).await;
        assert_eq!(ensure(app).await, (StatusCode::OK, original));
        assert_eq!(database(&repo).await, unchanged);
    }
}

#[tokio::test]
async fn u11_canonical_today_refusal_is_typed_and_has_zero_database_or_cache_mutation() {
    for (mutation, code) in [
        ("workspace", "workspace_drift"),
        ("already-purpose", "workspace_drift"),
        ("identity", "identity_unproven"),
        ("binding", "binding_mismatch"),
        ("native", "native_association_mismatch"),
        ("malformed", "snapshot_unproven"),
        ("snapshot-json", "snapshot_unproven"),
        ("binding-json", "binding_mismatch"),
        ("phase", "phase_unproven"),
        ("graph", "identity_unproven"),
        ("retired", "identity_unproven"),
    ] {
        refusal(mutation, code).await;
    }
}

async fn refusal(mutation: &str, code: &str) {
    let (tmp, repo, state) = app_state(AppServer::Down).await;
    let app = router(&state).await;
    let (_, dto) = ensure(app.clone()).await;
    legacy(
        &repo,
        &dto,
        matches!(mutation, "pending" | "missing-queue" | "compaction"),
    )
    .await;
    if matches!(mutation, "workspace" | "already-purpose") {
        let mut track = repo
            .track_get(dto["track_id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        track.workspace.path = "/drift".into();
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(repo.db())
            .await
            .unwrap();
        calm_server::db::sqlite::track_workspace_write_tx(
            &mut tx,
            track.id.as_str(),
            &track.workspace,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let sql = match mutation {
        "workspace" | "multiple" => "SELECT 1",
        "already-purpose" => "UPDATE tracks SET purpose='launchpad'",
        "identity" => "UPDATE cards SET session_id=NULL WHERE role='planner'",
        "binding" => {
            "UPDATE cards SET payload=json_set(payload,'$.planner_provider','claude') WHERE role='planner'"
        }
        "native" => "UPDATE worker_sessions SET thread_id='different-native'",
        "malformed" => "UPDATE worker_sessions SET handle_state_json='{}'",
        "snapshot-json" => "UPDATE worker_sessions SET handle_state_json='{'",
        "binding-json" => "UPDATE cards SET payload='{' WHERE role='planner'",
        "phase" => {
            "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.phase','turn_running')"
        }
        "graph" => "DELETE FROM terminals",
        "area-root"
        | "new-area-root"
        | "area-unrecorded"
        | "area-missing-source"
        | "area-bad-source"
        | "area-report"
        | "area-terminal"
        | "area-closed"
        | "area"
        | "new-area" => "UPDATE areas SET kind='user'",
        "pending" => {
            r#"UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.pending_queue',json('[{"type":"user_message","text":"pending"}]'))"#
        }
        "missing-queue" => {
            "UPDATE worker_sessions SET handle_state_json=json_remove(handle_state_json,'$.pending_queue')"
        }
        "compaction" => {
            r#"UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.planner_compaction',json('{"request_id":"pending","requested_turn_id":"old-turn","requested_at_ms":1,"wake_enqueued":false,"phase":{"state":"requested"}}'))"#
        }
        "retired" => "UPDATE worker_sessions SET state='superseded'",
        _ => unreachable!(),
    };
    sqlx::query(sql).execute(repo.pool()).await.unwrap();
    if matches!(mutation, "new-area" | "new-area-root") {
        sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('replacement','System','#abc',1,'system',1,1)")
                .execute(repo.pool())
                .await
                .unwrap();
    }
    let extra = match mutation {
        "area-unrecorded" => {
            "UPDATE cards SET payload=json_set(payload,'$.terminal_presentation.source','unrecorded') WHERE kind='terminal'"
        }
        "area-missing-source" => {
            "UPDATE cards SET payload=json_remove(payload,'$.terminal_presentation') WHERE kind='terminal'"
        }
        "area-bad-source" => "UPDATE cards SET payload='{' WHERE kind='terminal'",
        "area-report" => "UPDATE cards SET role='worker' WHERE role='reportcard'",
        "area-terminal" => "DELETE FROM terminals",
        "area-closed" => "UPDATE tracks SET closed_at=1",
        // Put a valid purpose candidate first: ordering must not hide the other candidate.
        "multiple" => {
            "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) SELECT 'other',area_id,'Today',2,0,0 FROM tracks"
        }
        _ => "SELECT 1",
    };
    sqlx::query(extra).execute(repo.pool()).await.unwrap();
    if mutation == "multiple" {
        sqlx::query("UPDATE tracks SET purpose='launchpad' WHERE id=?1")
            .bind(dto["track_id"].as_str().unwrap())
            .execute(repo.pool())
            .await
            .unwrap();
    }
    let state = if matches!(mutation, "area-root" | "new-area-root") {
        state.with_workspace_root(tmp.path().join("moved"))
    } else {
        state
    };
    let app = router(&state).await;
    let before = database(&repo).await;
    let caches = cache_values(&repo, &state).await;
    let (a, b) = tokio::join!(ensure(app.clone()), ensure(app.clone()));
    for (status, body) in [a, b] {
        assert_eq!(status, StatusCode::CONFLICT, "{mutation}: {body}");
        assert_eq!(body["code"], format!("today_adoption_{code}"));
    }
    let replay = ensure(router(&state).await).await;
    assert_eq!(replay.0, StatusCode::CONFLICT, "{mutation}: {}", replay.1);
    assert_eq!(replay.1["code"], format!("today_adoption_{code}"));
    assert_eq!(database(&repo).await, before, "{mutation}");
    assert_eq!(cache_values(&repo, &state).await, caches);
}

#[tokio::test]
async fn u11_canonical_today_missing_area_refuses_legacy() {
    refusal("area", "identity_unproven").await;
}
#[tokio::test]
async fn u11_canonical_today_replacement_area_refuses_legacy() {
    refusal("new-area", "identity_unproven").await;
}
#[tokio::test]
async fn u11_canonical_today_historical_pending_refused() {
    refusal("pending", "phase_unproven").await;
}
#[tokio::test]
async fn u11_canonical_today_historical_missing_queue_refused() {
    refusal("missing-queue", "phase_unproven").await;
}
#[tokio::test]
async fn u11_canonical_today_historical_compaction_refused() {
    refusal("compaction", "phase_unproven").await;
}

async fn cache_values(repo: &SqlxRepo, state: &calm_server::state::AppState) -> Value {
    let tracks: Vec<String> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(repo.pool())
        .await
        .unwrap();
    let cards: Vec<String> = sqlx::query_scalar("SELECT id FROM cards ORDER BY id")
        .fetch_all(repo.pool())
        .await
        .unwrap();
    json!([
        state.track_area_cache.len(),
        state.card_role_cache.len(),
        tracks
            .iter()
            .map(|id| state.track_area_cache.area_of(&id.clone().into()))
            .collect::<Vec<_>>(),
        cards
            .iter()
            .map(|id| (
                state.card_role_cache.get(&id.clone().into()),
                state.card_role_cache.track_of(&id.clone().into())
            ))
            .collect::<Vec<_>>()
    ])
}

#[tokio::test]
async fn u11_canonical_today_ordinary_same_title_is_not_legacy() {
    let (_tmp, repo, state) = app_state(AppServer::Down).await;
    let app = router(&state).await;
    let (_, ordinary) = ensure(app.clone()).await;
    legacy(&repo, &ordinary, false).await;
    sqlx::query("UPDATE areas SET kind='user'")
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.terminal_presentation.source','manual') WHERE kind='terminal'").execute(repo.pool()).await.unwrap();
    let (status, today) = ensure(app).await;
    assert_eq!(status, StatusCode::CREATED, "{today}");
    assert_ne!(today["track_id"], ordinary["track_id"]);
    let purpose: Option<String> = sqlx::query_scalar("SELECT purpose FROM tracks WHERE id=?1")
        .bind(ordinary["track_id"].as_str().unwrap())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert!(purpose.is_none());
}

#[tokio::test]
async fn u11_canonical_today_uncertain_area_root() {
    refusal("area-root", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_new_area_root() {
    refusal("new-area-root", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_area_unrecorded() {
    refusal("area-unrecorded", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_area_missing_source() {
    refusal("area-missing-source", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_area_bad_source() {
    refusal("area-bad-source", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_area_report() {
    refusal("area-report", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_area_terminal() {
    refusal("area-terminal", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_area_closed() {
    refusal("area-closed", "identity_unproven").await;
}

#[tokio::test]
async fn u11_canonical_today_uncertain_multiple() {
    refusal("multiple", "identity_unproven").await;
}
