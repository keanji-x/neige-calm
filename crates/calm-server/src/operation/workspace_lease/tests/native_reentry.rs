//! Native issuance consumes authenticated lineage, never a same-card shortcut.
use super::*;
use crate::operation::workspace_lease::execution_guard::{
    self, ExecutionWriteGuard, NativeProvider,
};

#[tokio::test]
async fn native_writer_reenters_released_root_held_by_its_descendant() {
    let cwd = tempfile::tempdir().unwrap();
    let (repo, track, card) = lease_fixture(cwd.path()).await;
    execution_guard::bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        &card,
        "thread",
        cwd.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    let parent = ExecutionWriteGuard::acquire_native(
        repo.pool(),
        &card,
        "thread",
        "",
        NativeProvider::Codex,
    )
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    execution_guard::acquire_execution_write_tx(
        &mut tx,
        &track,
        &card,
        "child",
        "terminal",
        cwd.path(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    parent.rejected().await.unwrap(); // No provider issuance was attempted for this fixture.
    let resumed = ExecutionWriteGuard::acquire_native(
        repo.pool(),
        &card,
        "thread",
        "",
        NativeProvider::Codex,
    )
    .await;
    assert!(
        resumed.is_ok(),
        "the authenticated logical writer should resume while its descendant remains held"
    );
    resumed.unwrap().rejected().await.unwrap();
}

#[tokio::test]
async fn native_writer_cannot_borrow_another_owner_or_replace_unknown_issuance() {
    let cwd = tempfile::tempdir().unwrap();
    let (repo, track, card) = lease_fixture(cwd.path()).await;
    execution_guard::bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        &card,
        "thread",
        cwd.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    let parent = ExecutionWriteGuard::acquire_native(
        repo.pool(),
        &card,
        "thread",
        "",
        NativeProvider::Codex,
    )
    .await
    .unwrap();
    assert!(
        ExecutionWriteGuard::acquire_native(
            repo.pool(),
            &card,
            "thread",
            "",
            NativeProvider::Codex
        )
        .await
        .is_err(),
        "an existing unknown native issuance must remain exclusive"
    );
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    execution_guard::acquire_execution_write_tx(
        &mut tx,
        &track,
        &card,
        "child",
        "terminal",
        cwd.path(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    parent.rejected().await.unwrap();
    let other = crate::db::RepoSyncDomainRaw::card_create(
        &repo,
        crate::model::NewCard {
            track_id: track.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        },
    )
    .await
    .unwrap();
    execution_guard::bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        other.id.as_str(),
        "other-thread",
        cwd.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    assert!(
        ExecutionWriteGuard::acquire_native(
            repo.pool(),
            other.id.as_str(),
            "other-thread",
            "",
            NativeProvider::Codex
        )
        .await
        .is_err(),
        "another authenticated caller cannot borrow the root owner's lineage"
    );
}

#[tokio::test]
async fn native_writer_reader_fence_uses_the_persisted_execution_cwd() {
    let track_cwd = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let (repo, track, card) = lease_fixture(track_cwd.path()).await;
    execution_guard::bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        &card,
        "thread",
        cwd.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner, \
        created_at_ms,updated_at_ms,access_mode) VALUES('reader',?1,?2,?3,'held','reader',0,0,'read_only')")
        .bind(&card).bind(&track).bind(cwd.path().to_str().unwrap()).execute(repo.pool()).await.unwrap();
    assert!(
        ExecutionWriteGuard::acquire_native(
            repo.pool(),
            &card,
            "thread",
            "",
            NativeProvider::Codex
        )
        .await
        .is_err(),
        "a reader at actual execution cwd must block even when Track cwd differs"
    );
}

#[tokio::test]
async fn native_read_guards_share_scope_but_fence_native_writer() {
    use execution_guard::{ExecutionReadGuard, NativeTaskGuard};
    let cwd = tempfile::tempdir().unwrap();
    let (repo, track, card) = lease_fixture(cwd.path()).await;
    let other = crate::db::RepoSyncDomainRaw::card_create(
        &repo,
        crate::model::NewCard {
            track_id: track.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        },
    )
    .await
    .unwrap();
    execution_guard::bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        &card,
        "reader-one",
        cwd.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    execution_guard::bind_execution(
        repo.pool(),
        NativeProvider::Codex,
        other.id.as_str(),
        "reader-two",
        cwd.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    read_task_intent(&repo, &track, &card, cwd.path(), "held", "running").await;
    read_task_intent(
        &repo,
        &track,
        other.id.as_str(),
        cwd.path(),
        "held",
        "running",
    )
    .await;
    let first = NativeTaskGuard::Read(
        ExecutionReadGuard::acquire_native(repo.pool(), &card, "reader-one", NativeProvider::Codex)
            .await
            .unwrap(),
    );
    let second = NativeTaskGuard::Read(
        ExecutionReadGuard::acquire_native(
            repo.pool(),
            other.id.as_str(),
            "reader-two",
            NativeProvider::Codex,
        )
        .await
        .unwrap(),
    );
    assert!(
        ExecutionWriteGuard::acquire_native(
            repo.pool(),
            &card,
            "reader-one",
            "",
            NativeProvider::Codex
        )
        .await
        .is_err()
    );
    let modes: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT access_mode,write_root_id FROM workspace_leases \
        WHERE holder_kind='native' AND state='held'",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        modes,
        vec![("read_only".into(), None), ("read_only".into(), None)]
    );
    first.rejected().await.unwrap();
    second.rejected().await.unwrap();
}

async fn read_task_intent(
    repo: &crate::db::sqlite::SqlxRepo,
    track: &str,
    card: &str,
    cwd: &Path,
    state: &str,
    status: &str,
) {
    let task = format!("task-{card}");
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id, \
        declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,?1,'codex','read',?3,?4,?5,'user',0,0)")
        .bind(&task).bind(track).bind(serde_json::json!({"neige_workspace":{"access":"read_only"}}).to_string()).bind(status).bind(card).execute(repo.pool()).await.unwrap();
    use crate::operation::OperationRepo;
    let operations = crate::operation::SqlxOperationRepo::new(repo.pool().clone());
    let owner = operations
        .insert_operation(
            "codex-worker",
            crate::operation::OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(task.clone()),
                payload_hash: "read-claim".into(),
            },
            serde_json::json!({}),
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner, \
        created_at_ms,updated_at_ms,access_mode) VALUES(?1,?2,?3,?4,?5,?6,0,0,'read_only')",
    )
    .bind(format!("lease-{card}"))
    .bind(card)
    .bind(track)
    .bind(cwd.to_str().unwrap())
    .bind(state)
    .bind(owner)
    .execute(repo.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn native_read_issuance_refuses_closed_intent_mismatch_and_ended_task() {
    use execution_guard::{ExecutionReadGuard, NativeTaskGuard};
    for (state, status, mismatch) in [
        ("released", "running", false),
        ("held", "running", true),
        ("held", "done", false),
    ] {
        let cwd = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (repo, track, card) = lease_fixture(cwd.path()).await;
        read_task_intent(&repo, &track, &card, cwd.path(), state, status).await;
        execution_guard::bind_execution(
            repo.pool(),
            NativeProvider::Codex,
            &card,
            "read-thread",
            if mismatch { other.path() } else { cwd.path() }
                .to_str()
                .unwrap(),
        )
        .await
        .unwrap();
        let result = ExecutionReadGuard::acquire_native(
            repo.pool(),
            &card,
            "read-thread",
            NativeProvider::Codex,
        )
        .await;
        let allowed = result.is_ok();
        if let Ok(guard) = result {
            NativeTaskGuard::Read(guard).rejected().await.unwrap();
        }
        assert!(
            !allowed,
            "closed/mismatched/ended read intent must be rejected in native issuance transaction"
        );
    }
}

#[tokio::test]
async fn native_write_issuance_checks_task_directory_and_real_ended_task() {
    for ended in [false, true] {
        let cwd = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (repo, track, card) = lease_fixture(cwd.path()).await;
        use crate::operation::OperationRepo;
        let task = new_id();
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id, \
            declared_by,created_at_ms,updated_at_ms) VALUES(?1,?2,?1,'codex','write','{}',?3,?4,'user',0,0)")
            .bind(&task).bind(&track).bind(if ended {"done"} else {"running"}).bind(&card).execute(repo.pool()).await.unwrap();
        let operations = crate::operation::SqlxOperationRepo::new(repo.pool().clone());
        let owner = operations
            .insert_operation(
                "codex-worker",
                crate::operation::OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(task.clone()),
                    payload_hash: "write-pin".into(),
                },
                serde_json::json!({}),
            )
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        acquire_plain_workspace_lease_tx(&mut tx, &card, &track, &owner, cwd.path())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        execution_guard::bind_execution(
            repo.pool(),
            NativeProvider::Codex,
            &card,
            "writer",
            if ended { cwd.path() } else { other.path() }
                .to_str()
                .unwrap(),
        )
        .await
        .unwrap();
        let result = ExecutionWriteGuard::acquire_native(
            repo.pool(),
            &card,
            "writer",
            &task,
            NativeProvider::Codex,
        )
        .await;
        let allowed = result.is_ok();
        if let Ok(guard) = result {
            guard.rejected().await.unwrap();
        }
        assert!(
            !allowed,
            "Task writers require actual frozen cwd and an active claimed Task"
        );
    }
}
