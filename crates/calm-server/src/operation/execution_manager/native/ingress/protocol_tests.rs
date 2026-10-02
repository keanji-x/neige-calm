use super::*;
use crate::operation::execution_manager::native::supervisor::execution_backend::CodexBackend;

async fn launched(f: &Fixture) -> (WebSocketStream<UnixStream>, CodexBackend, Record) {
    let mut client = client(f).await;
    initialize(&mut client).await;
    let reply = rpc(
        &mut client,
        json!("start"),
        "turn/start",
        json!({"threadId":"ingress-thread","input":[]}),
    )
    .await;
    assert!(reply.get("result").is_some(), "{reply}");
    let execution: String = sqlx::query_scalar(
        "SELECT lease_id FROM workspace_leases WHERE holder_kind='native' AND state='held'",
    )
    .fetch_one(f.repo.pool())
    .await
    .unwrap();
    let record = super::super::super::super::storage::load(f.repo.pool(), &execution)
        .await
        .unwrap()
        .unwrap();
    let (wire, _notifications) =
        super::super::super::wire::CodexAppServer::connect(&f.scope.provider)
            .await
            .unwrap();
    wire.initialize(super::super::super::wire::ClientInfo {
        name: "fake-stop".into(),
        version: "test".into(),
    })
    .await
    .unwrap();
    (client, CodexBackend::for_ingress(Arc::new(wire)), record)
}

#[tokio::test]
async fn native_ingress_descendant_active_after_parent_idle_retains_the_parent() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let (_client, backend, record) = launched(&f).await;
    provider.stopped.store(true, Ordering::SeqCst);
    provider.stop_allowed.store(true, Ordering::SeqCst);
    provider.children.lock().unwrap().insert(
        "child".into(),
        child_facts("child", "ingress-thread", &f.scope.cwd, "inProgress"),
    );
    let manager = ExecutionManager::new(f.repo.pool().clone());
    assert!(manager.cancel(&backend, &record.id).await.is_err());
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
        .bind(&record.id)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "held");
    provider.child_stop_allowed.store(true, Ordering::SeqCst);
    assert!(manager.cancel(&backend, &record.id).await.unwrap());
    quiesce(&f.scope.socket).await.unwrap();
}

#[tokio::test]
async fn native_ingress_paged_roster_and_new_grandchild_require_fresh_complete_stop() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let (_client, backend, record) = launched(&f).await;
    provider.stopped.store(true, Ordering::SeqCst);
    provider.stop_allowed.store(true, Ordering::SeqCst);
    provider.child_stop_allowed.store(true, Ordering::SeqCst);
    provider.paged.store(true, Ordering::SeqCst);
    provider.spawn_grandchild.store(true, Ordering::SeqCst);
    provider.children.lock().unwrap().insert(
        "child".into(),
        child_facts("child", "ingress-thread", &f.scope.cwd, "inProgress"),
    );
    let manager = ExecutionManager::new(f.repo.pool().clone());
    assert!(
        !manager.cancel(&backend, &record.id).await.unwrap(),
        "a new grandchild invalidates the stop observation"
    );
    assert!(manager.cancel(&backend, &record.id).await.unwrap());
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|frame| frame["method"] == "thread/list"
                && frame["params"]["cursor"] == "next-page")
    );
    quiesce(&f.scope.socket).await.unwrap();
}

#[tokio::test]
async fn native_ingress_wrong_ancestry_foreign_scope_and_unknown_child_facts_never_release() {
    for bad in ["ancestry", "scope", "status"] {
        let f = fixture().await;
        let provider = provider(&f).await;
        let foreign = tempfile::tempdir().unwrap();
        let (_client, backend, record) = launched(&f).await;
        provider.stopped.store(true, Ordering::SeqCst);
        provider.stop_allowed.store(true, Ordering::SeqCst);
        provider.child_stop_allowed.store(true, Ordering::SeqCst);
        let child = child_facts(
            "child",
            if bad == "ancestry" {
                "another-root"
            } else {
                "ingress-thread"
            },
            if bad == "scope" {
                foreign.path().to_str().unwrap()
            } else {
                &f.scope.cwd
            },
            if bad == "status" {
                "queued"
            } else {
                "inProgress"
            },
        );
        provider
            .children
            .lock()
            .unwrap()
            .insert("child".into(), child);
        let manager = ExecutionManager::new(f.repo.pool().clone());
        let outcome = manager.cancel(&backend, &record.id).await;
        assert!(
            !matches!(outcome, Ok(true)),
            "{bad} is not positive stop evidence"
        );
        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
                .bind(&record.id)
                .fetch_one(f.repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "held");
        quiesce(&f.scope.socket).await.unwrap();
    }
}

