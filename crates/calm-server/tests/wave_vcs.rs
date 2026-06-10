use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, runtime_start_tx, terminal_create_tx, wave_update_tx,
};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{
    Card, CardRole, NewCard, NewCove, NewTerminal, NewWave, WavePatch, new_id, now_ms,
};
use calm_server::routes::theme::RequestTheme;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::state::WriteContext;
use calm_server::wave_cove_cache::WaveCoveCache;
use calm_server::wave_fs_view::WaveFsView;
use calm_server::wave_report::WaveReportPayload;
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

#[allow(clippy::too_many_arguments)]
async fn add_card_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    roles: &CardRoleCache,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
    kind: &str,
    role: CardRole,
    payload: serde_json::Value,
) -> Card {
    let card_id = new_id();
    let lookup_card_id = card_id.clone();
    let scope = EventScope::Card {
        card: CardId::from(card_id.clone()),
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let new_card = NewCard {
        wave_id: wave_id.clone(),
        kind: kind.into(),
        sort: None,
        payload,
    };
    let roles = roles.clone();
    repo.write_with_event(
        ActorId::Kernel,
        scope,
        None,
        bus,
        write,
        Box::new(move |tx| {
            let roles = roles.clone();
            let card_id = card_id.clone();
            let new_card = new_card.clone();
            Box::pin(async move {
                let card = card_create_with_id_tx(
                    tx,
                    card_id,
                    new_card,
                    role,
                    !matches!(role, CardRole::ReportCard | CardRole::Spec),
                    &roles,
                )
                .await?;
                Ok(Event::CardAdded(card))
            })
        }),
    )
    .await
    .expect("card added event");

    match repo.card_get(&lookup_card_id).await {
        Ok(Some(card)) => card,
        other => panic!("created card missing after CardAdded event: {other:?}"),
    }
}

async fn add_report_card(
    repo: &SqlxRepo,
    bus: &EventBus,
    roles: &CardRoleCache,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
) -> Card {
    add_card_with_event(
        repo,
        bus,
        roles,
        write,
        wave_id,
        cove_id,
        "wave-report",
        CardRole::ReportCard,
        serde_json::to_value(WaveReportPayload::initial()).expect("report payload"),
    )
    .await
}

async fn insert_raw_report_card(repo: &SqlxRepo, roles: &CardRoleCache, wave_id: &WaveId) -> Card {
    let mut tx = repo.pool().begin().await.expect("begin raw report insert");
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave_id.clone(),
            kind: "wave-report".into(),
            sort: None,
            payload: serde_json::to_value(WaveReportPayload::initial()).expect("report payload"),
        },
        CardRole::ReportCard,
        false,
        roles,
    )
    .await
    .expect("insert raw report card");
    tx.commit().await.expect("commit raw report insert");
    card
}

async fn start_codex_runtime_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
    card_id: &CardId,
) {
    let runtime_id = new_id();
    let card_id_for_runtime = card_id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    repo.write_with_event(
        ActorId::Kernel,
        scope,
        None,
        bus,
        write,
        Box::new(move |tx| {
            let runtime_id = runtime_id.clone();
            let card_id = card_id_for_runtime.clone();
            Box::pin(async move {
                let terminal = terminal_create_tx(
                    tx,
                    NewTerminal {
                        card_id: card_id.clone(),
                        program: "codex".into(),
                        cwd: "/tmp".into(),
                        env: json!({}),
                        theme: RequestTheme::default_dark(),
                    },
                )
                .await?;
                let runtime = runtime_start_tx(
                    tx,
                    RuntimeInit {
                        id: runtime_id,
                        card_id: card_id.to_string(),
                        kind: RuntimeKind::CodexCard,
                        agent_provider: Some(AgentProvider::Codex),
                        status: RunStatus::Running,
                        terminal_run_id: Some(terminal.id),
                        thread_id: Some("thread-1".into()),
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        lease_owner: None,
                        lease_until_ms: None,
                        now_ms: now_ms(),
                    },
                )
                .await?;
                Ok(Event::RuntimeStarted {
                    runtime_id: runtime.id,
                    card_id: runtime.card_id,
                    kind: runtime.kind,
                    agent_provider: runtime.agent_provider,
                    status: runtime.status,
                })
            })
        }),
    )
    .await
    .expect("runtime started event");
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

async fn wave_commit_rows(
    repo: &SqlxRepo,
    wave_id: &str,
) -> Vec<(String, Option<String>, Option<i64>)> {
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
                row.try_get::<Option<i64>, _>("event_id").unwrap(),
            )
        })
        .collect()
}

