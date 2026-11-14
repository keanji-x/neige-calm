//! Public MCP/CLI navigation through real bindings for each agent role.
use crate::support::mcp::{boot_with_role, call_tool_via_socket, cli_output, neige_cli_via_socket};
use calm_server::model::{CardRole, NewCard, NewTrack};
use serde_json::{Value, json};

async fn exercise(role: CardRole) {
    let boot = boot_with_role(role).await;
    let bound = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let sibling = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: bound.area_id.clone(),
            title: "foreign".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    for track in [&bound, &sibling] {
        boot.repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "track-report".into(),
                sort: None,
                payload: serde_json::to_value(
                    calm_server::track_report::TrackReportPayload::initial(),
                )
                .unwrap(),
            })
            .await
            .unwrap();
    }
    let call = |tool: &'static str, args: Value| {
        call_tool_via_socket(
            &boot.socket_path,
            &boot.raw_token,
            &boot.thread_id,
            1,
            tool,
            args,
        )
    };
    let own = call("neige_cat", json!({})).await;
    assert!(own.get("error").is_none(), "{own}");
    for path in [
        "report.md",
        "./report.md",
        "/mcp-test/mcp-test/report.md",
        "../mcp-test/report.md",
    ] {
        let result = call("neige_cat", json!({"path":path})).await;
        assert_eq!(
            result["result"]["structuredContent"], own["result"]["structuredContent"],
            "{path}: {result}"
        );
    }
    for path in [Value::Null, json!(""), json!(42)] {
        assert_eq!(
            call("neige_cat", json!({"path":path})).await["error"]["code"],
            -32602
        );
    }
    for (relative, absolute) in [
        (".", "/mcp-test/mcp-test/"),
        ("cards", "/mcp-test/mcp-test/cards/"),
    ] {
        let a = call("neige_ls", json!({"path":relative})).await;
        let b = call("neige_ls", json!({"path":absolute})).await;
        assert!(a.get("error").is_none(), "{a}");
        assert_eq!(
            a["result"]["structuredContent"],
            b["result"]["structuredContent"]
        );
    }
    for prefix in ["../foreign/", "/mcp-test/foreign/"] {
        assert!(
            call("neige_cat", json!({"path":format!("{prefix}report.md")}))
                .await
                .get("error")
                .is_none()
        );
        for tail in [
            "conversation.md",
            "track.json",
            "cards/index.json",
            "runs/index.json",
            "ops/x/result",
            "guide/report.md",
            "host/path",
        ] {
            let path = format!("{prefix}{tail}");
            assert_eq!(
                call("neige_cat", json!({"path":path})).await["error"]["code"],
                -32602,
                "{role:?} {path}"
            );
            let (out, _, exit) = cli_output(
                &neige_cli_via_socket(&boot.socket_path, &boot.raw_token, &["cat", &path]).await,
            );
            assert_eq!((exit, out.as_str()), (4, ""));
        }
    }
    for path in [
        "report.md/..",
        " ",
        " cards",
        "cards /..",
        "report.md/x",
        "report.md/",
        "missing/..",
        "foreign/report.md",
        "../../../",
        "//",
        "cards//index.json",
        "area/reports/mcp-test.md",
        "/areas/x/tracks/y/report.md",
        "/report-changes/1970-01-01.json",
    ] {
        for tool in ["neige_cat", "neige_ls"] {
            let response = call(tool, json!({"path":path})).await;
            assert_eq!(
                response["error"]["code"], -32602,
                "{role:?} {tool} {path}: {response}"
            );
        }
    }
    assert!(
        call("neige_ls", json!({"path":"../.."}))
            .await
            .get("error")
            .is_none()
    );
    boot.sqlx
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE areas SET kind='system' WHERE id=?1").bind(bound.area_id.as_str()),
        )
        .await
        .unwrap();
    assert!(
        call("neige_cat", json!({"path":"report.md"}))
            .await
            .get("error")
            .is_none()
    );
    assert!(
        call("neige_ls", json!({"path":"cards"}))
            .await
            .get("error")
            .is_none()
    );
    assert_eq!(
        call("neige_ls", json!({"path":".."})).await["error"]["code"],
        -32403
    );
    assert_eq!(
        call("neige_cat", json!({"path":"../foreign/report.md"})).await["error"]["code"],
        -32403
    );
    assert_eq!(
        call("neige_cat", json!({"path":"/mcp-test/foreign/report.md"})).await["error"]["code"],
        -32602
    );
    let edits = call(
        "neige_cat",
        json!({"path":"report-edits/1970-01-01.json","through_event_id":0}),
    )
    .await;
    assert!(edits.get("error").is_none(), "{edits}");
    let edits: Value = serde_json::from_str(
        edits["result"]["structuredContent"]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(edits["edits"], json!([]));
    assert!(
        call("neige_cat", json!({"path":"guide/report.md"}))
            .await
            .get("error")
            .is_none()
    );
    let root = call("neige_ls", json!({"path":"../.."})).await;
    assert!(
        root["result"]["structuredContent"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["area_id"] != bound.area_id.as_str())
    );
}

#[tokio::test]
async fn named_paths_planner_foreign_file_fence() {
    exercise(CardRole::Planner).await;
}
#[tokio::test]
async fn named_paths_assistant_foreign_file_fence() {
    exercise(CardRole::Assistant).await;
}
#[tokio::test]
async fn named_paths_worker_foreign_file_fence() {
    exercise(CardRole::Worker).await;
}
