//! Production operation recovery from the durable phase-2 checkpoint.
use super::*;

#[tokio::test]
async fn force_new_thread_recovery_after_phase2_crash() {
    recover_phase2(
        calm_server::harness::profile::HarnessProfile::Planner,
        false,
    )
    .await;
}

#[tokio::test]
async fn assistant_provider_legacy_wire_replays_a_phase2_crash_without_becoming_root() {
    recover_phase2(
        calm_server::harness::profile::HarnessProfile::Assistant,
        false,
    )
    .await;
}

#[tokio::test]
async fn assistant_provider_versioned_wire_replays_a_phase2_crash_without_becoming_root() {
    recover_phase2(
        calm_server::harness::profile::HarnessProfile::Assistant,
        true,
    )
    .await;
}

async fn recover_phase2(profile: calm_server::harness::profile::HarnessProfile, versioned: bool) {
    use calm_server::harness::profile::HarnessProfile;
    let assistant = profile == HarnessProfile::Assistant;
    let role = if assistant {
        CardRole::Assistant
    } else {
        CardRole::Planner
    };
    let kind = if assistant {
        WorkerSessionKind::ManagedAssistant
    } else {
        WorkerSessionKind::SharedPlanner
    };
    let tmp = TempDir::new().unwrap();
    let db_url = sqlite_url(&tmp, "phase2-crash.db");
    let (card_id, track_id, old_runtime_id, placeholder_id, op_id) = {
        let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "phase2-crash".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "phase2 crash".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let mut mint = repo.pool().begin().await.unwrap();
        let card = card_create_with_id_tx(&mut mint,new_id(),NewCard {
            track_id:track.id.clone(),title:None,kind:"codex".into(),sort:None,
            payload:if assistant {json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":"codex"})}
                else {json!({"schemaVersion":1,"planner_harness":true,"planner_provider":"codex"})},
        },role,false,repo.card_role_cache()).await.unwrap();
        mint.commit().await.unwrap();
        let old_runtime_id = new_id();
        let old_snapshot = HarnessSnapshot::initial(0, vec![]);
        let placeholder_id = new_id();
        let placeholder_snapshot = HarnessSnapshot::initial(0, vec![]);
        let now = now_ms();
        let mut payload = start_payload(
            track.id.as_ref(),
            &card.id,
            &track.workspace.path,
            Some("recover after crash"),
        );
        payload["profile"] = json!(profile);
        if versioned {
            payload["version"] = json!("assistant-v1");
            payload["provider"] = json!("codex");
        }
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card).unwrap(),
        );
        output.data = json!({
            "card_id": card.id.to_string(),
            "track_id": track.id.to_string(),
            "runtime_id": placeholder_id.clone(),
            "runtime_deferred": true,
            "cwd": track.workspace.path.clone(),
            "goal": "recover after crash",
            "report_card_id": null,
            "snapshot": serde_json::to_value(&placeholder_snapshot).unwrap(),
            "old_runtime_id": old_runtime_id.clone(),
            "old_runtime_status": WorkerSessionState::Idle,
        });
        let op_id = new_id();

        let mut tx = repo.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: old_runtime_id.clone(),
                card_id: card.id.to_string(),
                kind: kind.clone(),
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some("thread-old-before-crash".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&old_snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now,
            },
        )
        .await
        .unwrap();
        session_prepare_deferred_planner_tx(
            &mut tx,
            &WorkerSessionInit {
                id: placeholder_id.clone(),
                card_id: card.id.to_string(),
                kind: kind.clone(),
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&placeholder_snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now + 1,
            },
        )
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO operations (
                   id, operation_key, kind, idempotency_key, payload_hash,
                   target_type, target_id, target_json, payload_json,
                   tx_output_json, phase, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, 'planner-harness-start', NULL, ?3,
                       'card', ?4, ?5, ?6, ?7, 'tx_committed', ?8, ?8)"#,
        )
        .bind(&op_id)
        .bind(new_id())
        .bind(new_id())
        .bind(card.id.as_str())
        .bind(serde_json::to_string(&json!({"type": "card", "id": card.id})).unwrap())
        .bind(serde_json::to_string(&payload).unwrap())
        .bind(serde_json::to_string(&output).unwrap())
        .bind(now + 2)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        (
            card.id.to_string(),
            track.id.to_string(),
            old_runtime_id,
            placeholder_id,
            op_id,
        )
    };

    let repo = Arc::new(SqlxRepo::open(&db_url).await.unwrap());
    let role_cache = calm_server::card_role_cache::CardRoleCache::new();
    role_cache.insert(
        CardId::from(card_id.clone()),
        role,
        TrackId::from(track_id.clone()),
    );
    let state = app_state_for_boot_test_with_role_cache(repo.clone(), role_cache)
        .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
            repo.clone(),
            None,
        ));

    calm_server::recover_operations_on_boot(&state)
        .await
        .unwrap();

    let active = repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("phase-2 recovery should leave a new active session");
    assert_eq!(active.id, placeholder_id);
    assert_eq!(active.status, WorkerSessionState::Idle);
    assert_eq!(active.thread_id.as_deref(), Some("fake-thread-0001"));
    assert_ne!(active.id, old_runtime_id);

    let old_state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(&old_runtime_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(old_state, "superseded");

    let active_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
             FROM worker_sessions
            WHERE card_id = ?1
              AND state IN ('starting','running','idle','turn_pending')"#,
    )
    .bind(&card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(active_count, 1);

    let card_session: Option<String> =
        sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?1")
            .bind(&card_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(card_session.as_deref(), Some(placeholder_id.as_str()));

    if assistant {
        let root: Option<String> =
            sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
                .bind(&track_id)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert!(root.is_none(), "Assistant replay cannot become root");
        assert_eq!(active.kind, WorkerSessionKind::ManagedAssistant);
    }
    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(&op_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(phase, "succeeded");

    if let Some(handle) = state.harness.remove(&placeholder_id) {
        handle.shutdown().await.unwrap();
    }
}
