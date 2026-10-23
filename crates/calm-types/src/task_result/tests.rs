use super::*;
use serde_json::json;

fn review_v1() -> &'static ResultContract {
    result_contract("review-v1").expect("review-v1 is registered")
}

fn errors(result: Value) -> Vec<String> {
    (review_v1().validate)(&result).expect_err("result must be refused")
}

fn finding(title: &str) -> Value {
    json!({"title": title, "evidence": "decision_sink.rs:180 releases the lease first"})
}

#[test]
fn review_v1_accepts_pass_without_findings() {
    let result = json!({"verdict": "pass", "summary": "meets every criterion",
        "blocking_findings": []});
    assert_eq!((review_v1().validate)(&result), Ok(()));
    assert_eq!(review_v1().check(&result), Ok(()));
    let full = json!({"verdict": "pass", "summary": "ok", "blocking_findings": [],
        "non_blocking": [finding("naming")], "details": {"any": [1, "json"]}});
    assert_eq!((review_v1().validate)(&full), Ok(()));
}

#[test]
fn review_v1_rejects_missing_verdict() {
    let errors = errors(json!({"summary": "ok", "blocking_findings": []}));
    assert_eq!(
        errors,
        ["verdict: required; must be \"pass\" | \"blocking\""]
    );
}

#[test]
fn review_v1_rejects_pass_with_blocking_findings() {
    let errors = errors(json!({"verdict": "pass", "summary": "ok",
        "blocking_findings": [finding("lease")]}));
    assert_eq!(
        errors,
        ["verdict: \"pass\" requires blocking_findings to be empty"]
    );
}

#[test]
fn review_v1_rejects_blocking_without_findings() {
    let errors = errors(json!({"verdict": "blocking", "summary": "bad",
        "blocking_findings": []}));
    assert_eq!(
        errors,
        ["verdict: \"blocking\" requires at least one entry in blocking_findings"]
    );
}

#[test]
fn review_v1_rejects_blank_evidence_and_unknown_top_level_key() {
    let errors = errors(
        json!({"verdict": "blocking", "summary": "bad", "passed": false,
        "blocking_findings": [{"title": "lease", "evidence": " \n"}]}),
    );
    assert_eq!(
        errors,
        [
            "passed: unknown field",
            "blocking_findings[0].evidence: required non-blank string",
        ]
    );
}

#[test]
fn review_v1_bounds_its_text_and_finding_counts() {
    let mut result = json!({"verdict": "blocking", "summary": "é".repeat(1025),
        "blocking_findings": vec![finding("x"); 33],
        "non_blocking": [{"title": "t", "evidence": "a".repeat(4097), "extra": 1}]});
    let errors = errors(result.clone());
    assert_eq!(
        errors,
        [
            "summary: too long (2050 bytes), limit is 2048",
            "blocking_findings: too many findings (33), limit is 32",
            "non_blocking[0].extra: unknown field",
            "non_blocking[0].evidence: too long (4097 bytes), limit is 4096",
        ]
    );
    result["summary"] = json!("a".repeat(2048));
    result["blocking_findings"] = json!(vec![finding("x"); 32]);
    result["non_blocking"] = json!([{"title": "t", "evidence": "a".repeat(4096)}]);
    assert_eq!((review_v1().validate)(&result), Ok(()));
}

#[test]
fn review_v1_rejects_null_findings_lists_by_their_own_rule() {
    let errors = errors(json!({"verdict": "pass", "summary": "ok",
        "blocking_findings": null, "non_blocking": null}));
    assert_eq!(
        errors,
        [
            "blocking_findings: required array of findings",
            "non_blocking: must be an array of findings or omitted",
        ]
    );
}

#[test]
fn review_v1_refuses_a_non_object_result() {
    for (result, kind) in [(json!("looks good"), "string"), (Value::Null, "null")] {
        assert_eq!(
            errors(result),
            [format!("result: must be a JSON object, got {kind}")]
        );
    }
}

/// The refusal names the contract, every violation and the same shape the worker prompt states.
#[test]
fn a_result_contract_refusal_names_the_contract_the_violations_and_the_shape() {
    let refusal = review_v1()
        .check(&json!({"summary": "", "blocking_findings": []}))
        .unwrap_err();
    assert_eq!(
        refusal,
        format!(
            "result does not match result_contract review-v1: verdict: required; must be \
             \"pass\" | \"blocking\"; summary: required non-blank string. Expected: {}",
            review_v1().shape_hint
        )
    );
    assert!(
        review_v1()
            .shape_hint
            .starts_with("a JSON object {\"verdict\"")
    );
    assert!(!review_v1().shape_hint.ends_with('\n'));
}

#[test]
fn review_v1_conclusion_states_the_verdict_first() {
    let pass = json!({"verdict": "pass", "summary": "all green", "blocking_findings": []});
    assert_eq!(review_v1().conclusion_of(&pass).unwrap(), "pass: all green");
    let blocking = json!({"verdict": "blocking", "summary": "two defects",
        "blocking_findings": [finding("lease \"held\""), finding("no event")]});
    assert_eq!(
        review_v1().conclusion_of(&blocking).unwrap(),
        "blocking: 2 blocking findings: \"lease \"held\"\"; \"no event\". two defects"
    );
    let one = json!({"verdict": "blocking", "summary": "s",
        "blocking_findings": [finding("only")]});
    assert_eq!(
        review_v1().conclusion_of(&one).unwrap(),
        "blocking: 1 blocking finding: \"only\". s"
    );
    assert_eq!(review_v1().conclusion_of(&json!({"verdict": "pass"})), None);
}

