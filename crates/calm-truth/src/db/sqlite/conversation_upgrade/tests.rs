use super::super::*;
use super::{install_schema, prepare};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme};
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use calm_types::runtime::AgentProvider;
use calm_types::worker::WorkerSessionState;
use serde_json::json;

fn database() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("upgrade.db").display());
    (dir, url)
}

async fn carrier(repo: &SqlxRepo) -> (String, String) {
    carrier_profile(repo, false).await
}

async fn carrier_profile(repo: &SqlxRepo, plain: bool) -> (String, String) {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let area = area_create_tx(
        &mut tx,
        LEGACY_OWNER_USER_ID,
        NewArea {
            name: "upgrade".into(),
            color: "#123".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "upgrade".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        crate::model::new_id(),
        NewCard {
            track_id: track.id,
            kind: "codex".into(),
            sort: None,
            payload: if plain {
                json!({"harness_profile":"plain_chat"})
            } else {
                json!({"planner_provider":"codex"})
            },
            title: None,
        },
        if plain {
            CardRole::Worker
        } else {
            CardRole::Planner
        },
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    let worker = crate::model::new_id();
    let mut init = crate::session_projection_repo::WorkerSessionInit::shared_planner(
        worker.clone(),
        card.id.to_string(),
        AgentProvider::Codex,
        WorkerSessionState::Idle,
        None,
        serde_json::to_value(CanonicalHarnessSnapshot::dormant(0)).unwrap(),
        0,
    );
    if plain {
        init.kind = calm_types::runtime::WorkerSessionKind::CodexCard;
    }
    session_start_runtime_tx(&mut tx, init).await.unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), worker)
}

async fn assert_boot_refused(url: &str) {
    let error = SqlxRepo::open(url)
        .await
        .err()
        .expect("boot must fail closed");
    assert!(
        error.to_string().contains("conversation upgrade"),
        "{error}"
    );
}

#[tokio::test]
async fn u13_boot_allows_consumed_creation_receipt_without_resources() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("upgrade.db").display());
    let repo = SqlxRepo::open(&url).await.unwrap();
    sqlx::query("INSERT INTO track_create_idempotency(area_id,idempotency_key,track_id,planner_card_id,report_card_id,created_at_ms,request_fingerprint_version) VALUES ('gone-area','consumed','gone-track','gone-planner','gone-report',0,0)")
        .execute(repo.pool()).await.unwrap();
    repo.db().close().await;
    let reopened = SqlxRepo::open(&url)
        .await
        .expect("creation receipts are format-neutral");
    reopened.db().close().await;
}

#[tokio::test]
async fn u13_fresh_and_canonical_only_boot_without_marker() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    carrier(&repo).await;
    repo.db().close().await;
    let repo = SqlxRepo::open(&url).await.expect("canonical-only boot");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_upgrade")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    repo.db().close().await;
}

#[tokio::test]
async fn u13_boot_rejects_malformed_mixed_closed_and_retired_frames() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    carrier(&repo).await; // canonical evidence cannot hide a different V1 frame
    let (card, worker) = carrier(&repo).await;
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=(SELECT track_id FROM cards WHERE id=?1)")
        .bind(&card)
        .execute(repo.pool())
        .await
        .unwrap();
    for state in ["idle", "superseded", "failed", "exited"] {
        for raw in [
            None,
            Some("{"),
            Some(r#"{"schema_version":1,"mode":"harness","pending_queue":[]}"#),
            Some(r#"{"schema_version":5,"mode":"harness","canonical_inputs":{}}"#),
        ] {
            sqlx::query("UPDATE worker_sessions SET state=?1,handle_state_json=?2 WHERE id=?3")
                .bind(state)
                .bind(raw)
                .bind(&worker)
                .execute(repo.pool())
                .await
                .unwrap();
            assert_boot_refused(&url).await;
        }
    }
    sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
        .bind(&card)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET contract='assistant' WHERE id=?1")
        .bind(&worker)
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    conversation_archive_set_tx(&mut tx, &card, true)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_boot_refused(&url).await;
    repo.db().close().await;
}

#[tokio::test]
async fn u13_schema_only_installs_without_population_or_backfills() {
    let (_dir, url) = database();
    let raw = SqliteDb::connect(&url).await.unwrap();
    let old = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version < 199)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(raw.read_pool()).await.unwrap();
    sqlx::query("INSERT INTO track_create_idempotency(area_id,idempotency_key,track_id,planner_card_id,report_card_id,created_at_ms,request_fingerprint_version) VALUES ('a','old','t','p','r',1,0)")
        .execute(raw.writer_pool()).await.unwrap();
    raw.close().await;
    let db = install_schema(&url).await.unwrap();
    let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM track_create_idempotency),
                (SELECT count(*) FROM conversation_presentations),
                (SELECT count(*) FROM conversation_upgrade),
                (SELECT count(*) FROM database_identity),
                (SELECT count(*) FROM report_data_migrations)",
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(counts, (1, 0, 0, 0, 0));
    db.close().await;
    let reopened = SqlxRepo::open(&url)
        .await
        .expect("receipt without historical resources");
    reopened.db().close().await;
}