async fn blob_text(repo: &SqlxRepo, hash: &str) -> String {
    let bytes: Vec<u8> =
        sqlx::query_scalar("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'blob'")
            .bind(hash)
            .fetch_one(repo.pool())
            .await
            .expect("blob bytes");
    String::from_utf8(bytes).expect("blob utf8")
}

async fn head_manifest(repo: &SqlxRepo, wave_id: &WaveId) -> wave_vcs::TreeManifest {
    let head = wave_vcs::head(repo.pool(), wave_id)
        .await
        .expect("head query")
        .expect("head");
    wave_vcs::tree_at(repo.pool(), &head)
        .await
        .expect("tree query")
        .expect("tree")
}

#[tokio::test]
async fn snapshot_tree_hash_is_deterministic_for_same_state() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "terminal",
        CardRole::Worker,
        json!({"z": "last", "a": "first"}),
    )
    .await;

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
}

#[tokio::test]
async fn commit_hook_rolls_back_event_when_vcs_commit_fails() {
    let repo = fresh_repo().await;
    let bus = EventBus::new();
    let (_roles, _coves, write) = write_context();
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
}

#[tokio::test]
async fn wave_delete_cascades_refs_and_commits_but_leaves_objects() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let (roles, _coves, _write) = write_context();
    insert_raw_report_card(&repo, &roles, &wave.id).await;

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
    let write = WriteContext::new(roles.clone(), coves.clone());
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;

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
    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0].1, None);
    assert_eq!(commits[1].1.as_deref(), Some(commits[0].0.as_str()));
    assert_eq!(commits[2].1.as_deref(), Some(commits[1].0.as_str()));
    assert!(commits[2].2 > commits[1].2);
}

#[tokio::test]
async fn backfill_is_idempotent_and_uses_null_event_id_for_eventless_wave() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let (roles, _coves, _write) = write_context();
    insert_raw_report_card(&repo, &roles, &wave.id).await;

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
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
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
    assert_eq!(commits.len(), 2);
    assert_eq!(commits.last().unwrap().2, Some(*ids.last().unwrap()));
}

#[tokio::test]
async fn incremental_commit_changes_only_expected_wave_paths() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let wave_id = wave.id.clone();
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

#[tokio::test]
async fn card_added_commit_updates_index_markdown_card_count() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before");

    add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "terminal",
        CardRole::Worker,
        json!({"schemaVersion": 1}),
    )
    .await;
    let after = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head after");

    let diff = wave_vcs::diff(repo.pool(), &before, &after, None)
        .await
        .expect("diff");
    assert!(diff.iter().any(|entry| entry.path == "index.md"));
    let manifest = head_manifest(&repo, &wave.id).await;
    let index = manifest.entries.get("index.md").expect("index.md entry");
    let text = blob_text(&repo, &index.blob_hash).await;
    assert!(text.contains("- Cards: 2"));
}

#[tokio::test]
async fn manifest_blob_bytes_match_wave_fs_view_for_populated_wave() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "run-a", "goal": "check parity"}),
    )
    .await;

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::CodexWorkerRequested {
            idempotency_key: "run-a".into(),
            goal: "check parity".into(),
            context: json!({"source": "test"}),
            acceptance_criteria: Some("bytes match".into()),
        },
    )
    .await
    .expect("worker requested");
    start_codex_runtime_with_event(&repo, &bus, &write, &wave.id, &cove.id, &worker.id).await;
    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: worker.id.clone(),
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::CodexHook {
            card_id: worker.id.clone(),
            kind: "hook.codex.user_prompt_submit".into(),
            hook_idempotency_key: "hook-1".into(),
            payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": "hello"}),
        },
    )
    .await
    .expect("hook event");
    repo.log_pure_event(
        ActorId::KernelDispatcher,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::TaskCompleted {
            idempotency_key: "run-a".into(),
            result: json!({"summary": "done"}),
            artifacts: vec![],
        },
    )
    .await
    .expect("task completed");

    let manifest = head_manifest(&repo, &wave.id).await;
    let view = WaveFsView::new(&repo, &write);
    for (path, entry) in &manifest.entries {
        let vcs = blob_text(&repo, &entry.blob_hash).await;
        let fs = view.cat(&wave, path).await.expect(path);
        assert_eq!(vcs, fs.content, "path {path}");
    }
}