#[tokio::test]
async fn native_ingress_01592_default_nullable_frames_keep_frozen_scope_and_string_loaded_ids() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let mut client = client(&f).await;
    initialize(&mut client).await;
    let resume = json!({"threadId":"ingress-thread","approvalPolicy":null,"approvalsReviewer":null,
        "baseInstructions":null,"config":{},"cwd":null,"developerInstructions":null,"excludeTurns":false,
        "history":null,"initialTurnsPage":null,"model":null,"modelProvider":null,"path":null,
        "permissions":null,"personality":null,"runtimeWorkspaceRoots":null,"sandbox":null,"serviceTier":null});
    let reply = rpc(&mut client, json!(1), "thread/resume", resume).await;
    assert!(reply.get("result").is_some(), "{reply}");
    let loaded = rpc(
        &mut client,
        json!(2),
        "thread/loaded/list",
        json!({"cursor":null,"limit":100}),
    )
    .await;
    assert_eq!(loaded["result"]["data"], json!(["ingress-thread"]));
    let start = json!({"threadId":"ingress-thread","input":[{"type":"text","text":"hello"}],
        "additionalContext":null,"approvalPolicy":null,"approvalsReviewer":null,"clientUserMessageId":null,
        "collaborationMode":null,"cwd":null,"cyberAccessProgram":null,"disabledPluginIds":null,
        "effort":null,"environments":null,"model":null,"multiAgentMode":null,"outputSchema":null,
        "permissions":null,"personality":null,"responsesapiClientMetadata":null,"runtimeWorkspaceRoots":null,
        "sandboxPolicy":null,"serviceTier":null,"serviceTierForTurn":null,"summary":null,"toolOutput":null,"turnTrigger":null});
    let reply = rpc(&mut client, json!(3), "turn/start", start).await;
    assert!(reply.get("result").is_some(), "{reply}");
    let requests = provider.requests.lock().unwrap();
    let resumed = requests
        .iter()
        .find(|frame| frame["method"] == "thread/resume")
        .unwrap();
    assert_eq!(
        resumed["params"]["runtimeWorkspaceRoots"],
        json!([f.scope.cwd])
    );
    assert_eq!(resumed["params"]["sandbox"], "workspace-write");
    assert_eq!(resumed["params"]["approvalPolicy"], "never");
    drop(requests);
    quiesce(&f.scope.socket).await.unwrap();
}

#[tokio::test]
async fn native_ingress_non_null_scope_capabilities_and_unknown_fields_fail_closed() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let mut client = client(&f).await;
    initialize(&mut client).await;
    for (key, value) in [
        ("path", json!("/another/rollout")),
        ("history", json!([])),
        ("runtimeWorkspaceRoots", json!(["/another/worktree"])),
        ("approvalsReviewer", json!("user")),
        ("unknownFutureCapability", Value::Null),
    ] {
        let mut params = json!({"threadId":"ingress-thread"});
        params[key] = value;
        assert!(
            rpc(&mut client, json!(key), "thread/resume", params)
                .await
                .get("error")
                .is_some()
        );
    }
    for (key, value) in [
        ("environments", json!([])),
        ("multiAgentMode", json!("enabled")),
        ("cyberAccessProgram", json!({})),
        (
            "runtimeWorkspaceRoots",
            json!([f.scope.cwd, "/another/worktree"]),
        ),
        ("toolOutput", json!({})),
        ("unknownFutureCapability", Value::Null),
    ] {
        let mut params = json!({"threadId":"ingress-thread","input":[]});
        params[key] = value;
        assert!(
            rpc(&mut client, json!(key), "turn/start", params)
                .await
                .get("error")
                .is_some()
        );
    }
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "only initialize can reach provider"
    );
    quiesce(&f.scope.socket).await.unwrap();
}

