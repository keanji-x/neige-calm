use super::*;

#[test]
fn diff_unknown_or_missing_status_is_a_render_error() {
    let value = json!({ "files": [{ "path": "a.md", "status": "added", "patch": "+x" }] });
    assert_eq!(
        render(Render::Diff, "neige_track_diff", false, &value).unwrap(),
        "a.md new\n+x\n"
    );
    for file in [
        json!({ "path": "a.md", "status": "renamed" }),
        json!({ "path": "a.md" }),
    ] {
        let value = json!({ "files": [file] });
        assert!(
            render(Render::Diff, "neige_track_diff", false, &value).is_err(),
            "{value}"
        );
    }
}

#[test]
fn log_renders_null_message_and_event_id_but_requires_a_hash() {
    let value = json!({ "commits": [
        { "hash": "abcdef123456", "event_id": 42, "message": "m" },
        { "hash": "0123", "event_id": null, "message": null }
    ]});
    assert_eq!(
        render(Render::Log, "neige_track_log", false, &value).unwrap(),
        "abcdef12 event=42 m\n0123 event=- \n"
    );
    let value = json!({ "commits": [{ "event_id": 1, "message": "m" }] });
    assert!(render(Render::Log, "neige_track_log", false, &value).is_err());
}

#[test]
fn tags_print_space_joined_and_require_a_string_array() {
    let tool = "neige_report_tag";
    let value = json!({ "tags": ["认证", "架构", "排障"] });
    assert_eq!(
        render(Render::Tags, tool, false, &value).unwrap(),
        "认证 架构 排障\n"
    );
    assert_eq!(
        render(Render::Tags, tool, true, &value).unwrap(),
        format!("{value}\n")
    );
    assert_eq!(
        render(Render::Tags, tool, false, &json!({ "tags": [] })).unwrap(),
        "\n"
    );
    for bad in [json!({}), json!({ "tags": [1] }), json!({ "tags": "a" })] {
        let err = render(Render::Tags, tool, false, &bad).unwrap_err();
        assert_eq!(
            err.message,
            "neige_report_tag value missing string array tags"
        );
    }
}

#[test]
fn content_pretty_prints_json_and_otherwise_prints_raw() {
    let json_view = json!({ "content": "{\"a\":1}", "content_type": "application/json" });
    assert_eq!(
        render(Render::Content, "neige_track_cat", true, &json_view).unwrap(),
        "{\n  \"a\": 1\n}\n"
    );
    for value in [
        json!({ "content": "{not json", "content_type": "application/json" }),
        json!({ "content": "{\"a\":1}" }),
    ] {
        let raw = value["content"].as_str().unwrap();
        assert_eq!(
            render(Render::Content, "neige_track_cat", false, &value).unwrap(),
            raw
        );
    }
}

/// A fresh track as `neige_track_state` returns it: open, empty title, no tasks, planner + report card.
fn draft_track_state() -> Value {
    json!({
        "track": {
            "id": "trk_1", "area_id": "area_1", "title": "", "closed_at": null,
            "cwd": "/tmp/x", "sort": 0.5, "created_at": 1, "updated_at": 2
        },
        "caller_card_id": "crd_planner",
        "cards": [
            { "id": "crd_planner", "kind": "codex", "role": "planner", "sort": 1.0,
              "created_at": 1, "updated_at": 1,
              "runtime": { "worker_session_id": "ws_1", "kind": "codex", "status": "running" } },
            { "id": "crd_report", "kind": "track-report", "role": "reportcard", "sort": 2.0,
              "created_at": 1, "updated_at": 1, "runtime": null }
        ],
        "report_startup_read_required": false,
        "tasks": []
    })
}

/// An open track at work: a running worker bound to one task, an exited worker bound to another.
fn working_track_state() -> Value {
    json!({
        "track": {
            "id": "trk_2", "area_id": "area_1", "title": "Fix login redirect",
            "closed_at": null, "cwd": "/tmp/y", "sort": 0.5, "created_at": 1, "updated_at": 2
        },
        "caller_card_id": "crd_planner",
        "cards": [
            { "id": "crd_planner", "kind": "codex", "role": "planner", "sort": 1.0,
              "created_at": 1, "updated_at": 1,
              "runtime": { "worker_session_id": "ws_1", "kind": "codex", "status": "idle" } },
            { "id": "crd_report", "kind": "track-report", "role": "reportcard", "sort": 2.0,
              "created_at": 1, "updated_at": 1, "runtime": null },
            { "id": "crd_worker", "kind": "claude", "role": "worker", "sort": 3.0,
              "created_at": 1, "updated_at": 1,
              "runtime": { "worker_session_id": "ws_2", "kind": "claude", "status": "running" } },
            { "id": "crd_old", "kind": "codex", "role": "worker", "sort": 4.0,
              "created_at": 1, "updated_at": 1,
              "runtime": { "worker_session_id": "ws_3", "kind": "codex", "status": "exited" } }
        ],
        "report_startup_read_required": true,
        "tasks": [
            { "key": "fix-login", "status": "running", "worker_card_id": "crd_worker", "access": "read_write", "start": "checkout" },
            { "key": "add-test", "status": "pending", "worker_card_id": null, "access": "read_write", "start": "checkout" },
            { "key": "old", "status": "failed", "worker_card_id": "crd_old", "access": "read_write", "start": "checkout" }
        ]
    })
}

