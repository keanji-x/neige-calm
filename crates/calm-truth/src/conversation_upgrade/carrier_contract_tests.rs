use super::{classification_tests::*, *};
use serde_json::{Value, json};

#[tokio::test]
async fn u13_p1_facts_carrier_issued_object_shape() {
    for (value, reason) in [
        (Some(json!(false)), Some(FactReason::Malformed)),
        (Some(json!(7)), Some(FactReason::Malformed)),
        (Some(json!("wrong")), Some(FactReason::Malformed)),
        (Some(json!([])), Some(FactReason::Malformed)),
        (None, Some(FactReason::Missing)),
        (Some(Value::Null), Some(FactReason::Null)),
        (Some(json!({"turn_id":"t","segments":[]})), None),
    ] {
        let mut v = original();
        if let Some(value) = value {
            v["issued_input_segments"] = value;
        } else {
            v.as_object_mut().unwrap().remove("issued_input_segments");
        }
        v["pending_envelope_ids"] = json!([42]);
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        let w = &facts.workers()[0];
        if let Some(reason) = reason {
            assert!(blocked(w, "/issued_input_segments", reason));
        }
        assert_eq!(
            w.fields.iter().any(|f| at(f, "/issued_input_segments")
                && f.interpretation == Interpretation::Stored(v["issued_input_segments"].clone())),
            v["issued_input_segments"].is_object()
        );
        assert!(w.fields.iter().any(|f| at(f, "/pending_envelope_ids/0")
            && f.interpretation == Interpretation::Stored(json!(42))));
        assert!(matches!(w.frame, Some(FrameFacts::LegacyV1)));
    }
}

#[tokio::test]
async fn u13_p1_facts_carrier_observation_ledger_deferred() {
    let mut v = original();
    v["pending_queue"] = json!([
        {"type":"forge_scan_completed","track_id":"t","overlapping_prs":[false,"bad",3]},
        {"type":"task_completed","idempotency_key":"k","result":{"opaque":[null,true]}}
    ]);
    let fixture = fixture(&v.to_string()).await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    let w = &facts.workers()[0];
    for node in facts.sources().nodes() {
        if let SourceLocation::Cell { json: Some(a), .. } = &node.source.location {
            let p = a.pointer();
            if p.starts_with("/pending_queue/0") || p.starts_with("/pending_queue/1") {
                assert!(w.fields.iter().any(|f| f.source == node.source
                    && f.expected_path.is_none()
                    && f.interpretation
                        == Interpretation::Blocked(FactReason::DeferredObservationInterpretation)));
            }
        }
    }
    assert!(!w.fields.iter().any(|f| matches!(&f.source.location,
        SourceLocation::Cell { json: Some(a), .. } if a.pointer().starts_with("/pending_queue/"))
        && matches!(f.interpretation, Interpretation::Stored(_))));
    assert!(blocked(w, "/pending_queue/0", FactReason::Malformed));
    assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
}

// Permanent source guard: payload interpretation cannot discard sources when a
// duplicate prevents the frame from entering any semantic decoder.
#[tokio::test]
async fn u13_p1_facts_carrier_duplicate_observation_sources_before_early_return() {
    for version in [1, 2, 3, 4, 5, 99] {
        let raw = format!(
            r#"{{"schema_version":{version},"pending_queue":[{{"type":"track_goal","text":"first","text":{{"opaque":[null,7]}}}}],"canonical_inputs":{{"pending":[{{"kind":"system","observation":{{"type":"track_goal","text":"original"}}}}]}}}}"#
        );
        let fixture = fixture(&raw).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        let w = &facts.workers()[0];
        assert!(w.frame.is_none());
        assert_eq!(facts.sources().duplicates().len(), 1);
        let mut observations = 0;
        for node in facts.sources().nodes() {
            if let SourceLocation::Cell { json: Some(a), .. } = &node.source.location {
                let p = a.pointer();
                if p.starts_with("/pending_queue/0")
                    || p.starts_with("/canonical_inputs/pending/0/observation")
                {
                    observations += 1;
                    assert!(w.fields.iter().any(|f| f.source == node.source
                        && f.expected_path.is_none()
                        && f.interpretation
                            == Interpretation::Blocked(
                                FactReason::DeferredObservationInterpretation
                            )));
                }
            }
        }
        assert_eq!(observations, 10);
        assert!(
            !w.fields
                .iter()
                .any(|f| matches!(f.interpretation, Interpretation::Stored(_))
                    && matches!(
                        &f.source.location,
                        SourceLocation::Cell { json: Some(_), .. }
                    ))
        );
        assert!(
            w.fields
                .iter()
                .any(|f| f.interpretation == Interpretation::Stored(json!("running")))
        );
    }
}

