use super::*;

struct LegacyCompensationHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    route_repo: Arc<dyn crate::db::RouteRepo>,
    spawn_ctx: SpawnCtx,
    output: TxOutput,
    op: Operation,
    card_id: String,
    runtime_id: String,
    events: EventBus,
}

async fn legacy_compensation_harness(
    card_kind: &str,
    session_kind: crate::session_projection_repo::WorkerSessionKind,
    agent_provider: Option<crate::session_projection_repo::AgentProvider>,
) -> LegacyCompensationHarness {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        repo.as_ref(),
        crate::model::NewCove {
            name: "legacy compensation".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        repo.as_ref(),
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "legacy compensation".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        repo.as_ref(),
        crate::model::NewCard {
            wave_id: wave.id,
            title: None,
            kind: card_kind.into(),
            sort: None,
            payload: json!({ "schemaVersion": 1 }),
        },
    )
    .await
    .unwrap();
    let runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: session_kind,
            agent_provider,
            status: crate::session_projection_repo::WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    let events = EventBus::new();
    let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let spawn_ctx = SpawnCtx::new(
        route_repo.clone(),
        operation_repo,
        Arc::new(DaemonClient::new_stub()),
        terminal_renderer,
        events.clone(),
        OperationCompletionBus::new(),
    );
    let card_id = card.id.to_string();

    LegacyCompensationHarness {
        repo,
        route_repo,
        spawn_ctx,
        output: TxOutput::new("card", Some(card_id.clone()), json!({})),
        op: Operation {
            id: new_id(),
            operation_key: new_id(),
            kind: format!("{card_kind}-test"),
            idempotency_key: Some(new_id()),
            payload_hash: new_id(),
            target_type: "card".into(),
            target_id: Some(card_id.clone()),
            target: json!({ "type": "card", "id": card_id }),
            payload: json!({}),
            tx_output: None,
            phase: Phase::Compensating,
            phase_detail: None,
            attempt: 0,
            last_error: None,
            compensation_state: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_artifacts: None,
            parked_at_ms: None,
            parked_deadline_ms: None,
        },
        card_id,
        runtime_id,
        events,
    }
}

async fn assert_legacy_failed_status_compensation(
    adapter: &dyn ProviderAdapter,
    harness: LegacyCompensationHarness,
) {
    let step = CompensationStep {
        op: "runtime_set_status_failed_for_card".into(),
        args: json!({ "card_id": harness.card_id }),
        completed: false,
        attempts: 0,
        last_error: None,
    };

    adapter
        .compensate_step(&step, &harness.output, &harness.op, &harness.spawn_ctx)
        .await
        .unwrap();

    let runtime =
        crate::session_projection_repo::WorkerSessionProjectionRepo::session_projection_by_id(
            harness.repo.as_ref(),
            &harness.runtime_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.status,
        crate::session_projection_repo::WorkerSessionState::Failed
    );
}

#[tokio::test]
async fn prompted_adapters_accept_legacy_failed_status_compensation_op() {
    let harness = legacy_compensation_harness(
        "codex",
        crate::session_projection_repo::WorkerSessionKind::CodexCard,
        Some(crate::session_projection_repo::AgentProvider::Codex),
    )
    .await;
    let repo: Arc<dyn crate::db::Repo> = harness.repo.clone();
    let adapter = crate::operation::codex_adapter::CodexAdapter::new(
        harness.route_repo.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(
            crate::pending_codex_threads::PendingThreadStartRegistry::new(
                repo,
                harness.events.clone(),
            ),
        ),
        Arc::new(Mutex::new(())),
        crate::card_role_cache::CardRoleCache::new(),
        crate::wave_cove_cache::WaveCoveCache::new(),
    );
    assert_legacy_failed_status_compensation(&adapter, harness).await;

    let harness = legacy_compensation_harness(
        "claude",
        crate::session_projection_repo::WorkerSessionKind::ClaudeCard,
        Some(crate::session_projection_repo::AgentProvider::Claude),
    )
    .await;
    let adapter = crate::operation::claude_adapter::ClaudeAdapter::new(
        harness.route_repo.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        crate::card_role_cache::CardRoleCache::new(),
        crate::wave_cove_cache::WaveCoveCache::new(),
    );
    assert_legacy_failed_status_compensation(&adapter, harness).await;

    let harness = legacy_compensation_harness(
        "claude",
        crate::session_projection_repo::WorkerSessionKind::ClaudeCard,
        Some(crate::session_projection_repo::AgentProvider::Claude),
    )
    .await;
    let adapter = crate::operation::claude_restart_adapter::ClaudeRestartAdapter::new(
        harness.route_repo.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        crate::card_role_cache::CardRoleCache::new(),
        crate::wave_cove_cache::WaveCoveCache::new(),
    );
    assert_legacy_failed_status_compensation(&adapter, harness).await;
}