fn lines_starting<'a>(text: &'a str, label: &str) -> Vec<&'a str> {
    text.lines()
        .filter(|line| line.starts_with(label))
        .collect()
}

#[test]
fn state_text_of_a_draft_track_says_untitled_and_names_the_caller() {
    let text = render(
        Render::State,
        "neige_track_state",
        false,
        &draft_track_state(),
    )
    .unwrap();
    assert_eq!(
        text,
        "track      trk_1\n\
         title      (untitled)\n\
         closed_at  -\n\
         you        crd_planner planner\n\
         report     empty skeleton\n\
         tasks      -\n\
         live       crd_planner  planner  codex  (you)\n"
    );
    assert_eq!(text.lines().filter(|l| l.contains("closed_at")).count(), 1);
}

#[test]
fn state_text_prints_the_close_time_of_a_closed_track() {
    let mut value = draft_track_state();
    let closed_at = 1_790_000_000_000_i64;
    value["track"]["closed_at"] = json!(closed_at);
    let text = render(Render::State, "neige_track_state", false, &value).unwrap();
    let at = chrono::Local
        .timestamp_millis_opt(closed_at)
        .single()
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    assert_eq!(
        lines_starting(&text, "closed_at"),
        vec![format!("closed_at  {at}")]
    );
}

#[test]
fn state_text_lists_every_task_and_only_live_cards() {
    let text = render(
        Render::State,
        "neige_track_state",
        false,
        &working_track_state(),
    )
    .unwrap();
    assert_eq!(
        text,
        "track      trk_2\n\
         title      Fix login redirect\n\
         closed_at  -\n\
         you        crd_planner planner\n\
         report     has content\n\
         tasks      fix-login running start=checkout\n\
         \x20          add-test pending start=checkout\n\
         \x20          old failed start=checkout\n\
         live       crd_planner  planner  codex   (you)\n\
         \x20          crd_worker   worker   claude  session running\n"
    );
    assert_eq!(text.lines().filter(|l| l.contains("closed_at")).count(), 1);
    for absent in ["crd_report", "crd_old", "exited"] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
}

#[test]
fn state_text_shows_a_worker_caller_as_you() {
    let mut value = working_track_state();
    value["caller_card_id"] = json!("crd_worker");
    let text = render(Render::State, "neige_track_state", false, &value).unwrap();
    assert_eq!(
        lines_starting(&text, "you "),
        vec!["you        crd_worker worker"]
    );
    assert_eq!(
        text.lines()
            .skip_while(|l| !l.starts_with("live"))
            .collect::<Vec<_>>(),
        vec![
            "live       crd_planner  planner  codex   session idle",
            "           crd_worker   worker   claude  (you)",
        ]
    );
}

#[test]
fn state_text_report_line_is_the_state_only() {
    let report = |value: &Value| {
        let text = render(Render::State, "neige_track_state", false, value).unwrap();
        lines_starting(&text, "report").join("\n")
    };
    let mut value = draft_track_state();
    assert_eq!(report(&value), "report     empty skeleton");
    value["report_startup_read_required"] = json!(true);
    assert_eq!(report(&value), "report     has content");
    value["cards"].as_array_mut().unwrap().pop();
    value["report_startup_read_required"] = json!(false);
    assert_eq!(report(&value), "report     none");
}

