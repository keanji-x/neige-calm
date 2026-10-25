//! Postcommit failure is durable acceptance, followed by original-key repair.
use super::*;
use crate::test_seams::{PausePoint, install_pause_for_test};

async fn phase(
    events: &mut tokio::sync::broadcast::Receiver<crate::event::BroadcastEnvelope>,
    worker: &str,
    phase: HarnessPhaseTag,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let envelope = events.recv().await.unwrap();
            if let crate::event::Event::HarnessPhaseChanged {
                worker_session_id,
                new_phase,
                ..
            } = envelope.event
                && worker_session_id == worker
            {
                assert!(envelope.id > 0);
                eprintln!("committed phase event: {new_phase:?}");
                if new_phase == phase {
                    break;
                }
            }
        }
    })
    .await
    .expect("subscribed production phase event");
}

#[tokio::test]
async fn u8b_canonical_workspace_failure_holds_v2_and_repairs_once() {
    for (provider, isolation) in [
        ("codex", "default"),
        ("opencode", "default"),
        ("codex", "none"),
        ("opencode", "none"),
    ] {
        eprintln!("cold repair scenario {provider} {isolation}");
        let mut fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let a = area(&app, &cookie, "cold repair").await;
        let path = repo(&fixture);
        let mut request_body = borrowed(&a, provider, &path, isolation);
        request_body["first_message"] = json!(FIRST_BODY);
        let pause = PausePoint::default();
        install_pause_for_test(
            "canonical-workspace-committed",
            a["id"].as_str().unwrap(),
            pause.clone(),
        );
        let mut creating = {
            let app = app.clone();
            let cookie = cookie.clone();
            let request = request_body.clone();
            tokio::spawn(async move { create(&app, &cookie, "first", request).await })
        };
        tokio::select! {
            _ = pause.entered.notified() => {},
            result = &mut creating => panic!("missing postcommit seam: {result:?}"),
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("missing postcommit seam"),
        }
        let binding = crate::db::sqlite::track_create_idempotency_get_pool(
            fixture.repo.pool(),
            a["id"].as_str().unwrap(),
            "first",
        )
        .await
        .unwrap()
        .unwrap();
        let track = fixture
            .repo
            .track_get(&binding.track_id)
            .await
            .unwrap()
            .unwrap();
        let worktree = std::path::PathBuf::from(track.workspace.agent_cwd());
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let displaced = fixture.root.path().join("displaced-repo");
        if isolation == "none" {
            std::fs::rename(&path, &displaced).unwrap();
        }
        std::fs::write(&worktree, "postcommit fault").unwrap();
        let original = fixture.carrier(&binding.track_id).await;
        let receipt = fixture.receipt(a["id"].as_str().unwrap()).await;
        pause.release.notify_one();
        assert!(!creating.await.unwrap().0.is_success());
        assert_eq!(fixture.carrier(&binding.track_id).await, original);
        assert_eq!(original.2.canonical_inputs.pending.len(), 1);
        assert!(original.2.canonical_inputs.attempts.is_empty());
        assert!(state.harness.get(&original.1).is_none());
        let durable_counts = counts(&fixture).await;
        for _ in 0..2 {
            assert!(
                create(&app, &cookie, "first", request_body.clone())
                    .await
                    .0
                    .is_success()
                    == (isolation == "none")
            );
            assert_eq!(counts(&fixture).await, durable_counts);
            assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
        }
        let folders = fixture
            .repo
            .area_folders_by_area(a["id"].as_str().unwrap())
            .await
            .unwrap();
        delete_claim(
            &app,
            &cookie,
            &a,
            &serde_json::to_value(&folders[0]).unwrap(),
        )
        .await;
        let reclaim = area(&app, &cookie, "new claimant").await;
        claim(&app, &cookie, &reclaim, &path).await;
        fixture.shutdown_process(&state).await;
        drop((state, app));
        // The in-flight HTTP future has finished; close and reopen the actual database.
        tokio::time::timeout(Duration::from_secs(10), fixture.repo.pool().close())
            .await
            .expect("old database connections released before cold reopen");
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
        if provider == "opencode" {
            super::super::opencode_creation_tests::configure(&mut fixture);
        }
        let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
        assert!(
            create(&app, &cookie, "first", request_body.clone())
                .await
                .0
                .is_success()
                == (isolation == "none")
        );
        assert_eq!(fixture.carrier(&binding.track_id).await, original);
        let grants: i64 =
            sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens WHERE card_id=?1")
                .bind(&original.0)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert_eq!(grants, 0);
        let native: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT thread_id,agent_session_id,mcp_token_hash FROM worker_sessions WHERE id=?1",
        )
        .bind(&original.1)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(native, (None, None, None));
        let acp: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM acp_managed_sessions WHERE worker_session_id=?1),
             (SELECT count(*) FROM acp_submissions WHERE worker_session_id=?1)",
        )
        .bind(&original.1)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(acp, (0, 0));
        let raw: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                .bind(&original.1)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
        assert!(
            serde_json::from_str::<Value>(&raw)
                .unwrap()
                .get("pending_queue")
                .is_none()
        );
        std::fs::remove_file(&worktree).unwrap();
        if isolation == "none" {
            std::fs::rename(&displaced, &path).unwrap();
        }
        let mut events = state.events.subscribe();
        let ready = PausePoint::default();
        install_pause_for_test("retained-workspace-ready", &original.0, ready.clone());
        let mut native = if provider == "codex" {
            Some(connect(&state.shared_codex_appserver).await)
        } else {
            None
        };
        let began = std::time::Instant::now();
        let repaired = post(&app, &cookie, "/api/tracks", "first", request_body.clone()).await;
        tokio::time::timeout(Duration::from_secs(10), ready.entered.notified())
            .await
            .expect("actual materialization before retained claim");
        assert!(worktree.is_dir());
        read(&app, &cookie, &binding.track_id, MARKER).await;
        assert_eq!(fixture.carrier(&binding.track_id).await, original);
        ready.release.notify_one();
        if let Some((_, peer)) = native.as_mut() {
            let turn = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let call = request(peer).await;
                    let result = match call["method"].as_str().unwrap() {
                        "config/read" => json!({"config":{}}),
                        "thread/start" => {
                            assert_eq!(call["params"]["cwd"], worktree.to_str().unwrap());
                            json!({"thread":{"id":THREAD}})
                        }
                        "turn/start" => break call,
                        method => panic!("unexpected native method {method}"),
                    };
                    send(peer, json!({"id":call["id"],"result":result})).await;
                }
            })
            .await
            .unwrap();
            assert!(turn["params"]["input"].to_string().contains(FIRST_BODY));
            let crossing = fixture.carrier(&binding.track_id).await;
            assert_eq!(crossing.2.canonical_inputs.attempts.len(), 1);
            assert!(matches!(
                crossing.2.canonical_inputs.attempts[0].delivery,
                InputDeliveryFact::CrossingUnknown
            ));
            send(peer, json!({"id":turn["id"],"result":{"turn":{"id":TURN}}})).await;
            notify_thread(
                &state.shared_codex_appserver,
                peer,
                THREAD,
                "turn/started",
                json!({"id":TURN,"status":"inProgress"}),
            )
            .await;
            phase(&mut events, &original.1, HarnessPhaseTag::TurnRunning).await;
            notify_thread(
                &state.shared_codex_appserver,
                peer,
                THREAD,
                "turn/completed",
                json!({"id":TURN,"status":"completed"}),
            )
            .await;
        }
        // A subscription precedes dispatch: even ACP's transient phase cannot be
        // missed by polling. Durable state below independently proves completion.
        phase(&mut events, &original.1, HarnessPhaseTag::TurnCompleted).await;
        eprintln!(
            "{provider} cold repair to durable completed event: {:?}",
            began.elapsed()
        );
        let done = fixture.carrier(&binding.track_id).await;
        assert_eq!((&done.0, &done.1), (&original.0, &original.1));
        assert!(done.2.canonical_inputs.pending.is_empty());
        assert!(done.2.canonical_inputs.active.is_none());
        assert_eq!(done.2.canonical_inputs.attempts.len(), 1);
        if provider == "codex" {
            assert!(matches!(
                done.2.canonical_inputs.attempts[0].delivery,
                InputDeliveryFact::Accepted { .. }
            ));
        } else {
            // ACP reports the receipted local turn as Unknown; END closes the
            // execution without inventing a provider delivery acknowledgement.
            assert_eq!(
                done.2.canonical_inputs.attempts[0].delivery,
                InputDeliveryFact::Unknown
            );
        }
        let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
            .await
            .unwrap();
        let submission = crate::db::sqlite::conversation::submission_plan_tx(
            &mut tx,
            &done.2.canonical_inputs.attempts[0].attempt.submission_id,
        )
        .await
        .unwrap();
        assert_eq!(
            submission.plan.input_refs().unwrap(),
            original
                .2
                .canonical_inputs
                .pending
                .iter()
                .map(|entry| entry.accepted_input_ref().unwrap())
                .collect::<Vec<_>>()
        );
        tx.rollback().await.unwrap();
        if provider == "opencode" {
            let records =
                std::fs::read_to_string(fixture.root.path().join("requests.jsonl")).unwrap();
            let prompts: Vec<Value> = records
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|record| record["method"] == "session/prompt")
                .collect();
            assert_eq!(prompts.len(), 1);
            assert!(
                prompts[0]["params"]["prompt"]
                    .to_string()
                    .contains(FIRST_BODY)
            );
        }
        let exit = PausePoint::default();
        install_pause_for_test("canonical-human-wake-finished", &original.0, exit.clone());
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(10),
                post(&app, &cookie, "/api/tracks", "first", request_body)
            )
            .await
            .expect("completed original-key replay"),
            repaired
        );
        tokio::time::timeout(Duration::from_secs(5), exit.entered.notified())
            .await
            .unwrap();
        exit.release.notify_one();
        assert_eq!(
            fixture.carrier(&binding.track_id).await.2.canonical_inputs,
            done.2.canonical_inputs
        );
        assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
        let locks = crate::state::RouteState::from_ref(&state).planner_recovery_locks;
        let _lifecycle = tokio::time::timeout(
            Duration::from_secs(10),
            crate::per_card_lock::lock_card(&locks, &original.0),
        )
        .await
        .expect("replay wake released its lifecycle fence");
        drop(_lifecycle);
        if provider == "codex" && isolation == "none" {
            let (_, peer) = native.as_mut().unwrap();
            let route = crate::state::RouteState::from_ref(&state);
            let mut violations = Vec::new();
            for cold in [false, true] {
                if cold {
                    super::super::native_attempt_recovery_tests::shutdown(
                        &state,
                        &original.1,
                        peer,
                    )
                    .await;
                }
                assert_eq!(state.harness.get(&original.1).is_none(), cold);
                assert_eq!(done.2.header.last_thread_id.as_deref(), Some(THREAD));
                let worker_id = original.1.clone().into();
                let worker = fixture.repo.session_get(&worker_id).await.unwrap();
                let before = counts(&fixture).await;
                std::fs::rename(&path, &displaced).unwrap();
                let mut headers = axum::http::HeaderMap::new();
                headers.insert(header::COOKIE, cookie.parse().unwrap());
                let (status, source) =
                    crate::auth::authenticated_source(auth.clone(), headers).await;
                assert_eq!(status, StatusCode::OK);
                let fence = crate::operation::planner_start_fence::CardStartFence::lock(
                    &route.planner_recovery_locks,
                    &route.repo,
                    &route.operation_runtime,
                    &original.0.clone().into(),
                )
                .await;
                crate::harness::backend::factory::PlannerFactory::new(
                    Arc::downgrade(&state.shared_codex_appserver),
                    &route.claude_planner,
                    &route.acp_planner,
                )
                .ready_retained(&crate::session_projection_repo::AgentProvider::Codex)
                .await
                .unwrap();
                let source = source.unwrap();
                let wake = fence.wake_retained(
                    route.planner_start_adapter.clone(),
                    source.clone(),
                    route.workspace_root.clone(),
                    route.events.clone(),
                );
                tokio::pin!(wake);
                tokio::time::timeout(Duration::from_secs(10), async {
                    tokio::select! {
                        result = &mut wake => {
                            if result.is_ok() {
                                violations.push(format!("cold={cold}: missing directory wake succeeded"));
                            }
                        },
                        call = request(peer) => {
                            violations.push(format!("cold={cold}: native crossing {}", call["method"]));
                            // Finish the real fence task even on the deliberately red tree.
                            send(peer, json!({"id":call["id"],"error":{"code":-32000,"message":"unexpected crossing"}})).await;
                            assert!(wake.await.is_err());
                        },
                    }
                })
                .await
                .expect("ready retained wake must decide without a native crossing");
                assert_eq!(fixture.repo.session_get(&worker_id).await.unwrap(), worker);
                assert_eq!(fixture.carrier(&binding.track_id).await, done);
                assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
                assert_eq!(counts(&fixture).await, before);
                assert_eq!(state.harness.get(&original.1).is_none(), cold);
                assert!(!path.exists(), "none admission must not materialize");
                std::fs::rename(&displaced, &path).unwrap();
                read(&app, &cookie, &binding.track_id, MARKER).await;
                let restored = async {
                    crate::operation::planner_start_fence::CardStartFence::lock(
                        &route.planner_recovery_locks,
                        &route.repo,
                        &route.operation_runtime,
                        &original.0.clone().into(),
                    )
                    .await
                    .wake_retained(
                        route.planner_start_adapter.clone(),
                        source,
                        route.workspace_root.clone(),
                        route.events.clone(),
                    )
                    .await
                };
                tokio::pin!(restored);
                let mut methods = Vec::new();
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        tokio::select! {
                            result = &mut restored => break result.unwrap(),
                            call = request(peer) => {
                                let method = call["method"].as_str().unwrap();
                                assert!(cold && matches!(method, "thread/read" | "thread/resume"));
                                methods.push(method.to_string());
                                send(peer, json!({"id":call["id"],"result":{"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}}})).await;
                            },
                        }
                    }
                })
                .await
                .expect("restored resource permits the original ready lifecycle");
                assert_eq!(
                    methods,
                    if cold {
                        vec!["thread/read", "thread/resume"]
                    } else {
                        vec![]
                    }
                );
                assert!(state.harness.get(&original.1).is_some());
                assert_eq!(
                    fixture.carrier(&binding.track_id).await.2.canonical_inputs,
                    done.2.canonical_inputs
                );
                assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
                eprintln!("completed none retained missing-resource case cold={cold}");
            }
            assert!(violations.is_empty(), "{violations:?}");
        }
        if let Some((_, peer)) = native.as_mut() {
            super::super::native_attempt_recovery_tests::shutdown(&state, &original.1, peer).await;
        } else {
            tokio::time::timeout(
                Duration::from_secs(10),
                state.harness.shutdown_and_remove(&original.1),
            )
            .await
            .expect("ACP teardown")
            .unwrap();
        }
        fixture.shutdown_process(&state).await;
    }
}
