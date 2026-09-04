use std::collections::BTreeSet;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, card_update_tx, session_mark_superseded_runtime_tx,
    session_set_status_tx, session_start_runtime_tx, terminal_create_tx, track_update_tx,
};
use calm_server::event::{EditAuthor, Event, EventBus, EventScope, TrackUpdatedPayload};
use calm_server::harness::HarnessSnapshot;
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::model::{
    Card, CardPatch, CardRole, NewArea, NewCard, NewTerminal, NewTrack, TrackLifecycle, TrackPatch,
    new_id, now_ms,
};
use calm_server::routes::theme::RequestTheme;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::WriteContext;
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_fs_view::TrackFsView;
use calm_server::track_report::{TrackReportPayload, persist_report};
use calm_server::track_vcs::{self, DiffStatus, MANIFEST_SCHEMA_VERSION};
use serde_json::json;
use sqlx::{Row, SqlitePool};

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

async fn fresh_file_repo() -> (tempfile::TempDir, Arc<SqlxRepo>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track-vcs.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let repo = SqlxRepo::open(&url).await.expect("open sqlite repo");
    (dir, Arc::new(repo))
}

async fn make_area(repo: &SqlxRepo) -> calm_server::model::Area {
    repo.area_create(NewArea {
        name: "area".into(),
        color: "#abcdef".into(),
        sort: None,
    })
    .await
    .expect("create area")
}

async fn make_track(repo: &SqlxRepo, area_id: &str) -> calm_server::model::Track {
    repo.track_create(NewTrack {
        template_input: None,
        area_id: AreaId::from(area_id),
        title: "track".into(),
        sort: None,
        cwd: "/tmp".into(),
        template_id: None,
        plugin_scope: None,
        attach_folder: false,
        theme: RequestTheme::default_dark(),
    })
    .await
    .expect("create track")
}

#[allow(clippy::too_many_arguments)]
async fn add_card_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    roles: &CardRoleCache,
    write: &WriteContext,
    track_id: &TrackId,
    area_id: &AreaId,
    kind: &str,
    role: CardRole,
    payload: serde_json::Value,
) -> Card {
    add_card_with_id_with_event(
        repo,
        bus,
        roles,
        write,
        track_id,
        area_id,
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
    track_id: &TrackId,
    area_id: &AreaId,
    card_id: String,
    kind: &str,
    role: CardRole,
    payload: serde_json::Value,
) -> Card {
    let lookup_card_id = card_id.clone();
    let scope = EventScope::Card {
        card: CardId::from(card_id.clone()),
        track: track_id.clone(),
        area: area_id.clone(),
    };
    let new_card = NewCard {
        track_id: track_id.clone(),
        title: None,
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
                    !matches!(role, CardRole::ReportCard | CardRole::Planner),
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
    track_id: &TrackId,
    area_id: &AreaId,
) -> Card {
    add_card_with_event(
        repo,
        bus,
        roles,
        write,
        track_id,
        area_id,
        "track-report",
        CardRole::ReportCard,
        serde_json::to_value(TrackReportPayload::initial()).expect("report payload"),
    )
    .await
}

async fn update_card_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    card: &Card,
    area_id: &AreaId,
    patch: CardPatch,
) -> Card {
    let card_id = card.id.clone();
    let lookup_card_id = card_id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        track: card.track_id.clone(),
        area: area_id.clone(),
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

async fn update_track_title_with_actor(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    track_id: &TrackId,
    area_id: &AreaId,
    title: &str,
    actor: ActorId,
) {
    let track_id_for_tx = track_id.clone();
    let title = title.to_string();
    repo.write_with_event(
        actor,
        EventScope::Track {
            track: track_id.clone(),
            area: area_id.clone(),
        },
        None,
        bus,
        write,
        Box::new(move |tx| {
            let track_id = track_id_for_tx.clone();
            let title = title.clone();
            Box::pin(async move {
                let updated = track_update_tx(
                    tx,
                    track_id.as_str(),
                    TrackPatch {
                        title: Some(title),
                        ..TrackPatch::default()
                    },
                )
                .await?;
                Ok(Event::TrackUpdated(TrackUpdatedPayload::new(updated, None)))
            })
        }),
    )
    .await
    .expect("track title update event");
}

async fn insert_raw_card(
    repo: &SqlxRepo,
    roles: &CardRoleCache,
    track_id: &TrackId,
    kind: &str,
    role: CardRole,
    payload: serde_json::Value,
) -> Card {
    let mut tx = repo.pool().begin().await.expect("begin raw card insert");
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track_id.clone(),
            title: None,
            kind: kind.into(),
            sort: None,
            payload,
        },
        role,
        !matches!(role, CardRole::ReportCard | CardRole::Planner),
        roles,
    )
    .await
    .expect("insert raw card");
    tx.commit().await.expect("commit raw card insert");
    card
}

async fn insert_raw_report_card(
    repo: &SqlxRepo,
    roles: &CardRoleCache,
    track_id: &TrackId,
) -> Card {
    insert_raw_card(
        repo,
        roles,
        track_id,
        "track-report",
        CardRole::ReportCard,
        serde_json::to_value(TrackReportPayload::initial()).expect("report payload"),
    )
    .await
}