#[test]
fn state_text_escapes_control_characters_so_a_title_cannot_forge_a_line() {
    let mut value = working_track_state();
    value["track"]["title"] = json!("Example\nclosed_at  1\r\t\u{7}");
    value["cards"][2]["kind"] = json!("cl\naude");
    value["tasks"][1]["key"] = json!("add\ntest");
    let text = render(Render::State, "neige_track_state", false, &value).unwrap();
    assert_eq!(
        lines_starting(&text, "title"),
        vec!["title      Example\\nclosed_at  1\\r\\t\\u{7}"]
    );
    assert_eq!(lines_starting(&text, "closed_at"), vec!["closed_at  -"]);
    assert_eq!(
        text,
        "track      trk_2\n\
         title      Example\\nclosed_at  1\\r\\t\\u{7}\n\
         closed_at  -\n\
         you        crd_planner planner\n\
         report     has content\n\
         tasks      fix-login running start=checkout\n\
         \x20          add\\ntest pending start=checkout\n\
         \x20          old failed start=checkout\n\
         live       crd_planner  planner  codex     (you)\n\
         \x20          crd_worker   worker   cl\\naude  session running\n"
    );
    assert!(
        !text.trim_end_matches('\n').contains(['\r', '\t', '\u{7}']),
        "{text}"
    );
    assert!(text.contains("  cl\\naude  "), "{text}");

    value["track"]["title"] = json!("修复 登录 跳转 — café");
    let text = render(Render::State, "neige_track_state", false, &value).unwrap();
    assert_eq!(
        lines_starting(&text, "title"),
        vec!["title      修复 登录 跳转 — café"]
    );
}

#[test]
fn state_json_is_the_compact_tool_result() {
    let value = working_track_state();
    assert_eq!(
        render(Render::State, "neige_track_state", true, &value).unwrap(),
        format!("{value}\n")
    );
}

#[test]
fn state_text_start_is_explicit_for_checkout_and_upstream() {
    for start in ["checkout", "upstream"] {
        for access in ["read_only", "read_write"] {
            let mut value = working_track_state();
            for task in value["tasks"].as_array_mut().unwrap() {
                task["start"] = json!(start);
            }
            value["tasks"][0]["access"] = json!(access);
            let text = render(Render::State, "neige_track_state", false, &value).unwrap();
            let suffix = if access == "read_only" {
                " read_only"
            } else {
                ""
            };
            assert_eq!(
                lines_starting(&text, "tasks"),
                vec![format!(
                    "tasks      fix-login running{suffix} start={start}"
                )]
            );
        }
    }
}

#[test]
fn state_start_is_required_only_for_text_and_json_objects_pass_through() {
    for start in [
        None,
        Some(Value::Null),
        Some(json!(42)),
        Some(json!(true)),
        Some(json!("main")),
    ] {
        let mut value = working_track_state();
        if let Some(start) = start {
            value["tasks"][0]["start"] = start;
        } else {
            value["tasks"][0].as_object_mut().unwrap().remove("start");
        }
        let err = render(Render::State, "neige_track_state", false, &value).unwrap_err();
        assert_eq!(
            err.message,
            if value["tasks"][0]["start"] == "main" {
                "neige_track_state task has unknown start \"main\""
            } else {
                "neige_track_state task missing string start"
            }
        );
        assert_eq!(
            err.detail,
            json!({"kind":"shape","tool":"neige_track_state","task":value["tasks"][0]})
        );
        assert_eq!(
            render(Render::State, "neige_track_state", true, &value).unwrap(),
            format!("{value}\n")
        );
    }
}

#[test]
fn state_text_access_matrix_preserves_the_full_read_write_output() {
    for status in [
        "pending",
        "dispatched",
        "running",
        "verifying",
        "done",
        "failed",
        "canceled",
    ] {
        for (access, suffix) in [("read_write", ""), ("read_only", " read_only")] {
            let mut value = working_track_state();
            value["tasks"][0]["status"] = json!(status);
            value["tasks"][0]["access"] = json!(access);
            value["tasks"][0]["key"] = json!("fix\nlogin");
            assert_eq!(
                render(Render::State, "neige_track_state", false, &value).unwrap(),
                format!(
                    "track      trk_2\ntitle      Fix login redirect\nclosed_at  -\nyou        crd_planner planner\nreport     has content\ntasks      fix\\nlogin {status}{suffix} start=checkout\n           add-test pending start=checkout\n           old failed start=checkout\nlive       crd_planner  planner  codex   (you)\n           crd_worker   worker   claude  session running\n"
                )
            );
        }
    }
}

#[test]
fn state_text_marks_readers_without_a_live_worker() {
    let mut value = working_track_state();
    value["tasks"][1]["access"] = json!("read_only");
    value["tasks"][2]["access"] = json!("read_only");
    let text = render(Render::State, "neige_track_state", false, &value).unwrap();
    assert!(
        text.contains("\n           add-test pending read_only start=checkout\n           old failed read_only start=checkout\n"),
        "{text}"
    );
    assert!(!text.contains("crd_old"), "{text}");
}

