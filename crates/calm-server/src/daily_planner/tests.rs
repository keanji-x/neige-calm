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
async fn daily_planner_freezes_workspace_report_working_method() {
    let (_tmp, _repo, state) = fixture().await;
    let route = RouteState::from_ref(&state);
    let track = reconcile(&route, at("2026-10-04T01:00:00Z")).await.unwrap();
    let card = route
        .repo
        .cards_by_track(track.id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "codex")
        .unwrap();
    let context: calm_types::conversation::SystemRenderContext = serde_json::from_value(
        card.payload[crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY].clone(),
    )
    .unwrap();
    let instructions = context.text();
    for required in [
        "neige_workspace_ls",
        "neige_workspace_cat",
        "neige_workspace_diff",
        "neige_workspace_log",
        "all user-visible Areas",
        "next_cursor is null",
        "no write authority over other Tracks",
    ] {
        assert!(
            instructions.contains(required),
            "missing startup instruction: {required}"
        );
    }
}

#[tokio::test]
async fn u9_canonical_daily_rollover_no_backfill() {
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
        crate::managed_track::refuse_track_delete(Some(repo.db().clone()), old.id.as_str())
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
                    calm_types::organization::LEGACY_OWNER_USER_ID,
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
async fn u9_canonical_managed_identity_collision() {
    let (_tmp, repo, state) = fixture().await;
    let route = RouteState::from_ref(&state);
    let now = at("2026-10-04T01:00:00Z");
    let track = reconcile(&route, now).await.unwrap();
    let other = foreign_track(&route).await;
    let before = allocation_counts(repo.pool()).await;
    for variant in 0..6 {
        let mut p = NewTrack {
            area_id: track.area_id.clone(),
            title: track.title.clone(),
            sort: None,
            cwd: String::new(),
            template_id: track.template_id.clone(),
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        };
        let mut identity = ManagedTrackIdentity {
            owner: OWNER.into(),
            identity: "2026-10-04".into(),
            report_read_scope: ReportReadScope::Workspace,
            report_time_zone: TIME_ZONE,
            tool_policy: ToolPolicy::Reports,
            kernel_controls_lifecycle: true,
        };
        match variant {
            0 => p.area_id = other.area_id.clone(),
            1 => p.template_id = Some("investigation".into()),
            2 => identity.report_read_scope = ReportReadScope::Area,
            3 => identity.report_time_zone = chrono_tz::UTC,
            4 => identity.tool_policy = ToolPolicy::Standard,
            5 => identity.kernel_controls_lifecycle = false,
            _ => unreachable!(),
        }
        assert!(
            matches!(
                crate::routes::tracks::create_managed_track(route.clone(), p, identity).await,
                Err(CalmError::Conflict(_))
            ),
            "variant {variant}"
        );
        assert_eq!(allocation_counts(repo.pool()).await, before);
    }
    assert_eq!(reconcile(&route, now).await.unwrap().id, track.id);
    assert_eq!(allocation_counts(repo.pool()).await, before);
}

#[tokio::test]
async fn u9_canonical_daily_empty_replay() {
    use crate::harness::snapshot::CanonicalHarnessSnapshot;
    use crate::session_projection_repo::WorkerSessionState;
    use calm_truth::session_repo::SessionRepo;
    let (_tmp, repo, state) = fixture().await;
    let route = RouteState::from_ref(&state);
    let now = at("2026-10-04T01:00:00Z");
    let (a, b) = tokio::join!(reconcile(&route, now), reconcile(&route, now));
    let track = a.unwrap();
    assert_eq!(track.id, b.unwrap().id);
    let (card, worker, raw): (String, String, String) = sqlx::query_as(
        "SELECT c.id, w.id, w.handle_state_json FROM cards c
         JOIN worker_sessions w ON w.id=c.session_id
         WHERE c.track_id=?1 AND c.role='planner'",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    let frame: CanonicalHarnessSnapshot = serde_json::from_str(&raw).unwrap();
    assert!(frame.canonical_inputs.pending.is_empty());
    assert!(frame.canonical_inputs.active.is_none());
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert!(frame.header.last_thread_id.is_none());
    assert!(frame.header.last_turn_id.is_none());
    let raw: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        raw["schema_version"],
        serde_json::to_value(CanonicalHarnessSnapshot::dormant(0)).unwrap()["schema_version"]
    );
    assert!(raw.get("pending_queue").is_none());
    let session = repo
        .session_get(&calm_types::worker::WorkerSessionId(worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state, WorkerSessionState::Idle);
    assert!(session.thread_id.is_none());
    assert!(session.agent_session_id.is_none());
    assert!(session.active_turn_id.is_none());
    assert!(session.mcp_token_hash.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>("SELECT root_session_id FROM tracks WHERE id=?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .unwrap()
            .as_deref(),
        Some(worker.as_str())
    );
    assert!(state.harness.get(&worker).is_none());
    let counts: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM worker_sessions),
         (SELECT count(*) FROM managed_track_identities WHERE track_id=?1),
         (SELECT count(*) FROM cards WHERE track_id=?1 AND role='reportcard'),
         (SELECT count(*) FROM operations), (SELECT count(*) FROM conversation_presentations),
         (SELECT count(*) FROM card_mcp_tokens)",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 1, 0, 0, 0));
    let before = allocation_counts(repo.pool()).await;
    assert_eq!(reconcile(&route, now).await.unwrap().id, track.id);
    assert_eq!(allocation_counts(repo.pool()).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>("SELECT session_id FROM cards WHERE id=?1")
            .bind(&card)
            .fetch_one(repo.pool())
            .await
            .unwrap()
            .as_deref(),
        Some(worker.as_str())
    );
    // Ordinary managed templates also freeze context; there is no daily identity special case.
    let ordinary = foreign_track(&route).await;
    let planner = route
        .repo
        .cards_by_track(ordinary.id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|card| card.kind == "codex")
        .unwrap();
    assert!(
        planner
            .payload
            .get(crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY)
            .is_some()
    );
}

async fn allocation_counts(pool: &sqlx::SqlitePool) -> (i64, i64, i64, i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM cards),
        (SELECT count(*) FROM worker_sessions),(SELECT count(*) FROM managed_track_identities),
        (SELECT count(*) FROM events),(SELECT count(*) FROM conversation_presentations),
        (SELECT count(*) FROM overlays)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn u9_canonical_managed_allocation_rollback() {
    for table in ["worker_sessions", "events"] {
        let (_tmp, repo, state) = fixture().await;
        let route = RouteState::from_ref(&state);
        // System Area ensure has its own earlier transaction; isolate allocation rollback.
        ensure_area(&route).await.unwrap();
        let before = allocation_counts(repo.pool()).await;
        sqlx::query(&format!(
            "CREATE TRIGGER fail_u9 BEFORE INSERT ON {table}
            BEGIN SELECT RAISE(ABORT,'u9 allocation fault'); END"
        ))
        .execute(repo.pool())
        .await
        .unwrap();
        assert!(
            reconcile(&route, at("2026-10-04T01:00:00Z")).await.is_err(),
            "{table}"
        );
        assert_eq!(allocation_counts(repo.pool()).await, before, "{table}");
        sqlx::query("DROP TRIGGER fail_u9")
            .execute(repo.pool())
            .await
            .unwrap();
        reconcile(&route, at("2026-10-04T01:00:00Z")).await.unwrap();
    }
}
