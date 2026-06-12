use std::collections::BTreeSet;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, card_update_tx, runtime_set_status_tx, runtime_start_tx,
    terminal_create_tx, wave_update_tx,
};
use calm_server::event::{Event, EventBus, EventScope, WaveUpdatedPayload};
use calm_server::harness::HarnessSnapshot;
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{
    Card, CardPatch, CardRole, NewCard, NewCove, NewTerminal, NewWave, WaveLifecycle, WavePatch,
    new_id, now_ms,
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
    add_card_with_id_with_event(
        repo,
        bus,
        roles,
        write,
        wave_id,
        cove_id,
        new_id(),
        kind,
        role,
        payload,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn add_card_with_id_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    roles: &CardRoleCache,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
    card_id: String,
    kind: &str,
    role: CardRole,
    payload: serde_json::Value,
) -> Card {
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

async fn update_card_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    card: &Card,
    cove_id: &CoveId,
    patch: CardPatch,
) -> Card {
    let card_id = card.id.clone();
    let lookup_card_id = card_id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        wave: card.wave_id.clone(),
        cove: cove_id.clone(),
    };
    repo.write_with_event(
        ActorId::Kernel,
        scope,
        None,
        bus,
        write,
        Box::new(move |tx| {
            let card_id = card_id.clone();
            let patch = patch.clone();
            Box::pin(async move {
                let card = card_update_tx(tx, card_id.as_str(), patch).await?;
                Ok(Event::CardUpdated(card))
            })
        }),
    )
    .await
    .expect("card updated event");

    repo.card_get(lookup_card_id.as_str())
        .await
        .expect("card lookup after update")
        .expect("updated card exists")
}

async fn update_wave_title_with_actor(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
    title: &str,
    actor: ActorId,
) {
    let wave_id_for_tx = wave_id.clone();
    let title = title.to_string();
    repo.write_with_event(
        actor,
        EventScope::Wave {
            wave: wave_id.clone(),
            cove: cove_id.clone(),
        },
        None,
        bus,
        write,
        Box::new(move |tx| {
            let wave_id = wave_id_for_tx.clone();
            let title = title.clone();
            Box::pin(async move {
                let updated = wave_update_tx(
                    tx,
                    wave_id.as_str(),
                    WavePatch {
                        title: Some(title),
                        ..WavePatch::default()
                    },
                )
                .await?;
                Ok(Event::WaveUpdated(WaveUpdatedPayload::new(updated, None)))
            })
        }),
    )
    .await
    .expect("wave title update event");
}

async fn insert_raw_card(
    repo: &SqlxRepo,
    roles: &CardRoleCache,
    wave_id: &WaveId,
    kind: &str,
    role: CardRole,
    payload: serde_json::Value,
) -> Card {
    let mut tx = repo.pool().begin().await.expect("begin raw card insert");
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave_id.clone(),
            kind: kind.into(),
            sort: None,
            payload,
        },
        role,
        !matches!(role, CardRole::ReportCard | CardRole::Spec),
        roles,
    )
    .await
    .expect("insert raw card");
    tx.commit().await.expect("commit raw card insert");
    card
}

async fn insert_raw_report_card(repo: &SqlxRepo, roles: &CardRoleCache, wave_id: &WaveId) -> Card {
    insert_raw_card(
        repo,
        roles,
        wave_id,
        "wave-report",
        CardRole::ReportCard,
        serde_json::to_value(WaveReportPayload::initial()).expect("report payload"),
    )
    .await
}

async fn start_codex_runtime_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
    card_id: &CardId,
) -> String {
    let runtime_id = new_id();
    let returned_runtime_id = runtime_id.clone();
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
    returned_runtime_id
}

#[allow(clippy::too_many_arguments)]
async fn set_runtime_status_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    wave_id: &WaveId,
    cove_id: &CoveId,
    card_id: &CardId,
    runtime_id: &str,
    old_status: RunStatus,
    new_status: RunStatus,
) {
    let runtime_id = runtime_id.to_string();
    let card_id_for_event = card_id.clone();
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
            let card_id = card_id_for_event.clone();
            let old_status = old_status.clone();
            let new_status = new_status.clone();
            Box::pin(async move {
                runtime_set_status_tx(tx, &runtime_id, new_status.clone()).await?;
                Ok(Event::RuntimeStatusChanged {
                    runtime_id,
                    card_id: card_id.to_string(),
                    old_status,
                    new_status,
                })
            })
        }),
    )
    .await
    .expect("runtime status changed event");
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

async fn vcs_object_hashes(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT hash FROM wave_vcs_objects ORDER BY hash")
        .fetch_all(pool)
        .await
        .expect("object hashes")
}

async fn set_all_vcs_objects_created_at(pool: &SqlitePool, created_at: i64) {
    sqlx::query("UPDATE wave_vcs_objects SET created_at = ?1")
        .bind(created_at)
        .execute(pool)
        .await
        .expect("age objects");
}

async fn set_vcs_object_created_at(pool: &SqlitePool, hash: &str, created_at: i64) {
    sqlx::query("UPDATE wave_vcs_objects SET created_at = ?1 WHERE hash = ?2")
        .bind(created_at)
        .bind(hash)
        .execute(pool)
        .await
        .expect("age object");
}

async fn vcs_object_exists(pool: &SqlitePool, hash: &str) -> bool {
    let exists: i64 =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wave_vcs_objects WHERE hash = ?1)")
            .bind(hash)
            .fetch_one(pool)
            .await
            .expect("object exists");
    exists != 0
}