#[test]
fn phase_split_round_trips_all_variants() {
    let cases = vec![
        Phase::Pending,
        Phase::TxCommitted,
        Phase::AppServerInteract {
            kind: AppServerInteractKind::MintAndAwait {
                thread_id: Some("thread-1".into()),
            },
        },
        Phase::AppServerInteract {
            kind: AppServerInteractKind::RegisterPending {
                entry_id: Some("pending-1".into()),
            },
        },
        Phase::SpawnStarted,
        Phase::SpawnSucceeded,
        Phase::Parked,
        Phase::Succeeded,
        Phase::Compensating,
        Phase::Failed,
        Phase::Stuck {
            reason: "needs operator".into(),
            since: 1_718_000_000,
        },
    ];

    for phase in cases {
        let (tag, detail) = phase.serialize_split();
        let joined = Phase::deserialize_join(tag.as_str(), detail.as_ref()).unwrap();
        assert_eq!(joined, phase);
    }
}

#[tokio::test]
async fn migration_check_rejects_parked_without_artifacts_deadline() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let now = now_ms();
    let err = sqlx::query(
        r#"INSERT INTO operations (
                   id, operation_key, kind, payload_hash, target_type,
                   target_json, payload_json, phase, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, 'test-kind', 'hash', 'unknown',
                       '{"type":"unknown","id":null}', '{}', 'parked', ?3, ?3)"#,
    )
    .bind(new_id())
    .bind(new_id())
    .bind(now)
    .execute(sqlx_repo.pool())
    .await
    .unwrap_err();
    assert!(
        matches!(err, sqlx::Error::Database(_)),
        "parked row without artifacts/deadline must fail CHECK: {err}"
    );
}

#[tokio::test]
async fn set_parked_requires_lease_and_artifacts_record_rejects_stale_lease() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let op = claimed_spawn_started_operation(&repo).await;
    let deadline = now_ms() + 10_000;

    assert!(
        repo.set_parked(&op, deadline).await.unwrap().is_none(),
        "parking without recorded artifacts must miss"
    );

    let mut stale = op.clone();
    stale.lease_owner = Some("stale-driver".into());
    assert!(
        repo.record_spawn_artifacts(&stale, &sample_spawn_artifacts())
            .await
            .is_err(),
        "stale lease cannot record artifacts"
    );

    repo.record_spawn_artifacts(&op, &sample_spawn_artifacts())
        .await
        .unwrap();
    let parked = repo
        .set_parked(&op, deadline)
        .await
        .unwrap()
        .expect("leased op with artifacts parks");
    assert_eq!(parked.phase, Phase::Parked);
    assert!(parked.lease_owner.is_none());
    assert!(parked.lease_until_ms.is_none());
    assert!(parked.spawn_artifacts.is_some());
    assert_eq!(parked.parked_deadline_ms, Some(deadline));
    assert!(parked.parked_at_ms.is_some());

    assert!(
        repo.set_parked(&op, deadline).await.unwrap().is_none(),
        "old lease cannot park again after lease clear"
    );
}

#[tokio::test]
async fn complete_parked_tx_splices_result_and_double_complete_is_resolved() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let op = parked_operation(&repo, now_ms() + 10_000).await;

    let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
    let completion = complete_parked_tx(
        &mut tx,
        &op.id,
        &ParkedOutcome::Succeeded {
            result: json!({ "parked": "done" }),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(completion, ParkedCompletion::Completed(_)));

    let result = repo.operation_result(&op.id).await.unwrap().unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Succeeded { ref result }
            if result == &json!({ "parked": "done" })
    ));

    let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
    let second = complete_parked_tx(
        &mut tx,
        &op.id,
        &ParkedOutcome::Succeeded {
            result: json!({ "ignored": true }),
        },
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert!(matches!(
        second,
        ParkedCompletion::AlreadyResolved {
            phase: PhaseTag::Succeeded
        }
    ));
}

#[tokio::test]
async fn complete_after_compensating_and_cancel_after_complete_are_noops() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let op = parked_operation(&repo, now_ms() + 10_000).await;
    let claimed = repo.claim_parked(&op.id).await.unwrap().unwrap();
    let output = required_output(&claimed).unwrap().clone();
    let state = CompensationStateVersioned {
        version: 1,
        from_phase: PhaseTag::Parked,
        reason: "cancel".into(),
        steps: Vec::new(),
    };
    repo.set_compensating(&claimed, &state, &output)
        .await
        .unwrap()
        .expect("claim flips to compensating");

    let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
    let completion = complete_parked_tx(
        &mut tx,
        &op.id,
        &ParkedOutcome::Failed {
            last_error: "late".into(),
            last_error_class: Some("late".into()),
        },
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert!(matches!(
        completion,
        ParkedCompletion::AlreadyResolved {
            phase: PhaseTag::Compensating
        }
    ));

    let op = parked_operation(&repo, now_ms() + 10_000).await;
    let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
    assert!(matches!(
        complete_parked_tx(
            &mut tx,
            &op.id,
            &ParkedOutcome::Succeeded {
                result: json!({ "ok": true }),
            },
        )
        .await
        .unwrap(),
        ParkedCompletion::Completed(_)
    ));
    tx.commit().await.unwrap();

    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn crate::db::RouteRepo> = Arc::new(sqlx_repo);
    let operation_repo = Arc::new(repo);
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let runtime = OperationRuntime::new_unchecked(
        operation_repo.clone(),
        Vec::new(),
        EventBus::new(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            Arc::new(DaemonClient::new_stub()),
            terminal_renderer,
            EventBus::new(),
            completion,
        ),
    );
    assert!(!runtime.cancel_parked(&op.id, "too late").await.unwrap());
}

