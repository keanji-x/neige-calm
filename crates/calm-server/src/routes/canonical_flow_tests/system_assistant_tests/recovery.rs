use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;

pub(super) async fn receipts(
    fixture: &Fixture,
    card: &str,
) -> Vec<calm_types::system_assistant::SystemAssistantReceipt> {
    sqlx::query_scalar::<_,String>("SELECT receipt_json FROM conversation_system_assistant_receipts WHERE card_id=?1 ORDER BY event_id")
        .bind(card).fetch_all(fixture.repo.pool()).await.unwrap().into_iter().map(|value|serde_json::from_str(&value).unwrap()).collect()
}
pub(super) async fn cold_next(peer: &mut Peer, claimed: bool) -> Value {
    loop {
        let call = request(peer).await;
        let result = match call["method"].as_str().unwrap() {
            "config/read" => json!({"config":{}}),
            "thread/start" if !claimed => json!({"thread":{"id":THREAD}}),
            "thread/read" => {
                assert_eq!(call["params"]["threadId"], THREAD);
                json!({"thread":{"id":THREAD,"status":{"type":"notLoaded"},"turns":[]}})
            }
            "thread/resume" => {
                assert_eq!(call["params"]["threadId"], THREAD);
                json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
            }
            "turn/start" => return call,
            method => panic!("unexpected cold Assistant request {method}"),
        };
        send(peer, json!({"id":call["id"],"result":result})).await;
    }
}
async fn boot_reload(peer: &mut Peer) {
    // As in boot_catalog_follower_tests: readiness owns one catalog reload.
    let call = native_request(peer).await;
    assert_eq!(call["method"], "config/mcpServer/reload");
    assert!(call["params"].is_null());
    send(peer, json!({"id":call["id"],"result":{}})).await;
}
async fn cold_original(
    claimed: bool,
    corrupt: bool,
    drift: bool,
    revoked: Option<authority::Revocation>,
) {
    cold_original_source(claimed, corrupt, drift, revoked, false).await;
}
async fn cold_original_source(
    claimed: bool,
    corrupt: bool,
    drift: bool,
    revoked: Option<authority::Revocation>,
    quiet: bool,
) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let (accepted, frozen_body) = if quiet {
        watcher::accept(&fixture, &state, &track).await
    } else {
        (
            summary(&state, &app, &cookie, "first", &body(1)).await,
            body(1),
        )
    };
    let card = accepted["card_id"].as_str().unwrap().to_owned();
    let original = receipts(&fixture, &card).await;
    let (worker, _) = frame(&fixture, &card).await;
    if claimed {
        sqlx::query(&format!("CREATE TRIGGER hold_summary_crossing BEFORE UPDATE OF handle_state_json ON worker_sessions WHEN NEW.id='{worker}' AND json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts')>0 BEGIN SELECT RAISE(ABORT,'hold crossing'); END"))
            .execute(fixture.repo.pool()).await.unwrap();
        let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
        state.dispatcher.reconcile_tick_for_test().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let before = frame(&fixture, &card).await.1;
                if before.canonical_inputs.active.is_some() {
                    assert!(before.canonical_inputs.attempts.is_empty());
                    break;
                }
                tokio::select! {
                    call=request(&mut peer)=> {
                        let result=match call["method"].as_str().unwrap() {
                            "config/read"=>json!({"config":{}}),
                            "thread/start"=>json!({"thread":{"id":THREAD}}),
                            method=>panic!("claim must not cross: {method}"),
                        };
                        send(&mut peer,json!({"id":call["id"],"result":result})).await;
                    }
                    _=tokio::time::sleep(Duration::from_millis(5))=>{}
                }
            }
        })
        .await
        .unwrap();
        shutdown(&fixture, &state).await;
    }
    let saved = frame(&fixture, &card).await.1;
    fixture.shutdown_process(&state).await;
    state
        .retire_dispatch_and_stop_provider_sweep()
        .await
        .unwrap();
    let repo = crate::state::WorkerState::from_ref(&state).repo;
    repo.sqlite_db().unwrap().close().await;
    drop((state, app, cookie));
    fixture.repo.db().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    if claimed {
        sqlx::query("DROP TRIGGER hold_summary_crossing")
            .execute(fixture.repo.pool())
            .await
            .unwrap();
    }
    if corrupt {
        sqlx::query("UPDATE events SET actor=?2 WHERE id=?1")
            .bind(original[0].event_id)
            .bind(serde_json::to_string(&crate::ids::ActorId::User).unwrap())
            .execute(fixture.repo.pool())
            .await
            .unwrap();
    }
    let _original_root =
        drift.then(|| std::mem::replace(&mut fixture.root, tempfile::tempdir().unwrap()));
    let (state, _app, _cookie) = fixture.application().await;
    assert_eq!(receipts(&fixture, &card).await, original);
    assert_eq!(frame(&fixture, &card).await.0, worker);
    assert_eq!(
        frame(&fixture, &card).await.1.canonical_inputs,
        saved.canonical_inputs
    );
    if let Some(change) = revoked {
        change.apply(&fixture, &state, &card).await;
    }
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    if drift || corrupt || revoked.is_some() {
        boot_reload(&mut peer).await;
    }
    state.dispatcher.reconcile_tick_for_test().await;
    if drift || corrupt || revoked.is_some() {
        let card_id = card.clone().into();
        tokio::select! {
            result = state.dispatcher.bootstrap_canonical_child(&card_id, &worker) => assert!(result.is_err()),
            call = request(&mut peer) => panic!("root drift crossed the native boundary: {}", call["method"]),
        }
        assert_eq!(
            frame(&fixture, &card).await.1.canonical_inputs,
            saved.canonical_inputs
        );
        crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
        assert_eq!(receipts(&fixture, &card).await, original);
        shutdown(&fixture, &state).await;
        return;
    }
    let native = Native {
        fixture: &fixture,
        state: &state,
        card: &card,
    };
    let bootstrap = cold_next(&mut peer, claimed).await;
    native
        .complete(
            &mut peer,
            bootstrap,
            if quiet {
                crate::worker_watch::watcher::BOOTSTRAP_TEXT.trim_end()
            } else {
                BOOTSTRAP
            },
            "cold-bootstrap",
        )
        .await;
    let body_call = next(&mut peer, false).await;
    native
        .complete(&mut peer, body_call, &frozen_body, "cold-summary")
        .await;
    let after = phase(&fixture, &card, HarnessPhaseTag::TurnCompleted).await;
    assert!(after.canonical_inputs.pending.is_empty());
    assert_eq!(after.canonical_inputs.attempts.len(), 2);
    assert_eq!(receipts(&fixture, &card).await, original);
    assert_eq!(frame(&fixture, &card).await.0, worker);
    if claimed {
        assert_eq!(
            after.canonical_inputs.attempts[0].attempt.submission_id,
            saved.canonical_inputs.active.unwrap().submission_ids[0]
        );
    }
    shutdown(&fixture, &state).await;
}
#[tokio::test]
async fn cold_boot_without_broadcast_or_handle_runs_original_bootstrap_and_body() {
    cold_original(false, false, false, None).await;
}
#[tokio::test]
async fn cold_unattempted_claim_requalifies_original_plan_and_continues_body() {
    cold_original(true, false, false, None).await;
}

