use super::*;
use serde_json::json;

#[test]
fn chart_payload_valid_and_invalid() {
    assert_eq!(
        validate_payload(
            KIND_CHART_CANDLES,
            &json!({
                "symbol": "0700.HK",
                "period": "day",
                "candles": [[1000, 1.0, 2.0, 0.5, 1.5, 100], [2000, 1.5, 2.5, 1.0, 2.0]],
                "overlays": ["ma20", "ma60"],
                "caption": "Tencent daily"
            }),
        ),
        Ok(())
    );
    // Field-level errors, all collected.
    let err = validate_payload(
        KIND_CHART_CANDLES,
        &json!({ "candles": [[1, 2]], "period": "year", "extra": 1 }),
    )
    .unwrap_err();
    assert!(err.contains("extra: unknown field"), "{err}");
    assert!(err.contains("symbol: required"), "{err}");
    assert!(err.contains("period: must be one of"), "{err}");
    assert!(err.contains("at least 2 candles"), "{err}");
    assert!(err.contains("candles[0]"), "{err}");
    // Non-object payload.
    assert!(validate_payload(KIND_CHART_CANDLES, &json!([1])).is_err());
}

#[test]
fn table_payload_valid_and_invalid() {
    assert_eq!(
        validate_payload(
            KIND_TABLE,
            &json!({
                "columns": [
                    { "key": "name", "label": "公司" },
                    { "key": "pe", "label": "PE", "align": "right" }
                ],
                "rows": [
                    { "name": "腾讯", "pe": 18.2 },
                    { "name": "阿里", "pe": null }
                ],
                "caption": "可比公司",
                "highlight": "腾讯"
            }),
        ),
        Ok(())
    );
    let err = validate_payload(
        KIND_TABLE,
        &json!({
            "columns": [
                { "key": "a", "label": "A", "align": "center", "width": 1 },
                { "key": "a", "label": 3 }
            ],
            "rows": [{ "b": {} }],
        }),
    )
    .unwrap_err();
    assert!(err.contains("columns[0].align"), "{err}");
    assert!(err.contains("columns[0].width: unknown field"), "{err}");
    assert!(err.contains("columns[1].key: duplicate"), "{err}");
    assert!(err.contains("columns[1].label: required string"), "{err}");
    assert!(
        err.contains("rows[0].b: not a declared column key"),
        "{err}"
    );
    assert!(
        err.contains("rows[0].b: must be string | number | null"),
        "{err}"
    );
    assert!(
        validate_payload(KIND_TABLE, &json!({ "columns": [], "rows": [] })).is_err(),
        "empty columns rejected"
    );
}

#[test]
fn app_payload_valid_and_invalid() {
    assert_eq!(
        validate_payload(
            KIND_APP,
            &json!({ "src": "/apps/screener", "title": "选股器", "height": 600 })
        ),
        Ok(())
    );
    assert_eq!(validate_payload(KIND_APP, &json!({ "src": "/x" })), Ok(()));
    // Percent-encoded control chars are harmless (browsers do not
    // pre-decode before URL parsing) — stay allowed, as do spaces
    // and non-control UTF-8.
    for ok in ["/%0A/page", "/apps/%5C", "/path with space", "/应用/看板"] {
        assert_eq!(
            validate_payload(KIND_APP, &json!({ "src": ok })),
            Ok(()),
            "{ok} must stay allowed"
        );
    }
    for (payload, needle) in [
        (json!({ "src": "https://evil.example/x" }), "src"),
        (json!({ "src": "//evil.example/x" }), "src"),
        (json!({ "src": "relative/path" }), "src"),
        (json!({}), "src"),
        // Backslash bypasses (#960 PR3 review round 1): WHATWG URL
        // parsing normalizes `\` to `/`, so `/\host` becomes a
        // protocol-relative URL in the browser.
        (json!({ "src": "/\\evil.example/x" }), "src"),
        (json!({ "src": "/x\\..\\..\\evil" }), "src"),
        (json!({ "src": "/apps\\x" }), "src"),
        // Control-character bypasses (#960 PR3 review round 2):
        // WHATWG strips tab/newline/C0 before parsing, so
        // `/\n/evil` would normalize to `//evil` in the browser.
        (json!({ "src": "/\n/evil.example/x" }), "src"),
        (json!({ "src": "/\t/evil.example/x" }), "src"),
        (json!({ "src": "/\r/evil.example/x" }), "src"),
        (json!({ "src": "/x\u{0}" }), "src"),
        (json!({ "src": "/x\u{7f}" }), "src"),
        (json!({ "src": "/x\u{85}y" }), "src"),
        (json!({ "src": "/x", "height": 80 }), "height"),
        (json!({ "src": "/x", "height": 9000 }), "height"),
        (json!({ "src": "/x", "onload": "x" }), "unknown field"),
    ] {
        let err = validate_payload(KIND_APP, &payload).unwrap_err();
        assert!(err.contains(needle), "{payload} → {err}");
    }
}