async fn start_codex_runtime_with_event(
    repo: &SqlxRepo,
    bus: &EventBus,
    write: &WriteContext,
    track_id: &TrackId,
    area_id: &AreaId,
    card_id: &CardId,
) -> String {
    let runtime_id = new_id();
    let returned_runtime_id = runtime_id.clone();
    let card_id_for_runtime = card_id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        track: track_id.clone(),
        area: area_id.clone(),
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
                let runtime = session_start_runtime_tx(
                    tx,
                    WorkerSessionInit {
                        id: runtime_id,
                        card_id: card_id.to_string(),
                        kind: WorkerSessionKind::CodexCard,
                        agent_provider: Some(AgentProvider::Codex),
                        status: WorkerSessionState::Running,
                        terminal_run_id: Some(terminal.id),
                        thread_id: Some("thread-1".into()),
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        spawn_op_id: None,
                        now_ms: now_ms(),
                    },
                )
                .await?;
                Ok(Event::WorkerSessionStarted {
                    worker_session_id: runtime.id,
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
    track_id: &TrackId,
    area_id: &AreaId,
    card_id: &CardId,
    runtime_id: &str,
    old_status: WorkerSessionState,
    new_status: WorkerSessionState,
) {
    let runtime_id = runtime_id.to_string();
    let card_id_for_event = card_id.clone();
    let scope = EventScope::Card {
        card: card_id.clone(),
        track: track_id.clone(),
        area: area_id.clone(),
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
            let old_status = old_status;
            let new_status = new_status;
            Box::pin(async move {
                session_set_status_tx(tx, &runtime_id, new_status).await?;
                Ok(Event::WorkerSessionStatusChanged {
                    worker_session_id: runtime_id,
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

fn write_context() -> (CardRoleCache, TrackAreaCache, WriteContext) {
    let roles = CardRoleCache::new();
    let areas = TrackAreaCache::new();
    let write = WriteContext::new(roles.clone(), areas.clone());
    (roles, areas, write)
}

async fn count_rows(pool: &SqlitePool, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar::<_, i64>(&sql)
        .fetch_one(pool)
        .await
        .expect("count rows")
}

async fn vcs_object_hashes(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT hash FROM track_vcs_objects ORDER BY hash")
        .fetch_all(pool)
        .await
        .expect("object hashes")
}

async fn set_all_vcs_objects_created_at(pool: &SqlitePool, created_at: i64) {
    sqlx::query("UPDATE track_vcs_objects SET created_at = ?1")
        .bind(created_at)
        .execute(pool)
        .await
        .expect("age objects");
}

async fn set_vcs_object_created_at(pool: &SqlitePool, hash: &str, created_at: i64) {
    sqlx::query("UPDATE track_vcs_objects SET created_at = ?1 WHERE hash = ?2")
        .bind(created_at)
        .bind(hash)
        .execute(pool)
        .await
        .expect("age object");
}

async fn vcs_object_exists(pool: &SqlitePool, hash: &str) -> bool {
    let exists: i64 =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM track_vcs_objects WHERE hash = ?1)")
            .bind(hash)
            .fetch_one(pool)
            .await
            .expect("object exists");
    exists != 0
}

fn old_vcs_object_timestamp() -> i64 {
    now_ms() - 2 * 60 * 60 * 1000
}

async fn track_commit_rows(
    repo: &SqlxRepo,
    track_id: &str,
) -> Vec<(String, Option<String>, Option<i64>)> {
    let rows = sqlx::query(
        r#"SELECT hash, parent_hash, event_id
           FROM track_vcs_commits
           WHERE track_id = ?1
           ORDER BY event_id ASC"#,
    )
    .bind(track_id)
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
        sqlx::query_scalar("SELECT bytes FROM track_vcs_objects WHERE hash = ?1 AND kind = 'blob'")
            .bind(hash)
            .fetch_one(repo.pool())
            .await
            .expect("blob bytes");
    String::from_utf8(bytes).expect("blob utf8")
}

async fn live_track_file_paths(
    view: &TrackFsView<'_>,
    track: &calm_server::model::Track,
) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        let entries = view
            .ls(
                track,
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

async fn head_manifest(repo: &SqlxRepo, track_id: &TrackId) -> track_vcs::TreeManifest {
    let head = track_vcs::head(repo.pool(), track_id)
        .await
        .expect("head query")
        .expect("head");
    track_vcs::tree_at(repo.pool(), &head)
        .await
        .expect("tree query")
        .expect("tree")
}

async fn refresh_transcripts(repo: &SqlxRepo, track_id: &TrackId) -> track_vcs::CommitHash {
    let mut tx = repo.pool().begin().await.expect("begin transcript refresh");
    let commit = track_vcs::snapshot_transcripts_for_cards_in_track(
        &mut tx,
        track_id,
        None,
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("snapshot transcripts");
    tx.commit().await.expect("commit transcript refresh");
    commit
}

async fn commit_tree_hash(repo: &SqlxRepo, commit: &str) -> String {
    sqlx::query_scalar("SELECT tree_hash FROM track_vcs_commits WHERE hash = ?1")
        .bind(commit)
        .fetch_one(repo.pool())
        .await
        .expect("commit tree hash")
}

fn transcript_paths(card_id: &CardId) -> (String, String) {
    (
        format!("cards/{}/events.json", card_id.as_str()),
        format!("cards/{}/conversation.md", card_id.as_str()),
    )
}

fn transcript_blob_hashes(
    manifest: &track_vcs::TreeManifest,
    card_id: &CardId,
) -> (String, String) {
    let (events_path, conversation_path) = transcript_paths(card_id);
    (
        manifest
            .entries
            .get(&events_path)
            .unwrap_or_else(|| panic!("{events_path} entry"))
            .blob_hash
            .clone(),
        manifest
            .entries
            .get(&conversation_path)
            .unwrap_or_else(|| panic!("{conversation_path} entry"))
            .blob_hash
            .clone(),
    )
}

#[allow(clippy::too_many_arguments)]
async fn log_codex_hook(
    repo: &SqlxRepo,
    bus: &EventBus,
    roles: &CardRoleCache,
    areas: &TrackAreaCache,
    track_id: &TrackId,
    area_id: &AreaId,
    card_id: &CardId,
    key: &str,
    prompt: &str,
) -> i64 {
    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: card_id.clone(),
            track: track_id.clone(),
            area: area_id.clone(),
        },
        None,
        bus,
        roles,
        areas,
        Event::CodexHook {
            card_id: card_id.clone(),
            kind: "hook.codex.user_prompt_submit".into(),
            hook_idempotency_key: key.into(),
            payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt}),
        },
    )
    .await
    .expect("codex hook event")
}

#[allow(clippy::too_many_arguments)]
async fn log_claude_hook(
    repo: &SqlxRepo,
    bus: &EventBus,
    roles: &CardRoleCache,
    areas: &TrackAreaCache,
    track_id: &TrackId,
    area_id: &AreaId,
    card_id: &CardId,
    key: &str,
    prompt: &str,
) -> i64 {
    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: card_id.clone(),
            track: track_id.clone(),
            area: area_id.clone(),
        },
        None,
        bus,
        roles,
        areas,
        Event::ClaudeHook {
            card_id: card_id.clone(),
            kind: "hook.claude.user_prompt_submit".into(),
            hook_idempotency_key: key.into(),
            payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt}),
        },
    )
    .await
    .expect("claude hook event")
}

async fn seed_head_payload_blob(
    repo: &SqlxRepo,
    track_id: &TrackId,
    payload_path: &str,
    payload: serde_json::Value,
) -> String {
    let parent = track_vcs::head(repo.pool(), track_id)
        .await
        .expect("head query")
        .expect("head");
    let mut manifest = track_vcs::tree_at(repo.pool(), &parent)
        .await
        .expect("tree query")
        .expect("tree");
    let payload_bytes = serde_json::to_vec(&payload).expect("legacy payload json");

    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin legacy payload seed");
    let blob_hash = track_vcs::put_blob(&mut tx, "blob", &payload_bytes)
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
        r#"INSERT INTO track_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'tree', ?2, ?3)"#,
    )
    .bind(&tree_hash)
    .bind(manifest_bytes)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .expect("insert legacy tree object");
    let tree = track_vcs::TreeSnapshot {
        tree_hash,
        manifest,
    };
    track_vcs::commit_tree(
        &mut tx,
        track_id,
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
    track_id: &TrackId,
    card_id: &CardId,
) -> String {
    let parent = track_vcs::head(repo.pool(), track_id)
        .await
        .expect("head query")
        .expect("head");
    let mut manifest = track_vcs::tree_at(repo.pool(), &parent)
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
        r#"INSERT INTO track_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'tree', ?2, ?3)"#,
    )
    .bind(&tree_hash)
    .bind(manifest_bytes)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .expect("insert legacy tree object");
    let tree = track_vcs::TreeSnapshot {
        tree_hash,
        manifest,
    };
    let legacy_head = track_vcs::commit_tree(
        &mut tx,
        track_id,
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "terminal",
        CardRole::Worker,
        json!({"z": "last", "a": "first"}),
    )
    .await;

    let mut tx = repo.pool().begin().await.expect("begin");
    let first = track_vcs::snapshot_tree(&mut tx, &track.id, MANIFEST_SCHEMA_VERSION)
        .await
        .expect("snapshot");
    for _ in 0..5 {
        let next = track_vcs::snapshot_tree(&mut tx, &track.id, MANIFEST_SCHEMA_VERSION)
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "terminal",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "legacy-paths"}),
    )
    .await;
    let legacy_head = seed_legacy_card_lens_manifest(&repo, &track.id, &worker.id).await;
    let legacy_manifest = track_vcs::tree_at(repo.pool(), &legacy_head)
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

    update_track_title_with_actor(
        &repo,
        &bus,
        &write,
        &track.id,
        &area.id,
        "post cutover",
        ActorId::User,
    )
    .await;

    let manifest = head_manifest(&repo, &track.id).await;
    assert!(manifest.entries.contains_key(&meta_path));
    assert!(manifest.entries.contains_key(&payload_path));
    assert!(!manifest.entries.contains_key(&legacy_meta_path));
    assert!(!manifest.entries.contains_key(&legacy_payload_path));

    let legacy_manifest_after = track_vcs::tree_at(repo.pool(), &legacy_head)
        .await
        .expect("legacy tree query after")
        .expect("legacy tree after");
    assert_eq!(legacy_manifest_after, legacy_manifest);
}

