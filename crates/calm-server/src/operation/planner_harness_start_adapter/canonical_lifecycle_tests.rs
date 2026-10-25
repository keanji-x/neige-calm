#![cfg(test)]
//! The actual original fence/start must never turn accepted canonical history
//! into another legacy carrier, even when its current pointer is unavailable.
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::harness::snapshot::CanonicalHarnessSnapshot;
use crate::model::{NewArea, NewTrack, RequestTheme};
use crate::operation::OperationKey;
use crate::operation::planner_start_fence::CardStartFence;
use crate::plugin_host::PluginRegistry;
use crate::state::{AppState, CodexClient, DaemonClient, RouteState};
use axum::extract::FromRef;
use calm_truth::db::RepoRead;
use calm_types::conversation::HumanPayload;
use calm_types::organization::LEGACY_OWNER_USER_ID;

struct Fixture {
    repo: Arc<SqlxRepo>,
    state: AppState,
    card: CardId,
    worker: String,
    payload: PlannerHarnessStartOperationPayload,
    _execution: Option<crate::test_support::execution::Fixture>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_native(false).await
    }

    async fn with_native(fake: bool) -> Self {
        Self::build(fake, AgentProvider::Codex, false).await
    }

    async fn with_provider(fake: bool, provider: AgentProvider) -> Self {
        Self::build(fake, provider, true).await
    }

    async fn build(fake: bool, provider: AgentProvider, persistent: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::init_fixture_git_repo(dir.path());
        let uri = if persistent {
            format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("canonical.sqlite").display()
            )
        } else {
            "sqlite::memory:".into()
        };
        let repo = Arc::new(SqlxRepo::open(&uri).await.unwrap());
        let area = repo
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "retained canonical lifecycle".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "same original carrier".into(),
                sort: None,
                cwd: dir.path().to_string_lossy().into_owned(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let card=card_create_with_id_tx(&mut tx,new_id(),NewCard {
            track_id:track.id.clone(),kind:"codex".into(),title:None,sort:None,
            payload:json!({"schemaVersion":1,"planner_harness":true,"planner_provider":provider,"permission_mode":"never"}),
        },CardRole::Planner,true,repo.card_role_cache()).await.unwrap().id;
        let worker = new_id();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::shared_planner(
                worker.clone(),
                card.to_string(),
                provider.clone(),
                WorkerSessionState::Idle,
                None,
                serde_json::to_value(CanonicalHarnessSnapshot::dormant(0)).unwrap(),
                0,
            ),
        )
        .await
        .unwrap();
        let mut carrier = crate::db::sqlite::conversation::current_input_carrier_tx(
            &mut tx,
            card.as_str(),
            &worker,
        )
        .await
        .unwrap();
        crate::db::sqlite::conversation::accept_pending_human_tx(
            &mut tx,
            &mut carrier,
            LEGACY_OWNER_USER_ID,
            HumanPayload {
                text: "permanent accepted canonical history".into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let events = crate::event::EventBus::new();
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            dir.path().join("plugins"),
            dir.path().join("plugin-data"),
            vec![],
            events.clone(),
            WriteContext::new(
                repo.card_role_cache().clone(),
                repo.track_area_cache().clone(),
            ),
        ));
        let shared = if fake {
            SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None)
        } else {
            SharedCodexAppServer::new_stub(repo.clone())
        };
        let mut state = AppState::from_parts(
            repo.clone(),
            events,
            Arc::new(DaemonClient::new_stub()),
            plugin,
            Arc::new(CodexClient::new_stub()),
            Some(repo.card_role_cache().clone()),
            Some(repo.track_area_cache().clone()),
        )
        .with_shared_codex_appserver(shared);
        let execution = if matches!(
            provider.protocol(),
            calm_types::runtime::AgentProtocol::Claude | calm_types::runtime::AgentProtocol::Acp
        ) {
            let execution = crate::test_support::execution::Fixture::new(repo.db().clone()).await;
            state.execution = Some(execution.engine.clone());
            state
                .operation_runtime
                .execution()
                .bind(execution.engine.clone());
            state.plugin.execution.bind(execution.engine.clone());
            Some(execution)
        } else {
            None
        };
        let payload = PlannerHarnessStartOperationPayload {
            actor: ActorId::Kernel,
            track_id: track.id.to_string(),
            planner_card_id: card.clone(),
            report_card_id: None,
            sort: None,
            cwd: dir.path().to_string_lossy().into_owned(),
            goal: None,
            reset_harness_items: true,
            force_new_thread: true,
            profile: HarnessProfile::Planner,
            create_card: None,
            opening_briefing: None,
            first_message: None,
            create_request_sha256: None,
        };
        Self {
            repo,
            state,
            card,
            worker,
            payload,
            _execution: execution,
            _dir: dir,
        }
    }

    async fn legacy_start(&self) {
        let route = RouteState::from_ref(&self.state);
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions")
            .fetch_one(self.repo.pool())
            .await
            .unwrap();
        let fence = CardStartFence::lock(
            &route.planner_recovery_locks,
            &route.repo,
            &route.operation_runtime,
            &self.card,
        )
        .await;
        let _result = fence
            .start(
                &self.payload,
                OperationKey {
                    operation_key: format!("legacy retry:{}", new_id()),
                    idempotency_key: Some(new_id()),
                    payload_hash: "legacy request".into(),
                },
            )
            .await;
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions")
            .fetch_one(self.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            after, before,
            "old internal start must not allocate a V1 carrier over accepted canonical history"
        );
        let legacy:i64=sqlx::query_scalar("SELECT count(*) FROM worker_sessions WHERE json_extract(handle_state_json,'$.schema_version')=1")
            .fetch_one(self.repo.pool()).await.unwrap();
        assert_eq!(
            legacy, 0,
            "canonical history cannot downgrade through failed/cleared pointer"
        );
        let messages: i64 =
            sqlx::query_scalar("SELECT count(*) FROM conversation_messages WHERE card_id=?1")
                .bind(self.card.as_str())
                .fetch_one(self.repo.pool())
                .await
                .unwrap();
        assert_eq!(messages, 1);
        assert!(
            self.repo
                .card_get(self.card.as_str())
                .await
                .unwrap()
                .is_some()
        );
    }
}

