use super::{
    classification_tests::*,
    core_matrix_tests::{assert_case, replace},
    *,
};
use serde_json::{Value, json};

// Concrete public-shape fixtures; independent of projection output.
fn delivery_payloads() -> Vec<Value> {
    vec![
        json!({"kind":"candidate","candidate_id":"c","commit_sha":"h","base_sha":"b","base_is_ancestor":true}),
        json!({"kind":"failed","code":"unresolved","reason":"r","retry_allowed":false}),
    ]
}
fn verification_payloads() -> Vec<Value> {
    let sample = json!({"head":"h","dirty":["entry"],"provenance":{"realpath":"p","common_dir":"g","registered":true}});
    let mut evidence = vec![
        json!({"kind":"refused","cwd":"c","before":sample,"reasons":["head","dirty","provenance"]}),
        json!({"kind":"verified","cwd":"c","before":sample,"after":sample,"reasons":[]}),
        json!({"kind":"reused","run":"a#r1","cwd":"c","before":sample}),
    ];
    for phase in [
        json!({"kind":"prepare","reason":"r"}),
        json!({"kind":"finalize","cwd":"c","reason":"r"}),
        json!({"kind":"compensation","cwd":"c","reason":"r"}),
        json!({"kind":"reconciliation","cwd":"c","last_error":"r"}),
    ] {
        evidence.push(json!({"kind":"unsampled","phase":phase}));
    }
    let mut targets: Vec<_> = evidence.into_iter().map(|evidence| {
        json!({"kind":"candidate","candidate_id":"c","commit_sha":"h","lease_id":"l","evidence":evidence})
    }).collect();
    for reason in [
        json!({"kind":"delivery_pending","delivery_id":"d"}),
        json!({"kind":"delivery_failed","delivery_id":"d"}),
        json!({"kind":"no_delivery_row"}),
    ] {
        targets.push(json!({"kind":"no_candidate","reason":reason}));
    }
    for reason in [
        "legacy_lease",
        "legacy_frozen",
        "legacy_verdict",
        "terminal",
    ] {
        targets.push(json!({"kind":"unbound","reason":reason}));
    }
    targets
}
fn nested_frames() -> Vec<(Value, String)> {
    let mut frames = vec![];
    for result in delivery_payloads() {
        let mut v = original();
        v["pending_queue"] = json!([{"type":"task_git_delivery_settled","key":"k","attempt_id":"a","result":result}]);
        frames.push((v, "/pending_queue/0/result".into()));
    }
    for target in verification_payloads() {
        let mut v = original();
        v["pending_queue"] = json!([{"type":"task_gate_result","idempotency_key":"k","key":"k","passed":true,"log_tail":"l","attempt":1,"target":target}]);
        frames.push((v, "/pending_queue/0/target".into()));
    }
    for diagnostics in [
        json!({"status":"available","failed_tests":["test"],"failed_steps":["step"],"error_summary":"e","log_url":"u","truncated":false}),
        json!({"status":"unavailable","reason":"r"}),
    ] {
        for locator in [json!({"url":"u"}), json!({"id":"i"})] {
            let mut check = json!({"name":"n","diagnostics":diagnostics});
            check
                .as_object_mut()
                .unwrap()
                .extend(locator.as_object().unwrap().clone());
            let mut v = original();
            v["pending_queue"] = json!([{"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","failed_checks":[check]}]);
            frames.push((v, "/pending_queue/0/failed_checks/0".into()));
        }
    }
    frames
}
// Test-only enumeration of fixture paths, independent of FrameView/SourceRefs.
fn object_paths(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            out.push(path.into());
            for (name, child) in object {
                object_paths(
                    child,
                    &format!("{path}/{}", name.replace('~', "~0").replace('/', "~1")),
                    out,
                );
            }
        }
        Value::Array(array) => {
            for (i, child) in array.iter().enumerate() {
                object_paths(child, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}
async fn assert_stored(v: &Value, path: &str, value: Value) {
    let fixture = fixture(&v.to_string()).await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    assert!(
        facts.workers()[0].fields.iter().any(|f| at(f, path)
            && f.expected_path.is_none()
            && f.interpretation == Interpretation::Stored(value.clone())),
        "{path}"
    );
    assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
}
#[tokio::test]
async fn u13_p1_facts_observation_public_nested_source_leaf_matrix() {
    for (v, root) in nested_frames() {
        assert_stored(&v, &root, v.pointer(&root).unwrap().clone()).await;
        let mut parents = vec![];
        object_paths(v.pointer(&root).unwrap(), &root, &mut parents);
        for parent in parents {
            let object = v.pointer(&parent).unwrap().as_object().unwrap();
            // Each field, including public tags: absence/null/type; independent
            // healthy siblings continue even when the aggregate reports first error.
            for (name, good) in object {
                let path = format!("{parent}/{name}");
                for (bad, reason) in [
                    (None, FactReason::Missing),
                    (Some(Value::Null), FactReason::Null),
                    (
                        Some(if good.is_boolean() {
                            json!("bad")
                        } else {
                            json!(false)
                        }),
                        FactReason::Malformed,
                    ),
                ] {
                    let mut changed = v.clone();
                    replace(&mut changed, &path, bad);
                    // Forge's untagged URL alternative selects id on absent url.
                    let expected = if name == "url" && reason == FactReason::Missing {
                        format!("{parent}/id")
                    } else {
                        path.clone()
                    };
                    assert_case(&changed, &[(&expected, reason)]).await;
                }
                assert_stored(&v, &path, good.clone()).await;
            }
            let mut simultaneous = v.clone();
            let mut expected = vec![];
            for (name, good) in object {
                if name == "kind" || name == "status" {
                    continue;
                }
                let path = format!("{parent}/{name}");
                replace(
                    &mut simultaneous,
                    &path,
                    Some(if good.is_boolean() {
                        json!("bad")
                    } else {
                        json!(false)
                    }),
                );
                expected.push((path, FactReason::Malformed));
            }
            let refs: Vec<_> = expected
                .iter()
                .map(|(p, r)| (p.as_str(), r.clone()))
                .collect();
            assert_case(&simultaneous, &refs).await;
            // Unknown descendants retain exact raw nodes, not an invented schema.
            let mut unknown = v.clone();
            unknown.pointer_mut(&parent).unwrap()["future/~"] = json!({"leaf":true});
            let path = format!("{parent}/future~1~0");
            assert_case(
                &unknown,
                &[
                    (&path, FactReason::UnknownField),
                    (&format!("{path}/leaf"), FactReason::Unclassified),
                ],
            )
            .await;
            for tag in ["kind", "status"] {
                if !object.contains_key(tag) {
                    continue;
                }
                let mut changed = v.clone();
                replace(
                    &mut changed,
                    &format!("{parent}/{tag}"),
                    Some(json!("future")),
                );
                assert_case(
                    &changed,
                    &[
                        (&parent, FactReason::Malformed),
                        (&format!("{parent}/{tag}"), FactReason::Malformed),
                        (
                            &format!("{parent}/{tag}"),
                            FactReason::DeferredObservationInterpretation,
                        ),
                    ],
                )
                .await;
            }
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_observation_public_array_siblings_and_diagnostics() {
    let mut cases = vec![];
    let mut scan = original();
    scan["pending_queue"] =
        json!([{"type":"forge_scan_completed","track_id":"t","overlapping_prs":[1]}]);
    cases.push((
        scan,
        "/pending_queue/0/overlapping_prs".to_string(),
        json!(3),
    ));
    for (v, root) in nested_frames() {
        let mut parents = vec![];
        object_paths(v.pointer(&root).unwrap(), &root, &mut parents);
        for parent in parents {
            for (name, value) in v.pointer(&parent).unwrap().as_object().unwrap() {
                if let Some(array) = value.as_array() {
                    let good = array.first().cloned().unwrap_or_else(|| {
                        if name == "reasons" {
                            json!("head")
                        } else {
                            json!("healthy")
                        }
                    });
                    cases.push((v.clone(), format!("{parent}/{name}"), good));
                }
            }
        }
    }
    for (mut v, path, good) in cases {
        *v.pointer_mut(&path).unwrap() = json!([false, {}, good]);
        assert_case(
            &v,
            &[
                (&format!("{path}/0"), FactReason::Malformed),
                (&format!("{path}/1"), FactReason::Malformed),
            ],
        )
        .await;
        assert_stored(&v, &format!("{path}/2"), good).await;
    }
    // Owner semantic diagnostics remain authoritative even with well-typed leaves.
    let mut v = original();
    v["pending_queue"] = json!([{"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","failed_checks":[{
        "name":"n","id":"i","diagnostics":{"status":"available","failed_tests":[" "],"failed_steps":[],"error_summary":"e","log_url":"u","truncated":false}
    }]}]);
    assert_case(
        &v,
        &[(
            "/pending_queue/0/failed_checks/0/diagnostics",
            FactReason::Malformed,
        )],
    )
    .await;
    assert_stored(
        &v,
        "/pending_queue/0/failed_checks/0/diagnostics/error_summary",
        json!("e"),
    )
    .await;
}

#[tokio::test]
async fn u13_p1_facts_observation_released_actual_entry_and_system_refusal() {
    for version in 2..=5 {
        for (observation, refused) in [
            (json!({"type":"system_context","text":"healthy"}), false),
            (json!({"type":"user_message","text":"human"}), true),
        ] {
            let mut v = serde_json::to_value(
                calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(7),
            )
            .unwrap();
            v["schema_version"] = json!(version);
            v["canonical_inputs"]["pending"] = json!([{"kind":"system","observation_id":"o","observation":observation,"queue_position":0,"queued_at_ms":1,"envelope_id":null,"addressable":false}]);
            let fixture = fixture(&v.to_string()).await;
            let audit = fixture.audit().await;
            let facts = classify_facts(&audit).unwrap();
            super::node_oracle::assert_all(&fixture, &facts).await;
            let w = &facts.workers()[0];
            let path = "/canonical_inputs/pending/0/observation";
            assert_eq!(blocked(w, path, FactReason::Malformed), refused);
            assert!(
                matches!(w.frame, Some(FrameFacts::DeferredReleased { original_version }) if original_version == version)
            );
            assert_stored(&v, &format!("{path}/text"), observation["text"].clone()).await;
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_observation_public_leaf_enums_and_opaque_result() {
    for code in [
        "workspace_missing",
        "provenance_mismatch",
        "commit_failed",
        "unresolved",
    ] {
        let mut v = original();
        v["pending_queue"] = json!([{"type":"task_git_delivery_settled","key":"k","attempt_id":"a","result":{"kind":"failed","code":code,"reason":"r","retry_allowed":false}}]);
        assert_stored(&v, "/pending_queue/0/result/code", json!(code)).await;
        v["pending_queue"][0]["result"]["code"] = json!("future");
        assert_case(
            &v,
            &[("/pending_queue/0/result/code", FactReason::Malformed)],
        )
        .await;
    }
    for author in ["planner", "user", "assistant", "kernel", "plugin"] {
        for presentation in [
            json!({"version":"legacy"}),
            json!({"version":"knowledge","knowledge_text":"k"}),
        ] {
            let mut v = original();
            v["pending_queue"] = json!([{"type":"report_edited","track_id":"t","body_sha256":"h","body":"b","author":author,"presentation":presentation}]);
            assert_stored(&v, "/pending_queue/0/author", json!(author)).await;
            assert_stored(&v, "/pending_queue/0/presentation", presentation.clone()).await;
            v["pending_queue"][0]["presentation"]["version"] = json!("future");
            assert_case(
                &v,
                &[(
                    "/pending_queue/0/presentation/version",
                    FactReason::Malformed,
                )],
            )
            .await;
        }
    }
    for kind in ["codex_stop", "claude_stop"] {
        let mut v = original();
        v["pending_queue"] =
            json!([{"type":"worker_hook_stop","track_id":"t","card_id":"c","kind":kind}]);
        assert_stored(&v, "/pending_queue/0/kind", json!(kind)).await;
        v["pending_queue"][0]["kind"] = json!("future");
        assert_case(&v, &[("/pending_queue/0/kind", FactReason::Malformed)]).await;
    }
    for result in [json!(null), json!(false), json!([1,{"unknown":[true]}])] {
        let mut v = original();
        v["pending_queue"] =
            json!([{"type":"task_completed","idempotency_key":"k","result":result}]);
        if result.is_null() {
            assert_case(&v, &[("/pending_queue/0/result", FactReason::Null)]).await;
        } else {
            assert_stored(&v, "/pending_queue/0/result", result.clone()).await;
        }
        if result.is_array() {
            assert_case(
                &v,
                &[(
                    "/pending_queue/0/result/1/unknown/0",
                    FactReason::Unclassified,
                )],
            )
            .await;
        }
    }
    let mut v = original();
    v["pending_queue"] = json!([{"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","failed_checks":[{"name":"n","diagnostics":{"status":"unavailable","reason":"r"}}]}]);
    assert_case(
        &v,
        &[("/pending_queue/0/failed_checks/0/id", FactReason::Missing)],
    )
    .await;
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    let parent = "/pending_queue/0/failed_checks/0";
    assert!(facts.workers()[0].fields.iter().any(|f| f.expected_path.as_deref() == Some(parent)
        && matches!(&f.source.location, SourceLocation::Cell { json: Some(a), .. } if a.pointer() == parent)
        && f.interpretation == Interpretation::Blocked(FactReason::OwnerDiagnosticNotLeaf)));
}

#[tokio::test]
async fn u13_p1_facts_observation_numeric_root_and_nested_struct_sequences() {
    let sample = json!(["h", ["entry"], ["p", "g", true]]);
    let mut observations = super::observation_tests::observations();
    observations.extend([
        json!({"type":"task_gate_result","idempotency_key":"k","key":"k","passed":true,"log_tail":"l","attempt":1,"target":{"kind":"candidate","candidate_id":"c","commit_sha":"h","lease_id":"l","evidence":{"kind":"verified","cwd":"c","before":sample,"after":sample,"reasons":[]}}}),
        json!({"type":"report_edited","track_id":"t","body_sha256":"h","body":"b","blocks_after":[["b",1]]}),
        json!({"type":"ask_answered","track_id":"t","answers":[["q","a"]]}),
        json!({"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","snapshot":["h","m",true]}),
    ]);
    for observation in observations {
        assert!(
            serde_json::from_value::<calm_types::observation::Observation>(observation.clone())
                .is_ok()
        );
        let v = json!([1,"harness","idle",{"kind":"legacy"},0,[observation],[1],
            [{"id":"q","rev":1,"queued_at_ms":1,"attachments":[]}],[[]],null,null,null,null,null,
            null,null,null,null,null,null,null,false,null,null,null]);
        assert_stored(&v, "/5/0/type", observation["type"].clone()).await;
        for (name, value) in observation.as_object().unwrap() {
            if !value.is_null() {
                assert_stored(&v, &format!("/5/0/{name}"), value.clone()).await;
            }
        }
        let root = "/5/0/target/evidence/before";
        if observation["type"] == "task_gate_result" && observation["target"]["kind"] == "candidate"
        {
            for (tail, value) in [
                ("/0", json!("h")),
                ("/1/0", json!("entry")),
                ("/2/0", json!("p")),
                ("/2/1", json!("g")),
                ("/2/2", json!(true)),
            ] {
                assert_stored(&v, &format!("{root}{tail}"), value).await;
            }
            let mut bad = v.clone();
            bad[5][0]["target"]["evidence"]["before"] =
                json!([false, [false, {}, "healthy"], [false, false, true]]);
            assert_case(
                &bad,
                &[
                    (&format!("{root}/0"), FactReason::Malformed),
                    (&format!("{root}/1/0"), FactReason::Malformed),
                    (&format!("{root}/1/1"), FactReason::Malformed),
                    (&format!("{root}/2/0"), FactReason::Malformed),
                    (&format!("{root}/2/1"), FactReason::Malformed),
                ],
            )
            .await;
            assert_stored(&bad, &format!("{root}/1/2"), json!("healthy")).await;
            assert_stored(&bad, &format!("{root}/2/2"), json!(true)).await;
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_observation_released_all_owners_and_checkpoint_representations() {
    let mut observations = super::observation_tests::observations();
    observations.extend(
        nested_frames()
            .into_iter()
            .map(|(v, _)| v["pending_queue"][0].clone()),
    );
    for version in 2..=5 {
        for observation in &observations {
            let accepted = serde_json::from_value::<calm_types::conversation::SystemObservation>(
                observation.clone(),
            )
            .is_ok();
            for sequence in [false, true] {
                let pending = json!([{"kind":"system","observation_id":"o","observation":observation,"queue_position":0,"queued_at_ms":1,"envelope_id":null,"addressable":false}]);
                let checkpoint = if sequence {
                    json!([1, pending, null, [], []])
                } else {
                    json!({"next_queue_position":1,"pending":pending,"active":null,"attempts":[],"receipts":[]})
                };
                let v = json!({"schema_version":version,"canonical_inputs":checkpoint});
                let fixture = fixture(&v.to_string()).await;
                let audit = fixture.audit().await;
                let facts = classify_facts(&audit).unwrap();
                super::node_oracle::assert_all(&fixture, &facts).await;
                let w = &facts.workers()[0];
                let path = if sequence {
                    "/canonical_inputs/1/0/observation"
                } else {
                    "/canonical_inputs/pending/0/observation"
                };
                assert_eq!(blocked(w, path, FactReason::Malformed), !accepted);
                assert!(
                    matches!(w.frame,Some(FrameFacts::DeferredReleased {original_version}) if original_version==version)
                );
                assert!(w.fields.iter().any(|f| at(f, &format!("{path}/type"))
                    && f.interpretation == Interpretation::Stored(observation["type"].clone())));
                assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
            }
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_observation_locator_owner_and_exact_deferred_retirement() {
    for locator in [
        json!({"url":"u"}),
        json!({"id":"i"}),
        json!({"url":false,"id":"i"}),
        json!({"url":null,"id":"i"}),
    ] {
        let mut check = json!({"name":"n"});
        check
            .as_object_mut()
            .unwrap()
            .extend(locator.as_object().unwrap().clone());
        assert!(
            serde_json::from_value::<calm_types::event::ForgeFailedCheck>(check.clone()).is_ok()
        );
        let mut v = original();
        v["pending_queue"] = json!([
            {"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","failed_checks":[check]},
            {"type":"task_completed","idempotency_key":"k","result":{"opaque":[null,true]}},
            {"type":"future","text":"uninterpreted"}
        ]);
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        let w = &facts.workers()[0];
        assert!(!blocked(
            w,
            "/pending_queue/0/failed_checks/0",
            FactReason::Malformed
        ));
        assert!(!blocked(
            w,
            "/pending_queue/1/idempotency_key",
            FactReason::DeferredObservationInterpretation
        ));
        for path in ["/pending_queue/1/result/opaque/0", "/pending_queue/2/text"] {
            assert!(blocked(
                w,
                path,
                FactReason::DeferredObservationInterpretation
            ));
            assert!(
                !w.fields
                    .iter()
                    .any(|f| at(f, path) && matches!(f.interpretation, Interpretation::Stored(_)))
            );
        }
        assert!(blocked(
            w,
            "/pending_queue/2/type",
            FactReason::MissingOwnerDecoder
        ));
        assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
    }
}
#[tokio::test]
async fn u13_p1_facts_observation_struct_array_slots_bad_bad_good() {
    let mut v = original();
    v["pending_queue"] = json!([
        {"type":"report_edited","track_id":"t","body_sha256":"h","body":"b","blocks_after":[false,{},["b",1]]},
        {"type":"ask_answered","track_id":"t","answers":[false,{},["q","a"]]},
        {"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","failed_checks":[false,{}, {"name":"n","id":"i"}]}
    ]);
    for (slot, field, good) in [
        (0, "blocks_after", json!(["b", 1])),
        (1, "answers", json!(["q", "a"])),
        (2, "failed_checks", json!({"name":"n","id":"i"})),
    ] {
        let path = format!("/pending_queue/{slot}/{field}");
        assert_case(
            &v,
            &[
                (&format!("{path}/0"), FactReason::Malformed),
                (&format!("{path}/1"), FactReason::Malformed),
            ],
        )
        .await;
        assert_stored(&v, &format!("{path}/2"), good).await;
    }
}

#[tokio::test]
async fn u13_p1_facts_observation_default_and_optional_presence() {
    let observations = super::observation_tests::observations();
    for (index, fields) in [
        (
            3,
            "presentation author body_before doc_rev_after blocks_after",
        ),
        (4, "result_contract"),
        (6, "idempotency_key"),
        (7, "failing_step exit_code status_detail target"),
        (8, "retained_path"),
        (
            15,
            "snapshot failed_checks snapshot/all_checks_completed failed_checks/0/diagnostics",
        ),
    ] {
        for field in fields.split_ascii_whitespace() {
            for null in [false, true] {
                let mut v = original();
                v["pending_queue"] = json!([observations[index]]);
                let path = format!("/pending_queue/0/{field}");
                replace(&mut v, &path, null.then_some(Value::Null));
                let fixture = fixture(&v.to_string()).await;
                let audit = fixture.audit().await;
                let facts = classify_facts(&audit).unwrap();
                super::node_oracle::assert_all(&fixture, &facts).await;
                let w = &facts.workers()[0];
                assert!(w.fields.iter().any(|f| at(f, &path)
                    && f.interpretation
                        == Interpretation::Presence {
                            required: false,
                            presence: if null {
                                Presence::Null
                            } else {
                                Presence::Missing
                            }
                        }));
                assert!(blocked(
                    w,
                    &path,
                    if null {
                        FactReason::Null
                    } else {
                        FactReason::Missing
                    }
                ));
            }
        }
    }
}
