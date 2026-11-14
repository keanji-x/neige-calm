//! Own-view node checks and the real MCP content/listing boundaries.
use super::*;

#[tokio::test]
async fn own_node_checks_locate_implicit_parents_without_changing_mcp_listing() {
    use calm_server::mcp_server::tools::track_file::own_node_kind;
    use calm_server::track_fs_view::{TrackFsError, TrackFsNodeKind, TrackFsView};
    let boot = boot().await;
    request_codex(&boot, "node-run").await;
    complete_run(&boot, "node-run", "node result").await;
    request_codex(&boot, "suffix-only.md").await;
    request_codex(&boot, "node-run.json").await;
    complete_run(&boot, "node-run.json", "directory result").await;
    let id = seed_gated_task(&boot, "node-gate.json", 1).await;
    let foreign = seed_gated_task_in(&boot, &boot.other_track_id, "foreign-node", 1).await;
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let planner = planner_identity(&boot);
    let worker = worker_identity(&boot);
    let plain = TrackFsView::new(boot.ctx.repo.as_ref(), &boot.ctx.write);
    let enabled = plain
        .clone()
        .with_gate_log_access(boot.gate_logs_dir.clone())
        .with_planner_run_summary();
    for path in [
        "runs/node-run".to_owned(),
        "runs/suffix-only.md".to_owned(),
        format!("runs/{id}"),
        format!("runs/{id}/gates"),
    ] {
        assert_eq!(
            enabled.node_kind(&track, &path).await.unwrap(),
            TrackFsNodeKind::Directory
        );
        assert_eq!(
            own_node_kind(&boot.ctx, &planner, &track, &path)
                .await
                .unwrap(),
            TrackFsNodeKind::Directory
        );
        let err = call_tool(
            &boot,
            TOOL_LS,
            planner_identity(&boot),
            json!({"path":path}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
    }
    let dual = own_node_kind(&boot.ctx, &planner, &track, "runs/node-run.json")
        .await
        .unwrap();
    assert!(dual.is_directory());
    assert!(dual.is_file());
    assert_eq!(
        call_tool(
            &boot,
            TOOL_LS,
            planner_identity(&boot),
            json!({"path":"runs/node-run.json"})
        )
        .await
        .unwrap_err()
        .code,
        RpcError::INVALID_PARAMS
    );
    let parent = call_tool(
        &boot,
        TOOL_LS,
        planner_identity(&boot),
        json!({"path":"runs/node-run.json/.."}),
    )
    .await
    .unwrap();
    let runs = call_tool(
        &boot,
        TOOL_LS,
        planner_identity(&boot),
        json!({"path":"runs"}),
    )
    .await
    .unwrap();
    assert_eq!(
        parent, runs,
        "dual identity remains navigable as a directory"
    );
    for path in [
        "runs/node-run.md",
        "runs/suffix-only.md.md",
        "runs/node-run.json",
        "runs/node-run.json/result",
        "runs/node-run/result",
        "runs/node-run/planner-messages.json",
        "runs/node-run/summary.json",
    ] {
        assert!(enabled.node_kind(&track, path).await.unwrap().is_file());
        let value = call_tool(
            &boot,
            TOOL_CAT,
            planner_identity(&boot),
            json!({"path":path}),
        )
        .await
        .unwrap();
        let absolute = call_tool(
            &boot,
            TOOL_CAT,
            planner_identity(&boot),
            json!({"path":format!("/track-file-test/track file test/{path}")}),
        )
        .await
        .unwrap();
        assert_eq!(absolute, value, "same own view for {path}");
        assert!(value["content"].is_string());
        if path == "runs/node-run.json" {
            assert_eq!(content_json(&value)["attempt_id"], "node-run");
        }
    }
    // Location succeeds while a gate log has no body on disk; cat still refuses it.
    let log = format!("runs/{id}/gates/1.log");
    assert_eq!(
        enabled.node_kind(&track, &log).await.unwrap(),
        TrackFsNodeKind::File
    );
    assert!(matches!(
        plain.node_kind(&track, &log).await,
        Err(TrackFsError::Forbidden(_))
    ));
    assert_eq!(
        call_tool(
            &boot,
            TOOL_CAT,
            planner_identity(&boot),
            json!({"path":log})
        )
        .await
        .unwrap_err()
        .code,
        RpcError::INVALID_PARAMS
    );
    for path in [
        format!("runs/{foreign}"),
        format!("runs/{foreign}/gates"),
        format!("runs/{foreign}/gates/1.log"),
        format!("cards/{}", boot.other_track_card_id),
    ] {
        assert_eq!(
            own_node_kind(&boot.ctx, &planner, &track, &path)
                .await
                .unwrap_err()
                .code,
            RpcError::FORBIDDEN
        );
    }
    assert!(matches!(
        plain.node_kind(&track, "runs/node-run/summary.json").await,
        Err(TrackFsError::Forbidden(_))
    ));
    assert_eq!(
        own_node_kind(&boot.ctx, &worker, &track, "runs/node-run/summary.json")
            .await
            .unwrap_err()
            .code,
        RpcError::FORBIDDEN
    );
    assert_eq!(
        call_tool(
            &boot,
            TOOL_CAT,
            worker_identity(&boot),
            json!({"path":"runs/node-run/summary.json"})
        )
        .await
        .unwrap_err()
        .code,
        RpcError::FORBIDDEN
    );
    for path in [
        "runs/unknown",
        "runs/unknown/result",
        "runs/node-run/unknown.json",
        "guide/unknown.md",
    ] {
        assert_eq!(
            own_node_kind(&boot.ctx, &planner, &track, path)
                .await
                .unwrap_err()
                .code,
            RpcError::INVALID_PARAMS
        );
    }
    assert_eq!(
        own_node_kind(&boot.ctx, &planner, &track, "guide")
            .await
            .unwrap(),
        TrackFsNodeKind::Directory
    );
    assert_eq!(
        own_node_kind(&boot.ctx, &planner, &track, "guide/report.md")
            .await
            .unwrap(),
        TrackFsNodeKind::File
    );
    assert_eq!(
        own_node_kind(&boot.ctx, &other_planner_identity(&boot), &track, "guide")
            .await
            .unwrap_err()
            .code,
        RpcError::FORBIDDEN
    );
}

#[tokio::test]
async fn own_terminal_node_uses_persisted_address_and_keeps_cat_checks() {
    use calm_server::mcp_server::tools::track_file::own_node_kind;
    use calm_server::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    use calm_server::track_fs_view::TrackFsNodeKind;
    let boot = boot().await;
    let id = seed_gated_task(&boot, "terminal-node", 0).await;
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let address = directory.path().join("persisted-output");
    let operations = SqlxOperationRepo::new(boot.sqlx_repo.db().clone());
    let op = operations
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: "terminal-node".into(),
                idempotency_key: Some(id.clone()),
                payload_hash: "node".into(),
            },
            json!({"track_id":boot.track_id}),
        )
        .await
        .unwrap();
    boot.sqlx_repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
                .bind(json!({"data":{"terminal_worker_pipe":{"log_path":address}}}).to_string())
                .bind(op.as_str()),
        )
        .await
        .unwrap();
    let path = format!("runs/{id}/terminal-worker.log");
    assert_eq!(
        own_node_kind(&boot.ctx, &planner_identity(&boot), &track, &path)
            .await
            .unwrap(),
        TrackFsNodeKind::File
    );
    assert_eq!(
        call_tool(
            &boot,
            TOOL_CAT,
            planner_identity(&boot),
            json!({"path":path})
        )
        .await
        .unwrap_err()
        .code,
        RpcError::INVALID_PARAMS
    );
    tokio::fs::write(&address, "terminal evidence")
        .await
        .unwrap();
    let value = call_tool(
        &boot,
        TOOL_CAT,
        planner_identity(&boot),
        json!({"path":path}),
    )
    .await
    .unwrap();
    assert_eq!(value["content"], "terminal evidence");
    assert_eq!(value["content_type"], "text/plain");
    let mut foreign_track = track.clone();
    foreign_track.id = boot.other_track_id.clone();
    assert_eq!(
        own_node_kind(
            &boot.ctx,
            &other_planner_identity(&boot),
            &foreign_track,
            &path
        )
        .await
        .unwrap_err()
        .code,
        RpcError::FORBIDDEN
    );
}