fn old_vcs_object_timestamp() -> i64 {
    now_ms() - 2 * 60 * 60 * 1000
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

async fn live_wave_file_paths(
    view: &WaveFsView<'_>,
    wave: &calm_server::model::Wave,
) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        let entries = view
            .ls(
                wave,
                if dir.is_empty() {
                    None
                } else {
                    Some(dir.as_str())
                },
            )
            .await
            .expect("live ls");
        for entry in entries {
            let name = entry.name.trim_end_matches('/');
            let path = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            if entry.kind == "dir" {
                dirs.push(path);
            } else {
                files.insert(path);
            }
        }
    }
    files
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

async fn seed_head_payload_blob(
    repo: &SqlxRepo,
    wave_id: &WaveId,
    payload_path: &str,
    payload: serde_json::Value,
) -> String {
    let parent = wave_vcs::head(repo.pool(), wave_id)
        .await
        .expect("head query")
        .expect("head");
    let mut manifest = wave_vcs::tree_at(repo.pool(), &parent)
        .await
        .expect("tree query")
        .expect("tree");
    let payload_bytes = serde_json::to_vec(&payload).expect("legacy payload json");

    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin legacy payload seed");
    let blob_hash = wave_vcs::put_blob(&mut tx, "blob", &payload_bytes)
        .await
        .expect("put legacy payload blob");
    let entry = manifest
        .entries
        .get_mut(payload_path)
        .expect("payload entry");
    entry.blob_hash = blob_hash.clone();
    entry.byte_len = payload_bytes.len() as u64;
    entry.content_type = "application/json".into();
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest json");
    let tree_hash = format!("legacy-tree-{}", new_id());
    sqlx::query(
        r#"INSERT INTO wave_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'tree', ?2, ?3)"#,
    )
    .bind(&tree_hash)
    .bind(manifest_bytes)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .expect("insert legacy tree object");
    let tree = wave_vcs::TreeSnapshot {
        tree_hash,
        manifest,
    };
    wave_vcs::commit_tree(
        &mut tx,
        wave_id,
        Some(&parent),
        &tree,
        None,
        "legacy projected payload seed",
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit legacy payload seed");
    tx.commit().await.expect("commit legacy payload seed");
    blob_hash
}

async fn seed_legacy_card_lens_manifest(
    repo: &SqlxRepo,
    wave_id: &WaveId,
    card_id: &CardId,
) -> String {
    let parent = wave_vcs::head(repo.pool(), wave_id)
        .await
        .expect("head query")
        .expect("head");
    let mut manifest = wave_vcs::tree_at(repo.pool(), &parent)
        .await
        .expect("tree query")
        .expect("tree");

    for (new_leaf, legacy_leaf) in [
        (".meta.json", "meta.json"),
        (".payload.json", "payload.json"),
    ] {
        let new_path = format!("cards/{}/{new_leaf}", card_id.as_str());
        let legacy_path = format!("cards/{}/{legacy_leaf}", card_id.as_str());
        let entry = manifest
            .entries
            .remove(&new_path)
            .unwrap_or_else(|| panic!("missing {new_path}"));
        manifest.entries.insert(legacy_path, entry);
    }

    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest json");
    let tree_hash = format!("legacy-tree-{}", new_id());
    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin legacy manifest seed");
    sqlx::query(
        r#"INSERT INTO wave_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'tree', ?2, ?3)"#,
    )
    .bind(&tree_hash)
    .bind(manifest_bytes)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .expect("insert legacy tree object");
    let tree = wave_vcs::TreeSnapshot {
        tree_hash,
        manifest,
    };
    let legacy_head = wave_vcs::commit_tree(
        &mut tx,
        wave_id,
        Some(&parent),
        &tree,
        None,
        "legacy card lens path seed",
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit legacy manifest seed");
    tx.commit().await.expect("commit legacy manifest seed");
    legacy_head
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

#[tokio::test]
async fn next_commit_after_legacy_card_lens_manifest_rewrites_dotfile_paths() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "terminal",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "legacy-paths"}),
    )
    .await;
    let legacy_head = seed_legacy_card_lens_manifest(&repo, &wave.id, &worker.id).await;
    let legacy_manifest = wave_vcs::tree_at(repo.pool(), &legacy_head)
        .await
        .expect("legacy tree query")
        .expect("legacy tree");

    let legacy_meta_path = format!("cards/{}/meta.json", worker.id.as_str());
    let legacy_payload_path = format!("cards/{}/payload.json", worker.id.as_str());
    let meta_path = format!("cards/{}/.meta.json", worker.id.as_str());
    let payload_path = format!("cards/{}/.payload.json", worker.id.as_str());
    assert!(legacy_manifest.entries.contains_key(&legacy_meta_path));
    assert!(legacy_manifest.entries.contains_key(&legacy_payload_path));
    assert!(!legacy_manifest.entries.contains_key(&meta_path));
    assert!(!legacy_manifest.entries.contains_key(&payload_path));

    update_wave_title_with_actor(
        &repo,
        &bus,
        &write,
        &wave.id,
        &cove.id,
        "post cutover",
        ActorId::User,
    )
    .await;

    let manifest = head_manifest(&repo, &wave.id).await;
    assert!(manifest.entries.contains_key(&meta_path));
    assert!(manifest.entries.contains_key(&payload_path));
    assert!(!manifest.entries.contains_key(&legacy_meta_path));
    assert!(!manifest.entries.contains_key(&legacy_payload_path));

    let legacy_manifest_after = wave_vcs::tree_at(repo.pool(), &legacy_head)
        .await
        .expect("legacy tree query after")
        .expect("legacy tree after");
    assert_eq!(legacy_manifest_after, legacy_manifest);
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
                        agent_message: None,
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
async fn actor_event_batch_writes_wave_vcs_commit_with_lifecycle_and_verdict() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let spec = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "spec",
        CardRole::Spec,
        json!({"schemaVersion": 1}),
    )
    .await;

    let before_commits = wave_commit_rows(&repo, wave.id.as_str()).await;
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: cove.id.clone(),
    };
    let wave_id = wave.id.clone();
    let spec_actor = ActorId::AiSpec(spec.id.clone());
    let event_ids = repo
        .write_with_actor_events(
            None,
            &bus,
            &write,
            Box::new(move |tx| {
                let scope = scope.clone();
                let wave_id = wave_id.clone();
                let spec_actor = spec_actor.clone();
                Box::pin(async move {
                    let mut events = Vec::new();
                    if let Some(auto_events) =
                        calm_server::wave_lifecycle::auto_promote_draft_in_tx(tx, &wave_id).await?
                    {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::Kernel, scope.clone(), event)),
                        );
                    }
                    if let Some(lifecycle_events) =
                        calm_server::wave_lifecycle::apply_requested_transition_in_tx(
                            tx,
                            &wave_id,
                            WaveLifecycle::Dispatching,
                            &spec_actor,
                            "dispatch accepted work".into(),
                        )
                        .await?
                    {
                        events.extend(
                            lifecycle_events
                                .into_iter()
                                .map(|event| (spec_actor.clone(), scope.clone(), event)),
                        );
                    }
                    events.push((
                        spec_actor,
                        scope,
                        Event::TaskCompleted {
                            idempotency_key: "actor-batch-verdict".into(),
                            result: json!({
                                "status": "accepted",
                                "reason": "verified",
                            }),
                            artifacts: vec![],
                            agent_message: Some("accept worker result".into()),
                        },
                    ));
                    Ok(events)
                })
            }),
        )
        .await
        .expect("actor event batch");
    assert_eq!(event_ids.len(), 5);

    let after_commits = wave_commit_rows(&repo, wave.id.as_str()).await;
    assert_eq!(after_commits.len(), before_commits.len() + 1);
    let latest = after_commits.last().expect("latest commit");
    assert_eq!(
        latest.1.as_deref(),
        before_commits.last().map(|row| row.0.as_str())
    );
    assert_eq!(latest.2, event_ids.last().copied());

    let commit = sqlx::query(
        r#"SELECT event_id, lifecycle, message
           FROM wave_vcs_commits
           WHERE hash = ?1"#,
    )
    .bind(&latest.0)
    .fetch_one(repo.pool())
    .await
    .expect("latest commit row");
    assert_eq!(
        commit.try_get::<Option<i64>, _>("event_id").unwrap(),
        event_ids.last().copied()
    );
    assert_eq!(
        commit.try_get::<String, _>("lifecycle").unwrap(),
        "dispatching"
    );
    assert_eq!(
        commit.try_get::<Option<String>, _>("message").unwrap(),
        Some("task.completed".into())
    );

    let rows = sqlx::query(
        r#"SELECT id, kind
           FROM events
           WHERE id >= ?1 AND id <= ?2
           ORDER BY id ASC"#,
    )
    .bind(event_ids[0])
    .bind(*event_ids.last().unwrap())
    .fetch_all(repo.pool())
    .await
    .expect("batch event rows");
    let batch = rows
        .into_iter()
        .map(|row| {
            (
                row.try_get::<i64, _>("id").unwrap(),
                row.try_get::<String, _>("kind").unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        batch,
        vec![
            (event_ids[0], "wave.lifecycle_changed".into()),
            (event_ids[1], "wave.updated".into()),
            (event_ids[2], "wave.lifecycle_changed".into()),
            (event_ids[3], "wave.updated".into()),
            (event_ids[4], "task.completed".into()),
        ]
    );

    let manifest = head_manifest(&repo, &wave.id).await;
    let wave_entry = manifest.entries.get("wave.json").expect("wave json");
    let wave_json: serde_json::Value =
        serde_json::from_str(&blob_text(&repo, &wave_entry.blob_hash).await).unwrap();
    assert_eq!(
        wave_json
            .get("lifecycle")
            .and_then(serde_json::Value::as_str),
        Some("dispatching")
    );

    let run_entry = manifest
        .entries
        .get("runs/actor-batch-verdict.json")
        .expect("verdict run json");
    let run_json: serde_json::Value =
        serde_json::from_str(&blob_text(&repo, &run_entry.blob_hash).await).unwrap();
    assert_eq!(
        run_json
            .pointer("/events/verdict/event_id")
            .and_then(serde_json::Value::as_i64),
        event_ids.last().copied()
    );
    assert_eq!(
        run_json
            .pointer("/events/verdict/kind")
            .and_then(serde_json::Value::as_str),
        Some("task.completed")
    );
    assert_eq!(
        run_json
            .pointer("/verdict/status")
            .and_then(serde_json::Value::as_str),
        Some("accepted")
    );
    assert_eq!(
        run_json
            .pointer("/verdict/reason")
            .and_then(serde_json::Value::as_str),
        Some("verified")
    );
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
async fn object_sweep_deletes_old_orphans_but_keeps_fresh_ones() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let object_hashes = vcs_object_hashes(repo.pool()).await;
    assert!(object_hashes.len() > 1);
    let fresh_hash = object_hashes[0].clone();

    repo.wave_delete(wave.id.as_str())
        .await
        .expect("delete wave");
    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;
    set_vcs_object_created_at(repo.pool(), &fresh_hash, now_ms()).await;

    let deleted = wave_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep objects");

    assert_eq!(deleted, (object_hashes.len() - 1) as u64);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_objects").await, 1);
    assert!(vcs_object_exists(repo.pool(), &fresh_hash).await);
}

#[tokio::test]
async fn object_sweep_keeps_objects_referenced_by_live_commits() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let before = count_rows(repo.pool(), "wave_vcs_objects").await;
    assert!(before > 0);

    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;
    let deleted = wave_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep objects");

    assert_eq!(deleted, 0);
    assert_eq!(count_rows(repo.pool(), "wave_vcs_objects").await, before);
}

