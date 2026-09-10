//! A2 public authoring/report/verdict/claim paths. Only the external provider is fake.
use super::*;
use crate::mcp_track_report::{call_tool, planner_identity};

fn reviewer() -> Value {
    let mut task = declaration(
        "review",
        json!({"role":"candidate_reviewer","producer":"produce","slot":"project","purpose":"candidate-review-input"}),
    );
    task["context"]["neige_execution"]["workspace"] = json!("file-input");
    task
}
async fn review_source() -> (Fixture, Task, String, Value) {
    review_source_scenario("controlled").await
}
async fn review_source_scenario(scenario: &str) -> (Fixture, Task, String, Value) {
    review_source_check(scenario, CHECK).await
}
async fn review_source_check(scenario: &str, check: &str) -> (Fixture, Task, String, Value) {
    review_source_listener(scenario, check, false).await
}
async fn review_source_listener(
    scenario: &str,
    check: &str,
    live: bool,
) -> (Fixture, Task, String, Value) {
    review_source_contract(scenario, check, live, "produce", "project", true).await
}
async fn review_source_contract(
    scenario: &str,
    check: &str,
    live: bool,
    producer_key: &str,
    slot: &str,
    legacy_consumer: bool,
) -> (Fixture, Task, String, Value) {
    let fx = fixture(scenario).await;
    if !live {
        fx.state.dispatcher.abort_event_listener_for_test();
    }
    let mut task = producer(check);
    task["key"] = json!(producer_key);
    task["context"]["neige_execution"]["file_delivery"]["slot"] = json!(slot);
    task["context"]["neige_execution"]["file_delivery"]["policy"]["scope"] =
        json!("review-required");
    task["context"]["neige_execution"]["file_delivery"]["policy"]["reviewer"] = json!("review");
    declare(&fx.boot, task).await;
    schedule(&fx).await;
    let producer = current(&fx.boot, producer_key).await;
    assert_eq!(
        producer.status,
        TaskStatus::Running,
        "{}",
        listed(&fx).await
    );
    let path = workspace(&fx, &producer).await;
    for (name, bytes) in FILES {
        std::fs::write(path.join(name), bytes).unwrap();
    }
    settle(&fx, &producer, true).await;
    let publication = publish(&fx, &producer).await;
    std::fs::remove_dir_all(path).unwrap();
    let mut review = reviewer();
    review["context"]["neige_execution"]["file_delivery"]["producer"] = json!(producer_key);
    review["context"]["neige_execution"]["file_delivery"]["slot"] = json!(slot);
    declare(&fx.boot, review).await;
    if legacy_consumer {
        let mut consume = consumer();
        consume["context"]["neige_execution"]["file_delivery"]["producer"] = json!(producer_key);
        consume["context"]["neige_execution"]["file_delivery"]["slot"] = json!(slot);
        declare(&fx.boot, consume).await;
    }
    schedule(&fx).await;
    let evidence = verified(&fx, &publication).await;
    schedule(&fx).await;
    (fx, producer, publication, evidence)
}
async fn verdict(
    fx: &Fixture,
    task: &Task,
    status: &str,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(&fx.boot, "calm.task.verdict", planner_identity(&fx.boot), json!({"idempotency_key":task.id,"status":status,"reason":"Reviewed exact evidence","message":"Review decision"})).await
}
async fn report_review(fx: &Fixture, passed: bool) -> Task {
    let task = current(&fx.boot, "review").await;
    assert_eq!(task.status, TaskStatus::Running, "{}", listed(fx).await);
    let path = workspace(fx, &task).await;
    for (name, bytes) in FILES {
        assert_eq!(
            std::fs::read(path.join("inputs/source").join(name)).unwrap(),
            bytes.as_bytes()
        );
    }
    std::fs::write(path.join("report-result.json"), json!({"passed":passed,"blocking_findings":if passed {vec![]} else {vec!["double accepts invalid nonnumeric input without a documented contract"]}}).to_string()).unwrap();
    settle(fx, &task, true).await;
    current(&fx.boot, "review").await
}
async fn decision_count(fx: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM task_candidate_decisions")
        .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}