#[tokio::test]
async fn since_last_turn_suppresses_legacy_planner_payload_cutover_noise() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let planner = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "planner",
        CardRole::Planner,
        json!({"schemaVersion": 1}),
    )
    .await;
    let legacy_head = seed_legacy_card_lens_manifest(&repo, &track.id, &planner.id).await;

    update_track_title_with_actor(
        &repo,
        &bus,
        &write,
        &track.id,
        &area.id,
        "post cutover",
        ActorId::User,
    )
    .await;

    let block = track_vcs::since_last_turn_block(
        repo.pool(),
        &track.id,
        Some(&legacy_head),
        None,
        Some(&planner.id),
    )
    .await
    .expect("since-last-turn block")
    .block
    .expect("cutover diff block");
    let legacy_payload_noise = format!("cards/{}/payload.json deleted", planner.id.as_str());
    let payload_noise = format!("cards/{}/.payload.json new", planner.id.as_str());
    assert!(
        !block.contains(&legacy_payload_noise),
        "legacy payload cutover noise leaked into planner observation: {block}"
    );
    assert!(
        !block.contains(&payload_noise),
        "payload cutover noise leaked into planner observation: {block}"
    );
    assert!(block.contains("track.json edited"), "block = {block}");
}

#[tokio::test]
async fn next_commit_after_legacy_eventless_card_lens_manifest_rewrites_dotfile_paths() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    let worker = insert_raw_card(
        &repo,
        &roles,
        &track.id,
        "terminal",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "eventless-legacy-paths"}),
    )
    .await;
    let backfilled = track_vcs::backfill_existing_tracks(repo.pool())
        .await
        .expect("backfill");
    assert_eq!(backfilled, 1);
    let legacy_head = seed_legacy_card_lens_manifest(&repo, &track.id, &worker.id).await;
    let legacy_manifest = track_vcs::tree_at(repo.pool(), &legacy_head)
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

    update_track_title_with_actor(
        &repo,
        &bus,
        &write,
        &track.id,
        &area.id,
        "post eventless cutover",
        ActorId::User,
    )
    .await;

    let manifest = head_manifest(&repo, &track.id).await;
    assert!(manifest.entries.contains_key(&meta_path));
    assert!(manifest.entries.contains_key(&payload_path));
    assert!(!manifest.entries.contains_key(&legacy_meta_path));
    assert!(!manifest.entries.contains_key(&legacy_payload_path));
}

