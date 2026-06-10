use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, wave_update_tx};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CoveId, WaveId};
use calm_server::model::{NewCard, NewCove, NewWave, WavePatch};
use calm_server::routes::theme::RequestTheme;
use calm_server::state::WriteContext;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_server::wave_vcs::{self, DiffStatus, MANIFEST_SCHEMA_VERSION};
use serde_json::json;
use sqlx::{Row, SqlitePool};

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

async fn fresh_file_repo() -> (tempfile::TempDir, Arc<SqlxRepo>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("wave-vcs.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let repo = SqlxRepo::open(&url).await.expect("open sqlite repo");
    (dir, Arc::new(repo))
}

async fn make_cove(repo: &SqlxRepo) -> calm_server::model::Cove {
    repo.cove_create(NewCove {
        name: "cove".into(),
        color: "#abcdef".into(),
        sort: None,
    })
    .await
    .expect("create cove")
}

async fn make_wave(repo: &SqlxRepo, cove_id: &str) -> calm_server::model::Wave {
    repo.wave_create(NewWave {
        cove_id: CoveId::from(cove_id),
        title: "wave".into(),
        sort: None,
        cwd: "/tmp".into(),
        attach_folder: false,
        theme: RequestTheme::default_dark(),
    })
    .await
    .expect("create wave")
}

async fn make_card(repo: &SqlxRepo, wave_id: &str) -> calm_server::model::Card {
    repo.card_create(NewCard {
        wave_id: WaveId::from(wave_id),
        kind: "terminal".into(),
        sort: None,
        payload: json!({"z": "last", "a": "first"}),
    })
    .await
    .expect("create card")
}

fn write_context() -> (CardRoleCache, WaveCoveCache, WriteContext) {
    let roles = CardRoleCache::new();
    let coves = WaveCoveCache::new();
    let write = WriteContext::new(roles.clone(), coves.clone());
    (roles, coves, write)
}

async fn count_rows(pool: &SqlitePool, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar::<_, i64>(&sql)
        .fetch_one(pool)
        .await
        .expect("count rows")
}

async fn wave_commit_rows(repo: &SqlxRepo, wave_id: &str) -> Vec<(String, Option<String>, i64)> {
    let rows = sqlx::query(
        r#"SELECT hash, parent_hash, event_id
           FROM wave_vcs_commits
           WHERE wave_id = ?1
           ORDER BY event_id ASC"#,
    )
    .bind(wave_id)
    .fetch_all(repo.pool())
    .await
    .expect("commit rows");

    rows.into_iter()
        .map(|row| {
            (
                row.try_get::<String, _>("hash").unwrap(),
                row.try_get::<Option<String>, _>("parent_hash").unwrap(),
                row.try_get::<i64, _>("event_id").unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn snapshot_tree_hash_is_deterministic_for_same_state() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    make_card(&repo, wave.id.as_str()).await;

    let mut tx = repo.pool().begin().await.expect("begin");
    let first = wave_vcs::snapshot_tree(&mut tx, &wave.id, MANIFEST_SCHEMA_VERSION)
        .await
        .expect("snapshot");
    for _ in 0..5 {
        let next = wave_vcs::snapshot_tree(&mut tx, &wave.id, MANIFEST_SCHEMA_VERSION)
            .await
            .expect("snapshot");
        assert_eq!(next.tree_hash, first.tree_hash);
        assert_eq!(next.manifest, first.manifest);
    }
    tx.rollback().await.expect("rollback");
}

#[test]
fn canonical_json_sorts_keys_and_uses_integer_time_shape() {
    let bytes = wave_vcs::canonical_json_bytes(&json!({
        "updated_at": 123456789_i64,
        "b": 2,
        "a": {"d": 4, "c": 3}
    }))
    .expect("canonical json");
    let text = String::from_utf8(bytes).expect("utf8");
    assert_eq!(text, r#"{"a":{"c":3,"d":4},"b":2,"updated_at":123456789}"#);
    assert!(!text.contains(' '));
    assert!(!text.contains('.'));
}

#[tokio::test]
async fn commit_hook_rolls_back_event_when_vcs_commit_fails() {
    let repo = fresh_repo().await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    let missing_wave = WaveId::from("missing-wave");
    let missing_cove = CoveId::from("missing-cove");

    let err = repo
        .write_with_event(
            ActorId::User,
            EventScope::Wave {
                wave: missing_wave,
                cove: missing_cove,
            },
            None,
            &bus,
            &write,
            Box::new(|_tx| {
                Box::pin(async {
                    Ok(Event::TaskCompleted {
                        idempotency_key: "rollback".into(),
                        result: json!({"status": "accepted"}),
                        artifacts: vec![],
                    })
                })
            }),
        )
        .await
        .expect_err("missing wave should fail VCS commit");
    assert!(format!("{err}").contains("wave missing-wave"));

    assert_eq!(count_rows(repo.pool(), "events").await, 0);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_commits").await, 0);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_refs").await, 0);
    drop((roles, coves));
}

#[tokio::test]
async fn wave_delete_cascades_refs_and_commits_but_leaves_objects() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;

    let backfilled = wave_vcs::backfill_existing_waves(repo.pool())
        .await
        .expect("backfill");
    assert_eq!(backfilled, 1);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_refs").await, 1);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_commits").await, 1);
    assert!(count_rows(repo.pool(), "wave_vcs_objects").await > 0);

    repo.wave_delete(wave.id.as_str())
        .await
        .expect("delete wave");
    assert_eq!(count_rows(repo.pool(), "wave_vcs_refs").await, 0);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_commits").await, 0);
    assert!(count_rows(repo.pool(), "wave_vcs_objects").await > 0);
}

