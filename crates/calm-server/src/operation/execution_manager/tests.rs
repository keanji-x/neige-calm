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
    fn kind(&self) -> BackendKind {
        BackendKind::NativeTurn
    }
    async fn launch(&self, permit: LaunchPermit, _: ()) -> LaunchOutcome {
        assert!(!permit.nonce().is_empty());
        if self.uncertain {
            LaunchOutcome::Uncertain(CalmError::CodexAppServer("lost response".into()))
        } else {
            LaunchOutcome::Started(format!("turn-{}", permit.record().holder))
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

async fn fixture() -> (crate::db::sqlite::SqlxRepo, tempfile::TempDir, String) {
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
async fn owner(
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
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status, \
             worker_card_id,declared_by,created_at_ms,updated_at_ms) \
            VALUES(?1,?2,?1,'codex','read',?3,'running',?4,'user',0,0)")
            .bind(&task).bind(track).bind(serde_json::json!({"neige_workspace":{"access":"read_only"}}).to_string()).bind(&card).execute(repo.pool()).await.unwrap();
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
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status, \
             worker_card_id,declared_by,created_at_ms,updated_at_ms) \
        VALUES(?1,?2,?1,'codex','write','null','running',?3,'user',0,0)")
        .bind(&task).bind(&track).bind(&owner.card).execute(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type, \
             target_json,payload_json,phase,created_at_ms,updated_at_ms) \
        VALUES(?1,?1,'codex-worker',?2,'hash','card','{}','{}','succeeded',0,0)")
        .bind(&operation).bind(task).execute(repo.pool()).await.unwrap();
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
