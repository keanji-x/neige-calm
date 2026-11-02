use super::{classification_tests::*, raw_tests::Fixture, *};
use serde_json::{Value, json};

pub(super) async fn assert_case(v: &Value, paths: &[(&str, FactReason)]) {
    let fixture = fixture(&v.to_string()).await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    let w = &facts.workers()[0];
    for (path, reason) in paths {
        assert!(blocked(w, path, reason.clone()), "{path}: {reason:?}");
        let fact = w
            .fields
            .iter()
            .find(|f| at(f, path) && f.interpretation == Interpretation::Blocked(reason.clone()))
            .unwrap();
        let mut anchor = *path;
        while v.pointer(anchor).is_none() {
            anchor = anchor.rsplit_once('/').map_or("", |(p, _)| p);
        }
        assert_eq!(
            fact.expected_path.as_deref(),
            (anchor != *path).then_some(*path)
        );
        assert!(matches!(&fact.source.location,
            SourceLocation::Cell { column_name, json: Some(a), .. }
                if column_name == "handle_state_json" && a.pointer() == anchor));
    }
    assert_eq!(facts.semantic_status(), "BlockedAudit/NeedsQualification");
}
pub(super) fn replace(v: &mut Value, path: &str, bad: Option<Value>) {
    let (parent, name) = path.rsplit_once('/').unwrap();
    let object = v.pointer_mut(parent).unwrap().as_object_mut().unwrap();
    if let Some(bad) = bad {
        object.insert(name.into(), bad);
    } else {
        object.remove(name);
    }
}
pub(super) async fn fields(base: &Value, parent: &str, opaque: &[&str]) {
    for name in base.pointer(parent).unwrap().as_object().unwrap().keys() {
        let path = format!("{parent}/{name}");
        if matches!(name.as_str(), "type" | "version" | "provider" | "status") {
            continue;
        }
        for (bad, reason) in [
            (None, FactReason::Missing),
            (Some(Value::Null), FactReason::Null),
        ] {
            let mut v = base.clone();
            replace(&mut v, &path, bad);
            let expected = if parent.ends_with("failed_checks/0")
                && name == "url"
                && reason == FactReason::Missing
            {
                format!("{parent}/id")
            } else {
                path.clone()
            };
            assert_case(&v, &[(&expected, reason)]).await;
        }
        if opaque.contains(&name.as_str()) {
            continue;
        }
        let mut v = base.clone();
        let bad = if base.pointer(&path).unwrap().is_boolean() {
            json!("bad")
        } else {
            json!(false)
        };
        replace(&mut v, &path, Some(bad));
        assert_case(&v, &[(&path, FactReason::Malformed)]).await;
    }
    let mut simultaneous = base.clone();
    let mut simultaneous_paths = vec![];
    for (name, value) in base.pointer(parent).unwrap().as_object().unwrap() {
        if matches!(name.as_str(), "type" | "version" | "provider" | "status")
            || (name == "kind" && parent.starts_with("/turn_budget"))
            || opaque.contains(&name.as_str())
        {
            continue;
        }
        let path = format!("{parent}/{name}");
        replace(
            &mut simultaneous,
            &path,
            Some(if value.is_boolean() {
                json!("bad")
            } else {
                json!(false)
            }),
        );
        simultaneous_paths.push(path);
    }
    let expected: Vec<_> = simultaneous_paths
        .iter()
        .map(|p| (p.as_str(), FactReason::Malformed))
        .collect();
    assert_case(&simultaneous, &expected).await;
    let mut v = base.clone();
    let path = format!("{parent}/future");
    v.pointer_mut(parent).unwrap()["future"] = json!({"nested":[1]});
    assert_case(
        &v,
        &[
            (&path, FactReason::UnknownField),
            (&format!("{path}/nested/0"), FactReason::Unclassified),
        ],
    )
    .await;
}

