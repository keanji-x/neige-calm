//! A fresh real Planner must discover and invoke the compiled development tool itself.
use super::*;

#[tokio::test]
async fn real_planner_discovers_builtin_tool_on_first_turn() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!("no codex bin");
    };
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let goal = "This disposable fixture is a tool availability probe, not an issue-development task. \
        Use your discovered MCP tools directly; do not use shell commands, custom clients or create tasks. \
        First invoke calm.track.publish with idempotency_key=bootstrap-discovery-probe, \
        title=Discovery probe, body=No candidate. Observe its publish-not-a-candidate refusal: \
        this fresh fixture has no delivered attempt. Report the result and stop.".to_string();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            template_id: Some("issue-development".into()),
            template_input: None,
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
        Err(reason) => {
            skip!("{reason}");
        }
    };
    boot_planner_harness_via_start_op(&fx, goal).await;
    let deadline = Instant::now() + planner_planning_budget();
    loop {
        let completed = support::agent_diag::planner_transcript_rows(&fx.repo)
            .await
            .unwrap();
        let publish = completed
            .into_iter()
            .filter(|(_, _, _, method, _)| method == "item/completed")
            .map(|(_, _, _, _, params)| serde_json::from_str::<Value>(&params).unwrap())
            .find(|params| {
                params["item"]["type"] == "mcpToolCall"
                    && params["item"]["server"] == "calm"
                    && params["item"]["tool"] == "calm.track.publish"
            });
        if let Some(publish) = publish {
            assert_eq!(publish["item"]["status"], "failed", "{publish:#}");
            let error = publish["item"]["error"]["message"]
                .as_str()
                .expect("publish candidate refusal");
            assert!(error.contains("publish-not-a-candidate"), "{error}");
            break;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                &fx,
                "fresh Planner did not invoke the compiled publish tool".into(),
            )
            .await;
        }
        sleep(Duration::from_millis(100)).await;
    }
    eprintln!("verified: real Planner invoked publication and it retained its candidate fence");
    shutdown_planner_harness_if_registered(&fx).await;
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    shutdown_shared_codex(&fx.shared).await;
}
