use super::{classification_tests::*, *};
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;
use serde_json::{Value, json};

fn released(version: u64) -> Value {
    let mut v = serde_json::to_value(CanonicalHarnessSnapshot::dormant(7)).unwrap();
    v["schema_version"] = json!(version);
    v["canonical_inputs"]["attempts"] = json!([{
        "attempt":{"submission_id":"11111111111111111111111111111111","attempt_id":"22222222222222222222222222222222"},
        "correlation":{"provider":"codex","thread_id":"thread","native_session_id":null,"local_turn_id":null,"native_turn_id":null,"rpc_request_id":null},
        "delivery":{"kind":"unknown"}
    }]);
    if version >= 3 {
        v["canonical_inputs"]["attempts"][0]["revision"] = json!(4);
        v["canonical_inputs"]["attempts"][0]["invocation"] =
            json!({"kind":"prior_invocation_not_recorded"});
    }
    if version <= 3 {
        v["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("receipts");
    }
    v
}
#[tokio::test]
async fn u13_p1_facts_released_owner_adaptation_and_stored_presence() {
    for version in 2..=5 {
        let mut v = released(version);
        v.as_object_mut().unwrap().remove("last_turn_id");
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        let Some(FrameFacts::DerivedByReleasedAdapter {
            original_version,
            adapted,
            ..
        }) = &w.frame
        else {
            panic!("V{version}");
        };
        assert_eq!(*original_version, version);
        assert_eq!(
            adapted.as_ref(),
            &CanonicalHarnessSnapshot::from_released(v.clone()).unwrap()
        );
        assert!(blocked(w, "/last_turn_id", FactReason::Missing));
        assert!(blocked(w, "/last_thread_id", FactReason::Null));
        let adapted = serde_json::to_value(adapted).unwrap();
        assert_eq!(
            adapted["canonical_inputs"]["attempts"][0]["revision"],
            json!(if version == 2 { 0 } else { 4 })
        );
        if version == 2 {
            assert!(
                !w.fields
                    .iter()
                    .any(|f| at(f, "/canonical_inputs/attempts/0/revision"))
            );
        }
        assert!(blocked(
            w,
            "/canonical_inputs",
            FactReason::RelationshipRequiresP2
        ));
    }
}
#[tokio::test]
async fn u13_p1_facts_released_malformed_future_and_nested_unknown() {
    for version in 2..=5 {
        let mut v = released(version);
        v["canonical_inputs"]["attempts"][0]["delivery"]["future"] = json!({"authority":true});
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        assert!(blocked(
            &facts.workers()[0],
            "/canonical_inputs/attempts/0/delivery/future/authority",
            FactReason::Unclassified
        ));
        for field in ["mode", "turn_budget", "canonical_inputs"] {
            let mut bad = released(version);
            bad.as_object_mut().unwrap().remove(field);
            let fixture = super::classification_tests::fixture(&bad.to_string()).await;
            let audit = fixture.audit().await;
            let facts = classify_facts(&audit).unwrap();
            assert!(facts.workers()[0].frame.is_none(), "V{version} {field}");
            assert!(
                facts.workers()[0]
                    .fields
                    .iter()
                    .any(|f| f.interpretation == Interpretation::Blocked(FactReason::Malformed))
            );
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_v4_keep_derived_paths_and_nested_options() {
    let mut v = released(4);
    let request = json!({"carrier_id":"carrier","submission_id":"11111111111111111111111111111111","attempt_id":"22222222222222222222222222222222","revision":"3"});
    let result = json!({"carrier_id":"carrier","submission_id":request["submission_id"],"attempt_id":request["attempt_id"],"revision":"4","waiting_for_settlement":true});
    v["canonical_inputs"]["receipts"] = json!([{"key":"keep","principal":"human","fingerprint":"a".repeat(64),"request":request,"result":result}]);
    v["canonical_inputs"]["attempts"][0]["correlation"]
        .as_object_mut()
        .unwrap()
        .remove("native_session_id");
    let fixture = fixture(&v.to_string()).await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    let w = &facts.workers()[0];
    assert!(blocked(
        w,
        "/canonical_inputs/attempts/0/correlation/native_session_id",
        FactReason::Missing
    ));
    let Some(FrameFacts::DerivedByReleasedAdapter {
        adapted, derived, ..
    }) = &w.frame
    else {
        panic!("V4 Keep")
    };
    let wire = serde_json::to_value(adapted).unwrap();
    assert_eq!(
        wire["canonical_inputs"]["receipts"][0]["decision"],
        json!({"kind":"keep","request":request,"result":result})
    );
    assert_eq!(
        wire["canonical_inputs"]["attempts"][0]["revision"],
        v["canonical_inputs"]["attempts"][0]["revision"]
    );
    let tag = derived
        .iter()
        .find(|d| d.target_path == "/canonical_inputs/receipts/0/decision/kind")
        .unwrap();
    assert_eq!(tag.value, json!("keep"));
    assert!(
        matches!(&tag.ancestor.location, SourceLocation::Cell { json: Some(a), .. } if a.pointer() == "/canonical_inputs/receipts/0")
    );
    assert!(
        facts
            .sources()
            .nodes()
            .iter()
            .any(|n| n.source == tag.ancestor)
    );
}
