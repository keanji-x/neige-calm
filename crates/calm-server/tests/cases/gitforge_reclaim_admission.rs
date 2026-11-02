//! Upgrade fixtures seed an already-admitted legacy operation, then exercise the
//! production execution engine, parked recovery, receipts and HTTP admission.
use super::*;
use workspace_runtime::execution::*;

async fn seed_legacy_reclaim(
    f: &Fixture,
    race_dirty: bool,
) -> (ExecutionId, Box<workspace::reclaim::PreparedReclaim>) {
    let claim = calm_truth::db::sqlite::worktree_events::unmatched_provisioned_worktrees(
        &mut f.b.repo.pool().acquire().await.unwrap(),
    )
    .await
    .unwrap()
    .into_iter()
    .find(|claim| Path::new(&claim.path) == f.clean)
    .unwrap();
    let op = format!("workspace-reclaim:{}:close", claim.event_id);
    let request = workspace::reclaim::ReclaimRequest {
        repo: f.up.clone.clone(),
        path: f.clean.clone(),
        protected: vec![f.dirty.clone(), f.unrecorded.clone()],
        excluded_refs: Vec::new(),
        ownership_conflict: false,
        unknown_terminal: false,
    };
    // Physical preflight remains the real owning workspace implementation. Only
    // persistence setup models an old server, which the new server never admits.
    let workspace::reclaim::Preparation::Ready(prepared) =
        workspace::reclaim::prepare(&request, |_, _| {}).unwrap()
    else {
        panic!("legacy fixture must be eligible before the policy upgrade");
    };
    let target = workspace_runtime::reclaim::ExtraWorktree {
        operation_id: op.clone(),
        provision_id: claim.event_id,
        track_id: claim.track_id,
        card_id: claim.card_id,
        area_id: claim.area_id,
        request,
    };
    let payload = json!({"target":target,"delete_authorized":true});
    let mut intent = workspace_runtime::execution::test_support::intent(f.up.clone.clone());
    intent.metadata.consumer = "workspace-reclaim".into();
    intent.metadata.owner = OperationRef(op.clone());
    intent.metadata.track = TrackId(f.track.clone());
    intent.metadata.alias = ProcessAlias(format!("reclaim:{op}"));
    intent.metadata.program.program = "/bin/sh".into();
    intent.metadata.program.args = [
        "-c".to_owned(),
        "exec \"$@\"".to_owned(),
        "legacy-reclaim".to_owned(),
    ]
    .into_iter()
    .chain(std::iter::once(
        prepared.command.get_program().to_str().unwrap().to_owned(),
    ))
    .chain(
        prepared
            .command
            .get_args()
            .map(|arg| arg.to_str().unwrap().to_owned()),
    )
    .collect();
    intent.metadata.program.environment = prepared
        .command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_str().unwrap().to_owned(),
                    value.to_str().unwrap().to_owned(),
                )
            })
        })
        .collect();
    let id = intent.metadata.id.clone();
    let mut output =
        calm_server::operation::TxOutput::new("track", Some(f.track.clone()), Value::Null);
    output.data = json!({"workspace_reclaim":{"execution_id":id}});
    let engine = f._execution.lifecycle.engine().unwrap();
    let db = f.b.repo.db().clone();
    let ticket = db.writer().with_writer(|session| Box::pin(async move {
        let mut tx = session.begin_immediate().await.unwrap();
        sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_id,target_json,payload_json,phase,tx_output_json,parked_at_ms,parked_deadline_ms,created_at_ms,updated_at_ms) \
            VALUES (?1,?1,'workspace-reclaim',?1,'legacy-fixture','track',?2,'{}',?3,'parked',?4,1,9223372036854775807,1,1)")
            .bind(&op).bind(&f.track).bind(payload.to_string()).bind(serde_json::to_string(&output).unwrap())
            .execute(&mut **tx).await.unwrap();
        let ticket = engine.admit(&mut tx,intent).await.unwrap();
        tx.commit().await.unwrap();
        ticket
    })).await.unwrap();
    if race_dirty {
        std::fs::write(f.clean.join("raced-user-work"), b"preserve").unwrap();
    }
    f._execution
        .lifecycle
        .engine()
        .unwrap()
        .drive(ticket.committed());
    (id, prepared)
}