#[tokio::test]
async fn concurrent_same_wave_writes_form_linear_history() {
    let (_dir, repo) = fresh_file_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let roles = CardRoleCache::new();
    let coves = WaveCoveCache::new();

    let mut handles = Vec::new();
    for key in ["one", "two"] {
        let repo = repo.clone();
        let bus = bus.clone();
        let roles = roles.clone();
        let coves = coves.clone();
        let wave_id = wave.id.clone();
        let cove_id = cove.id.clone();
        handles.push(tokio::spawn(async move {
            repo.log_pure_event(
                ActorId::User,
                EventScope::Wave {
                    wave: wave_id,
                    cove: cove_id,
                },
                None,
                &bus,
                &roles,
                &coves,
                Event::TaskCompleted {
                    idempotency_key: key.into(),
                    result: json!({"status": "accepted"}),
                    artifacts: vec![],
                },
            )
            .await
            .expect("log event");
        }));
    }
    for handle in handles {
        handle.await.expect("join");
    }

    let commits = wave_commit_rows(&repo, wave.id.as_str()).await;
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].1, None);
    assert_eq!(commits[1].1.as_deref(), Some(commits[0].0.as_str()));
    assert!(commits[1].2 > commits[0].2);
}

#[tokio::test]
async fn backfill_is_idempotent_and_uses_null_event_id_for_eventless_wave() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;

    assert_eq!(
        wave_vcs::backfill_existing_waves(repo.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        wave_vcs::backfill_existing_waves(repo.pool())
            .await
            .unwrap(),
        0
    );

    let row = sqlx::query(
        "SELECT COUNT(*) AS n, SUM(CASE WHEN event_id IS NULL THEN 1 ELSE 0 END) AS null_events FROM wave_vcs_commits WHERE wave_id = ?1",
    )
    .bind(wave.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(row.try_get::<i64, _>("n").unwrap(), 1);
    assert_eq!(row.try_get::<i64, _>("null_events").unwrap(), 1);
}

#[tokio::test]
async fn batch_write_creates_one_commit_at_last_wave_event() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (_roles, _coves, write) = write_context();
    let wave_id = wave.id.clone();
    let cove_id = cove.id.clone();

    let ids = repo
        .write_with_events(
            ActorId::User,
            None,
            &bus,
            &write,
            Box::new(move |tx| {
                let wave_id = wave_id.clone();
                let cove_id = cove_id.clone();
                Box::pin(async move {
                    let updated = wave_update_tx(
                        tx,
                        wave_id.as_str(),
                        WavePatch {
                            title: Some("renamed".into()),
                            ..WavePatch::default()
                        },
                    )
                    .await?;
                    Ok(vec![
                        (
                            EventScope::Wave {
                                wave: wave_id.clone(),
                                cove: cove_id.clone(),
                            },
                            Event::WaveUpdated(updated),
                        ),
                        (
                            EventScope::Wave {
                                wave: wave_id,
                                cove: cove_id,
                            },
                            Event::TaskCompleted {
                                idempotency_key: "batch".into(),
                                result: json!({"status": "accepted"}),
                                artifacts: vec![],
                            },
                        ),
                    ])
                })
            }),
        )
        .await
        .expect("batch write");

    let commits = wave_commit_rows(&repo, wave.id.as_str()).await;
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].2, *ids.last().unwrap());
}

#[tokio::test]
async fn incremental_commit_changes_only_expected_wave_paths() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (_roles, _coves, write) = write_context();
    let wave_id = wave.id.clone();

    repo.write_with_event(
        ActorId::User,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &write,
        Box::new(|_tx| {
            Box::pin(async {
                Ok(Event::TaskCompleted {
                    idempotency_key: "root".into(),
                    result: json!({"status": "accepted"}),
                    artifacts: vec![],
                })
            })
        }),
    )
    .await
    .expect("root commit");
    let first = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head");

    repo.write_with_event(
        ActorId::User,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &write,
        Box::new(move |tx| {
            let wave_id = wave_id.clone();
            Box::pin(async move {
                let updated = wave_update_tx(
                    tx,
                    wave_id.as_str(),
                    WavePatch {
                        title: Some("second title".into()),
                        ..WavePatch::default()
                    },
                )
                .await?;
                Ok(Event::WaveUpdated(updated))
            })
        }),
    )
    .await
    .expect("second commit");
    let second = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head");

    let diff = wave_vcs::diff(repo.pool(), &first, &second, None)
        .await
        .expect("diff");
    let paths = diff
        .iter()
        .map(|entry| (entry.path.as_str(), entry.status))
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            ("index.md", DiffStatus::Modified),
            ("wave.json", DiffStatus::Modified)
        ]
    );
}