async fn checked_workers(fixture: super::raw_tests::Fixture) -> Vec<WorkerFacts> {
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    facts.workers
}
async fn carrier_workers(v: &Value) -> Vec<WorkerFacts> {
    checked_workers(fixture(&v.to_string()).await).await
}
fn stored(w: &WorkerFacts, path: &str, value: Value) -> bool {
    w.fields.iter().any(|f| {
        at(f, path)
            && f.expected_path.is_none()
            && f.interpretation == Interpretation::Stored(value.clone())
    })
}

#[tokio::test]
async fn u13_p1_facts_carrier_struct_sequence_siblings() {
    let mut v = original();
    v["token_usage"] = json!([false, false, null, false]);
    v["last_turn_base"] = json!(["healthy", "head"]);
    let workers = carrier_workers(&v).await;
    let w = &workers[0];
    for index in [0, 1, 3] {
        assert!(blocked(
            w,
            &format!("/token_usage/{index}"),
            FactReason::Malformed
        ));
    }
}

// Every fixture enters OfflineSnapshot; the independent byte/node oracle checks
// the original SQL cells, array indices, ancestry and every emitted SourceRef.
#[tokio::test]
async fn u13_p1_facts_carrier_sequence_matrix() {
    let image = "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png";
    let mut base = original();
    base["token_usage"] = json!({"used_tokens":1,"total_tokens":2,"context_window":null,"at_ms":3});
    base["last_turn_base"] = json!(["healthy", "head"]);
    base["turn_budget"] = json!(["planner", 15, "attempt", false, ["text", "context"]]);
    base["issued_input_segments"] = json!({"turn_id":"turn","segments":[
        ["user","text",[[image,"image/png",1,"/image"]],["user_message",9,["entry",1]]],
        {"presentation":"user","text":"healthy object","attachments":[]},
        ["user","healthy sequence",[],null]
    ]});
    base["pending_entry_meta"] =
        json!([{"id":"entry","rev":1,"queued_at_ms":2,"attachments":[[image,1,"/image"]]}]);
    let provider = json!([1,"execution",{"Turn":{"turn_id":"turn"}},{"Released":{"authorized_at_ms":7}},{"Stopping":{"reason":"Finished","at_ms":8}}]);
    // Values and expected indices are explicit wire examples, independent of
    // the production field-address helper and public owner serialization.
    for (path, good, bad, bad_leaves) in [
        (
            "/pending_entry_meta/0",
            json!(["entry", 3, 4, []]),
            json!([false, false, false, [[false, false, false]]]),
            "0 1 2 3/0/0 3/0/1 3/0/2",
        ),
        (
            "/token_usage",
            json!([1, 2, null, 3]),
            json!([false, false, null, false]),
            "0 1 3",
        ),
        (
            "/compact_outcome",
            json!(["op", "thread", null, "completed"]),
            json!([false, false, null, false]),
            "0 1 3",
        ),
        (
            "/last_turn_base",
            json!(["turn", "head"]),
            json!([false, false]),
            "0 1",
        ),
        (
            "/turn_budget",
            base["turn_budget"].clone(),
            json!(["planner", false, false, false, ["text", false]]),
            "1 2 4/1",
        ),
        (
            "/turn_budget/4",
            json!(["text", "context"]),
            json!(["text", false]),
            "1",
        ),
        (
            "/pending_rewind",
            json!(["codex", "turn", false]),
            json!(["codex", false, false]),
            "1",
        ),
        (
            "/pending_rewind",
            json!(["codex_v2", "thread", "turn", false]),
            json!(["codex_v2", false, false, false]),
            "1 2",
        ),
        (
            "/pending_rewind",
            json!(["claude", uuid::Uuid::nil(), uuid::Uuid::nil()]),
            json!(["claude", false, false]),
            "1 2",
        ),
        (
            "/interruption_intent",
            json!(["turn", "max_turn_duration"]),
            json!([false, false]),
            "0 1",
        ),
        (
            "/interruption_intent",
            json!(["user_stop_v1", "s", "c", "t", "thread", "turn", 7]),
            json!(["user_stop_v1", false, false, false, false, false, false]),
            "1 2 3 4 5 6",
        ),
        (
            "/interruption_intent",
            json!([
                "v1",
                "s",
                "c",
                "t",
                "thread",
                "turn",
                "max_turn_duration",
                7,
                8,
                9
            ]),
            json!([
                "v1", false, false, false, false, false, false, false, false, false
            ]),
            "1 2 3 4 5 6 7 8 9",
        ),
        (
            "/issued_input_segments/segments/0",
            base["issued_input_segments"]["segments"][0].clone(),
            json!([false, false, [], null]),
            "0 1",
        ),
        (
            "/issued_input_segments/segments/0/2/0",
            json!([image, "image/png", 1, "/image"]),
            json!([false, false, false, false]),
            "0 1 2 3",
        ),
        (
            "/issued_input_segments/segments/0/3",
            json!(["user_message", 9, ["entry", 1]]),
            json!([false, false, [false, false]]),
            "0 1 2/0 2/1",
        ),
        (
            "/issued_input_segments/segments/0/3/2",
            json!(["entry", 1]),
            json!([false, false]),
            "0 1",
        ),
        (
            "/pending_entry_meta/0/attachments/0",
            json!([image, 1, "/image"]),
            json!([false, false, false]),
            "0 1 2",
        ),
        (
            "",
            provider.clone(),
            json!([false,false,{"Turn":{"turn_id":false}},{"Released":{"authorized_at_ms":false}},{"Stopping":{"reason":false,"at_ms":false}}]),
            "0 1 2/Turn/turn_id 3/Released/authorized_at_ms 4/Stopping/reason 4/Stopping/at_ms",
        ),
    ] {
        let column = if path.is_empty() {
            "provider_execution_json"
        } else {
            "handle_state_json"
        };
        let mut extra = good.clone();
        extra
            .as_array_mut()
            .unwrap()
            .push(json!({"opaque":[null,7]}));
        let mut missing = good.clone();
        missing.as_array_mut().unwrap().pop();
        for (value, kind) in [
            (bad, "bad"),
            (good.clone(), "good"),
            (Value::Null, "null"),
            (json!([]), "short"),
            (missing, "missing"),
            (extra, "extra"),
        ] {
            let mut frame = base.clone();
            let mut claim = provider.clone();
            let target = if column == "handle_state_json" {
                &mut frame
            } else {
                &mut claim
            };
            if path.is_empty() {
                *target = value.clone();
            } else if path.matches('/').count() == 1 {
                target[&path[1..]] = value.clone();
            } else {
                *target.pointer_mut(path).unwrap() = value.clone();
            }
            let fixture = super::raw_tests::Fixture::new(&format!(
                "{TABLES}{}{} UPDATE worker_sessions SET provider_execution_json={} WHERE id='sequence';",
                worker_row("sequence", "running", &frame.to_string()),
                worker_row("w", "running", &base.to_string()), quoted(&claim.to_string())
            )).await;
            let workers = checked_workers(fixture).await;
            assert_eq!(workers.len(), 2);
            let w = &workers[0];
            let exact = |p: &str, interpretation: Interpretation| {
                w.fields.iter().any(|f| f.expected_path.is_none()
                    && f.interpretation == interpretation
                    && matches!(&f.source.location,
                        SourceLocation::Cell { row, column_name, json: Some(a), .. }
                        if row.raw_key == vec![RawCell::Integer(1)] && column_name == column && a.pointer() == p))
            };
            for field in &w.fields {
                if let (
                    Some(expected),
                    SourceLocation::Cell {
                        column_name,
                        json: Some(a),
                        ..
                    },
                ) = (&field.expected_path, &field.source.location)
                {
                    let raw = if column_name == "handle_state_json" {
                        &frame
                    } else {
                        &claim
                    };
                    let anchor = a.pointer();
                    if raw.pointer(&anchor).is_some_and(Value::is_array) && expected != &anchor {
                        assert!(
                            expected
                                .strip_prefix(&format!("{anchor}/"))
                                .unwrap()
                                .split('/')
                                .next()
                                .unwrap()
                                .parse::<usize>()
                                .is_ok(),
                            "{expected}"
                        );
                    }
                }
            }

            match kind {
                "good" => {
                    if path == "/pending_entry_meta/0" {
                        assert!(!blocked(w, path, FactReason::Malformed));
                    } else {
                        assert!(
                            exact(path, Interpretation::Stored(good.clone())),
                            "{column}:{path}"
                        );
                    }
                    for (i, leaf) in good.as_array().unwrap().iter().enumerate() {
                        if !leaf.is_array() && !leaf.is_object() && !leaf.is_null() {
                            assert!(
                                exact(&format!("{path}/{i}"), Interpretation::Stored(leaf.clone())),
                                "{path}/{i}"
                            );
                        }
                    }
                }
                "bad" => {
                    for leaf in bad_leaves.split_ascii_whitespace() {
                        assert!(
                            exact(
                                &format!("{path}/{leaf}"),
                                Interpretation::Blocked(FactReason::Malformed)
                            ),
                            "{path}/{leaf}"
                        );
                    }
                }
                "null" => assert!(exact(path, Interpretation::Blocked(FactReason::Null))),
                "short" | "missing" => {
                    let index = if kind == "short" {
                        0
                    } else {
                        good.as_array().unwrap().len() - 1
                    };
                    assert!(w.fields.iter().any(|f| f.expected_path.as_deref() == Some(&format!("{path}/{index}"))
                        && matches!(&f.source.location, SourceLocation::Cell { column_name, json: Some(a), .. } if column_name == column && a.pointer() == path)
                        && f.interpretation == Interpretation::Blocked(FactReason::Missing)));
                }
                "extra" => assert!(exact(
                    &format!("{path}/{}", good.as_array().unwrap().len()),
                    Interpretation::Blocked(FactReason::UnknownField)
                )),
                _ => unreachable!(),
            }
            assert!(stored(&workers[1], "/token_usage/used_tokens", json!(1)));
            assert!(stored(
                &workers[1],
                "/issued_input_segments/segments/1/text",
                json!("healthy object")
            ));
            assert!(stored(
                w,
                "/issued_input_segments/segments/2/1",
                json!("healthy sequence")
            ));
        }
    }
    let intents = ["v1", "user_stop_v1"].map(|turn| ("intent", json!([turn, "max_turn_duration"])));
    let contexts = [
        json!(["render_plan", null]),
        json!(["legacy_not_recorded", null]),
        json!({"kind":"render_plan","text":null}),
        json!({"kind":"legacy_not_recorded","text":null}),
    ]
    .map(|value| ("context", value));
    let budgets = [json!(["legacy"]), json!(["not_applicable"])].map(|value| ("budget", value));
    for (kind, value) in intents.into_iter().chain(contexts).chain(budgets) {
        let mut frame = base.clone();
        let path = match kind {
            "intent" => "/interruption_intent",
            "context" => "/turn_budget/4",
            _ => "/turn_budget",
        };
        *frame.pointer_mut(path).unwrap() = value.clone();
        let workers = carrier_workers(&frame).await;
        let w = &workers[0];
        assert!(stored(w, path, value.clone()));
        match kind {
            "intent" => assert!(!w.fields.iter().any(|f| {
                f.expected_path
                    .as_deref()
                    .is_some_and(|p| p.starts_with("/interruption_intent/"))
                    && f.interpretation == Interpretation::Blocked(FactReason::Missing)
            })),
            "context" => {
                let leaf = if value.is_array() {
                    "/turn_budget/4/1"
                } else {
                    "/turn_budget/4/text"
                };
                assert!(blocked(w, leaf, FactReason::Null));
                assert!(!blocked(w, leaf, FactReason::UnknownField));
            }
            _ => {}
        }
    }
}