#[tokio::test]
async fn accepted_canonical_card_cannot_restart_as_legacy_after_failure() {
    let fx = Fixture::with_native(true).await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    session_fail_if_active_runtime_tx(&mut tx, &fx.worker)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    fx.legacy_start().await;
}

#[tokio::test]
async fn accepted_canonical_card_cannot_restart_as_legacy_after_pointer_clear() {
    let fx = Fixture::with_native(true).await;
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(fx.card.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    fx.legacy_start().await;
}

async fn source() -> (
    crate::auth::AuthState,
    String,
    crate::auth::HumanInputSource,
) {
    let auth = crate::auth::AuthState::new(crate::auth::AuthConfig {
        username: Some("Owner".into()),
        password: Some("fixture-password".into()),
        dev_autologin: false,
        display_name: "Owner".into(),
    });
    let token = auth.sessions.create(
        crate::auth::SessionIdentity::owner(&auth.config),
        crate::auth::SessionAuthority::PasswordLogin,
    );
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("{}={token}", crate::auth::SESSION_COOKIE)
            .parse()
            .unwrap(),
    );
    let (status, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    (auth, token, source.unwrap())
}

async fn wake(fx: &Fixture, source: crate::auth::HumanInputSource) -> Result<()> {
    let route = RouteState::from_ref(&fx.state);
    CardStartFence::lock(
        &route.planner_recovery_locks,
        &route.repo,
        &route.operation_runtime,
        &fx.card,
    )
    .await
    .wake_retained(
        route.planner_start_adapter.clone(),
        source,
        route.workspace_root.clone(),
        route.events.clone(),
    )
    .await
}

#[tokio::test]
async fn retained_down_preserves_saveable_original_carrier_and_has_no_native_marker() {
    let fx = Fixture::new().await;
    let (_auth, _token, source) = source().await;
    let principal = source.current_principal().unwrap();
    assert!(wake(&fx, source).await.is_err());
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, WorkerSessionState::Idle);
    assert!(row.spawn_op_id.is_none());
    assert!(row.thread_id.is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM operations")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let mut carrier = crate::db::sqlite::conversation::current_input_carrier_tx(
        &mut tx,
        fx.card.as_str(),
        &fx.worker,
    )
    .await
    .unwrap();
    crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &principal,
        &mut carrier,
        HumanPayload {
            text: "ordinary send after unavailable start".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(carrier.checkpoint().pending.len(), 2);
}

#[tokio::test]
async fn retained_revoked_source_cannot_enter_original_start_effects() {
    let fx = Fixture::with_native(true).await;
    let (auth, token, source) = source().await;
    auth.sessions.remove(&token).unwrap();
    assert!(matches!(
        wake(&fx, source).await,
        Err(CalmError::Unauthorized)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM operations")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        0
    );
    assert!(
        fx.repo
            .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
            .await
            .unwrap()
            .unwrap()
            .thread_id
            .is_none()
    );
}

#[tokio::test]
async fn retained_live_retry_uses_original_carrier_and_actual_native_start_once() {
    use crate::shared_codex_appserver::native_owner_tests::{
        connect, notify_thread, request, send,
    };
    let fx = Fixture::new().await;
    let (_auth, _token, source) = source().await;
    assert!(wake(&fx, source.clone()).await.is_err());
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let mut carrier = crate::db::sqlite::conversation::current_input_content_carrier_tx(
        &mut tx,
        fx.card.as_str(),
    )
    .await
    .unwrap();
    crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "second accepted before native ready".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let daemon = fx.state.shared_codex_appserver.clone();
    let (_client, mut peer) = connect(&daemon).await;
    let unrelated_repo = crate::operation::SqlxOperationRepo::new(fx.repo.db().clone());
    use crate::operation::OperationRepo;
    let unrelated = unrelated_repo
        .insert_operation(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "unrelated pending legacy operation".into(),
            },
            serde_json::to_value(&fx.payload).unwrap(),
        )
        .await
        .unwrap();
    let request_source = source.clone();
    let owning = async { wake(&fx, request_source).await };
    let native = async {
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "thread/start");
        assert!(
            !call
                .to_string()
                .contains("permanent accepted canonical history")
        );
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"thread":{"id":"retained-native-thread"}}}),
        )
        .await;
        for (turn, text) in [
            ("retained-turn-1", "permanent accepted canonical history"),
            ("retained-turn-2", "second accepted before native ready"),
        ] {
            let call = loop {
                let call = request(&mut peer).await;
                if call["method"] == "turn/start" {
                    break call;
                }
                assert_eq!(call["method"], "config/read");
                send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
            };
            assert_eq!(call["params"]["threadId"], "retained-native-thread");
            assert!(call["params"]["input"].to_string().contains(text));
            notify_thread(
                &daemon,
                &mut peer,
                "retained-native-thread",
                "turn/started",
                json!({"id":turn,"status":"inProgress"}),
            )
            .await;
            send(
                &mut peer,
                json!({"id":call["id"],"result":{"turn":{"id":turn}}}),
            )
            .await;
            notify_thread(
                &daemon,
                &mut peer,
                "retained-native-thread",
                "turn/completed",
                json!({"id":turn,"status":"completed"}),
            )
            .await;
        }
    };
    let (result, ()) = tokio::join!(owning, native);
    result.unwrap();
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.thread_id.as_deref(), Some("retained-native-thread"));
    assert!(row.spawn_op_id.is_some());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM worker_sessions")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_messages")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        2
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let row = fx
                .repo
                .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
                .await
                .unwrap()
                .unwrap();
            let frame: CanonicalHarnessSnapshot =
                serde_json::from_value(row.handle_state_json.unwrap()).unwrap();
            if frame.header.phase == HarnessPhaseTag::TurnCompleted
                && frame.header.last_turn_id.as_deref() == Some("retained-turn-2")
            {
                assert!(frame.canonical_inputs.active.is_none());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        unrelated_repo
            .get_operation(&unrelated)
            .await
            .unwrap()
            .unwrap()
            .phase
            .tag(),
        PhaseTag::Pending,
        "the source-bearing exact invocation must not claim another pending operation"
    );
    let token_before = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap()
        .mcp_token_hash;
    assert!(
        daemon
            .resume_candidates_for_test()
            .iter()
            .any(|(thread, card)| thread == "retained-native-thread" && card == fx.card.as_str())
    );
    let automatic =
        crate::shared_codex_appserver::native_owner_tests::resume_cached_without_source(&daemon);
    use futures_util::StreamExt;
    tokio::select! {
        ()=automatic=>{},
        _request=peer.next()=>panic!("automatic recovery crossed canonical source boundary"),
    }
    assert!(
        token_before
            == fx
                .repo
                .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
                .await
                .unwrap()
                .unwrap()
                .mcp_token_hash
    );
    let handle = fx.state.harness.get(&fx.worker).unwrap();
    let mut stopping = tokio::spawn(async move { handle.shutdown().await });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        tokio::select! {
            stopped=&mut stopping=>stopped.unwrap().unwrap(),
            call=request(&mut peer)=> { assert_eq!(call["method"],"turn/interrupt");send(&mut peer,json!({"id":call["id"],"result":{}})).await;stopping.await.unwrap().unwrap(); }
        }
    }).await.unwrap();
}

