use super::{
    classification_tests::*,
    core_matrix_tests::{assert_case, fields, provider, provider_case, replace},
    raw_tests::Fixture,
    *,
};
use serde_json::{Value, json};

pub(super) fn observations() -> Vec<Value> {
    vec![
        json!({"type":"track_goal","text":"t"}),
        json!({"type":"system_context","text":"t"}),
        json!({"type":"user_message","text":"t"}),
        json!({"type":"report_edited","track_id":"t","body_sha256":"h","body":"b","presentation":{"version":"legacy"},"author":null,"body_before":"before","doc_rev_after":1,"blocks_after":[]}),
        json!({"type":"task_completed","idempotency_key":"k","result":{"opaque":{"future":true}},"result_contract":"r"}),
        json!({"type":"task_failed","idempotency_key":"k","error":"e"}),
        json!({"type":"worker_hook_stop","track_id":"t","card_id":"c","kind":"codex_stop","idempotency_key":"k"}),
        json!({"type":"task_gate_result","idempotency_key":"k","key":"k","passed":true,"log_tail":"l","attempt":1,"failing_step":"f","exit_code":1,"status_detail":"s","target":null}),
        json!({"type":"task_git_delivery_settled","key":"k","attempt_id":"a","result":{"kind":"failed","code":"unresolved","reason":"r","retry_allowed":false},"retained_path":"p"}),
        json!({"type":"track_wake","source":"s","key":"k","text":"t"}),
        json!({"type":"workspace_leased","track_id":"t","card_id":"c","lease_id":"l","path":"p"}),
        json!({"type":"workspace_released","track_id":"t","card_id":"c","lease_id":"l"}),
        json!({"type":"forge_pr_merged","track_id":"t","pr_number":1}),
        json!({"type":"forge_pr_opened","track_id":"t","pr_number":1}),
        json!({"type":"forge_scan_completed","track_id":"t","overlapping_prs":[]}),
        json!({"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","snapshot":{"head_sha":"h","mergeable":"m","all_checks_completed":true},"failed_checks":[{"name":"n","url":"u","diagnostics":{"status":"unavailable","reason":"r"}}]}),
        json!({"type":"forge_issue_closed","track_id":"t","issue_number":1}),
        json!({"type":"worktree_provisioned","track_id":"t","card_id":"c","path":"p"}),
        json!({"type":"worktree_committed","track_id":"t","card_id":"c","commit_sha":"h","branch":"b"}),
        json!({"type":"ask_answered","track_id":"t","answers":[{"title":"q","answer":"a"}]}),
    ]
}

#[tokio::test]
async fn u13_p1_facts_issued_root_shape_regression() {
    let mut actual = vec![];
    for issued in [
        Some(json!(false)),
        Some(json!(7)),
        Some(json!("wrong")),
        Some(json!([])),
        None,
        Some(Value::Null),
        Some(json!({"turn_id":"t","segments":[]})),
    ] {
        let mut v = original();
        if let Some(issued) = issued {
            v["issued_input_segments"] = issued;
        } else {
            v.as_object_mut().unwrap().remove("issued_input_segments");
        }
        v["pending_queue"] = json!([{"type":"track_goal","text":"healthy"}]);
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        let w = &facts.workers()[0];
        let stored = |path: &str, value: Value| {
            w.fields
                .iter()
                .any(|f| at(f, path) && f.interpretation == Interpretation::Stored(value.clone()))
        };
        assert!(stored("/pending_queue/0/text", json!("healthy")));
        actual.push((
            blocked(w, "/issued_input_segments", FactReason::Malformed),
            stored("/issued_input_segments", v["issued_input_segments"].clone()),
        ));
    }
    assert_eq!(
        actual,
        vec![
            (true, false),
            (true, false),
            (true, false),
            (true, false),
            (false, false),
            (false, false),
            (false, true)
        ]
    );
}

