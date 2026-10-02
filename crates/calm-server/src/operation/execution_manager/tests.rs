use super::*;
use crate::db::RepoSyncDomainRaw;
use crate::model::{NewArea, NewCard, NewTrack};
use crate::operation::workspace_lease::execution_guard::{NativeProvider, bind_execution};
use std::sync::atomic::{AtomicBool, Ordering};

struct RegisteredBackend {
    stopped: AtomicBool,
    uncertain: bool,
    wrong_generation: bool,
}
#[async_trait::async_trait]
impl Backend for RegisteredBackend {
    type Request = ();
    type Output = ();
    fn kind(&self) -> BackendKind {
        BackendKind::NativeTurn
    }
    async fn launch(&self, permit: LaunchPermit, _: ()) -> LaunchOutcome<()> {
        assert!(!permit.nonce().is_empty());
        if self.uncertain {
            LaunchOutcome::Uncertain(CalmError::CodexAppServer("lost response".into()))
        } else {
            LaunchOutcome::Started {
                identity: format!("turn-{}", permit.record().holder),
                output: (),
            }
        }
    }
    async fn recover(&self, record: &Record) -> Result<Observation> {
        Ok(Observation {
            execution: if self.wrong_generation {
                "another-generation".into()
            } else {
                record.id.clone()
            },
            identity: Some(format!("turn-{}", record.holder)),
            stopped: self.stopped.load(Ordering::SeqCst),
        })
    }
    async fn stop(&self, record: &Record) -> Result<Observation> {
        self.recover(record).await
    }
}

