//! Deterministic ingress after the switch has parked both captured carriers.
use super::*;
use crate::test_seams::{PausePoint, install_pause_for_test};

type Switching =
    tokio::task::JoinHandle<Result<planner_switch::PlannerSwitchReceipt, PlannerSwitchError>>;

async fn captured(p: &Pair) -> (PausePoint, Switching) {
    let pause = PausePoint::default();
    install_pause_for_test("planner-switch-before-commit", &p.cards[0], pause.clone());
    let route = RouteState::from_ref(&p.state);
    let human = p.human.clone();
    let request = p.request();
    let switching = tokio::spawn(async move {
        planner_switch::switch_planner(&route, &Actor("user".into()), &human, request).await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    (pause, switching)
}

async fn refused_after(p: &Pair, pause: PausePoint, switching: Switching, expected: &str) {
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(10), switching)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result.as_ref().err().and_then(code),
        Some(expected),
        "switch succeeded: {}",
        result.is_ok()
    );
    p.unchanged().await;
    assert_eq!(http::detail(p).await["planner_revision"], 0);
}

#[tokio::test]
async fn planner_switch_native_binding_after_capture_refused() {
    let mut p = Pair::with_provider("claude").await;
    let (_, before) = assistant_canonical::carrier(&p.fixture, &p.cards[1]).await;
    let attempt = &before.canonical_inputs.attempts[0];
    assert!(matches!(
        attempt.delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert!(before.canonical_inputs.receipts.is_empty());
    claude_creation_tests::assistant::wait_frame(&p.fixture, &p.cards[1], |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.referenced_submissions().is_empty()
    })
    .await;
    let pause = PausePoint::default();
    install_pause_for_test("planner-switch-before-commit", &p.cards[0], pause.clone());
    let route = RouteState::from_ref(&p.state);
    let human = p.human.clone();
    let switch_request = p.request();
    let switching = tokio::spawn(async move {
        planner_switch::switch_planner(&route, &Actor("user".into()), &human, switch_request).await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();

    // Use the same attribution owner as native session registration, not a SQL
    // edit of agent_session_id. Preserve the existing native thread identity.
    let worker = p.workers[1].clone();
    let stored = p
        .fixture
        .repo
        .session_get(&worker.clone().into())
        .await
        .unwrap()
        .unwrap();
    let native = uuid::Uuid::new_v4().to_string();
    let registered = native.clone();
    crate::db::write_in_tx_typed(p.state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_bind_attribution_tx(
                tx,
                &worker,
                crate::session_projection_repo::ThreadAttribution {
                    worker_session_id: worker.clone(),
                    provider: crate::session_projection_repo::AgentProvider::Claude,
                    thread_id: stored.thread_id,
                    session_id: Some(registered),
                    active_turn_id: stored.active_turn_id,
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let bound: Option<String> =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&p.workers[1])
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(bound.as_deref(), Some(native.as_str()));
    pause.release.notify_one();

    let switch = async { switching.await.unwrap() };
    tokio::pin!(switch);
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut switch => break result,
                call = request(&mut p.peer) => {
                    assert!(matches!(call["method"].as_str(), Some("thread/unsubscribe" | "turn/interrupt" | "config/read")), "{call}");
                    send(&mut p.peer, json!({"id":call["id"],"result":{}})).await;
                }
            }
        }
    }).await.unwrap();
    assert_eq!(
        result.as_ref().err().and_then(code),
        Some("planner_changed"),
        "native registration succeeded; switch must refuse (switch succeeded: {})",
        result.is_ok()
    );
    p.unchanged().await;
    assert_eq!(http::detail(&p).await["planner_revision"], 0);
    let persisted: Option<String> =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&p.workers[1])
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(persisted.as_deref(), Some(native.as_str()));
    p.close().await;
}

#[tokio::test]
async fn planner_switch_message_after_capture_refused() {
    for index in [0, 1] {
        let p = Pair::new().await;
        let (pause, switching) = captured(&p).await;
        let sent = tokio::time::timeout(
            Duration::from_secs(5),
            post(
                &p.app,
                &p.cookie,
                &format!("/api/cards/{}/planner/input", p.cards[index]),
                "overlapping-message",
                json!({"text":"Preserve overlapping input"}),
            ),
        )
        .await
        .expect("accepted ingress must not wait for the paused switch");
        assert_eq!(sent["worker_session_id"], p.workers[index]);
        refused_after(&p, pause, switching, "planner_changed").await;
        let text: String = sqlx::query_scalar(
            "SELECT payload_json FROM conversation_message_revisions WHERE message_id=?1 ORDER BY revision DESC LIMIT 1")
            .bind(sent["entry_id"].as_str().unwrap()).fetch_one(p.fixture.repo.pool()).await.unwrap();
        assert!(text.contains("Preserve overlapping input"));
        p.close().await;
    }
}