#[tokio::test]
async fn u13_p1_facts_observation_aggregate_siblings_regression() {
    let mut v = original();
    v["pending_queue"] = json!([
        {"type":"forge_scan_completed","track_id":"t","overlapping_prs":[false,"bad",3]},
        {"type":"task_git_delivery_settled","key":"k","attempt_id":"a","result":{
            "kind":"candidate","candidate_id":false,"commit_sha":false,"base_sha":"healthy","base_is_ancestor":"bad"}},
        {"type":"task_gate_result","idempotency_key":"k","key":"k","passed":true,"log_tail":"l","attempt":1,"target":{
            "kind":"candidate","candidate_id":false,"commit_sha":false,"lease_id":"healthy","evidence":{"kind":"unsampled","phase":{"kind":"prepare","reason":"r"}}}}
    ]);
    let fixture = fixture(&v.to_string()).await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    let w = &facts.workers()[0];
    let malformed = [
        "/pending_queue/0/overlapping_prs/0",
        "/pending_queue/0/overlapping_prs/1",
        "/pending_queue/1/result/candidate_id",
        "/pending_queue/1/result/commit_sha",
        "/pending_queue/1/result/base_is_ancestor",
        "/pending_queue/2/target/candidate_id",
        "/pending_queue/2/target/commit_sha",
    ]
    .map(|p| blocked(w, p, FactReason::Malformed));
    let stored = [
        ("/pending_queue/0/overlapping_prs/2", json!(3)),
        ("/pending_queue/1/result/base_sha", json!("healthy")),
        ("/pending_queue/2/target/lease_id", json!("healthy")),
    ]
    .map(|(p, value)| {
        w.fields
            .iter()
            .any(|f| at(f, p) && f.interpretation == Interpretation::Stored(value.clone()))
    });
    assert_eq!((malformed, stored), ([true; 7], [true; 3]));
}

