use std::sync::{Arc, Mutex};

use crate::{
    db::sqlite::{CheckoutOccupancy, SqlxRepo, TrackWorkspacePlan, checkout_occupancy},
    event::EventBus,
    ids::ActorId,
    model::new_id,
    operation::{
        OperationCompletionBus, OperationKey, OperationRepo, OperationRuntime, ProviderAdapter,
        SpawnCtx, SpawnHandle, SqlxOperationRepo,
        terminal_adapter::{TerminalWorkerAdapter, TerminalWorkerOperationPayload},
    },
    state::DaemonClient,
    terminal_renderer::TerminalRendererRegistry,
};

use super::tests::{init, observed_root, task, track};

pub(super) fn context(repo: Arc<SqlxRepo>, operations: Arc<SqlxOperationRepo>) -> SpawnCtx {
    SpawnCtx::new(
        repo.clone(),
        operations,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(repo),
        EventBus::new(),
        OperationCompletionBus::new(),
    )
}

#[tokio::test]
async fn workspace_identity_managed_preflight_does_not_create_or_repair() {
    for unborn in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let current = track(
            &repo,
            tmp.path(),
            TrackWorkspacePlan::ManagedUnder(tmp.path().into()),
        )
        .await;
        let directory = std::path::Path::new(&current.workspace.path);
        if unborn {
            crate::workspace_service::workspace_materialize::materialize_workspace(
                &current.workspace,
                tmp.path(),
                current.id.as_str(),
            )
            .unwrap();
            let deleted = std::process::Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(["update-ref", "-d", "HEAD"])
                .output()
                .unwrap();
            assert!(deleted.status.success());
        }
        let exists_before = directory.exists();
        let ctx = context(
            repo.clone(),
            Arc::new(SqlxOperationRepo::new(repo.pool().clone())),
        );
        super::prepare_task(&ctx, current.id.as_str())
            .await
            .unwrap();
        assert_eq!(
            directory.exists(),
            exists_before,
            "registration must not create a managed directory before its original prepare phase"
        );
        if unborn {
            let head = std::process::Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(["rev-parse", "--verify", "HEAD"])
                .output()
                .unwrap();
            assert!(
                !head.status.success(),
                "registration must not repair an unborn managed repository"
            );
        }
        assert!(observed_root(&repo, &current).await.is_none());
        let mut tx = repo.pool().begin().await.unwrap();
        let plan = crate::operation::workspace_lease::prepare_worker_lease_as_tx(
            &mut tx,
            current.id.as_str(),
            crate::model::TaskAccess::ReadWrite,
            tmp.path(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(plan.path, directory);
        assert!(
            directory.is_dir(),
            "the original production prepare still materializes its directory"
        );
    }
}

#[tokio::test]
async fn workspace_identity_legacy_non_git_terminal_owner_prepares_without_git_adoption() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let plain = track(&repo, tmp.path(), TrackWorkspacePlan::AttachedFromCwd).await;
    task(&repo, &plain).await;
    let operations = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let ctx = context(repo.clone(), operations);
    let adapter = TerminalWorkerAdapter::new(
        repo.clone(),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
    );
    let payload = serde_json::to_value(TerminalWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: plain.id.to_string(),
        idempotency_key: plain.id.to_string(),
        cmd: "printf legacy".into(),
        cwd: None,
    })
    .unwrap();
    adapter.prepare_external(&payload, &ctx).await.unwrap();
    assert!(
        observed_root(&repo, &plain).await.is_none(),
        "a legacy directory execution supplies no Git workspace identity"
    );
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            plain.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Busy
    );
}