#[tokio::test]
async fn u13_prepare_durable_idempotent_and_never_completes() {
    let (_dir, url) = database();
    let db = install_schema(&url).await.unwrap();
    db.close().await;
    let first = prepare(&url).await.unwrap();
    let second = prepare(&url).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(first.state, "required");
    let db = install_schema(&url).await.unwrap();
    assert_eq!(
        super::state::read(&mut db.read_pool().acquire().await.unwrap())
            .await
            .unwrap(),
        Some(first)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM report_data_migrations")
            .fetch_one(db.read_pool())
            .await
            .unwrap(),
        0
    );
    let error = sqlx::query("UPDATE conversation_upgrade SET state='completed'")
        .execute(db.writer_pool())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint"));
    db.close().await;
    assert_boot_refused(&url).await;
}

#[tokio::test]
async fn u13_boot_and_prepare_reject_mismatched_or_malformed_marker() {
    let (_dir, url) = database();
    let original = prepare(&url).await.unwrap();
    let db = install_schema(&url).await.unwrap();
    // Emulate damaged/foreign marker bytes, including unsupported future state.
    // The normal SQL writer cannot insert these values.
    let mut connection = db.writer_pool().acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints=ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    for mutation in [
        "deployment_id='00000000-0000-0000-0000-000000000000'",
        "schema_sha256='bad'",
        "protocol_version=2",
        "state='applying'",
        "state='completed'",
        "prepared_at_ms=-1",
        "singleton=2",
    ] {
        sqlx::query(&format!("UPDATE conversation_upgrade SET {mutation}"))
            .execute(&mut *connection)
            .await
            .unwrap();
        assert_boot_refused(&url).await;
        assert!(
            prepare(&url).await.is_err(),
            "prepare cannot overwrite {mutation}"
        );
        sqlx::query("UPDATE conversation_upgrade SET singleton=1,deployment_id=?1,schema_sha256=?2,protocol_version=1,state='required',prepared_at_ms=?3")
            .bind(&original.deployment_id).bind(&original.schema_sha256).bind(original.prepared_at_ms)
            .execute(&mut *connection).await.unwrap();
    }
    drop(connection);
    sqlx::query("DELETE FROM database_identity")
        .execute(db.writer_pool())
        .await
        .unwrap();
    assert_boot_refused(&url).await;
    assert!(
        prepare(&url).await.is_err(),
        "new identity cannot bless the old marker"
    );
    db.close().await;
}

#[tokio::test]
async fn u13_required_barrier_precedes_semantic_backfills_and_caches() {
    let (_dir, url) = database();
    prepare(&url).await.unwrap();
    let db = install_schema(&url).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_backfill BEFORE INSERT ON report_data_migrations BEGIN SELECT RAISE(ABORT,'semantic backfill reached'); END")
        .execute(db.writer_pool()).await.unwrap();
    db.close().await;
    assert_boot_refused(&url).await;
    let db = install_schema(&url).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM report_data_migrations")
            .fetch_one(db.read_pool())
            .await
            .unwrap(),
        0
    );
    db.close().await;
}

#[tokio::test]
async fn u13_historical_receipts_and_orphan_native_evidence_refuse_boot() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    // Monotone canonical card evidence cannot launder an old/null ACK.
    for hash in ["v1:old", "unknown:version", ""] {
        sqlx::query("INSERT INTO planner_input_idempotency(card_id,idempotency_key,payload_hash,worker_session_id,entry_id,created_at_ms) VALUES (?1,'old',?2,?3,NULL,0)")
            .bind(&card).bind(hash).bind(&worker).execute(repo.pool()).await.unwrap();
        assert_boot_refused(&url).await;
        sqlx::query("DELETE FROM planner_input_idempotency")
            .execute(repo.pool())
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO harness_items(worker_session_id,card_id,track_id,thread_id,method,params,created_at_ms) VALUES ('missing-worker',?1,'missing-track','old-thread','item/completed','{}',0)")
        .bind(&card).execute(repo.pool()).await.unwrap();
    assert_boot_refused(&url).await;
    repo.db().close().await;
}

