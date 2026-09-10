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
        "review",
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