pub(super) async fn fixture() -> (crate::db::sqlite::SqlxRepo, tempfile::TempDir, String) {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let area = repo
        .area_create(NewArea {
            name: "manager".into(),
            color: "#000000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            area_id: area.id,
            title: "manager".into(),
            sort: None,
            cwd: cwd.path().to_str().unwrap().into(),
            template_input: None,
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    (repo, cwd, track.id.to_string())
}
pub(super) async fn owner(
    repo: &crate::db::sqlite::SqlxRepo,
    cwd: &std::path::Path,
    track: &str,
    holder: &str,
    read: bool,
) -> Owner {
    let card = repo
        .card_create(NewCard {
            track_id: track.to_owned().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap()
        .id
        .to_string();
    bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        &card,
        holder,
        cwd.to_str().unwrap(),
    )
    .await
    .unwrap();
    if read {
        let task = crate::model::new_id();
        let operation = crate::model::new_id();
        sqlx::query(
            "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status, \
             worker_card_id,declared_by,created_at_ms,updated_at_ms) \
            VALUES(?1,?2,?1,'codex','read',?3,'running',?4,'user',0,0)",
        )
        .bind(&task)
        .bind(track)
        .bind(serde_json::json!({"neige_workspace":{"access":"read_only"}}).to_string())
        .bind(&card)
        .execute(repo.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type, \
             target_json,payload_json,phase,created_at_ms,updated_at_ms) \
            VALUES(?1,?1,'codex-worker',?2,'hash','card','{}','{}','succeeded',0,0)")
            .bind(&operation).bind(task).execute(repo.pool()).await.unwrap();
        sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,access_mode,created_at_ms,updated_at_ms) \
            VALUES(?1,?2,?3,?4,'held',?5,'read_only',0,0)")
            .bind(crate::model::new_id()).bind(&card).bind(track).bind(cwd.to_str().unwrap()).bind(operation).execute(repo.pool()).await.unwrap();
    }
    Owner {
        card,
        holder: holder.into(),
    }
}
#[tokio::test]
async fn registered_backend_obeys_resource_lifetime_without_caller_release() {
    let (repo, cwd, track) = fixture().await;
    let a = owner(&repo, cwd.path(), &track, "a", true).await;
    let b = owner(&repo, cwd.path(), &track, "b", true).await;
    let writer = owner(&repo, cwd.path(), &track, "writer", false).await;
    let manager = ExecutionManager::new(repo.pool().clone());
    let backend = RegisteredBackend {
        stopped: AtomicBool::new(false),
        uncertain: false,
        wrong_generation: false,
    };
    let first = manager.submit(&backend, &a, (), None).await.unwrap();
    let second = manager.submit(&backend, &b, (), None).await.unwrap();
    assert!(manager.submit(&backend, &writer, (), None).await.is_err());
    sqlx::query("UPDATE tasks SET status='done'")
        .execute(repo.pool())
        .await
        .unwrap();
    let mut report_tx = repo.pool().begin().await.unwrap();
    let report_events = task_ended_tx(
        &mut report_tx,
        &a.card,
        crate::operation::workspace_lease::ReleaseDelivery::CommitAsTaskEnded,
    )
    .await
    .unwrap();
    assert!(
        report_events.is_empty(),
        "business completion cannot hand off a live execution"
    );
    report_tx.commit().await.unwrap();
    assert!(
        !manager
            .recover(&backend, &first.execution_id)
            .await
            .unwrap(),
        "business report is not stop evidence"
    );
    assert!(
        !manager.cancel(&backend, &first.execution_id).await.unwrap(),
        "unconfirmed cancellation retains readers"
    );
    let wrong = RegisteredBackend {
        stopped: AtomicBool::new(true),
        uncertain: false,
        wrong_generation: true,
    };
    assert!(manager.recover(&wrong, &first.execution_id).await.is_err());
    backend.stopped.store(true, Ordering::SeqCst);
    assert!(
        manager
            .recover(&backend, &first.execution_id)
            .await
            .unwrap()
    );
    assert!(
        manager
            .recover(&backend, &second.execution_id)
            .await
            .unwrap()
    );
    assert!(
        manager
            .recover(&backend, &first.execution_id)
            .await
            .unwrap(),
        "repeat settlement is idempotent"
    );
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workspace_leases WHERE holder_kind='native' AND state='held'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(live, 0);
    let next = manager.submit(&backend, &writer, (), None).await.unwrap();
    assert!(
        manager.recover(&backend, &next.execution_id).await.unwrap(),
        "manager stop also closes ended reader task reservations"
    );
}
#[tokio::test]
async fn registered_backend_uncertain_launch_keeps_manager_owned_reservation() {
    let (repo, cwd, track) = fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "unknown", false).await;
    let backend = RegisteredBackend {
        stopped: AtomicBool::new(false),
        uncertain: true,
        wrong_generation: false,
    };
    let manager = ExecutionManager::new(repo.pool().clone());
    assert!(manager.submit(&backend, &owner, (), None).await.is_err());
    let execution: String = sqlx::query_scalar(
        "SELECT lease_id FROM workspace_leases WHERE holder_kind='native' AND state='held'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert!(!manager.cancel(&backend, &execution).await.unwrap());
    backend.stopped.store(true, Ordering::SeqCst);
    assert!(manager.recover(&backend, &execution).await.unwrap());
}

#[tokio::test]
async fn native_stop_can_precede_writable_task_business_report() {
    let (repo, cwd, track) = fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "rw-task", false).await;
    let task = crate::model::new_id();
    let operation = crate::model::new_id();
    let lease = crate::model::new_id();
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status, \
             worker_card_id,declared_by,created_at_ms,updated_at_ms) \
        VALUES(?1,?2,?1,'codex','write','null','running',?3,'user',0,0)",
    )
    .bind(&task)
    .bind(&track)
    .bind(&owner.card)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type, \
             target_json,payload_json,phase,created_at_ms,updated_at_ms) \
        VALUES(?1,?1,'codex-worker',?2,'hash','card','{}','{}','succeeded',0,0)",
    )
    .bind(&operation)
    .bind(task)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,access_mode,write_root_id, \
         created_at_ms,updated_at_ms) \
        VALUES(?1,?2,?3,?4,'held',?5,'read_write',?1,0,0)")
        .bind(&lease).bind(&owner.card).bind(&track).bind(cwd.path().to_str().unwrap()).bind(operation).execute(repo.pool()).await.unwrap();
    let backend = RegisteredBackend {
        stopped: AtomicBool::new(true),
        uncertain: false,
        wrong_generation: false,
    };
    let manager = ExecutionManager::new(repo.pool().clone());
    let receipt = manager.submit(&backend, &owner, (), None).await.unwrap();
    assert!(
        manager
            .recover(&backend, &receipt.execution_id)
            .await
            .unwrap(),
        "physical stop commits independently of a later task report"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
        .bind(&lease)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        state, "held",
        "a running business task retains its remaining workspace intent"
    );
}

