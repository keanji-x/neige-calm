//! Revoke the real request issuer between actual provider metadata requests.
use super::*;
use futures_util::StreamExt;

#[tokio::test]
async fn canonical_revoked_source_cannot_continue_from_config_to_catalog() {
    let fx = Fixture::new().await;
    let card_id = fx.harness.inner.card_id.as_str();
    configure_default_effort(&fx.original.repo, card_id).await;
    let before = fx.frame().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let config = request(&mut peer).await;
    assert_eq!(config["method"], "config/read");
    eprintln!("stage: actual config/read received; no response sent yet");
    assert_eq!(fx.frame().await, before);
    fx.auth.sessions.remove(&fx.session).unwrap();
    assert!(fx.source.current_principal().is_err());
    eprintln!("stage: actual SessionStore credential revoked");
    send(
        &mut peer,
        json!({"id":config["id"],"result":{"config":{
            "model":"fixture-private-model","model_reasoning_effort":null
        }}}),
    )
    .await;

    // This existing production command can be answered only when the running
    // loop has left selection. Its revoked source is rejected, not inserted.
    // It gives the no-next-RPC branch a real completion barrier, not a sleep.
    let settled = fx
        .harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone());
    tokio::pin!(settled);
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut settled => {
                assert!(matches!(result, Err(CalmError::Unauthorized)));
                None
            }
            call = request(&mut peer) => {
                let method = call["method"].as_str().unwrap().to_owned();
                eprintln!("stage: native RPC after source revocation: {method}");
                // Let the real owner complete so the independent claim/marker
                // assertions and cleanup execute before the decisive failure.
                let reply = if method == "model/list" {
                    json!({"id":call["id"],"result":{"data":[{
                        "id":"fixture-preset","model":"fixture-private-model",
                        "displayName":"Fixture model","description":"test peer",
                        "supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"test effort"}],
                        "defaultReasoningEffort":"medium","isDefault":true
                    }],"nextCursor":null}})
                } else {
                    json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected post-revocation RPC"}})
                };
                send(&mut peer, reply).await;
                assert!(matches!(settled.await, Err(CalmError::Unauthorized)));
                Some(method)
            }
        }
    })
    .await
    .expect("the actual loop must finish or expose its next native request");
    let after = fx.frame().await;
    assert_eq!(
        after, before,
        "revoked input stays in its original pending position"
    );
    assert!(after.canonical_inputs.active.is_none());
    assert!(after.canonical_inputs.attempts.is_empty());
    let submissions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1")
            .bind(card_id)
            .fetch_one(fx.original.repo.pool())
            .await
            .unwrap();
    assert_eq!(submissions, 0);
    assert!(
        fx.harness
            .issuance_block()
            .await
            .as_deref()
            .is_some_and(|notice| notice.contains("session")),
        "the actual source refusal must reach standing feedback rather than only a log"
    );
    eprintln!("stage: loop completed; original pending preserved; no claim or marker");
    tokio::time::timeout(Duration::from_secs(5), fx.harness.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unexpected, None,
        "a revoked source cannot authorize a new private model/list request after config/read"
    );
}