#[tokio::test]
async fn object_sweep_reports_corrupt_tree_object_hash() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;

    let tree_hash: String = sqlx::query_scalar(
        r#"SELECT tree_hash
           FROM wave_vcs_commits
           WHERE wave_id = ?1
           ORDER BY created_at DESC
           LIMIT 1"#,
    )
    .bind(wave.id.as_str())
    .fetch_one(repo.pool())
    .await
    .expect("tree hash");

    sqlx::query("UPDATE wave_vcs_objects SET bytes = ?1 WHERE hash = ?2")
        .bind(b"not-json".to_vec())
        .bind(&tree_hash)
        .execute(repo.pool())
        .await
        .expect("corrupt tree object");

    let err = wave_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect_err("corrupt tree manifest fails closed");

    assert!(
        err.to_string().contains(&tree_hash),
        "error should include corrupt tree object hash: {err}"
    );
}

#[tokio::test]
async fn object_sweep_keeps_blob_shared_by_deleted_and_live_waves() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let deleted_wave = make_wave(&repo, cove.id.as_str()).await;
    let live_wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &deleted_wave.id, &cove.id).await;
    add_report_card(&repo, &bus, &roles, &write, &live_wave.id, &cove.id).await;

    let deleted_manifest = head_manifest(&repo, &deleted_wave.id).await;
    let live_manifest = head_manifest(&repo, &live_wave.id).await;
    let deleted_blobs = deleted_manifest
        .entries
        .values()
        .map(|entry| entry.blob_hash.clone())
        .collect::<BTreeSet<_>>();
    let live_blobs = live_manifest
        .entries
        .values()
        .map(|entry| entry.blob_hash.clone())
        .collect::<BTreeSet<_>>();
    let shared_blob = deleted_blobs
        .intersection(&live_blobs)
        .next()
        .expect("shared blob")
        .clone();

    repo.wave_delete(deleted_wave.id.as_str())
        .await
        .expect("delete wave");
    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;
    let deleted = wave_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep objects");

    assert!(deleted > 0);
    assert!(vcs_object_exists(repo.pool(), &shared_blob).await);
    assert!(
        wave_vcs::tree_at(
            repo.pool(),
            &wave_vcs::head(repo.pool(), &live_wave.id)
                .await
                .expect("live head")
                .expect("live head exists")
        )
        .await
        .expect("live tree")
        .is_some()
    );
}

