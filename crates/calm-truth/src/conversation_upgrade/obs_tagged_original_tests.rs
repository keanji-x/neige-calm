use super::{classification_tests::*, *};
use calm_types::{
    conversation::SystemObservation, event::ForgeCheckDiagnostics,
    git_candidate::DeliverySettlement, harness::snapshot::CanonicalHarnessSnapshot,
    observation::ReportEditPresentation, verify_target::*,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub(super) fn codec<T: DeserializeOwned>(value: Value, accepted: bool) {
    let outcomes = [
        serde_json::from_value::<T>(value.clone()).is_ok(),
        T::deserialize(&value).is_ok(),
        serde_json::from_str::<T>(&value.to_string()).is_ok(),
    ];
    assert_eq!(outcomes, [accepted; 3], "{value}");
}
fn gate(target: Value) -> Value {
    json!({"type":"task_gate_result","idempotency_key":"k","key":"k","passed":true,"log_tail":"log","attempt":1,"target":target})
}
pub(super) fn frame(version: u64, observations: Vec<Value>) -> Value {
    let mut v = serde_json::to_value(CanonicalHarnessSnapshot::dormant(7)).unwrap();
    v["schema_version"] = json!(version);
    if version < 4 {
        v["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("receipts");
    }
    v["canonical_inputs"]["next_queue_position"] = json!(observations.len());
    v["canonical_inputs"]["pending"] = Value::Array(observations.into_iter().enumerate().map(|(i, observation)| json!({"kind":"system","observation_id":format!("{i:032x}"),"observation":observation,"queue_position":i,"queued_at_ms":1,"envelope_id":null,"addressable":true})).collect());
    v
}
pub(super) fn cases() -> Vec<(Value, &'static str, &'static str)> {
    let mut cases = vec![
        (
            gate(json!(["unbound", "terminal"])),
            "target/reason",
            "target/1",
        ),
        (
            gate(json!(["no_candidate", ["delivery_pending", "delivery"]])),
            "target/reason/delivery_id",
            "target/1/1",
        ),
        (
            gate(json!(["no_candidate", ["delivery_failed", "delivery"]])),
            "target/reason/delivery_id",
            "target/1/1",
        ),
        (
            json!({"type":"report_edited","presentation":["knowledge","knowledge"],"track_id":"track","body_sha256":"hash","body":"body"}),
            "presentation/knowledge_text",
            "presentation/1",
        ),
        (
            json!({"type":"task_git_delivery_settled","key":"key","attempt_id":"attempt","result":["failed","unresolved","reason",false]}),
            "result/reason",
            "result/2",
        ),
        (
            json!({"type":"task_git_delivery_settled","key":"key","attempt_id":"attempt","result":["candidate","candidate","commit","base",true]}),
            "result/base_is_ancestor",
            "result/4",
        ),
    ];
    let sample = json!(["head", ["dirty"], ["realpath", "common", true]]);
    for evidence in [
        json!(["refused", "cwd", sample, ["head"]]),
        json!(["verified", "cwd", sample, sample, []]),
        json!(["reused", "run", "cwd", sample]),
    ] {
        let reused = evidence[0] == "reused";
        cases.push((
            gate(json!([
                "candidate",
                "candidate",
                "commit",
                "lease",
                evidence
            ])),
            "target/evidence/before/provenance/registered",
            if reused {
                "target/4/3/2/2"
            } else {
                "target/4/2/2/2"
            },
        ));
    }
    for phase in [
        json!(["prepare", "reason"]),
        json!(["finalize", "cwd", "reason"]),
        json!(["compensation", "cwd", "reason"]),
        json!(["reconciliation", "cwd", "reason"]),
    ] {
        let prepare = phase[0] == "prepare";
        let reconciliation = phase[0] == "reconciliation";
        cases.push((
            gate(json!([
                "candidate",
                "candidate",
                "commit",
                "lease",
                ["unsampled", phase]
            ])),
            if reconciliation {
                "target/evidence/phase/last_error"
            } else {
                "target/evidence/phase/reason"
            },
            if prepare {
                "target/4/1/1"
            } else {
                "target/4/1/2"
            },
        ));
    }
    for diagnostics in [
        json!(["available", ["test"], ["step"], "error", "url", true]),
        json!(["unavailable", "reason"]),
    ] {
        let available = diagnostics[0] == "available";
        cases.push((json!({"type":"forge_pr_checks","track_id":"track","pr_number":1,"conclusion":"failure","failed_checks":[{"name":"job","id":"id","diagnostics":diagnostics}]}), if available {"failed_checks/0/diagnostics/error_summary"} else {"failed_checks/0/diagnostics/reason"}, if available {"failed_checks/0/diagnostics/3"} else {"failed_checks/0/diagnostics/1"}));
    }
    cases
}

fn original_frame(version: u64, observation: Value) -> (Value, String) {
    let healthy = json!({"type":"system_context","text":"healthy"});
    if version > 1 {
        return (
            frame(version, vec![observation, healthy]),
            "/canonical_inputs/pending".into(),
        );
    }
    let mut v = original();
    v["pending_queue"] = json!([observation, healthy]);
    v["pending_envelope_ids"] = json!([1, 2]);
    v["pending_entry_meta"] =
        json!([{"id":"q0","rev":1,"queued_at_ms":1},{"id":"q1","rev":1,"queued_at_ms":1}]);
    v["pending_message_ids"] = json!([["m0"], ["m1"]]);
    (v, "/pending_queue".into())
}
fn observation_root(version: u64, parent: &str, index: usize) -> String {
    if version == 1 {
        format!("{parent}/{index}")
    } else {
        format!("{parent}/{index}/observation")
    }
}
#[test]
fn u13_p1_released_obs_tagged_public_codecs() {
    codec::<ReportEditPresentation>(json!(["knowledge", "text"]), true);
    codec::<ReportEditPresentation>(json!(["legacy"]), true);
    codec::<DeliverySettlement>(json!(["failed", "unresolved", "reason", false]), true);
    codec::<VerifyTarget>(json!(["unbound", "terminal"]), true);
    codec::<NoCandidateReason>(json!(["delivery_pending", "delivery"]), true);
    codec::<VerifyTargetEvidence>(json!(["unsampled", ["prepare", "reason"]]), true);
    codec::<SamplePhase>(json!(["finalize", "cwd", "reason"]), true);
    codec::<ForgeCheckDiagnostics>(json!(["unavailable", "reason"]), true);
    codec::<VerifyTarget>(json!(["future", {"authority": true}]), false);
    codec::<VerifyTarget>(json!(["unbound"]), false);
    codec::<VerifyTarget>(json!(["unbound", 7]), false);
    codec::<ForgeCheckDiagnostics>(json!(["unavailable", ""]), false);
    codec::<SystemObservation>(json!({"type":"user_message","text":"human"}), false);
}
pub(super) async fn source_matrix(check: impl Fn(u64, &WorkerFacts, &str, &SourceRef, &Value)) {
    let mut missing = vec![];
    for version in 1..=5 {
        for (observation, target, source) in cases() {
            codec::<SystemObservation>(observation.clone(), true);
            let (v, parent) = original_frame(version, observation);
            if version > 1 {
                assert!(CanonicalHarnessSnapshot::from_released(v.clone()).is_ok());
            }
            let fixture = fixture(&v.to_string()).await;
            let before = std::fs::read(&fixture.path).unwrap();
            let audit = fixture.audit().await;
            let facts = classify_facts(&audit).unwrap();
            let w = &facts.workers()[0];
            let path = format!("{}/{source}", observation_root(version, &parent, 0));
            let node = facts.sources().nodes().iter().find(|n| matches!(&n.source.location, SourceLocation::Cell { json: Some(a), .. } if a.pointer() == path)).unwrap();
            let expected = v.pointer(&path).unwrap();
            let stored = w.fields.iter().any(|f| {
                f.source == node.source
                    && f.expected_path.is_none()
                    && f.interpretation == Interpretation::Stored(expected.clone())
            });
            let deferred = w.fields.iter().any(|f| {
                f.source == node.source
                    && f.interpretation
                        == Interpretation::Blocked(FactReason::DeferredObservationInterpretation)
            });
            println!("V{version} {path} stored={stored} deferred={deferred}");
            if !stored || deferred {
                missing.push((version, path));
            }
            if version > 1 {
                check(
                    version,
                    w,
                    &format!("/canonical_inputs/pending/0/observation/{target}"),
                    &node.source,
                    expected,
                );
            }
            let healthy = format!("{}/text", observation_root(version, &parent, 1));
            assert!(
                w.fields.iter().any(|f| at(f, &healthy)
                    && f.interpretation == Interpretation::Stored(json!("healthy")))
            );
            assert!(!blocked(
                w,
                &healthy,
                FactReason::DeferredObservationInterpretation
            ));
            assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
            assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
        }
    }
    assert!(missing.is_empty(), "{missing:?}");
}
#[tokio::test]
async fn u13_p1_obs_tagged_original_unknown_anchor() {
    for version in 1..=5 {
        let observation = gate(json!(["future",{"authority":true}]));
        codec::<SystemObservation>(observation.clone(), false);
        let (v, parent) = original_frame(version, observation);
        if version > 1 {
            assert!(CanonicalHarnessSnapshot::from_released(v.clone()).is_err());
        }
        let fixture = fixture(&v.to_string()).await;
        let before = std::fs::read(&fixture.path).unwrap();
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        let w = &facts.workers()[0];
        let path = format!("{}/target/0", observation_root(version, &parent, 0));
        let node = facts.sources().nodes().iter().find(|n| matches!(&n.source.location, SourceLocation::Cell { json: Some(a), .. } if a.pointer() == path)).unwrap();
        let actual: Vec<_> = w
            .fields
            .iter()
            .filter(|f| f.source == node.source)
            .collect();
        println!("V{version} unknown {path}: {actual:?}");
        assert!(actual.iter().any(|f| f.expected_path.is_none()
            && f.interpretation == Interpretation::Blocked(FactReason::Malformed)));
        let healthy = format!("{}/text", observation_root(version, &parent, 1));
        assert!(w.fields.iter().any(
            |f| at(f, &healthy) && f.interpretation == Interpretation::Stored(json!("healthy"))
        ));
        assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
        assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
    }
}
pub(super) async fn rejection_matrix(check: impl Fn(&WorkerFacts)) {
    for version in 2..=5 {
        for target in [
            json!(["future",{"authority":true}]),
            json!(["unbound"]),
            json!(["unbound", 7]),
            json!([
                "candidate",
                7,
                false,
                "lease",
                ["unsampled", ["prepare", false]]
            ]),
        ] {
            codec::<SystemObservation>(gate(target.clone()), false);
            let v = frame(
                version,
                vec![
                    gate(target.clone()),
                    json!({"type":"system_context","text":"healthy"}),
                ],
            );
            assert!(CanonicalHarnessSnapshot::from_released(v.clone()).is_err());
            let audit = frame_audit(&v).await;
            let facts = classify_facts(&audit).unwrap();
            let w = &facts.workers()[0];
            check(w);
            assert!(
                w.fields
                    .iter()
                    .any(|f| at(f, "/canonical_inputs/pending/1/observation/text")
                        && f.interpretation == Interpretation::Stored(json!("healthy")))
            );
            assert!(blocked(
                w,
                "/canonical_inputs/pending/0/observation/target",
                FactReason::Malformed
            ));
            assert!(facts.sources().nodes().iter().filter(|n| matches!(&n.source.location, SourceLocation::Cell { json: Some(a), .. } if a.pointer().starts_with("/canonical_inputs/pending/0/observation/target"))).all(|n| w.fields.iter().any(|f| f.source == n.source)));
            assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
        }
    }
}

#[tokio::test]
async fn u13_p1_obs_tagged_original_sources() {
    source_matrix(|_, _, _, _, _| {}).await;
}
#[tokio::test]
async fn u13_p1_obs_tagged_original_rejections_keep_siblings() {
    rejection_matrix(|_| {}).await;
}

#[tokio::test]
async fn u13_p1_obs_tagged_preserves_uninterpreted_sequence_carrier() {
    let mut v = frame(5, vec![gate(json!(["unbound", "terminal"]))]);
    let slot = &v["canonical_inputs"]["pending"][0];
    let sequence: Vec<_> = [
        "kind",
        "observation_id",
        "observation",
        "queue_position",
        "queued_at_ms",
        "envelope_id",
        "addressable",
    ]
    .into_iter()
    .map(|name| slot[name].clone())
    .collect();
    v["canonical_inputs"]["pending"][0] = json!(sequence);
    assert!(CanonicalHarnessSnapshot::from_released(v.clone()).is_ok());
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    let w = &facts.workers()[0];
    // Released adaptation preserves sequence carriers; nested tagged values must not activate
    // named-field dispatch against an array and invent a missing observation.
    assert!(
        !w.fields
            .iter()
            .any(|f| f.expected_path.as_deref() == Some("/canonical_inputs/pending/0/observation"))
    );
    assert!(matches!(
        w.frame,
        Some(FrameFacts::DerivedByReleasedAdapter {
            original_version: 5,
            ..
        })
    ));
}