#[test]
fn u13_p1_facts_carrier_provider_variant_sequence_contract() {
    use crate::db::sqlite::provider_execution::{Decision, Purpose, Release};
    for v in [json!({"Turn":["turn"]}), json!({"Compaction":["turn"]})] {
        assert!(serde_path_to_error::deserialize::<_, Purpose>(&v).is_err());
    }
    for v in [json!({"Releasing":[7]}), json!({"Released":[7]})] {
        assert!(serde_path_to_error::deserialize::<_, Release>(&v).is_err());
    }
    assert!(
        serde_path_to_error::deserialize::<_, Decision>(&json!({"Stopping":["Finished",8]}))
            .is_err()
    );
}

#[tokio::test]
async fn u13_p1_facts_carrier_meta_mixed_duplicate_ids() {
    let image = "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png";
    let mut frame = original();
    frame["pending_entry_meta"] = json!([
        ["dup",3,4,[[image,1,"/image"]]], ["dup",3,4,[]],
        {"id":"dup","rev":3,"queued_at_ms":4,"attachments":[]}
    ]);
    let workers = carrier_workers(&frame).await;
    let w = &workers[0];
    for path in [
        "/pending_entry_meta/0/0",
        "/pending_entry_meta/1/0",
        "/pending_entry_meta/2/id",
    ] {
        assert!(blocked(w, path, FactReason::DuplicateIdentity), "{path}");
    }
    for (index, value) in [json!(image), json!(1), json!("/image")]
        .into_iter()
        .enumerate()
    {
        let path = format!("/pending_entry_meta/0/3/0/{index}");
        assert!(stored(w, &path, value.clone()));
    }
}