#[test]
fn canonical_json_sorts_keys_and_uses_integer_time_shape() {
    let bytes = track_vcs::canonical_json_bytes(&json!({
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
    let (_roles, _areas, write) = write_context();
    let missing_track = TrackId::from("missing-track");
    let missing_area = AreaId::from("missing-area");

    let err = repo
        .write_with_event(
            ActorId::User,
            EventScope::Track {
                track: missing_track,
                area: missing_area,
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
        .expect_err("missing track should fail VCS commit");
    assert!(format!("{err}").contains("track missing-track"));

    assert_eq!(count_rows(repo.pool(), "events").await, 0);
    assert_eq!(count_rows(repo.pool(), "track_vcs_commits").await, 0);
    assert_eq!(count_rows(repo.pool(), "track_vcs_refs").await, 0);
}

#[tokio::test]
async fn actor_event_batch_writes_track_vcs_commit_with_lifecycle_and_verdict() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let planner = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "planner",
        CardRole::Planner,
        json!({"schemaVersion": 1}),
    )
    .await;

    let before_commits = track_commit_rows(&repo, track.id.as_str()).await;
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: area.id.clone(),
    };
    let track_id = track.id.clone();
    let planner_actor = ActorId::AiPlanner(planner.id.clone());
    let event_ids = repo
        .write_with_actor_events(
            None,
            &bus,
            &write,
            Box::new(move |tx| {
                let scope = scope.clone();
                let track_id = track_id.clone();
                let planner_actor = planner_actor.clone();
                Box::pin(async move {
                    let mut events = Vec::new();
                    if let Some(auto_events) =
                        calm_server::track_lifecycle::auto_promote_draft_in_tx(tx, &track_id)
                            .await?
                    {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::Kernel, scope.clone(), event)),
                        );
                    }
                    if let Some(lifecycle_events) =
                        calm_server::track_lifecycle::apply_requested_transition_in_tx(
                            tx,
                            &track_id,
                            TrackLifecycle::Dispatching,
                            &planner_actor,
                            "dispatch accepted work".into(),
                        )
                        .await?
                    {
                        events.extend(
                            lifecycle_events
                                .into_iter()
                                .map(|event| (planner_actor.clone(), scope.clone(), event)),
                        );
                    }
                    events.push((
                        planner_actor,
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

    let after_commits = track_commit_rows(&repo, track.id.as_str()).await;
    assert_eq!(after_commits.len(), before_commits.len() + 1);
    let latest = after_commits.last().expect("latest commit");
    assert_eq!(
        latest.1.as_deref(),
        before_commits.last().map(|row| row.0.as_str())
    );
    assert_eq!(latest.2, event_ids.last().copied());

    let commit = sqlx::query(
        r#"SELECT event_id, lifecycle, message
           FROM track_vcs_commits
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
            (event_ids[0], "track.lifecycle_changed".into()),
            (event_ids[1], "track.updated".into()),
            (event_ids[2], "track.lifecycle_changed".into()),
            (event_ids[3], "track.updated".into()),
            (event_ids[4], "task.completed".into()),
        ]
    );

    let manifest = head_manifest(&repo, &track.id).await;
    let track_entry = manifest.entries.get("track.json").expect("track json");
    let track_json: serde_json::Value =
        serde_json::from_str(&blob_text(&repo, &track_entry.blob_hash).await).unwrap();
    assert_eq!(
        track_json
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
async fn track_delete_cascades_refs_and_commits_but_leaves_objects() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let (roles, _areas, _write) = write_context();
    insert_raw_report_card(&repo, &roles, &track.id).await;

    let backfilled = track_vcs::backfill_existing_tracks(repo.pool())
        .await
        .expect("backfill");
    assert_eq!(backfilled, 1);
    assert_eq!(count_rows(repo.pool(), "track_vcs_refs").await, 1);
    assert_eq!(count_rows(repo.pool(), "track_vcs_commits").await, 1);
    assert!(count_rows(repo.pool(), "track_vcs_objects").await > 0);

    repo.track_delete(track.id.as_str())
        .await
        .expect("delete track");
    assert_eq!(count_rows(repo.pool(), "track_vcs_refs").await, 0);
    assert_eq!(count_rows(repo.pool(), "track_vcs_commits").await, 0);
    assert!(count_rows(repo.pool(), "track_vcs_objects").await > 0);
}

#[tokio::test]
async fn object_sweep_deletes_old_orphans_but_keeps_fresh_ones() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let object_hashes = vcs_object_hashes(repo.pool()).await;
    assert!(object_hashes.len() > 1);
    let fresh_hash = object_hashes[0].clone();

    repo.track_delete(track.id.as_str())
        .await
        .expect("delete track");
    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;
    set_vcs_object_created_at(repo.pool(), &fresh_hash, now_ms()).await;

    let deleted = track_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep objects");

    assert_eq!(deleted, (object_hashes.len() - 1) as u64);
    assert_eq!(count_rows(repo.pool(), "track_vcs_objects").await, 1);
    assert!(vcs_object_exists(repo.pool(), &fresh_hash).await);
}

#[tokio::test]
async fn object_sweep_keeps_objects_referenced_by_live_commits() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let before = count_rows(repo.pool(), "track_vcs_objects").await;
    assert!(before > 0);

    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;
    let deleted = track_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep objects");

    assert_eq!(deleted, 0);
    assert_eq!(count_rows(repo.pool(), "track_vcs_objects").await, before);
}

#[tokio::test]
async fn object_sweep_reports_corrupt_tree_object_hash() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;

    let tree_hash: String = sqlx::query_scalar(
        r#"SELECT tree_hash
           FROM track_vcs_commits
           WHERE track_id = ?1
           ORDER BY created_at DESC
           LIMIT 1"#,
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .expect("tree hash");

    sqlx::query("UPDATE track_vcs_objects SET bytes = ?1 WHERE hash = ?2")
        .bind(b"not-json".to_vec())
        .bind(&tree_hash)
        .execute(repo.pool())
        .await
        .expect("corrupt tree object");

    let err = track_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect_err("corrupt tree manifest fails closed");

    assert!(
        err.to_string().contains(&tree_hash),
        "error should include corrupt tree object hash: {err}"
    );
}

#[tokio::test]
async fn object_sweep_keeps_blob_shared_by_deleted_and_live_tracks() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let deleted_track = make_track(&repo, area.id.as_str()).await;
    let live_track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &deleted_track.id, &area.id).await;
    add_report_card(&repo, &bus, &roles, &write, &live_track.id, &area.id).await;

    let deleted_manifest = head_manifest(&repo, &deleted_track.id).await;
    let live_manifest = head_manifest(&repo, &live_track.id).await;
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

    repo.track_delete(deleted_track.id.as_str())
        .await
        .expect("delete track");
    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;
    let deleted = track_vcs::sweep_unreferenced_objects_once(repo.pool())
        .await
        .expect("sweep objects");

    assert!(deleted > 0);
    assert!(vcs_object_exists(repo.pool(), &shared_blob).await);
    assert!(
        track_vcs::tree_at(
            repo.pool(),
            &track_vcs::head(repo.pool(), &live_track.id)
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
    let area = make_area(&repo).await;
    let live_track = make_track(&repo, area.id.as_str()).await;
    let deleted_track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &live_track.id, &area.id).await;
    add_report_card(&repo, &bus, &roles, &write, &deleted_track.id, &area.id).await;
    repo.track_delete(deleted_track.id.as_str())
        .await
        .expect("delete track");
    set_all_vcs_objects_created_at(repo.pool(), old_vcs_object_timestamp()).await;

    let sweep_repo = repo.clone();
    let write_repo = repo.clone();
    let write_bus = bus.clone();
    let write_context = write.clone();
    let live_track_id = live_track.id.clone();
    let live_area_id = area.id.clone();
    let update_track_id = live_track.id.clone();

    let sweep = tokio::spawn(async move {
        track_vcs::sweep_unreferenced_objects_once(sweep_repo.pool())
            .await
            .expect("sweep objects")
    });
    let write = tokio::spawn(async move {
        write_repo
            .write_with_event(
                ActorId::User,
                EventScope::Track {
                    track: live_track_id,
                    area: live_area_id,
                },
                None,
                &write_bus,
                &write_context,
                Box::new(move |tx| {
                    let update_track_id = update_track_id.clone();
                    Box::pin(async move {
                        let updated = track_update_tx(
                            tx,
                            update_track_id.as_str(),
                            TrackPatch {
                                title: Some("updated during sweep".into()),
                                ..TrackPatch::default()
                            },
                        )
                        .await?;
                        Ok(Event::TrackUpdated(TrackUpdatedPayload::new(updated, None)))
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
        track_vcs::head(repo.pool(), &live_track.id)
            .await
            .expect("live head")
            .is_some()
    );
}

#[tokio::test]
async fn concurrent_same_track_writes_form_linear_history() {
    let (_dir, repo) = fresh_file_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let roles = CardRoleCache::new();
    let areas = TrackAreaCache::new();
    let write = WriteContext::new(roles.clone(), areas.clone());
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;

    let mut handles = Vec::new();
    for key in ["one", "two"] {
        let repo = repo.clone();
        let bus = bus.clone();
        let roles = roles.clone();
        let areas = areas.clone();
        let track_id = track.id.clone();
        let area_id = area.id.clone();
        handles.push(tokio::spawn(async move {
            repo.log_pure_event(
                ActorId::User,
                EventScope::Track {
                    track: track_id,
                    area: area_id,
                },
                None,
                &bus,
                &roles,
                &areas,
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

    let commits = track_commit_rows(&repo, track.id.as_str()).await;
    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0].1, None);
    assert_eq!(commits[1].1.as_deref(), Some(commits[0].0.as_str()));
    assert_eq!(commits[2].1.as_deref(), Some(commits[1].0.as_str()));
    assert!(commits[2].2 > commits[1].2);
}

#[tokio::test]
async fn backfill_is_idempotent_and_uses_null_event_id_for_eventless_track() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let (roles, _areas, _write) = write_context();
    insert_raw_report_card(&repo, &roles, &track.id).await;

    assert_eq!(
        track_vcs::backfill_existing_tracks(repo.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        track_vcs::backfill_existing_tracks(repo.pool())
            .await
            .unwrap(),
        0
    );

    let row = sqlx::query(
        "SELECT COUNT(*) AS n, SUM(CASE WHEN event_id IS NULL THEN 1 ELSE 0 END) AS null_events FROM track_vcs_commits WHERE track_id = ?1",
    )
    .bind(track.id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(row.try_get::<i64, _>("n").unwrap(), 1);
    assert_eq!(row.try_get::<i64, _>("null_events").unwrap(), 1);
}

#[tokio::test]
async fn backfilled_eventless_cards_survive_incremental_index_rerenders() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, _write) = write_context();
    insert_raw_report_card(&repo, &roles, &track.id).await;
    let worker = insert_raw_card(
        &repo,
        &roles,
        &track.id,
        "terminal",
        CardRole::Worker,
        json!({"schemaVersion": 1, "label": "legacy"}),
    )
    .await;

    assert_eq!(
        track_vcs::backfill_existing_tracks(repo.pool())
            .await
            .unwrap(),
        1
    );
    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: worker.id.clone(),
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::CardUpdated(worker.clone()),
    )
    .await
    .expect("legacy card update event");

    let manifest = head_manifest(&repo, &track.id).await;
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
async fn batch_write_creates_one_commit_at_last_track_event() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let track_id = track.id.clone();
    let area_id = area.id.clone();

    let ids = repo
        .write_with_events(
            ActorId::User,
            None,
            &bus,
            &write,
            Box::new(move |tx| {
                let track_id = track_id.clone();
                let area_id = area_id.clone();
                Box::pin(async move {
                    let updated = track_update_tx(
                        tx,
                        track_id.as_str(),
                        TrackPatch {
                            title: Some("renamed".into()),
                            ..TrackPatch::default()
                        },
                    )
                    .await?;
                    Ok(vec![
                        (
                            EventScope::Track {
                                track: track_id.clone(),
                                area: area_id.clone(),
                            },
                            Event::TrackUpdated(TrackUpdatedPayload::new(updated, None)),
                        ),
                        (
                            EventScope::Track {
                                track: track_id,
                                area: area_id,
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

    let commits = track_commit_rows(&repo, track.id.as_str()).await;
    assert_eq!(commits.len(), 2);
    let latest = commits.last().unwrap();
    assert_eq!(latest.2, Some(*ids.last().unwrap()));
    let author: Option<String> =
        sqlx::query_scalar("SELECT author FROM track_vcs_commits WHERE hash = ?1")
            .bind(&latest.0)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(author.as_deref(), Some("user"));
}

#[tokio::test]
async fn mixed_actor_batch_commit_is_unattributed_in_diff_block() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "mixed-actor-batch"}),
    )
    .await;
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before mixed batch");
    let track_id = track.id.clone();
    let area_id = area.id.clone();

    repo.write_with_actor_events(
        None,
        &bus,
        &write,
        Box::new(move |tx| {
            let track_id = track_id.clone();
            let area_id = area_id.clone();
            Box::pin(async move {
                let updated = track_update_tx(
                    tx,
                    track_id.as_str(),
                    TrackPatch {
                        title: Some("mixed actor title".into()),
                        ..TrackPatch::default()
                    },
                )
                .await?;
                Ok(vec![
                    (
                        ActorId::User,
                        EventScope::Track {
                            track: track_id.clone(),
                            area: area_id.clone(),
                        },
                        Event::TrackUpdated(TrackUpdatedPayload::new(updated, None)),
                    ),
                    (
                        ActorId::Kernel,
                        EventScope::Track {
                            track: track_id,
                            area: area_id,
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

    let after = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after mixed batch");
    let author: Option<String> =
        sqlx::query_scalar("SELECT author FROM track_vcs_commits WHERE hash = ?1")
            .bind(&after)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(author, None);

    let block = track_vcs::since_last_turn_block(repo.pool(), &track.id, Some(&before), None, None)
        .await
        .unwrap()
        .block
        .expect("diff block");
    assert!(block.contains("track.json edited"), "block = {block}");
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
async fn incremental_commit_changes_only_expected_track_paths() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let track_id = track.id.clone();
    let first = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head");

    repo.write_with_event(
        ActorId::User,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &write,
        Box::new(move |tx| {
            let track_id = track_id.clone();
            Box::pin(async move {
                let updated = track_update_tx(
                    tx,
                    track_id.as_str(),
                    TrackPatch {
                        title: Some("second title".into()),
                        ..TrackPatch::default()
                    },
                )
                .await?;
                Ok(Event::TrackUpdated(TrackUpdatedPayload::new(updated, None)))
            })
        }),
    )
    .await
    .expect("second commit");
    let second = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head");

    let diff = track_vcs::diff(repo.pool(), &first, &second, None)
        .await
        .expect("diff");
    let root_diff = track_vcs::diff(repo.pool(), &first, &second, Some("/"))
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
            ("track.json", DiffStatus::Modified)
        ]
    );
}

#[tokio::test]
async fn card_added_commit_updates_index_markdown_card_count() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before");

    add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "terminal",
        CardRole::Worker,
        json!({"schemaVersion": 1}),
    )
    .await;
    let after = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after");

    let diff = track_vcs::diff(repo.pool(), &before, &after, None)
        .await
        .expect("diff");
    assert!(diff.iter().any(|entry| entry.path == "index.md"));
    let manifest = head_manifest(&repo, &track.id).await;
    let index = manifest.entries.get("index.md").expect("index.md entry");
    let text = blob_text(&repo, &index.blob_hash).await;
    assert!(text.contains("- Cards: 2"));
}

#[tokio::test]
async fn hook_events_advance_commits_without_rewriting_transcripts_or_objects() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "hook-no-drip"}),
    )
    .await;
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before hooks");
    let before_tree = commit_tree_hash(&repo, &before).await;
    let before_hashes = transcript_blob_hashes(&head_manifest(&repo, &track.id).await, &worker.id);
    let before_objects = count_rows(repo.pool(), "track_vcs_objects").await;
    let before_commits = count_rows(repo.pool(), "track_vcs_commits").await;

    let codex_event_id = log_codex_hook(
        &repo,
        &bus,
        &roles,
        &areas,
        &track.id,
        &area.id,
        &worker.id,
        "hook-no-drip-codex",
        "codex progress",
    )
    .await;
    let codex_head = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after codex hook");
    let codex_record = track_vcs::commit_record(repo.pool(), &codex_head)
        .await
        .unwrap()
        .expect("codex hook commit");
    assert_eq!(codex_record.parent_hash.as_deref(), Some(before.as_str()));
    assert_eq!(codex_record.event_id, Some(codex_event_id));
    assert_eq!(codex_record.tree_hash, before_tree);
    assert_eq!(
        transcript_blob_hashes(&head_manifest(&repo, &track.id).await, &worker.id),
        before_hashes
    );
    assert_eq!(
        count_rows(repo.pool(), "track_vcs_objects").await,
        before_objects
    );

    let claude_event_id = log_claude_hook(
        &repo,
        &bus,
        &roles,
        &areas,
        &track.id,
        &area.id,
        &worker.id,
        "hook-no-drip-claude",
        "claude progress",
    )
    .await;
    let claude_head = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after claude hook");
    let claude_record = track_vcs::commit_record(repo.pool(), &claude_head)
        .await
        .unwrap()
        .expect("claude hook commit");
    assert_eq!(
        claude_record.parent_hash.as_deref(),
        Some(codex_head.as_str())
    );
    assert_eq!(claude_record.event_id, Some(claude_event_id));
    assert_eq!(claude_record.tree_hash, before_tree);
    assert_eq!(
        transcript_blob_hashes(&head_manifest(&repo, &track.id).await, &worker.id),
        before_hashes
    );
    assert_eq!(
        count_rows(repo.pool(), "track_vcs_objects").await,
        before_objects
    );
    assert_eq!(
        count_rows(repo.pool(), "track_vcs_commits").await,
        before_commits + 2
    );
}

#[tokio::test]
async fn hook_only_commits_leave_transcript_paths_unchanged_until_turn_refresh() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "hook-unchanged"}),
    )
    .await;
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before hooks");

    log_codex_hook(
        &repo,
        &bus,
        &roles,
        &areas,
        &track.id,
        &area.id,
        &worker.id,
        "hook-unchanged-1",
        "progress one",
    )
    .await;
    log_codex_hook(
        &repo,
        &bus,
        &roles,
        &areas,
        &track.id,
        &area.id,
        &worker.id,
        "hook-unchanged-2",
        "progress two",
    )
    .await;
    let after = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after hooks");
    let (events_path, conversation_path) = transcript_paths(&worker.id);

    let diff = track_vcs::diff(repo.pool(), &before, &after, None)
        .await
        .expect("hook-only diff");
    assert!(
        diff.iter()
            .all(|entry| entry.path != events_path && entry.path != conversation_path),
        "hook-only commits must not dirty transcript paths: {diff:?}"
    );
    let since = track_vcs::since_last_turn_block(repo.pool(), &track.id, Some(&before), None, None)
        .await
        .expect("since-last-turn block");
    assert_eq!(since.current_head.as_deref(), Some(after.as_str()));
    assert!(
        since.block.is_none(),
        "unchanged transcript-only hook commits must not produce a diff block: {since:?}"
    );
}

#[tokio::test]
async fn turn_boundary_refresh_makes_hook_transcripts_fresh_once() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "turn-boundary"}),
    )
    .await;
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before hooks");

    for seq in 0..3 {
        log_codex_hook(
            &repo,
            &bus,
            &roles,
            &areas,
            &track.id,
            &area.id,
            &worker.id,
            &format!("turn-boundary-hook-{seq}"),
            &format!("boundary progress {seq}"),
        )
        .await;
    }

    let refresh = refresh_transcripts(&repo, &track.id).await;
    let manifest = track_vcs::tree_at(repo.pool(), &refresh)
        .await
        .expect("refresh tree")
        .expect("refresh manifest");
    let view = TrackFsView::new(&repo, &write);
    for path in [
        format!("cards/{}/events.json", worker.id.as_str()),
        format!("cards/{}/conversation.md", worker.id.as_str()),
    ] {
        let vcs = blob_text(
            &repo,
            &manifest
                .entries
                .get(&path)
                .unwrap_or_else(|| panic!("{path} entry"))
                .blob_hash,
        )
        .await;
        let live = view
            .cat(&track, &path)
            .await
            .unwrap_or_else(|_| panic!("live {path}"))
            .content;
        assert_eq!(vcs, live, "path {path}");
    }

    let (events_path, conversation_path) = transcript_paths(&worker.id);
    let diff = track_vcs::diff(repo.pool(), &before, &refresh, None)
        .await
        .expect("boundary diff");
    assert_eq!(
        diff.iter()
            .filter(|entry| entry.path == events_path || entry.path == conversation_path)
            .count(),
        2,
        "turn-boundary transcript refresh should surface each transcript path once: {diff:?}"
    );
    let block = track_vcs::since_last_turn_block(repo.pool(), &track.id, Some(&before), None, None)
        .await
        .expect("boundary since-last-turn")
        .block
        .expect("boundary diff block");
    assert_eq!(block.matches(&format!("- {events_path} edited")).count(), 1);
    assert_eq!(
        block
            .matches(&format!("- {conversation_path} edited"))
            .count(),
        1
    );
}

#[tokio::test]
async fn transcript_refresh_includes_backfilled_inherited_cards() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    insert_raw_report_card(&repo, &roles, &track.id).await;
    let worker = insert_raw_card(
        &repo,
        &roles,
        &track.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "inherited-transcript"}),
    )
    .await;
    assert_eq!(
        track_vcs::backfill_existing_tracks(repo.pool())
            .await
            .expect("backfill"),
        1
    );
    let before_hashes = transcript_blob_hashes(&head_manifest(&repo, &track.id).await, &worker.id);

    log_codex_hook(
        &repo,
        &bus,
        &roles,
        &areas,
        &track.id,
        &area.id,
        &worker.id,
        "inherited-transcript-hook",
        "inherited card progress",
    )
    .await;
    let refresh = refresh_transcripts(&repo, &track.id).await;
    let manifest = track_vcs::tree_at(repo.pool(), &refresh)
        .await
        .expect("refresh tree")
        .expect("refresh manifest");
    let after_hashes = transcript_blob_hashes(&manifest, &worker.id);
    assert_ne!(after_hashes, before_hashes);

    let (_, conversation_path) = transcript_paths(&worker.id);
    let vcs_conversation = blob_text(
        &repo,
        &manifest
            .entries
            .get(&conversation_path)
            .expect("conversation entry")
            .blob_hash,
    )
    .await;
    assert!(
        vcs_conversation.contains("inherited card progress"),
        "conversation.md = {vcs_conversation}"
    );
    let live_conversation = TrackFsView::new(&repo, &write)
        .cat(&track, &conversation_path)
        .await
        .expect("live inherited conversation")
        .content;
    assert_eq!(vcs_conversation, live_conversation);
}

#[tokio::test]
async fn turn_boundary_refresh_tree_hash_matches_full_snapshot_replay() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "replay-parity"}),
    )
    .await;
    for seq in 0..4 {
        log_codex_hook(
            &repo,
            &bus,
            &roles,
            &areas,
            &track.id,
            &area.id,
            &worker.id,
            &format!("replay-parity-hook-{seq}"),
            &format!("replay parity progress {seq}"),
        )
        .await;
    }

    let refresh = refresh_transcripts(&repo, &track.id).await;
    let refresh_tree_hash = commit_tree_hash(&repo, &refresh).await;
    let mut tx = repo.pool().begin().await.expect("begin replay snapshot");
    let replayed = track_vcs::snapshot_tree(&mut tx, &track.id, MANIFEST_SCHEMA_VERSION)
        .await
        .expect("replayed full snapshot");
    tx.rollback().await.expect("rollback replay snapshot");

    assert_eq!(replayed.tree_hash, refresh_tree_hash);
}

#[tokio::test]
async fn manifest_blob_bytes_match_track_fs_view_for_populated_track() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "run-a", "goal": "check parity"}),
    )
    .await;

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
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
    start_codex_runtime_with_event(&repo, &bus, &write, &track.id, &area.id, &worker.id).await;
    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: worker.id.clone(),
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::CodexHook {
            card_id: worker.id.clone(),
            kind: "hook.codex.user_prompt_submit".into(),
            hook_idempotency_key: "hook-1".into(),
            payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": "hello"}),
        },
    )
    .await
    .expect("hook event");
    let hook_head = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after hook");
    let hook_author: Option<String> =
        sqlx::query_scalar("SELECT author FROM track_vcs_commits WHERE hash = ?1")
            .bind(&hook_head)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(hook_author.as_deref(), Some("kernel"));

    repo.log_pure_event(
        ActorId::KernelDispatcher,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::TaskCompleted {
            idempotency_key: "run-a".into(),
            result: json!({"summary": "done"}),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await
    .expect("task completed");

    refresh_transcripts(&repo, &track.id).await;
    let manifest = head_manifest(&repo, &track.id).await;
    let view = TrackFsView::new(&repo, &write);
    let manifest_paths = manifest.entries.keys().cloned().collect::<BTreeSet<_>>();
    let live_paths = live_track_file_paths(&view, &track).await;
    assert_eq!(manifest_paths, live_paths);

    for (path, entry) in &manifest.entries {
        let vcs = blob_text(&repo, &entry.blob_hash).await;
        let fs = view.cat(&track, path).await.expect(path);
        assert_eq!(vcs, fs.content, "path {path}");
    }
}

#[tokio::test]
async fn snapshot_transcripts_helper_produces_live_identical_blobs() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "snapshot-transcripts"}),
    )
    .await;

    for seq in 0..5 {
        repo.log_pure_event(
            ActorId::Kernel,
            EventScope::Card {
                card: worker.id.clone(),
                track: track.id.clone(),
                area: area.id.clone(),
            },
            None,
            &bus,
            &roles,
            &areas,
            Event::CodexHook {
                card_id: worker.id.clone(),
                kind: "hook.codex.user_prompt_submit".into(),
                hook_idempotency_key: format!("snapshot-hook-{seq}"),
                payload: json!({
                    "hook_event_name": "UserPromptSubmit",
                    "prompt": format!("snapshot prompt {seq}"),
                    "seq": seq,
                }),
            },
        )
        .await
        .expect("hook event");
    }

    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin transcript snapshot");
    let commit = track_vcs::snapshot_transcripts_for_cards_in_track(
        &mut tx,
        &track.id,
        None,
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("snapshot transcripts");
    tx.commit().await.expect("commit transcript snapshot");

    let manifest = track_vcs::tree_at(repo.pool(), &commit)
        .await
        .expect("tree query")
        .expect("tree");
    let view = TrackFsView::new(&repo, &write);
    for path in [
        format!("cards/{}/events.json", worker.id.as_str()),
        format!("cards/{}/conversation.md", worker.id.as_str()),
    ] {
        let vcs = blob_text(
            &repo,
            &manifest
                .entries
                .get(&path)
                .unwrap_or_else(|| panic!("{path} entry"))
                .blob_hash,
        )
        .await;
        let fs = view
            .cat(&track, &path)
            .await
            .unwrap_or_else(|_| panic!("live {path}"))
            .content;
        assert_eq!(vcs, fs, "path {path}");
    }
}