#[test]
fn payload_size_caps_are_enforced_with_the_limit_in_the_error() {
    // candles > 5000
    let candles: Vec<Value> = (0..(MAX_CHART_CANDLES as i64 + 1))
        .map(|i| json!([i, 1, 2, 0, 1]))
        .collect();
    let err = validate_payload(
        KIND_CHART_CANDLES,
        &json!({ "symbol": "X", "candles": candles }),
    )
    .unwrap_err();
    assert!(err.contains("limit is 5000"), "{err}");

    // columns > 32
    let columns: Vec<Value> = (0..=MAX_TABLE_COLUMNS)
        .map(|i| json!({ "key": format!("k{i}"), "label": "L" }))
        .collect();
    let err = validate_payload(KIND_TABLE, &json!({ "columns": columns, "rows": [] })).unwrap_err();
    assert!(err.contains("limit is 32"), "{err}");

    // rows > 500
    let rows: Vec<Value> = (0..=MAX_TABLE_ROWS).map(|_| json!({ "k": 1 })).collect();
    let err = validate_payload(
        KIND_TABLE,
        &json!({ "columns": [{ "key": "k", "label": "K" }], "rows": rows }),
    )
    .unwrap_err();
    assert!(err.contains("limit is 500"), "{err}");

    // any string field > 2048 chars
    let long = "x".repeat(MAX_STRING_CHARS + 1);
    for payload in [
        json!({ "symbol": long, "candles": [[1,1,1,1,1],[2,2,2,2,2]] }),
        json!({ "src": format!("/{long}") }),
        json!({ "src": "/x", "title": long }),
    ] {
        let kind = if payload.get("symbol").is_some() {
            KIND_CHART_CANDLES
        } else {
            KIND_APP
        };
        let err = validate_payload(kind, &payload).unwrap_err();
        assert!(err.contains("limit is 2048"), "{payload} → {err}");
    }
    let err = validate_payload(
        KIND_TABLE,
        &json!({ "columns": [{ "key": "k", "label": "K" }], "rows": [{ "k": long }] }),
    )
    .unwrap_err();
    assert!(
        err.contains("rows[0].k") && err.contains("limit is 2048"),
        "{err}"
    );

    // canonical JSON > 256KB (field-valid table: 200 rows of
    // 2000-char strings ≈ 400KB)
    let cell = "y".repeat(2000);
    let rows: Vec<Value> = (0..200).map(|_| json!({ "k": cell })).collect();
    let err = validate_payload(
        KIND_TABLE,
        &json!({ "columns": [{ "key": "k", "label": "K" }], "rows": rows }),
    )
    .unwrap_err();
    assert!(err.contains("256KB"), "{err}");

    // At-the-limit shapes pass.
    let candles: Vec<Value> = (0..100).map(|i| json!([i, 1, 2, 0, 1])).collect();
    assert_eq!(
        validate_payload(
            KIND_CHART_CANDLES,
            &json!({ "symbol": "X", "candles": candles })
        ),
        Ok(())
    );
}

#[test]
fn unknown_kind_is_an_error() {
    let err = validate_payload("metrics", &json!({})).unwrap_err();
    assert!(err.contains("unknown block kind `metrics`"), "{err}");
    assert!(!is_data_kind("prose"));
    assert!(is_data_kind("chart.candles"));
    assert!(is_data_kind("task"));
}

fn valid_task() -> Value {
    json!({
        "key": "impl-parser", "kind": "codex", "goal": "split parser",
        "acceptance": "tests pass", "gate": { "cwd": "/repo", "timeout_secs": 1800,
            "steps": [{ "name": "test", "cmd": "cargo test" }] },
        "no_gate_reason": "not used by policy when gate exists", "depends_on": ["design"],
        "priority": 0, "cwd": "/repo", "context": {},
        "refs": ["neige://wave/w1#b_1f3a"], "ready": true,
        "declared_by": "spec", "released_by_user": false, "spawn": "in-wave"
    })
}

