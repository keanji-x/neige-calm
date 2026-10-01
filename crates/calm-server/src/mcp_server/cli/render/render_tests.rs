use super::*;

#[test]
fn diff_unknown_or_missing_status_is_a_render_error() {
    let value = json!({ "files": [{ "path": "a.md", "status": "added", "patch": "+x" }] });
    assert_eq!(
        render(Render::Diff, "calm.track.diff", false, &value).unwrap(),
        "a.md new\n+x\n"
    );
    for file in [
        json!({ "path": "a.md", "status": "renamed" }),
        json!({ "path": "a.md" }),
    ] {
        let value = json!({ "files": [file] });
        assert!(
            render(Render::Diff, "calm.track.diff", false, &value).is_err(),
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
        render(Render::Log, "calm.track.log", false, &value).unwrap(),
        "abcdef12 event=42 m\n0123 event=- \n"
    );
    let value = json!({ "commits": [{ "event_id": 1, "message": "m" }] });
    assert!(render(Render::Log, "calm.track.log", false, &value).is_err());
}

#[test]
fn tags_print_space_joined_and_require_a_string_array() {
    let tool = "calm.report.tag";
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
            "calm.report.tag value missing string array tags"
        );
    }
}

#[test]
fn content_pretty_prints_json_and_otherwise_prints_raw() {
    let json_view = json!({ "content": "{\"a\":1}", "content_type": "application/json" });
    assert_eq!(
        render(Render::Content, "calm.track.cat", true, &json_view).unwrap(),
        "{\n  \"a\": 1\n}\n"
    );
    for value in [
        json!({ "content": "{not json", "content_type": "application/json" }),
        json!({ "content": "{\"a\":1}" }),
    ] {
        let raw = value["content"].as_str().unwrap();
        assert_eq!(
            render(Render::Content, "calm.track.cat", false, &value).unwrap(),
            raw
        );
    }
}

/// A fresh track as `calm.track.state` returns it: open, empty title, no tasks, planner + report card.
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
            { "key": "fix-login", "status": "running", "worker_card_id": "crd_worker" },
            { "key": "add-test", "status": "pending", "worker_card_id": null },
            { "key": "old", "status": "failed", "worker_card_id": "crd_old" }
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
        "calm.track.state",
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
         live       crd_planner  planner  codex  (you)\n"
    );
    assert_eq!(text.lines().filter(|l| l.contains("closed_at")).count(), 1);
}

#[test]
fn state_text_prints_the_close_time_of_a_closed_track() {
    let mut value = draft_track_state();
    let closed_at = 1_790_000_000_000_i64;
    value["track"]["closed_at"] = json!(closed_at);
    let text = render(Render::State, "calm.track.state", false, &value).unwrap();
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
fn state_text_lists_only_live_cards_and_no_task_counts() {
    let text = render(
        Render::State,
        "calm.track.state",
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
         live       crd_planner  planner  codex   (you)\n\
         \x20          crd_worker   worker   claude  session running  task fix-login running\n"
    );
    assert_eq!(text.lines().filter(|l| l.contains("closed_at")).count(), 1);
    assert!(lines_starting(&text, "tasks").is_empty(), "{text}");
    for absent in ["crd_report", "crd_old", "exited", "old"] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
}

#[test]
fn state_text_shows_a_worker_caller_as_you_and_keeps_its_task() {
    let mut value = working_track_state();
    value["caller_card_id"] = json!("crd_worker");
    let text = render(Render::State, "calm.track.state", false, &value).unwrap();
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
            "           crd_worker   worker   claude  (you)         task fix-login running",
        ]
    );
}