async fn wait_settled(f: &Fixture, id: &ExecutionId) {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let settled: bool = sqlx::query_scalar(
                "SELECT settled_at_ms IS NOT NULL FROM workspace_executions WHERE execution_id=?",
            )
            .bind(id.as_str())
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
            if settled {
                break;
            }
            f._execution.lifecycle.drive([id.clone()]);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn retirement_legacy_success_recovers_after_receipt_failure_and_owner_deletion() {
    let f = Fixture::new().await;
    let (status, body) = f.b.delete_track(&f.track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (id, _prepared) = seed_legacy_reclaim(&f, false).await;
    wait_settled(&f, &id).await;
    assert!(!f.clean.exists());
    f.b.repo.db().writer().execute(sqlx::query("CREATE TRIGGER hold_legacy_receipt BEFORE INSERT ON events WHEN NEW.kind='worktree.removed' BEGIN SELECT RAISE(FAIL,'receipt unavailable'); END")).await.unwrap();
    let runtime = recover_reclaims(&f).await;
    runtime.sweep_parked().await.unwrap();
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='worktree.removed' AND scope_track=?",
    )
    .bind(&f.track)
    .fetch_one(f.b.repo.pool())
    .await
    .unwrap();
    assert_eq!(before, 0);
    f.b.repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER hold_legacy_receipt"))
        .await
        .unwrap();
    // Expiry is the existing lease retry protocol after a failed completion.
    f.b.repo.db().writer().execute(sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE kind='workspace-reclaim' AND phase='parked'")).await.unwrap();
    runtime.sweep_parked().await.unwrap();
    runtime.sweep_parked().await.unwrap();
    let after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='worktree.removed' AND scope_track=?",
    )
    .bind(&f.track)
    .fetch_one(f.b.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        after, 1,
        "legacy success receipts exactly once after repair"
    );
    assert!(f.kernel.exists());
}

#[tokio::test]
async fn retirement_legacy_nonzero_exit_is_not_successful_cleanup() {
    let f = Fixture::new().await;
    f.close().await;
    let (id, _prepared) = seed_legacy_reclaim(&f, true).await;
    wait_settled(&f, &id).await;
    let runtime = recover_reclaims(&f).await;
    runtime.sweep_parked().await.unwrap();
    assert_eq!(
        std::fs::read(f.clean.join("raced-user-work")).unwrap(),
        b"preserve"
    );
    let removed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='worktree.removed' AND scope_track=?",
    )
    .bind(&f.track)
    .fetch_one(f.b.repo.pool())
    .await
    .unwrap();
    assert_eq!(removed, 0);
}

#[tokio::test]
async fn retirement_legacy_unknown_effect_keeps_protection_across_runtime_restart() {
    legacy_pending_allows_progress(true).await;
}

#[tokio::test]
async fn reclaim_recovery_legacy_pending_keeps_other_work_progressing() {
    legacy_pending_allows_progress(false).await;
}

async fn legacy_pending_allows_progress(restart: bool) {
    let mut f = Fixture::new().await;
    f.close().await;
    f.b.repo.db().writer().execute(sqlx::query("CREATE TRIGGER hold_legacy_settlement BEFORE UPDATE OF settled_at_ms ON workspace_executions \
        WHEN json_extract(NEW.intent_json,'$.consumer')='workspace-reclaim' BEGIN SELECT RAISE(FAIL,'settlement unavailable'); END")).await.unwrap();
    let (id, _prepared) = seed_legacy_reclaim(&f, false).await;
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while f.clean.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let other = crate::support::planner::historical_track(
        &f.b.state,
        json!({"area_id":f.b.area_id,"title":"other","planner_provider":"codex",
        "theme":calm_server::routes::theme::RequestTheme::default_dark()}),
        None,
    )
    .await
    .unwrap();
    f.b.shutdown_harnesses().await;
    if restart {
        f.b.state = state_for_reclaim(&f.b, &f._execution.socket);
        f.b.state
            .operation_runtime
            .execution()
            .bind(Arc::new(f._execution.lifecycle.engine().unwrap().clone()));
        f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
    }
    assert_pending_pipe_allows_unrelated_worker(&mut f, id.as_str()).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), f.b.state.operation_runtime.sweep_parked())
        .await.unwrap().unwrap();
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(5), recover_reclaims(&f))
        .await.expect("legacy boot recovery returns while settlement is unavailable");
    tokio::time::timeout(std::time::Duration::from_secs(2), recovered.sweep_parked())
        .await.unwrap().unwrap();
    let uri = format!(
        "/api/tracks/{}/terminal-cards",
        other["id"].as_str().unwrap()
    );
    let body = json!({"cwd":f.unrecorded,"program":"true","theme":calm_server::routes::theme::RequestTheme::default_dark()});
    let result = f.b.as_user("POST", &uri, body.clone()).await;
    assert_eq!(
        result.0,
        StatusCode::CONFLICT,
        "unknown legacy effect still protected: {:?}",
        result.1
    );
    assert_eq!(result.1["code"], "workspace_busy");
    f.b.repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER hold_legacy_settlement"))
        .await
        .unwrap();
    wait_settled(&f, &id).await;
    let result = f.b.as_user("POST", &uri, body).await;
    assert_eq!(
        result.0,
        StatusCode::CREATED,
        "legacy fence released only after settlement: {:?}",
        result.1
    );
    f.b.shutdown_harnesses().await;
}