#[tokio::test]
async fn u13_p1_facts_nested_presence_matrix() {
    let mut base = original();
    for (path, value) in [
        (
            "/turn_budget",
            json!({"kind":"planner","budget":15,"start_attempt":"a","warning_attempted":false,"context_usage":{"kind":"text","text":"frozen"}}),
        ),
        (
            "/token_usage",
            json!({"used_tokens":1,"total_tokens":2,"context_window":null,"at_ms":3}),
        ),
        (
            "/compact_outcome",
            json!({"operation_id":"o","thread_id":"t","turn_id":null,"status":"completed"}),
        ),
        ("/last_turn_base", json!({"turn_id":"t","seen_head":null})),
        (
            "/pending_rewind",
            json!({"provider":"codex","before_turn_id":"t","reverted":false}),
        ),
        (
            "/interruption_intent",
            json!({"version":"user_stop_v1","session_id":"w","card_id":"card","track_id":"track","thread_id":"t","turn_id":"turn","requested_at_ms":1}),
        ),
        (
            "/issued_input_segments",
            json!({"turn_id":"t","segments":[{"presentation":"system_report_edited","text":"old","attachments":[{"id":"0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png","contentType":"image/png","size":1,"url":"u"}],"origin":{"observation":"user_message","event_id":null,"queue_entry":{"id":"q","rev":1}}}]}),
        ),
        (
            "/pending_entry_meta",
            json!([{"id":"q","rev":1,"queued_at_ms":1,"attachments":[{"id":"0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png","size":1,"path":"original"}]}]),
        ),
    ] {
        base[path.strip_prefix('/').unwrap()] = value;
    }
    base["pending_queue"] = json!([
        {"type":"report_edited","track_id":"t","body_sha256":"h","body":"b","presentation":null,"author":null,"body_before":null,"doc_rev_after":null,"blocks_after":null},
        {"type":"task_completed","idempotency_key":"k","result":{"opaque":true},"result_contract":null},
        {"type":"worker_hook_stop","track_id":"t","card_id":"c","kind":"codex_stop","idempotency_key":"k"},
        {"type":"task_gate_result","idempotency_key":"k","key":"k","passed":true,"log_tail":"l","attempt":1,"failing_step":null,"exit_code":null,"status_detail":null,"target":null},
        {"type":"task_git_delivery_settled","key":"k","attempt_id":"a","result":{"kind":"failed","code":"unresolved","reason":"r","retry_allowed":false},"retained_path":null},
        {"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","snapshot":{"head_sha":"h","mergeable":"m","all_checks_completed":null},"failed_checks":[{"name":"n","url":"u","diagnostics":null}]}
    ]);
    for parent in "/turn_budget /turn_budget/context_usage /token_usage /compact_outcome /last_turn_base /pending_rewind /interruption_intent /issued_input_segments /issued_input_segments/segments/0 /issued_input_segments/segments/0/attachments/0 /issued_input_segments/segments/0/origin /issued_input_segments/segments/0/origin/queue_entry /pending_entry_meta/0/attachments/0 /pending_queue/0 /pending_queue/1 /pending_queue/2 /pending_queue/3 /pending_queue/4 /pending_queue/5 /pending_queue/5/snapshot /pending_queue/5/failed_checks/0".split_ascii_whitespace() {
        for field in base.pointer(parent).unwrap().as_object().unwrap().keys().filter(|k| !matches!(k.as_str(), "provider" | "version" | "type") && (k.as_str() != "kind" || parent.starts_with("/pending_queue"))) {
            for null in [false, true] {
                let mut v = base.clone();
                let obj = v.pointer_mut(parent).unwrap().as_object_mut().unwrap();
                if null { obj[field] = Value::Null; } else { obj.remove(field); }
                let audit = frame_audit(&v).await;
                let facts = classify_facts(&audit).unwrap();
                let path = if parent.ends_with("failed_checks/0") && field == "url" && !null {
                    assert!(blocked(&facts.workers()[0], parent, FactReason::OwnerDiagnosticNotLeaf));
                    format!("{parent}/id")
                } else { format!("{parent}/{field}") };
                assert!(blocked(&facts.workers()[0], &path, if null { FactReason::Null } else { FactReason::Missing }), "{path}");
                let field = facts.workers()[0].fields.iter().find(|f| at(f, &path)).unwrap();
                assert!(facts.sources().nodes().iter().any(|n| n.source == field.source));
            }
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_precise_leaf_and_observation_presence() {
    let mut v = original();
    v["token_usage"] = json!({"used_tokens":1,"total_tokens":2,"at_ms":"bad"});
    v["pending_queue"] =
        json!([{"type":"report_edited","track_id":"t","body_sha256":"h","body":"b"}]);
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    let w = &facts.workers()[0];
    assert!(blocked(w, "/token_usage/at_ms", FactReason::Malformed));
    assert!(blocked(w, "/pending_queue/0/author", FactReason::Missing));
}

#[tokio::test]
async fn u13_p1_facts_simultaneous_issued_and_snapshot_leaves() {
    let mut v = original();
    v["issued_input_segments"] = json!({"turn_id":"t","segments":[{"presentation":"bad","text":false,"attachments":[{"id":"bad","contentType":false,"size":"bad","url":false}],"origin":{"observation":false,"event_id":"bad","queue_entry":{"id":false,"rev":"bad"}}}]});
    v["pending_queue"] = json!([{"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","snapshot":{"head_sha":false,"mergeable":false,"all_checks_completed":"bad"},"failed_checks":[]}]);
    let audit = frame_audit(&v).await;
    let facts = classify_facts(&audit).unwrap();
    for path in [
        "/issued_input_segments/segments/0/text",
        "/issued_input_segments/segments/0/attachments/0/size",
        "/issued_input_segments/segments/0/attachments/0/url",
        "/issued_input_segments/segments/0/origin/event_id",
        "/issued_input_segments/segments/0/origin/queue_entry/rev",
        "/pending_queue/0/snapshot/mergeable",
        "/pending_queue/0/snapshot/all_checks_completed",
    ] {
        assert!(
            blocked(&facts.workers()[0], path, FactReason::Malformed),
            "{path}"
        );
    }
}

#[tokio::test]
async fn u13_p1_facts_observation_full_variant_matrix() {
    for observation in observations() {
        assert!(
            serde_json::from_value::<calm_types::observation::Observation>(observation.clone())
                .is_ok()
        );
        let mut v = original();
        v["pending_queue"] = json!([observation.clone(), {"type":"track_goal","text":"neighbor"}]);
        fields(
            &v,
            "/pending_queue/0",
            if observation["type"] == "task_completed" {
                &["result"]
            } else {
                &[]
            },
        )
        .await;
        for (bad, reason) in [
            (None, FactReason::Missing),
            (Some(Value::Null), FactReason::Null),
            (Some(json!(false)), FactReason::Malformed),
            (Some(json!("future")), FactReason::MissingOwnerDecoder),
        ] {
            let mut bad_frame = v.clone();
            replace(&mut bad_frame, "/pending_queue/0/type", bad);
            assert_case(&bad_frame, &[("/pending_queue/0/type", reason)]).await;
        }
        // All independently typed leaves fail together, beside a valid slot.
        let mut paths = vec![];
        for (name, value) in observation.as_object().unwrap() {
            if name == "type" || (name == "result" && observation["type"] == "task_completed") {
                continue;
            }
            let path = format!("/pending_queue/0/{name}");
            replace(
                &mut v,
                &path,
                Some(if value.is_boolean() {
                    json!("bad")
                } else {
                    json!(false)
                }),
            );
            paths.push(path);
        }
        let expected: Vec<_> = paths
            .iter()
            .map(|p| (p.as_str(), FactReason::Malformed))
            .collect();
        assert_case(&v, &expected).await;
        let audit = frame_audit(&v).await;
        let facts = classify_facts(&audit).unwrap();
        assert!(
            facts.workers()[0]
                .fields
                .iter()
                .any(|f| at(f, "/pending_queue/1/text")
                    && f.interpretation == Interpretation::Stored(json!("neighbor")))
        );
    }
    let mut v = original();
    v["pending_queue"] = json!([observations().remove(15)]);
    for parent in [
        "/pending_queue/0/snapshot",
        "/pending_queue/0/failed_checks/0",
    ] {
        // url is an untagged alternative: missing url selects missing id.
        fields(&v, parent, &[]).await;
    }
    v["pending_queue"][0]["failed_checks"][0]["diagnostics"] = json!({"status":"available","failed_tests":["test"],"failed_steps":[],"error_summary":"e","log_url":"u","truncated":false});
    fields(&v, "/pending_queue/0/failed_checks/0/diagnostics", &[]).await;
    let mut opaque = original();
    opaque["pending_queue"] =
        json!([{"type":"task_completed","idempotency_key":"k","result":{"future":{"leaf":false}}}]);
    assert_case(
        &opaque,
        &[(
            "/pending_queue/0/result/future/leaf",
            FactReason::Unclassified,
        )],
    )
    .await;
}

#[tokio::test]
async fn u13_p1_facts_system_observation_uses_same_domain_projection() {
    for observation in observations() {
        let mut v = original();
        v["pending_queue"] = json!([observation.clone()]);
        let fixture = fixture(&v.to_string()).await;
        let audit = fixture.audit().await;
        let mut facts = classify_facts(&audit).unwrap();
        let w = &mut facts.workers[0];
        let cell = w.fields.iter().find(|f| matches!(&f.source.location,
            SourceLocation::Cell { column_name, json: None, .. } if column_name == "handle_state_json")).unwrap().source.clone();
        let accepted =
            serde_json::from_value::<calm_types::conversation::SystemObservation>(observation)
                .is_ok();
        let before = w.fields.len();
        super::observation_projection::observation(
            &mut super::classify::FrameView {
                worker: w,
                cell,
                value: &v,
                harness_sequence: false,
            },
            "/pending_queue/0",
            true,
        )
        .unwrap();
        assert_eq!(
            w.fields[before..].iter().any(|f| at(f, "/pending_queue/0")
                && f.interpretation == Interpretation::Blocked(FactReason::Malformed)),
            !accepted
        );
        super::node_oracle::assert_all(&fixture, &facts).await;
    }
}

#[tokio::test]
async fn u13_p1_facts_observation_nested_and_duplicate_variant_matrix() {
    for observation in observations() {
        let field = observation
            .as_object()
            .unwrap()
            .keys()
            .find(|k| k.as_str() != "type")
            .unwrap();
        let encoded = serde_json::to_string(&observation).unwrap();
        let raw = format!(
            "{{{}:{},{}",
            serde_json::to_string(field).unwrap(),
            serde_json::to_string(&observation[field]).unwrap(),
            &encoded[1..]
        );
        let fixture = Fixture::new(&format!(
            "{TABLES}{}{}",
            worker_row(
                "duplicate",
                "closed",
                &format!("{{\"schema_version\":1,\"pending_queue\":[{raw}]}}")
            ),
            worker_row("neighbor", "idle", &original().to_string())
        ))
        .await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        assert!(
            facts.workers()[0]
                .fields
                .iter()
                .any(|f| f.interpretation == Interpretation::Blocked(FactReason::DuplicateMember))
        );
        assert!(matches!(
            facts.workers()[1].frame,
            Some(FrameFacts::LegacyV1)
        ));
    }
    let mut v = original();
    v["pending_queue"] = json!([
        {"type":"report_edited","track_id":"t","body_sha256":"h","body":"b","presentation":{"version":"knowledge","knowledge_text":"k"},"blocks_after":[{"id":"b","rev":1}]},
        {"type":"ask_answered","track_id":"t","answers":[{"title":"q","answer":"a"}]},
        {"type":"forge_pr_checks","track_id":"t","pr_number":1,"conclusion":"c","failed_checks":[{"name":"n","id":"i","diagnostics":{"status":"unavailable","reason":"r"}}]}
    ]);
    for parent in [
        "/pending_queue/0/presentation",
        "/pending_queue/0/blocks_after/0",
        "/pending_queue/1/answers/0",
        "/pending_queue/2/failed_checks/0",
        "/pending_queue/2/failed_checks/0/diagnostics",
    ] {
        fields(&v, parent, &[]).await;
    }
    for (bad, reason) in [
        (None, FactReason::Missing),
        (Some(Value::Null), FactReason::Null),
        (Some(json!(false)), FactReason::Malformed),
    ] {
        let mut changed = v.clone();
        replace(
            &mut changed,
            "/pending_queue/2/failed_checks/0/diagnostics/status",
            bad,
        );
        assert_case(
            &changed,
            &[(
                "/pending_queue/2/failed_checks/0/diagnostics/status",
                reason,
            )],
        )
        .await;
    }
    let mut unknown = v.clone();
    unknown["pending_queue"][2]["failed_checks"][0]["diagnostics"]["status"] = json!("future");
    assert_case(
        &unknown,
        &[(
            "/pending_queue/2/failed_checks/0/diagnostics",
            FactReason::Malformed,
        )],
    )
    .await;
    for parent in [
        "/purpose/Compaction",
        "/release/Released",
        "/decision/Stopping",
    ] {
        let mut v = provider();
        v.pointer_mut(parent).unwrap()["future"] = json!({"leaf":1});
        provider_case(
            &quoted(&v.to_string()),
            Some((&format!("{parent}/future"), FactReason::UnknownField)),
        )
        .await;
    }
}