#[tokio::test]
async fn failed_canonical_current_carrier_can_accept_content_but_cannot_claim() {
    use crate::db::sqlite::conversation as truth;
    let fx = Fixture::new().await;
    let (_auth, _token, source) = source().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    session_fail_if_active_runtime_tx(&mut tx, &fx.worker)
        .await
        .unwrap();
    assert!(
        truth::current_input_carrier_tx(&mut tx, fx.card.as_str(), &fx.worker)
            .await
            .is_err()
    );
    let mut carrier = truth::current_input_content_carrier_tx(&mut tx, fx.card.as_str())
        .await
        .unwrap();
    crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "accepted while execution stays held".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    let plan = truth::human_plan_for_pending_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(2).unwrap(),
        vec![],
    )
    .await
    .unwrap();
    let provenance =
        calm_types::conversation::ConversationExecutionProvenance::LegacySingleUserDeployment {
            deployment_owner_user_id: LEGACY_OWNER_USER_ID.into(),
            provider: AgentProvider::Codex,
            profile: HarnessProfile::Planner,
        };
    assert!(
        truth::claim_initial_submission_tx(&mut tx, &mut carrier, plan, &provenance)
            .await
            .unwrap_err()
            .to_string()
            .contains("content only")
    );
    tx.commit().await.unwrap();
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, WorkerSessionState::Failed);
    let frame: CanonicalHarnessSnapshot =
        serde_json::from_value(row.handle_state_json.unwrap()).unwrap();
    assert_eq!(frame.canonical_inputs.pending.len(), 2);
    assert!(frame.canonical_inputs.active.is_none());
    assert!(wake(&fx, source).await.is_err());
}

