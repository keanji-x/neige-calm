//! Real-model Calendar usability probe, executed only by the Docker-isolated runner.
use super::*;

#[tokio::test]
async fn real_planner_creates_calendar_commitment() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let goal = include_str!("../../prompts/tests/calendar-preview-request.md")
        .trim()
        .to_string();
    let expected: Value = serde_json::from_str(include_str!(
        "../../prompts/tests/calendar-preview-expected.json"
    ))
    .unwrap();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            bound_issue: None,
            plan_source: PlanSource::RealPlannerTurn,
            issue_body: None,
            require_task_gates: false,
            repo_seed: RepoSeed::ReadmeOnly,
        },
        codex_bin,
    )
    .await
    {
        Ok(fx) => fx,
        Err(reason) => skip!("{reason}"),
    };
    // Represent an ordinary unbound Track. Do not widen the development-owner policy.
    sqlx::query("UPDATE tracks SET plugin_scope=NULL WHERE id=?")
        .bind(fx.track_id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    fx.plugin_host.reconcile_builtins().await.unwrap();
    fx.plugin_host.enable("dev.neige.calendar").await.unwrap();
    assert!(
        fx.repo
            .plugin_kv_list("dev.neige.calendar", "entry:")
            .await
            .unwrap()
            .is_empty()
    );
    boot_planner_harness_via_start_op(&fx, goal).await;
    let deadline = Instant::now() + planner_planning_budget();
    let (entry, calls) = loop {
        let entries = fx
            .repo
            .plugin_kv_list("dev.neige.calendar", "entry:")
            .await
            .unwrap();
        let rows = support::agent_diag::planner_transcript_rows(&fx.repo)
            .await
            .unwrap();
        let calls = rows
            .into_iter()
            .filter(|(_, _, _, method, _)| method == "item/completed")
            .map(|(_, _, _, _, params)| serde_json::from_str::<Value>(&params).unwrap())
            .filter(|params| {
                params["item"]["type"] == "mcpToolCall"
                    && params["item"]["server"] == "calm"
                    && params["item"]["tool"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("calm.calendar."))
            })
            .map(|params| params["item"].clone())
            .collect::<Vec<_>>();
        if entries.len() == 1 {
            let entry = &entries[0].1;
            let successful = |call: &Value| {
                call["status"] == "completed"
                    && call["error"].is_null()
                    && call["result"]["isError"] != true
            };
            let created = calls.iter().position(|call| {
                successful(call)
                    && call["tool"] == "calm.calendar.create"
                    && call["result"]["structuredContent"]["id"] == entry["id"]
            });
            if created.is_some_and(|index| {
                calls.iter().skip(index + 1).any(|call| {
                    successful(call)
                        && call["tool"] == "calm.calendar.list"
                        && call["result"]["structuredContent"]
                            .as_array()
                            .is_some_and(|listed| listed.iter().any(|item| item == entry))
                })
            }) {
                break (entry.clone(), calls);
            }
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                &fx,
                "Planner did not create and verify the calendar task".into(),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(entry["task"]["title"], expected["title"]);
    assert_eq!(entry["task"]["description"], expected["description"]);
    assert_eq!(entry["cancelled"], false);
    assert_eq!(
        entry["task"]["schedule"]["start"],
        "2026-10-02T09:00:00+08:00"
    );
    assert_eq!(
        entry["task"]["schedule"]["end"],
        "2026-10-02T10:00:00+08:00"
    );
    assert_eq!(entry["task"]["schedule"]["timezone"], "Asia/Shanghai");
    assert_eq!(entry["source_track_id"], fx.track_id.as_str());
    assert_eq!(entry["created_by"], format!("card:{}", fx.planner_card_id));
    assert!(!fx.used_injected_plan());
    let harness = recover_planner_harness(&fx)
        .await
        .expect("live Planner harness");
    wait_for_planner_turn_settled(&fx, &harness, planner_planning_budget()).await;
    let final_entries = fx
        .repo
        .plugin_kv_list("dev.neige.calendar", "entry:")
        .await
        .unwrap();
    assert_eq!(final_entries.len(), 1);
    assert_eq!(final_entries[0].1, entry);
    eprintln!("CALENDAR_PLANNER_ENTRY={entry}");
    for call in calls {
        eprintln!("CALENDAR_PLANNER_CALL={call}");
    }
    let rows = support::agent_diag::planner_transcript_rows(&fx.repo)
        .await
        .unwrap();
    let mut confirmed = false;
    for (_, _, _, method, params) in rows {
        if method == "item/completed" {
            let value: Value = serde_json::from_str(&params).unwrap();
            if value["item"]["type"] == "agentMessage" {
                confirmed |= value["item"]["phase"] == "final_answer"
                    && value["item"]["text"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty());
                eprintln!("CALENDAR_PLANNER_REPLY={}", value["item"]);
            }
        }
    }
    assert!(
        confirmed,
        "Planner must reply after verifying the saved task"
    );
    shutdown_planner_harness_if_registered(&fx).await;
    fx.plugin_host.stop("dev.neige.calendar").await.unwrap();
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    shutdown_shared_codex(&fx.shared).await;
}
