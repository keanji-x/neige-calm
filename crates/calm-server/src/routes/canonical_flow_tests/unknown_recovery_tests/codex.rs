//! Native completion recovery retains the original canonical execution and pending order.
use super::*;
#[tokio::test]
async fn canonical_codex_cold_accepted_completion_preserves_reply_and_pending_order() {
    codex_completion("exact").await;
}
#[tokio::test]
async fn canonical_codex_cold_accepted_completion_refuses_other_turn() {
    codex_completion("wrong-turn").await;
}
#[tokio::test]
async fn canonical_codex_cold_accepted_completion_refuses_other_thread() {
    codex_completion("wrong-thread").await;
}
#[tokio::test]
async fn canonical_codex_cold_accepted_completion_rechecks_human_source() {
    codex_completion("revoke").await;
}

#[tokio::test]
async fn canonical_codex_cold_accepted_completion_refuses_other_backend() {
    Box::pin(codex_completion("backend")).await;
}
#[tokio::test]
async fn canonical_codex_cold_accepted_completion_rechecks_backend_after_read() {
    Box::pin(codex_completion("backend-race")).await;
}
#[tokio::test]
async fn canonical_codex_cold_accepted_completion_rechecks_current_carrier() {
    Box::pin(codex_completion("carrier-race")).await;
}