async fn assert_pending_pipe_allows_unrelated_worker(f: &mut Fixture, reclaim: &str) {
    use calm_server::model::TaskStatus;
    let original_state = f.b.state.clone();
    let other_root = f.b.tmp.path().join("other-project");
    std::fs::create_dir(&other_root).unwrap();
    let other_upstream = upstream(&other_root);
    let (status, other) = f.b.create_at(&other_upstream.clone, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other_id = other["id"].as_str().unwrap().to_owned();
    let other = other_id.as_str();
    f.b.shutdown_harnesses().await;
    // Only the optional viewer uses this endpoint. Typed reclaim/Pipe effects
    // remain bound to the real fixture engine; no Codex command is executed.
    let viewer = crate::support::execution::RefuseLegacySpawn::start();
    f.b.state = state_for_reclaim(&f.b, &viewer.socket);
    f.b.state
        .operation_runtime
        .execution()
        .bind(Arc::new(f._execution.lifecycle.engine().unwrap().clone()));
    // Only the provider IO is fake; use the production worker adapter with its
    // required per-card MCP capability rather than an incomplete from_parts hatch.
    let shared = SharedCodexAppServer::new_fake_running_with_pending(f.b.repo.clone(), None);
    let server = calm_server::mcp_server::McpServer::spawn_with_context(
        f.b.state.mcp_context(),
        f.b.tmp.path().join("test-mcp.sock"),
        f.b.tmp.path().join("unused-test-shim"),
        calm_server::mcp_server::build_default_registry(),
    )
    .await
    .unwrap();
    f.b.state =
        f.b.state
            .clone()
            .with_shared_codex_appserver(shared)
            .with_mcp_server(server);
    f.b.state.dispatcher.abort_event_listener_for_test();
    f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
    let mut rev: i64 = sqlx::query_scalar(
        "SELECT json_extract(payload,'$.docRev') FROM cards WHERE track_id=?1 AND kind='track-report'",
    )
    .bind(other)
    .fetch_one(f.b.repo.pool())
    .await
    .unwrap();
    let track = f.b.repo.track_get(other).await.unwrap().unwrap();
    let head = git_stdout(
        Path::new(track.workspace.agent_cwd()),
        ["rev-parse", "HEAD"],
    );
    for payload in [
        json!({"key":"blocked-pipe", "kind":"terminal", "command":"printf forbidden > blocked-ran",
            "priority":10, "ready":true, "declared_by":"user"}),
        json!({"key":"unrelated-review", "kind":"codex", "goal":"Read the independent checkout",
            "access":"read_only", "head":head.trim(), "ready":true, "declared_by":"user"}),
    ] {
        let (status, result) =
            f.b.as_user(
                "POST",
                &format!("/api/tracks/{other}/report/blocks"),
                json!({"kind":"task", "payload":payload, "ifDocRev":rev}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        rev = result["docRev"].as_i64().unwrap();
    }
    let scheduler = f.b.state.scheduler_for_test();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        scheduler.schedule_track(other.into()),
    )
    .await
    .expect("the real Pending Pipe must release the Track scheduling turn");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let review =
                f.b.repo
                    .task_current_get(other, "unrelated-review")
                    .await
                    .unwrap()
                    .unwrap();
            if review.status == TaskStatus::Running {
                break;
            }
            assert_ne!(
                review.status,
                TaskStatus::Failed,
                "{:?}",
                review.status_detail
            );
            scheduler.sweep_all().await;
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the independent managed worker advances while A is still unsettled");
    let blocked =
        f.b.repo
            .task_current_get(other, "blocked-pipe")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(blocked.status, TaskStatus::Dispatched);
    let op =
        f.b.state
            .operation_runtime
            .find_by_kind_and_idempotency("terminal-worker", &blocked.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(op.phase, calm_server::operation::Phase::Pending);
    assert!(op.tx_output.is_none());
    assert!(
        !Path::new(track.workspace.agent_cwd())
            .join("blocked-ran")
            .exists()
    );
    let view =
        calm_server::task_recovery::task_recovery_view(f.b.repo.as_ref(), other, "blocked-pipe")
            .await
            .unwrap();
    let reason = view.current.unwrap().blocking_reason.unwrap();
    assert!(reason.contains(&f.track), "{reason}");
    assert!(reason.contains("reclaim"), "{reason}");
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT settled_at_ms IS NULL FROM workspace_executions WHERE execution_id=?1",
        )
        .bind(reclaim)
        .fetch_one(f.b.repo.pool())
        .await
        .unwrap()
    );
    // Production deletion stops the worker and removes its optional viewer
    // projection before the later global terminal-admission assertions.
    viewer.allow_absence_probes();
    let deleted = f.b.delete_track(other).await;
    assert_eq!(deleted.0, StatusCode::NO_CONTENT, "{deleted:?}");
    f.b.state = original_state;
    f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
}

