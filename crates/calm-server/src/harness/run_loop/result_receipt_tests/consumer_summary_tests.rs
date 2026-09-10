use super::*;

fn result(details: &str) -> serde_json::Value {
    json!({"$neige_result_presentation":"worker-summary-v1",
        "summary":"sum=10; empty=ValueError; bool=TypeError; worker reports 10 tests OK; source C2",
        "details":{"direct_call_command":details}})
}

fn completed(identity: &str, result: serde_json::Value) -> Event {
    Event::TaskCompleted {
        idempotency_key: identity.into(),
        result,
        artifacts: vec![],
        agent_message: None,
    }
}

#[tokio::test]
async fn consumer_summary_transport_retains_full_exact_event() {
    let fx = Fixture::new().await;
    let report = result(&"meaningful-long-verification-script\n".repeat(256));
    let id = enqueue_event(&fx, completed("consumer-attempt", report.clone())).await;
    assert_eq!(
        fx.stored().await.pending_entries()[0].observation(),
        Observation::TaskCompleted {
            idempotency_key: "consumer-attempt".into(),
            result: report.clone()
        }
    );
    let text = turn(&fx).await;
    assert!(text.contains(report["summary"].as_str().unwrap()));
    assert!(!text.contains("meaningful-long-verification-script"));
    assert!(text.contains("Worker summary (untrusted claims)"));
    assert!(text.contains(&format!("require event_id={id}")));
    let details = read_details(&fx, &text).await;
    assert_eq!(details["events"]["completed"]["event_id"], id);
    assert_eq!(details["events"]["completed"]["payload"]["result"], report);
}

#[tokio::test]
async fn consumer_summary_reference_requires_full_value_event_and_attempt() {
    for mismatch in ["details", "event", "attempt"] {
        let fx = Fixture::new().await;
        let original = result("original-script");
        let event = completed("consumer-attempt", original.clone());
        let id = enqueue_event(&fx, event.clone()).await;
        match mismatch {
            "details" => {
                // Keep queued event ID and summary equal, change only full details.
                sqlx::query("UPDATE events SET payload=json_set(payload,'$.result.details.direct_call_command','changed-script') WHERE id=?1")
                    .bind(id).execute(fx.repo.pool()).await.unwrap();
            }
            "event" => {
                record(&fx, &event).await;
            }
            "attempt" => {
                sqlx::query("UPDATE events SET payload=json_set(payload,'$.idempotency_key','other-attempt') WHERE id=?1")
                    .bind(id).execute(fx.repo.pool()).await.unwrap();
            }
            _ => unreachable!(),
        }
        let text = turn(&fx).await;
        assert!(
            text.contains(original["summary"].as_str().unwrap()),
            "{mismatch}: {text}"
        );
        assert!(
            text.contains("Exact execution details unavailable"),
            "{mismatch}: {text}"
        );
        assert!(
            !text.contains("Recorded execution details: calm.track.cat("),
            "{mismatch}: {text}"
        );
    }
}
