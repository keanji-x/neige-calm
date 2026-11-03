use super::{classification_tests::*, raw_tests::Fixture, *};
use calm_types::{conversation::*, harness::snapshot::*};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn frame(version: u64) -> Value {
    let mut v = serde_json::to_value(CanonicalHarnessSnapshot::dormant(7)).unwrap();
    v["schema_version"] = json!(version);
    v["canonical_inputs"]["next_queue_position"] = json!(8);
    v["canonical_inputs"]["pending"] = json!([
        {"kind":"human","message":{"message_id":"3".repeat(32),"revision":2},"queue_position":2,"queued_at_ms":9,"envelope_id":null},
        {"kind":"system","observation_id":"4".repeat(32),"observation":{"type":"system_context","text":"context"},"queue_position":7,"queued_at_ms":10,"envelope_id":null,"addressable":true}
    ]);
    v["canonical_inputs"]["active"] =
        json!({"logical_turn_id":"5".repeat(32),"submission_ids":["1".repeat(32)]});
    v["canonical_inputs"]["attempts"] = json!([{
        "attempt":{"submission_id":"1".repeat(32),"attempt_id":"2".repeat(32)},
        "correlation":{"provider":"codex","thread_id":"thread","native_session_id":null,"local_turn_id":null,"native_turn_id":null,"rpc_request_id":null},
        "delivery":{"kind":"unknown"}
    }]);
    if version >= 3 {
        v["canonical_inputs"]["attempts"][0]["revision"] = json!(4);
        v["canonical_inputs"]["attempts"][0]["invocation"] = json!({"kind":"recorded","configuration":{"model":null,"effort":null,"permission":"never"}});
    }
    if version <= 3 {
        v["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("receipts");
    } else {
        let request = json!({"carrier_id":"carrier","submission_id":"1".repeat(32),"attempt_id":"2".repeat(32),"revision":"3"});
        let result = json!({"carrier_id":"carrier","submission_id":"1".repeat(32),"attempt_id":"2".repeat(32),"revision":"4","waiting_for_settlement":true});
        let receipt = if version == 4 {
            json!({"key":"keep","principal":"human","fingerprint":"a".repeat(64),"request":request,"result":result})
        } else {
            json!({"key":"keep","principal":"human","fingerprint":"a".repeat(64),"decision":{"kind":"keep","request":request,"result":result}})
        };
        v["canonical_inputs"]["receipts"] = json!([receipt]);
    }
    assert!(CanonicalHarnessSnapshot::from_released(v.clone()).is_ok());
    v
}
fn sequence(v: &Value, fields: &str) -> Value {
    Value::Array(
        fields
            .split_whitespace()
            .map(|name| v[name].clone())
            .collect(),
    )
}
fn probe<T: DeserializeOwned + Serialize>(name: &str, v: Value, expected: [bool; 3]) {
    let owned = serde_json::from_value::<T>(v.clone());
    let borrowed = T::deserialize(&v);
    let text = serde_json::from_str::<T>(&v.to_string());
    let actual = [owned.is_ok(), borrowed.is_ok(), text.is_ok()];
    println!("publicprobe {name}: owned/borrowed/text={actual:?}");
    assert_eq!(actual, expected, "{name}");
}
#[test]
fn u13_p1_released_publicprobe_actual_entry_representations() {
    let v = frame(5);
    probe::<ConversationInputCheckpoint>(
        "checkpoint sequence",
        sequence(
            &v["canonical_inputs"],
            "next_queue_position pending active attempts receipts",
        ),
        [true; 3],
    );
    probe::<InputAttemptCheckpoint>(
        "attempt sequence",
        sequence(
            &v["canonical_inputs"]["attempts"][0],
            "revision invocation attempt correlation delivery",
        ),
        [true; 3],
    );
    probe::<FrozenInputInvocation>(
        "required nullable sequence",
        json!([null, null, "never"]),
        [true; 3],
    );
    probe::<InputInvocation>(
        "tagged sequence",
        json!(["prior_invocation_not_recorded"]),
        [true; 3],
    );
    probe::<CanonicalHarnessSnapshot>(
        "flatten root sequence",
        json!([5, "harness", v["canonical_inputs"], {}]),
        [false; 3],
    );
    probe::<InputInvocation>("unknown variant", json!({"kind":"future"}), [false; 3]);
    probe::<FrozenInputInvocation>(
        "missing model",
        json!({"effort":null,"permission":"never"}),
        [false; 3],
    );
    probe::<InputInvocation>(
        "recorded sequence",
        json!(["recorded", [null, null, "never"]]),
        [true; 3],
    );
    probe::<InputDeliveryFact>(
        "accepted sequence",
        json!(["accepted","receipt",{"kind":"submission","submission_id":"1".repeat(32)}]),
        [true; 3],
    );
    probe::<PendingInput>(
        "human sequence",
        json!(["human",{"message_id":"3".repeat(32),"revision":2},2,9,null]),
        [true; 3],
    );
    probe::<NativeInputAcceptance>(
        "acceptance sequence",
        json!(["native_turn", "native"]),
        [true; 3],
    );
    probe::<SystemObservation>(
        "system observation sequence",
        json!(["system_context", "context"]),
        [true; 3],
    );
    probe::<PendingInput>(
        "system sequence",
        json!(["system","4".repeat(32),{"type":"system_context","text":"context"},7,10,null,true]),
        [true; 3],
    );
    probe::<HumanInputResolutionDecision>(
        "keep sequence",
        json!(["keep",{"carrier_id":"carrier","submission_id":"1".repeat(32),"attempt_id":"2".repeat(32),"revision":"3"},{"carrier_id":"carrier","submission_id":"1".repeat(32),"attempt_id":"2".repeat(32),"revision":"4","waiting_for_settlement":true}]),
        [true; 3],
    );
    for version in 2..=5 {
        let mut v = frame(version);
        let c = &v["canonical_inputs"];
        v["canonical_inputs"] = sequence(
            c,
            if version >= 4 {
                "next_queue_position pending active attempts receipts"
            } else {
                "next_queue_position pending active attempts"
            },
        );
        assert!(
            CanonicalHarnessSnapshot::from_released(v).is_ok(),
            "actual adapter V{version} checkpoint sequence"
        );
        let mut v = frame(version);
        v["canonical_inputs"]["attempts"][0] = sequence(
            &v["canonical_inputs"]["attempts"][0],
            if version == 2 {
                "attempt correlation delivery"
            } else {
                "revision invocation attempt correlation delivery"
            },
        );
        assert!(
            CanonicalHarnessSnapshot::from_released(v).is_ok(),
            "actual adapter V{version} attempt sequence"
        );
        if version >= 4 {
            let mut v = frame(version);
            v["canonical_inputs"]["receipts"][0] = sequence(
                &v["canonical_inputs"]["receipts"][0],
                if version == 4 {
                    "key principal fingerprint request result"
                } else {
                    "key principal fingerprint decision"
                },
            );
            assert!(
                CanonicalHarnessSnapshot::from_released(v).is_ok(),
                "actual adapter V{version} receipt sequence"
            );
        }
    }
}
#[tokio::test]
async fn u13_p1_released_presence_nullable_default_and_source_bytes() {
    for version in 2..=5 {
        let mut v = frame(version);
        v.as_object_mut().unwrap().remove("last_turn_id");
        v["pending_rewind"] = json!({"provider":"codex","before_turn_id":"turn"});
        v["canonical_inputs"]["attempts"][0]["correlation"]
            .as_object_mut()
            .unwrap()
            .remove("native_session_id");
        let raw = format!(" \n{} \t", v);
        let fixture = fixture(&raw).await;
        let bytes = std::fs::read(&fixture.path).unwrap();
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        assert!(
            matches!(w.frame, Some(FrameFacts::DerivedByReleasedAdapter { original_version, .. }) if original_version == version)
        );
        for (path, required, presence) in [
            ("/last_turn_id", false, Presence::Missing),
            ("/last_thread_id", false, Presence::Null),
            (
                "/canonical_inputs/attempts/0/correlation/native_session_id",
                false,
                Presence::Missing,
            ),
        ] {
            assert!(w.fields.iter().any(|f| at(f, path)
                && f.interpretation == Interpretation::Presence { required, presence }));
        }
        if version >= 3 {
            for field in ["model", "effort"] {
                let path = format!("/canonical_inputs/attempts/0/invocation/configuration/{field}");
                assert!(w.fields.iter().any(|f| at(f, &path)
                    && f.interpretation
                        == Interpretation::Presence {
                            required: true,
                            presence: Presence::Null
                        }));
                let mut bad = v.clone();
                bad["canonical_inputs"]["attempts"][0]["invocation"]["configuration"]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(CanonicalHarnessSnapshot::from_released(bad.clone()).is_err());
                let audit = frame_audit(&bad).await;
                let badfacts = classify_facts(&audit).unwrap();
                assert!(badfacts.workers()[0].frame.is_none());
                assert!(blocked(&badfacts.workers()[0], &path, FactReason::Missing));
            }
        }
        let FrameFacts::DerivedByReleasedAdapter {
            adapted, derived, ..
        } = w.frame.as_ref().unwrap()
        else {
            panic!()
        };
        let normalized = serde_json::to_value(adapted).unwrap();
        for (path, value, ancestor) in [
            ("/last_turn_id", json!(null), ""),
            (
                "/canonical_inputs/attempts/0/correlation/native_session_id",
                json!(null),
                "/canonical_inputs/attempts/0/correlation",
            ),
        ] {
            let d = derived.iter().find(|d| d.target_path == path).unwrap();
            assert_eq!(d.value, value);
            assert_eq!(normalized.pointer(path), Some(&d.value));
            assert!(
                matches!(&d.ancestor.location, SourceLocation::Cell { json:Some(a), .. } if a.pointer()==ancestor)
            );
            assert!(
                d.owner_adapter
                    .ends_with("CanonicalHarnessSnapshot::from_released")
            );
            assert!(
                facts
                    .sources()
                    .nodes()
                    .iter()
                    .any(|n| n.source == d.ancestor)
            );
        }
        if version <= 3 {
            assert!(
                derived
                    .iter()
                    .any(|d| d.target_path == "/canonical_inputs/receipts" && d.value == json!([]))
            );
        }
        if version == 2 {
            for (path, value) in [
                ("revision", json!(0)),
                (
                    "invocation",
                    json!({"kind":"prior_invocation_not_recorded"}),
                ),
            ] {
                let path = format!("/canonical_inputs/attempts/0/{path}");
                assert!(
                    derived
                        .iter()
                        .any(|d| d.target_path == path && d.value == value)
                );
                assert!(!w.fields.iter().any(|f| at(f, &path)));
            }
        }
        assert_eq!(std::fs::read(&fixture.path).unwrap(), bytes);
        assert!(
            audit
                .inventory()
                .relations
                .iter()
                .flat_map(|r| &r.rows)
                .flat_map(|r| &r.cells)
                .any(|c| *c == RawCell::Text(raw.as_bytes().into()))
        );
        assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
    }
}
#[tokio::test]
async fn u13_p1_released_unknown_bad_variant_future_duplicate_and_good_neighbor() {
    for version in 2..=5 {
        let valid = frame(version);
        let mut unknown = valid.clone();
        unknown["canonical_inputs"]["attempts"][0]["delivery"]["future"] =
            json!({"raw":{"authority":true}});
        let mut bad = valid.clone();
        bad["canonical_inputs"]["attempts"][0]["delivery"] = json!({"kind":"accepted","basis":"receipt","evidence":{"kind":"native_turn","native_turn_id":"mismatch"}});
        bad["canonical_inputs"]["pending"][0]["queue_position"] = json!("bad");
        let mut variant = valid.clone();
        variant["canonical_inputs"]["attempts"][0]["delivery"] = json!({"kind":"future","raw":23});
        let mut future = valid.clone();
        future["schema_version"] = json!(99);
        let duplicate = valid.to_string().replacen(
            "\"mode\":\"harness\"",
            "\"mode\":\"harness\",\"mode\":\"harness\"",
            1,
        );
        let rows = [
            ("unknown", unknown.to_string()),
            ("bad", bad.to_string()),
            ("variant", variant.to_string()),
            ("future", future.to_string()),
            ("duplicate", duplicate),
            ("syntax", "{bad".into()),
            ("good", valid.to_string()),
        ];
        let sql = rows
            .iter()
            .map(|(id, raw)| worker_row(id, "closed", raw))
            .collect::<String>();
        let fixture = Fixture::new(&format!("{TABLES}{sql}")).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        assert_eq!(facts.workers().len(), rows.len());
        for (id, _) in rows {
            let w = facts
                .workers()
                .iter()
                .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(id.as_bytes().into())))
                .unwrap();
            assert_eq!(
                matches!(w.frame, Some(FrameFacts::DerivedByReleasedAdapter { .. })),
                matches!(id, "good" | "unknown"),
                "V{version} {id}"
            );
            if id == "unknown" {
                assert!(CanonicalHarnessSnapshot::from_released(unknown.clone()).is_ok());
                assert!(blocked(
                    w,
                    "/canonical_inputs/attempts/0/delivery/future",
                    FactReason::UnknownField
                ));
                assert!(blocked(
                    w,
                    "/canonical_inputs/attempts/0/delivery/future/raw/authority",
                    FactReason::Unclassified
                ));
            }
            if id == "bad" {
                assert!(blocked(
                    w,
                    "/canonical_inputs/pending/0/queue_position",
                    FactReason::Malformed
                ));
                assert!(w.fields.iter().any(|f| at(
                    f,
                    "/canonical_inputs/pending/1/queue_position"
                ) && f.interpretation
                    == Interpretation::Stored(json!(7))));
            }
            if id == "duplicate" {
                assert!(w.fields.iter().any(|f| matches!(&f.source.location,
                    SourceLocation::Cell { column_name, json: None, .. } if column_name == "handle_state_json")
                    && f.interpretation == Interpretation::Blocked(FactReason::DuplicateMember)));
                assert!(!facts.sources().duplicates().is_empty());
            }
            if id == "future" {
                assert!(blocked(
                    w,
                    "/schema_version",
                    FactReason::UnsupportedVersion
                ));
            }
            if id == "syntax" {
                assert!(w.frame.is_none());
            }
        }
        assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
    }
}
#[tokio::test]
async fn u13_p1_released_original_sequence_paths_and_derived_ancestors() {
    for version in 2..=5 {
        let mut v = frame(version);
        let attempt = &mut v["canonical_inputs"]["attempts"][0];
        *attempt = sequence(
            attempt,
            if version == 2 {
                "attempt correlation delivery"
            } else {
                "revision invocation attempt correlation delivery"
            },
        );
        let c = &mut v["canonical_inputs"];
        *c = sequence(
            c,
            if version >= 4 {
                "next_queue_position pending active attempts receipts"
            } else {
                "next_queue_position pending active attempts"
            },
        );
        assert!(CanonicalHarnessSnapshot::from_released(v.clone()).is_ok());
        let audit = frame_audit(&v).await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        assert!(w.fields.iter().any(|f| at(f, "/canonical_inputs/3/0/0")
            && matches!(
                f.interpretation,
                Interpretation::Presence {
                    required: true,
                    presence: Presence::Present
                }
            )));
        let FrameFacts::DerivedByReleasedAdapter { derived, .. } = w.frame.as_ref().unwrap() else {
            panic!()
        };
        let path = if version == 2 {
            "/canonical_inputs/attempts/0/invocation"
        } else {
            "/canonical_inputs/attempts/0/revision"
        };
        let d = derived.iter().find(|d| d.target_path == path).unwrap();
        let ancestor = if version == 2 {
            "/canonical_inputs/3/0"
        } else {
            "/canonical_inputs/3/0/0"
        };
        assert!(
            matches!(&d.ancestor.location, SourceLocation::Cell{json:Some(a),..} if a.pointer()==ancestor)
        );
        assert!(
            facts
                .sources()
                .nodes()
                .iter()
                .any(|n| n.source == d.ancestor)
        );
        assert!(
            w.fields
                .iter()
                .any(|f| at(f, "/canonical_inputs/1/1/observation/text")
                    && f.interpretation == Interpretation::Stored(json!("context")))
        );
    }
}

