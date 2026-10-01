use super::{
    model::*,
    store::{self, Access},
    *,
};
use crate::{
    card_role_cache::CardRoleCache,
    db::{prelude::*, sqlite::SqlxRepo},
    event::{EventBus, EventScope},
    ids::ActorId,
    mcp_server::registry::{AppContext, ToolCallIdentity},
    model::{CardRole, NewArea, NewCard, NewTrack},
    plugin_host::{PluginHost, PluginRegistry},
    session_projection_repo::AgentProvider,
    state::WriteContext,
    track_area_cache::TrackAreaCache,
};
use serde_json::json;
use std::sync::Arc;

struct Fixture {
    ctx: Arc<AppContext>,
    repo: Arc<SqlxRepo>,
    host: Arc<PluginHost>,
    _dir: tempfile::TempDir,
    roles: CardRoleCache,
    areas: TrackAreaCache,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Arc::new(
            SqlxRepo::open(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("calendar.db").display()
            ))
            .await
            .unwrap(),
        );
        let events = EventBus::new();
        let roles = CardRoleCache::new();
        let areas = TrackAreaCache::new();
        let write = WriteContext::new(roles.clone(), areas.clone());
        let host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty().with_builtins()),
            repo.clone(),
            dir.path().join("plugins"),
            dir.path().join("data"),
            vec![],
            events.clone(),
            write.clone(),
        ));
        host.reconcile_builtins().await.unwrap();
        host.enable(PLUGIN_ID).await.unwrap();
        let cell = Arc::new(tokio::sync::OnceCell::new());
        cell.set(host.clone()).ok().unwrap();
        let ctx = AppContext::new(
            repo.clone(),
            events,
            write,
            None,
            cell,
            Arc::new(tokio::sync::OnceCell::new()),
            dir.path().join("gates"),
        );
        Self {
            ctx,
            repo,
            host,
            _dir: dir,
            roles,
            areas,
        }
    }
    async fn identity(&self, role: CardRole) -> ToolCallIdentity {
        let area = self
            .repo
            .area_create(NewArea {
                name: "Calendar".into(),
                color: "#123456".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = self
            .repo
            .track_create(NewTrack {
                area_id: area.id.clone(),
                title: "Research".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                template_input: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::model::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = self
            .repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: json!({}),
                title: None,
            })
            .await
            .unwrap();
        sqlx::query("UPDATE cards SET role=? WHERE id=?")
            .bind(if role == CardRole::Planner {
                "planner"
            } else {
                "assistant"
            })
            .bind(card.id.as_str())
            .execute(self.repo.pool())
            .await
            .unwrap();
        self.repo.seed_card_role_cache(&self.roles).await.unwrap();
        self.repo.seed_track_area_cache(&self.areas).await.unwrap();
        self.repo
            .seed_card_role_cache(self.repo.card_role_cache())
            .await
            .unwrap();
        let session_id = crate::model::new_id();
        let mut tx = self.repo.pool().begin().await.unwrap();
        crate::db::sqlite::session_start_runtime_tx(
            &mut tx,
            crate::session_projection_repo::WorkerSessionInit {
                id: session_id.clone(),
                card_id: card.id.to_string(),
                kind: crate::session_projection_repo::WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: crate::session_projection_repo::WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(session_id.clone()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: crate::model::now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        ToolCallIdentity {
            card_id: card.id.to_string(),
            role,
            provider: AgentProvider::Codex,
            session_id: session_id.clone(),
            track_id: Some(track.id.to_string()),
            area_id: area.id.to_string(),
            thread_id: session_id,
        }
    }
}
fn human() -> Access {
    Access {
        track: None,
        actor: ActorId::User,
        scope: EventScope::System,
        creator: "user".into(),
    }
}
fn draft() -> Draft {
    Draft {
        title: "Research options".into(),
        description: "Deliver a recommendation".into(),
        schedule: Schedule::AllDay {
            date: "2026-10-02".into(),
        },
    }
}
fn request() -> Create {
    Create {
        idempotency_key: "research".into(),
        task: draft(),
    }
}
fn window() -> Window {
    Window {
        from: "2026-10-02".into(),
        until: "2026-10-03".into(),
        timezone: "Asia/Shanghai".into(),
    }
}

#[tokio::test]
async fn calendar_create_retry_update_conflict_cancel_and_durable_receipt() {
    let fx = Fixture::new().await;
    let first = store::create(&fx.ctx, human(), request()).await.unwrap();
    let retry = store::create(&fx.ctx, human(), request()).await.unwrap();
    assert_eq!(first.id, retry.id);
    let mut changed = request();
    changed.task.title = "Different".into();
    assert!(store::create(&fx.ctx, human(), changed).await.is_err());
    let update = Update {
        expected_version: 1,
        task: draft(),
        cancelled: true,
    };
    assert_eq!(
        store::update(&fx.ctx, human(), first.id.clone(), update.clone())
            .await
            .unwrap()
            .version,
        2
    );
    assert!(
        store::update(&fx.ctx, human(), first.id.clone(), update)
            .await
            .is_err()
    );
    assert!(
        store::list(&fx.ctx, &human(), window())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store::create(&fx.ctx, human(), request())
            .await
            .unwrap()
            .cancelled
    );
    let reopened = SqlxRepo::open(&format!(
        "sqlite://{}",
        fx._dir.path().join("calendar.db").display()
    ))
    .await
    .unwrap();
    assert_eq!(
        reopened
            .plugin_kv_list(PLUGIN_ID, "entry:")
            .await
            .unwrap()
            .len(),
        1
    );
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='plugin.data.changed'")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    assert_eq!(count, 2, "retry and rejected edits publish no change");
}
#[tokio::test]
async fn calendar_native_tools_enforce_scope_role_session_and_enablement() {
    let fx = Fixture::new().await;
    let registry = crate::mcp_server::build_default_registry();
    let create = registry.lookup("calm.calendar.create").unwrap();
    let list = registry.lookup("calm.calendar.list").unwrap();
    let update = registry.lookup("calm.calendar.update").unwrap();
    let owner = fx.identity(CardRole::Planner).await;
    let other = fx.identity(CardRole::Assistant).await;
    let result = create(fx.ctx.clone(), owner.clone(), json!(request()))
        .await
        .unwrap();
    let encoded = serde_json::to_value(result).unwrap();
    let entry: Entry = serde_json::from_value(encoded["structuredContent"].clone()).unwrap();
    assert_eq!(entry.source_track_id, owner.track_id);
    let other_list = list(
        fx.ctx.clone(),
        other.clone(),
        json!({"from":"2026-10-02","until":"2026-10-03","timezone":"Asia/Shanghai"}),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(other_list).unwrap()["structuredContent"],
        json!([])
    );
    assert!(
        update(
            fx.ctx.clone(),
            other.clone(),
            json!({"id":entry.id,"expected_version":1,"task":draft(),"cancelled":true})
        )
        .await
        .is_err()
    );
    let mut worker = owner.clone();
    worker.role = CardRole::Worker;
    assert!(
        create(fx.ctx.clone(), worker, json!(request()))
            .await
            .is_err()
    );
    let mut forged = json!(request());
    forged["source_track_id"] = json!(other.track_id);
    assert!(create(fx.ctx.clone(), owner.clone(), forged).await.is_err());
    // A fresh valid assistant can create only within its own Track.
    create(fx.ctx.clone(), other.clone(), json!(request()))
        .await
        .unwrap();
    let mut stale = owner.clone();
    stale.session_id = "missing-session".into();
    let mut second = request();
    second.idempotency_key = "another".into();
    assert!(create(fx.ctx.clone(), stale, json!(second)).await.is_err());
    fx.host.disable(PLUGIN_ID).await.unwrap();
    assert!(
        create(fx.ctx.clone(), owner, json!(request()))
            .await
            .is_err()
    );
    assert_eq!(
        store::list(&fx.ctx, &human(), window())
            .await
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn calendar_timezones_midnight_and_dst() {
    let mut d = draft();
    d.schedule = Schedule::Timed {
        start: "2026-10-01T23:30:00+08:00".into(),
        end: "2026-10-02T00:00:00+08:00".into(),
        timezone: "Asia/Shanghai".into(),
    };
    d.validate().unwrap();
    assert!(!window().contains(&d.schedule).unwrap());
    if let Schedule::Timed { end, .. } = &mut d.schedule {
        *end = "2026-10-02T00:01:00+08:00".into();
    }
    assert!(window().contains(&d.schedule).unwrap());
    for (start, end, valid) in [
        (
            "2026-03-08T02:30:00-05:00",
            "2026-03-08T04:00:00-04:00",
            false,
        ),
        (
            "2026-11-01T01:30:00-04:00",
            "2026-11-01T01:30:00-05:00",
            true,
        ),
        (
            "2026-10-01T14:00:00+08:00",
            "2026-10-01T15:00:00+08:00",
            false,
        ),
    ] {
        d.schedule = Schedule::Timed {
            start: start.into(),
            end: end.into(),
            timezone: "America/New_York".into(),
        };
        assert_eq!(d.validate().is_ok(), valid);
    }
    assert!(date("2026-02-30").is_err());
    assert!(timezone("not-a-zone").is_err());
}

#[tokio::test]
async fn calendar_http_create_edit_and_disable_share_the_plugin_store() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let fx = Fixture::new().await;
    let state = crate::state::AppState::from_parts(
        fx.repo.clone(),
        fx.ctx.events.clone(),
        Arc::new(crate::state::DaemonClient {
            data_dir: fx._dir.path().join("daemon"),
            proc_supervisor_sock: None,
        }),
        fx.host.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        None,
        None,
    )
    .with_mcp_context(fx.ctx.clone());
    let app = crate::routes::protected_router()
        .layer(axum::middleware::from_fn(crate::actor::actor_middleware))
        .with_state(state);
    let make = |actor: &str| {
        Request::builder()
            .method("POST")
            .uri("/api/calendar/tasks")
            .header("content-type", "application/json")
            .header("x-calm-actor", actor)
            .body(Body::from(serde_json::to_vec(&request()).unwrap()))
            .unwrap()
    };
    let rejected = app.clone().oneshot(make("ai:codex")).await.unwrap();
    assert_eq!(rejected.status(), 403);
    let created = app.clone().oneshot(make("user")).await.unwrap();
    assert_eq!(created.status(), 200);
    let entry: Entry =
        serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(entry.source_track_id.is_none());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/calendar/tasks/{}", entry.id))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&Update {
                        expected_version: 1,
                        task: draft(),
                        cancelled: true,
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        store::list(&fx.ctx, &human(), window())
            .await
            .unwrap()
            .is_empty()
    );
    fx.host.disable(PLUGIN_ID).await.unwrap();
    assert_eq!(app.oneshot(make("user")).await.unwrap().status(), 409);
    let task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    let track_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        (task_count, track_count),
        (0, 0),
        "scheduling never launches execution"
    );
}

#[tokio::test]
async fn calendar_concurrent_creation_and_edits_are_serialized() {
    let fx = Fixture::new().await;
    let (first, second) = tokio::join!(
        store::create(&fx.ctx, human(), request()),
        store::create(&fx.ctx, human(), request())
    );
    let first = first.unwrap();
    assert_eq!(first.id, second.unwrap().id);
    let edit = Update {
        expected_version: 1,
        task: draft(),
        cancelled: false,
    };
    let (a, b) = tokio::join!(
        store::update(&fx.ctx, human(), first.id.clone(), edit.clone()),
        store::update(&fx.ctx, human(), first.id, edit)
    );
    assert_ne!(
        a.is_ok(),
        b.is_ok(),
        "only one concurrent edit owns the revision"
    );
    assert_eq!(
        store::list(&fx.ctx, &human(), window()).await.unwrap()[0].version,
        2
    );
}

#[tokio::test]
async fn calendar_planner_timed_roundtrip_and_bound_track_limit() {
    let fx = Fixture::new().await;
    let registry = crate::mcp_server::build_default_registry();
    let create = registry.lookup("calm.calendar.create").unwrap();
    let list = registry.lookup("calm.calendar.list").unwrap();
    let update = registry.lookup("calm.calendar.update").unwrap();
    let planner = fx.identity(CardRole::Planner).await;
    let timed = json!({"title":"Review research","description":"Deliver recommendations","schedule":{
        "kind":"timed","start":"2026-10-02T09:00:00+08:00","end":"2026-10-02T10:00:00+08:00","timezone":"Asia/Shanghai"
    }});
    let request = json!({"idempotency_key":"planner-review-timed","task":timed});
    let first = create(fx.ctx.clone(), planner.clone(), request.clone())
        .await
        .unwrap();
    let first = serde_json::to_value(first).unwrap()["structuredContent"].clone();
    let retry = create(fx.ctx.clone(), planner.clone(), request)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(retry).unwrap()["structuredContent"],
        first
    );
    assert_eq!(first["source_track_id"], json!(planner.track_id));
    let visible = list(
        fx.ctx.clone(),
        planner.clone(),
        json!({"from":"2026-10-02","until":"2026-10-03","timezone":"Asia/Shanghai"}),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(visible).unwrap()["structuredContent"],
        json!([first])
    );
    let mut moved = timed.clone();
    moved["schedule"]["start"] = json!("2026-10-02T11:00:00+08:00");
    moved["schedule"]["end"] = json!("2026-10-02T12:00:00+08:00");
    let revised = update(
        fx.ctx.clone(),
        planner.clone(),
        json!({
            "id":first["id"],"expected_version":first["version"],"task":moved,"cancelled":false
        }),
    )
    .await
    .unwrap();
    let revised = serde_json::to_value(revised).unwrap()["structuredContent"].clone();
    assert_eq!(revised["version"], 2);
    let human_view = store::list(&fx.ctx, &human(), window()).await.unwrap();
    assert_eq!(human_view.len(), 1);
    assert_eq!(serde_json::to_value(&human_view[0].task).unwrap(), moved);
    update(
        fx.ctx.clone(),
        planner.clone(),
        json!({
            "id":first["id"],"expected_version":revised["version"],"task":moved,"cancelled":true
        }),
    )
    .await
    .unwrap();
    let visible = list(
        fx.ctx.clone(),
        planner.clone(),
        json!({"from":"2026-10-02","until":"2026-10-03","timezone":"Asia/Shanghai"}),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(visible).unwrap()["structuredContent"],
        json!([])
    );
    assert!(
        store::list(&fx.ctx, &human(), window())
            .await
            .unwrap()
            .is_empty()
    );

    // Evidence for the current product limitation, not permission to bypass ownership:
    // a development-owned Planner cannot discover or invoke Calendar's native tools.
    let owner = crate::builtin_plugins::dev::PLUGIN_ID;
    fx.host.enable(owner).await.unwrap();
    sqlx::query("UPDATE tracks SET plugin_scope=? WHERE id=?")
        .bind(owner)
        .bind(planner.track_id.as_deref().unwrap())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let scope = crate::mcp_server::tool_visibility::plugin_scope_for_track(
        &fx.ctx,
        planner.track_id.as_deref(),
    )
    .await;
    assert!(!scope.allows_manifest(crate::builtin_plugins::get(PLUGIN_ID).unwrap().manifest()));
    let error = create(
        fx.ctx.clone(),
        planner,
        json!({"idempotency_key":"bound-review","task":timed}),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, -32601);
}
