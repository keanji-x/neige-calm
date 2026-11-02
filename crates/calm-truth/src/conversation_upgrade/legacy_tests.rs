use super::{classification_tests::*, raw_tests::Fixture, *};
use serde_json::{Value, json};

#[tokio::test]
async fn u13_p1_facts_parallel_independent_lengths_and_missing_slots() {
    for populated in [
        "pending_queue",
        "pending_envelope_ids",
        "pending_entry_meta",
        "pending_message_ids",
    ] {
        let mut v = original();
        v[populated] = match populated {
            "pending_queue" => {
                json!([{"type":"user_message","text":"original"},null,{"type":"future"}])
            }
            "pending_envelope_ids" => json!([9, null, "bad"]),
            "pending_entry_meta" => {
                json!([{"id":"q","rev":2,"queued_at_ms":3,"attachments":[]},null,5])
            }
            _ => json!([["m"], null, "bad"]),
        };
        let audit = frame_audit(&v).await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        for name in [
            "pending_queue",
            "pending_envelope_ids",
            "pending_entry_meta",
            "pending_message_ids",
        ] {
            let path = format!("/{name}");
            assert!(w.fields.iter().any(|f| at(f, &path)
                && f.interpretation
                    == Interpretation::ArrayLength(if name == populated { 3 } else { 0 })));
            assert!(blocked(w, &path, FactReason::UnequalLengths));
            for i in 0..3 {
                let path = format!("/{name}/{i}");
                if name != populated {
                    assert!(blocked(w, &path, FactReason::Missing));
                } else {
                    assert!(w.fields.iter().any(|f| at(f, &path)));
                }
            }
        }
        assert!(blocked(w, &format!("/{populated}/1"), FactReason::Null));
        assert!(blocked(
            w,
            &format!("/{populated}/2"),
            FactReason::Malformed
        ));
    }
    for replacement in [Value::Null, json!(false), json!({"future":true})] {
        let mut v = original();
        v["pending_queue"] = replacement.clone();
        let audit = frame_audit(&v).await;
        let facts = classify_facts(&audit).unwrap();
        assert!(blocked(
            &facts.workers()[0],
            "/pending_queue",
            if replacement.is_null() {
                FactReason::Null
            } else {
                FactReason::Malformed
            }
        ));
    }
}

#[tokio::test]
async fn u13_p1_facts_parallel_identity_meta_and_slot_presence() {
    let mut v = original();
    v["pending_queue"] = json!([{"type":"user_message","text":"one"},null]);
    v["pending_entry_meta"] = json!([
        {"id":"dup","rev":0,"queued_at_ms":1,"attachments":[]},
        {"id":"dup","rev":-1,"queued_at_ms":"bad","attachments":null},
        {"id":"","rev":0}]);
    v["pending_message_ids"] = json!([["m", "m"], ["m", "", 7]]);
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    let w = &facts.workers()[0];
    for path in [
        "/pending_entry_meta/0/id",
        "/pending_entry_meta/1/id",
        "/pending_message_ids/0/0",
        "/pending_message_ids/0/1",
        "/pending_message_ids/1/0",
    ] {
        assert!(blocked(w, path, FactReason::DuplicateIdentity), "{path}");
    }
    for path in [
        "/pending_entry_meta/2/id",
        "/pending_entry_meta/1/rev",
        "/pending_entry_meta/1/queued_at_ms",
        "/pending_message_ids/1/1",
        "/pending_message_ids/1/2",
    ] {
        assert!(blocked(w, path, FactReason::Malformed), "{path}");
    }
    for path in [
        "/pending_entry_meta/2/queued_at_ms",
        "/pending_entry_meta/2/attachments",
        "/pending_queue/2",
    ] {
        assert!(blocked(w, path, FactReason::Missing), "{path}");
    }
    assert!(blocked(
        w,
        "/pending_entry_meta/1/attachments",
        FactReason::Null
    ));
}

#[tokio::test]
async fn u13_p1_facts_provider_claim_does_not_supply_authority() {
    let claim = json!({"version":1,"execution_id":"execution","purpose":{"Turn":{"turn_id":"turn"}},"release":{"Releasing":{"authorized_at_ms":1}},"decision":{"Stopping":{"reason":"Finished","at_ms":1}},"future":{"idle":true}});
    let mut old = claim.clone();
    old["release"] = json!("Held");
    old["decision"] = json!("Observing");
    let mut cases = vec![(old, None), (claim.clone(), None)];
    for path in [
        "/version",
        "/execution_id",
        "/purpose",
        "/release",
        "/decision",
        "/purpose/Turn/turn_id",
        "/release/Releasing/authorized_at_ms",
        "/decision/Stopping/reason",
        "/decision/Stopping/at_ms",
    ] {
        for (bad, reason) in [
            (None, FactReason::Missing),
            (Some(Value::Null), FactReason::Null),
            (Some(json!(false)), FactReason::Malformed),
        ] {
            let mut v = claim.clone();
            let (parent, field) = path.rsplit_once('/').unwrap();
            let object = v.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            if let Some(bad) = bad {
                object.insert(field.into(), bad);
            } else {
                object.remove(field);
            }
            cases.push((v, Some((path.to_owned(), reason))));
        }
    }
    for (field, value) in [
        ("purpose", json!("RewindCheck")),
        ("purpose", json!({"Compaction":{"turn_id":"t"}})),
        ("release", json!("NotRequired")),
        ("release", json!("Held")),
        ("release", json!({"Released":{"authorized_at_ms":1}})),
        ("decision", json!("Observing")),
    ] {
        let mut v = claim.clone();
        v[field] = value;
        cases.push((v, None));
    }
    for (claim, expected) in cases {
        let fixture = Fixture::new(&format!(
            "{TABLES}{} UPDATE worker_sessions SET provider_execution_json={};",
            worker_row("w", "running", &original().to_string()),
            quoted(&claim.to_string())
        ))
        .await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        assert!(blocked(w, "/future/idle", FactReason::Unclassified));
        assert!(
            w.fields.iter().any(|f| f.interpretation
                == Interpretation::Blocked(FactReason::MissingProviderAuthority))
        );
        assert!(matches!(w.frame, Some(FrameFacts::LegacyV1)));
        if let Some((path, reason)) = expected {
            assert!(blocked(w, &path, reason.clone()), "{path}: {reason:?}");
        } else {
            assert!(w.fields.iter().any(|f| at(f, "") && f.interpretation == Interpretation::Stored(claim.clone())));
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_nested_defaults_are_not_source_presence() {
    let mut v = original();
    v["turn_budget"] =
        json!({"kind":"planner","budget":15,"start_attempt":"attempt","warning_attempted":false});
    v["issued_input_segments"] =
        json!({"turn_id":"t","segments":[{"presentation":"system_report_edited","text":"old"}]});
    v["token_usage"] = json!({"used_tokens":1,"total_tokens":2,"at_ms":3});
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    for path in [
        "/turn_budget/context_usage",
        "/issued_input_segments/segments/0/attachments",
        "/issued_input_segments/segments/0/origin",
        "/token_usage/context_window",
    ] {
        assert!(
            blocked(&facts.workers()[0], path, FactReason::Missing),
            "{path}"
        );
    }
}
