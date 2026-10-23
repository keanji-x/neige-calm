//! Revoke real execution authority at the persisted crossing, before native send.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;

pub(super) async fn hold(fixture: &Fixture) {
    sqlx::query("CREATE TRIGGER revoke_u7_crossing AFTER UPDATE OF handle_state_json ON worker_sessions
        WHEN json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts')>json_array_length(OLD.handle_state_json,'$.canonical_inputs.attempts')
        BEGIN DELETE FROM area_members; END")
        .execute(fixture.repo.pool()).await.unwrap();
}

pub(super) async fn release_twice(
    fixture: &Fixture,
    app: &axum::Router,
    cookie: &str,
    track: &str,
    body: &Value,
    mut peer: Option<&mut Peer>,
) {
    let (card, _, _) = fixture.carrier(track).await;
    let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    let frozen = &payload[crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY];
    let mut original_pending = None;
    let mut originals = Vec::new();
    if peer.is_some() {
        post(app, cookie, "/api/tracks", "first", body.clone()).await;
    }
    for round in 1..=2 {
        let settled = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let frame = fixture.carrier(track).await.2;
                if frame.header.phase == HarnessPhaseTag::Idle
                    && frame.canonical_inputs.attempts.len() == round
                    && frame.canonical_inputs.active.is_none()
                {
                    assert_eq!(frame.canonical_inputs.pending.len(), 1);
                    assert_eq!(original_pending.get_or_insert_with(|| frame.canonical_inputs.pending.clone()), &frame.canonical_inputs.pending);
                    assert!(frame.canonical_inputs.attempts.iter().all(|attempt| matches!(
                        attempt.delivery, calm_types::conversation::InputDeliveryFact::NotDispatched { .. }
                    )));
                    break;
                }
                if let Some(peer) = peer.as_deref_mut() {
                    if let Ok(call) = tokio::time::timeout(Duration::from_millis(10), request(peer)).await {
                        let result = match call["method"].as_str().unwrap() {
                            "config/read" => json!({"config":{}}),
                            "thread/start" | "thread/resume" => json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}}),
                            "thread/read" => json!({"thread":{"id":THREAD,"status":{"type":"notLoaded"},"turns":[]}}),
                            method => panic!("no actual prompt before release: {method}"),
                        };
                        send(peer, json!({"id":call["id"],"result":result})).await;
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }).await;
        assert!(
            settled.is_ok(),
            "round {round}: real settle/release: {:?}",
            fixture.carrier(track).await.2
        );
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id,render_plan_json,render_plan_sha256 FROM conversation_input_submissions WHERE card_id=?1 ORDER BY rowid")
            .bind(&card).fetch_all(fixture.repo.pool()).await.unwrap();
        assert_eq!(rows.len(), round, "immutable submissions survive release");
        assert_eq!(&rows[..originals.len()], originals.as_slice());
        let plan: calm_types::conversation::SubmissionRenderPlan =
            serde_json::from_str(&rows[round - 1].1).unwrap();
        assert!(
            plan.segments().iter().any(|segment| matches!(
                segment,
                calm_types::conversation::SubmissionRenderSegment::Context {
                    payload: calm_types::conversation::PrivateHistoryPayload::Present { value }
                } if serde_json::to_value(value).unwrap() == *frozen
            )),
            "every wholly NotDispatched claim retains frozen creation Context"
        );
        assert_eq!(
            plan.input_refs().unwrap().len(),
            1,
            "Context never becomes Human"
        );
        originals = rows;
        sqlx::query("INSERT INTO area_members(area_id,user_id,role,joined_at) SELECT ?1,id,'owner',0 FROM application_users")
            .bind(body["area_id"].as_str().unwrap()).execute(fixture.repo.pool()).await.unwrap();
        if round == 1 {
            post(app, cookie, "/api/tracks", "first", body.clone()).await;
        }
    }
    sqlx::query("DROP TRIGGER revoke_u7_crossing")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
}