#[tokio::test]
async fn object_sweep_smoke_serializes_with_concurrent_event_write() {
    let (_dir, repo) = fresh_file_repo().await;
    let cove = make_cove(&repo).await;
    let live_wave = make_wave(&repo, cove.id.as_str()).await;
    let deleted_wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &live_wave.id, &cove.id).await;
    add_report_card(&repo, &bus, &roles, &write, &deleted_wave.id, &cove.id).await;
    repo.wave_delete(deleted_wave.id.as_str())
        .await
        .expect("delete wave");
    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;

    let sweep_repo = repo.clone();
    let write_repo = repo.clone();
    let write_bus = bus.clone();
    let write_context = write.clone();
    let live_wave_id = live_wave.id.clone();
    let live_cove_id = cove.id.clone();
    let update_wave_id = live_wave.id.clone();

    let sweep = tokio::spawn(async move {
        wave_vcs::sweep_unreferenced_objects_once(sweep_repo.pool())
            .await
            .expect("sweep objects")
    });
    let write = tokio::spawn(async move {
        write_repo
            .write_with_event(
                ActorId::User,
                EventScope::Wave {
                    wave: live_wave_id,
                    cove: live_cove_id,
                },
                None,
                &write_bus,
                &write_context,
                Box::new(move |tx| {
                    let update_wave_id = update_wave_id.clone();
                    Box::pin(async move {
                        let updated = wave_update_tx(
                            tx,
                            update_wave_id.as_str(),
                            WavePatch {
                                title: Some("updated during sweep".into()),
                                ..WavePatch::default()
                            },
                        )
                        .await?;
                        Ok(Event::WaveUpdated(WaveUpdatedPayload::new(updated, None)))
                    })
                }),
            )
            .await
            .expect("write event")
    });
    let (deleted, event_id) = tokio::join!(sweep, write);

    assert!(deleted.expect("sweep join") > 0);
    assert!(event_id.expect("write join") > 0);
    assert!(
        wave_vcs::head(repo.pool(), &live_wave.id)
            .await
            .expect("live head")
            .is_some()
    );
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
                    agent_message: None,
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
async fn backfilled_eventless_cards_survive_incremental_index_rerenders() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, _write) = write_context();
    insert_raw_report_card(&repo, &roles, &wave.id).await;
    let worker = insert_raw_card(
        &repo,
        &roles,
        &wave.id,
        "terminal",
        CardRole::Worker,
        json!({"schemaVersion": 1, "label": "legacy"}),
    )
    .await;

    assert_eq!(
        wave_vcs::backfill_existing_waves(repo.pool())
            .await
            .unwrap(),
        1
    );
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
        Event::CardUpdated(worker.clone()),
    )
    .await
    .expect("legacy card update event");

    let manifest = head_manifest(&repo, &wave.id).await;
    assert!(
        manifest
            .entries
            .contains_key(&format!("cards/{}/.meta.json", worker.id.as_str())),
        "backfilled card path disappeared from manifest"
    );

    let cards_index = manifest
        .entries
        .get("cards/index.json")
        .expect("cards index");
    let cards: Vec<serde_json::Value> =
        serde_json::from_str(&blob_text(&repo, &cards_index.blob_hash).await).unwrap();
    assert!(
        cards
            .iter()
            .any(|card| card.get("id").and_then(|id| id.as_str()) == Some(worker.id.as_str())),
        "cards/index.json = {cards:?}"
    );

    let index = manifest.entries.get("index.md").expect("index.md");
    let index_md = blob_text(&repo, &index.blob_hash).await;
    assert!(index_md.contains("- Cards: 2"), "index.md = {index_md}");
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
                            Event::WaveUpdated(WaveUpdatedPayload::new(updated, None)),
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
                                agent_message: None,
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
    let latest = commits.last().unwrap();
    assert_eq!(latest.2, Some(*ids.last().unwrap()));
    let author: Option<String> =
        sqlx::query_scalar("SELECT author FROM wave_vcs_commits WHERE hash = ?1")
            .bind(&latest.0)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(author.as_deref(), Some("user"));
}