struct ReplyBackend {
    pool: SqlitePool,
}
#[async_trait::async_trait]
impl Backend for ReplyBackend {
    type Request = serde_json::Value;
    type Output = serde_json::Value;
    fn kind(&self) -> BackendKind {
        BackendKind::NativeTurn
    }
    async fn launch(
        &self,
        permit: LaunchPermit,
        reply: Self::Request,
    ) -> LaunchOutcome<Self::Output> {
        let (phase, nonce): (String, String) = sqlx::query_as(
            "SELECT holder_phase,native_client_id FROM workspace_leases WHERE lease_id=?1 AND state='held'"
        ).bind(&permit.record().id).fetch_one(&self.pool).await.unwrap();
        assert_eq!(phase, "issuing");
        assert_eq!(nonce, permit.nonce());
        LaunchOutcome::Started {
            identity: "reply-turn".into(),
            output: reply,
        }
    }
    async fn recover(&self, _: &Record) -> Result<Observation> {
        unreachable!()
    }
    async fn stop(&self, _: &Record) -> Result<Observation> {
        unreachable!()
    }
}
#[tokio::test]
async fn native_ingress_manager_commits_identity_before_returning_complete_output() {
    let (repo, cwd, track) = fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "reply-thread", false).await;
    let manager = ExecutionManager::new(repo.pool().clone());
    let backend = ReplyBackend {
        pool: repo.pool().clone(),
    };
    let output =
        serde_json::json!({"turn":{"id":"reply-turn","items":[]},"future":{"preserved":true}});
    let receipt = manager
        .submit(&backend, &owner, output.clone(), None)
        .await
        .unwrap();
    let (phase, turn): (String, String) = sqlx::query_as(
        "SELECT holder_phase,native_observed_turn_id FROM workspace_leases WHERE lease_id=?1",
    )
    .bind(receipt.execution_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(phase, "running");
    assert_eq!(turn, "reply-turn");
    assert_eq!(receipt.output, output);
}

struct UncertainSessionBackend;
#[async_trait::async_trait]
impl Backend for UncertainSessionBackend {
    type Request = ();
    type Output = ();
    fn kind(&self) -> BackendKind {
        BackendKind::NativeSession
    }
    async fn launch(&self, permit: LaunchPermit, _: ()) -> LaunchOutcome<()> {
        assert!(matches!(permit, LaunchPermit::Write(_)));
        LaunchOutcome::Uncertain(CalmError::Conflict(
            "fixture request has no acknowledgement".into(),
        ))
    }
    async fn recover(&self, record: &Record) -> Result<Observation> {
        Ok(Observation {
            execution: record.id.clone(),
            identity: None,
            stopped: false,
        })
    }
    async fn stop(&self, record: &Record) -> Result<Observation> {
        self.recover(record).await
    }
}

pub(crate) async fn issue_session_fixture(pool: &SqlitePool, execution: &str) {
    let manager = ExecutionManager::new(pool.clone());
    assert!(
        manager
            .launch_reserved(&UncertainSessionBackend, execution, ())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_fence_quiesce_closes_future_generations() {
    let (repo, cwd, track) = fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "closing-fixture", false).await;
    let repo = std::sync::Arc::new(repo);
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    );
    let thread = daemon
        .thread_start_mint_for_card(
            &owner.card,
            crate::shared_codex_appserver::SharedThreadStartParams {
                cwd: cwd.path().to_str().unwrap().into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: crate::shared_codex_appserver::ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    daemon
        .turn_start(
            &thread,
            vec![crate::codex_appserver::InputItem::text("initial")],
            &crate::planner_model::TurnModelSelection::inherit(),
            None,
        )
        .await
        .unwrap();
    assert!(daemon.quiesce_native_thread(&thread).await.unwrap());
    assert!(
        daemon
            .turn_start(
                &thread,
                vec![crate::codex_appserver::InputItem::text("late issuer")],
                &crate::planner_model::TurnModelSelection::inherit(),
                None
            )
            .await
            .is_err(),
        "worker cleanup permanently closes admission before removing its projection"
    );
}

#[tokio::test]
async fn native_fence_stopped_legacy_discovery_does_not_reserve_writer_against_reader() {
    let (repo, cwd, track) = fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "stopped-legacy", false).await;
    sqlx::query(
        "UPDATE workspace_execution_bindings SET scope_phase='recovering' WHERE holder_id=?1",
    )
    .bind(&owner.holder)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,access_mode,created_at_ms,updated_at_ms) \
        VALUES('existing-reader','reader',?1,?2,'held','reader','read_only',0,0)")
        .bind(&track).bind(cwd.path().to_str().unwrap()).execute(repo.pool()).await.unwrap();
    let repo = std::sync::Arc::new(repo);
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    );
    daemon
        .set_native_thread_history_for_test(
            serde_json::json!({"thread":{"id":owner.holder,"cwd":cwd.path(),
        "status":{"type":"idle"},"turns":[{"id":"old-finished","status":"completed","items":[]}]}}),
        )
        .unwrap();
    assert!(
        daemon.quiesce_native_thread(&owner.holder).await.unwrap(),
        "positive stopped facts must not temporarily acquire an overlapping write execution"
    );
    let reader: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id='existing-reader'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(reader, "held");
}