#[tokio::test]
async fn snapshot_transcripts_helper_is_deterministic_noop_on_unchanged() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "snapshot-transcripts-noop"}),
    )
    .await;

    for seq in 0..3 {
        repo.log_pure_event(
            ActorId::Kernel,
            EventScope::Card {
                card: worker.id.clone(),
                track: track.id.clone(),
                area: area.id.clone(),
            },
            None,
            &bus,
            &roles,
            &areas,
            Event::CodexHook {
                card_id: worker.id.clone(),
                kind: "hook.codex.user_prompt_submit".into(),
                hook_idempotency_key: format!("snapshot-noop-hook-{seq}"),
                payload: json!({
                    "hook_event_name": "UserPromptSubmit",
                    "prompt": format!("noop prompt {seq}"),
                    "seq": seq,
                }),
            },
        )
        .await
        .expect("hook event");
    }

    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin first transcript snapshot");
    let first = track_vcs::snapshot_transcripts_for_cards_in_track(
        &mut tx,
        &track.id,
        None,
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("first transcript snapshot");
    tx.commit().await.expect("commit first transcript snapshot");
    let first_record = track_vcs::commit_record(repo.pool(), &first)
        .await
        .expect("first commit record")
        .expect("first commit");

    let mut tx = repo
        .pool()
        .begin()
        .await
        .expect("begin second transcript snapshot");
    let second = track_vcs::snapshot_transcripts_for_cards_in_track(
        &mut tx,
        &track.id,
        None,
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("second transcript snapshot");
    tx.commit()
        .await
        .expect("commit second transcript snapshot");
    let second_record = track_vcs::commit_record(repo.pool(), &second)
        .await
        .expect("second commit record")
        .expect("second commit");

    assert_eq!(second_record.tree_hash, first_record.tree_hash);
}

#[tokio::test]
async fn hook_event_transcript_is_capped_to_recent_events_with_live_vcs_parity() {
    const EXPECTED_CAP: usize = 500;
    const EXTRA_EVENTS: usize = 50;

    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "hook-cap", "goal": "cap hooks"}),
    )
    .await;

    for seq in 0..(EXPECTED_CAP + EXTRA_EVENTS) {
        repo.log_pure_event(
            ActorId::Kernel,
            EventScope::Card {
                card: worker.id.clone(),
                track: track.id.clone(),
                area: area.id.clone(),
            },
            None,
            &bus,
            &roles,
            &areas,
            Event::CodexHook {
                card_id: worker.id.clone(),
                kind: "hook.codex.user_prompt_submit".into(),
                hook_idempotency_key: format!("hook-{seq:03}"),
                payload: json!({
                    "hook_event_name": "UserPromptSubmit",
                    "prompt": format!("prompt-{seq:03}"),
                    "seq": seq,
                }),
            },
        )
        .await
        .expect("hook event");
    }

    let events_path = format!("cards/{}/events.json", worker.id.as_str());
    let conversation_path = format!("cards/{}/conversation.md", worker.id.as_str());
    refresh_transcripts(&repo, &track.id).await;
    let manifest = head_manifest(&repo, &track.id).await;
    let view = TrackFsView::new(&repo, &write);

    let vcs_events = blob_text(
        &repo,
        &manifest
            .entries
            .get(&events_path)
            .expect("events.json entry")
            .blob_hash,
    )
    .await;
    let live_events = view
        .cat(&track, &events_path)
        .await
        .expect("live events.json")
        .content;
    assert_eq!(vcs_events, live_events);

    let events: serde_json::Value = serde_json::from_str(&vcs_events).expect("events json");
    let events = events.as_array().expect("events array");
    assert_eq!(events.len(), EXPECTED_CAP);
    let seqs = events
        .iter()
        .map(|event| event["payload"]["seq"].as_u64().expect("seq"))
        .collect::<Vec<_>>();
    let expected = (EXTRA_EVENTS as u64..(EXPECTED_CAP + EXTRA_EVENTS) as u64).collect::<Vec<_>>();
    assert_eq!(seqs, expected);

    let vcs_conversation = blob_text(
        &repo,
        &manifest
            .entries
            .get(&conversation_path)
            .expect("conversation.md entry")
            .blob_hash,
    )
    .await;
    let live_conversation = view
        .cat(&track, &conversation_path)
        .await
        .expect("live conversation.md")
        .content;
    assert_eq!(vcs_conversation, live_conversation);
    assert!(!vcs_conversation.contains("prompt-049"));
    assert!(vcs_conversation.contains("prompt-050"));
    assert!(vcs_conversation.contains("prompt-549"));
}

