//! Failure and cold recovery stay on the accepted carrier and Workspace owners.
use super::*;

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn u8_canonical_materialization_failure_cold_recovery_retains_acceptance() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    use futures::{FutureExt, StreamExt};
    for provider in ["codex", "opencode"] {
        let mut fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let a = area(&app, &cookie, "recover").await;
        let mut body = body(&a, provider);
        body["admin"] = json!(true);
        body["first_message"] = json!(FIRST_BODY);
        let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
        let track = created["id"].as_str().unwrap().to_owned();
        let (card, worker, original) = fixture.carrier(&track).await;
        let receipt = fixture.receipt(a["id"].as_str().unwrap()).await;
        let path = std::path::PathBuf::from(created["workspace"]["path"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "materialization poison").unwrap();
        std::fs::write(
            fixture.root.path().join("cold-observer-workspace"),
            path.to_str().unwrap(),
        )
        .unwrap();
        fixture.shutdown_process(&state).await;
        drop((state, app));
        tokio::time::timeout(Duration::from_secs(10), fixture.repo.pool().close())
            .await
            .expect("U8 cold database connections released");
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
        if provider == "opencode" {
            super::super::opencode_creation_tests::configure(&mut fixture);
        }
        let (state, app, cookie) = fixture.application().await;
        let mut native = if provider == "codex" {
            Some(connect(&state.shared_codex_appserver).await)
        } else {
            None
        };
        // Keep the materialization failure in place. Observe each cold replay's
        // actual wake exit, never an old failed operation or a particular phase.
        // Post-repair phase progress and its timing belong to U8b.
        for replay in 0..2 {
            let wake = PausePoint::default();
            install_pause_for_test("canonical-human-wake-finished", &card, wake.clone());
            let ack = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
            tokio::time::timeout(Duration::from_secs(5), wake.entered.notified())
                .await
                .unwrap_or_else(|error| {
                    panic!("{provider} cold replay {replay} wake exit: {error}")
                });
            // This seam is after the wake's fence release. It proves exit only;
            // it neither holds that fence nor claims Running/Completed progress.
            wake.release.notify_one();
            assert_eq!(
                ack, created,
                "{provider} replay must return the original ACK"
            );
            let down = fixture.carrier(&track).await;
            assert_eq!((&down.0, &down.1), (&card, &worker));
            assert_eq!(down.2.canonical_inputs, original.canonical_inputs);
            assert_eq!(down.2.canonical_inputs.pending.len(), 1);
            assert!(down.2.canonical_inputs.active.is_none());
            assert!(down.2.canonical_inputs.attempts.is_empty());
            assert!(down.2.header.last_thread_id.is_none());
            assert!(down.2.header.last_turn_id.is_none());
            assert!(
                state.harness.get(&worker).is_none(),
                "no false native readiness"
            );
            if let Some((_, peer)) = native.as_mut() {
                // At wake exit any execution RPC is already visible to the peer.
                // The independent boot catalog reload is maintenance only.
                while let Some(Some(message)) = peer.next().now_or_never() {
                    let message = message.unwrap();
                    let call: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                    eprintln!("cold recovery codex observed method={}", call["method"]);
                    assert_eq!(call["method"], "config/mcpServer/reload");
                    assert!(call["params"].is_null());
                    send(peer, json!({"id":call["id"],"result":{}})).await;
                }
            }
            assert_acp_no_execution(&fixture);
            let session = fixture
                .repo
                .session_get(&calm_types::worker::WorkerSessionId(worker.clone()))
                .await
                .unwrap()
                .unwrap();
            assert!(session.thread_id.is_none());
            assert!(session.agent_session_id.is_none());
            assert!(session.active_turn_id.is_none());
            assert!(session.mcp_token_hash.is_none());
            let execution: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
                "SELECT
                 (SELECT count(*) FROM acp_managed_sessions WHERE worker_session_id=?1),
                 (SELECT count(*) FROM acp_submissions WHERE worker_session_id=?1),
                 (SELECT count(*) FROM card_mcp_tokens WHERE card_id=?2),
                 (SELECT count(*) FROM worker_sessions WHERE id=?1 AND provider_execution_json IS NOT NULL),
                 (SELECT count(*) FROM workspace_executions
                  WHERE json_extract(intent_json,'$.consumer')='acp-planner'
                    AND json_extract(intent_json,'$.owner')='worker-session:' || ?1),
                 (SELECT count(*) FROM conversation_input_submissions WHERE card_id=?2)",
            )
            .bind(&worker)
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
            assert_eq!(
                execution,
                (0, 0, 0, 0, 0, 0),
                "no execution ownership, delivery or MCP grant"
            );
            assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
            assert_eq!(fixture.counts(&card).await, (1, 1, 1, 0, 0));
            let raw: String =
                sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                    .bind(&worker)
                    .fetch_one(fixture.repo.pool())
                    .await
                    .unwrap();
            let raw: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                raw["schema_version"],
                serde_json::to_value(CanonicalHarnessSnapshot::dormant(0)).unwrap()["schema_version"]
            );
            assert!(raw.get("pending_queue").is_none(), "no V1 queue fallback");
            assert!(path.is_file(), "the Workspace fault must remain present");
        }
        fixture.shutdown_process(&state).await;
    }
}