async fn bind_other_backend(fixture: &Fixture, worker: &str) {
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    // Controlled namespace change: persisted backend metadata is the owning authority.
    sqlx::query(concat!("INSERT INTO agent_backend_registry(provider,backend_id,protocol,configured_home,canonical_home) ","VALUES('codex','other','codex',?1,?1)"))
        .bind(fixture.root.path().join("other-home").to_str().unwrap().as_bytes()).execute(&mut **tx).await.unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=NULL WHERE id=?1")
        .bind(worker)
        .execute(&mut **tx)
        .await
        .unwrap();
    crate::db::sqlite::session_backend::insert_tx(&mut tx, worker, "codex", "other")
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
        .bind(THREAD)
        .bind(worker)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_codex_cold_accepted_completion_refuses_missing_final_items() {
    Box::pin(codex_completion("missing-items")).await;
}
#[tokio::test]
async fn canonical_codex_cold_accepted_completion_refuses_duplicate_item_identity() {
    Box::pin(codex_completion("duplicate-items")).await;
}

#[tokio::test]
async fn canonical_codex_cold_unloaded_accepted_completion_resumes_before_pending_dispatch() {
    Box::pin(codex_completion("cold")).await;
}

async fn codex_completion(scenario: &str) {
    let succeeds = matches!(scenario, "exact" | "cold");
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Native completion","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created=post(&app,&cookie,"/api/tracks","initial",json!({"area_id":area["id"],"title":"","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let call = super::native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnRunning)
        .await;
    let (card, worker, original) = fixture.carrier(&track).await;
    let queued = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "queued-a",
        json!({"text":"Queued alpha after original"}),
    )
    .await;
    let before = fixture.carrier(&track).await.2;
    assert_eq!(before.canonical_inputs.pending.len(), 1);
    let pending = before.canonical_inputs.pending.clone();
    // Snapshot the actual production checkpoint BEFORE normal shutdown can persist Stop intent.
    let cold = fixture.root.path().join("cold-completion.db");
    sqlx::query("VACUUM INTO ?1")
        .bind(cold.to_str().unwrap())
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    super::native_attempt_recovery_tests::shutdown(&state, &worker, &mut peer).await;
    peer.close(None).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((state, app, peer, _client));
    fixture.url = format!("sqlite:{}", cold.display());
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    if scenario == "backend" {
        bind_other_backend(&fixture, &worker).await;
    }
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", &card, wake.clone());
    let appended = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "queued-b",
        json!({"text":"Queued beta after restart"}),
    )
    .await;
    let after_enqueue = fixture.carrier(&track).await.2;
    assert_eq!(after_enqueue.canonical_inputs.pending[0], pending[0]);
    let expected_pending = after_enqueue.canonical_inputs.pending.clone();
    let mut completed = json!({"id":if scenario=="wrong-turn"{"another-turn"}else{TURN},"status":"completed","items":[{"id":"recovered-final","type":"agentMessage","text":"Recovered native final reply"}]});
    if scenario == "missing-items" {
        completed.as_object_mut().unwrap().remove("items");
    }
    if scenario == "duplicate-items" {
        let duplicate = completed["items"][0].clone();
        completed["items"].as_array_mut().unwrap().push(duplicate);
    }
    let next=tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            tokio::select! {
                _=wake.entered.notified(),if !succeeds=>break None,
                call=request(&mut peer)=>{
                    let result=match call["method"].as_str().unwrap() {
                        "config/read"=>json!({"config":{}}),
                        "thread/read"=>{
                            assert_eq!(call["params"]["threadId"],THREAD);
                            if scenario=="revoke" {post(&app,&cookie,"/api/auth/logout","logout",json!({})).await;}
                            if scenario=="backend-race" {bind_other_backend(&fixture,&worker).await;}
                            if scenario=="carrier-race" {sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1").bind(&worker).execute(fixture.repo.pool()).await.unwrap();}

                            json!({"thread":{"id":if scenario=="wrong-thread"{"another-thread"}else{THREAD},"status":{"type":if scenario=="cold"{"notLoaded"}else{"idle"}},"turns":[completed.clone()]}})
                        }
                        "thread/resume"=>{
                            assert!(succeeds,"invalid completion must not authorize resume");
                            json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[completed.clone()]}})
                        }
                        "turn/start"=>{assert!(succeeds,"invalid completion dispatched new input");break Some(call);}
                        method=>panic!("recovery must retain original native namespace: {method}"),
                    };
                    send(&mut peer,json!({"id":call["id"],"result":result})).await;
                }
            }
        }
    }).await.expect("accepted original completion must reconcile through native owner");
    wake.release.notify_one();
    let (_, same_worker, after) = fixture.carrier(&track).await;
    assert_eq!(same_worker, worker);
    assert_eq!(after.header.push_watermark, before.header.push_watermark);
    if let Some(next) = next {
        // The owner may dispatch the accepted messages in separate native turns.
        // Verify identity/order/exactly-once delivery without requiring batching.
        let mut current = next;
        let mut bodies = Vec::new();
        let mut native_turns = vec![completed.clone()];
        for index in 0..2 {
            assert_eq!(current["params"]["threadId"], THREAD);
            let body = current["params"]["input"].to_string();
            assert!(
                !body.contains(FIRST_BODY),
                "original accepted prompt was resent"
            );
            bodies.push(body);
            let turn = format!("resumed-next-{index}");
            send(
                &mut peer,
                json!({"id":current["id"],"result":{"turn":{"id":turn}}}),
            )
            .await;
            notify_thread(
                &state.shared_codex_appserver,
                &mut peer,
                THREAD,
                "turn/started",
                json!({"id":turn,"status":"inProgress"}),
            )
            .await;
            let end = json!({"id":turn,"status":"completed","items":[]});
            notify_thread(
                &state.shared_codex_appserver,
                &mut peer,
                THREAD,
                "turn/completed",
                end.clone(),
            )
            .await;
            native_turns.push(end);
            if bodies
                .iter()
                .any(|body| body.contains("Queued beta after restart"))
            {
                break;
            }
            current=tokio::time::timeout(Duration::from_secs(10),async {
                loop {
                    let call=request(&mut peer).await;
                    let result=match call["method"].as_str().unwrap() {
                        "turn/start"=>break call,
                        "config/read"=>json!({"config":{}}),
                        "thread/read"=>json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":native_turns.clone()}}),
                        method=>panic!("next accepted input must retain native ownership: {method}"),
                    };
                    send(&mut peer,json!({"id":call["id"],"result":result})).await;
                }
            }).await.expect("later accepted Human input must progress after the first ends");
        }
        assert_eq!(
            bodies
                .iter()
                .filter(|body| body.contains("Queued alpha after original"))
                .count(),
            1
        );
        assert_eq!(
            bodies
                .iter()
                .filter(|body| body.contains("Queued beta after restart"))
                .count(),
            1
        );
        let body = bodies.join("\n");
        assert!(
            body.find("Queued alpha after original").unwrap()
                < body.find("Queued beta after restart").unwrap()
        );
        fixture
            .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
            .await;
        let actual:Vec<(String,i64,i64)>=sqlx::query_as(concat!("SELECT message_id,revision,queue_position FROM conversation_submission_inputs WHERE message_id IN ","(?1,?2) ORDER BY queue_position"))
            .bind(queued["entry_id"].as_str().unwrap()).bind(appended["entry_id"].as_str().unwrap()).fetch_all(fixture.repo.pool()).await.unwrap();
        let expected: Vec<_> = expected_pending
            .iter()
            .map(|pending| {
                let calm_types::conversation::PendingInput::Human {
                    message,
                    queue_position,
                    ..
                } = pending
                else {
                    panic!("expected accepted Human input")
                };
                (
                    message.message_id.as_str().to_owned(),
                    i64::from(message.revision.get()),
                    i64::try_from(*queue_position).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            actual, expected,
            "queued identity/revision/order changed at the claim owner"
        );
        assert_ne!(queued["entry_id"], appended["entry_id"]);
        assert!(
            after
                .canonical_inputs
                .attempts
                .iter()
                .any(|a| a.attempt == original.canonical_inputs.attempts[0].attempt)
        );
        let rows = fixture
            .repo
            .transcript_rows_of_thread(&card, THREAD)
            .await
            .unwrap();
        let items: Vec<&str> = rows
            .iter()
            .filter(|row| row.method == "item/completed")
            .map(|row| row.params.as_str())
            .collect();
        assert_eq!(
            items
                .iter()
                .filter(|p| p.contains("Recovered native final reply"))
                .count(),
            1
        );
        // Duplicate original terminal remains a single historical outcome.
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            completed.clone(),
        )
        .await;
        let terminals = fixture
            .repo
            .transcript_rows_of_thread(&card, THREAD)
            .await
            .unwrap()
            .iter()
            .filter(|row| row.method == "turn/completed" && row.turn_id.as_deref() == Some(TURN))
            .count();
        assert_eq!(terminals, 1);
        state.harness.shutdown_and_remove(&worker).await.unwrap();
    } else {
        assert_eq!(
            after.canonical_inputs.active,
            before.canonical_inputs.active
        );
        assert_eq!(
            after.canonical_inputs.attempts,
            before.canonical_inputs.attempts
        );
        assert_eq!(after.canonical_inputs.pending, expected_pending);
        let replies = fixture
            .repo
            .transcript_rows_of_thread(&card, THREAD)
            .await
            .unwrap()
            .iter()
            .filter(|row| row.params.contains("Recovered native final reply"))
            .count();
        assert_eq!(replies, 0);
    }
    peer.close(None).await.unwrap();
    fixture.shutdown_process(&state).await;
}