#[tokio::test]
async fn workspace_identity_legacy_per_card_submit_replay_keeps_original_source_and_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let old_cwd = tmp.path().join("legacy-per-card");
    init(&source);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["worktree", "add", "-b", "legacy-per-card"])
        .arg(&old_cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let current = track(
        &repo,
        &source,
        TrackWorkspacePlan::AttachedWithTrackWorktree(source.clone()),
    )
    .await;
    crate::workspace_service::track_worktree::ensure_track_worktree(&current)
        .await
        .unwrap();
    task(&repo, &current).await;
    let base = workspace::base::resolve_head_base(&old_cwd).unwrap();
    let common = workspace::base::lease_git_common_dir(&old_cwd).unwrap();
    sqlx::query(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,created_at_ms,updated_at_ms, \
         base_sha,base_source,canonical_path,git_common_dir,delivery_policy,access_mode) \
         VALUES ('historical','card',?1,?2,'held','historical-owner',1,1,?3,'commit',?2,?4,'kernel','read_write')",
    ).bind(current.id.as_str()).bind(old_cwd.to_str().unwrap()).bind(&base)
        .bind(common.to_str().unwrap()).execute(repo.pool()).await.unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let capture = observed.clone();
    let adapter = Arc::new(TerminalWorkerAdapter::new_with_spawn_hook(
        repo.clone(),
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
        Arc::new(move |_, _, cwd, _| {
            capture.lock().unwrap().push(cwd);
            Box::pin(async { Ok(SpawnHandle::NoOp) })
        }),
    ));
    let operations = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
    let ctx = context(repo.clone(), operations.clone());
    let payload = serde_json::to_value(TerminalWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: current.id.to_string(),
        idempotency_key: current.id.to_string(),
        cmd: "printf original".into(),
        cwd: Some(old_cwd.to_str().unwrap().into()),
    })
    .unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(current.id.to_string()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let id = operations
        .insert_operation("terminal-worker", key.clone(), payload.clone())
        .await
        .unwrap();
    let claimed = operations
        .claim_drive_batch(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    operations
        .prepare_tx_and_advance(&claimed, adapter.as_ref())
        .await
        .unwrap()
        .unwrap();
    let before: (String, String, String) = sqlx::query_as(
        "SELECT payload_json,payload_hash,tx_output_json FROM operations WHERE id=?1",
    )
    .bind(id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    // This is the stored v1 phase produced by the production repository before upgrade.
    let runtime = OperationRuntime::new(
        operations,
        vec![adapter.clone()],
        EventBus::new(),
        OperationCompletionBus::new(),
        ctx.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        runtime
            .submit("terminal-worker", key.clone(), payload.clone())
            .await
            .unwrap(),
        id
    );
    assert_eq!(
        runtime
            .submit("terminal-worker", key.clone(), payload.clone())
            .await
            .unwrap(),
        id
    );
    for (field, changed) in [
        (
            "cwd",
            serde_json::json!(tmp.path().join("another").to_str().unwrap()),
        ),
        ("cmd", serde_json::json!("printf changed")),
    ] {
        let mut request = payload.clone();
        request[field] = changed;
        let mut changed_key = key.clone();
        changed_key.payload_hash =
            crate::routes::idempotency_key::stable_payload_hash(&request).unwrap();
        assert!(
            runtime
                .submit("terminal-worker", changed_key, request)
                .await
                .is_err(),
            "changed {field} must conflict"
        );
    }
    let after: (String, String, String) = sqlx::query_as(
        "SELECT payload_json,payload_hash,tx_output_json FROM operations WHERE id=?1",
    )
    .bind(id.as_str())
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(after, before);
    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
        .bind(id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        phase, "succeeded",
        "the old execution must actually complete its production drive"
    );
    assert_eq!(
        &*observed.lock().unwrap(),
        &[old_cwd.to_str().unwrap().to_owned()]
    );
    assert!(
        observed_root(&repo, &current).await.is_none(),
        "replaying an old external source cannot acquire the current track's identity"
    );
    adapter.prepare_external(&payload, &ctx).await.unwrap();
    assert!(observed_root(&repo, &current).await.is_none());
    let proof: (String, String, String) = sqlx::query_as(
        "SELECT path,base_sha,git_common_dir FROM workspace_leases WHERE lease_id='historical'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        proof,
        (
            old_cwd.to_str().unwrap().into(),
            base,
            common.to_str().unwrap().into()
        )
    );
    assert_eq!(
        checkout_occupancy(
            &mut repo.pool().acquire().await.unwrap(),
            current.id.as_str(),
            ""
        )
        .await
        .unwrap(),
        CheckoutOccupancy::Busy
    );
}
