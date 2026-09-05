use super::*;

pub(super) struct ClaudeWorkerHarness {
    pub(super) repo: Arc<crate::db::sqlite::SqlxRepo>,
    pub(super) adapter: ClaudeWorkerAdapter,
    pub(super) track_id: String,
    pub(super) events: EventBus,
    pub(super) workspace: tempfile::TempDir,
}

pub(super) async fn claude_worker_harness() -> ClaudeWorkerHarness {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let area = crate::db::RepoSyncDomainRaw::area_create(
        repo.as_ref(),
        crate::model::NewArea {
            name: "claude workspace leases".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(workspace.path().join("worker-source"), "tracked source").unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["add", "worker-source"])
            .current_dir(workspace.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "source"
            ])
            .current_dir(workspace.path())
            .status()
            .unwrap()
            .success()
    );
    let track = crate::db::RepoSyncDomainRaw::track_create(
        repo.as_ref(),
        crate::model::NewTrack {
            template_input: None,
            area_id: area.id,
            title: "claude workspace leases".into(),
            sort: None,
            cwd: workspace.path().to_string_lossy().into_owned(),
            template_id: None,
            plugin_scope: None,
            attach_folder: true,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    ClaudeWorkerHarness {
        adapter: ClaudeWorkerAdapter::new(
            route_repo,
            Arc::new(CodexClient::new_stub()),
            None,
            CardRoleCache::new(),
            TrackAreaCache::new(),
            workspace.path().to_path_buf(),
        ),
        repo,
        track_id: track.id.to_string(),
        events: EventBus::new(),
        workspace,
    }
}

pub(super) fn claude_worker_payload(track_id: &str, key: &str) -> Value {
    serde_json::to_value(ClaudeWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: track_id.to_string(),
        idempotency_key: format!("{track_id}:{key}"),
        goal: format!("do {key}"),
        cwd: None,
        context: Value::Null,
        acceptance_criteria: None,
    })
    .unwrap()
}

pub(super) fn claude_worker_op(id: &str, payload: Value) -> Operation {
    Operation {
        id: id.to_string(),
        operation_key: format!("op-key-{id}"),
        kind: "claude-worker".into(),
        idempotency_key: Some(id.to_string()),
        payload_hash: "hash".into(),
        target_type: "unknown".into(),
        target_id: None,
        target: json!({ "type": "unknown", "id": null }),
        payload,
        tx_output: None,
        phase: crate::operation::Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    }
}

pub(super) async fn prepare_claude_worker(
    harness: &ClaudeWorkerHarness,
    key: &str,
) -> (TxOutput, Vec<BroadcastEnvelope>, String) {
    let payload = claude_worker_payload(&harness.track_id, key);
    let task_id = format!("{}:{key}", harness.track_id);
    sqlx::query(
        "INSERT OR IGNORE INTO tasks \
         (id, track_id, key, kind, goal, context_json, depends_on_json, status, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, 'claude', 'test', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(&task_id)
    .bind(&harness.track_id)
    .bind(key)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "claude-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("op-{key}")),
                payload_hash: format!("hash-{key}"),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == op_id)
        .unwrap();
    let claimed_op_id = op.id.clone();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let output = harness
        .adapter
        .prepare_tx(&mut tx, &payload, &op)
        .await
        .unwrap();
    let events = output.post_commit_events.clone();
    tx.commit().await.unwrap();
    (output, events, claimed_op_id)
}