#[tokio::test]
async fn u13_preparation_preserves_closed_historical_bytes() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    let raw = r#"{ "schema_version":1,"mode":"harness","pending_queue":["keep exact bytes"] }"#;
    sqlx::query("UPDATE worker_sessions SET state='superseded',handle_state_json=?1 WHERE id=?2")
        .bind(raw)
        .bind(&worker)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=(SELECT track_id FROM cards WHERE id=?1)")
        .bind(&card)
        .execute(repo.pool())
        .await
        .unwrap();
    repo.db().close().await;
    let marker = prepare(&url).await.unwrap();
    assert_eq!(prepare(&url).await.unwrap(), marker);
    let db = install_schema(&url).await.unwrap();
    let preserved: (String, String) =
        sqlx::query_as("SELECT state,handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(db.read_pool())
            .await
            .unwrap();
    assert_eq!(preserved, ("superseded".into(), raw.into()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_presentations")
            .fetch_one(db.read_pool())
            .await
            .unwrap(),
        0
    );
    db.close().await;
    assert_boot_refused(&url).await;
}

#[tokio::test]
async fn u13_worker_and_plainchat_frames_remain_outside_upgrade() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    sqlx::query("UPDATE cards SET role='worker' WHERE id=?1")
        .bind(&card)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE worker_sessions SET contract='executor',handle_state_json='{}' WHERE id=?1",
    )
    .bind(&worker)
    .execute(repo.pool())
    .await
    .unwrap();
    repo.db().close().await;
    let repo = SqlxRepo::open(&url)
        .await
        .expect("Worker/PlainChat is outside the upgrade");
    repo.db().close().await;
}

#[tokio::test]
async fn u13_mixed_native_history_requires_its_existing_canonical_index() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let row = repo
        .fixture_transcript_insert(
            &worker,
            &card,
            &track,
            "thread",
            Some("turn"),
            Some("item"),
            Some("agentMessage"),
            "item/completed",
            r#"{"item":{"id":"item","type":"agentMessage"}}"#,
            None,
        )
        .await
        .unwrap();
    assert_boot_refused(&url).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    conversation::allocate_presentation_tx(
        &mut tx,
        &card,
        conversation::PresentationSource::NativeItem { row_id: row },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Older native revisions share the owner's source identity. Requiring each
    // raw row id to appear in the index would reject valid canonical history.
    repo.fixture_transcript_insert(
        &worker,
        &card,
        &track,
        "thread",
        Some("turn"),
        Some("item"),
        Some("agentMessage"),
        "item/started",
        r#"{"item":{"id":"item","type":"agentMessage"}}"#,
        None,
    )
    .await
    .unwrap();
    repo.db().close().await;
    let reopened = SqlxRepo::open(&url)
        .await
        .expect("indexed canonical native history");
    reopened.db().close().await;
}

#[tokio::test]
async fn u13_historical_and_unknown_start_journals_cannot_use_canonical_card_evidence() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    let retained =
        json!({"canonical_activation_version":2,"card_id":card,"worker_session_id":worker});
    sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) VALUES ('op','key','planner-harness-start','hash','card','{}',?1,'pending',0,0)")
        .bind(retained.to_string()).execute(repo.pool()).await.unwrap();
    let canonical = SqlxRepo::open(&url)
        .await
        .expect("canonical retained journal");
    canonical.db().close().await;
    for payload in [
        json!({"planner_card_id":card,"profile":"assistant"}),
        json!({"planner_card_id":"missing-card"}),
        json!({"canonical_activation_version":3,"card_id":card,"worker_session_id":worker}),
        json!({"canonical_activation_version":2,"card_id":card,"worker_session_id":"missing-worker"}),
        json!({"canonical_activation_version":2,"card_id":card,"worker_session_id":worker,"first_message":"old"}),
    ] {
        sqlx::query("UPDATE operations SET payload_json=?1 WHERE id='op'")
            .bind(payload.to_string())
            .execute(repo.pool())
            .await
            .unwrap();
        assert_boot_refused(&url).await;
    }
    repo.db().close().await;
}

mod regressions;