#[tokio::test]
async fn mixed_actor_batch_commit_is_unattributed_in_diff_block() {
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
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "mixed-actor-batch"}),
    )
    .await;
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before mixed batch");
    let wave_id = wave.id.clone();
    let cove_id = cove.id.clone();

    repo.write_with_actor_events(
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
                        title: Some("mixed actor title".into()),
                        ..WavePatch::default()
                    },
                )
                .await?;
                Ok(vec![
                    (
                        ActorId::User,
                        EventScope::Wave {
                            wave: wave_id.clone(),
                            cove: cove_id.clone(),
                        },
                        Event::WaveUpdated(WaveUpdatedPayload::new(updated, None)),
                    ),
                    (
                        ActorId::Kernel,
                        EventScope::Wave {
                            wave: wave_id,
                            cove: cove_id,
                        },
                        Event::TaskCompleted {
                            idempotency_key: "mixed-actor-batch".into(),
                            result: json!({"status": "accepted"}),
                            artifacts: vec![],
                            agent_message: None,
                        },
                    ),
                ])
            })
        }),
    )
    .await
    .expect("mixed actor batch");

    let after = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head after mixed batch");
    let author: Option<String> =
        sqlx::query_scalar("SELECT author FROM wave_vcs_commits WHERE hash = ?1")
            .bind(&after)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(author, None);

    let block = wave_vcs::since_last_turn_block(repo.pool(), &wave.id, Some(&before), None)
        .await
        .unwrap()
        .block
        .expect("diff block");
    assert!(block.contains("wave.json edited"), "block = {block}");
    assert!(
        block.contains("runs/mixed-actor-batch.json edited"),
        "block = {block}"
    );
    assert!(
        !block.contains("(by "),
        "mixed actor commit should not render an attribution suffix: {block}"
    );
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
                Ok(Event::WaveUpdated(WaveUpdatedPayload::new(updated, None)))
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
    let root_diff = wave_vcs::diff(repo.pool(), &first, &second, Some("/"))
        .await
        .expect("root diff");
    assert_eq!(root_diff, diff);

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
            agent_message: None,
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
    let hook_head = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head after hook");
    let hook_author: Option<String> =
        sqlx::query_scalar("SELECT author FROM wave_vcs_commits WHERE hash = ?1")
            .bind(&hook_head)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(hook_author.as_deref(), Some("kernel"));

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
            agent_message: None,
        },
    )
    .await
    .expect("task completed");

    let manifest = head_manifest(&repo, &wave.id).await;
    let view = WaveFsView::new(&repo, &write);
    let manifest_paths = manifest.entries.keys().cloned().collect::<BTreeSet<_>>();
    let live_paths = live_wave_file_paths(&view, &wave).await;
    assert_eq!(manifest_paths, live_paths);

    for (path, entry) in &manifest.entries {
        let vcs = blob_text(&repo, &entry.blob_hash).await;
        let fs = view.cat(&wave, path).await.expect(path);
        assert_eq!(vcs, fs.content, "path {path}");
    }
}

#[tokio::test]
async fn card_retarget_from_wave_report_removes_report_blob() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    let report = add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let before_head = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before retarget");
    let before = head_manifest(&repo, &wave.id).await;
    assert!(before.entries.contains_key("report.md"));

    update_card_with_event(
        &repo,
        &bus,
        &write,
        &report,
        &cove.id,
        CardPatch {
            kind: Some("terminal".into()),
            sort: None,
            payload: Some(json!({"schemaVersion": 1})),
            deletable: None,
        },
    )
    .await;

    let after = head_manifest(&repo, &wave.id).await;
    assert!(!after.entries.contains_key("report.md"));
    let block = wave_vcs::since_last_turn_block(repo.pool(), &wave.id, Some(&before_head), None)
        .await
        .unwrap()
        .block
        .expect("diff block");
    assert!(block.contains("report.md deleted"), "block = {block}");
    assert!(
        !block.contains("report.md deleted (unified patch follows)"),
        "deleted report should not advertise an inline hunk: {block}"
    );
    assert!(
        !block.contains("--- a/report.md"),
        "deleted report should not include a full-content patch: {block}"
    );
}

#[tokio::test]
async fn since_last_turn_report_diff_uses_dynamic_fence_for_markdown_code_blocks() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    let report = add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let payload = |body: &str| {
        serde_json::to_value(WaveReportPayload {
            schema_version: WaveReportPayload::SCHEMA_VERSION,
            summary: String::new(),
            body: body.to_string(),
        })
        .expect("report payload")
    };
    let old_body = "# Goal\n\n```text\nstable\n```\n\nold line\n";
    let new_body = "# Goal\n\n```text\nstable\n```\n\nnew line\n";
    let report = update_card_with_event(
        &repo,
        &bus,
        &write,
        &report,
        &cove.id,
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload(old_body)),
            deletable: None,
        },
    )
    .await;
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before report edit");

    update_card_with_event(
        &repo,
        &bus,
        &write,
        &report,
        &cove.id,
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload(new_body)),
            deletable: None,
        },
    )
    .await;
    let after = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head after report edit");

    let since = wave_vcs::since_last_turn_block(repo.pool(), &wave.id, Some(&before), None)
        .await
        .unwrap();
    assert_eq!(since.current_head.as_deref(), Some(after.as_str()));
    let block = since.block.expect("diff block");
    assert!(
        block.contains("report.md edited (by kernel) (unified patch follows)"),
        "block = {block}"
    );
    assert!(
        block.contains("````diff\n--- a/report.md"),
        "dynamic fence should grow beyond the triple backtick run: {block}"
    );
    assert_eq!(
        block.lines().filter(|line| *line == "````diff").count(),
        1,
        "block = {block}"
    );
    assert_eq!(
        block.lines().filter(|line| *line == "````").count(),
        1,
        "block = {block}"
    );
    let diff_start = block.find("````diff\n").expect("opening fence") + "````diff\n".len();
    let diff_end = diff_start + block[diff_start..].find("\n````\n").expect("closing fence");
    let diff_body = &block[diff_start..diff_end];
    assert!(
        diff_body.contains("\n ```\n"),
        "diff should contain the markdown code fence context line: {block}"
    );
    assert!(
        diff_body.contains("\n-old line\n+new line"),
        "diff should contain the report edit hunk: {block}"
    );
}

