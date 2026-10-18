use super::*;
use crate::db::RepoEventWrite;
use crate::db::sqlite::{
    SqlxRepo, area_create_tx, card_create_with_id_tx, session_insert_tx, track_create_tx,
};
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};

async fn verdict_for(
    role: CardRole,
    current: &str,
    home: &str,
    read_error: &'static str,
) -> std::result::Result<(), RoleViolation> {
    let card = CardId::from("planner");
    let (cache, areas) = seeded_caches(&card, CardRole::Planner);
    let mut tx = FakeWriteTx::with_worker_session(worker_session("old", Some(card.clone())))
        .with_card("planner", role, home);
    tx.bindings.push((card, current.into()));
    tx.card_read_error = read_error;
    enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiPlannerSession(WorkerSessionId::from("old")),
        &area_updated(),
        &card_scope("planner", "w", "c"),
        &cache,
        &areas,
    )
    .await
}

#[tokio::test]
async fn stale_planner_cache_cannot_override_sql_role() {
    assert!(verdict_for(CardRole::Worker, "old", "w", "").await.is_err());
}
#[tokio::test]
async fn active_old_session_requires_current_card_pointer() {
    assert!(
        verdict_for(CardRole::Planner, "new", "w", "")
            .await
            .is_err()
    );
}
#[tokio::test]
async fn planner_session_requires_matching_card_track() {
    assert!(
        verdict_for(CardRole::Planner, "old", "other", "")
            .await
            .is_err()
    );
}
#[tokio::test]
async fn planner_binding_read_failure_denies() {
    for read in ["role", "binding", "track"] {
        assert!(
            matches!(
                verdict_for(CardRole::Planner, "old", "w", read).await,
                Err(RoleViolation::SessionResolutionError { .. })
            ),
            "{read}"
        );
    }
}
#[tokio::test]
async fn current_sql_planner_remains_authorized() {
    assert!(verdict_for(CardRole::Planner, "old", "w", "").await.is_ok());
}

async fn seeded_repo() -> (SqlxRepo, ActorId, EventScope) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.pool())
        .await
        .unwrap();
    let area = area_create_tx(
        &mut tx,
        calm_types::organization::LEGACY_OWNER_USER_ID,
        NewArea {
            name: "a".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        "planner".into(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        },
        CardRole::Planner,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    for id in ["old", "new"] {
        let mut session = worker_session(
            id,
            if id == "old" {
                Some(card.id.clone())
            } else {
                None
            },
        );
        session.track_id = track.id.clone();
        session_insert_tx(&mut tx, session).await.unwrap();
    }
    sqlx::query("UPDATE cards SET session_id='old' WHERE id='planner'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let scope = EventScope::Card {
        card: card.id,
        track: track.id,
        area: area.id,
    };
    (
        repo,
        ActorId::AiPlannerSession(WorkerSessionId::from("old")),
        scope,
    )
}

async fn write_probe(repo: &SqlxRepo, actor: ActorId, scope: EventScope) -> Result<i64> {
    repo.write_with_event(
        actor,
        scope,
        None,
        &crate::event::EventBus::new(),
        &crate::state::WriteContext::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
        ),
        Box::new(|tx| {
            Box::pin(async move {
                sqlx::query("UPDATE cards SET title='unauthorized' WHERE id='planner'")
                    .execute(&mut **tx)
                    .await?;
                Ok(area_updated())
            })
        }),
    )
    .await
}

#[tokio::test]
async fn admitted_old_request_denied_after_pointer_commit_without_business_or_event_write() {
    let (repo, admitted_actor, scope) = seeded_repo().await;
    sqlx::query("UPDATE cards SET session_id='new' WHERE id='planner'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        write_probe(&repo, admitted_actor, scope).await,
        Err(crate::error::TruthError::Forbidden(_))
    ));
    assert_no_write(&repo).await;
}

#[tokio::test]
async fn committed_role_change_denies_stale_cache_without_business_or_event_write() {
    let (repo, actor, scope) = seeded_repo().await;
    sqlx::query("UPDATE cards SET role='worker' WHERE id='planner'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        write_probe(&repo, actor, scope).await,
        Err(crate::error::TruthError::Forbidden(_))
    ));
    assert_no_write(&repo).await;
}

async fn assert_no_write(repo: &SqlxRepo) {
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM cards WHERE id='planner'")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_ne!(title.as_deref(), Some("unauthorized"));
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(events, 0);
}

#[tokio::test]
async fn current_planner_event_write_commits() {
    let (repo, actor, scope) = seeded_repo().await;
    assert!(write_probe(&repo, actor, scope).await.unwrap() > 0);
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
async fn missing_sql_binding_denies_even_with_planner_cache() {
    let card = CardId::from("planner");
    let (cache, areas) = seeded_caches(&card, CardRole::Planner);
    let mut tx = FakeWriteTx::with_worker_session(worker_session("old", Some(card))).with_card(
        "planner",
        CardRole::Planner,
        "w",
    );
    assert!(
        enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiPlannerSession(WorkerSessionId::from("old")),
            &area_updated(),
            &card_scope("planner", "w", "c"),
            &cache,
            &areas
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn sqlite_role_read_failure_rolls_back_business_and_event_write() {
    let (repo, actor, scope) = seeded_repo().await;
    // A committed schema fault makes the production role SELECT fail; the
    // business UPDATE still succeeds before the gate and must be rolled back.
    sqlx::query("ALTER TABLE cards RENAME COLUMN role TO unreadable_role")
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        write_probe(&repo, actor, scope).await,
        Err(crate::error::TruthError::Forbidden(_))
    ));
    assert_no_write(&repo).await;
}