#[tokio::test]
async fn u13_p1_facts_carrier_ambiguous_intent_source() {
    use calm_types::harness::{HarnessInterruptionIntent, HarnessLegacyInterruptionIntent};
    for tag in ["v1", "user_stop_v1"] {
        for value in [
            json!([tag, "future_reason"]),
            json!([tag, null]),
            json!([tag, false]),
            json!([tag]),
            json!([tag, {"raw":[false,null]}]),
        ] {
            assert!(
                serde_path_to_error::deserialize::<_, HarnessInterruptionIntent>(&value).is_err()
            );
            assert!(
                serde_path_to_error::deserialize::<_, HarnessLegacyInterruptionIntent>(&value)
                    .is_err()
            );
            let mut v = original();
            v["interruption_intent"] = value.clone();
            v["push_watermark"] = json!(42);
            v["token_usage"] = json!([false, 2, null, false]);
            let workers = carrier_workers(&v).await;
            let w = &workers[0];
            for field in &w.fields {
                if let SourceLocation::Cell { json: Some(a), .. } = &field.source.location
                    && a.pointer().starts_with("/interruption_intent")
                {
                    assert!(!matches!(field.interpretation, Interpretation::Stored(_)));
                    assert!(w.fields.iter().any(|f| f.source == field.source
                        && f.expected_path.is_none()
                        && f.interpretation
                            == Interpretation::Blocked(FactReason::AmbiguousRepresentation)));
                    assert!(
                        field
                            .expected_path
                            .as_ref()
                            .is_none_or(|_| field.interpretation
                                != Interpretation::Blocked(FactReason::Missing))
                    );
                }
            }
            for i in 0..value.as_array().unwrap().len() {
                let path = format!("/interruption_intent/{i}");
                assert!(w.fields.iter().any(|f| at(f, &path)
                    && f.expected_path.is_none()
                    && f.interpretation
                        == Interpretation::Blocked(FactReason::AmbiguousRepresentation)));
            }
            for (path, value) in [("/push_watermark", json!(42)), ("/token_usage/1", json!(2))] {
                assert!(stored(w, path, value.clone()));
            }
            for path in ["/token_usage/0", "/token_usage/3"] {
                assert!(blocked(w, path, FactReason::Malformed));
            }
        }
        // Success of the public untagged owner, rather than tag text, proves
        // these colliding slots are the Legacy turn id and reason.
        let mut v = original();
        v["interruption_intent"] = json!([tag, "max_turn_duration"]);
        assert!(matches!(
            serde_path_to_error::deserialize::<_, HarnessInterruptionIntent>(
                &v["interruption_intent"]
            ),
            Ok(HarnessInterruptionIntent::Legacy(_))
        ));
        let workers = carrier_workers(&v).await;
        assert!(stored(
            &workers[0],
            "/interruption_intent/1",
            json!("max_turn_duration")
        ));
    }
    for (value, reason, expected) in [
        (
            Some(json!([])),
            FactReason::Missing,
            "/interruption_intent/0",
        ),
        (None, FactReason::Missing, "/interruption_intent"),
        (Some(Value::Null), FactReason::Null, "/interruption_intent"),
        (
            Some(json!(["turn", "future_reason"])),
            FactReason::Malformed,
            "/interruption_intent/1",
        ),
    ] {
        let mut v = original();
        super::core_matrix_tests::replace(&mut v, "/interruption_intent", value);
        super::core_matrix_tests::assert_case(&v, &[(expected, reason)]).await;
    }
}

