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
        fixture.shutdown_process(&state).await;
        drop((state, app));
        fixture.repo.pool().close().await;
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
                    eprintln!("cold recovery codex maintenance method={}", call["method"]);
                    assert_eq!(call["method"], "config/mcpServer/reload");
                    assert!(call["params"].is_null());
                    send(peer, json!({"id":call["id"],"result":{}})).await;
                }
            }
            assert_acp_maintenance_only(&fixture, &path);
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
            let execution: (i64, i64, i64) = sqlx::query_as(
                "SELECT
                 (SELECT count(*) FROM acp_managed_sessions WHERE worker_session_id=?1),
                 (SELECT count(*) FROM acp_submissions WHERE worker_session_id=?1),
                 (SELECT count(*) FROM card_mcp_tokens WHERE card_id=?2)",
            )
            .bind(&worker)
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
            assert_eq!(
                execution,
                (0, 0, 0),
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

/// The existing spy records every RPC by process, and maintenance worker markers
/// plus credential presence separately. Catalog discovery creates and closes a
/// native session with empty MCP servers; that is not accepted-carrier execution.
#[cfg(feature = "fixtures")]
fn assert_acp_maintenance_only(fixture: &Fixture, workspace: &std::path::Path) {
    fn records(root: &std::path::Path, name: &str) -> Vec<Value> {
        let text = match std::fs::read_to_string(root.join(name)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => panic!("ACP spy {name}: {error}"),
        };
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    let root = fixture.root.path();
    let mut processes = std::collections::BTreeMap::<u64, Vec<Value>>::new();
    for record in records(root, "requests.jsonl") {
        processes
            .entry(record["_pid"].as_u64().unwrap())
            .or_default()
            .push(record);
    }
    let environments = records(root, "environment.jsonl");
    let mut catalogs = 0;
    for (pid, calls) in &processes {
        let methods: Vec<_> = calls
            .iter()
            .map(|call| call["method"].as_str().unwrap())
            .collect();
        // Only methods and process IDs leave the fixture, never params or env values.
        eprintln!("cold recovery ACP maintenance pid={pid} methods={methods:?}");
        match methods.as_slice() {
            ["initialize"] => {} // readiness worker, verified by the marker counts below
            ["initialize", "session/new", "session/close"] => {
                catalogs += 1;
                let params = &calls[1]["params"];
                assert_eq!(params["mcpServers"], json!([]));
                assert_ne!(
                    std::path::Path::new(params["cwd"].as_str().unwrap()),
                    workspace
                );
                let session = calls[2]["params"]["sessionId"].as_str().unwrap();
                assert!(!session.is_empty());
                // The spy names this file from the session it returned, and close
                // itself checks that the requested ID is its current session.
                let native: Value = serde_json::from_str(
                    &std::fs::read_to_string(root.join(format!("{session}.json"))).unwrap(),
                )
                .unwrap();
                assert_eq!(native["inputs"], json!([]));
                assert_eq!(native["cwd"], params["cwd"]);
            }
            _ => panic!("unexpected ACP execution/setup methods (see sanitized trace)"),
        }
    }
    let mut catalog_workers = 0;
    let mut readiness_workers = 0;
    for env in &environments {
        for credential in [
            "NEIGE_MCP_TOKEN",
            "NEIGE_MCP_SOCKET",
            "NEIGE_MCP_DAEMON_TOKEN",
        ] {
            assert_eq!(
                env[credential], false,
                "maintenance must have no {credential}"
            );
        }
        match (env["catalog"].as_bool(), env["readiness"].as_bool()) {
            (Some(true), Some(false)) => catalog_workers += 1,
            (Some(false), Some(true)) => readiness_workers += 1,
            _ => panic!("ACP process must belong to a maintenance worker"),
        }
    }
    assert_eq!(catalog_workers, catalogs);
    assert_eq!(readiness_workers, processes.len() - catalogs);
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
        state.harness.shutdown_and_remove(&worker).await.unwrap();
        fixture.shutdown_process(&state).await;
    }
}