#[tokio::test]
async fn planner_switch_event_after_capture_refused() {
    let p = Pair::new().await;
    let (pause, switching) = captured(&p).await;
    let area = p
        .fixture
        .repo
        .track_get(&p.track)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let id = p
        .fixture
        .repo
        .log_pure_event(
            crate::ids::ActorId::KernelDispatcher,
            crate::event::EventScope::Track {
                track: p.track.clone().into(),
                area,
            },
            None,
            &p.state.events,
            &p.state.card_role_cache,
            &p.state.track_area_cache,
            crate::event::Event::TrackReportEdited {
                track_id: p.track.clone().into(),
                card_id: p.report_card_id().await,
                author: crate::event::EditAuthor::User,
                author_plugin_id: None,
                edit_id: "overlapping-event".into(),
                summary_before: String::new(),
                summary_after: String::new(),
                body_before: String::new(),
                body_after: "Preserve overlapping event".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
    refused_after(&p, pause, switching, "planner_pending_observations").await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE id=?1")
        .bind(id)
        .fetch_one(p.fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    p.close().await;
}

#[tokio::test]
async fn planner_switch_ask_after_capture_refused() {
    let p = Pair::new().await;
    let (pause, switching) = captured(&p).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(p.fixture.repo.db())
        .await
        .unwrap();
    let (scope, event) = crate::ask::ask_requested_tx(
        &mut tx,
        &p.cards[0].clone().into(),
        vec![crate::event::AskQuestion {
            title: "Overlapping ask".into(),
            options: vec!["Yes".into()],
        }],
    )
    .await
    .unwrap();
    let id = crate::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &crate::ids::ActorId::AiPlannerSession(p.workers[0].clone().into()),
        &scope,
        None,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    refused_after(&p, pause, switching, "planner_open_ask").await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE id=?1")
        .bind(id)
        .fetch_one(p.fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    p.close().await;
}

#[tokio::test]
async fn planner_switch_old_credential_mcp_request_refused() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut p = Pair::new().await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(p.fixture.repo.db())
        .await
        .unwrap();
    let token =
        crate::mcp_server::wiring::mint_and_persist_card_token(&mut tx, &p.cards[0], &p.workers[0])
            .await
            .unwrap();
    tx.commit().await.unwrap();
    let socket = &p.state.mcp_server.as_ref().unwrap().shim_config.socket_path;
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2024-11-05","_meta":{"dev.neige/auth":{"token":token}}}});
    let mut connection = BufReader::new(tokio::net::UnixStream::connect(socket).await.unwrap());
    connection
        .get_mut()
        .write_all(format!("{initialize}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), connection.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let admitted: Value = serde_json::from_str(&line).unwrap();
    assert!(admitted.get("result").is_some(), "{admitted}");
    p.switch().await.unwrap();
    // A fresh connection exercises the actual product handshake with the old credential.
    let socket = &p.state.mcp_server.as_ref().unwrap().shim_config.socket_path;
    let mut connection = BufReader::new(tokio::net::UnixStream::connect(socket).await.unwrap());
    connection
        .get_mut()
        .write_all(format!("{initialize}\n").as_bytes())
        .await
        .unwrap();
    line.clear();
    tokio::time::timeout(Duration::from_secs(5), connection.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let denied: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        denied["error"]["code"],
        crate::mcp_server::handshake::TOKEN_NOT_RECOGNIZED_CODE
    );
    // Retirement also rejects the handshake, so pin revocation itself: a
    // superseded-state check must not hide retained credentials in either store.
    let hash: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&p.workers[0])
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    assert!(
        hash.is_none(),
        "old Planner credential mirror must be revoked"
    );
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens WHERE card_id=?1")
        .bind(&p.cards[0])
        .fetch_one(p.fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(tokens, 0, "old Planner card credential must be revoked");
    p.close().await;
}

/// Product behavior is to refuse cutover while an admitted MCP mutation is in flight.
#[tokio::test]
async fn planner_switch_admitted_mcp_write_blocks_cutover() {
    use crate::mcp_server::{registry::ConnectionIdentity, transport::dispatch_tools_call};
    let mut p = Pair::new().await;
    let ctx = p.state.mcp_context();
    let registry = Arc::new(crate::mcp_server::build_default_registry());
    let worker = p
        .fixture
        .repo
        .session_get(&p.workers[0].clone().into())
        .await
        .unwrap()
        .unwrap();
    let thread = worker.thread_id.unwrap();
    // Exercise the production MCP dispatcher, including identity resolution,
    // role checks, read ledger and the report mutation's normal persistence owner.
    let params = |name: &str, arguments: Value| {
        json!({
            "name": name, "arguments": arguments, "_meta": {"threadId": thread}
        })
    };
    dispatch_tools_call(
        &ctx,
        None,
        params("neige_report_read", json!({})),
        &ConnectionIdentity::DaemonTrust,
        &registry,
    )
    .await
    .unwrap();
    let before: Value =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(&p.track)
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();

    let discovery = PausePoint::default();
    install_pause_for_test("mcp-identity-discovered", &p.workers[0], discovery.clone());
    let arguments = params(
        "neige_report_write",
        json!({
            "body": "# Admitted write\nPreserve the old Planner's accepted mutation.",
            "message": "admitted-before-cutover"
        }),
    );
    let mut writing = tokio::spawn(async move {
        dispatch_tools_call(
            &ctx,
            None,
            arguments,
            &ConnectionIdentity::DaemonTrust,
            &registry,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), discovery.entered.notified())
        .await
        .unwrap();
    // The second discovery occurs with ToolCallGuard held, after the active
    // session has been revalidated. Re-arm before releasing the first discovery.
    let admitted = PausePoint::default();
    install_pause_for_test("mcp-identity-discovered", &p.workers[0], admitted.clone());
    discovery.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), admitted.entered.notified())
        .await
        .unwrap();
    assert!(
        p.state
            .mcp_context()
            .tool_calls
            .fence(&p.workers[0])
            .is_err()
    );
    let result = p.switch().await;
    assert_eq!(
        result.as_ref().err().and_then(code),
        Some("planner_pending_submission")
    );
    p.unchanged().await;
    assert_eq!(http::detail(&p).await["planner_revision"], 0);
    let paused: Value =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(&p.track)
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(paused, before, "the admitted mutation has not written yet");

    let mut completions = RouteState::from_ref(&p.state)
        .activity_wake
        .subscribe_report_completions_for_test()
        .unwrap();
    admitted.release.notify_one();
    let written = match tokio::time::timeout(Duration::from_secs(5), &mut writing).await {
        Ok(joined) => joined.unwrap().unwrap(),
        Err(error) => {
            writing.abort();
            let _ = writing.await;
            p.close().await;
            panic!("admitted write timed out: {error}");
        }
    };
    assert!(written.get("structuredContent").is_some(), "{written}");
    let after: Value =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(&p.track)
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    assert!(
        after
            .to_string()
            .contains("Preserve the old Planner's accepted mutation.")
    );
    // Identify the one durable edit from this old Planner, independently of text/doc_rev.
    let report = p.report_card_id().await;
    let area = p
        .fixture
        .repo
        .track_get(&p.track)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let scope = crate::event::EventScope::Card {
        card: report.clone(),
        track: p.track.clone().into(),
        area,
    };
    let actor = crate::ids::ActorId::AiPlannerSession(p.workers[0].clone().into());
    let edits = p
        .fixture
        .repo
        .events_for_track(&p.track, &["track.report_edited"], None)
        .await
        .unwrap();
    let edits: Vec<_> = edits.into_iter().filter(|edit| {
        edit.actor == actor && edit.scope == scope && matches!(&edit.event,
            crate::event::Event::TrackReportEdited { track_id, card_id, author: crate::event::EditAuthor::Planner, .. }
            if track_id.as_str() == p.track && card_id == &report)
    }).collect();
    assert_eq!(edits.len(), 1, "one edit from the old Planner");
    let edit = &edits[0];
    assert!(edit.id > 0);
    let completed = tokio::time::timeout(
        Duration::from_secs(5),
        crate::track_activity::report_completion_for_test::wait_for_report(&mut completions, edit),
    )
    .await;
    if !matches!(completed, Ok(Ok(_))) {
        p.close().await;
        panic!("original activity owner did not complete this edit: {completed:?}");
    }
    // This closes only this env, not all background writers. Keep the original CAS.
    p.switch().await.unwrap();
    assert_eq!(http::detail(&p).await["planner_revision"], 1);
    p.close().await;
}

#[tokio::test]
async fn planner_switch_preparation_waits_for_presentation_tail() {
    let tail = crate::test_seams::PausePoint::default();
    let held = PausePoint::default();
    let mut p = Pair::with_completion_hold("codex", Some(&tail), Some(&held)).await;
    let rows = p
        .fixture
        .repo
        .transcript_rows_of_thread(&p.cards[1], "switch-assistant")
        .await
        .unwrap();
    let reply = rows
        .iter()
        .find(|row| row.item_uuid.as_deref() == Some("original-native-reply"))
        .unwrap();
    let tail_committed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM events e JOIN conversation_presentations p
         ON json_extract(e.payload,'$.cursor.position')=p.sequence
         WHERE e.kind='harness.presentation.changed' AND e.scope_card=?1
           AND p.card_id=?1 AND p.native_item_row_id=?2)",
    )
    .bind(&p.cards[1])
    .bind(reply.id)
    .fetch_one(p.fixture.repo.pool())
    .await
    .unwrap();
    assert!(
        tail_committed,
        "ready must follow the original reply's event commit"
    );
    // Cutover still succeeds directly, without a retry that could hide Changed.
    p.switch().await.unwrap();
    assert_eq!(http::detail(&p).await["planner_revision"], 1);
    p.close().await;
}