#[tokio::test]
async fn own_operation_parents_reuse_operation_lookup_without_reading_output() {
    use calm_server::mcp_server::tools::track_file::own_node_kind;
    use calm_server::operation::forge_action_adapter::FORGE_ACTION_KIND;
    use calm_server::operation::{
        OperationCompletionBus, OperationKey, OperationRepo, OperationRuntime, SpawnCtx,
        SqlxOperationRepo,
    };
    use calm_server::track_fs_view::TrackFsNodeKind;
    let boot = boot().await;
    let operations = Arc::new(SqlxOperationRepo::new(boot.sqlx_repo.db().clone()));
    let completion = OperationCompletionBus::new();
    let spawn = SpawnCtx::new(
        boot.ctx.repo.clone(),
        operations.clone(),
        Arc::new(calm_server::state::DaemonClient::new_stub()),
        calm_server::terminal_renderer::TerminalRendererRegistry::new_with_repo(boot.repo.clone()),
        boot.ctx.events.clone(),
        completion.clone(),
    );
    let runtime = OperationRuntime::new_unchecked(
        operations.clone(),
        vec![],
        boot.ctx.events.clone(),
        completion,
        spawn,
    );
    assert!(boot.ctx.operation_runtime.set(Arc::new(runtime)).is_ok());
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    for (key, kind, track_id, allowed) in [
        ("own-op", FORGE_ACTION_KIND, &boot.track_id, true),
        ("foreign-op", FORGE_ACTION_KIND, &boot.other_track_id, false),
        ("other-kind", "terminal-worker", &boot.track_id, false),
    ] {
        let id = operations
            .insert_operation(
                kind,
                OperationKey {
                    operation_key: key.into(),
                    idempotency_key: Some(key.into()),
                    payload_hash: key.into(),
                },
                json!({"track_id":track_id}),
            )
            .await
            .unwrap();
        for (path, expected) in [
            (format!("ops/{id}"), TrackFsNodeKind::Directory),
            (format!("ops/{id}/result"), TrackFsNodeKind::File),
        ] {
            let node = own_node_kind(&boot.ctx, &planner_identity(&boot), &track, &path).await;
            if allowed {
                assert_eq!(node.unwrap(), expected);
            } else {
                assert_eq!(node.unwrap_err().code, RpcError::NOT_FOUND);
            }
        }
        // Pending operations locate successfully but still have no cat output.
        assert_eq!(
            call_tool(
                &boot,
                TOOL_CAT,
                planner_identity(&boot),
                json!({"path":format!("ops/{id}/result")})
            )
            .await
            .unwrap_err()
            .code,
            if allowed {
                RpcError::INVALID_PARAMS
            } else {
                RpcError::NOT_FOUND
            }
        );
    }
    assert_eq!(
        own_node_kind(&boot.ctx, &planner_identity(&boot), &track, "ops/missing")
            .await
            .unwrap_err()
            .code,
        RpcError::NOT_FOUND
    );
}
