use super::*;

#[tokio::test]
async fn plan_list_summary_exact_current_key() {
    let boot = boot().await;
    write_task_block(&boot, json!({"key":"a", "kind":"codex", "goal":"neighbor"})).await;
    write_task_block(&boot, json!({"key":"ab", "kind":"codex", "goal":"wanted"})).await;
    let before = all_persistent_rows(&boot).await;
    let out = call_tool(
        &boot,
        TOOL_PLAN_LIST,
        planner_identity(&boot),
        json!({"detail":"summary","key":"ab"}),
    )
    .await
    .unwrap();
    assert_eq!(out["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(out["tasks"][0]["key"], "ab");
    assert_eq!(
        out["tasks"][0]["attempt_id"],
        format!("{}:ab", boot.track_id)
    );
    assert!(out["tasks"][0].get("goal").is_none());
    assert_eq!(
        out["tasks"][0]["full_evidence"]["arguments"],
        json!({"detail":"full","key":"ab"})
    );
    assert_eq!(all_persistent_rows(&boot).await, before);
}

#[tokio::test]
async fn plan_list_summary_args_strict() {
    let boot = boot().await;
    for args in [
        json!(null),
        json!([]),
        json!({"key":null}),
        json!({"key":""}),
        json!({"key":"  "}),
        json!({"key":3}),
        json!({"key":[]}),
        json!({"keys":["a"]}),
        json!({"detail":null}),
        json!({"detail":"other"}),
        json!({"detail":true}),
        json!({"unknown":true}),
    ] {
        let error = call_tool(&boot, TOOL_PLAN_LIST, planner_identity(&boot), args.clone())
            .await
            .expect_err(&args.to_string());
        assert_eq!(error.code, -32602);
    }
}

#[tokio::test]
async fn plan_list_summary_missing_current() {
    let boot = boot().await;
    write_task_block(&boot, json!({"key":"a", "kind":"codex", "goal":"neighbor"})).await;
    for key in ["missing", "A", " a", "a "] {
        let error = call_tool(
            &boot,
            TOOL_PLAN_LIST,
            planner_identity(&boot),
            json!({"detail":"summary","key":key}),
        )
        .await
        .expect_err("exact missing key");
        assert!(
            error.message.contains("current execution unavailable"),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn plan_list_summary_large_goal_and_legacy_full() {
    let boot = boot().await;
    let goal = "large-private-goal ".repeat(100);
    write_task_block(&boot, json!({"key":"a", "kind":"codex", "goal":goal})).await;
    let full = call_tool(&boot, TOOL_PLAN_LIST, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(full["tasks"][0]["goal"], goal);
    assert!(full["tasks"][0].get("full_evidence").is_none());
    let summary = call_tool(
        &boot,
        TOOL_PLAN_LIST,
        planner_identity(&boot),
        json!({"detail":"summary"}),
    )
    .await
    .unwrap();
    assert!(summary.to_string().len() <= 12 * 1024);
    assert!(!summary.to_string().contains("large-private-goal"));
    for field in [
        "key",
        "attempt_id",
        "generation",
        "status",
        "blocking_reason",
    ] {
        assert_eq!(summary["tasks"][0][field], full["tasks"][0][field]);
    }
}

#[tokio::test]
async fn plan_list_summary_same_track_only() {
    let local = boot().await;
    let foreign = local
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: local.area_id.clone(),
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
    write_task_block(
        &local,
        json!({"key":"same", "kind":"codex", "goal":"local"}),
    )
    .await;
    let pool = local.repo.sqlite_pool().unwrap();
    for key in ["same", "foreign-only"] {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'codex','foreign','{}','pending',1,1)")
            .bind(format!("{}:{key}",foreign.id)).bind(foreign.id.as_str()).bind(key).execute(&pool).await.unwrap();
    }
    let missing = call_tool(
        &local,
        TOOL_PLAN_LIST,
        planner_identity(&local),
        json!({"key":"foreign-only","detail":"summary"}),
    )
    .await
    .unwrap_err();
    assert!(missing.message.contains("current execution unavailable"));
    let result = call_tool(
        &local,
        TOOL_PLAN_LIST,
        planner_identity(&local),
        json!({"key":"same"}),
    )
    .await
    .unwrap();
    assert_eq!(result["tasks"][0]["goal"], "local");
    assert_eq!(
        result["tasks"][0]["attempt_id"],
        format!("{}:same", local.track_id)
    );
    for args in [
        json!({"detail":"summary","key":"same"}),
        json!({"detail":"full","key":"same"}),
    ] {
        let error = call_tool(&local, TOOL_PLAN_LIST, worker_identity(&local), args)
            .await
            .unwrap_err();
        assert!(error.message.contains("Planner"));
    }
}