#[tokio::test]
async fn card_retarget_from_track_report_is_refused_and_preserves_report_blob() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    let report = add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let before_head = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before retarget");
    let before = head_manifest(&repo, &track.id).await;
    assert!(before.entries.contains_key("report.md"));

    let mut tx = repo.pool().begin().await.expect("begin report retarget");
    let error = card_update_tx(
        &mut tx,
        report.id.as_str(),
        CardPatch {
            title: None,
            kind: Some("terminal".into()),
            sort: None,
            payload: Some(json!({"schemaVersion": 1})),
            deletable: None,
        },
    )
    .await
    .expect_err("track-report retarget must be refused");
    assert!(
        error
            .to_string()
            .contains("track-report kind transitions and payloads")
    );
    tx.rollback().await.unwrap();

    let after = head_manifest(&repo, &track.id).await;
    assert_eq!(after, before);
    assert!(after.entries.contains_key("report.md"));
    assert_eq!(
        track_vcs::head(repo.pool(), &track.id).await.unwrap(),
        Some(before_head)
    );
}

#[tokio::test]
async fn since_last_turn_report_diff_uses_dynamic_fence_for_markdown_code_blocks() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    let report = add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let old_body = "# Goal\n\n```text\nstable\n```\n\nold line\n";
    let new_body = "# Goal\n\n```text\nstable\n```\n\nnew line\n";
    let initial = TrackReportPayload::initial();
    let old_payload = TrackReportPayload::new("", old_body);
    let report = persist_report(
        &repo,
        &bus,
        &write,
        ActorId::Kernel,
        EditAuthor::Kernel,
        track.clone(),
        report,
        initial,
        old_payload.clone(),
        0,
        None,
        None,
        false,
    )
    .await
    .expect("persist old report");
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before report edit");

    persist_report(
        &repo,
        &bus,
        &write,
        ActorId::Kernel,
        EditAuthor::Kernel,
        track.clone(),
        report,
        old_payload,
        TrackReportPayload::new("", new_body),
        1,
        None,
        None,
        false,
    )
    .await
    .expect("persist new report");
    let after = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after report edit");

    let since = track_vcs::since_last_turn_block(repo.pool(), &track.id, Some(&before), None, None)
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before updates");

    for i in 0..=50 {
        update_track_title_with_actor(
            &repo,
            &bus,
            &write,
            &track.id,
            &area.id,
            &format!("title-{i}"),
            ActorId::User,
        )
        .await;
    }

    let block = track_vcs::since_last_turn_block(repo.pool(), &track.id, Some(&before), None, None)
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before update");

    update_track_title_with_actor(
        &repo,
        &bus,
        &write,
        &track.id,
        &area.id,
        "legacy-null-author",
        ActorId::User,
    )
    .await;
    let after = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after update");
    sqlx::query("UPDATE track_vcs_commits SET author = NULL WHERE hash = ?1")
        .bind(&after)
        .execute(repo.pool())
        .await
        .unwrap();

    let block = track_vcs::since_last_turn_block(repo.pool(), &track.id, Some(&before), None, None)
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let high_id = add_card_with_id_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
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
        &track.id,
        &area.id,
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
        &area.id,
        CardPatch {
            title: None,
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
        &area.id,
        CardPatch {
            title: None,
            kind: None,
            sort: Some(1.0),
            payload: None,
            deletable: None,
        },
    )
    .await;
    let expected_ids = vec![low_id.id.to_string(), high_id.id.to_string()];

    let live_ids = repo
        .cards_by_track(track.id.as_str())
        .await
        .expect("cards by track")
        .into_iter()
        .map(|card| card.id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(live_ids, expected_ids);

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
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

    let manifest = head_manifest(&repo, &track.id).await;
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
    let snapshot = track_vcs::snapshot_tree(&mut tx, &track.id, MANIFEST_SCHEMA_VERSION)
        .await
        .expect("snapshot");
    tx.rollback().await.expect("rollback snapshot");
    assert_eq!(snapshot.manifest, manifest);
}

