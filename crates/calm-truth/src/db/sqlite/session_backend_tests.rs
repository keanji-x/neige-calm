use super::{SqlxRepo, session_backend::*};

async fn fixture() -> SqlxRepo {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    // Real migrated tables and triggers, with minimal owning track/session rows.
    use crate::{
        db::RepoSyncDomainRaw,
        model::{NewArea, NewTrack, RequestTheme},
    };
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "a".into(),
                color: "#abc".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            area_id: area.id,
            title: "t".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    for id in ["default", "other"] {
        sqlx::query("INSERT INTO agent_backend_registry VALUES ('codex', ?, 'codex', ?, ?)")
            .bind(id)
            .bind(id.as_bytes())
            .bind(id.as_bytes())
            .execute(repo.pool())
            .await
            .unwrap();
    }
    for id in ["a", "b", "c"] {
        sqlx::query(
            "INSERT INTO worker_sessions
            (id, track_id, provider, mode, contract, state, created_at_ms, updated_at_ms)
            VALUES (?, ?, 'codex', 'resumable', 'executor', 'running', 0, 0)",
        )
        .bind(id)
        .bind(track.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    }
    repo
}

async fn bind(repo: &SqlxRepo, session: &str, backend: &str) {
    let mut tx = repo.pool().begin().await.unwrap();
    insert_tx(&mut tx, session, "codex", backend).await.unwrap();
    tx.commit().await.unwrap();
}

fn constraint(error: sqlx::Error) {
    assert!(
        error.as_database_error().unwrap().is_check_violation()
            || error.as_database_error().unwrap().code().as_deref() == Some("1811")
    );
}

#[tokio::test]
async fn agent_backend_binding_is_immutable_and_cascades() {
    let repo = fixture().await;
    assert_eq!(
        binding_for_session(repo.db(), "a").await.unwrap(),
        SessionBackend::Unpooled
    );
    bind(&repo, "a", "other").await;
    assert_eq!(
        binding_for_session(repo.db(), "a").await.unwrap(),
        SessionBackend::Bound(BackendBinding {
            provider: "codex".into(),
            backend_id: "other".into()
        })
    );
    constraint(
        sqlx::query(
            "UPDATE worker_session_backends SET backend_id='default' WHERE worker_session_id='a'",
        )
        .execute(repo.pool())
        .await
        .unwrap_err(),
    );
    sqlx::query("DELETE FROM worker_sessions WHERE id='a'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        binding_for_session(repo.db(), "a").await.unwrap(),
        SessionBackend::Unpooled
    );
}

#[tokio::test]
async fn agent_backend_binding_requires_matching_provider_and_no_thread() {
    let repo = fixture().await;
    sqlx::query("UPDATE worker_sessions SET thread_id='historical' WHERE id='a'")
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    assert!(insert_tx(&mut tx, "a", "codex", "other").await.is_err());
    assert!(insert_tx(&mut tx, "b", "claude", "other").await.is_err());
    assert!(
        insert_tx(&mut tx, "missing", "codex", "other")
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    bind(&repo, "b", "other").await;
    constraint(
        sqlx::query("UPDATE worker_sessions SET provider='claude' WHERE id='b'")
            .execute(repo.pool())
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn agent_backend_thread_cannot_cross_bound_or_implicit_default() {
    let repo = fixture().await;
    bind(&repo, "b", "other").await;
    bind(&repo, "c", "other").await;
    sqlx::query("UPDATE worker_sessions SET thread_id='same' WHERE id='a'")
        .execute(repo.pool())
        .await
        .unwrap();
    constraint(
        sqlx::query("UPDATE worker_sessions SET thread_id='same' WHERE id='b'")
            .execute(repo.pool())
            .await
            .unwrap_err(),
    );
    sqlx::query("UPDATE worker_sessions SET thread_id='other-thread' WHERE id IN ('b','c')")
        .execute(repo.pool())
        .await
        .unwrap();
    constraint(
        sqlx::query("UPDATE worker_sessions SET thread_id='other-thread' WHERE id='a'")
            .execute(repo.pool())
            .await
            .unwrap_err(),
    );
    constraint(sqlx::query("INSERT INTO worker_sessions
        (id, track_id, provider, mode, contract, state, thread_id, created_at_ms, updated_at_ms)
        SELECT 'direct', track_id, provider, mode, contract, state, thread_id, created_at_ms, updated_at_ms
        FROM worker_sessions WHERE id='b'").execute(repo.pool()).await.unwrap_err());
    assert_eq!(
        live_bound_threads(repo.pool(), "codex")
            .await
            .unwrap()
            .len(),
        2
    );
    let mut tx = repo.pool().begin().await.unwrap();
    assert_eq!(
        live_bound_counts_tx(&mut tx, "codex").await.unwrap(),
        vec![("other".into(), 2)]
    );
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET state='exited' WHERE id='c'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        live_bound_threads(repo.pool(), "codex").await.unwrap(),
        vec![("other-thread".into(), "other".into())]
    );
}