async fn retained_revocation_case(point: Option<&str>) {
    use crate::shared_codex_appserver::native_owner_tests::{
        assert_no_request, connect, request, send,
    };
    let fx = Fixture::new().await;
    let (auth, token, live_source) = source().await;
    let daemon = fx.state.shared_codex_appserver.clone();
    let (client, mut peer) = connect(&daemon).await;
    let sink = if point.is_none() {
        Some(client.lock_outbound_sink_for_test().await)
    } else {
        None
    };
    let pause = crate::test_seams::PausePoint::default();
    if let Some(point) = point {
        crate::test_seams::install_pause_for_test(point, fx.card.as_str(), pause.clone());
    }
    let route = RouteState::from_ref(&fx.state);
    let card = fx.card.clone();
    let mut activation = tokio::spawn(async move {
        CardStartFence::lock(
            &route.planner_recovery_locks,
            &route.repo,
            &route.operation_runtime,
            &card,
        )
        .await
        .wake_retained(
            route.planner_start_adapter.clone(),
            live_source,
            route.workspace_root.clone(),
            route.events.clone(),
        )
        .await
    });
    if point.is_some() {
        tokio::time::timeout(std::time::Duration::from_secs(5), pause.entered.notified())
            .await
            .unwrap();
    } else {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while client.pending_requests_for_test() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    auth.sessions.remove(&token).unwrap();
    drop(sink);
    pause.release.notify_one();
    let unexpected = tokio::select! {
        result = &mut activation => { assert!(result.unwrap().is_err()); false }
        call = request(&mut peer) => {
            send(&mut peer, json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected revoked send"}})).await;
            assert!(activation.await.unwrap().is_err());
            true
        }
    };
    assert!(
        !unexpected,
        "revocation before admission returns must prevent RPC"
    );
    assert_no_request(&mut peer).await;
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.spawn_op_id.is_none(),
        "BeforeSend releases the unstarted claim"
    );
    assert!(row.thread_id.is_none());
    assert_eq!(row.state, WorkerSessionState::Idle);
    let frame: CanonicalHarnessSnapshot =
        serde_json::from_value(row.handle_state_json.unwrap()).unwrap();
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.active.is_none());
    assert!(frame.canonical_inputs.attempts.is_empty());

    // This is new authority, not a retry by the revoked invocation. Answer its
    // legitimate RPC instead of waiting 30 seconds and mislabeling its frame.
    let (_new_auth, _new_token, new_source) = source().await;
    let retry = wake(&fx, new_source.clone());
    let native = async {
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "thread/start");
        send(&mut peer, json!({"id":call["id"],"error":{"code":-32000,"message":"fixture leaves native outcome unknown"}})).await;
    };
    let (result, ()) = tokio::join!(retry, native);
    assert!(result.is_err());
    // A sent request has Unknown delivery, so even this fresh source may not
    // mint again. Completion plus the reply barrier checks queued frames too.
    assert!(wake(&fx, new_source).await.is_err());
    assert_no_request(&mut peer).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM operations")
            .fetch_one(fx.repo.pool())
            .await
            .unwrap(),
        2
    );
    let row = fx
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
        .await
        .unwrap()
        .unwrap();
    assert!(row.spawn_op_id.is_some());
    assert!(row.thread_id.is_none());
    assert_eq!(row.state, WorkerSessionState::Idle);
}

