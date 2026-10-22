//! Shared fixture creates real typed owner rows; adapters remain the behavior under test.
use uuid::Uuid;

use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, new_id};
use crate::session_projection_repo::{WorkerSessionInit, WorkerSessionKind, WorkerSessionState};

pub(super) struct Owner {
    pub repo: Arc<SqlxRepo>,
    pub card: String,
    pub track: String,
    pub worker: String,
    pub thread: String,
    pub db_url: String,
}

pub(super) async fn seed(cwd: &std::path::Path, provider: AgentProvider) -> Owner {
    seed_with_url(cwd, provider, "sqlite::memory:").await
}

pub(super) async fn seed_with_url(
    cwd: &std::path::Path,
    provider: AgentProvider,
    db_url: &str,
) -> Owner {
    std::fs::create_dir_all(cwd.join("data")).unwrap();
    let repo = Arc::new(SqlxRepo::open(db_url).await.unwrap());
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "dispatch evidence".into(),
                color: "#111111".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "dispatch evidence".into(),
            sort: None,
            cwd: cwd.to_string_lossy().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.db())
        .await
        .unwrap();
    let card = card_create_with_id_tx(&mut tx, new_id(), NewCard { track_id: track.id.clone(),
        title: None, kind: "codex".into(), sort: None,
        payload: serde_json::json!({"schemaVersion":1,"planner_harness":true,"planner_provider":provider.wire_name(),"permission_mode":"never"}) },
        CardRole::Planner, false, &crate::card_role_cache::CardRoleCache::new()).await.unwrap();
    let thread = Uuid::new_v4().to_string();
    let worker = new_id();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: worker.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(provider),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread.clone()),
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
    Owner {
        db_url: db_url.to_owned(),
        repo,
        card: card.id.to_string(),
        track: track.id.to_string(),
        worker,
        thread,
    }
}

pub(super) async fn fail_terminal_outcome(repo: &crate::db::sqlite::SqlxRepo) {
    sqlx::query(
        "CREATE TRIGGER fail_outcome BEFORE INSERT ON harness_items
        WHEN NEW.method='turn/completed' BEGIN SELECT RAISE(ABORT,'outcome failed'); END",
    )
    .execute(repo.pool())
    .await
    .unwrap();
}
