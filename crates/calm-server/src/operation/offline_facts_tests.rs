use super::offline_facts::*;
use super::{Phase, TxOutput};
use crate::conversation_keys::first_message_digest;
use calm_types::conversation_receipt::{
    ConversationInitialInputReceiptResult as ReceiptResult, InitialInputUnavailableReason as Reason,
};
use serde_json::{Value, json};

const TEXT: &str = "  original 雪\n";

fn payload() -> Value {
    json!({"actor":calm_types::ids::ActorId::User,"wave_id":"track","spec_card_id":"card","cwd":"/unused",
        "profile":"assistant","create_card":{"idempotency_key":"key"},"first_message":TEXT})
}

fn output() -> TxOutput {
    serde_json::from_value(json!({"target_type":"card","target_id":"card","result":null,
        "data":{"card_id":"card","track_id":"track","runtime_id":"original-worker",
            "initial_input_receipt":{"version":"initial-input-v1","entryId":"entry","entryRev":7,
                "queuedAtMs":0,"initialRuntimeId":"original-worker","textSha256":first_message_digest(TEXT)}}})).unwrap()
}

fn read(
    payload: &Value,
    output: Option<&TxOutput>,
    phase: &Phase,
) -> Result<ReceiptResult, Reason> {
    historical_initial_input_receipt(
        Some(HistoricalInitialInput {
            payload,
            target_type: "card",
            target_id: Some("card"),
            phase,
            tx_output: output,
        }),
        "track",
        "card",
        "key",
    )
}

#[test]
fn u13_b2_p0_historical_wrapped_and_unwrapped_preserve_receipt_bytes() {
    let output = output();
    let expected = output.data["initial_input_receipt"].clone();
    for provider in std::iter::once(None).chain(
        calm_types::runtime::AgentProvider::ALL
            .into_iter()
            .map(Some),
    ) {
        let mut payload = payload();
        if let Some(provider) = &provider {
            payload["version"] = json!("assistant-v1");
            payload["provider"] = json!(provider.wire_name());
        }
        let receipt = read(&payload, Some(&output), &Phase::Succeeded).unwrap();
        assert_eq!(
            serde_json::to_value(receipt).unwrap(),
            json!({"kind":"known","receipt":expected})
        );
        if provider.is_some() {
            for field in ["version", "provider"] {
                let mut missing = payload.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert_eq!(
                    read(&missing, Some(&output), &Phase::Succeeded),
                    Err(Reason::Integrity)
                );
                let mut null = payload.clone();
                null[field] = Value::Null;
                assert_eq!(
                    read(&null, Some(&output), &Phase::Succeeded),
                    Err(Reason::Integrity)
                );
            }
        }
    }
}

#[test]
fn u13_b2_p0_historical_all_phases_keep_unavailability() {
    assert_eq!(
        historical_initial_input_receipt(None, "track", "card", "key"),
        Err(Reason::CreationIncomplete)
    );
    let payload = payload();
    let output = output();
    for (phase, detail) in [
        ("pending", None),
        ("tx_committed", None),
        (
            "app_server_interact",
            Some(json!({"kind":"mint_and_await","thread_id":null})),
        ),
        (
            "awaiting_retry",
            Some(json!({"kind":"register_pending","entry_id":null})),
        ),
        ("spawn_started", None),
        ("spawn_succeeded", None),
        ("parked", None),
        ("compensating", None),
        ("failed", None),
        ("stuck", Some(json!({"reason":"original","since":1}))),
    ] {
        let phase = Phase::deserialize_join(phase, detail.as_ref()).unwrap();
        assert_eq!(
            read(&payload, Some(&output), &phase),
            Err(Reason::CreationIncomplete)
        );
        assert_eq!(
            read(&json!({}), Some(&output), &phase),
            Err(Reason::Integrity)
        );
    }
    let phase = Phase::deserialize_join("succeeded", None).unwrap();
    assert!(matches!(
        read(&payload, Some(&output), &phase),
        Ok(ReceiptResult::Known { .. })
    ));
}