#[tokio::test]
async fn u13_p1_facts_attachment_domain_all_forms_and_siblings() {
    for bound in [false, true] {
        let parent = if bound {
            "/pending_entry_meta/0/attachments/0"
        } else {
            "/issued_input_segments/segments/0/attachments/0"
        };
        let mut v = original();
        v["issued_input_segments"] = json!({"turn_id":"t","segments":[
            {"presentation":"system_report_edited","text":"x","attachments":[],"origin":null}]});
        v["pending_entry_meta"] = json!([{"id":"q","rev":0,"queued_at_ms":1,"attachments":[]}]);
        let mut a = json!({"id":"0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png","size":1});
        if bound {
            a["path"] = json!("p");
        } else {
            a["contentType"] = json!("image/png");
            a["url"] = json!("u");
        }
        let array = parent.rsplit_once('/').unwrap().0;
        *v.pointer_mut(array).unwrap() = json!([a]);
        fields(&v, parent, &[]).await;
        let names: Vec<_> = v
            .pointer(parent)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let paths: Vec<_> = names.iter().map(|n| format!("{parent}/{n}")).collect();
        for path in &paths {
            replace(&mut v, path, Some(json!(false)));
        }
        let expected: Vec<_> = paths
            .iter()
            .map(|p| (p.as_str(), FactReason::Malformed))
            .collect();
        assert_case(&v, &expected).await;
        for size in [json!(-1), json!(1.5), json!("1")] {
            replace(&mut v, &format!("{parent}/size"), Some(size));
            assert_case(&v, &[(&format!("{parent}/size"), FactReason::Malformed)]).await;
        }
    }
}

pub(super) fn provider() -> Value {
    json!({"version":1,"execution_id":"e","purpose":{"Compaction":{"turn_id":"t"}},"release":{"Released":{"authorized_at_ms":1}},"decision":{"Stopping":{"reason":"Finished","at_ms":1}}})
}
pub(super) async fn provider_case(sql: &str, expected: Option<(&str, FactReason)>) {
    provider_checks(sql, expected, &[]).await;
}
async fn provider_checks(
    sql: &str,
    expected: Option<(&str, FactReason)>,
    stored: &[(String, Value)],
) {
    let fixture = Fixture::new(&format!(
        "{TABLES}{}{} UPDATE worker_sessions SET provider_execution_json={} WHERE id='neighbor'; UPDATE worker_sessions SET provider_execution_json={sql} WHERE id='w';",
        worker_row("w", "running", &original().to_string()),
        worker_row("neighbor", "idle", &original().to_string()),
        quoted(&provider().to_string())
    ))
    .await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    super::node_oracle::assert_all(&fixture, &facts).await;
    assert_eq!(facts.workers().len(), 2);
    for w in facts.workers() {
        assert!(matches!(w.frame, Some(FrameFacts::LegacyV1)));
        assert!(
            w.fields.iter().any(|f| f.interpretation
                == Interpretation::Blocked(FactReason::MissingProviderAuthority))
        );
    }
    let w = facts
        .workers()
        .iter()
        .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(b"w".to_vec())))
        .unwrap();
    let mut c = fixture.oracle().await;
    let text: String =
        sqlx::query_scalar("SELECT provider_execution_json FROM worker_sessions WHERE id='w'")
            .fetch_one(&mut c)
            .await
            .unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    for (path, value) in stored {
        assert!(w.fields.iter().any(|f| at(f, path) && f.expected_path.is_none()
            && matches!(&f.source.location, SourceLocation::Cell { column_name, .. } if column_name == "provider_execution_json")
            && f.interpretation == Interpretation::Stored(value.clone())), "{path}");
    }
    if let Some((path, reason)) = expected {
        let mut anchor = path;
        while value.pointer(anchor).is_none() {
            anchor = anchor.rsplit_once('/').unwrap().0;
        }
        assert!(w.fields.iter().any(|f| matches!(&f.source.location, SourceLocation::Cell { column_name, json: Some(a), .. } if column_name == "provider_execution_json" && a.pointer() == anchor)
            && f.expected_path.as_deref() == (anchor != path).then_some(path)
            && f.interpretation == Interpretation::Blocked(reason.clone())));
        assert!(blocked(w, path, reason.clone()));
        assert!(w.fields.iter().any(|f| at(f, path)
            && matches!(&f.source.location, SourceLocation::Cell { column_name, .. } if column_name == "provider_execution_json")
            && f.interpretation == Interpretation::Blocked(reason.clone())));
    } else {
        assert_provider_root(w, &value);
    }
    let neighbor = facts
        .workers()
        .iter()
        .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(b"neighbor".to_vec())))
        .unwrap();
    assert_provider_root(neighbor, &provider());
}
fn assert_provider_root(w: &WorkerFacts, value: &Value) {
    assert!(w.fields.iter().any(|f| matches!(&f.source.location,
        SourceLocation::Cell { column_name, json: Some(a), .. } if column_name == "provider_execution_json" && a.0.is_empty())
        && f.expected_path.is_none() && f.interpretation == Interpretation::Stored(value.clone())));
}
#[tokio::test]
async fn u13_p1_facts_provider_text_reader_contract() {
    use crate::db::sqlite::provider_execution::Record;
    for sequence in [false, true] {
        for (group, variant, names, values) in [
            ("purpose", "Turn", vec!["turn_id"], json!(["t"])),
            ("purpose", "Compaction", vec!["turn_id"], json!(["t"])),
            ("release", "Releasing", vec!["authorized_at_ms"], json!([7])),
            ("release", "Released", vec!["authorized_at_ms"], json!([7])),
            (
                "decision",
                "Stopping",
                vec!["reason", "at_ms"],
                json!(["Finished", 8]),
            ),
        ] {
            let mut base = provider();
            base[group] = json!({variant: if sequence { values.clone() } else {
                Value::Object(names.iter().zip(values.as_array().unwrap()).map(|(k,v)| ((*k).into(),v.clone())).collect())
            }});
            let parent = format!("/{group}/{variant}");
            let leaves: Vec<_> = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let slot = if sequence {
                        i.to_string()
                    } else {
                        (*name).into()
                    };
                    (format!("{parent}/{slot}"), values[i].clone())
                })
                .collect();
            for root_sequence in [false, true] {
                let root = if root_sequence {
                    json!([
                        base["version"],
                        base["execution_id"],
                        base["purpose"],
                        base["release"],
                        base["decision"]
                    ])
                } else {
                    base.clone()
                };
                let text = root.to_string();
                assert!(serde_json::from_str::<Record>(&text).is_ok());
                if sequence {
                    assert!(
                        serde_json::from_value::<Record>(serde_json::from_str(&text).unwrap())
                            .is_err()
                    );
                }
                let prefix = match group {
                    "purpose" => "2",
                    "release" => "3",
                    _ => "4",
                };
                let root_leaves: Vec<_> = leaves
                    .iter()
                    .map(|(p, v)| {
                        let path = if root_sequence {
                            p.replacen(group, prefix, 1)
                        } else {
                            p.clone()
                        };
                        (path, v.clone())
                    })
                    .collect();
                provider_checks(&quoted(&text), None, &root_leaves).await;
            }
            let mut multi = base.clone();
            for (path, _) in &leaves {
                *multi.pointer_mut(path).unwrap() = json!(false);
            }
            for (path, _) in &leaves {
                provider_checks(
                    &quoted(&multi.to_string()),
                    Some((path, FactReason::Malformed)),
                    &[("/execution_id".into(), json!("e"))],
                )
                .await;
            }
            for bad_slot in 0..names.len() {
                for (bad, reason) in [
                    (None, FactReason::Missing),
                    (Some(Value::Null), FactReason::Null),
                    (Some(json!(false)), FactReason::Malformed),
                ] {
                    let mut v = base.clone();
                    let path = leaves[bad_slot].0.clone();
                    if sequence {
                        let a = v.pointer_mut(&parent).unwrap().as_array_mut().unwrap();
                        if let Some(bad) = bad {
                            a[bad_slot] = bad;
                        } else {
                            a.truncate(bad_slot);
                        }
                    } else {
                        replace(&mut v, &path, bad);
                    }
                    let healthy: Vec<_> = leaves
                        .iter()
                        .filter(|(p, value)| v.pointer(p) == Some(value))
                        .cloned()
                        .chain([("/execution_id".into(), json!("e"))])
                        .collect();
                    provider_checks(&quoted(&v.to_string()), Some((&path, reason)), &healthy).await;
                }
            }
        }
    }
}