#[tokio::test]
async fn since_last_turn_range_over_bound_falls_back_without_attribution() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before updates");

    for i in 0..=50 {
        update_wave_title_with_actor(
            &repo,
            &bus,
            &write,
            &wave.id,
            &cove.id,
            &format!("title-{i}"),
            ActorId::User,
        )
        .await;
    }

    let block = wave_vcs::since_last_turn_block(repo.pool(), &wave.id, Some(&before), None)
        .await
        .unwrap()
        .block
        .expect("diff block");
    assert!(block.contains("index.md edited"), "block = {block}");
    assert!(
        !block.contains("(by "),
        "over-bound range should use old unattributed rendering: {block}"
    );
}

#[tokio::test]
async fn since_last_turn_legacy_null_author_commit_has_no_suffix() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &wave.id, &cove.id).await;
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before update");

    update_wave_title_with_actor(
        &repo,
        &bus,
        &write,
        &wave.id,
        &cove.id,
        "legacy-null-author",
        ActorId::User,
    )
    .await;
    let after = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head after update");
    sqlx::query("UPDATE wave_vcs_commits SET author = NULL WHERE hash = ?1")
        .bind(&after)
        .execute(repo.pool())
        .await
        .unwrap();

    let block = wave_vcs::since_last_turn_block(repo.pool(), &wave.id, Some(&before), None)
        .await
        .unwrap()
        .block
        .expect("diff block");
    assert!(block.contains("index.md edited"), "block = {block}");
    assert!(
        !block.contains("(by "),
        "NULL legacy author should not render an attribution suffix: {block}"
    );
}

#[tokio::test]
async fn duplicate_run_key_uses_shared_card_order_for_delta_and_snapshot() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    let high_id = add_card_with_id_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "worker-z-card".into(),
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "dup-key", "name": "high-id"}),
    )
    .await;
    let low_id = add_card_with_id_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "worker-a-card".into(),
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "dup-key", "name": "low-id"}),
    )
    .await;
    let high_id = update_card_with_event(
        &repo,
        &bus,
        &write,
        &high_id,
        &cove.id,
        CardPatch {
            kind: None,
            sort: Some(1.0),
            payload: None,
            deletable: None,
        },
    )
    .await;
    let low_id = update_card_with_event(
        &repo,
        &bus,
        &write,
        &low_id,
        &cove.id,
        CardPatch {
            kind: None,
            sort: Some(1.0),
            payload: None,
            deletable: None,
        },
    )
    .await;
    let expected_ids = vec![low_id.id.to_string(), high_id.id.to_string()];

    let live_ids = repo
        .cards_by_wave(wave.id.as_str())
        .await
        .expect("cards by wave")
        .into_iter()
        .map(|card| card.id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(live_ids, expected_ids);

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
            idempotency_key: "dup-key".into(),
            goal: "duplicate key".into(),
            context: json!({}),
            acceptance_criteria: None,
            agent_message: None,
        },
    )
    .await
    .expect("request event");

    let manifest = head_manifest(&repo, &wave.id).await;
    let cards_index_entry = manifest
        .entries
        .get("cards/index.json")
        .expect("cards index");
    let cards_index: Vec<serde_json::Value> =
        serde_json::from_str(&blob_text(&repo, &cards_index_entry.blob_hash).await).unwrap();
    let manifest_card_ids = cards_index
        .iter()
        .map(|card| card["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(manifest_card_ids, expected_ids);

    let run_entry = manifest.entries.get("runs/dup-key.json").expect("run json");
    let run_json: serde_json::Value =
        serde_json::from_str(&blob_text(&repo, &run_entry.blob_hash).await).unwrap();
    assert_eq!(
        run_json["worker_card_id"].as_str(),
        Some(expected_ids[0].as_str())
    );

    let mut tx = repo.pool().begin().await.expect("begin snapshot");
    let snapshot = wave_vcs::snapshot_tree(&mut tx, &wave.id, MANIFEST_SCHEMA_VERSION)
        .await
        .expect("snapshot");
    tx.rollback().await.expect("rollback snapshot");
    assert_eq!(snapshot.manifest, manifest);
}

#[tokio::test]
async fn superseded_only_runtime_payload_matches_live_view_without_runtime_fields() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "superseded-only"}),
    )
    .await;

    let mut tx = repo.pool().begin().await.expect("begin runtime");
    let runtime = runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: new_id(),
            card_id: worker.id.to_string(),
            kind: RuntimeKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Running,
            terminal_run_id: None,
            thread_id: Some("stale-thread".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("runtime start");
    let superseded_at = now_ms();
    sqlx::query(
        r#"UPDATE runtimes
              SET status = 'superseded',
                  updated_at_ms = ?1,
                  completed_at_ms = ?1
            WHERE id = ?2"#,
    )
    .bind(superseded_at)
    .bind(&runtime.id)
    .execute(&mut *tx)
    .await
    .expect("mark superseded");
    tx.commit().await.expect("commit runtime");

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
        Event::RuntimeSuperseded {
            old_runtime_id: runtime.id,
            new_runtime_id: "missing-replacement".into(),
            card_id: worker.id.to_string(),
        },
    )
    .await
    .expect("runtime superseded event");

    let manifest = head_manifest(&repo, &wave.id).await;
    let payload_path = format!("cards/{}/.payload.json", worker.id.as_str());
    let entry = manifest.entries.get(&payload_path).expect("payload entry");
    let vcs_payload = blob_text(&repo, &entry.blob_hash).await;
    let view = WaveFsView::new(&repo, &write);
    let live_payload = view.cat(&wave, &payload_path).await.expect("live payload");
    assert_eq!(vcs_payload, live_payload.content);

    let payload: serde_json::Value = serde_json::from_str(&vcs_payload).unwrap();
    assert!(payload.get("codex_thread_id").is_none(), "{payload:?}");
    assert!(payload.get("codex_thread_status").is_none(), "{payload:?}");
}