async fn binding(fx: &Fixture, task: &Task) -> Value {
    let raw: String = sqlx::query_scalar(
        "SELECT binding_json FROM task_candidate_input_bindings WHERE attempt_id=?1",
    )
    .bind(&task.id)
    .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    serde_json::from_str(&raw).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_exact_native_report_verdict_and_consumer() {
    let (fx, producer, publication, machine) = review_source().await;
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    assert!(
        verdict(&fx, &producer, "accepted").await.is_err(),
        "early acceptance must fail"
    );
    assert_eq!(decision_count(&fx).await, 0);
    let review = report_review(&fx, true).await;
    let review_binding = binding(&fx, &review).await;
    assert_eq!(review_binding["candidate"], machine["candidate"]);
    assert_eq!(
        review_binding["verification_operation_id"],
        machine["verification_operation_id"]
    );
    verdict(&fx, &review, "accepted").await.unwrap();
    schedule(&fx).await;
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending,
        "report accepted is not code accepted"
    );
    assert_eq!(decision_count(&fx).await, 0);
    verdict(&fx, &producer, "accepted").await.unwrap();
    assert_eq!(decision_count(&fx).await, 1);
    schedule(&fx).await;
    let consume = current(&fx.boot, "consume").await;
    assert_eq!(consume.status, TaskStatus::Running, "{}", listed(&fx).await);
    for (task, expects_summary) in [(&consume, true), (&review, false)] {
        let identity = review_identity(&fx, task).await;
        let card = fx
            .boot
            .repo
            .card_get(&identity.card_id)
            .await
            .unwrap()
            .unwrap();
        let prompt = card.payload["prompt"].as_str().unwrap();
        assert_eq!(
            prompt.contains("worker-summary-v1"),
            expects_summary,
            "{prompt}"
        );
        if expects_summary {
            assert!(prompt.contains("preserve that contract instead of wrapping it"));
            assert!(prompt.contains("not independent kernel verification or Planner acceptance"));
        }
    }
    let input = binding(&fx, &consume).await;
    assert_eq!(input["candidate"], machine["candidate"]);
    assert_eq!(input["candidate"]["publication_operation_id"], publication);
    for (name, bytes) in FILES {
        assert_eq!(
            std::fs::read(
                workspace(&fx, &consume)
                    .await
                    .join("inputs/source")
                    .join(name)
            )
            .unwrap(),
            bytes.as_bytes()
        );
    }
    verdict(&fx, &producer, "accepted").await.unwrap();
    assert_eq!(
        decision_count(&fx).await,
        1,
        "same evidence reuses original decision"
    );
    let view = listed(&fx).await;
    let consumer_view = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "consume")
        .unwrap();
    assert_eq!(
        consumer_view["file_delivery"]["qualified"], true,
        "repeated acceptance preserves claimed qualification: {view}"
    );
    assert!(
        !view.to_string().contains(fx.root.path().to_str().unwrap()),
        "private paths leaked: {view}"
    );
    let text = view.to_string();
    assert!(
        text.contains("report_event_id") && text.contains("decision_event_id"),
        "{view}"
    );
    assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
    settle(&fx, &consume, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_blockers_keep_candidate_and_task_done() {
    let (fx, producer, publication, _) = review_source().await;
    let review = report_review(&fx, false).await;
    verdict(&fx, &review, "accepted").await.unwrap();
    assert!(verdict(&fx, &producer, "accepted").await.is_err());
    verdict(&fx, &producer, "rejected").await.unwrap();
    schedule(&fx).await;
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
    assert_eq!(current(&fx.boot, "review").await.status, TaskStatus::Done);
    let view = listed(&fx).await.to_string();
    assert!(
        view.contains("blocking_findings")
            && view.contains("nonnumeric")
            && view.contains(&publication),
        "{view}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_raw_event_cannot_forge_or_replay_acceptance() {
    let (fx, producer, _, _) = review_source().await;
    report_review(&fx, true).await;
    verdict(&fx, &producer, "accepted").await.unwrap();
    let original: String = sqlx::query_scalar(
        "SELECT event_json FROM task_candidate_decisions ORDER BY event_id LIMIT 1",
    )
    .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    verdict(&fx, &producer, "rejected").await.unwrap();
    let event: calm_server::event::Event = serde_json::from_str(&original).unwrap();
    let actor = planner_identity(&fx.boot).to_actor_id();
    let track = fx
        .boot
        .repo
        .track_get(fx.boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let scope = calm_server::event::EventScope::Track {
        track: track.id,
        area: track.area_id,
    };
    // Real general event writer, deliberately outside the dedicated verdict transaction.
    calm_server::db::write_with_actor_events_typed(
        fx.boot.repo.as_ref(),
        None,
        &fx.boot.ctx.events,
        &fx.boot.ctx.write,
        move |_| Box::pin(async move { Ok(((), vec![(actor, scope, event)])) }),
    )
    .await
    .unwrap();
    let view = listed(&fx).await;
    let producer_view = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["key"] == "produce")
        .unwrap();
    assert_eq!(
        producer_view["file_delivery"]["qualified"], false,
        "an unreceipted event must not qualify the public read, even when foreign keys block claim: {view}"
    );
    schedule(&fx).await;
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    assert_eq!(
        decision_count(&fx).await,
        2,
        "raw JSON cannot create a verdict receipt"
    );
    assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
    verdict(&fx, &producer, "accepted").await.unwrap();
    assert_eq!(decision_count(&fx).await, 3);
    schedule(&fx).await;
    let consume = current(&fx.boot, "consume").await;
    assert_eq!(consume.status, TaskStatus::Running, "{}", listed(&fx).await);
    settle(&fx, &consume, true).await;
}
async fn review_identity(fx: &Fixture, task: &Task) -> calm_server::mcp_server::ToolCallIdentity {
    let op = fx
        .state
        .operation_runtime
        .find_by_kind_and_idempotency("codex-isolated-worker", &task.id)
        .await
        .unwrap()
        .unwrap();
    let identity =
        &op.tx_output.as_ref().unwrap().data["isolated_execution"]["request"]["identity"];
    let mut caller = planner_identity(&fx.boot);
    caller.role = calm_server::model::CardRole::Worker;
    caller.provider = calm_server::session_projection_repo::AgentProvider::Codex;
    caller.card_id = identity["card_id"].as_str().unwrap().to_owned();
    caller.session_id = identity["session_id"].as_str().unwrap().to_owned();
    caller
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_report_schema_identity_and_replay_are_fenced() {
    let (fx, producer, _, _) = review_source().await;
    let review = current(&fx.boot, "review").await;
    let identity = review_identity(&fx, &review).await;
    for result in [
        json!({"$neige_result_presentation":"worker-summary-v1","summary":"review claims pass","details":{"passed":true,"blocking_findings":[]}}),
        json!({"passed":true}),
        json!({"passed":false,"blocking_findings":[]}),
        json!({"passed":true,"blocking_findings":["blocker"]}),
        json!({"passed":true,"blocking_findings":[],"subject":"forged"}),
    ] {
        let response = call_tool(
            &fx.boot,
            "calm.task.complete",
            identity.clone(),
            json!({"idempotency_key":review.id,"result":result,"artifacts":[]}),
        )
        .await;
        assert!(response.is_err(), "{response:?}");
    }
    let mut foreign = identity.clone();
    foreign.session_id = review_identity(&fx, &producer).await.session_id;
    assert!(call_tool(&fx.boot,"calm.task.complete",foreign,json!({"idempotency_key":review.id,"result":{"passed":true,"blocking_findings":[]},"artifacts":[]})).await.is_err());
    assert_eq!(
        current(&fx.boot, "review").await.status,
        TaskStatus::Running
    );
    report_review(&fx, true).await;
    call_tool(&fx.boot,"calm.task.complete",identity.clone(),json!({"idempotency_key":review.id,"result":{"passed":true,"blocking_findings":[]},"artifacts":[]})).await.unwrap();
    assert!(call_tool(&fx.boot,"calm.task.complete",identity,json!({"idempotency_key":review.id,"result":{"passed":false,"blocking_findings":["changed report"]},"artifacts":[]})).await.is_err());
    verdict(&fx, &producer, "accepted").await.unwrap();
    schedule(&fx).await;
    settle(&fx, &current(&fx.boot, "consume").await, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_recovery_preserves_exact_decision() {
    let (fx, producer, _, _) = review_source().await;
    report_review(&fx, true).await;
    verdict(&fx, &producer, "accepted").await.unwrap();
    schedule(&fx).await;
    let first = current(&fx.boot, "consume").await;
    let original = binding(&fx, &first).await;
    let id: i64 = sqlx::query_scalar(
        "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
    )
    .bind(&first.id)
    .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    settle(&fx, &first, false).await;
    call_tool(&fx.boot,"calm.plan.recover",planner_identity(&fx.boot),json!({"key":"consume","expected_attempt_id":first.id,"idempotency_key":"same-review-input","reason":"Retry with the exact accepted candidate."})).await.unwrap();
    schedule(&fx).await;
    let second = current(&fx.boot, "consume").await;
    assert_eq!(second.status, TaskStatus::Running, "{}", listed(&fx).await);
    assert_ne!(first.id, second.id);
    assert_eq!(binding(&fx, &second).await, original);
    let next: i64 = sqlx::query_scalar(
        "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
    )
    .bind(&second.id)
    .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert_eq!(id, next);
    settle(&fx, &second, false).await;
    verdict(&fx, &producer, "rejected").await.unwrap();
    let view = listed(&fx).await;
    let consumer = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "consume")
        .unwrap();
    assert_eq!(consumer["file_delivery"]["qualified"], false, "{view}");
    assert_eq!(consumer["recovery"]["allowed"], false, "{view}");
    assert_eq!(consumer["file_delivery"]["input"]["decision_event_id"], id);
    assert_eq!(
        consumer["file_delivery"]["review"]["passed"], true,
        "historical review survives rejection: {view}"
    );
    assert_eq!(
        consumer["file_delivery"]["verification"]["passed"], true,
        "historical machine result survives rejection: {view}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_preturn_rejects_revocation_and_corrupt_bytes() {
    for defect in [
        "reject",
        "withdrawal",
        "review-withdrawal",
        "consumer-withdrawal",
        "bytes",
        "reaccept",
    ] {
        let (fx, producer, _, _) = review_source_scenario("candidate-review-preturn").await;
        report_review(&fx, true).await;
        verdict(&fx, &producer, "accepted").await.unwrap();
        let scheduler = fx.state.dispatcher.scheduler();
        scheduler.schedule_track(fx.boot.track_id.clone()).await;
        let consume = current(&fx.boot, "consume").await;
        let path = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(op) = fx
                    .state
                    .operation_runtime
                    .find_by_kind_and_idempotency("codex-isolated-worker", &consume.id)
                    .await
                    .unwrap()
                    && let Some(output) = op.tx_output
                    && let Some(path) =
                        output.data["isolated_execution"]["request"]["workspace"].as_str()
                {
                    let path = PathBuf::from(path);
                    if path.join("await-preturn").exists() {
                        break path;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let original = binding(&fx, &consume).await;
        let decision: i64 = sqlx::query_scalar(
            "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
        )
        .bind(&consume.id)
        .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        match defect {
            "reject" => {
                verdict(&fx, &producer, "rejected").await.unwrap();
            }
            "reaccept" => {
                verdict(&fx, &producer, "rejected").await.unwrap();
                verdict(&fx, &producer, "accepted").await.unwrap();
            }
            "withdrawal" | "review-withdrawal" | "consumer-withdrawal" => {
                let subject = match defect {
                    "review-withdrawal" => current(&fx.boot, "review").await.id,
                    "consumer-withdrawal" => consume.id.clone(),
                    _ => producer.id.clone(),
                };
                sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
                    .bind(subject)
                    .execute(&fx.boot.repo.sqlite_pool().unwrap())
                    .await
                    .unwrap();
            }
            "bytes" => {
                std::fs::write(
                    path.join("inputs/source/project.py"),
                    b"tampered before first turn",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        std::fs::write(path.join("resume-preturn"), b"").unwrap();
        schedule(&fx).await;
        assert_eq!(
            current(&fx.boot, "consume").await.status,
            TaskStatus::Failed,
            "{defect}: {}",
            listed(&fx).await
        );
        assert!(
            !path.join("result.txt").exists(),
            "{defect}: first turn must not start"
        );
        assert_eq!(binding(&fx, &consume).await, original);
        let retained: i64 = sqlx::query_scalar(
            "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
        )
        .bind(&consume.id)
        .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        assert_eq!(decision, retained, "{defect}: never silently rebind");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_failed_machine_never_starts_reviewer() {
    let (fx, producer, _, machine) = review_source_check("controlled", "exit 6").await;
    assert_eq!(machine["verdict"]["passed"], false);
    assert_eq!(
        current(&fx.boot, "review").await.status,
        TaskStatus::Pending
    );
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    assert!(verdict(&fx, &producer, "accepted").await.is_err());
    assert_eq!(decision_count(&fx).await, 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_wrong_execution_or_machine_evidence_blocks_acceptance() {
    for defect in [
        "review-session",
        "review-attempt",
        "machine-policy",
        "machine-subject",
    ] {
        let (fx, producer, _, machine) = review_source().await;
        let review = report_review(&fx, true).await;
        let op = fx
            .state
            .operation_runtime
            .find_by_kind_and_idempotency("codex-isolated-worker", &review.id)
            .await
            .unwrap()
            .unwrap();
        let (id, path, value) = match defect {
            "review-session" => (
                op.id.to_string(),
                "$.data.isolated_execution.request.identity.session_id",
                json!("different-session"),
            ),
            "review-attempt" => (
                op.id.to_string(),
                "$.data.isolated_execution.request.identity.attempt_id",
                json!(producer.id),
            ),
            "machine-policy" => (
                machine["verification_operation_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                "$.result.policy.timeout_secs",
                json!(21),
            ),
            _ => (
                machine["verification_operation_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                "$.result.candidate.snapshot",
                json!("0".repeat(64)),
            ),
        };
        sqlx::query(
            "UPDATE operations SET tx_output_json=json_set(tx_output_json,?1,json(?2)) WHERE id=?3",
        )
        .bind(path)
        .bind(value.to_string())
        .bind(id)
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        assert!(
            verdict(&fx, &producer, "accepted").await.is_err(),
            "{defect}"
        );
        schedule(&fx).await;
        assert_eq!(
            current(&fx.boot, "consume").await.status,
            TaskStatus::Pending,
            "{defect}"
        );
        assert_eq!(decision_count(&fx).await, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_review_qualification_receipt_failure_rolls_back_decision_event() {
    let (fx, producer, _, _) = review_source().await;
    report_review(&fx, true).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let before:i64=sqlx::query_scalar("SELECT count(*) FROM events WHERE scope_track=?1 AND scope_kind='track' AND kind='task.completed'").bind(&producer.track_id).fetch_one(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER refuse_candidate_receipt BEFORE INSERT ON task_candidate_decisions BEGIN SELECT RAISE(ABORT,'injected receipt write failure'); END").execute(&pool).await.unwrap();
    let error = verdict(&fx, &producer, "accepted").await.unwrap_err();
    assert!(
        error.message.contains("injected receipt write failure"),
        "{error:?}"
    );
    let after:i64=sqlx::query_scalar("SELECT count(*) FROM events WHERE scope_track=?1 AND scope_kind='track' AND kind='task.completed'").bind(&producer.track_id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        before, after,
        "event cannot survive a failed receipt transaction"
    );
    assert_eq!(decision_count(&fx).await, 0);
    assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
    sqlx::query("DROP TRIGGER refuse_candidate_receipt")
        .execute(&pool)
        .await
        .unwrap();
    verdict(&fx, &producer, "accepted").await.unwrap();
    assert!(
        sqlx::query("UPDATE task_candidate_decisions SET event_json='{}'")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[path = "candidate_review_settlement.rs"]
mod settlement;

#[path = "candidate_repair.rs"]
mod repair;

#[path = "candidate_review_dispatch.rs"]
mod dispatch;