#[tokio::test]
async fn u13_p1_released_bad_types_and_semantic_failures_never_qualify() {
    for version in 2..=5 {
        let mut cases = vec![
            ("/mode", json!("future")),
            (
                "/turn_budget",
                json!({"kind":"planner","budget":0,"start_attempt":"a","warning_attempted":false}),
            ),
            ("/canonical_inputs", json!(42)),
            ("/canonical_inputs/next_queue_position", json!(0)),
            ("/canonical_inputs/pending/0/message/revision", json!(0)),
            ("/canonical_inputs/pending/1/observation_id", json!("bad")),
            ("/canonical_inputs/active/submission_ids", json!([])),
            (
                "/canonical_inputs/attempts/0/attempt/attempt_id",
                json!(false),
            ),
            (
                "/canonical_inputs/attempts/0/correlation/thread_id",
                json!(""),
            ),
            (
                "/canonical_inputs/attempts/0/correlation/rpc_request_id",
                json!(-1),
            ),
            ("/canonical_inputs/attempts/0/delivery", json!(["future"])),
        ];
        if version >= 3 {
            for value in [json!(42), json!({}), json!({"kind":"future"})] {
                cases.push(("/canonical_inputs/attempts/0/invocation", value));
            }
            cases.push(("/canonical_inputs/attempts/0/revision", json!(-1)));
            cases.push((
                "/canonical_inputs/attempts/0/invocation/configuration/model",
                json!(false),
            ));
            cases.push((
                "/canonical_inputs/attempts/0/invocation/configuration/effort",
                json!([]),
            ));
        }
        if version >= 4 {
            cases.push(("/canonical_inputs/receipts/0/key", json!("")));
            let path = if version == 4 {
                "/canonical_inputs/receipts/0/result/revision"
            } else {
                "/canonical_inputs/receipts/0/decision/result/revision"
            };
            cases.push((path, json!("99")));
        }
        for (path, bad) in cases {
            let mut v = frame(version);
            *v.pointer_mut(path).unwrap() = bad;
            assert!(
                CanonicalHarnessSnapshot::from_released(v.clone()).is_err(),
                "V{version} {path}"
            );
            let audit = frame_audit(&v).await;
            let facts = classify_facts(&audit).unwrap();
            let w = &facts.workers()[0];
            assert!(w.frame.is_none(), "V{version} {path}");
            assert!(blocked(w, "", FactReason::Malformed));
            if version < 5 {
                assert!(blocked(
                    w,
                    "/canonical_inputs",
                    FactReason::MissingOwnerDecoder
                ));
            }
            assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
        }
    }
}
#[tokio::test]
async fn u13_p1_released_default_and_nullable_values_are_separate_from_originals() {
    for version in 2..=5 {
        let mut v = frame(version);
        v["turn_budget"] = json!({"kind":"planner","budget":15,"start_attempt":"original","warning_attempted":true});
        v["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("active");
        v["canonical_inputs"]["pending"][0]
            .as_object_mut()
            .unwrap()
            .remove("envelope_id");
        if version >= 3 {
            v["canonical_inputs"]["attempts"][0]["invocation"]["configuration"] =
                json!({"model":"frozen","effort":"high","permission":"never"});
        }
        let audit = frame_audit(&v).await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        for path in [
            "/canonical_inputs/active",
            "/canonical_inputs/pending/0/envelope_id",
            "/turn_budget/context_usage",
        ] {
            assert!(w.fields.iter().any(|f| at(f, path)
                && f.interpretation
                    == Interpretation::Presence {
                        required: false,
                        presence: Presence::Missing
                    }));
        }
        let FrameFacts::DerivedByReleasedAdapter {
            adapted, derived, ..
        } = w.frame.as_ref().unwrap()
        else {
            panic!()
        };
        assert!(adapted.canonical_inputs.active.is_none());
        assert!(
            derived
                .iter()
                .any(|d| d.target_path == "/turn_budget/context_usage"
                    && d.value == json!({"kind":"legacy_not_recorded"}))
        );
        assert!(!facts.sources().nodes().iter().any(|n|matches!(&n.source.location,SourceLocation::Cell{json:Some(a),..} if a.pointer()=="/turn_budget/context_usage")));
        if version >= 3 {
            for (name, value) in [("model", "frozen"), ("effort", "high")] {
                assert!(w.fields.iter().any(|f| at(
                    f,
                    &format!("/canonical_inputs/attempts/0/invocation/configuration/{name}")
                ) && f.interpretation
                    == Interpretation::Stored(json!(value))));
            }
        }
    }
}
#[tokio::test]
async fn u13_p1_released_tagged_sequences_use_proven_original_positions() {
    for version in 2..=5 {
        let mut v = frame(version);
        let c = &mut v["canonical_inputs"];
        c["pending"][0] = sequence(
            &c["pending"][0],
            "kind message queue_position queued_at_ms envelope_id",
        );
        c["pending"][1]["observation"] = json!(["system_context", "context"]);
        c["pending"][1] = sequence(
            &c["pending"][1],
            "kind observation_id observation queue_position queued_at_ms envelope_id addressable",
        );
        c["attempts"][0]["delivery"] = json!(["unknown"]);
        if version >= 3 {
            c["attempts"][0]["invocation"] = json!(["recorded", [null, null, "never"]]);
        }
        if version == 5 {
            c["receipts"][0]["decision"] =
                sequence(&c["receipts"][0]["decision"], "kind request result");
        }
        let adapted = CanonicalHarnessSnapshot::from_released(v.clone()).unwrap();
        let audit = frame_audit(&v).await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        let FrameFacts::DerivedByReleasedAdapter {
            adapted: actual, ..
        } = w.frame.as_ref().unwrap()
        else {
            panic!()
        };
        assert_eq!(actual.as_ref(), &adapted);
        for (path, value) in [
            ("/canonical_inputs/pending/0/2", json!(2)),
            ("/canonical_inputs/pending/1/2/1", json!("context")),
            ("/canonical_inputs/attempts/0/delivery/0", json!("unknown")),
        ] {
            assert!(
                w.fields
                    .iter()
                    .any(|f| at(f, path)
                        && f.interpretation == Interpretation::Stored(value.clone())),
                "V{version} {path}"
            );
        }
        if version >= 3 {
            assert!(
                w.fields
                    .iter()
                    .any(|f| at(f, "/canonical_inputs/attempts/0/invocation/1/0")
                        && f.interpretation
                            == Interpretation::Presence {
                                required: true,
                                presence: Presence::Null
                            })
            );
        }
        assert!(
            !w.fields
                .iter()
                .any(|f| at(f, "/canonical_inputs/pending/1/2/1")
                    && f.interpretation
                        == Interpretation::Blocked(FactReason::DeferredObservationInterpretation))
        );
        assert!(
            !w.fields
                .iter()
                .any(|f| at(f, "/canonical_inputs/pending/0/2")
                    && f.interpretation
                        == Interpretation::Blocked(FactReason::DeferredObservationInterpretation))
        );
    }
}

#[tokio::test]
async fn u13_p1_released_delivery_variants_and_v5_retry_are_evidence_only() {
    for version in 2..=5 {
        for delivery in [
            json!({"kind":"crossing_unknown"}),
            json!({"kind":"unknown"}),
            json!({"kind":"not_dispatched","basis":"local_preflight"}),
            json!({"kind":"accepted","basis":"receipt","evidence":{"kind":"submission","submission_id":"1".repeat(32)}}),
            json!({"kind":"accepted","basis":"receipt","evidence":{"kind":"native_turn","native_turn_id":"native"}}),
        ] {
            let mut v = frame(version);
            v["canonical_inputs"]["attempts"][0]["delivery"] = delivery.clone();
            if delivery.pointer("/evidence/native_turn_id").is_some() {
                v["canonical_inputs"]["attempts"][0]["correlation"]["native_turn_id"] =
                    json!("native");
            }
            let expected = CanonicalHarnessSnapshot::from_released(v.clone()).unwrap();
            let audit = frame_audit(&v).await;
            let facts = classify_facts(&audit).unwrap();
            let FrameFacts::DerivedByReleasedAdapter { adapted, .. } =
                facts.workers()[0].frame.as_ref().unwrap()
            else {
                panic!()
            };
            assert_eq!(adapted.as_ref(), &expected);
            assert_eq!(
                serde_json::to_value(adapted).unwrap()["canonical_inputs"]["attempts"][0]["delivery"],
                delivery
            );
            assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
        }
    }
    let mut v = frame(5);
    let predecessor = v["canonical_inputs"]["receipts"][0]["decision"]["request"].clone();
    v["canonical_inputs"]["receipts"][0]["decision"] = json!({"kind":"retry","predecessor":predecessor,"retry_intent_id":"6".repeat(32),"revision":"4","waiting_for_settlement":true});
    let expected = CanonicalHarnessSnapshot::from_released(v.clone()).unwrap();
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    let FrameFacts::DerivedByReleasedAdapter { adapted, .. } =
        facts.workers()[0].frame.as_ref().unwrap()
    else {
        panic!()
    };
    assert_eq!(adapted.as_ref(), &expected);
    assert!(facts.workers()[0].fields.iter().any(|f| at(
        f,
        "/canonical_inputs/receipts/0/decision/retry_intent_id"
    ) && f.interpretation
        == Interpretation::Stored(json!("6".repeat(32)))));
}

#[tokio::test]
async fn u13_p1_released_shared_sequence_derived_sources() {
    let mut mismatches = vec![];
    for version in 2..=5 {
        for nested in [false, true] {
            let mut v = frame(version);
            v["token_usage"] = json!([11, 22, null, 33]);
            let pending = &mut v["canonical_inputs"]["pending"][1];
            pending["observation"] = if nested {
                json!({"type":"ask_answered","track_id":"track","answers":[["title","answer"]]})
            } else {
                json!(["system_context", "context"])
            };
            *pending = sequence(
                pending,
                "kind observation_id observation queue_position queued_at_ms envelope_id addressable",
            );
            let c = &mut v["canonical_inputs"];
            *c = sequence(
                c,
                if version >= 4 {
                    "next_queue_position pending active attempts receipts"
                } else {
                    "next_queue_position pending active attempts"
                },
            );
            assert!(CanonicalHarnessSnapshot::from_released(v.clone()).is_ok());
            let audit = frame_audit(&v).await;
            let facts = classify_facts(&audit).unwrap();
            let w = &facts.workers()[0];
            let FrameFacts::DerivedByReleasedAdapter { derived, .. } = w.frame.as_ref().unwrap()
            else {
                panic!()
            };
            let (target, source, value) = if nested {
                ("answers/0/answer", "answers/0/1", json!("answer"))
            } else {
                ("text", "1", json!("context"))
            };
            for (target, source, value) in [
                (
                    "/token_usage/used_tokens".into(),
                    "/token_usage/0".into(),
                    json!(11),
                ),
                (
                    format!("/canonical_inputs/pending/1/observation/{target}"),
                    format!("/canonical_inputs/1/1/2/{source}"),
                    value,
                ),
            ] {
                let original = w
                    .fields
                    .iter()
                    .find(|f| {
                        at(f, &source) && f.interpretation == Interpretation::Stored(value.clone())
                    })
                    .unwrap();
                let d = derived.iter().find(|d| d.target_path == target).unwrap();
                assert_eq!(d.value, value);
                if d.ancestor != original.source {
                    mismatches.push(format!("V{version} {target} -> {source}"));
                }
                assert!(
                    facts
                        .sources()
                        .nodes()
                        .iter()
                        .any(|n| n.source == d.ancestor)
                );
            }
            assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:?}");
}