#[tokio::test]
async fn native_fence_stopped_legacy_scope_hands_off_terminal_read_intent() {
    let (repo, cwd, track) = fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "legacy-read-handoff", true).await;
    let task: String = sqlx::query_scalar("SELECT id FROM tasks WHERE worker_card_id=?1")
        .bind(&owner.card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.pool())
        .await
        .unwrap();
    crate::db::sqlite::task_complete_from_worker_tx(
        &mut tx,
        &task,
        &track,
        crate::db::sqlite::TaskReporter::Kernel,
        crate::model::now_ms(),
    )
    .await
    .unwrap();
    assert!(
        task_ended_tx(
            &mut tx,
            &owner.card,
            crate::operation::workspace_lease::ReleaseDelivery::CommitAsTaskEnded
        )
        .await
        .unwrap()
        .is_empty()
    );
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE workspace_execution_bindings SET scope_phase='recovering' WHERE holder_id=?1",
    )
    .bind(&owner.holder)
    .execute(repo.pool())
    .await
    .unwrap();
    let repo = std::sync::Arc::new(repo);
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    );
    daemon.set_native_thread_history_for_test(serde_json::json!({"thread":{"id":owner.holder,"cwd":cwd.path(),
        "status":{"type":"idle"},"turns":[{"id":"read-finished","status":"completed","items":[]}]}})).unwrap();
    assert!(daemon.quiesce_native_thread(&owner.holder).await.unwrap());
    let state: (String, Option<i64>) = sqlx::query_as(
        "SELECT state,read_stop_confirmed_at_ms FROM workspace_leases \
        WHERE card_id=?1 AND holder_kind='task'",
    )
    .bind(&owner.card)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        state.0, "released",
        "provider scope stop settles the exact terminal read task intent"
    );
    assert!(state.1.is_some());
}

#[tokio::test]
async fn native_fence_unknown_scope_keeps_closed_reader_and_deletion_barrier() {
    let (repo, cwd, track) = fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "unknown-closed-scope", false).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.pool())
        .await
        .unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit {
            id: crate::model::new_id(),
            card_id: owner.card.clone(),
            kind: crate::session_projection_repo::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::session_projection_repo::AgentProvider::Codex),
            status: crate::session_projection_repo::WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(owner.holder.clone()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: crate::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE workspace_execution_bindings SET scope_phase='recovering' WHERE holder_id=?1",
    )
    .bind(&owner.holder)
    .execute(repo.pool())
    .await
    .unwrap();
    let repo = std::sync::Arc::new(repo);
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    );
    assert!(daemon.quiesce_native_thread(&owner.holder).await.is_err());
    let phase: String = sqlx::query_scalar(
        "SELECT scope_phase FROM workspace_execution_bindings WHERE holder_id=?1",
    )
    .bind(&owner.holder)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(phase, "closed");
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.pool())
        .await
        .unwrap();
    assert!(
        super::require_worker_cleanup_tx(&mut tx, &owner.card)
            .await
            .is_err()
    );
    assert!(
        !crate::db::sqlite::workspace_available(
            &mut tx,
            &track,
            "",
            calm_types::workspace_access::WorkspaceAccess::ReadOnly,
            Some(cwd.path().to_str().unwrap()),
            None
        )
        .await
        .unwrap()
    );
    tx.rollback().await.unwrap();
}