#[tokio::test]
async fn same_idempotency_key_different_hash_conflicts() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let key = OperationKey {
        operation_key: "op-a".into(),
        idempotency_key: Some("same-key".into()),
        payload_hash: "hash-a".into(),
    };
    let payload = json!({ "wave_id": "wave-a" });
    let first = repo
        .insert_operation("terminal-create", key, payload.clone())
        .await
        .unwrap();
    assert!(!first.is_empty());

    let err = repo
        .insert_operation(
            "terminal-create",
            OperationKey {
                operation_key: "op-b".into(),
                idempotency_key: Some("same-key".into()),
                payload_hash: "hash-b".into(),
            },
            payload,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CalmError::Conflict(_)));
}

#[tokio::test]
async fn operation_event_append_creates_wave_vcs_commit() {
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::{
        append_decision_event_in_tx, begin_immediate_tx, card_create_with_id_tx,
    };
    use crate::event::{Event, EventScope};
    use crate::ids::{ActorId, CardId};
    use crate::model::{CardRole, NewCard, NewCove, NewWave};
    use crate::routes::theme::RequestTheme;
    use crate::wave_report::WaveReportPayload;
    use calm_truth::decision_gate::PermissiveGate;

    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cove = sqlx_repo
        .cove_create(NewCove {
            name: "cove".into(),
            color: "#abcdef".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = sqlx_repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "wave".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let roles = CardRoleCache::new();
    let card_id = new_id();
    let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
    let report = card_create_with_id_tx(
        &mut tx,
        card_id.clone(),
        NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "wave-report".into(),
            sort: None,
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
        },
        CardRole::ReportCard,
        false,
        &roles,
    )
    .await
    .unwrap();
    let scope = EventScope::Card {
        card: CardId::from(card_id),
        wave: wave.id.clone(),
        cove: cove.id.clone(),
    };
    let event = Event::CardAdded(report);
    let event_id = append_decision_event_in_tx(
        &mut tx,
        &PermissiveGate,
        &ActorId::Kernel,
        &scope,
        None,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let head = crate::wave_vcs::head(sqlx_repo.pool(), &wave.id)
        .await
        .unwrap()
        .expect("vcs head");
    let stored_event_id: i64 =
        sqlx::query_scalar("SELECT updated_event_id FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(sqlx_repo.pool())
            .await
            .unwrap();
    assert_eq!(stored_event_id, event_id);
    let author: Option<String> =
        sqlx::query_scalar("SELECT author FROM wave_vcs_commits WHERE hash = ?1")
            .bind(&head)
            .fetch_one(sqlx_repo.pool())
            .await
            .unwrap();
    assert_eq!(author.as_deref(), Some("kernel"));
    assert!(
        crate::wave_vcs::tree_at(sqlx_repo.pool(), &head)
            .await
            .unwrap()
            .expect("tree")
            .entries
            .contains_key("report.md")
    );
}

#[tokio::test]
async fn set_phase_clears_lease_and_rejects_stale_owner() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let op_id = repo
        .insert_operation(
            "terminal-create",
            OperationKey {
                operation_key: "phase-fence-op".into(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": "wave-a" }),
        )
        .await
        .unwrap();
    let mut claimed = repo.claim_drive_batch(1).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let op = claimed.pop().unwrap();
    assert!(op.lease_owner.is_some());

    let next = repo
        .set_phase(&op, Phase::TxCommitted)
        .await
        .unwrap()
        .expect("claimed owner advances");
    assert_eq!(next.phase, Phase::TxCommitted);
    assert!(next.lease_owner.is_none());
    assert!(next.lease_until_ms.is_none());

    let stale = repo.set_phase(&op, Phase::SpawnStarted).await.unwrap();
    assert!(
        stale.is_none(),
        "stale owner must not advance after set_phase clears the lease"
    );
    let stored = repo.get_operation(&op_id).await.unwrap().unwrap();
    assert_eq!(stored.phase, Phase::TxCommitted);
}

#[tokio::test]
async fn stale_driver_cannot_win_final_transition_after_reclaim() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let op_id = repo
        .insert_operation(
            "terminal-create",
            OperationKey {
                operation_key: "final-fence-op".into(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": "wave-a" }),
        )
        .await
        .unwrap();
    let now = now_ms();
    sqlx::query(
        r#"UPDATE operations
               SET phase = 'spawn_succeeded',
                   lease_owner = 'driver-a',
                   lease_until_ms = ?1,
                   updated_at_ms = ?2
               WHERE id = ?3"#,
    )
    .bind(now - 1)
    .bind(now)
    .bind(&op_id)
    .execute(sqlx_repo.pool())
    .await
    .unwrap();
    let stale_driver = repo.get_operation(&op_id).await.unwrap().unwrap();
    assert_eq!(stale_driver.lease_owner.as_deref(), Some("driver-a"));

    let mut claimed = repo.claim_drive_batch(1).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let driver_b = claimed.pop().unwrap();
    assert_ne!(driver_b.lease_owner, stale_driver.lease_owner);

    let stale = repo
        .set_phase(&stale_driver, Phase::Succeeded)
        .await
        .unwrap();
    assert!(stale.is_none(), "driver A's stale final transition loses");
    let winner = repo
        .set_phase(&driver_b, Phase::Succeeded)
        .await
        .unwrap()
        .expect("driver B owns the final transition");
    assert_eq!(winner.phase, Phase::Succeeded);

    let stored = repo.get_operation(&op_id).await.unwrap().unwrap();
    assert_eq!(stored.phase, Phase::Succeeded);
    assert!(stored.lease_owner.is_none());
}

#[tokio::test]
async fn claim_drive_batch_excludes_parked_and_claim_parked_is_exact_phase() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let parked = parked_operation(&repo, now_ms() + 10_000).await;

    assert!(
        repo.claim_drive_batch(32).await.unwrap().is_empty(),
        "parked operations are not drive-claimable"
    );
    assert!(
        repo.claim_parked(&parked.id).await.unwrap().is_some(),
        "parked operations are claimable through the exact-phase path"
    );

    let compensating = parked_operation(&repo, now_ms() + 10_000).await;
    sqlx::query("UPDATE operations SET phase = 'compensating', lease_owner = NULL WHERE id = ?1")
        .bind(&compensating.id)
        .execute(sqlx_repo.pool())
        .await
        .unwrap();
    assert!(repo.claim_parked(&compensating.id).await.unwrap().is_none());

    let terminal = parked_operation(&repo, now_ms() + 10_000).await;
    sqlx::query("UPDATE operations SET phase = 'succeeded', lease_owner = NULL WHERE id = ?1")
        .bind(&terminal.id)
        .execute(sqlx_repo.pool())
        .await
        .unwrap();
    assert!(repo.claim_parked(&terminal.id).await.unwrap().is_none());
}

#[tokio::test]
async fn claim_parked_fetch_misses_when_completion_wins_after_update() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let parked = parked_operation(&repo, now_ms() + 10_000).await;
    let now = now_ms();
    let lease_owner = new_id();
    let result = sqlx::query(
        r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase = 'parked'
                 AND (lease_owner IS NULL OR lease_until_ms < ?3)"#,
    )
    .bind(&lease_owner)
    .bind(now + OPERATION_LEASE_MS)
    .bind(now)
    .bind(&parked.id)
    .execute(sqlx_repo.pool())
    .await
    .unwrap();
    assert_eq!(result.rows_affected(), 1);

    let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
    assert!(matches!(
        complete_parked_tx(
            &mut tx,
            &parked.id,
            &ParkedOutcome::Succeeded {
                result: json!({ "winner": "completion" }),
            },
        )
        .await
        .unwrap(),
        ParkedCompletion::Completed(_)
    ));
    tx.commit().await.unwrap();

    assert!(
        fetch_claimed_parked(sqlx_repo.pool(), &parked.id, &lease_owner)
            .await
            .unwrap()
            .is_none(),
        "post-claim fetch must miss after completion clears the lease"
    );
}

#[tokio::test]
async fn completion_clears_lease_so_claimed_deadline_write_loses_ordering_b() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let repo = SqlxOperationRepo::new(sqlx_repo.pool().clone());
    let parked = parked_operation(&repo, now_ms() + 10_000).await;
    let claimed = repo.claim_parked(&parked.id).await.unwrap().unwrap();
    assert!(claimed.lease_owner.is_some());

    let mut tx = begin_immediate_tx(sqlx_repo.pool()).await.unwrap();
    assert!(matches!(
        complete_parked_tx(
            &mut tx,
            &parked.id,
            &ParkedOutcome::Succeeded {
                result: json!({ "winner": "completion" }),
            },
        )
        .await
        .unwrap(),
        ParkedCompletion::Completed(_)
    ));
    tx.commit().await.unwrap();

    assert!(
        repo.mark_failed(
            &claimed,
            "deadline".into(),
            PhaseTag::Parked,
            Some("parked_deadline".into()),
        )
        .await
        .unwrap()
        .is_none(),
        "completion cleared the claim lease so mark_failed cannot overwrite"
    );
    let result = repo.operation_result(&parked.id).await.unwrap().unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Succeeded { ref result }
            if result == &json!({ "winner": "completion" })
    ));
}