#[tokio::test]
async fn retained_source_revoked_at_native_sink_proves_non_delivery_before_fresh_retry() {
    retained_revocation_case(None).await;
}

#[tokio::test]
async fn retained_revoked_after_claim_commit_releases_claim_for_fresh_retry() {
    retained_revocation_case(Some("retained-claim-committed")).await;
}

#[tokio::test]
async fn retained_revoked_after_admission_commit_prevents_rpc_and_keeps_input_pending() {
    retained_revocation_case(Some("retained-admission-committed")).await;
}

#[tokio::test]
async fn retained_native_refusal_preserves_original_unknown_without_private_error_archive() {
    use crate::shared_codex_appserver::native_owner_tests::{connect, request, send};
    for reply in [
        json!({"error":{"code":-32000,"message":"permanent accepted canonical history","data":{"secret":"private native diagnostic"}}}),
        json!({"result":{"thread":{"id":""}}}),
        json!({"result":{"thread":{}}}),
    ] {
        let fx = Fixture::new().await;
        let (_auth, _token, source) = source().await;
        let daemon = fx.state.shared_codex_appserver.clone();
        let (_client, mut peer) = connect(&daemon).await;
        let owning = wake(&fx, source.clone());
        let native = async {
            let call = request(&mut peer).await;
            assert_eq!(call["method"], "thread/start");
            let mut reply = reply;
            reply["id"] = call["id"].clone();
            send(&mut peer, reply).await;
        };
        let (result, ()) = tokio::join!(owning, native);
        assert!(result.is_err());
        assert!(wake(&fx, source).await.is_err());
        crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
        let journal: Vec<(Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT tx_output_json,last_error,compensation_state FROM operations")
                .fetch_all(fx.repo.pool())
                .await
                .unwrap();
        assert_eq!(journal.len(), 1);
        let journal = format!("{journal:?}");
        assert!(!journal.contains("permanent accepted canonical history"));
        assert!(!journal.contains("private native diagnostic"));
        let row = fx
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(fx.worker.clone()))
            .await
            .unwrap()
            .unwrap();
        assert!(row.spawn_op_id.is_some());
        assert!(row.thread_id.is_none());
        assert_eq!(row.state, WorkerSessionState::Idle);
        let frame: CanonicalHarnessSnapshot =
            serde_json::from_value(row.handle_state_json.unwrap()).unwrap();
        assert_eq!(frame.canonical_inputs.pending.len(), 1);
        assert!(frame.canonical_inputs.active.is_none());
        assert!(frame.canonical_inputs.attempts.is_empty());
    }
}

#[cfg(feature = "fixtures")]
mod claude_recovery;

mod acp_recovery;