#[test]
fn task_payload_validates_every_field_and_unknown_fields() {
    assert_eq!(validate_payload(KIND_TASK, &valid_task()), Ok(()));
    let cases = [
        ("key", json!("Bad Key"), "key:"),
        ("kind", json!("shell"), "kind:"),
        ("goal", json!("  "), "goal:"),
        ("acceptance", json!(7), "acceptance:"),
        ("no_gate_reason", json!(" "), "no_gate_reason:"),
        ("depends_on", json!([1]), "depends_on[0]"),
        ("priority", json!(1.5), "priority:"),
        ("cwd", json!("relative"), "cwd:"),
        ("refs", json!(["neige://wave/w1"]), "refs[0]"),
        ("ready", json!("yes"), "ready:"),
        ("released_by_user", json!(1), "released_by_user:"),
        ("spawn", json!("process"), "spawn:"),
    ];
    for (field, value, expected) in cases {
        let mut payload = valid_task();
        payload[field] = value;
        let error = validate_payload(KIND_TASK, &payload).unwrap_err();
        assert!(error.contains(expected), "{field}: {error}");
    }
    let mut empty_gate = valid_task();
    empty_gate["gate"] = json!({"steps":[]});
    assert_eq!(
        validate_payload(KIND_TASK, &empty_gate).unwrap_err(),
        "gate.steps must be non-empty"
    );
    let mut arbitrary_context = valid_task();
    arbitrary_context["context"] = json!([1, true, {"nested": "ok"}]);
    assert_eq!(validate_payload(KIND_TASK, &arbitrary_context), Ok(()));
    let mut payload = valid_task();
    payload["surprise"] = json!(true);
    assert!(
        validate_payload(KIND_TASK, &payload)
            .unwrap_err()
            .contains("surprise: unknown field")
    );
}

#[test]
fn acceptance_4_missing_explicit_in_track_and_null_spawn_normalize_identically() {
    let mut missing = valid_task();
    missing.as_object_mut().unwrap().remove("spawn");
    let explicit = valid_task();
    let mut null = valid_task();
    null["spawn"] = Value::Null;
    for payload in [&missing, &explicit, &null] {
        assert_eq!(validate_payload(KIND_TASK, payload), Ok(()));
    }
    let blocks = |payload: Value| {
        vec![crate::track_report::ReportBlock {
            id: "b_0001".into(),
            kind: KIND_TASK.into(),
            payload,
            rev: 1,
        }]
    };
    let values = [missing, explicit, null].map(|payload| {
        crate::report_blocks::tasks::project_task_declarations(&blocks(payload)).0[0]
            .spawn
            .clone()
    });
    assert_eq!(values, ["in-wave", "in-wave", "in-wave"]);
}

#[test]
fn acceptance_23_sub_track_rejects_claude_and_terminal_at_common_write_validation() {
    for kind in ["claude", "terminal"] {
        let mut payload = valid_task();
        payload["kind"] = json!(kind);
        payload["spawn"] = json!("sub-wave");
        let error = validate_payload(KIND_TASK, &payload).unwrap_err();
        assert!(error.contains("requires kind \"codex\""), "{kind}: {error}");
    }
}

#[test]
fn task_priority_rejects_integer_outside_i64_range() {
    let mut payload = valid_task();
    payload["priority"] = json!(9_223_372_036_854_775_808_u64);

    assert_eq!(
        validate_payload(KIND_TASK, &payload).unwrap_err(),
        "priority: must be an integer between -9223372036854775808 and 9223372036854775807"
    );
}

#[test]
fn task_declared_by_accepts_spec_and_user() {
    let mut payload = valid_task();
    payload["declared_by"] = json!("user");
    assert_eq!(validate_payload(KIND_TASK, &payload), Ok(()));

    payload["declared_by"] = json!("kernel");
    assert!(validate_payload(KIND_TASK, &payload).is_err());
}

#[test]
fn task_tombstone_is_a_closed_shape() {
    let tombstone = json!({"key":"old","tombstone":{"reason":"removed"},"declared_by":"spec","tombstoned_by":"user"});
    assert_eq!(validate_payload(KIND_TASK, &tombstone), Ok(()));
    let mut extra = tombstone.clone();
    extra["kind"] = json!("codex");
    assert!(
        validate_payload(KIND_TASK, &extra)
            .unwrap_err()
            .contains("kind: must be absent")
    );
    for invalid in [json!(false), json!(0), json!("")] {
        let payload =
            json!({"key":"old","tombstone":invalid,"declared_by":"spec","tombstoned_by":"user"});
        assert!(
            validate_payload(KIND_TASK, &payload)
                .unwrap_err()
                .contains("tombstone: must be an object")
        );
    }
    let mut live = valid_task();
    live["tombstoned_by"] = json!("spec");
    assert!(
        validate_payload(KIND_TASK, &live)
            .unwrap_err()
            .contains("must be absent")
    );
}