#[tokio::test]
async fn recover_on_boot_plan_contains_verify_parked_items() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let parked = parked_operation(repo.as_ref(), now_ms() + 10_000).await;
    let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = Arc::new(TestParkingAdapter {
        observer_runs,
        record_artifacts: true,
        steal_lease_after_artifacts: false,
    });
    let runtime = test_runtime(sqlx_repo, repo, vec![adapter]);

    let plan = runtime.recover_on_boot().await.unwrap();
    assert!(plan.items.iter().any(|item| {
        matches!(item, RecoveryItem::VerifyParked { op_id } if op_id == &parked.id)
    }));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn recover_on_boot_reclaims_non_recoverable_workspace_lease_from_old_boot() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        &sqlx_repo,
        crate::model::NewCove {
            name: "lease reclaim".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        &sqlx_repo,
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "lease reclaim".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        &sqlx_repo,
        crate::model::NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        },
    )
    .await
    .unwrap();

    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let op_id = repo
        .insert_operation(
            "codex-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(new_id()),
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": wave.id.clone() }),
        )
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE operations
               SET phase = 'succeeded',
                   updated_at_ms = ?1
               WHERE id = ?2"#,
    )
    .bind(now_ms())
    .bind(&op_id)
    .execute(&repo.pool)
    .await
    .unwrap();

    let lease_id = new_id();
    let path = format!(".claude/worktrees/{}/{}", wave.id, card.id);
    std::fs::create_dir_all(&path).unwrap();
    let now = now_ms();
    let stale_boot = stale_boot_id();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, ?7, ?8, ?8)"#,
    )
    .bind(&lease_id)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(&path)
    .bind(&op_id)
    .bind(now + 60_000)
    .bind(&stale_boot)
    .bind(now)
    .execute(&repo.pool)
    .await
    .unwrap();

    let runtime = test_runtime(sqlx_repo, repo.clone(), vec![]);
    let plan = runtime.recover_on_boot().await.unwrap();
    assert!(plan.items.is_empty());

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(state, "released");
    assert!(
        std::path::Path::new(&path).exists(),
        "boot reclaim releases only the lease row"
    );
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(released_events, 1);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn recover_on_boot_releases_workspace_lease_row_without_dir_cleanup() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        &sqlx_repo,
        crate::model::NewCove {
            name: "lease reclaim removal failure".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        &sqlx_repo,
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "lease reclaim removal failure".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        &sqlx_repo,
        crate::model::NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        },
    )
    .await
    .unwrap();

    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let op_id = repo
        .insert_operation(
            "codex-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(new_id()),
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": wave.id.clone() }),
        )
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE operations
               SET phase = 'succeeded',
                   updated_at_ms = ?1
               WHERE id = ?2"#,
    )
    .bind(now_ms())
    .bind(&op_id)
    .execute(&repo.pool)
    .await
    .unwrap();

    let tempdir = tempfile::tempdir().unwrap();
    let path_buf = tempdir.path().join("workspace");
    std::fs::create_dir_all(&path_buf).unwrap();
    let path = path_buf.to_str().unwrap().to_string();
    let lease_id = new_id();
    let now = now_ms();
    let stale_boot = stale_boot_id();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, ?7, ?8, ?8)"#,
    )
    .bind(&lease_id)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(&path)
    .bind(&op_id)
    .bind(now + 60_000)
    .bind(&stale_boot)
    .bind(now)
    .execute(&repo.pool)
    .await
    .unwrap();

    let runtime = test_runtime(sqlx_repo, repo.clone(), vec![]);
    let plan = runtime
        .recover_on_boot()
        .await
        .expect("boot reclaim releases the workspace lease row");
    assert!(plan.items.is_empty());

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(state, "released");
    assert!(
        path_buf.exists(),
        "boot reclaim leaves workspace artifacts for wave cleanup"
    );

    let replacement = new_id();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, 'held', 'replacement-owner', ?5, 'replacement-boot', ?6, ?6)"#,
    )
    .bind(&replacement)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(&path)
    .bind(now + 120_000)
    .bind(now + 1)
    .execute(&repo.pool)
    .await
    .expect("released lease row frees the active path index");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn recover_on_boot_keeps_non_recoverable_workspace_lease_from_same_boot() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        &sqlx_repo,
        crate::model::NewCove {
            name: "lease same boot".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        &sqlx_repo,
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "lease same boot".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        &sqlx_repo,
        crate::model::NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        },
    )
    .await
    .unwrap();

    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let op_id = repo
        .insert_operation(
            "codex-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(new_id()),
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": wave.id.clone() }),
        )
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE operations
               SET phase = 'succeeded',
                   updated_at_ms = ?1
               WHERE id = ?2"#,
    )
    .bind(now_ms())
    .bind(&op_id)
    .execute(&repo.pool)
    .await
    .unwrap();

    let lease_id = new_id();
    let path = format!(".claude/worktrees/{}/{}", wave.id, card.id);
    std::fs::create_dir_all(&path).unwrap();
    let now = now_ms();
    let boot_id = crate::proc_identity::read_boot_id().expect("current boot id");
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, ?7, ?8, ?8)"#,
    )
    .bind(&lease_id)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(&path)
    .bind(&op_id)
    .bind(now + 60_000)
    .bind(&boot_id)
    .bind(now)
    .execute(&repo.pool)
    .await
    .unwrap();

    let runtime = test_runtime(sqlx_repo, repo.clone(), vec![]);
    let plan = runtime.recover_on_boot().await.unwrap();
    assert!(plan.items.is_empty());

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(state, "held");
    assert!(
        std::path::Path::new(&path).exists(),
        "same-boot non-recoverable lease may belong to a live codex worker"
    );
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(released_events, 0);
    std::fs::remove_dir_all(&path).unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn recover_on_boot_keeps_recoverable_workspace_lease_from_old_boot() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        &sqlx_repo,
        crate::model::NewCove {
            name: "lease recoverable".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        &sqlx_repo,
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "lease recoverable".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        &sqlx_repo,
        crate::model::NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        },
    )
    .await
    .unwrap();

    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let op_id = repo
        .insert_operation(
            "park-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": wave.id.clone() }),
        )
        .await
        .unwrap();
    let output = TxOutput::new(
        "card",
        Some(card.id.to_string()),
        json!({ "prepared": true }),
    );
    sqlx::query(
        r#"UPDATE operations
               SET phase = 'tx_committed',
                   tx_output_json = ?1,
                   target_type = ?2,
                   target_id = ?3,
                   target_json = ?4,
                   lease_owner = NULL,
                   lease_until_ms = NULL
               WHERE id = ?5"#,
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&output.target_type)
    .bind(&output.target_id)
    .bind(
        serde_json::to_string(&json!({
            "type": output.target_type,
            "id": output.target_id,
        }))
        .unwrap(),
    )
    .bind(&op_id)
    .execute(&repo.pool)
    .await
    .unwrap();

    let lease_id = new_id();
    let path = format!(".claude/worktrees/{}/{}", wave.id, card.id);
    std::fs::create_dir_all(&path).unwrap();
    let now = now_ms();
    let stale_boot = stale_boot_id();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, ?7, ?8, ?8)"#,
    )
    .bind(&lease_id)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(&path)
    .bind(&op_id)
    .bind(now + 60_000)
    .bind(&stale_boot)
    .bind(now)
    .execute(&repo.pool)
    .await
    .unwrap();

    let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = Arc::new(TestParkingAdapter {
        observer_runs,
        record_artifacts: true,
        steal_lease_after_artifacts: false,
    });
    let runtime = test_runtime(sqlx_repo, repo.clone(), vec![adapter]);
    let plan = runtime.recover_on_boot().await.unwrap();
    assert!(plan.items.iter().any(|item| {
        matches!(
            item,
            RecoveryItem::Recover {
                op_id: planned,
                from_phase: Phase::TxCommitted,
                ..
            } if planned == &op_id
        )
    }));

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(state, "held");
    assert!(
        std::path::Path::new(&path).exists(),
        "boot reclaim must not remove a recoverable operation cwd"
    );
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(released_events, 0);

    runtime.apply_recovery(plan).await.unwrap();
    let stored = repo.get_operation(&op_id).await.unwrap().unwrap();
    assert_eq!(stored.phase, Phase::Parked);
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(state, "held");
    assert!(
        std::path::Path::new(&path).exists(),
        "normal recovery must keep the worker cwd available"
    );
    std::fs::remove_dir_all(&path).unwrap();
}

