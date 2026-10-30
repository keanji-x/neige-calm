//! A live input's source must not queue behind unrelated lifecycle restoration.
use super::*;

#[tokio::test]
async fn live_send_hands_off_authority_before_waiting_for_lifecycle_fence() {
    live_send_source_handoff(false, false, false).await;
}

#[tokio::test]
async fn cancelled_http_waiter_keeps_committed_source_handoff_alive() {
    live_send_source_handoff(true, false, false).await;
}

#[tokio::test]
async fn live_source_handoff_keeps_invalid_attached_workspace_from_native_execution() {
    live_send_source_handoff(false, true, false).await;
}

#[tokio::test]
async fn revoked_during_handoff_releases_wait_and_requires_a_current_source() {
    live_send_source_handoff(false, false, true).await;
}

async fn live_send_source_handoff(cancel_http: bool, invalid_workspace: bool, revoke: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let attached = fixture.root.path().join("attached-input-handoff");
    let track =
        if invalid_workspace {
            std::fs::create_dir(&attached).unwrap();
            for args in [
                vec!["init", "--initial-branch=main"],
                vec![
                    "-c",
                    "user.name=fixture",
                    "-c",
                    "user.email=fixture@example.com",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ],
            ] {
                let result = workspace::command::isolated_git_command()
                    .arg("-C")
                    .arg(&attached)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            let area = post(
                &app,
                &cookie,
                "/api/areas",
                "area",
                json!({"name":"Handoff Workspace","color":"#123456"}),
            )
            .await;
            let created = post(&app, &cookie, "/api/tracks", "track", json!({
            "area_id":area["id"],"title":"Attached source handoff","planner_provider":"codex",
            "cwd":attached,"isolation":"none","attach_folder":true,
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}
        })).await;
            created["id"].as_str().unwrap().to_owned()
        } else {
            assistant_canonical::track(&app, &cookie).await
        };
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "source-handoff-create",
        json!({"provider":"codex","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    assistant_canonical::complete_assistant_turn(&fixture, &state, &mut peer, card).await;
    if invalid_workspace {
        // Change the real user-owned Git directory, never its database binding.
        std::fs::rename(attached.join(".git"), attached.join(".git-held")).unwrap();
    }
    let (worker, _) = assistant_canonical::carrier(&fixture, card).await;
    let locks = crate::state::RouteState::from_ref(&state).planner_recovery_locks;
    let lifecycle = crate::per_card_lock::lock_card(&locks, card).await;
    let send_app = app.clone();
    let send_cookie = cookie.clone();
    let uri = format!("/api/cards/{card}/planner/input");
    let paused = crate::test_seams::PausePoint::default();
    let missing_source = crate::test_seams::PausePoint::default();
    if !invalid_workspace {
        crate::test_seams::install_pause_for_test(
            "canonical-missing-source-observed",
            card,
            missing_source.clone(),
        );
        crate::test_seams::install_pause_for_test(
            "canonical-human-source-handoff",
            card,
            paused.clone(),
        );
    }
    let sending = tokio::spawn(async move {
        post(
            &send_app,
            &send_cookie,
            &uri,
            "source-handoff-send",
            json!({"text":"Run this newly saved input on the existing live owner"}),
        )
        .await
    });
    let mut pre_handoff = None;
    if !invalid_workspace {
        tokio::time::timeout(Duration::from_secs(5), paused.entered.notified())
            .await
            .unwrap();
        // Observe a complete real scheduler decision while the accepted source
        // handoff is held. Missing context must keep the input queued, not
        // classify an unchecked session as an authentication failure.
        tokio::time::timeout(Duration::from_secs(5), missing_source.entered.notified())
            .await
            .unwrap();
        let handle = state.harness.get(&worker).unwrap();
        let notice = handle.issuance_block().await;
        let queued_counts = fixture.counts(card).await;
        missing_source.release.notify_one();
        pre_handoff = Some((notice, queued_counts));
    }
    if revoke {
        auth.sessions
            .remove(cookie.split_once('=').unwrap().1)
            .unwrap();
    }
    if cancel_http {
        sending.abort();
        assert!(sending.await.unwrap_err().is_cancelled());
        paused.release.notify_one();
    } else {
        if !invalid_workspace {
            paused.release.notify_one();
        }
        let ack = sending.await.unwrap();
        assert_eq!(ack["worker_session_id"], worker);
    }
    if revoke {
        // The fallback owner must be allowed to finish and release its guard.
        drop(lifecycle);
        let handle = state.harness.get(&worker).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.issuance_block().await.as_deref()
                    == Some("A current session must authorize this saved input.")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("failed handoff must not leave ordinary waiting forever");
        let unexpected = tokio::time::timeout(Duration::from_millis(150), request(&mut peer)).await;
        let counts = fixture.counts(card).await;
        peer.close(None).await.unwrap();
        fixture.shutdown_process(&state).await;
        assert!(
            unexpected.is_err(),
            "revoked handoff cannot reach private/native reads"
        );
        assert_eq!(counts, (2, 2, 1, 1, 0));
        assert_eq!(pre_handoff.unwrap().0, None);
        return;
    }
    let next = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let call = request(&mut peer).await;
            if invalid_workspace {
                break call;
            }
            match call["method"].as_str().unwrap() {
                "config/read" => {
                    send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                }
                "turn/start" => break call,
                method => panic!("a live send must keep its original native owner: {method}"),
            }
        }
    })
    .await;
    let block = state.harness.get(&worker).unwrap().issuance_block().await;
    let counts = fixture.counts(card).await;
    // Release every real lifecycle/native waiter before reporting a regression.
    drop(lifecycle);
    if let Ok(call) = &next
        && call["method"] == "turn/start"
    {
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"turn":{"id":"source-handoff-turn"}}}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":"source-handoff-turn","status":"completed"}),
        )
        .await;
    } else if let Ok(call) = &next {
        send(&mut peer, json!({"id":call["id"],"error":{"code":-32000,"message":"Workspace admission must precede private reads"}})).await;
    }
    peer.close(None).await.unwrap();
    fixture.shutdown_process(&state).await;
    if invalid_workspace {
        assert!(
            next.is_err(),
            "an invalid attached Workspace must not reach a private/native request: {next:?}"
        );
        assert_eq!(
            counts,
            (2, 2, 1, 1, 0),
            "the new input stays saved without a new claim or native attempt"
        );
        return;
    }
    let (notice, queued_counts) = pre_handoff.unwrap();
    assert_eq!(notice, None, "source handoff is ordinary pending work");
    assert_eq!(
        queued_counts,
        (2, 2, 1, 1, 0),
        "no claim or native attempt before source handoff"
    );
    let call = next.expect(
        "a saved live input must receive its source without waiting for lifecycle restoration",
    );
    assert!(
        call["params"]["input"]
            .to_string()
            .contains("newly saved input")
    );
    assert_eq!(call["params"]["threadId"], THREAD);
    assert_eq!(
        block, None,
        "the current authenticated send must not be held for missing authority"
    );
    assert_eq!(
        counts,
        (2, 2, 1, 2, 0),
        "one original message and one new input, each submitted exactly once"
    );
}