#[tokio::test]
async fn u13_p1_facts_carrier_unknown_context_content() {
    for value in [
        json!(["future", "opaque"]),
        json!(["future", {"raw":[null,7]}]),
        json!({"kind":"future","text":"opaque"}),
        json!(["future"]),
        json!({"kind":"future"}),
    ] {
        let mut v = original();
        v["turn_budget"] = json!(["planner", 15, "attempt", false, value]);
        let workers = carrier_workers(&v).await;
        let w = &workers[0];
        let content = if value.is_array() {
            "/turn_budget/4/1"
        } else {
            "/turn_budget/4/text"
        };
        if value.get(1).is_some() || value.get("text").is_some() {
            assert!(blocked(w, content, FactReason::AmbiguousRepresentation));
            assert!(
                !w.fields.iter().any(
                    |f| at(f, content) && matches!(f.interpretation, Interpretation::Stored(_))
                )
            );
        } else {
            assert!(!blocked(w, content, FactReason::Missing));
        }
        assert!(stored(w, "/turn_budget/1", json!(15)));
        assert!(stored(w, "/turn_budget/2", json!("attempt")));
    }
}

#[tokio::test]
async fn u13_p1_facts_carrier_issued_root_representations() {
    let healthy = json!(["user", "healthy", [], null]);
    let attachment = json!([false, false, false, false]);
    let origin = json!([false, false, [false, false]]);
    let bad_segment = json!([false, false, [attachment], origin]);
    for (root, suffix, bad, accepted) in [
        (json!(["turn", [healthy]]), "1/0/1", "", true),
        (
            json!({"turn_id":"turn","segments":[healthy]}),
            "segments/0/1",
            "",
            true,
        ),
        (
            json!([false, [bad_segment, healthy]]),
            "1/1/1",
            "0 1/0/0 1/0/1 1/0/2/0/0 1/0/2/0/1 1/0/2/0/2 1/0/2/0/3 1/0/3/0 1/0/3/1 1/0/3/2/0 1/0/3/2/1",
            false,
        ),
        (json!(["turn", [healthy], false]), "1/0/1", "", false),
    ] {
        let mut v = original();
        v["issued_input_segments"] = root.clone();
        let workers = carrier_workers(&v).await;
        let w = &workers[0];
        assert!(stored(
            w,
            &format!("/issued_input_segments/{suffix}"),
            json!("healthy")
        ));
        assert_eq!(stored(w, "/issued_input_segments", root), accepted);
        for suffix in bad.split_ascii_whitespace() {
            assert!(blocked(
                w,
                &format!("/issued_input_segments/{suffix}"),
                FactReason::Malformed
            ));
        }
        if !accepted {
            assert!(blocked(w, "/issued_input_segments", FactReason::Malformed));
        }
    }
    for (root, suffix, reason) in [
        (json!([]), "0", FactReason::Missing),
        (json!(["turn"]), "1", FactReason::Missing),
        (json!({"turn_id":"turn"}), "segments", FactReason::Missing),
        (json!([null, null]), "0", FactReason::Null),
        (json!(["turn", false]), "1", FactReason::Malformed),
        (json!(["turn", null]), "1", FactReason::Null),
        (json!(["turn", [], false]), "2", FactReason::UnknownField),
    ] {
        let mut v = original();
        v["issued_input_segments"] = root.clone();
        super::core_matrix_tests::assert_case(
            &v,
            &[
                ("/issued_input_segments", FactReason::Malformed),
                (&format!("/issued_input_segments/{suffix}"), reason),
            ],
        )
        .await;
        let workers = carrier_workers(&v).await;
        assert!(!stored(&workers[0], "/issued_input_segments", root));
    }
}