#[tokio::test]
async fn recover_on_boot_finishes_releasing_workspace_lease() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        &sqlx_repo,
        crate::model::NewCove {
            name: "lease releasing".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        &sqlx_repo,
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "lease releasing".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let card = crate::db::RepoSyncDomainRaw::card_create(
        &sqlx_repo,
        crate::model::NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        },
    )
    .await
    .unwrap();
    let lease_id = new_id();
    let path = format!(".claude/worktrees/{}/{}", wave.id, card.id);
    std::fs::create_dir_all(&path).unwrap();
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, 'releasing', 'release-owner', ?5, 'dead-boot', ?6, ?6)"#,
    )
    .bind(&lease_id)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(&path)
    .bind(now + 60_000)
    .bind(now)
    .execute(sqlx_repo.pool())
    .await
    .unwrap();

    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let runtime = test_runtime(sqlx_repo, repo.clone(), vec![]);
    let plan = runtime.recover_on_boot().await.unwrap();
    assert!(plan.items.is_empty());

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(state, "released");
    assert!(
        std::path::Path::new(&path).exists(),
        "boot reclaim finishes stale releasing row without workspace teardown"
    );
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    assert_eq!(released_events, 1);

    let replacement = new_id();
    sqlx::query(
        r#"INSERT INTO workspace_leases (
                   lease_id, card_id, wave_id, path, state, lease_owner,
                   lease_until_ms, boot_id, created_at_ms, updated_at_ms
               )
               VALUES (?1, ?2, ?3, ?4, 'held', 'new-owner', ?5, 'new-boot', ?6, ?6)"#,
    )
    .bind(&replacement)
    .bind(card.id.as_str())
    .bind(wave.id.as_str())
    .bind(&path)
    .bind(now + 120_000)
    .bind(now + 1)
    .execute(&repo.pool)
    .await
    .unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn boot_leave_parked_clears_abandoned_future_lease() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let parked = parked_operation(repo.as_ref(), now_ms() + 10_000).await;
    let (_child, artifacts) = live_child_spawn_artifacts();
    let artifacts_json = serde_json::to_string(&artifacts).unwrap();
    let now = now_ms();
    sqlx::query(
        r#"UPDATE operations
               SET spawn_artifacts_json = ?1,
                   lease_owner = 'abandoned-boot-lease',
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4
                 AND phase = 'parked'"#,
    )
    .bind(artifacts_json)
    .bind(now + OPERATION_LEASE_MS)
    .bind(now)
    .bind(&parked.id)
    .execute(sqlx_repo.pool())
    .await
    .unwrap();

    let before = repo.get_operation(&parked.id).await.unwrap().unwrap();
    assert_eq!(before.lease_owner.as_deref(), Some("abandoned-boot-lease"));
    assert!(before.lease_until_ms.is_some_and(|lease| lease > now_ms()));

    let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = Arc::new(TestParkingAdapter {
        observer_runs,
        record_artifacts: true,
        steal_lease_after_artifacts: false,
    });
    let runtime = test_runtime(sqlx_repo, repo.clone(), vec![adapter]);
    let plan = runtime.recover_on_boot().await.unwrap();

    runtime.apply_recovery(plan).await.unwrap();

    let stored = repo.get_operation(&parked.id).await.unwrap().unwrap();
    assert_eq!(stored.phase, Phase::Parked);
    assert!(stored.lease_owner.is_none());
    assert!(stored.lease_until_ms.is_none());

    let claimed = repo.claim_parked(&parked.id).await.unwrap();
    assert!(
        claimed.is_some(),
        "steady-state claim must not wait for the abandoned boot lease"
    );
}