#[tokio::test]
async fn u13_p1_facts_provider_remaining_variants_and_early_returns() {
    for path in [
        "/purpose/Compaction/turn_id",
        "/release/Released/authorized_at_ms",
    ] {
        for (bad, reason) in [
            (None, FactReason::Missing),
            (Some(Value::Null), FactReason::Null),
            (Some(json!(false)), FactReason::Malformed),
        ] {
            let mut v = provider();
            replace(&mut v, path, bad);
            provider_case(&quoted(&v.to_string()), Some((path, reason))).await;
        }
    }
    for (name, variants) in [
        (
            "purpose",
            vec![
                json!("Turn"),
                json!("Compaction"),
                json!({"RewindCheck":null}),
                json!({"RewindCheck":{}}),
                json!({"Turn":{},"Compaction":{}}),
                json!("future"),
                json!({"future":{}}),
            ],
        ),
        (
            "release",
            vec![
                json!("Released"),
                json!("Releasing"),
                json!({"Held":null}),
                json!({"Held":{}}),
                json!({"NotRequired":null}),
                json!({"Held":null,"Released":{}}),
                json!("future"),
            ],
        ),
        (
            "decision",
            vec![
                json!("Stopping"),
                json!({"Observing":null}),
                json!({"Observing":null,"Stopping":{}}),
                json!("future"),
            ],
        ),
    ] {
        for variant in variants {
            let mut v = provider();
            v[name] = variant;
            let accepted =
                serde_json::from_value::<crate::db::sqlite::provider_execution::Record>(v.clone())
                    .is_ok();
            provider_case(
                &quoted(&v.to_string()),
                if accepted {
                    None
                } else {
                    Some(("", FactReason::Malformed))
                },
            )
            .await;
        }
    }
    let mut future = provider();
    future["version"] = json!(2);
    provider_case(
        &quoted(&future.to_string()),
        Some(("/version", FactReason::UnsupportedVersion)),
    )
    .await;
    let mut unknown = provider();
    unknown["future"] = json!({"leaf":1});
    provider_case(
        &quoted(&unknown.to_string()),
        Some(("/future", FactReason::UnknownField)),
    )
    .await;
    for (sql, reason) in [
        ("NULL", FactReason::Null),
        ("X'80'", FactReason::Malformed),
        ("17", FactReason::Malformed),
        ("CAST(X'80' AS TEXT)", FactReason::Malformed),
        ("'{bad'", FactReason::Malformed),
        ("'false'", FactReason::Malformed),
        (
            "'{\"version\":1,\"version\":2}'",
            FactReason::DuplicateMember,
        ),
    ] {
        let fixture = Fixture::new(&format!(
            "{}{}{} UPDATE worker_sessions SET provider_execution_json={sql} WHERE id='w'; UPDATE worker_sessions SET provider_execution_json={} WHERE id='neighbor';",
            TABLES.replace("provider_execution_json TEXT", "provider_execution_json"),
            worker_row("w", "running", &original().to_string()),
            worker_row("neighbor", "idle", &original().to_string()),
            quoted(&provider().to_string())
        ))
        .await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        assert_eq!(facts.workers().len(), 2);
        let bad = facts
            .workers()
            .iter()
            .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(b"w".to_vec())))
            .unwrap();
        let fields = &bad.fields;
        let neighbor = facts
            .workers()
            .iter()
            .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(b"neighbor".to_vec())))
            .unwrap();
        assert_provider_root(neighbor, &provider());
        assert!(matches!(neighbor.frame, Some(FrameFacts::LegacyV1)));
        for expected in [reason, FactReason::MissingProviderAuthority] {
            assert!(fields.iter().any(|f| matches!(&f.source.location,
                SourceLocation::Cell { column_name, .. } if column_name == "provider_execution_json")
                && f.interpretation == Interpretation::Blocked(expected.clone())));
        }
    }
    let fixture = Fixture::new("CREATE TABLE worker_sessions(id,state); INSERT INTO worker_sessions VALUES('w','running'),('neighbor','idle');").await;
    let audit = fixture.audit().await;
    let facts = classify_facts(&audit).unwrap();
    for w in facts.workers() {
        assert!(w.fields.iter().any(|f| f.source == w.source
            && f.expected_path.as_deref() == Some("provider_execution_json")
            && f.interpretation == Interpretation::Blocked(FactReason::MissingProviderAuthority)));
    }
}