#[tokio::test]
async fn unknown_native_disconnect_survives_file_reopen_without_resend() {
    unknown_source(false).await;
}
async fn unknown_source(quiet: bool) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&state, &app, &cookie).await;
    let (accepted, _frozen_body) = if quiet {
        watcher::accept(&fixture, &state, &track).await
    } else {
        (
            summary(&state, &app, &cookie, "first", &body(1)).await,
            body(1),
        )
    };
    let card = accepted["card_id"].as_str().unwrap().to_owned();
    let original = receipts(&fixture, &card).await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let _crossing = next(&mut peer, true).await;
    let (worker, before) = frame(&fixture, &card).await;
    assert_eq!(
        before.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    );
    fixture
        .repo
        .fixture_transcript_insert(
            &worker,
            &card,
            accepted["track_id"].as_str().unwrap(),
            THREAD,
            Some("unknown-turn"),
            Some("preserved-sentinel"),
            Some("agentMessage"),
            "item/completed",
            "{}",
            Some(r#"[{"presentation":"assistant","text":"preserved sentinel"}]"#),
        )
        .await
        .unwrap();
    peer.close(None).await.unwrap();
    let uncertain = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let saved = frame(&fixture, &card).await.1;
            if saved.canonical_inputs.attempts[0].delivery == InputDeliveryFact::Unknown {
                break saved;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    shutdown(&fixture, &state).await;
    assert_eq!(
        frame(&fixture, &card).await.1.canonical_inputs,
        uncertain.canonical_inputs
    );
    state
        .retire_dispatch_and_stop_provider_sweep()
        .await
        .unwrap();
    let repo = crate::state::WorkerState::from_ref(&state).repo;
    repo.sqlite_db().unwrap().close().await;
    drop((client, peer, state, app, cookie));
    fixture.repo.db().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, _app, _cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    boot_reload(&mut peer).await;
    state.dispatcher.reconcile_tick_for_test().await;
    assert!(matches!(
        state
            .dispatcher
            .bootstrap_canonical_child(&card.clone().into(), &worker)
            .await,
        Err(crate::error::CalmError::Conflict(_))
    ));
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(receipts(&fixture, &card).await, original);
    assert_eq!(frame(&fixture, &card).await.0, worker);
    assert_eq!(
        frame(&fixture, &card).await.1.canonical_inputs,
        uncertain.canonical_inputs
    );
    assert_eq!(
        fixture
            .transcript_count(&card, Some(&worker), None, Some("preserved-sentinel"))
            .await,
        1
    );

    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn cold_pending_below_watermark_rechecks_original_source() {
    cold_original(false, true, false, None).await;
}
#[tokio::test]
async fn cold_unattempted_claim_below_watermark_rechecks_original_source() {
    cold_original(true, true, false, None).await;
}

#[tokio::test]
async fn different_workspace_root_refuses_original_work_without_replacement() {
    cold_original(false, false, true, None).await;
}

#[tokio::test]
async fn accepted_pending_rechecks_role_worker_profile_provider_and_producer() {
    for change in authority::REVOCATIONS {
        cold_original(false, false, false, Some(change)).await;
    }
}
#[tokio::test]
async fn unattempted_claim_rechecks_role_worker_profile_provider_and_producer() {
    for change in authority::REVOCATIONS {
        cold_original(true, false, false, Some(change)).await;
    }
}

#[tokio::test]
async fn watcher_cold_pending_recovers_original_input_after_observed_task_finishes() {
    cold_original_source(false, false, false, None, true).await;
}
#[tokio::test]
async fn watcher_cold_unattempted_claim_recovers_without_old_renderer() {
    cold_original_source(true, false, false, None, true).await;
}
#[tokio::test]
async fn watcher_unknown_survives_file_reopen_without_resend() {
    unknown_source(true).await;
}
#[tokio::test]
async fn watcher_pending_and_claim_recheck_all_target_revocations() {
    for change in authority::REVOCATIONS {
        for claimed in [false, true] {
            cold_original_source(claimed, false, false, Some(change), true).await;
        }
    }
}