#[tokio::test]
async fn parked_return_without_artifacts_fails_and_drops_observer() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = Arc::new(TestParkingAdapter {
        observer_runs: observer_runs.clone(),
        record_artifacts: false,
        steal_lease_after_artifacts: false,
    });
    let runtime = test_runtime(sqlx_repo, repo, vec![adapter]);
    let op_id = runtime
        .submit(
            "park-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": "wave-a" }),
        )
        .await
        .unwrap();

    let result = runtime.wait(&op_id).await.unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Failed {
            from_phase: PhaseTag::SpawnStarted,
            ..
        }
    ));
    assert_eq!(
        observer_runs.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "observer must be dropped when set_parked fails the artifact fence"
    );
}

#[tokio::test]
async fn set_parked_lost_lease_after_artifacts_drops_observer() {
    let sqlx_repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let pool = sqlx_repo.pool().clone();
    let repo = Arc::new(SqlxOperationRepo::new(pool));
    let observer_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let adapter = Arc::new(TestParkingAdapter {
        observer_runs: observer_runs.clone(),
        record_artifacts: true,
        steal_lease_after_artifacts: true,
    });
    let runtime = test_runtime(sqlx_repo, repo.clone(), vec![adapter]);
    let op_id = runtime
        .submit(
            "park-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": "wave-a" }),
        )
        .await
        .unwrap();

    tokio::task::yield_now().await;

    let stored = repo.get_operation(&op_id).await.unwrap().unwrap();
    assert_eq!(stored.phase, Phase::SpawnStarted);
    assert_eq!(stored.lease_owner.as_deref(), Some("stolen-driver"));
    assert!(stored.spawn_artifacts.is_some());
    assert_eq!(
        observer_runs.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "observer must be dropped when set_parked loses the lease"
    );
}

