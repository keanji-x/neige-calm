//! A live input's source must not queue behind unrelated lifecycle restoration.
use super::*;

#[tokio::test]
async fn live_send_hands_off_authority_before_waiting_for_lifecycle_fence() {
    live_send_source_handoff(false, false).await;
}

#[tokio::test]
async fn cancelled_http_waiter_keeps_committed_source_handoff_alive() {
    live_send_source_handoff(true, false).await;
}

#[tokio::test]
async fn live_source_handoff_keeps_invalid_attached_workspace_from_native_execution() {
    live_send_source_handoff(false, true).await;
}

async fn live_send_source_handoff(cancel_http: bool, invalid_workspace: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
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
    if !invalid_workspace {
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
    if !invalid_workspace {
        tokio::time::timeout(Duration::from_secs(5), paused.entered.notified())
            .await
            .unwrap();
        // Force the real scheduler to read the committed input before its
        // source arrives, rather than depending on a particular HTTP/tick race.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state
                    .harness
                    .get(&worker)
                    .unwrap()
                    .issuance_block()
                    .await
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the saved input must reach the real pre-handoff scheduler");
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
