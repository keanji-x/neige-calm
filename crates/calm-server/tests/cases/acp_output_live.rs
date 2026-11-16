//! Real producer content through both ACP drivers and receipt restart.
use super::*;

async fn boot_output(root: &Root, resident: bool) -> Stack {
    if resident {
        boot_as(root, "antigravity", json!({"ACP_FIXTURE_AGENT_NAME":"Antigravity ACP Server","ACP_FIXTURE_VERSION":"1.5.0","ACP_FIXTURE_CAPTURE_SHARED_TOKEN":"1"}), "1.5.0").await
    } else {
        super::acp_grok_provider::boot_grok(root).await
    }
}
async fn card_output(stack: &Stack, resident: bool) -> String {
    stack
        .create_canonical_track_with(
            json!({"planner_provider":if resident {"antigravity"} else {"grok"}}),
        )
        .await
        .1
}
async fn output_rows(stack: &Stack, card: &str) -> Vec<Value> {
    claude_planner_session_fixture::card_rows(stack.repo(), card, "item/completed")
        .await
        .into_iter()
        .filter(|v| v["item"].get("outputSegment").is_some())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_live_replies_both_drivers_keep_unknown_content_without_known_receipt() {
    for resident in [false, true] {
        let root = Root::new("unused");
        let stack = boot_output(&root, resident).await;
        let card = card_output(&stack, resident).await;
        std::fs::write(root.path().join("scenario"), "rich-unknown").unwrap();
        let (status, body) = stack.post_input(&card, "retain rich output").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let outcomes = stack.wait_outcomes(&card, 1).await;
        assert_eq!(outcomes[0]["status"], "failed");
        let rows = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let rows = output_rows(&stack, &card).await;
                if rows.len() == 2 {
                    break rows;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("both complete producer segments must be observed");
        assert_eq!(rows.len(), 2, "resident={resident}: {rows:?}");
        assert_eq!(
            rows[0]["item"]["outputContent"]["blocks"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            rows[1]["item"]["outputContent"]["blocks"][0]["type"],
            "audio"
        );
        assert_eq!(rows[1]["_partial"], true);
        let (state, outcome): (String, Option<String>) = sqlx::query_as(
            "SELECT state,outcome_json FROM acp_submissions WHERE worker_session_id=?",
        )
        .bind(stack.runtime(&card).await.id)
        .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
        assert_eq!(state, "unknown");
        assert!(outcome.is_none());
        stack.shutdown().await;
        let stack = boot_output(&root, resident).await;
        assert_eq!(output_rows(&stack, &card).await, rows);
        assert_eq!(requests(&root, "session/prompt").len(), 1);
        stack.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_live_replies_native_snapshot_and_receipt_restart() {
    for resident in [false, true] {
        let root = Root::new("unused");
        let stack = boot_output(&root, resident).await;
        let card = card_output(&stack, resident).await;
        std::fs::write(root.path().join("scenario"), "rich-live").unwrap();
        let (status, body) = stack.post_input(&card, "typed live").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        wait_file(&root, "rich-streamed").await;
        let path = format!("/api/cards/{card}/harness/live?contract=conversation_v1");
        let live = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (status, value) = stack.send("GET", &path, None).await;
                assert_eq!(status, StatusCode::OK, "{value}");
                if value["turn"]["items"][0]["output"]["revision"] == "1"
                    && value["turn"]["items"][0]["output"]["content"]["blocks"][0]["type"]
                        == "audio"
                {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("native typed thought snapshot");
        assert_eq!(
            live["turn"]["items"][0]["output"]["content"]["blocks"][0]["type"],
            "audio"
        );
        assert_eq!(
            live["turn"]["items"][0]["output"]["segment"]["ordinal"],
            "2"
        );
        std::fs::write(root.path().join("release-rich"), "").unwrap();
        assert_eq!(
            stack.wait_outcomes(&card, 1).await[0]["status"],
            "completed"
        );
        let runtime = stack.runtime(&card).await;
        super::acp_compact::wait_phase(&stack, &runtime.id, "turn_completed").await;
        let rows = output_rows(&stack, &card).await;
        assert_eq!(rows.len(), 2);
        let receipt: String = sqlx::query_scalar(
            "SELECT outcome_json FROM acp_submissions WHERE worker_session_id=?",
        )
        .bind(&runtime.id)
        .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
        let receipt: Value = serde_json::from_str(&receipt).unwrap();
        for row in &rows {
            assert!(
                receipt["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v["params"] == *row)
            );
        }
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (_, retired) = stack.send("GET", &path, None).await;
                if retired["turn"].is_null()
                    || retired["turn"]["items"].as_array().unwrap().is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("queued completion must retire the native live item");
        stack.shutdown().await;
        let stack = boot_output(&root, resident).await;
        assert_eq!(output_rows(&stack, &card).await, rows);
        assert_eq!(requests(&root, "session/prompt").len(), 1);
        stack.shutdown().await;
    }
}