async fn claimed_spawn_started_operation(repo: &SqlxOperationRepo) -> Operation {
    let op_id = repo
        .insert_operation(
            "park-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({ "wave_id": "wave-a" }),
        )
        .await
        .unwrap();
    let now = now_ms();
    let lease_owner = new_id();
    sqlx::query(
        r#"UPDATE operations
               SET lease_owner = ?1,
                   lease_until_ms = ?2,
                   updated_at_ms = ?3
               WHERE id = ?4"#,
    )
    .bind(&lease_owner)
    .bind(now + OPERATION_LEASE_MS)
    .bind(now)
    .bind(&op_id)
    .execute(&repo.pool)
    .await
    .unwrap();
    let output = TxOutput::new("unknown", None, json!({ "initial": true }));
    sqlx::query(
        r#"UPDATE operations
               SET phase = 'spawn_started',
                   tx_output_json = ?1,
                   target_type = ?2,
                   target_id = ?3,
                   target_json = ?4
               WHERE id = ?5"#,
    )
    .bind(serde_json::to_string(&output).unwrap())
    .bind(&output.target_type)
    .bind(&output.target_id)
    .bind(
        serde_json::to_string(&json!({
            "type": output.target_type,
            "id": output.target_id,
        }))
        .unwrap(),
    )
    .bind(&op_id)
    .execute(&repo.pool)
    .await
    .unwrap();
    repo.get_operation(&op_id).await.unwrap().unwrap()
}