#[tokio::test]
async fn u13_p1_facts_header_interruption_rewind_complete_matrix() {
    for name in super::legacy::HEADER {
        for (bad, reason) in [
            (None, FactReason::Missing),
            (Some(Value::Null), FactReason::Null),
            (Some(json!([])), FactReason::Malformed),
        ] {
            if name == "planner_compaction" && reason == FactReason::Malformed {
                continue;
            }
            let mut v = original();
            let path = format!("/{name}");
            replace(&mut v, &path, bad);
            assert_case(&v, &[(&path, reason)]).await;
        }
    }
    let uuid = "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f";
    for (parent, object) in [
        (
            "/pending_rewind",
            json!({"provider":"codex","before_turn_id":"t","reverted":true}),
        ),
        (
            "/pending_rewind",
            json!({"provider":"codex_v2","thread_id":"t","before_turn_id":"t","reverted":true}),
        ),
        (
            "/pending_rewind",
            json!({"provider":"claude","resume_at":uuid,"drops_turn":uuid}),
        ),
        (
            "/interruption_intent",
            json!({"turn_id":"t","reason":"max_turn_duration"}),
        ),
        (
            "/interruption_intent",
            json!({"version":"user_stop_v1","session_id":"w","card_id":"c","track_id":"t","thread_id":"n","turn_id":"u","requested_at_ms":1}),
        ),
        (
            "/interruption_intent",
            json!({"version":"v1","session_id":"w","card_id":"c","track_id":"t","thread_id":"n","turn_id":"u","requested_at_ms":1,"reason":"max_turn_duration","effective_elapsed_ms":1,"budget_ms":2}),
        ),
        (
            "/token_usage",
            json!({"used_tokens":1,"total_tokens":2,"at_ms":3,"context_window":4}),
        ),
        (
            "/compact_outcome",
            json!({"operation_id":"o","thread_id":"t","turn_id":"u","status":"completed"}),
        ),
        ("/last_turn_base", json!({"turn_id":"t","seen_head":"h"})),
        (
            "/turn_budget",
            json!({"kind":"planner","budget":15,"start_attempt":"a","warning_attempted":true,"context_usage":{"kind":"text","text":"t"}}),
        ),
    ] {
        let mut v = original();
        replace(&mut v, parent, Some(object.clone()));
        fields(&v, parent, &[]).await;
        let mut paths = vec![];
        for (name, value) in object.as_object().unwrap() {
            if matches!(name.as_str(), "provider" | "version" | "kind") {
                continue;
            }
            let path = format!("{parent}/{name}");
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
        if parent == "/interruption_intent" {
            let audit = frame_audit(&v).await;
            let facts = classify_facts(&audit).unwrap();
            assert!(facts.workers()[0].fields.iter().any(|f|
                f.expected_path.as_deref() == Some(parent)
                && matches!(&f.source.location, SourceLocation::Cell { json: Some(a), .. } if a.pointer() == parent)
                && f.interpretation == Interpretation::Blocked(FactReason::OwnerDiagnosticNotLeaf)));
        }
    }
    for (parent, tag) in [
        ("/interruption_intent", "version"),
        ("/pending_rewind", "provider"),
        ("/turn_budget", "kind"),
    ] {
        let mut v = original();
        replace(
            &mut v,
            parent,
            Some(json!({tag:"future","unknown":{"child":true}})),
        );
        assert_case(
            &v,
            &[
                (parent, FactReason::Malformed),
                (&format!("{parent}/unknown"), FactReason::UnknownField),
                (&format!("{parent}/unknown/child"), FactReason::Unclassified),
            ],
        )
        .await;
    }
    let mut v = original();
    v["turn_budget"] = json!({"kind":"planner","budget":15,"start_attempt":"a","warning_attempted":false,"context_usage":{"kind":"text","text":"t"}});
    fields(&v, "/turn_budget/context_usage", &[]).await;
    // Explicitly opaque server-owned value has no invented children.
    v["planner_compaction"] = json!({"unknown":{"child":false}});
    assert_case(
        &v,
        &[
            ("/planner_compaction", FactReason::MissingOwnerDecoder),
            (
                "/planner_compaction/unknown/child",
                FactReason::Unclassified,
            ),
        ],
    )
    .await;
}

#[tokio::test]
async fn u13_p1_facts_duplicate_nodes_and_malformed_neighbors_original_bytes() {
    for raw in [
        r#"{"schema_version":1,"pending_queue":[{"type":"track_goal","text":"first","text":"second","future":{"x":[],"x":[true]}}]}"#,
        r#"{"schema_version":1,"interruption_intent":{"version":"v1","turn_id":"first","turn_id":"second"}}"#,
        r#"{"schema_version":1,"pending_rewind":{"provider":"claude","resume_at":"first","resume_at":"second"}}"#,
        r#"{"schema_version":1,"issued_input_segments":{"segments":[{"attachments":[{"id":"first","id":"second"}]}]}}"#,
    ] {
        let fixture = Fixture::new(&format!(
            "{TABLES}{}{}{}",
            worker_row("duplicate", "closed", raw),
            worker_row("bad", "failed", "{bad"),
            worker_row("good", "running", &original().to_string())
        ))
        .await;
        let audit = fixture.audit().await;
        let facts = classify_facts(&audit).unwrap();
        super::node_oracle::assert_all(&fixture, &facts).await;
        assert_eq!(facts.workers().len(), 3);
        let by_id = |id: &str| {
            facts
                .workers()
                .iter()
                .find(|w| w.logical_key == WorkerKey::Stored(RawCell::Text(id.as_bytes().to_vec())))
                .unwrap()
        };
        assert!(
            by_id("duplicate")
                .fields
                .iter()
                .any(|f| f.interpretation == Interpretation::Blocked(FactReason::DuplicateMember))
        );
        assert!(
            by_id("bad")
                .fields
                .iter()
                .any(|f| f.interpretation == Interpretation::Blocked(FactReason::Malformed))
        );
        assert!(matches!(by_id("good").frame, Some(FrameFacts::LegacyV1)));
        assert!(!facts.sources().duplicates().is_empty());
    }
}