#[test]
fn u13_b2_p0_historical_payload_and_scope_integrity() {
    let output = output();
    for (field, value) in [
        ("profile", json!("planner")),
        ("wave_id", json!("other")),
        ("spec_card_id", json!("other")),
        ("create_card", json!(null)),
        ("create_card", json!({"idempotency_key":"other"})),
        ("create_card", json!({})),
        ("version", json!("future")),
        ("provider", json!("unknown")),
        ("first_message", json!(null)),
        ("first_message", json!("changed")),
    ] {
        let mut payload = payload();
        payload[field] = value;
        assert_eq!(
            read(&payload, Some(&output), &Phase::Succeeded),
            Err(Reason::Integrity),
            "{field}"
        );
    }
    for field in [
        "actor",
        "wave_id",
        "spec_card_id",
        "cwd",
        "profile",
        "create_card",
        "first_message",
    ] {
        let mut payload = payload();
        payload.as_object_mut().unwrap().remove(field);
        assert_eq!(
            read(&payload, Some(&output), &Phase::Succeeded),
            Err(Reason::Integrity),
            "{field}"
        );
    }
    for (target_type, target_id) in [
        ("track", Some("card")),
        ("card", None),
        ("card", Some("other")),
    ] {
        assert_eq!(
            historical_initial_input_receipt(
                Some(HistoricalInitialInput {
                    payload: &payload(),
                    target_type,
                    target_id,
                    phase: &Phase::Succeeded,
                    tx_output: Some(&output),
                }),
                "track",
                "card",
                "key"
            ),
            Err(Reason::Integrity)
        );
    }
}

#[test]
fn u13_b2_p0_historical_txoutput_missing_null_and_corruption() {
    let payload = payload();
    assert_eq!(
        read(&payload, None, &Phase::Succeeded),
        Err(Reason::Integrity)
    );
    let mut legacy = output();
    legacy
        .data
        .as_object_mut()
        .unwrap()
        .remove("initial_input_receipt");
    assert_eq!(
        read(&payload, Some(&legacy), &Phase::Succeeded),
        Ok(ReceiptResult::LegacyUnavailable)
    );
    for data in [Value::Null, json!({}), json!([])] {
        let mut bad = output();
        bad.data = data;
        assert_eq!(
            read(&payload, Some(&bad), &Phase::Succeeded),
            Err(Reason::Integrity)
        );
    }
    for (field, value) in [
        ("card_id", json!("other")),
        ("track_id", json!("other")),
        ("runtime_id", json!(null)),
        ("runtime_id", json!("other")),
        ("initial_input_receipt", json!(null)),
        ("initial_input_receipt", json!({})),
    ] {
        let mut bad = output();
        bad.data[field] = value;
        assert_eq!(
            read(&payload, Some(&bad), &Phase::Succeeded),
            Err(Reason::Integrity)
        );
    }
    for field in ["card_id", "track_id", "runtime_id"] {
        let mut bad = output();
        bad.data.as_object_mut().unwrap().remove(field);
        assert_eq!(
            read(&payload, Some(&bad), &Phase::Succeeded),
            Err(Reason::Integrity)
        );
    }
    for (field, value) in [
        ("version", json!("future")),
        ("entryId", json!("")),
        ("queuedAtMs", json!(-1)),
        ("initialRuntimeId", json!("")),
        ("textSha256", json!("other")),
        ("entryRev", json!(-1)),
        ("extra", json!(true)),
    ] {
        let mut bad = output();
        bad.data["initial_input_receipt"][field] = value;
        assert_eq!(
            read(&payload, Some(&bad), &Phase::Succeeded),
            Err(Reason::Integrity)
        );
    }
    for field in [
        "version",
        "entryId",
        "entryRev",
        "queuedAtMs",
        "initialRuntimeId",
        "textSha256",
    ] {
        let mut bad = output();
        bad.data["initial_input_receipt"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_eq!(
            read(&payload, Some(&bad), &Phase::Succeeded),
            Err(Reason::Integrity)
        );
    }
    for (target_type, target_id) in [
        ("track", Some("card")),
        ("card", None),
        ("card", Some("other")),
    ] {
        let mut bad = output();
        bad.target_type = target_type.into();
        bad.target_id = target_id.map(Into::into);
        assert_eq!(
            read(&payload, Some(&bad), &Phase::Succeeded),
            Err(Reason::Integrity)
        );
    }
}