struct RejectingClient;
#[async_trait::async_trait]
impl super::super::super::super::backend::Backend for RejectingClient {
    type Request = ();
    type Output = ();
    fn kind(&self) -> super::super::super::super::BackendKind {
        super::super::super::super::BackendKind::NativeSession
    }
    async fn launch(
        &self,
        _: super::super::super::super::LaunchPermit,
        _: (),
    ) -> super::super::super::super::backend::LaunchOutcome<()> {
        super::super::super::super::backend::LaunchOutcome::NotIssued(CalmError::Conflict(
            "fake client not issued".into(),
        ))
    }
    async fn recover(
        &self,
        _: &Record,
    ) -> Result<super::super::super::super::backend::Observation> {
        unreachable!()
    }
    async fn stop(&self, _: &Record) -> Result<super::super::super::super::backend::Observation> {
        unreachable!()
    }
}

#[tokio::test]
async fn native_ingress_implicit_producer_and_rejected_client_keep_one_durable_group() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let (wire, _notifications) =
        super::super::super::wire::CodexAppServer::connect(&f.scope.provider)
            .await
            .unwrap();
    wire.initialize(super::super::super::wire::ClientInfo {
        name: "fake-kernel".into(),
        version: "test".into(),
    })
    .await
    .unwrap();
    let backend = CodexBackend::for_ingress(Arc::new(wire));
    let manager = ExecutionManager::new(f.repo.pool().clone());
    let owner = super::super::super::super::Owner {
        card: f.scope.card.clone(),
        holder: "ingress-thread".into(),
    };
    let producer = manager
        .submit(
            &backend,
            &owner,
            super::super::super::super::native::supervisor::execution_backend::TurnRequest {
                thread: "ingress-thread".into(),
                params: json!({"threadId":"ingress-thread","input":[]}),
            },
            None,
        )
        .await
        .unwrap();
    let group: String = sqlx::query_scalar(
        "SELECT session_execution_id FROM native_session_executions WHERE execution_id=?1",
    )
    .bind(&producer.execution_id)
    .fetch_one(f.repo.pool())
    .await
    .unwrap();
    assert_eq!(group, f.scope.execution);
    assert_eq!(
        super::super::super::release_unissued_native_session(f.repo.as_ref(), &f.scope.terminal)
            .await
            .unwrap(),
        Some(false)
    );
    assert!(
        manager
            .launch_reserved(&RejectingClient, &f.scope.execution, ())
            .await
            .is_err()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
        .bind(&f.scope.execution)
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        state, "held",
        "no client issued is not remote producer stopped"
    );
    // Preflight and the actual submit must agree about the same closing group.
    provider.stop_allowed.store(true, Ordering::SeqCst);
    assert!(
        manager
            .cancel(&backend, &producer.execution_id)
            .await
            .unwrap()
    );
    assert!(
        !manager
            .native_writer_available(&backend, &owner)
            .await
            .unwrap()
    );
    let before = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|frame| frame["method"] == "turn/start")
        .count();
    assert!(
        manager
            .submit(
                &backend,
                &owner,
                super::super::super::super::native::supervisor::execution_backend::TurnRequest {
                    thread: "ingress-thread".into(),
                    params: json!({"threadId":"ingress-thread","input":[]}),
                },
                None
            )
            .await
            .is_err()
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame["method"] == "turn/start")
            .count(),
        before
    );
    assert_eq!(
        super::super::super::release_unissued_native_session(f.repo.as_ref(), &f.scope.terminal)
            .await
            .unwrap(),
        Some(true)
    );
    quiesce(&f.scope.socket).await.unwrap();
}