#[test]
fn state_text_report_line_is_the_state_only() {
    let report = |value: &Value| {
        let text = render(Render::State, "calm.track.state", false, value).unwrap();
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
    let text = render(Render::State, "calm.track.state", false, &value).unwrap();
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
         live       crd_planner  planner  codex     (you)\n\
         \x20          crd_worker   worker   cl\\naude  session running  task fix-login running\n"
    );
    assert!(
        !text.trim_end_matches('\n').contains(['\r', '\t', '\u{7}']),
        "{text}"
    );
    assert!(text.contains("  cl\\naude  "), "{text}");

    value["track"]["title"] = json!("修复 登录 跳转 — café");
    let text = render(Render::State, "calm.track.state", false, &value).unwrap();
    assert_eq!(
        lines_starting(&text, "title"),
        vec!["title      修复 登录 跳转 — café"]
    );
}

#[test]
fn state_json_is_the_compact_tool_result() {
    let value = working_track_state();
    assert_eq!(
        render(Render::State, "calm.track.state", true, &value).unwrap(),
        format!("{value}\n")
    );
}

#[test]
fn state_shape_errors_name_the_missing_fact() {
    type Mutation = fn(&mut Value);
    let cases: [(&str, Mutation); 8] = [
        (
            "calm.track.state value missing string caller_card_id",
            |v| {
                v.as_object_mut().unwrap().remove("caller_card_id");
            },
        ),
        (
            "calm.track.state track missing number-or-null closed_at",
            |v| {
                v["track"].as_object_mut().unwrap().remove("closed_at");
            },
        ),
        ("calm.track.state value missing array tasks", |v| {
            v.as_object_mut().unwrap().remove("tasks");
        }),
        (
            "calm.track.state card missing object-or-null runtime",
            |v| {
                v["cards"][0].as_object_mut().unwrap().remove("runtime");
            },
        ),
        (
            "calm.track.state runtime has unknown status \"exploded\"",
            |v| v["cards"][2]["runtime"]["status"] = json!("exploded"),
        ),
        ("calm.track.state task missing string status", |v| {
            v["tasks"][0].as_object_mut().unwrap().remove("status");
        }),
        (
            "calm.track.state task has unknown status \"finished\"",
            |v| v["tasks"][0]["status"] = json!("finished"),
        ),
        (
            "calm.track.state caller card crd_gone is not among the track's cards",
            |v| v["caller_card_id"] = json!("crd_gone"),
        ),
    ];
    for (message, mutate) in cases {
        let mut value = working_track_state();
        mutate(&mut value);
        let err = render(Render::State, "calm.track.state", false, &value).unwrap_err();
        assert_eq!(err.message, message);
    }
}

/// #1932: a live session's row names the session's status and its current task's own, whatever
/// that task's status: running, reported and awaiting verification, or ended.
#[test]
fn state_text_names_the_session_status_and_the_task_status_apart() {
    for status in ["running", "verifying", "done", "failed", "canceled"] {
        let mut value = working_track_state();
        value["tasks"][0]["status"] = json!(status);
        let text = render(Render::State, "calm.track.state", false, &value).unwrap();
        assert_eq!(
            text.lines()
                .skip_while(|l| !l.starts_with("live"))
                .collect::<Vec<_>>(),
            vec![
                "live       crd_planner  planner  codex   (you)".to_string(),
                format!(
                    "           crd_worker   worker   claude  session running  task fix-login {status}"
                ),
            ]
        );
    }
}

/// #1932: `tasks` holds each key's current execution only, so a live card that ran an earlier
/// attempt of a key holds no task once the current attempt is another card's.
#[test]
fn state_text_binds_a_task_only_to_the_card_of_its_current_execution() {
    let mut value = working_track_state();
    value["cards"][3]["runtime"]["status"] = json!("idle");
    value["tasks"][2] = json!({ "key": "old", "status": "done", "worker_card_id": "crd_worker" });
    let text = render(Render::State, "calm.track.state", false, &value).unwrap();
    assert_eq!(
        text.lines()
            .skip_while(|l| !l.starts_with("live"))
            .collect::<Vec<_>>(),
        vec![
            "live       crd_planner  planner  codex   (you)",
            "           crd_worker   worker   claude  session running  task fix-login running, old done",
            "           crd_old      worker   codex   session idle",
        ]
    );
}