#[test]
fn state_access_is_required_only_for_text_and_json_objects_pass_through() {
    for access in [
        None,
        Some(Value::Null),
        Some(json!(42)),
        Some(json!(true)),
        Some(json!("reader")),
    ] {
        let mut value = working_track_state();
        if let Some(access) = access {
            value["tasks"][0]["access"] = access;
        } else {
            value["tasks"][0].as_object_mut().unwrap().remove("access");
        }
        let err = render(Render::State, "neige_track_state", false, &value).unwrap_err();
        assert_eq!(
            err.message,
            if value["tasks"][0]["access"] == "reader" {
                "neige_track_state task has unknown access \"reader\""
            } else {
                "neige_track_state task missing string access"
            }
        );
        assert_eq!(
            err.detail,
            json!({"kind":"shape","tool":"neige_track_state","task":value["tasks"][0]})
        );
        assert_eq!(
            render(Render::State, "neige_track_state", true, &value).unwrap(),
            format!("{value}\n")
        );
    }
    for value in [
        Value::Null,
        json!([]),
        json!("state"),
        json!(1),
        json!(false),
    ] {
        for json in [false, true] {
            let err = render(Render::State, "neige_track_state", json, &value).unwrap_err();
            assert_eq!(
                err.message,
                "neige_track_state returned non-object structuredContent"
            );
            assert_eq!(err.detail["kind"], "shape");
        }
    }
}

#[test]
fn state_shape_errors_name_the_missing_fact() {
    type Mutation = fn(&mut Value);
    let cases: [(&str, Mutation); 8] = [
        (
            "neige_track_state value missing string caller_card_id",
            |v| {
                v.as_object_mut().unwrap().remove("caller_card_id");
            },
        ),
        (
            "neige_track_state track missing number-or-null closed_at",
            |v| {
                v["track"].as_object_mut().unwrap().remove("closed_at");
            },
        ),
        ("neige_track_state value missing array tasks", |v| {
            v.as_object_mut().unwrap().remove("tasks");
        }),
        (
            "neige_track_state card missing object-or-null runtime",
            |v| {
                v["cards"][0].as_object_mut().unwrap().remove("runtime");
            },
        ),
        (
            "neige_track_state runtime has unknown status \"exploded\"",
            |v| v["cards"][2]["runtime"]["status"] = json!("exploded"),
        ),
        ("neige_track_state task missing string status", |v| {
            v["tasks"][0].as_object_mut().unwrap().remove("status");
        }),
        (
            "neige_track_state task has unknown status \"finished\"",
            |v| v["tasks"][0]["status"] = json!("finished"),
        ),
        (
            "neige_track_state caller card crd_gone is not among the track's cards",
            |v| v["caller_card_id"] = json!("crd_gone"),
        ),
    ];
    for (message, mutate) in cases {
        let mut value = working_track_state();
        mutate(&mut value);
        let err = render(Render::State, "neige_track_state", false, &value).unwrap_err();
        assert_eq!(err.message, message);
    }
}

/// #1932: the task's status is its own line, whatever it is: running, reported and awaiting
/// verification, or ended; its worker's row names only the session's status.
#[test]
fn state_text_names_the_session_status_and_the_task_status_apart() {
    for status in ["running", "verifying", "done", "failed", "canceled"] {
        let mut value = working_track_state();
        value["tasks"][0]["status"] = json!(status);
        let text = render(Render::State, "neige_track_state", false, &value).unwrap();
        assert_eq!(
            lines_starting(&text, "tasks"),
            vec![format!("tasks      fix-login {status} start=checkout")]
        );
        assert_eq!(
            text.lines()
                .skip_while(|l| !l.starts_with("live"))
                .collect::<Vec<_>>(),
            vec![
                "live       crd_planner  planner  codex   (you)",
                "           crd_worker   worker   claude  session running",
            ]
        );
    }
}

/// #1932: `tasks` holds each key's current execution only, once, whichever card ran it; a live card
/// shows only its session.
#[test]
fn state_text_shows_each_key_once_at_its_current_execution() {
    let mut value = working_track_state();
    value["cards"][3]["runtime"]["status"] = json!("idle");
    value["tasks"][2] = json!({ "key": "old", "status": "done", "worker_card_id": "crd_worker", "access": "read_write", "start": "checkout" });
    let text = render(Render::State, "neige_track_state", false, &value).unwrap();
    assert_eq!(
        text.lines()
            .skip_while(|l| !l.starts_with("tasks"))
            .collect::<Vec<_>>(),
        vec![
            "tasks      fix-login running start=checkout",
            "           add-test pending start=checkout",
            "           old done start=checkout",
            "live       crd_planner  planner  codex   (you)",
            "           crd_worker   worker   claude  session running",
            "           crd_old      worker   codex   session idle",
        ]
    );
}
