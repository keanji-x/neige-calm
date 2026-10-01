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
            track_id: track.into(),
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
