use super::*;

#[tokio::test]
async fn g3_grok_assistant_receipt_failure_rolls_back_everything() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let planner = source(&fixture, &track).await;
    for side in [false, true] {
        let key = if side {
            "side-rollback"
        } else {
            "normal-rollback"
        };
        let card = crate::conversation_keys::derive_track_conversation_keys(&track, key).card_id;
        let mut body = json!({"provider":"grok","text":FIRST_BODY});
        if side {
            body["side"] = json!({"source_card_id":planner,"context":"Frozen rollback excerpt"});
        }
        assert_eq!(
            status(&app, "", &uri, key, body.clone()).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(facts(&fixture, &card).await, (0, 0, 0, 0, 0, 0));
        let mut invalid = body.clone();
        invalid["text"] = json!("");
        assert_eq!(
            status(&app, &cookie, &uri, key, invalid).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(facts(&fixture, &card).await, (0, 0, 0, 0, 0, 0));
        sqlx::query(
            "CREATE TRIGGER reject_g3_receipt BEFORE INSERT ON conversation_assistant_creations
            BEGIN SELECT RAISE(ABORT,'G3 receipt rejection'); END",
        )
        .execute(fixture.repo.pool())
        .await
        .unwrap();
        let before = table_counts(&fixture).await;
        let rejected = status(&app, &cookie, &uri, key, body.clone()).await;
        // Drop before asserting so a regression cannot leave a fixture trigger behind.
        sqlx::query("DROP TRIGGER reject_g3_receipt")
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        assert_eq!(rejected, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            table_counts(&fixture).await,
            before,
            "rollback must leave no orphan rows"
        );
        assert_eq!(facts(&fixture, &card).await, (0, 0, 0, 0, 0, 0));
        assert_eq!(fixture.counts(&card).await, (0, 0, 0, 0, 0));
        assert_eq!(prompts(&fixture, &AgentProvider::Grok), 0);
        let created = post(&app, &cookie, &uri, key, body).await;
        assert_eq!(created["id"], card);
        assert_eq!(facts(&fixture, &card).await, (1, 1, 1, 1, 1, 1));
    }
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn g3_grok_assistant_side_rejects_foreign_and_invalid_source() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let parent = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{parent}/conversations");
    let card = crate::conversation_keys::derive_track_conversation_keys(&parent, "foreign").card_id;
    // Another real track, allocated under a different key in the same authenticated area.
    let area: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
        .bind(&parent)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    let foreign = post(&app,&cookie,"/api/tracks","foreign-track",json!({"area_id":area,"title":"Other","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]}})).await;
    let foreign = source(&fixture, foreign["id"].as_str().unwrap()).await;
    for src in [foreign, "missing-card".into()] {
        let result = status(&app,&cookie,&uri,"foreign",json!({"provider":"grok","text":FIRST_BODY,"side":{"source_card_id":src,"context":"untrusted excerpt"}})).await;
        assert!(matches!(
            result,
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
        ));
        assert_eq!(facts(&fixture, &card).await, (0, 0, 0, 0, 0, 0));
    }
    // Ensure this case observes the capability too, rather than passing through legacy rejection.
    let created = post(&app,&cookie,&uri,"valid",json!({"provider":"grok","text":FIRST_BODY,"side":{"source_card_id":source(&fixture,&parent).await,"context":"valid excerpt"}})).await;
    assert_eq!(
        facts(&fixture, created["id"].as_str().unwrap()).await,
        (1, 1, 1, 1, 1, 1)
    );
    let branched = post(&app, &cookie, &uri, "assistant-source",
        json!({"provider":"grok","text":FIRST_BODY,"side":{"source_card_id":created["id"],"context":"Assistant excerpt"}})).await;
    assert_eq!(
        facts(&fixture, branched["id"].as_str().unwrap()).await,
        (1, 1, 1, 1, 1, 1)
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn g3_grok_assistant_stop_waits_for_original_end() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    std::fs::write(
        peer_root(&fixture, &AgentProvider::Grok).join("scenario"),
        "hold",
    )
    .unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "stop",
        json!({"provider":"grok","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    assert_eq!(facts(&fixture, card).await, (1, 1, 1, 1, 1, 1));
    let worker = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (worker, frame) = carrier(&fixture, card).await;
            if frame.header.phase == HarnessPhaseTag::TurnRunning {
                break worker;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let fenced = crate::acp_planner::test_seams::pause_after_fence(&worker);
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/interrupt"),
        "stop",
        json!({}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), fenced.entered.notified())
        .await
        .unwrap();
    // The native cancelled reply was read, but Engine cleanup has not certified END.
    let before_end = carrier(&fixture, card).await.1;
    assert!(!before_end.idle_for_lifecycle());
    assert!(before_end.canonical_inputs.active.is_some());
    let journal: (String, Option<String>) =
        sqlx::query_as("SELECT state,outcome_json FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(journal, ("sending".into(), None));
    fenced.release.notify_one();
    let done = idle(&fixture, card, 1).await;
    assert!(done.canonical_inputs.active.is_none());
    let ends = fixture
        .transcript_count(card, Some(&worker), Some("turn/completed"), None)
        .await;
    assert_eq!(ends, 1, "only the original driver certifies one END");
    assert_eq!(carrier(&fixture, card).await.0, worker);
    assert_eq!(
        calls(&fixture)
            .iter()
            .filter(|c| c["method"] == "session/cancel")
            .count(),
        1
    );
    let outcome: String =
        sqlx::query_scalar("SELECT outcome_json FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&outcome).unwrap()["status"],
        "interrupted"
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn g3_grok_assistant_side_tools_never_and_decline_reason() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let root = peer_root(&fixture, &AgentProvider::Grok);
    std::fs::write(root.join("scenario"), "grok-assistant").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let created = post(&app,&cookie,&format!("/api/tracks/{track}/conversations"),"tools",json!({"provider":"grok","text":FIRST_BODY,"side":{"source_card_id":source(&fixture,&track).await,"context":"Frozen tool excerpt"}})).await;
    let card = created["id"].as_str().unwrap();
    assert_eq!(facts(&fixture, card).await, (1, 1, 1, 1, 1, 1));
    idle(&fixture, card, 1).await;
    let replies: Value =
        serde_json::from_slice(&std::fs::read(root.join("grok-assistant-results.json")).unwrap())
            .unwrap();
    let replies: std::collections::BTreeMap<_, _> = replies
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["tool"].as_str().unwrap(), &entry["reply"]))
        .collect();
    for tool in ["neige_report_read", "neige_report_commit"] {
        let allowed = replies[tool];
        assert!(
            allowed.get("error").is_none() && allowed["result"].is_object(),
            "{tool}: {allowed}"
        );
        assert_ne!(allowed["result"]["isError"], true, "{tool}: {allowed}");
    }
    for tool in [
        "neige_task_accept",
        "neige_terminal_open",
        "neige_track_close",
    ] {
        let denied = replies[tool];
        assert!(
            denied["error"].is_object() || denied["result"]["isError"] == true,
            "{tool}: {denied}"
        );
    }
    let permissions: std::collections::BTreeMap<String, Value> =
        std::fs::read_to_string(root.join("permission-log.jsonl"))
            .unwrap()
            .lines()
            .map(|line| {
                let entry: Value = serde_json::from_str(line).unwrap();
                (
                    entry["tool"].as_str().unwrap().to_owned(),
                    entry["outcome"].clone(),
                )
            })
            .collect();
    for tool in ["neige_report_read", "neige_report_commit"] {
        assert_eq!(
            permissions[tool],
            json!({"outcome":"selected","optionId":"allow-once"})
        );
    }
    assert_eq!(
        permissions["run_terminal_command"],
        json!({"outcome":"cancelled"})
    );
    let worker = carrier(&fixture, card).await.0;
    let outcome: String =
        sqlx::query_scalar("SELECT outcome_json FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let outcome: Value = serde_json::from_str(&outcome).unwrap();
    assert_eq!(outcome["status"], "failed");
    let reason = outcome["error"]["message"]
        .as_str()
        .expect("structured decline reason");
    assert!(
        reason.contains(
            "Neige declined `Execute `echo one > one.txt`` under the `never` permission mode"
        ),
        "{reason}"
    );
    let argv: Vec<Value> = std::fs::read_to_string(root.join("argv.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        argv.iter()
            .all(|v| !v.to_string().contains("--always-approve"))
    );
    fixture.shutdown_process(&state).await;
}

// The acceptance transaction's concrete persisted facts, including orphan detection.
async fn table_counts(fixture: &Fixture) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in [
        "cards",
        "worker_sessions",
        "conversation_messages",
        "conversation_message_revisions",
        "conversation_presentations",
        "conversation_assistant_creations",
        "conversation_input_submissions",
        "conversation_turns",
        "acp_submissions",
    ] {
        counts.push(
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap(),
        );
    }
    counts
}