#[tokio::test]
async fn task_completion_updates_only_the_affected_run_paths() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    for key in ["one", "two"] {
        add_card_with_event(
            &repo,
            &bus,
            &roles,
            &write,
            &wave.id,
            &cove.id,
            "codex",
            CardRole::Worker,
            json!({"schemaVersion": 1, "idempotency_key": key}),
        )
        .await;
        repo.log_pure_event(
            ActorId::Kernel,
            EventScope::Wave {
                wave: wave.id.clone(),
                cove: cove.id.clone(),
            },
            None,
            &bus,
            &roles,
            &coves,
            Event::CodexWorkerRequested {
                idempotency_key: key.into(),
                goal: format!("run {key}"),
                context: json!({}),
                acceptance_criteria: None,
            },
        )
        .await
        .expect("request event");
    }
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before");

    repo.log_pure_event(
        ActorId::KernelDispatcher,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::TaskCompleted {
            idempotency_key: "one".into(),
            result: json!({"summary": "done"}),
            artifacts: vec![],
        },
    )
    .await
    .expect("completion");
    let after = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head after");
    let paths = wave_vcs::diff(repo.pool(), &before, &after, None)
        .await
        .expect("diff")
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec!["runs/index.json", "runs/one.json", "runs/one.md"]
    );
}

#[tokio::test]
async fn eventless_card_row_stays_hidden_until_card_added_event() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;

    let hidden_id = new_id();
    let mut tx = repo.pool().begin().await.expect("begin hidden insert");
    let hidden = card_create_with_id_tx(
        &mut tx,
        hidden_id.clone(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Worker,
        true,
        &roles,
    )
    .await
    .expect("insert hidden card row");
    tx.commit().await.expect("commit hidden row");

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::TaskCompleted {
            idempotency_key: "unrelated".into(),
            result: json!({"summary": "done"}),
            artifacts: vec![],
        },
    )
    .await
    .expect("unrelated event");
    let manifest = head_manifest(&repo, &wave.id).await;
    assert!(
        !manifest
            .entries
            .keys()
            .any(|path| path.starts_with(&format!("cards/{hidden_id}/")))
    );

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: hidden.id.clone(),
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::CardAdded(hidden),
    )
    .await
    .expect("CardAdded event");
    let manifest = head_manifest(&repo, &wave.id).await;
    assert!(
        manifest
            .entries
            .keys()
            .any(|path| path.starts_with(&format!("cards/{hidden_id}/")))
    );
}

#[tokio::test]
async fn cove_delete_cascades_wave_vcs_refs_and_commits() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    assert!(
        wave_vcs::head(repo.pool(), &wave.id)
            .await
            .unwrap()
            .is_some()
    );

    repo.cove_delete(cove.id.as_str())
        .await
        .expect("delete cove");
    assert_eq!(count_rows(repo.pool(), "wave_vcs_refs").await, 0);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_commits").await, 0);
    assert!(
        wave_vcs::head(repo.pool(), &wave.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn reserved_run_key_does_not_clobber_runs_index() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;

    repo.log_pure_event(
        ActorId::KernelDispatcher,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::TaskCompleted {
            idempotency_key: "index".into(),
            result: json!({"summary": "reserved"}),
            artifacts: vec![],
        },
    )
    .await
    .expect("reserved run event");

    let manifest = head_manifest(&repo, &wave.id).await;
    assert!(manifest.entries.contains_key("runs/index.json"));
    assert!(!manifest.entries.contains_key("runs/index.md"));
    let index = manifest.entries.get("runs/index.json").expect("runs index");
    assert_eq!(blob_text(&repo, &index.blob_hash).await, "[]");
}