#[tokio::test]
async fn spec_runtime_payload_blob_matches_live_view_without_projected_fields() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, coves, write) = write_context();
    let spec = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "codex",
        CardRole::Spec,
        json!({"schemaVersion": 1, "spec_harness": true}),
    )
    .await;

    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.last_thread_id = Some("spec-thread".into());
    let (runtime, terminal_id) = {
        let mut tx = repo.pool().begin().await.expect("begin runtime");
        let terminal = terminal_create_tx(
            &mut tx,
            NewTerminal {
                card_id: spec.id.clone(),
                program: "codex".into(),
                cwd: "/tmp".into(),
                env: json!({}),
                theme: RequestTheme::default_dark(),
            },
        )
        .await
        .expect("terminal create");
        let terminal_id = terminal.id.clone();
        let runtime = runtime_start_tx(
            &mut tx,
            RuntimeInit {
                id: new_id(),
                card_id: spec.id.to_string(),
                kind: RuntimeKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: RunStatus::Running,
                terminal_run_id: Some(terminal_id.clone()),
                thread_id: Some("spec-thread".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
                lease_owner: None,
                lease_until_ms: None,
                now_ms: now_ms(),
            },
        )
        .await
        .expect("runtime start");
        tx.commit().await.expect("commit runtime");
        (runtime, terminal_id)
    };

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: spec.id.clone(),
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &bus,
        &roles,
        &coves,
        Event::RuntimeStarted {
            runtime_id: runtime.id,
            card_id: runtime.card_id,
            kind: runtime.kind,
            agent_provider: runtime.agent_provider,
            status: runtime.status,
        },
    )
    .await
    .expect("runtime started event");

    let manifest = head_manifest(&repo, &wave.id).await;
    let payload_path = format!("cards/{}/.payload.json", spec.id.as_str());
    let entry = manifest.entries.get(&payload_path).expect("payload entry");
    let vcs_payload = blob_text(&repo, &entry.blob_hash).await;
    let view = WaveFsView::new(&repo, &write);
    let live_payload = view.cat(&wave, &payload_path).await.expect("live payload");
    assert_eq!(vcs_payload, live_payload.content);

    let payload: serde_json::Value = serde_json::from_str(&vcs_payload).unwrap();
    assert!(payload.get("codex_thread_id").is_none(), "{payload:?}");
    assert!(payload.get("codex_source").is_none(), "{payload:?}");
    assert!(payload.get("codex_thread_status").is_none(), "{payload:?}");
    assert!(payload.get("terminal_id").is_none(), "{payload:?}");

    let runtime_path = format!("cards/{}/runtime.json", spec.id.as_str());
    let entry = manifest.entries.get(&runtime_path).expect("runtime entry");
    let vcs_runtime = blob_text(&repo, &entry.blob_hash).await;
    let live_runtime = view.cat(&wave, &runtime_path).await.expect("live runtime");
    assert_eq!(vcs_runtime, live_runtime.content);

    let runtime: serde_json::Value = serde_json::from_str(&vcs_runtime).unwrap();
    assert_eq!(runtime["terminal_id"], terminal_id);
    assert_eq!(runtime["thread_id"], "spec-thread");
    assert_eq!(runtime["source"], "shared");
    assert_eq!(runtime["thread_status"], "started");
}

