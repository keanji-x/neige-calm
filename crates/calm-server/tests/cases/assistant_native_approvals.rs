//! Real canonical Assistant input → native permission → persisted Hold → HTTP answer → wire.
use super::*;
use calm_server::event::{AskDelivery, Event, EventScope};
use calm_types::ids::ActorId;
use std::time::Duration;

async fn native_approval(provider: &str, option: usize) {
    native_with_flow(provider, option, NativeFlow::Answer).await;
}

enum NativeFlow {
    Answer,
    DuplicateRace,
    Gone,
    GoneRace,
    Restart,
    Repeat,
}

async fn native_with_flow(provider: &str, mut option: usize, flow: NativeFlow) {
    let root = Root::new("ask");
    let stack = match provider {
        "opencode" => {
            let stack = boot_acp(&root).await;
            std::fs::write(root.path().join("scenario"), "assistant-approval").unwrap();
            stack
        }
        "codex" => {
            let mut config = root.config(false);
            config.codex_bin = env!("CARGO_BIN_EXE_osc-probe-child").into();
            Stack::boot_config(&config).await
        }
        _ => Stack::boot(&root).await,
    };
    let (track, _) = stack.create_canonical_track_with(json!({})).await;
    let (status, row) = create(
        &stack,
        &track,
        "native-approval",
        json!({
            "provider":provider, "text":"fake-approval: command", "permission_mode":"ask"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap();
    let runtime = stack.runtime(card).await;
    let found = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let asks = stack
                .repo()
                .events_for_track(&track, &["ask.requested"], None)
                .await
                .unwrap();
            if let Some(ask) = asks.into_iter().find(|ask| {
                matches!(
                    ask.event,
                    Event::AskRequested {
                        delivery: AskDelivery::Hold,
                        ..
                    }
                )
            }) {
                break ask;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if found.is_err() {
        let wire = native_wire(&root, &stack, provider);
        stack.shutdown().await;
        eprintln!("{provider} original native refusal wire: {wire}");
        let reached_native = match provider {
            "codex" => wire.contains("decline"),
            "claude" => {
                wire.contains(r#""request_id":"perm-1""#) && wire.contains(r#""behavior":"deny""#)
            }
            _ => wire.contains("native-permission") && wire.contains("cancelled"),
        };
        assert!(
            reached_native,
            "fixture did not prove a real native permission request: {wire}"
        );
        panic!(
            "{provider}: real native request was refused instead of persisting an Assistant Hold"
        );
    }
    let ask = found.unwrap();
    assert_eq!(
        ask.actor,
        ActorId::AiManagedSession(WorkerSessionId(runtime.id.clone()))
    );
    assert!(matches!(&ask.scope, EventScope::Card {card: owner,..} if owner.as_str()==card));
    let holder = stack.state.harness.get(&runtime.id).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !holder.held_requests().contains(ask.id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let answer_path = format!("/api/tracks/{track}/asks/{}/answer", ask.id);
    if matches!(flow, NativeFlow::GoneRace) {
        let answer = stack.send(
            "POST",
            &answer_path,
            Some(json!({"answers":[{"option":0}]})),
        );
        let gone = async {
            holder
                .held_request_sender()
                .send(
                    calm_server::harness::held_requests::HeldRequestMessage::Gone {
                        request_key: calm_server::harness::held_requests::RequestKey(
                            "perm-1".into(),
                        ),
                    },
                )
                .unwrap();
        };
        let ((status, body), ()) = tokio::join!(answer, gone);
        assert!(
            matches!(status, StatusCode::NO_CONTENT | StatusCode::CONFLICT),
            "{status}: {body}"
        );
        stack.wait_outcomes(card, 1).await;
        let terminal = stack
            .repo()
            .events_for_track(&track, &["ask.answered", "ask.withdrawn"], None)
            .await
            .unwrap();
        assert_eq!(terminal.len(), 1, "{terminal:?}");
        let wire = native_wire(&root, &stack, provider);
        let replies: Vec<Value> = wire
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| {
                row["type"] == "control_response" && row["response"]["request_id"] == "perm-1"
            })
            .collect();
        assert_eq!(replies.len(), 1, "{wire}");
        if matches!(terminal[0].event, Event::AskWithdrawn { .. }) {
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(replies[0]["response"]["response"]["behavior"], "deny");
        }
        stack.shutdown().await;
        return;
    }
    if matches!(flow, NativeFlow::Gone) {
        // The fixture's reported control request id is also the Claude adapter request key.
        holder
            .held_request_sender()
            .send(
                calm_server::harness::held_requests::HeldRequestMessage::Gone {
                    request_key: calm_server::harness::held_requests::RequestKey("perm-1".into()),
                },
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while holder.held_requests().contains(ask.id)
                || stack
                    .repo()
                    .events_for_track(&track, &["ask.withdrawn"], None)
                    .await
                    .unwrap()
                    .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let (status, _) = stack
            .send(
                "POST",
                &answer_path,
                Some(json!({"answers":[{"option":0}]})),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        stack.wait_outcomes(card, 1).await;
        assert!(
            stack
                .repo()
                .events_for_track(&track, &["ask.answered"], None)
                .await
                .unwrap()
                .is_empty()
        );
        let wire = native_wire(&root, &stack, provider);
        assert!(wire.contains(r#""behavior":"deny""#), "{wire}");
        assert!(!wire.contains(r#""behavior":"allow""#), "{wire}");
        stack.shutdown().await;
        return;
    }
    if matches!(flow, NativeFlow::Restart) {
        stack.shutdown().await;
        let restarted = Stack::boot(&root).await;
        let (status, _) = restarted
            .send(
                "POST",
                &answer_path,
                Some(json!({"answers":[{"option":0}]})),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            restarted
                .repo()
                .events_for_track(&track, &["ask.answered"], None)
                .await
                .unwrap()
                .is_empty()
        );
        let wire = native_wire(&root, &restarted, provider);
        assert!(!wire.contains(r#""behavior":"allow""#), "{wire}");
        assert!(
            restarted
                .state
                .harness
                .get(&runtime.id)
                .is_none_or(|h| !h.held_requests().contains(ask.id))
        );
        restarted.shutdown().await;
        return;
    }
    reject_share_and_agent_answers(&root, &stack, provider, &track, ask.id, &holder).await;
    if matches!(flow, NativeFlow::DuplicateRace) {
        let (allow, deny) = tokio::join!(
            stack.send(
                "POST",
                &answer_path,
                Some(json!({"answers":[{"option":0}]}))
            ),
            stack.send(
                "POST",
                &answer_path,
                Some(json!({"answers":[{"option":1}]}))
            ),
        );
        assert!(
            matches!(
                (allow.0, deny.0),
                (StatusCode::NO_CONTENT, StatusCode::CONFLICT)
                    | (StatusCode::CONFLICT, StatusCode::NO_CONTENT)
            ),
            "{allow:?} {deny:?}"
        );
        option = if allow.0 == StatusCode::NO_CONTENT {
            0
        } else {
            1
        };
    } else {
        let (status, response) = stack
            .send(
                "POST",
                &answer_path,
                Some(json!({"answers":[{"option":option}]})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
    }
    let outcomes = stack.wait_outcomes(card, 1).await;
    assert_eq!(outcomes[0]["status"], "completed", "{outcomes:?}");
    let answered = stack
        .repo()
        .events_for_track(&track, &["ask.answered"], None)
        .await
        .unwrap();
    assert_eq!(answered.len(), 1);
    assert!(
        matches!(&answered[0].event, Event::AskAnswered { ask_id, answers, .. }
        if *ask_id == ask.id && answers == &vec![calm_server::event::AskAnswer::Option(option)])
    );
    let (duplicate, _) = stack
        .send(
            "POST",
            &format!("/api/tracks/{track}/asks/{}/answer", ask.id),
            Some(json!({"answers":[{"option":option}]})),
        )
        .await;
    assert_eq!(duplicate, StatusCode::CONFLICT);
    match provider {
        "claude" => {
            let wire = root.read_fake("stdin").unwrap();
            let replies: Vec<Value> = wire
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|row| {
                    row["type"] == "control_response" && row["response"]["request_id"] == "perm-1"
                })
                .collect();
            assert_eq!(replies.len(), 1, "{wire}");
            assert_eq!(
                replies[0]["response"]["response"]["behavior"],
                if option == 0 { "allow" } else { "deny" }
            );
        }
        "opencode" => {
            let wire: Value = serde_json::from_slice(
                &std::fs::read(root.path().join("permission-reply.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(wire["id"], "native-permission");
            assert_eq!(wire["result"]["outcome"]["outcome"], "selected", "{wire}");
            assert_eq!(
                wire["result"]["outcome"]["optionId"],
                if option == 0 { "yes" } else { "no" }
            );
        }
        _ => {
            let wire = std::fs::read_to_string(
                stack
                    .state
                    .shared_codex_appserver
                    .sock_path()
                    .with_extension("approval-answers"),
            )
            .unwrap();
            let replies: Vec<Value> = wire
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(replies.len(), 1, "{wire}");
            assert_eq!(replies[0]["id"], "approval-0001");
            assert_eq!(
                replies[0]["result"]["decision"],
                if option == 0 { "accept" } else { "decline" }
            );
        }
    }
    if matches!(flow, NativeFlow::Repeat) {
        wait_assistant_idle(&stack, card).await;
        let (status, response) = stack.post_input(card, "fake-approval: command").await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let second = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let asks = stack
                    .repo()
                    .events_for_track(&track, &["ask.requested"], None)
                    .await
                    .unwrap();
                if let Some(second) = asks.into_iter().find(|row| row.id != ask.id) {
                    break second;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        // Another thread starts before the original reply: completion must still go to A.
        let (status, other) = create(
            &stack,
            &track,
            "overlapping-native-approval",
            json!({"provider":"codex", "text":"fake-approval: command", "permission_mode":"ask"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{other}");
        let other_card = other["id"].as_str().unwrap();
        let other_ask = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let asks = stack
                    .repo()
                    .events_for_track(&track, &["ask.requested"], None)
                    .await
                    .unwrap();
                if let Some(row) = asks.into_iter().find(|row| {
                    row.scope
                        .card_id()
                        .is_some_and(|id| id.as_str() == other_card)
                }) {
                    break row.id;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let original_thread = stack.runtime(card).await.thread_id.unwrap();
        assert_ne!(
            stack.runtime(other_card).await.thread_id.as_deref(),
            Some(original_thread.as_str())
        );
        let (client, _notifications) = calm_server::codex_appserver::CodexAppServer::connect(
            &stack.state.shared_codex_appserver.sock_path(),
        )
        .await
        .unwrap();
        let resumed = client.thread_resume(&original_thread).await.unwrap();
        assert_eq!(resumed.thread_id(), Some(original_thread.as_str()));
        let (status, response) = stack
            .send(
                "POST",
                &format!("/api/tracks/{track}/asks/{}/answer", second.id),
                Some(json!({"answers":[{"option":2}]})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
        let outcomes = stack.wait_outcomes(card, 2).await;
        assert_eq!(outcomes[1]["status"], "completed", "{outcomes:?}");
        assert_ne!(outcomes[0]["id"], outcomes[1]["id"]);
        assert!(stack.outcomes(other_card).await.is_empty());
        let (status, response) = stack
            .send(
                "POST",
                &format!("/api/tracks/{track}/asks/{other_ask}/answer"),
                Some(json!({"answers":[{"option":0}]})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
        let other_outcomes = stack.wait_outcomes(other_card, 1).await;
        assert_eq!(
            other_outcomes[0]["status"], "completed",
            "{other_outcomes:?}"
        );
        let wire = native_wire(&root, &stack, provider);
        let replies: Vec<Value> = wire
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 3, "{wire}");
        assert_eq!(replies[1]["result"]["decision"], "decline");
    }
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_codex_native_approval_repeated_turn_completes() {
    native_with_flow("codex", 0, NativeFlow::Repeat).await;
}

fn native_wire(root: &Root, stack: &Stack, provider: &str) -> String {
    match provider {
        "claude" => root.read_fake("stdin").unwrap_or_default(),
        "opencode" => {
            std::fs::read_to_string(root.path().join("permission-reply.json")).unwrap_or_default()
        }
        _ => std::fs::read_to_string(
            stack
                .state
                .shared_codex_appserver
                .sock_path()
                .with_extension("approval-answers"),
        )
        .unwrap_or_default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_codex_native_approval() {
    native_approval("codex", 0).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_claude_native_approval() {
    native_approval("claude", 0).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_acp_native_approval() {
    native_approval("opencode", 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_codex_native_approval_deny() {
    native_approval("codex", 2).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_claude_native_approval_deny() {
    native_approval("claude", 1).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_acp_native_approval_deny() {
    native_approval("opencode", 1).await;
}

async fn reject_share_and_agent_answers(
    root: &Root,
    stack: &Stack,
    provider: &str,
    track: &str,
    ask: i64,
    holder: &calm_server::harness::PlannerHarness,
) {
    let path = format!("/api/tracks/{track}/asks/{ask}/answer");
    let (status, link) = stack
        .send(
            "POST",
            &format!("/api/tracks/{track}/share-links"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{link}");
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    use axum::body::Body;
    use tower::ServiceExt;
    let unlocked = stack
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api/share-links/{id}/unlock"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"token":link["token"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unlocked.status(), StatusCode::OK);
    let cookie = unlocked.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let before = native_wire(root, stack, provider);
    let answers_before = stack
        .repo()
        .events_for_track(track, &["ask.answered"], None)
        .await
        .unwrap()
        .len();
    let mut statuses = Vec::new();
    let Event::AskRequested { questions, .. } = &stack
        .repo()
        .events_for_track(track, &["ask.requested"], None)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == ask)
        .unwrap()
        .event
    else {
        unreachable!()
    };
    let count = questions[0].options.len();
    for option in 0..count {
        let (status, _) = stack
            .send_with(
                Request::post(format!("{path}?share_link={id}"))
                    .header("cookie", &cookie)
                    .header("x-calm-actor", "user"),
                Some(json!({"answers":[{"option":option}]})),
            )
            .await;
        statuses.push(status);
    }
    assert_eq!(statuses, vec![StatusCode::FORBIDDEN; count]);
    let (status, _) = stack
        .send_with(
            Request::post(&path).header("x-calm-actor", "ai:codex"),
            Some(json!({"answers":[{"option":0}]})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        stack
            .repo()
            .events_for_track(track, &["ask.answered"], None)
            .await
            .unwrap()
            .len(),
        answers_before
    );
    assert!(holder.held_requests().contains(ask));
    assert_eq!(native_wire(root, stack, provider), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_native_approval_mcp_uses_shared_wake_and_planner_capabilities() {
    let root = Root::new("unused");
    let stack = boot_acp(&root).await;
    std::fs::write(root.path().join("scenario"), "assistant-approval-mcp").unwrap();
    let (track, _) = stack.create_canonical_track_with(json!({})).await;
    let (status, row) = create(
        &stack,
        &track,
        "mcp-boundary",
        json!({"provider":"opencode", "text":"MCP boundary", "permission_mode":"ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    stack.wait_outcomes(row["id"].as_str().unwrap(), 1).await;
    let replies: Vec<Value> = serde_json::from_slice(
        &std::fs::read(root.path().join("assistant-approval-mcp-results.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(replies.len(), 4);
    for reply in replies {
        match reply["id"].as_i64().unwrap() {
            2 | 5 => assert_ne!(reply["result"]["isError"], true, "{reply}"),
            3 | 4 => assert_eq!(
                reply["result"]["structuredContent"]["code"], -32602,
                "{reply}"
            ),
            _ => panic!("unexpected reply {reply}"),
        }
    }
    let asks = stack
        .repo()
        .events_for_track(&track, &["ask.requested"], None)
        .await
        .unwrap();
    assert_eq!(asks.len(), 1);
    assert_eq!(
        asks[0].scope.card_id().unwrap().as_str(),
        row["id"].as_str().unwrap()
    );
    assert!(matches!(
        asks[0].actor,
        calm_server::ids::ActorId::AiManagedSession(_)
    ));
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_native_approval_concurrent_answers_reply_once() {
    native_with_flow("claude", 0, NativeFlow::DuplicateRace).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_native_approval_gone_withdraws_before_refusing_original_request() {
    native_with_flow("claude", 0, NativeFlow::Gone).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_native_approval_restart_orphan_never_replays_allow() {
    native_with_flow("claude", 0, NativeFlow::Restart).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_native_approval_answer_gone_race_has_one_terminal_and_wire_reply() {
    native_with_flow("claude", 0, NativeFlow::GoneRace).await;
}

/// Wake uses the same production answer/dispatch chain, while native Holds stay paused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_native_approval_wake_answer_runs_only_the_source_conversation() {
    let root = Root::new("unused");
    let stack = boot_acp(&root).await;
    std::fs::write(root.path().join("scenario"), "assistant-wake-mcp").unwrap();
    let (track, planner) = stack.create_canonical_track_with(json!({})).await;
    let planner_session = stack.runtime(&planner).await.id;
    let mut cards = Vec::new();
    for key in ["wake-first", "wake-second"] {
        let (status, row) = create(
            &stack,
            &track,
            key,
            json!({"provider":"opencode", "text":key, "permission_mode":"ask"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{row}");
        let card = row["id"].as_str().unwrap().to_owned();
        stack.wait_outcomes(&card, 1).await;
        wait_assistant_idle(&stack, &card).await;
        cards.push(card);
    }
    let asks = stack
        .repo()
        .events_for_track(&track, &["ask.requested"], None)
        .await
        .unwrap();
    assert_eq!(asks.len(), 2);
    let ask = asks
        .iter()
        .find(|ask| {
            ask.scope
                .card_id()
                .is_some_and(|id| id.as_str() == cards[0])
        })
        .unwrap();
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/tracks/{track}/asks/{}/answer", ask.id),
            Some(json!({"answers":[{"option":0}]})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    stack.wait_outcomes(&cards[0], 2).await;
    wait_assistant_idle(&stack, &cards[0]).await;
    assert_eq!(stack.runtime(&planner).await.id, planner_session);
    let second = stack.runtime(&cards[1]).await;
    let prompts = requests(&root, "session/prompt");
    assert_eq!(
        prompts
            .iter()
            .filter(|p| p["params"]["sessionId"] == second.session_id.as_deref().unwrap())
            .count(),
        1,
        "another conversation must not receive this answer: {prompts:?}"
    );
    assert!(
        prompts
            .iter()
            .any(|p| p["params"]["prompt"].to_string().contains("Run?")),
        "the source's new turn must receive its persisted question and answer: {prompts:?}"
    );
    stack.shutdown().await;
}