// A snapshot may end at any RPC prefix: boot discovery is independent of the
// wake. Identity comes from this single trace, never marker/count pairing. This
// proves only the observed safe protocol, not an unknown process's launch owner.
#[cfg(feature = "fixtures")]
fn assert_acp_no_execution(fixture: &Fixture) {
    // Metadata discovery need not launch. Accepted-owner SQL checks in the caller
    // remain unconditional; every observed process/RPC still passes the oracle.
    let text = match std::fs::read_to_string(fixture.root.path().join("cold-trace.jsonl")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("ACP trace: {error}"),
    };
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_cold_trace(&records);
}

#[cfg(feature = "fixtures")]
fn assert_cold_trace(records: &[Value]) {
    // pid -> (last protocol stage, new request ID, returned native session ID)
    let mut processes = std::collections::BTreeMap::<u64, (u8, Value, Value)>::new();
    for record in records {
        let pid = record["pid"].as_u64().expect("trace PID");
        eprintln!(
            "cold ACP pid={pid} schema={} credential={} event={} id={} method={}",
            record["schema"], record["credential"], record["event"], record["id"], record["method"]
        );
        assert_eq!(record["schema"], 1, "trace schema");
        assert_eq!(record["credential"], false, "no ACP credentials");
        if record["event"] == "start" {
            assert!(
                processes
                    .insert(pid, (0, Value::Null, Value::Null))
                    .is_none()
            );
            continue;
        }
        let (stage, new_id, native) = processes.get_mut(&pid).expect("trace start");
        match (record["event"].as_str(), record["method"].as_str()) {
            (Some("request"), Some("initialize")) => {
                assert_eq!(*stage, 0);
                assert!(!record["id"].is_null());
                *stage = 1;
            }
            (Some("request"), Some("session/new")) => {
                assert_eq!(*stage, 1);
                assert_eq!(record["mcp_empty"], true, "no accepted MCP servers");
                assert_eq!(record["accepted_cwd"], false, "no accepted Workspace");
                assert!(!record["id"].is_null());
                *new_id = record["id"].clone();
                *stage = 2;
            }
            (Some("result"), Some("session/new")) => {
                assert_eq!(*stage, 2);
                assert_eq!(record["id"], *new_id, "actual RPC response ID");
                assert!(
                    record["session_id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty())
                );
                *native = record["session_id"].clone();
                *stage = 3;
            }
            (Some("request"), Some("session/close")) => {
                assert_eq!(*stage, 3);
                assert_eq!(
                    record["session_id"], *native,
                    "close actual returned session"
                );
                assert!(!record["id"].is_null());
                *stage = 4;
            }
            _ => panic!("unexpected ACP execution/setup RPC (sanitized metadata above)"),
        }
    }
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn u8_canonical_cold_observer_no_launch_needs_no_peer_trace() {
    let fixture = Fixture::new().await;
    assert!(!fixture.root.path().join("cold-trace.jsonl").exists());
    assert_acp_no_execution(&fixture);
}

/// Controlled real spy launches prove the oracle rejects execution facts without
/// mutating production or copying a provider implementation into this test.
#[cfg(feature = "fixtures")]
#[test]
fn u8_canonical_cold_observer_real_peer_counterexamples() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    fn rpc(child: &mut std::process::Child, out: &mut impl BufRead, call: Value) -> Value {
        writeln!(child.stdin.as_mut().unwrap(), "{call}").unwrap();
        loop {
            let mut line = String::new();
            assert!(out.read_line(&mut line).unwrap() > 0, "real spy response");
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == call["id"] {
                return response["result"].clone();
            }
        }
    }
    for variant in ["safe", "credential", "cwd", "prompt"] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("poison");
        std::fs::write(&workspace, "poison").unwrap();
        std::fs::write(
            root.path().join("cold-observer-workspace"),
            workspace.to_str().unwrap(),
        )
        .unwrap();
        std::fs::write(root.path().join("scenario"), "reply").unwrap();
        let mut command = Command::new("/usr/bin/python3");
        command
            .args([
                "-u",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/acp_planner/agent.py"
                ),
            ])
            .env_clear()
            .env("ACP_FIXTURE_ROOT", root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if variant == "credential" {
            command.env("NEIGE_MCP_TOKEN", "test-only-credential");
        }
        let mut child = command.spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        rpc(&mut child, &mut out, json!({"id":1,"method":"initialize"}));
        let cwd = if variant == "cwd" {
            &workspace
        } else {
            root.path()
        };
        let native = rpc(
            &mut child,
            &mut out,
            json!({"id":2,"method":"session/new","params":{"cwd":cwd,"mcpServers":[]}}),
        );
        if variant == "prompt" {
            rpc(
                &mut child,
                &mut out,
                json!({"id":3,"method":"session/prompt","params":{"sessionId":native["sessionId"],"prompt":[{"type":"text","text":"controlled counterexample"}]}}),
            );
        }
        rpc(
            &mut child,
            &mut out,
            json!({"id":4,"method":"session/close","params":{"sessionId":native["sessionId"]}}),
        );
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        let text = std::fs::read_to_string(root.path().join("cold-trace.jsonl")).unwrap();
        let records: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let accepted = std::panic::catch_unwind(|| assert_cold_trace(&records)).is_ok();
        assert_eq!(
            accepted,
            variant == "safe",
            "real peer counterexample {variant}"
        );
    }
}

