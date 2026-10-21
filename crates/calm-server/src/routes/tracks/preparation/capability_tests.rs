use super::*;
use crate::model::NewArea;
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::state::{AppState, CodexClient, DaemonClient, WriteContext};
use axum::extract::FromRef;
use calm_truth::db::RepoSyncDomainRaw;
use calm_truth::db::sqlite::SqlxRepo;
use serde_json::json;
use std::sync::Arc;

async fn fixture(dir: &std::path::Path) -> (RouteState, Arc<SqlxRepo>, String) {
    fixture_body(dir, "").await
}

async fn fixture_body(dir: &std::path::Path, extra: &str) -> (RouteState, Arc<SqlxRepo>, String) {
    let templates_dir = dir.join("templates");
    std::fs::create_dir(&templates_dir).unwrap();
    let body =
        calm_types::report_contract::canonical_line(&calm_types::track_report::work_brief_header());
    std::fs::write(templates_dir.join("cap.md"), format!("+++\nid = \"cap\"\ntitle = \"Capability\"\ncapabilities = [\"objectives\"]\n+++\n{body}\n# Plan\n{extra}")).unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "capability".into(),
                color: "".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let roles = crate::card_role_cache::CardRoleCache::new();
    let tracks = crate::track_area_cache::TrackAreaCache::new();
    let events = crate::event::EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: dir.into(),
            proc_supervisor_sock: None,
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            dir.into(),
            dir.join("plugins"),
            vec![],
            events,
            WriteContext::new(roles.clone(), tracks.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(tracks),
    )
    .with_templates_dir(&templates_dir);
    let mut state = RouteState::from_ref(&state);
    state.workspace_root = dir.join("workspaces");
    (state, repo, area.id.to_string())
}

fn new_track(area: &str, template: Option<&str>) -> NewTrack {
    serde_json::from_value(json!({"area_id": area, "title": "capability", "template_id": template, "cwd": "/tmp", "theme": {"fg": [216,219,226], "bg": [15,20,24]}})).unwrap()
}

async fn create(s: &RouteState, area: &str, source: preparation::NamedSource) -> Track {
    let init = source.resolve(s).await.unwrap();
    let mut p = new_track(area, None);
    init.stamp(&mut p);
    let options = CreateTrackOptions {
        human_source: None,
        admin: false,
        managed_identity: None,
        creator: None,
        creation_message: None,
        planner_provider: AgentProvider::Codex,
        model: None,
        reasoning_effort: None,
        folder_claim: FolderClaim::Skip,
        body_area_id: area.into(),
        normalized_cwd: p.cwd.clone(),
        init: init.init,
        workspace_plan: TrackWorkspacePlan::ManagedUnder(s.workspace_root.clone()),
        idempotency_claim: None,
    };
    create_track_structure(s.clone(), ActorId::Kernel, p, options)
        .await
        .unwrap()
        .0
}