/// Iterates the registry, so adding a contract edits no list here.
#[test]
fn every_result_contract_schema_example_passes_its_validator() {
    assert!(!RESULT_CONTRACTS.is_empty());
    for contract in RESULT_CONTRACTS {
        let schema = (contract.schema)();
        assert_eq!(schema["title"], contract.name);
        let examples = schema["examples"].as_array().expect("schema examples");
        assert!(
            examples.len() >= 2,
            "{}: a pass and a failing example",
            contract.name
        );
        for example in examples {
            assert_eq!(
                (contract.validate)(example),
                Ok(()),
                "{}: {example}",
                contract.name
            );
            assert!(!(contract.conclusion)(example).is_empty());
        }
        let names = RESULT_CONTRACTS
            .iter()
            .filter(|other| other.name == contract.name);
        assert_eq!(names.count(), 1, "{}: registered once", contract.name);
        assert!(std::ptr::eq(
            result_contract(contract.name).unwrap(),
            contract
        ));
    }
    assert!(result_contract("review-v2").is_none());
}

fn evaluation_result() -> Value {
    json!({"summary": "One evaluator sample", "judgements": [
        {"item": "clarity", "trend": "better", "evidence": [{"path": "src/lib.rs", "line": 1}]},
        {"item": "speed", "trend": "same", "evidence": []},
        {"item": "size", "trend": "worse", "evidence": []}
    ], "findings": {"blocking": 1, "major": 2, "minor": 3}})
}

#[test]
fn evaluation_result_contract_accepts_one_sample_and_counts_its_conclusion() {
    let contract = result_contract("evaluation-v1").expect("evaluation-v1 is registered");
    let mut result = evaluation_result();
    assert_eq!(contract.check(&result), Ok(()));
    assert_eq!(
        contract.conclusion_of(&result).as_deref(),
        Some("evaluation: 1 better / 1 same / 1 worse; findings 1/2/3")
    );
    result["judgements"] = json!([]);
    result["summary"] = json!("é".repeat(1024));
    result["findings"]["minor"] = json!(u32::MAX);
    assert_eq!(contract.check(&result), Ok(()));
    assert_eq!(
        contract.conclusion_of(&result).as_deref(),
        Some("evaluation: 0 better / 0 same / 0 worse; findings 1/2/4294967295")
    );
}

#[test]
fn evaluation_result_contract_refuses_malformed_fields() {
    let contract = result_contract("evaluation-v1").expect("evaluation-v1 is registered");
    for (pointer, value, field) in [
        ("/extra", json!(true), "extra"),
        ("/summary", json!(" \n"), "summary"),
        ("/summary", json!("é".repeat(1025)), "summary"),
        ("/judgements/0/extra", json!(true), "judgements[0]"),
        (
            "/judgements/0/trend",
            json!("improved"),
            "judgements[0].trend",
        ),
        ("/judgements/0/item", json!(4), "judgements[0].item"),
        (
            "/judgements/0/evidence/0/extra",
            json!(true),
            "judgements[0].evidence[0]",
        ),
        (
            "/judgements/0/evidence/0/line",
            json!(0),
            "judgements[0].evidence[0].line",
        ),
        (
            "/judgements/0/evidence/0/line",
            json!(4294967296_u64),
            "judgements[0].evidence[0].line",
        ),
        (
            "/judgements/0/evidence/0/path",
            json!(null),
            "judgements[0].evidence[0].path",
        ),
        ("/findings", json!([0, 0, 0]), "findings"),
        (
            "/judgements/0",
            json!(["clarity", "same", []]),
            "judgements[0]",
        ),
        (
            "/judgements/0/evidence/0",
            json!(["src/lib.rs", 1]),
            "judgements[0].evidence[0]",
        ),
        ("/findings/extra", json!(true), "findings"),
        ("/findings/blocking", json!(-1), "findings.blocking"),
        ("/findings/major", json!(1.5), "findings.major"),
        ("/findings/minor", json!(4294967296_u64), "findings.minor"),
    ] {
        let mut result = evaluation_result();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let parent = result.pointer_mut(parent).unwrap();
        if let Some(array) = parent.as_array_mut() {
            array[key.parse::<usize>().unwrap()] = value;
        } else {
            parent.as_object_mut().unwrap().insert(key.into(), value);
        }
        let errors = (contract.validate)(&result).expect_err(pointer);
        assert!(
            errors.iter().any(|e| e.contains(field)),
            "{pointer}: {errors:?}"
        );
        assert_eq!(contract.conclusion_of(&result), None);
        let refusal = contract.check(&result).unwrap_err();
        assert!(refusal.contains("evaluation-v1"));
        assert!(refusal.ends_with(contract.shape_hint));
    }
    for field in ["summary", "judgements", "findings"] {
        let mut result = evaluation_result();
        result.as_object_mut().unwrap().remove(field);
        let errors = (contract.validate)(&result).unwrap_err();
        assert!(errors.iter().any(|e| e.contains(field)), "{errors:?}");
    }
    for result in [json!(null), json!([]), json!("sample")] {
        assert!(contract.check(&result).unwrap_err().contains("result:"));
    }
}

#[test]
fn task_result_evaluation_v1_sample_uses_complete_contract() {
    let result = evaluation_result();
    let sample = evaluation_v1_sample(&result).unwrap();
    assert_eq!(
        serde_json::to_value(sample).unwrap()["judgements"],
        result["judgements"]
    );
    let mut missing = result;
    missing.as_object_mut().unwrap().remove("summary");
    assert!(
        evaluation_v1_sample(&missing)
            .unwrap_err()
            .contains("summary")
    );
    assert!(evaluation_v1_sample(&json!([])).is_err());
}