#[tokio::test]
async fn accepted_send_replay_keeps_its_receipt_without_a_current_carrier() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = assistant_canonical::track(&app, &cookie).await;
    // No native peer: acceptance is independent of activation readiness.
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "receipt-create",
        json!({"provider":"codex","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    let uri = format!("/api/cards/{card}/planner/input");
    let body = json!({"text":"Preserve this original send receipt"});
    let original = post(&app, &cookie, &uri, "original-send", body.clone()).await;
    let refused = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "unsupported-replacement")
                .body(Body::from(
                    json!({"text":"Do not accept this mutation","replaces_turn":"unsupported"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let missing: calm_types::conversation::CanonicalPendingIdentity =
        calm_types::conversation::MessageRevisionRef {
            message_id: calm_types::conversation::ConversationMessageId::parse(&"f".repeat(32))
                .unwrap(),
            revision: std::num::NonZeroU32::new(1).unwrap(),
        }
        .into();
    let index = state.harness.canonical_sources(
        original["worker_session_id"].as_str().unwrap(),
        &track.clone().into(),
    );
    assert!(
        matches!(
            index.readiness(&missing),
            crate::harness::registry::SourceReadiness::NeedsSource
        ),
        "a refused transaction must release its pre-commit handoff guard"
    );
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(card)
        .execute(fixture.repo.db().writer_pool())
        .await
        .unwrap();
    let replay = post(&app, &cookie, &uri, "original-send", body).await;
    let submissions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1")
            .bind(card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    fixture.shutdown_process(&state).await;
    assert_eq!(
        replay, original,
        "a missing execution owner cannot invalidate an accepted receipt"
    );
    assert_eq!(
        submissions, 0,
        "receipt replay cannot manufacture execution"
    );
}