async fn capabilities(repo: &SqlxRepo, track: &Track) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT capability FROM track_capabilities WHERE track_id=? ORDER BY capability",
    )
    .bind(track.id.as_str())
    .fetch_all(repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
#[allow(deprecated)]
async fn capability_stamped_only_for_template_init() {
    let dir = tempfile::tempdir().unwrap();
    let (s, repo, area) = fixture(dir.path()).await;
    // Also prove stamp order: structural report initialization updates the report card.
    sqlx::raw_sql(concat!(
        "CREATE TRIGGER capability_before_report BEFORE UPDATE ON cards WHEN NEW.kind='track-report' AND ",
        "(SELECT template_id FROM tracks WHERE id=NEW.track_id)='site/cap' BEGIN SELECT CASE WHEN NOT ",
        "EXISTS(SELECT 1 FROM track_capabilities WHERE track_id=NEW.track_id AND capability='objectives') ",
        "THEN RAISE(ABORT,'capability must precede report') END; END;",
    ))
        .execute(repo.pool()).await.unwrap();
    let template = create(
        &s,
        &area,
        preparation::NamedSource::Template("site/cap".into()),
    )
    .await;
    assert_eq!(capabilities(&repo, &template).await, ["objectives"]);
    let blank = create(&s, &area, preparation::NamedSource::Blank).await;
    let fork = create(
        &s,
        &area,
        preparation::NamedSource::Fork(template.id.to_string()),
    )
    .await;
    let mut tx = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
        .await
        .unwrap();
    let recipe =
        calm_truth::db::sqlite::track_recipe_create_tx(
            &mut tx,
            "capability",
            &calm_types::report_contract::canonical_line(
                &calm_types::track_report::work_brief_header(),
            ),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let recipe = create(&s, &area, preparation::NamedSource::Recipe(recipe.id)).await;
    let builtin = create(
        &s,
        &area,
        preparation::NamedSource::Template(crate::templates::INVESTMENT_RESEARCH.into()),
    )
    .await;
    for track in [&blank, &fork, &recipe, &builtin] {
        assert!(capabilities(&repo, track).await.is_empty(), "{}", track.id);
    }
    let mut tx = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
        .await
        .unwrap();
    let subtrack = track_create_tx(
        &mut tx,
        new_track(&area, None),
        None,
        &TrackWorkspacePlan::ManagedUnder(s.workspace_root.clone()),
        None,
        s.write.area_cache(),
    )
    .await
    .unwrap();
    crate::track_creation::initialize_track_tx(
        &mut tx,
        &subtrack,
        ("child-planner", "child-report"),
        crate::track_creation::TrackInitialization::Subtrack {
            seed: "delegated objective".into(),
            provider: AgentProvider::Codex,
            permission: crate::planner_permission_mode::PlannerPermissionMode::Never,
            selection: crate::planner_model::CardModelSelection::default(),
        },
        s.write.role_cache(),
        &s.acp_planner,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(capabilities(&repo, &subtrack).await.is_empty());
    let identity = crate::managed_track::ManagedTrackIdentity {
        owner: "capability-test".into(),
        identity: "reports".into(),
        report_read_scope: crate::managed_track::ReportReadScope::Area,
        report_time_zone: chrono_tz::UTC,
        tool_policy: crate::managed_track::ToolPolicy::Reports,
        kernel_controls_lifecycle: true,
    };
    let managed = create_managed_track(
        s.clone(),
        new_track(&area, Some(crate::templates::DAILY_PLANNER)),
        identity.clone(),
    )
    .await
    .unwrap();
    assert!(capabilities(&repo, &managed).await.is_empty());
    let before: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM track_capabilities),(SELECT count(*) FROM cards)").fetch_one(repo.pool()).await.unwrap();
    let mut identity = identity;
    identity.identity = "forbidden".into();
    let result =
        create_managed_track(s.clone(), new_track(&area, Some("site/cap")), identity).await;
    assert!(matches!(result, Err(CalmError::Internal(_))), "{result:?}");
    let after: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM track_capabilities),(SELECT count(*) FROM cards)").fetch_one(repo.pool()).await.unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn template_born_objective_is_defined_at_creation() {
    let dir = tempfile::tempdir().unwrap();
    let definition = serde_json::json!({"key":"build","title":"Build","mode":"converge", "checks":[{"id":"tests","role":"goal","config":{"kind":"command","params":{"command":"true"}}}],"constraints":[],"budget":{"turns":10,"tokens":1000,"wall_secs":60},"compact":"none"});
    let fence = calm_types::report_blocks::render_fence("objective", &definition);
    let (s, repo, area) = fixture_body(dir.path(), &fence).await;
    let track = create(
        &s,
        &area,
        preparation::NamedSource::Template("site/cap".into()),
    )
    .await;
    let (seq, state) = calm_truth::objective_host::load_state(repo.db(), &track.id)
        .await
        .unwrap();
    assert_eq!(seq.0, 1);
    assert_eq!(state.objectives.len(), 1);
    assert_eq!(
        serde_json::to_value(&state.objectives.values().next().unwrap().definition).unwrap(),
        definition
    );
}