async fn configure_default_effort(repo: &crate::db::sqlite::SqlxRepo, card_id: &str) {
    // Use the settings owner's real value transition: an explicit effort was
    // selected, then changed to the installation default. A config with no
    // effort must consult the actual model catalog to resolve that default.
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let mut payload = crate::operation::codex_adapter::card_payload_get_tx(&mut tx, card_id)
        .await
        .unwrap();
    let fields = payload.as_object_mut().unwrap();
    crate::planner_model::CardModelSelection::apply_to_payload(
        fields,
        Some("fixture-private-model"),
        Some("medium"),
    );
    crate::planner_model::CardModelSelection::apply_to_payload(
        fields,
        Some("fixture-private-model"),
        None,
    );
    crate::db::sqlite::card_update_tx(
        &mut tx,
        card_id,
        crate::model::CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn catalog_reply(call: &Value, next: Option<&str>) -> Value {
    json!({"id":call["id"],"result":{"data":[{
        "id":"fixture-preset","model":"fixture-private-model",
        "displayName":"Fixture model","description":"test peer",
        "supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"test effort"}],
        "defaultReasoningEffort":"medium","isDefault":true
    }],"nextCursor":next}})
}

async fn config_reply(
    peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer,
    call: &Value,
) {
    send(
        peer,
        json!({"id":call["id"],"result":{"config":{
            "model":"fixture-private-model","model_reasoning_effort":null
        }}}),
    )
    .await;
}

#[tokio::test]
async fn canonical_revoked_source_cannot_request_another_catalog_page() {
    let fx = Fixture::new().await;
    configure_default_effort(&fx.original.repo, fx.harness.inner.card_id.as_str()).await;
    let before = fx.frame().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let config = request(&mut peer).await;
    assert_eq!(config["method"], "config/read");
    config_reply(&mut peer, &config).await;
    let page = request(&mut peer).await;
    assert_eq!(page["method"], "model/list");
    fx.auth.sessions.remove(&fx.session).unwrap();
    send(&mut peer, catalog_reply(&page, Some("another-page"))).await;
    let settled = fx
        .harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone());
    tokio::pin!(settled);
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut settled => {
                assert!(matches!(result, Err(CalmError::Unauthorized)));
                None
            }
            call = request(&mut peer) => {
                send(&mut peer, catalog_reply(&call, None)).await;
                assert!(matches!(settled.await, Err(CalmError::Unauthorized)));
                Some(call["method"].clone())
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(fx.frame().await, before);
    fx.harness.shutdown().await.unwrap();
    assert_eq!(
        unexpected, None,
        "a prior catalog page is not authority for a new RPC"
    );
}

async fn bound_admission(fx: &Fixture) -> crate::conversation_inputs::SelectionReadAdmission {
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &fx.harness.inner.worker_session_id,
    )
    .await
    .unwrap();
    let plan = human_plan_for_pending_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(1).unwrap(),
        vec![],
    )
    .await
    .unwrap();
    let admission = crate::conversation_inputs::SelectionReadAdmission::pending_tx(
        &mut tx,
        fx.original.repo.clone(),
        fx.source.clone(),
        &carrier,
        plan,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    admission
}

#[tokio::test]
async fn sqlite_admission_under_sink_yields_before_server_reply_timeout() {
    use provider::codex::{PreSendGate, RequestCrossing, TurnApprovals};
    let fx = Fixture::new().await;
    let (client, mut peer) = connect(&fx.daemon).await;
    let admission = bound_admission(&fx).await;
    let before = fx.frame().await;
    let writer = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let entered = tokio::sync::Notify::new();
    let gate = PreSendGate::new(|| {
        Box::pin(async {
            entered.notify_one();
            admission
                .revalidate()
                .await
                .map_err(|error| provider::codex::error::Error::Refused(error.to_string()))
        })
    });
    let selection = provider::TurnModelSelection::inherit();
    let attempt = client.turn_start_attempt_gated(
        "thread",
        vec![],
        &selection,
        TurnApprovals::Unchanged,
        None,
        &gate,
    );
    tokio::pin!(attempt);
    tokio::select! {
        _ = entered.notified() => {}
        result = &mut attempt => panic!("admission did not wait for the real writer: {result:?}"),
    }
    send(
        &mut peer,
        json!({"id":"needs-reply","method":"fixture/unsupported","params":{}}),
    )
    .await;
    let (failure, reply) = tokio::join!(
        async { tokio::time::timeout(Duration::from_secs(4), &mut attempt).await },
        async { tokio::time::timeout(Duration::from_secs(4), peer.next()).await },
    );
    // Release the actual writer even on expected-red timeout.
    writer.rollback().await.unwrap();
    let failure = failure
        .expect("a blocked admission must release the sink before replies time out")
        .unwrap_err();
    assert_eq!(failure.crossing(), RequestCrossing::BeforeSend);
    let frame = reply
        .expect("the shared reply writer must acquire the sink")
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert_eq!(frame["id"], "needs-reply");
    assert_eq!(frame["error"]["code"], -32601);
    // A second real server request proves refusal did not poison the transport.
    send(
        &mut peer,
        json!({"id":"still-live","method":"fixture/unsupported","params":{}}),
    )
    .await;
    let frame = request(&mut peer).await;
    assert_eq!(frame["id"], "still-live");
    assert_eq!(fx.frame().await, before);
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_config_read_rechecks_source_after_the_actual_client_gate() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let admission = bound_admission(&fx).await;
    let gate =
        crate::shared_codex_appserver::native_owner_tests::hold_connected_client_gate(&fx.daemon)
            .await;
    let native_admission = crate::shared_codex_appserver::NativePromptAdmission::new(
        || admission.check_current(),
        || admission.admit_native_read(),
    );
    let read = fx.daemon.config_read_admitted(
        Some(admission.cwd()),
        tokio::time::Instant::now() + Duration::from_secs(5),
        &native_admission,
    );
    tokio::pin!(read);
    // Poll the real method while its actual connected-client owner is locked.
    // No source check is simulated and no ready/authorized flag is supplied.
    assert!(futures_util::poll!(read.as_mut()).is_pending());
    fx.auth.sessions.remove(&fx.session).unwrap();
    drop(gate);
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut read => {
                assert!(matches!(result, Err(CalmError::Unauthorized)));
                None
            }
            call = request(&mut peer) => {
                config_reply(&mut peer, &call).await;
                let _ = read.await;
                Some(call["method"].clone())
            }
        }
    })
    .await
    .unwrap();
    fx.harness.shutdown().await.unwrap();
    assert_eq!(
        unexpected, None,
        "a gate wait cannot preserve a revoked read admission"
    );
}