#[tokio::test]
async fn superseded_only_runtime_payload_matches_live_view_without_runtime_fields() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "superseded-only"}),
    )
    .await;

    let mut tx = repo.pool().begin().await.expect("begin runtime");
    let runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: new_id(),
            card_id: worker.id.to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some("stale-thread".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("runtime start");
    session_mark_superseded_runtime_tx(&mut tx, &runtime.id)
        .await
        .expect("mark superseded");
    tx.commit().await.expect("commit runtime");

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: worker.id.clone(),
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::WorkerSessionSuperseded {
            old_worker_session_id: runtime.id,
            new_worker_session_id: "missing-replacement".into(),
            card_id: worker.id.to_string(),
        },
    )
    .await
    .expect("runtime superseded event");

    let manifest = head_manifest(&repo, &track.id).await;
    let payload_path = format!("cards/{}/.payload.json", worker.id.as_str());
    let entry = manifest.entries.get(&payload_path).expect("payload entry");
    let vcs_payload = blob_text(&repo, &entry.blob_hash).await;
    let view = TrackFsView::new(&repo, &write);
    let live_payload = view.cat(&track, &payload_path).await.expect("live payload");
    assert_eq!(vcs_payload, live_payload.content);

    let payload: serde_json::Value = serde_json::from_str(&vcs_payload).unwrap();
    assert!(payload.get("codex_thread_id").is_none(), "{payload:?}");
    assert!(payload.get("codex_thread_status").is_none(), "{payload:?}");
}

