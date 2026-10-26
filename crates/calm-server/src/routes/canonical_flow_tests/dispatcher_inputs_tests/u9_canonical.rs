//! Real boot, authenticated input and MCP identity resolution; only the native peer is mocked.
use super::*;
use crate::mail::{MAX_HOP, hop_label};
use crate::mcp_server::{registry::ConnectionIdentity, transport::dispatch_tools_call};

pub(super) async fn tool(
    flow: &Flow,
    name: &str,
    arguments: Value,
) -> Result<Value, crate::mcp_server::framing::RpcError> {
    let result = dispatch_tools_call(
        &flow.state.mcp_context(),
        None,
        json!({"name":name,"arguments":arguments,"_meta":{"threadId":THREAD}}),
        &ConnectionIdentity::DaemonTrust,
        &crate::mcp_server::build_default_registry(),
    )
    .await?;
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
        .map_err(|e| crate::mcp_server::framing::RpcError::internal(e.to_string()))
}

pub(super) async fn incoming(flow: &Flow, from: &str, hop: i64) -> String {
    let id = crate::model::new_id();
    sqlx::query("INSERT INTO mails(id,from_track_id,to_track_id,summary,text,hop,sent_at) VALUES(?1,?2,?3,'incoming','body',?4,?5)")
        .bind(&id).bind(from).bind(&flow.track).bind(hop).bind(crate::model::now_ms())
        .execute(flow.fixture.repo.pool()).await.unwrap();
    id
}

#[cfg(test)]
#[tokio::test]
async fn u9_canonical_creator_real_turn_facts() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let mut body = super::super::u8_canonical::body(&json!({"id":flow.area}), "codex");
    body["admin"] = json!(true);
    let recipient = post(&flow.app, &flow.cookie, "/api/tracks", "recipient", body).await;
    let mail = incoming(&flow, recipient["id"].as_str().unwrap(), MAX_HOP).await;
    let cat = tool(&flow, "neige_mail_cat", json!({"mail_id":mail}))
        .await
        .unwrap();
    let mut tx = flow.fixture.repo.pool().begin().await.unwrap();
    let facts = crate::harness::turn_input::latest(&mut tx, &flow.card, &flow.worker)
        .await
        .unwrap();
    let time: i64 = sqlx::query_scalar(
        "SELECT t.created_at_ms FROM conversation_turns t \
         JOIN conversation_input_submissions s ON s.id=t.initial_submission_id \
         WHERE t.card_id=?1",
    )
    .bind(&flow.card)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    // Exercise ordinary mail::send via the real MCP router, before asserting the accessor.
    let sent = tool(
        &flow,
        "neige_mail_send",
        json!({"track_id":recipient["id"],"summary":"hello","text":"ordinary canonical mail"}),
    )
    .await;
    assert!(
        sent.is_ok() && cat["next_hop"] == hop_label(1) && cat["refused"] == false,
        "ordinary mail::send/cat: send={sent:?}; cat={cat:?}"
    );
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
    let sent = sent.unwrap();
    let (body, wake): (String,String) = sqlx::query_as("SELECT m.text,e.payload FROM mails m JOIN events e ON json_extract(e.payload,'$.key')=m.id \
            WHERE m.id=?1 AND e.kind='track.wake_requested'")
        .bind(sent["mail_id"].as_str().unwrap()).fetch_one(flow.fixture.repo.pool()).await.unwrap();
    assert_eq!(body, "ordinary canonical mail");
    let wake: Value = serde_json::from_str(&wake).unwrap();
    assert_eq!(
        wake["text"],
        crate::mail::wake_line(&flow.track, "", "hello", sent["mail_id"].as_str().unwrap())
    );
    assert_eq!(sent["hop"], hop_label(1));
    assert_eq!(cat["next_hop"], hop_label(1));
    assert_eq!(cat["refused"], false);
    assert_eq!(
        facts.unwrap(),
        crate::harness::turn_input::TurnInput {
            created_at_ms: time,
            has_user_segment: true
        }
    );
}