/// Pin the small observer contract independently of production launch policy.
/// Every counterexample changes one fact in an otherwise safe protocol trace.
#[cfg(feature = "fixtures")]
#[test]
fn u8_canonical_cold_observer_rejects_execution_evidence() {
    let safe = vec![
        json!({"schema":1,"pid":7,"credential":false,"event":"start"}),
        json!({"schema":1,"pid":7,"credential":false,"event":"request","id":1,"method":"initialize"}),
        json!({"schema":1,"pid":7,"credential":false,"event":"request","id":2,"method":"session/new","mcp_empty":true,"accepted_cwd":false}),
        json!({"schema":1,"pid":7,"credential":false,"event":"result","id":2,"method":"session/new","session_id":"returned"}),
        json!({"schema":1,"pid":7,"credential":false,"event":"request","id":3,"method":"session/close","session_id":"returned"}),
    ];
    for end in 0..=safe.len() {
        assert_cold_trace(&safe[..end]);
    }
    for (index, field, value) in [
        (0, "credential", json!(true)),
        (2, "mcp_empty", json!(false)),
        (2, "accepted_cwd", json!(true)),
        (2, "method", json!("session/prompt")),
        (2, "method", json!("session/load")),
        (2, "method", json!("session/resume")),
        (2, "method", json!("unknown/execution")),
        (3, "id", json!(42)),
        (4, "session_id", json!("unrelated")),
    ] {
        let mut unsafe_trace = safe.clone();
        unsafe_trace[index][field] = value;
        assert!(
            std::panic::catch_unwind(|| assert_cold_trace(&unsafe_trace)).is_err(),
            "observer must reject counterexample {index}/{field}"
        );
    }
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn u8_canonical_active_managed_admin_repoint_retains_worker_and_input() {
    for provider in ["codex", "opencode"] {
        let mut fixture = Fixture::new().await;
        if provider == "opencode" {
            super::super::opencode_creation_tests::configure(&mut fixture);
            std::fs::write(fixture.root.path().join("scenario"), "hold").unwrap();
        }
        let (state, app, cookie) = fixture.application().await;
        let a = area(&app, &cookie, "active").await;
        let mut body = body(&a, provider);
        body["admin"] = json!(true);
        body["first_message"] = json!(FIRST_BODY);
        let mut native = if provider == "codex" {
            Some(connect(&state.shared_codex_appserver).await)
        } else {
            None
        };
        let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
        let track = created["id"].as_str().unwrap();
        if let Some((_, peer)) = native.as_mut() {
            let call = super::super::native_attempt_recovery_tests::initial_request(peer).await;
            send(peer, json!({"id":call["id"],"result":{"turn":{"id":TURN}}})).await;
            notify_thread(
                &state.shared_codex_appserver,
                peer,
                THREAD,
                "turn/started",
                json!({"id":TURN,"status":"inProgress"}),
            )
            .await;
        }
        fixture
            .wait_phase(track, HarnessPhaseTag::TurnRunning)
            .await;
        let (card, worker, frame) = fixture.carrier(track).await;
        assert!(frame.canonical_inputs.active.is_some());
        let original = fixture.repo.track_get(track).await.unwrap().unwrap();
        let identity = retained_identity(&fixture, a["id"].as_str().unwrap(), &worker).await;
        let receipt = fixture.receipt(a["id"].as_str().unwrap()).await;
        let target = fixture.root.path().join("target");
        crate::test_support::init_fixture_git_repo(&target);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/tracks/{track}"))
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            serde_json::to_value(fixture.repo.track_get(track).await.unwrap().unwrap()).unwrap(),
            serde_json::to_value(original).unwrap()
        );
        assert_eq!(
            fixture.carrier(track).await,
            (card.clone(), worker.clone(), frame)
        );
        assert_eq!(
            retained_identity(&fixture, a["id"].as_str().unwrap(), &worker).await,
            identity
        );
        assert!(state.harness.get(&worker).is_some());
        assert_eq!(fixture.receipt(a["id"].as_str().unwrap()).await, receipt);
        // End the actual peer before taking down its driver.
        if let Some((_, peer)) = native.as_mut() {
            notify_thread(
                &state.shared_codex_appserver,
                peer,
                THREAD,
                "turn/completed",
                json!({"id":TURN,"status":"completed"}),
            )
            .await;
            fixture
                .wait_phase(track, HarnessPhaseTag::TurnCompleted)
                .await;
        }
        if provider == "opencode" {
            post(
                &app,
                &cookie,
                &format!("/api/cards/{card}/planner/interrupt"),
                "interrupt",
                json!({}),
            )
            .await;
            tokio::time::timeout(Duration::from_secs(10), async {
                while fixture
                    .carrier(track)
                    .await
                    .2
                    .canonical_inputs
                    .active
                    .is_some()
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        let locks = crate::state::RouteState::from_ref(&state).planner_recovery_locks;
        let _lifecycle = crate::per_card_lock::lock_card(&locks, &card).await;
        if let Some((_, peer)) = native.as_mut() {
            super::super::native_attempt_recovery_tests::shutdown(&state, &worker, peer).await;
        } else {
            state.harness.shutdown_and_remove(&worker).await.unwrap();
        }
        fixture.shutdown_process(&state).await;
    }
}
