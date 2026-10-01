//! A fresh real Planner must discover and invoke the compiled development tools itself.
use super::*;

#[tokio::test]
async fn real_planner_discovers_builtin_tools_on_first_turn() {
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
        this fresh fixture has no delivered attempt. Then invoke calm.review.round with subject \
        {phase: design, slice_id: bootstrap-discovery-probe}, n=1, cap=3, converged=true, and \
        channels [{role: probe-a, verdict: approved}, {role: probe-b, verdict: approved}]. \
        These channel verdicts are synthetic probe inputs; they do not claim real reviews occurred. \
        Report the two results and stop.".to_string();
    let fx = match boot_forge_e2e_fixture(
        FixtureSpec {
            goal: Some(goal.clone()),
            template_id: Some("issue-development".into()),
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
    assert!(
        actor_payload_rows(&fx.repo, "review.round")
            .await
            .is_empty()
    );
    boot_planner_harness_via_start_op(&fx, goal).await;
    let deadline = Instant::now() + planner_planning_budget();
    loop {
        let review = actor_payload_rows(&fx.repo, "review.round")
            .await
            .into_iter()
            .find(|(actor, payload)| {
                matches!(actor, ActorId::AiPlannerSession(_))
                    && payload["subject"]["slice_id"] == "bootstrap-discovery-probe"
            });
        let completed: Vec<(String,)> =
            sqlx::query_as("SELECT params FROM harness_items WHERE method = 'item/completed'")
                .fetch_all(fx.repo.pool())
                .await
                .unwrap();
        let publish = completed
            .into_iter()
            .map(|(params,)| serde_json::from_str::<Value>(&params).unwrap())
            .find(|params| {
                params["item"]["type"] == "mcpToolCall"
                    && params["item"]["server"] == "calm"
                    && params["item"]["tool"] == "calm.track.publish"
            });
        if let (Some((_actor, review)), Some(publish)) = (review, publish) {
            assert_eq!(review["converged"], true);
            assert_eq!(review["n"], 1);
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
                "fresh Planner did not invoke both compiled tools".into(),
            )
            .await;
        }
        sleep(Duration::from_millis(100)).await;
    }
    shutdown_planner_harness_if_registered(&fx).await;
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    shutdown_shared_codex(&fx.shared).await;
}