#[tokio::test]
async fn runtime_event_heals_legacy_projected_payload_blob_once() {
    let repo = fresh_repo().await;
    let cove = make_cove(&repo).await;
    let wave = make_wave(&repo, cove.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _coves, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &wave.id,
        &cove.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "legacy-heal", "goal": "heal"}),
    )
    .await;
    let runtime_id =
        start_codex_runtime_with_event(&repo, &bus, &write, &wave.id, &cove.id, &worker.id).await;
    let payload_path = format!("cards/{}/.payload.json", worker.id.as_str());

    let legacy_hash = seed_head_payload_blob(
        &repo,
        &wave.id,
        &payload_path,
        json!({
            "schemaVersion": 1,
            "idempotency_key": "legacy-heal",
            "goal": "heal",
            "terminal_id": "legacy-terminal",
            "codex_thread_id": "legacy-thread",
            "codex_thread_status": "started"
        }),
    )
    .await;
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .unwrap();
    set_runtime_status_with_event(
        &repo,
        &bus,
        &write,
        &wave.id,
        &cove.id,
        &worker.id,
        &runtime_id,
        RunStatus::Running,
        RunStatus::Idle,
    )
    .await;
    let after_heal = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .unwrap();

    let diff = wave_vcs::diff(repo.pool(), &before, &after_heal, None)
        .await
        .expect("heal diff");
    let payload_entries = diff
        .iter()
        .filter(|entry| entry.path == payload_path)
        .collect::<Vec<_>>();
    assert_eq!(payload_entries.len(), 1, "diff = {diff:?}");
    assert_eq!(payload_entries[0].status, DiffStatus::Modified);
    assert_eq!(
        payload_entries[0].old_hash.as_deref(),
        Some(legacy_hash.as_str())
    );

    let block = wave_vcs::since_last_turn_block(repo.pool(), &wave.id, Some(&before), None)
        .await
        .expect("since-last-turn block")
        .block
        .expect("payload heal block");
    let payload_line = format!("- {payload_path} edited (by kernel)\n");
    assert_eq!(block.matches(&payload_line).count(), 1, "{block}");

    let healed_manifest = head_manifest(&repo, &wave.id).await;
    let healed_entry = healed_manifest
        .entries
        .get(&payload_path)
        .expect("healed payload entry");
    let healed_payload: serde_json::Value =
        serde_json::from_str(&blob_text(&repo, &healed_entry.blob_hash).await).unwrap();
    assert!(
        healed_payload.get("terminal_id").is_none(),
        "{healed_payload:?}"
    );
    assert!(
        healed_payload.get("codex_thread_id").is_none(),
        "{healed_payload:?}"
    );
    assert!(
        healed_payload.get("codex_thread_status").is_none(),
        "{healed_payload:?}"
    );
    set_runtime_status_with_event(
        &repo,
        &bus,
        &write,
        &wave.id,
        &cove.id,
        &worker.id,
        &runtime_id,
        RunStatus::Idle,
        RunStatus::Running,
    )
    .await;
    let after_second_runtime_event = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .unwrap();
    let second_diff = wave_vcs::diff(repo.pool(), &after_heal, &after_second_runtime_event, None)
        .await
        .expect("second runtime diff");
    assert!(
        second_diff.iter().all(|entry| entry.path != payload_path),
        "payload heal should be one-time: {second_diff:?}"
    );
}

#[tokio::test]
async fn runtime_status_flip_does_not_change_run_json_bytes() {
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
        json!({
            "schemaVersion": 1,
            "idempotency_key": "runtime-flip",
            "prompt": "raw prompt"
        }),
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
            idempotency_key: "runtime-flip".into(),
            goal: "runtime must not project into run payload".into(),
            context: json!({}),
            acceptance_criteria: None,
            agent_message: None,
        },
    )
    .await
    .expect("request event");
    let runtime_id =
        start_codex_runtime_with_event(&repo, &bus, &write, &wave.id, &cove.id, &worker.id).await;

    let run_path = "runs/runtime-flip.json";
    let before = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head before runtime status flip");
    let before_manifest = head_manifest(&repo, &wave.id).await;
    let before_run_entry = before_manifest
        .entries
        .get(run_path)
        .expect("run json before runtime status flip");
    let before_run_json = blob_text(&repo, &before_run_entry.blob_hash).await;

    set_runtime_status_with_event(
        &repo,
        &bus,
        &write,
        &wave.id,
        &cove.id,
        &worker.id,
        &runtime_id,
        RunStatus::Running,
        RunStatus::Failed,
    )
    .await;

    let after = wave_vcs::head(repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("head after runtime status flip");
    let paths = wave_vcs::diff(repo.pool(), &before, &after, None)
        .await
        .expect("diff")
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert!(
        !paths.iter().any(|path| path == run_path),
        "runtime-only status flip must not diff {run_path}: {paths:?}"
    );

    let after_manifest = head_manifest(&repo, &wave.id).await;
    let after_run_entry = after_manifest
        .entries
        .get(run_path)
        .expect("run json after runtime status flip");
    let after_run_json = blob_text(&repo, &after_run_entry.blob_hash).await;
    assert_eq!(after_run_json, before_run_json);

    let run: serde_json::Value = serde_json::from_str(&after_run_json).unwrap();
    assert_eq!(run["status"], "running");
    assert_eq!(run["worker_card_payload"]["prompt"], "raw prompt");
    assert!(
        run["worker_card_payload"]
            .get("codex_thread_status")
            .is_none(),
        "worker_card_payload must stay raw: {run:?}"
    );
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
                agent_message: None,
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
            agent_message: None,
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

    let manifest = head_manifest(&repo, &wave.id).await;
    let run_entry = manifest
        .entries
        .get("runs/one.json")
        .expect("completed run json");
    let run_json: serde_json::Value =
        serde_json::from_str(&blob_text(&repo, &run_entry.blob_hash).await).unwrap();
    assert_eq!(run_json["status"], "completed");
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
            payload: json!({"schemaVersion": 1, "idempotency_key": "hidden-run"}),
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
        Event::CodexWorkerRequested {
            idempotency_key: "hidden-run".into(),
            goal: "hidden worker".into(),
            context: json!({}),
            acceptance_criteria: None,
            agent_message: None,
        },
    )
    .await
    .expect("request event");
    let manifest = head_manifest(&repo, &wave.id).await;
    assert!(
        !manifest
            .entries
            .keys()
            .any(|path| path.starts_with(&format!("cards/{hidden_id}/")))
    );
    let run_entry = manifest
        .entries
        .get("runs/hidden-run.json")
        .expect("run json");
    let run_json: serde_json::Value =
        serde_json::from_str(&blob_text(&repo, &run_entry.blob_hash).await).unwrap();
    assert_eq!(run_json["worker_card_id"], serde_json::Value::Null);
    assert_eq!(run_json["worker_card_payload"], serde_json::Value::Null);

    let mut tx = repo.pool().begin().await.expect("begin snapshot");
    let snapshot = wave_vcs::snapshot_tree(&mut tx, &wave.id, MANIFEST_SCHEMA_VERSION)
        .await
        .expect("snapshot");
    tx.rollback().await.expect("rollback snapshot");
    assert_eq!(snapshot.manifest, manifest);

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
            agent_message: None,
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
