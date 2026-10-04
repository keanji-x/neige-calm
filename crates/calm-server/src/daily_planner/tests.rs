use super::*;
use crate::card_role_cache::CardRoleCache;
use crate::db::Repo;
use crate::db::sqlite::SqlxRepo;
use crate::db::sqlite::area_create_tx;
use crate::event::EventBus;
use crate::model::NewArea;
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::state::{AppState, CodexClient, DaemonClient, WriteContext};
use crate::track_area_cache::TrackAreaCache;
use axum::extract::FromRef;
use std::sync::Arc;

pub(crate) async fn fixture() -> (tempfile::TempDir, Arc<SqlxRepo>, AppState) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let dyn_repo: Arc<dyn Repo> = repo.clone();
    let roles = CardRoleCache::new();
    let tracks = TrackAreaCache::new();
    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("data"),
        proc_supervisor_sock: None,
    });
    std::fs::create_dir_all(&daemon.data_dir).unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        dyn_repo.clone(),
        Default::default(),
        tmp.path().join("plugins"),
        Vec::new(),
        events.clone(),
        WriteContext::new(roles.clone(), tracks.clone()),
    ));
    let mut codex = CodexClient::new_stub();
    codex.codex_bin = "/bin/false".into();
    let state = AppState::from_parts(
        dyn_repo,
        events,
        daemon,
        plugin,
        Arc::new(codex),
        Some(roles),
        Some(tracks),
    )
    .with_workspace_root(tmp.path().join("workspaces"));
    (tmp, repo, state)
}

pub(crate) fn at(day: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(day)
        .unwrap()
        .with_timezone(&Utc)
}

#[tokio::test]
async fn daily_identity_replay_is_atomic_and_does_not_start_a_model() {
    let (_tmp, repo, state) = fixture().await;
    let route = RouteState::from_ref(&state);
    let now = at("2026-10-04T01:00:00Z");
    let (a, b) = tokio::join!(reconcile(&route, now), reconcile(&route, now));
    let first = a.unwrap();
    assert_eq!(first.id, b.unwrap().id);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(reconcile(&route, now).await.unwrap().id, first.id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM events")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        count
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM areas")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM cards")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM operations")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    assert!(std::path::Path::new(&first.workspace.path).is_dir());
    assert_eq!(first.template_id.as_deref(), Some("daily-planner"));
    assert!(first.closed_at.is_none());
}

#[tokio::test]
async fn daily_midnight_and_restart_close_prior_days_without_empty_backfill() {
    let (_tmp, repo, state) = fixture().await;
    let route = RouteState::from_ref(&state);
    let old = reconcile(&route, at("2026-10-03T15:59:59Z")).await.unwrap();
    let today = reconcile(&route, at("2026-10-03T16:00:00Z")).await.unwrap();
    assert_ne!(old.id, today.id);
    assert_eq!(old.title, "2026-10-03");
    assert_eq!(today.title, "2026-10-04");
    assert!(
        route
            .repo
            .track_get(old.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_some()
    );
    assert!(today.closed_at.is_none());
    // A fresh RouteState is a restart reconciliation over the same persisted identities.
    let later = reconcile(&RouteState::from_ref(&state), at("2026-10-07T01:00:00Z"))
        .await
        .unwrap();
    assert_eq!(later.title, "2026-10-07");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM managed_track_identities")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks WHERE closed_at IS NULL")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        1
    );
    assert!(
        route
            .repo
            .areas_list_user_visible()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        route
            .repo
            .area_get(later.area_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .kind,
        crate::model::AreaKind::System
    );
    assert!(
        crate::managed_track::refuse_track_delete(Some(repo.pool().clone()), old.id.as_str())
            .await
            .is_err()
    );
}

pub(crate) async fn foreign_track(state: &RouteState) -> Track {
    let (area, _) = crate::db::write_with_event_typed(
        state.repo.as_ref(),
        ActorId::Kernel,
        EventScope::System,
        None,
        &state.events,
        &state.write,
        |tx| {
            Box::pin(async move {
                let area = area_create_tx(
                    tx,
                    NewArea {
                        name: "Project".into(),
                        color: "#6574cd".into(),
                        sort: None,
                    },
                )
                .await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await
    .unwrap();
    crate::routes::tracks::create_managed_track(
        state.clone(),
        NewTrack {
            area_id: area.id.clone(),
            title: "Project result".into(),
            sort: None,
            cwd: String::new(),
            template_id: Some("investigation".into()),
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        ManagedTrackIdentity {
            owner: "test-project".into(),
            identity: area.id.to_string(),
            report_read_scope: ReportReadScope::Area,
            report_time_zone: TIME_ZONE,
            tool_policy: ToolPolicy::Standard,
            kernel_controls_lifecycle: false,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn daily_resolution_is_read_only_and_uses_the_live_metadata_contract() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let (_tmp, repo, state) = fixture().await;
    let route = RouteState::from_ref(&state);
    let app = crate::routes::daily_planner::router().with_state(state);
    let request = |uri: &str| {
        axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let response = app
        .clone()
        .oneshot(request("/api/today/daily?date=2026-10-04"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        b"null"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM events")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    let track = reconcile(&route, at("2026-10-04T01:00:00Z")).await.unwrap();
    let response = app
        .clone()
        .oneshot(request("/api/today/daily?date=2026-10-04"))
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["track_id"], track.id.as_str());
    assert_eq!(body["date"], "2026-10-04");
    assert_eq!(body["time_zone"], "Asia/Shanghai");
    assert_eq!(
        app.oneshot(request("/api/today/daily?date=invalid"))
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(
        crate::routes::tracks::admit_template(&route, "daily-planner")
            .await
            .is_none()
    );
    assert!(
        crate::managed_track::kernel_controls_lifecycle(&route.mcp_context, track.id.as_str())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn managed_identity_replay_refuses_changed_grants() {
    let (_tmp, repo, state) = fixture().await;
    let route = RouteState::from_ref(&state);
    let track = reconcile(&route, at("2026-10-04T01:00:00Z")).await.unwrap();
    let result = crate::routes::tracks::create_managed_track(
        route.clone(),
        NewTrack {
            area_id: track.area_id.clone(),
            title: track.title.clone(),
            sort: None,
            cwd: String::new(),
            template_id: track.template_id.clone(),
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        ManagedTrackIdentity {
            owner: OWNER.into(),
            identity: "2026-10-04".into(),
            report_read_scope: ReportReadScope::Area,
            report_time_zone: TIME_ZONE,
            tool_policy: ToolPolicy::Reports,
            kernel_controls_lifecycle: true,
        },
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("different creation metadata")
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT report_read_scope FROM managed_track_identities WHERE track_id=?1"
        )
        .bind(track.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap(),
        "workspace"
    );
}