pub(super) async fn parked_operation(
    repo: &SqlxOperationRepo,
    deadline_ms: TimestampMs,
) -> Operation {
    let op = claimed_spawn_started_operation(repo).await;
    repo.record_spawn_artifacts(&op, &sample_spawn_artifacts())
        .await
        .unwrap();
    repo.set_parked(&op, deadline_ms)
        .await
        .unwrap()
        .expect("operation parks")
}

fn sample_spawn_artifacts() -> SpawnArtifacts {
    SpawnArtifacts {
        pid: 1,
        pgid: 1,
        start_time: 1,
        boot_id: "boot".into(),
        log_path: None,
        extra: Value::Null,
    }
}

#[cfg(target_os = "linux")]
fn stale_boot_id() -> String {
    let current = crate::proc_identity::read_boot_id().expect("current boot id");
    let stale = "00000000-0000-0000-0000-000000000000";
    assert_ne!(current, stale, "test stale boot id must differ from host");
    stale.into()
}

#[cfg(target_os = "linux")]
struct ChildGuard(std::process::Child);

#[cfg(target_os = "linux")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
fn live_child_spawn_artifacts() -> (ChildGuard, SpawnArtifacts) {
    let child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn live child");
    let pid = i32::try_from(child.id()).expect("child pid fits i32");
    let start_time = crate::proc_identity::read_proc_start_time(pid).expect("child start time");
    let boot_id = crate::proc_identity::read_boot_id().expect("current boot id");
    let artifacts = SpawnArtifacts {
        pid,
        pgid: pid,
        start_time,
        boot_id,
        log_path: None,
        extra: Value::Null,
    };
    assert!(parked_artifacts_alive(&artifacts));
    (ChildGuard(child), artifacts)
}

fn test_runtime(
    sqlx_repo: crate::db::sqlite::SqlxRepo,
    operation_repo: Arc<SqlxOperationRepo>,
    adapters: Vec<Arc<dyn ProviderAdapter>>,
) -> OperationRuntime {
    let events = EventBus::new();
    let completion = OperationCompletionBus::new();
    let route_repo: Arc<dyn crate::db::RouteRepo> = Arc::new(sqlx_repo);
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    OperationRuntime::new_unchecked(
        operation_repo.clone(),
        adapters,
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            Arc::new(DaemonClient::new_stub()),
            terminal_renderer,
            events,
            completion,
        ),
    )
}

struct TestParkingAdapter {
    observer_runs: Arc<std::sync::atomic::AtomicUsize>,
    record_artifacts: bool,
    steal_lease_after_artifacts: bool,
}

#[async_trait]
impl ProviderAdapter for TestParkingAdapter {
    fn kind(&self) -> &'static str {
        "park-test"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::SpawnStarted,
            PhaseTag::Parked,
            PhaseTag::Compensating,
            PhaseTag::Failed,
        ]
    }

    async fn validate(&self, _input: &Value) -> Result<()> {
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        Ok(TxOutput::new("unknown", None, json!({ "prepared": true })))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        if self.record_artifacts {
            ctx.record_spawn_artifacts(op, &sample_spawn_artifacts())
                .await?;
            if self.steal_lease_after_artifacts {
                let pool = ctx.operation_repo.sqlite_pool();
                let now = now_ms();
                let result = sqlx::query(
                    r#"UPDATE operations
                           SET lease_owner = 'stolen-driver',
                               lease_until_ms = ?1,
                               updated_at_ms = ?2
                           WHERE id = ?3
                             AND phase = 'spawn_started'
                             AND lease_owner = ?4"#,
                )
                .bind(now + OPERATION_LEASE_MS)
                .bind(now)
                .bind(&op.id)
                .bind(required_lease_owner(op)?)
                .execute(&pool)
                .await?;
                if result.rows_affected() == 0 {
                    return Err(CalmError::Internal(
                        "test adapter failed to steal operation lease".into(),
                    ));
                }
            }
        }
        let observer_runs = self.observer_runs.clone();
        Ok(SpawnOutcome::Parked {
            deadline_ms: now_ms() + 10_000,
            observer: Box::pin(async move {
                observer_runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        })
    }

    async fn recover_parked(
        &self,
        _op: &Operation,
        _artifacts: &SpawnArtifacts,
        _alive: bool,
        _mode: RecoveryMode,
        _ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        Ok(ParkedRecovery::LeaveParked)
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: Vec::new(),
        })
    }

    async fn compensate_step(
        &self,
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Ok(())
    }
}