#[tokio::test]
async fn planner_runtime_payload_blob_matches_live_view_without_projected_fields() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    let planner = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Planner,
        json!({"schemaVersion": 1, "planner_harness": true}),
    )
    .await;

    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.last_thread_id = Some("planner-thread".into());
    let (runtime, terminal_id) = {
        let mut tx = repo.pool().begin().await.expect("begin runtime");
        let terminal = terminal_create_tx(
            &mut tx,
            NewTerminal {
                card_id: planner.id.clone(),
                program: "codex".into(),
                cwd: "/tmp".into(),
                env: json!({}),
                theme: RequestTheme::default_dark(),
            },
        )
        .await
        .expect("terminal create");
        let terminal_id = terminal.id.clone();
        let runtime = session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: new_id(),
                card_id: planner.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: Some(terminal_id.clone()),
                thread_id: Some("planner-thread".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
                spawn_op_id: None,
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
            card: planner.id.clone(),
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::WorkerSessionStarted {
            worker_session_id: runtime.id,
            card_id: runtime.card_id,
            kind: runtime.kind,
            agent_provider: runtime.agent_provider,
            status: runtime.status,
        },
    )
    .await
    .expect("runtime started event");

    let manifest = head_manifest(&repo, &track.id).await;
    let payload_path = format!("cards/{}/.payload.json", planner.id.as_str());
    let entry = manifest.entries.get(&payload_path).expect("payload entry");
    let vcs_payload = blob_text(&repo, &entry.blob_hash).await;
    let view = TrackFsView::new(&repo, &write);
    let live_payload = view.cat(&track, &payload_path).await.expect("live payload");
    assert_eq!(vcs_payload, live_payload.content);

    let payload: serde_json::Value = serde_json::from_str(&vcs_payload).unwrap();
    assert!(payload.get("codex_thread_id").is_none(), "{payload:?}");
    assert!(payload.get("codex_source").is_none(), "{payload:?}");
    assert!(payload.get("codex_thread_status").is_none(), "{payload:?}");
    assert!(payload.get("terminal_id").is_none(), "{payload:?}");

    let runtime_path = format!("cards/{}/runtime.json", planner.id.as_str());
    let entry = manifest.entries.get(&runtime_path).expect("runtime entry");
    let vcs_runtime = blob_text(&repo, &entry.blob_hash).await;
    let live_runtime = view.cat(&track, &runtime_path).await.expect("live runtime");
    assert_eq!(vcs_runtime, live_runtime.content);

    let runtime: serde_json::Value = serde_json::from_str(&vcs_runtime).unwrap();
    assert_eq!(runtime["terminal_id"], terminal_id);
    assert_eq!(runtime["thread_id"], "planner-thread");
    assert_eq!(runtime["source"], "shared");
    assert_eq!(runtime["thread_status"], "started");
}

#[tokio::test]
async fn runtime_event_heals_legacy_projected_payload_blob_once() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
        "codex",
        CardRole::Worker,
        json!({"schemaVersion": 1, "idempotency_key": "legacy-heal", "goal": "heal"}),
    )
    .await;
    let runtime_id =
        start_codex_runtime_with_event(&repo, &bus, &write, &track.id, &area.id, &worker.id).await;
    let payload_path = format!("cards/{}/.payload.json", worker.id.as_str());

    let legacy_hash = seed_head_payload_blob(
        &repo,
        &track.id,
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
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .unwrap();
    set_runtime_status_with_event(
        &repo,
        &bus,
        &write,
        &track.id,
        &area.id,
        &worker.id,
        &runtime_id,
        WorkerSessionState::Running,
        WorkerSessionState::Idle,
    )
    .await;
    let after_heal = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .unwrap();

    let diff = track_vcs::diff(repo.pool(), &before, &after_heal, None)
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

    let block = track_vcs::since_last_turn_block(repo.pool(), &track.id, Some(&before), None, None)
        .await
        .expect("since-last-turn block")
        .block
        .expect("payload heal block");
    let payload_line = format!("- {payload_path} edited (by kernel)\n");
    assert_eq!(block.matches(&payload_line).count(), 1, "{block}");

    let healed_manifest = head_manifest(&repo, &track.id).await;
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
        &track.id,
        &area.id,
        &worker.id,
        &runtime_id,
        WorkerSessionState::Idle,
        WorkerSessionState::Running,
    )
    .await;
    let after_second_runtime_event = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .unwrap();
    let second_diff = track_vcs::diff(repo.pool(), &after_heal, &after_second_runtime_event, None)
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    let worker = add_card_with_event(
        &repo,
        &bus,
        &roles,
        &write,
        &track.id,
        &area.id,
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
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
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
        start_codex_runtime_with_event(&repo, &bus, &write, &track.id, &area.id, &worker.id).await;

    let run_path = "runs/runtime-flip.json";
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before runtime status flip");
    let before_manifest = head_manifest(&repo, &track.id).await;
    let before_run_entry = before_manifest
        .entries
        .get(run_path)
        .expect("run json before runtime status flip");
    let before_run_json = blob_text(&repo, &before_run_entry.blob_hash).await;

    set_runtime_status_with_event(
        &repo,
        &bus,
        &write,
        &track.id,
        &area.id,
        &worker.id,
        &runtime_id,
        WorkerSessionState::Running,
        WorkerSessionState::Failed,
    )
    .await;

    let after = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after runtime status flip");
    let paths = track_vcs::diff(repo.pool(), &before, &after, None)
        .await
        .expect("diff")
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert!(
        !paths.iter().any(|path| path == run_path),
        "runtime-only status flip must not diff {run_path}: {paths:?}"
    );

    let after_manifest = head_manifest(&repo, &track.id).await;
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    for key in ["one", "two"] {
        add_card_with_event(
            &repo,
            &bus,
            &roles,
            &write,
            &track.id,
            &area.id,
            "codex",
            CardRole::Worker,
            json!({"schemaVersion": 1, "idempotency_key": key}),
        )
        .await;
        repo.log_pure_event(
            ActorId::Kernel,
            EventScope::Track {
                track: track.id.clone(),
                area: area.id.clone(),
            },
            None,
            &bus,
            &roles,
            &areas,
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
    let before = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head before");

    repo.log_pure_event(
        ActorId::KernelDispatcher,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::TaskCompleted {
            idempotency_key: "one".into(),
            result: json!({"summary": "done"}),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await
    .expect("completion");
    let after = track_vcs::head(repo.pool(), &track.id)
        .await
        .unwrap()
        .expect("head after");
    let paths = track_vcs::diff(repo.pool(), &before, &after, None)
        .await
        .expect("diff")
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec!["runs/index.json", "runs/one.json", "runs/one.md"]
    );

    let manifest = head_manifest(&repo, &track.id).await;
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
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;

    let hidden_id = new_id();
    let mut tx = repo.pool().begin().await.expect("begin hidden insert");
    let hidden = card_create_with_id_tx(
        &mut tx,
        hidden_id.clone(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
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
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
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
    let manifest = head_manifest(&repo, &track.id).await;
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
    let snapshot = track_vcs::snapshot_tree(&mut tx, &track.id, MANIFEST_SCHEMA_VERSION)
        .await
        .expect("snapshot");
    tx.rollback().await.expect("rollback snapshot");
    assert_eq!(snapshot.manifest, manifest);

    repo.log_pure_event(
        ActorId::Kernel,
        EventScope::Card {
            card: hidden.id.clone(),
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::CardAdded(hidden),
    )
    .await
    .expect("CardAdded event");
    let manifest = head_manifest(&repo, &track.id).await;
    assert!(
        manifest
            .entries
            .keys()
            .any(|path| path.starts_with(&format!("cards/{hidden_id}/")))
    );
}

#[tokio::test]
async fn area_delete_cascades_track_vcs_refs_and_commits() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, _areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;
    assert!(
        track_vcs::head(repo.pool(), &track.id)
            .await
            .unwrap()
            .is_some()
    );

    repo.area_delete(area.id.as_str())
        .await
        .expect("delete area");
    assert_eq!(count_rows(repo.pool(), "track_vcs_refs").await, 0);
    assert_eq!(count_rows(repo.pool(), "track_vcs_commits").await, 0);
    assert!(
        track_vcs::head(repo.pool(), &track.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn reserved_run_key_does_not_clobber_runs_index() {
    let repo = fresh_repo().await;
    let area = make_area(&repo).await;
    let track = make_track(&repo, area.id.as_str()).await;
    let bus = EventBus::new();
    let (roles, areas, write) = write_context();
    add_report_card(&repo, &bus, &roles, &write, &track.id, &area.id).await;

    repo.log_pure_event(
        ActorId::KernelDispatcher,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &bus,
        &roles,
        &areas,
        Event::TaskCompleted {
            idempotency_key: "index".into(),
            result: json!({"summary": "reserved"}),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await
    .expect("reserved run event");

    let manifest = head_manifest(&repo, &track.id).await;
    assert!(manifest.entries.contains_key("runs/index.json"));
    assert!(!manifest.entries.contains_key("runs/index.md"));
    let index = manifest.entries.get("runs/index.json").expect("runs index");
    assert_eq!(blob_text(&repo, &index.blob_hash).await, "[]");
}