#[tokio::test]
async fn canonical_inner_cannot_use_the_historical_selection_admission() {
    let fx = Fixture::new().await;
    configure_default_effort(&fx.original.repo, fx.harness.inner.card_id.as_str()).await;
    let before = fx.frame().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let resolve = fx
        .harness
        .inner
        .backend
        .resolve_selection(fx.harness.inner.as_ref());
    tokio::pin!(resolve);
    let unexpected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut resolve => { assert!(result.is_err()); None }
            call = request(&mut peer) => {
                send(&mut peer, json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected raw canonical read"}})).await;
                let _ = resolve.await;
                Some(call["method"].clone())
            }
        }
    }).await.unwrap();
    assert_eq!(fx.frame().await, before);
    fx.harness.shutdown().await.unwrap();
    assert_eq!(unexpected, None);
}

#[tokio::test]
async fn historical_selection_preserves_its_existing_metadata_read_behavior() {
    let fx = ExistingRuntime::new().await;
    let inner = fx.harness.inner.clone();
    configure_default_effort(&fx.repo, inner.card_id.as_str()).await;
    let daemon = crate::shared_codex_appserver::SharedCodexAppServer::new_stub(fx.repo.clone());
    let (_client, mut peer) = connect(&daemon).await;
    let backend = PlannerBackend::codex(
        daemon,
        inner.card_id.as_str(),
        inner.track_id.as_str(),
        &inner.worker_session_id,
    )
    .unwrap();
    let expected_cwd = fx
        .repo
        .track_get(inner.track_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .workspace
        .agent_cwd()
        .to_owned();
    let resolve = tokio::spawn(async move { backend.resolve_selection(inner.as_ref()).await });
    let config = request(&mut peer).await;
    assert_eq!(config["method"], "config/read");
    assert_eq!(config["params"]["cwd"], expected_cwd);
    config_reply(&mut peer, &config).await;
    let page = request(&mut peer).await;
    assert_eq!(page["method"], "model/list");
    send(&mut peer, catalog_reply(&page, None)).await;
    let selected = resolve.await.unwrap().unwrap();
    assert_eq!(selected.model.as_deref(), Some("fixture-private-model"));
    assert_eq!(selected.effort.as_deref(), Some("medium"));
}

#[tokio::test]
async fn canonical_selection_read_preserves_appended_pending_while_issuing_original() {
    let fx = Fixture::new().await;
    configure_default_effort(&fx.original.repo, fx.harness.inner.card_id.as_str()).await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let config = request(&mut peer).await;
    assert_eq!(config["method"], "config/read");
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &fx.harness.inner.worker_session_id,
    )
    .await
    .unwrap();
    let appended = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "later pending, with no observed execution source".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    config_reply(&mut peer, &config).await;
    let page = request(&mut peer).await;
    assert_eq!(page["method"], "model/list");
    send(&mut peer, catalog_reply(&page, None)).await;
    let start = request(&mut peer).await;
    assert_eq!(start["method"], "turn/start");
    let input = start["params"]["input"].to_string();
    assert!(input.contains("canonical scheduler human body"));
    assert!(!input.contains("later pending"));
    let crossing = fx.frame().await;
    assert_eq!(crossing.canonical_inputs.pending.len(), 1);
    assert_eq!(
        crossing.canonical_inputs.pending[0].input_ref(),
        appended.input_ref
    );
    let thread = start["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"original-with-appended-tail","status":"inProgress"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":start["id"],"result":{"turn":{"id":"original-with-appended-tail"}}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"original-with-appended-tail","status":"completed"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(
        done.canonical_inputs.pending,
        crossing.canonical_inputs.pending
    );
    let mut stopping = tokio::spawn({
        let handle = fx.harness.clone();
        async move { handle.shutdown().await }
    });
    tokio::select! {
        result=&mut stopping => result.unwrap().unwrap(),
        call=request(&mut peer) => {
            assert_eq!(call["method"],"turn/interrupt");
            send(&mut peer,json!({"id":call["id"],"result":{}})).await;
            stopping.await.unwrap().unwrap();
        }
    }
}