#[cfg(test)]
#[tokio::test]
async fn u9_canonical_turn_owner_system_hops_and_turn_read_isolation() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let mut body = super::super::u8_canonical::body(&json!({"id":flow.area}), "codex");
    body["admin"] = json!(true);
    let recipient = post(&flow.app, &flow.cookie, "/api/tracks", "recipient", body).await;
    let other = recipient["id"].as_str().unwrap();
    let old = incoming(&flow, other, MAX_HOP).await;
    tool(&flow, "neige_mail_cat", json!({"mail_id":old}))
        .await
        .unwrap();
    flow.report(SYSTEM_BODY).await;
    flow.end(TURN).await;
    let call = flow.next().await.unwrap();
    flow.accept(&call, "system-mail-turn").await;
    let (_, _, frame) = flow.fixture.carrier(&flow.track).await;
    let active = frame.canonical_inputs.active.unwrap();
    let mut tx = flow.fixture.repo.pool().begin().await.unwrap();
    let time: i64 = sqlx::query_scalar(
        "SELECT created_at_ms FROM conversation_turns WHERE id=?1 AND card_id=?2",
    )
    .bind(active.logical_turn_id.as_str())
    .bind(&flow.card)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        crate::harness::turn_input::latest(&mut tx, &flow.card, &flow.worker)
            .await
            .unwrap(),
        Some(crate::harness::turn_input::TurnInput {
            created_at_ms: time,
            has_user_segment: false
        })
    );
    tx.rollback().await.unwrap();
    // A released historical projection is a trap, never canonical Human evidence.
    flow.fixture
        .repo
        .fixture_transcript_insert(
            &flow.worker,
            &flow.card,
            &flow.track,
            THREAD,
            Some("old-projected-turn"),
            None,
            Some("userMessage"),
            "item/completed",
            "{}",
            Some(r#"[{"presentation":"user","text":"old Human projection"}]"#),
        )
        .await
        .unwrap();
    let first = tool(
        &flow,
        "neige_mail_send",
        json!({"track_id":other,"summary":"new turn","text":"old reads do not carry"}),
    )
    .await
    .unwrap();
    assert_eq!(first["hop"], hop_label(1));
    let mail = incoming(&flow, other, 3).await;
    let cat = tool(&flow, "neige_mail_cat", json!({"mail_id":mail}))
        .await
        .unwrap();
    assert_eq!(cat["next_hop"], hop_label(4));
    assert_eq!(cat["refused"], false);
    let stamp: i64 = sqlx::query_scalar("SELECT read_at FROM mails WHERE id=?1")
        .bind(&mail)
        .fetch_one(flow.fixture.repo.pool())
        .await
        .unwrap();
    tool(&flow, "neige_mail_cat", json!({"mail_id":mail}))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT read_at FROM mails WHERE id=?1")
            .bind(&mail)
            .fetch_one(flow.fixture.repo.pool())
            .await
            .unwrap(),
        stamp
    );
    let sent = tool(
        &flow,
        "neige_mail_send",
        json!({"track_id":other,"summary":"system","text":"hop four"}),
    )
    .await
    .unwrap();
    assert_eq!(sent["hop"], hop_label(4));
    let limit = incoming(&flow, other, MAX_HOP).await;
    let refused = tool(
        &flow,
        "neige_mail_send",
        json!({"mail_id":limit,"summary":"reply","text":"over the hop limit"}),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.data.unwrap()["refusal"], "hop_limit");
    let cat = tool(&flow, "neige_mail_cat", json!({"mail_id":limit}))
        .await
        .unwrap();
    assert_eq!(cat["next_hop"], Value::Null);
    assert_eq!(cat["refused"], true);
    let refused = tool(
        &flow,
        "neige_mail_send",
        json!({"track_id":other,"summary":"send","text":"over the hop limit"}),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.data.unwrap()["refusal"], "hop_limit");
    flow.report("Second real System turn").await;
    flow.end("system-mail-turn").await;
    let call = flow.next().await.unwrap();
    flow.accept(&call, "system-mail-turn-two").await;
    let sent = tool(
        &flow,
        "neige_mail_send",
        json!({"track_id":other,"summary":"isolated","text":"new turn"}),
    )
    .await
    .unwrap();
    assert_eq!(sent["hop"], hop_label(1));
    flow.end("system-mail-turn-two").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    // Same current session, but no active execution: missing-turn cat still stamps reads.
    let missing = incoming(&flow, other, 3).await;
    let cat = tool(&flow, "neige_mail_cat", json!({"mail_id":missing}))
        .await
        .unwrap();
    assert_eq!(cat["next_hop"], Value::Null);
    assert_eq!(cat["refused"], false);
    assert!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT read_at FROM mails WHERE id=?1")
            .bind(&missing)
            .fetch_one(flow.fixture.repo.pool())
            .await
            .unwrap()
            .is_some()
    );
    let refused = tool(
        &flow,
        "neige_mail_send",
        json!({"track_id":other,"summary":"missing","text":"no active turn"}),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.data.unwrap()["refusal"], "turn_unrecorded");
    flow.close().await;
}
