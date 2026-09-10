use super::*;
use crate::mcp_task_candidate_dispatch::candidate_args;
use crate::mcp_task_dispatch::{bind_planner, counts, dispatch, payload};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_candidate_waits_for_acceptance_then_prepares_exact_files_and_decision() {
    let (fx, producer, publication, machine) = review_source_contract(
        "controlled",
        CHECK,
        false,
        "release-2",
        "release_bundle",
        false,
    )
    .await;
    bind_planner(&fx.boot, &planner_identity(&fx.boot).session_id, false).await;
    report_review(&fx, true).await;
    // Reviewing is already schedulable: dispatch/verdict need no report edit.
    sqlx::query("UPDATE tracks SET lifecycle='reviewing' WHERE id=?1")
        .bind(fx.boot.track_id.as_str())
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let first = dispatch(&fx.boot, candidate_args()).await.unwrap();
    assert_eq!(first["current"]["track"]["lifecycle"], "reviewing");
    assert_eq!(
        first["current"]["track"]["lifecycle_allows_scheduling"],
        true
    );
    let key = first["receipt"]["task_key"].as_str().unwrap();
    let report = payload(&fx.boot).await;
    schedule(&fx).await;
    assert_eq!(current(&fx.boot, key).await.status, TaskStatus::Pending);
    assert_eq!(decision_count(&fx).await, 0);
    let waiting = dispatch(&fx.boot, candidate_args()).await.unwrap();
    let input = &waiting["current"]["candidate_input"];
    assert_eq!(input["kind"], "input-admission");
    assert_eq!(input["contract"]["producer"], "release-2");
    assert_eq!(input["contract"]["slot"], "release_bundle");
    assert_eq!(input["candidate"]["publication_operation_id"], publication);
    assert_eq!(input["qualified"], false);
    assert!(
        input["qualification"]["reason"]
            .as_str()
            .unwrap()
            .contains("acceptance"),
        "{input}"
    );

    verdict(&fx, &producer, "accepted").await.unwrap();
    schedule(&fx).await;
    let consume = current(&fx.boot, key).await;
    assert_eq!(consume.status, TaskStatus::Running, "{}", listed(&fx).await);
    let actual = binding(&fx, &consume).await;
    assert_eq!(actual["candidate"], machine["candidate"]);
    assert_eq!(
        actual["verification_operation_id"],
        machine["verification_operation_id"]
    );
    assert_eq!(actual["candidate"]["source"]["task_id"], producer.id);
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let (state, prepared): (String, Option<String>) = sqlx::query_as(
        "SELECT state,prepared_operation_id FROM task_candidate_input_bindings WHERE attempt_id=?1",
    )
    .bind(&consume.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "prepared");
    assert!(prepared.is_some());
    let accepted: i64 = sqlx::query_scalar(
        "SELECT event_id FROM task_candidate_decisions WHERE producer_attempt_id=?1",
    )
    .bind(&producer.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let bound: i64 = sqlx::query_scalar(
        "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
    )
    .bind(&consume.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bound, accepted);
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
    assert_eq!(payload(&fx.boot).await, report);
    let saved = counts(&fx.boot).await;
    let replay = dispatch(&fx.boot, candidate_args()).await.unwrap();
    assert_eq!(replay["receipt"], first["receipt"]);
    assert_eq!(counts(&fx.boot).await, saved);
    assert_eq!(saved.0, 1);
    assert_eq!(replay["current"]["allocation"]["attempt_id"], consume.id);
    let input = &replay["current"]["candidate_input"];
    assert_eq!(input["qualified"], true);
    assert_eq!(input["preparation"]["state"], "prepared");
    assert_eq!(input["preparation"]["decision_event_id"], accepted);
    assert_eq!(input["candidate"]["publication_operation_id"], publication);
    for excluded in [
        "history",
        "policy",
        "goal",
        "blocking_findings",
        "steps",
        "log_tail",
    ] {
        assert!(
            !input.to_string().contains(&format!("\"{excluded}\":")),
            "{input}"
        );
    }
    assert!(replay["current"].get("file_delivery").is_none());
    // The scheduler, not a Planner report edit, advances lifecycle on actual start.
    assert_eq!(replay["current"]["track"]["lifecycle"], "working");
    settle(&fx, &consume, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_candidate_diagnostic_preserves_failed_publication_and_pending_distinction() {
    let fx = fixture("controlled").await;
    fx.state.dispatcher.abort_event_listener_for_test();
    bind_planner(&fx.boot, &planner_identity(&fx.boot).session_id, false).await;
    let mut source = producer(CHECK);
    source["key"] = json!("release-2");
    source["context"]["neige_execution"]["file_delivery"]["slot"] = json!("release_bundle");
    declare(&fx.boot, source).await;
    schedule(&fx).await;
    let producer = current(&fx.boot, "release-2").await;
    assert_eq!(producer.status, TaskStatus::Running);
    let pending = dispatch(&fx.boot, candidate_args()).await.unwrap();
    let key = pending["receipt"]["task_key"].as_str().unwrap();
    // Intentionally omit the declared files: the real publication capture must fail.
    settle(&fx, &producer, true).await;
    schedule(&fx).await;
    let publication = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(op) = fx.state.operation_runtime.find_by_kind_and_idempotency(
                "task-file-publication", &format!("file:{}", producer.id),
            ).await.unwrap() {
                let result = fx.state.operation_runtime.wait(&op.id).await.unwrap();
                assert!(matches!(result.outcome, OperationOutcome::Failed { .. }), "{result:?}");
                // Wait for the existing source driver to finish its settlement event
                // before asserting replay writes nothing.
                let settled: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE kind='task.file_publication_settled' AND json_extract(payload,'$.operation_id')=?1)")
                    .bind(&op.id).fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
                if settled { break op.id; }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    let sealed: i64 = sqlx::query_scalar("SELECT count(*) FROM task_file_candidates")
        .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(sealed, 0);
    let view = listed(&fx).await;
    let delivery = &view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == key)
        .unwrap()["file_delivery"];
    assert_eq!(delivery["publication"]["state"], "failed");
    assert!(
        !delivery["publication"]["failure"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let before = payload(&fx.boot).await;
    let saved = counts(&fx.boot).await;
    let failed = dispatch(&fx.boot, candidate_args()).await.unwrap();
    assert_eq!(failed["receipt"], pending["receipt"]);
    assert_eq!(counts(&fx.boot).await, saved);
    assert_eq!(payload(&fx.boot).await, before);
    assert_eq!(current(&fx.boot, key).await.status, TaskStatus::Pending);
    let input = &failed["current"]["candidate_input"];
    // This fails before the projection fix, after actual publication failure is proven.
    assert_eq!(input["publication"], delivery["publication"]);
    assert_eq!(input["publication"]["operation_id"], publication);
    assert_eq!(input["qualified"], false);
    assert_eq!(
        pending["current"]["candidate_input"]["publication"]["state"],
        "waiting"
    );
    assert!(input.get("history").is_none());
    assert!(input["verification"].get("policy").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_candidate_diagnostic_preserves_failed_reviewer_reason() {
    let (fx, _, _, _) = review_source_contract(
        "controlled",
        CHECK,
        false,
        "release-2",
        "release_bundle",
        false,
    )
    .await;
    bind_planner(&fx.boot, &planner_identity(&fx.boot).session_id, false).await;
    let review = current(&fx.boot, "review").await;
    settle(&fx, &review, false).await;
    let first = dispatch(&fx.boot, candidate_args()).await.unwrap();
    let key = first["receipt"]["task_key"].as_str().unwrap();
    let full = listed(&fx).await;
    let delivery = &full["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == key)
        .unwrap()["file_delivery"];
    assert_eq!(delivery["review"]["state"], "failed");
    assert_eq!(delivery["review"]["reason"], "fixture requested failure");
    assert_eq!(delivery["review"]["operation"]["state"], "failed");
    let input = &first["current"]["candidate_input"];
    assert_eq!(input["review"]["state"], "failed");
    assert_eq!(input["review"]["reason"], delivery["review"]["reason"]);
    assert_eq!(
        input["review"]["operation"],
        delivery["review"]["operation"]
    );
    assert_eq!(input["review"]["review_attempt_id"], review.id);
    assert_eq!(input["qualified"], false);
    assert_eq!(current(&fx.boot, key).await.status, TaskStatus::Pending);
    assert!(input["review"].get("blocking_findings").is_none());
    assert!(input["review"].get("finding_responses").is_none());
    let saved = counts(&fx.boot).await;
    let report = payload(&fx.boot).await;
    let replay = dispatch(&fx.boot, candidate_args()).await.unwrap();
    assert_eq!(replay["receipt"], first["receipt"]);
    assert_eq!(counts(&fx.boot).await, saved);
    assert_eq!(payload(&fx.boot).await, report);
}
