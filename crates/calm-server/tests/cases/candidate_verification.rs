//! A1 production paths; only the external Codex provider is faked.
use crate::{
    file_delivery::{listed, publish, schedule, settle, workspace},
    isolated_codex_smoke::{Fixture, fixture},
    task_recovery::{current, declare},
};
use calm_server::{
    model::{Task, TaskStatus},
    operation::OperationOutcome,
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
fn declaration(key: &str, delivery: Value) -> Value {
    json!({"key":key,"kind":"codex","goal":"Write or consume the explicitly declared project files.","declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,"no_gate_reason":"Post-publication candidate machine checks.","context":{"neige_execution":{"version":"isolated-codex-v1","workspace":if delivery["role"]=="candidate_consumer" {"file-input"} else {"empty"},"file_delivery":delivery}}})
}
fn producer(command: &str) -> Value {
    declaration(
        "produce",
        json!({"role":"candidate_producer","slot":"project","paths":["project.py","README.md","test_project.py"],"policy":{"scope":"declared-checks-only","timeout_secs":20,"steps":[{"name":"stdlib-tests","cmd":command}]}}),
    )
}
fn consumer() -> Value {
    declaration(
        "consume",
        json!({"role":"candidate_consumer","producer":"produce","slot":"project","purpose":"verified-candidate-input"}),
    )
}
const CHECK: &str = "python3 -c 'import unittest; s=unittest.defaultTestLoader.discover(\".\", pattern=\"test_*.py\"); assert s.countTestCases()==2; r=unittest.TextTestRunner().run(s); assert r.wasSuccessful()'";
const FILES: [(&str, &str); 3] = [
    ("project.py", "def double(x):\n    return x * 2\n"),
    ("README.md", "A sealed tiny project.\n"),
    (
        "test_project.py",
        "import unittest\nfrom project import double\nclass Project(unittest.TestCase):\n    def test_positive(self): self.assertEqual(double(21),42)\n    def test_zero(self): self.assertEqual(double(0),0)\n",
    ),
];
async fn source(command: &str) -> (Fixture, Task, PathBuf, String) {
    source_scenario(command, "controlled").await
}
async fn source_scenario(command: &str, scenario: &str) -> (Fixture, Task, PathBuf, String) {
    let fx = fixture(scenario).await;
    fx.state.dispatcher.abort_event_listener_for_test();
    declare(&fx.boot, producer(command)).await;
    schedule(&fx).await;
    let task = current(&fx.boot, "produce").await;
    assert_eq!(task.status, TaskStatus::Running, "{}", listed(&fx).await);
    let path = workspace(&fx, &task).await;
    for (name, bytes) in FILES {
        std::fs::write(path.join(name), bytes).unwrap();
    }
    settle(&fx, &task, true).await;
    let publication = publish(&fx, &task).await;
    (fx, task, path, publication)
}
async fn verification(fx: &Fixture, publication: &str) -> calm_server::operation::Operation {
    let observed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(op) = fx
                .state
                .operation_runtime
                .find_by_kind_and_idempotency(
                    "candidate-verify",
                    &format!("candidate:{publication}"),
                )
                .await
                .unwrap()
            {
                return op;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    match observed {
        Ok(op) => op,
        Err(error) => {
            let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as("SELECT a.operation_key,o.id,o.phase FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key WHERE a.publication_operation_id=?1")
                .bind(publication).fetch_all(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
            panic!(
                "verification {publication} timed out: {error}; allocation/operation state: {rows:?}"
            );
        }
    }
}
async fn verified(fx: &Fixture, publication: &str) -> Value {
    let op = verification(fx, publication).await;
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        fx.state.operation_runtime.wait(&op.id),
    )
    .await
    .unwrap()
    .unwrap();
    let OperationOutcome::Succeeded { result } = result.outcome else {
        panic!("{result:?}");
    };
    result
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_exact_files_survive_source_deletion_before_real_checks_and_consumer()
 {
    let (fx, task, path, publication) = source(CHECK).await;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind='candidate-verify'")
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(
        count, 0,
        "candidate capture must not secretly execute checks"
    );
    std::fs::write(
        path.join("project.py"),
        "raise RuntimeError('wrong source')",
    )
    .unwrap();
    std::fs::remove_dir_all(path).unwrap();
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let evidence = verified(&fx, &publication).await;
    assert_eq!(evidence["verdict"]["passed"], true, "{evidence}");
    assert!(
        evidence["verdict"]["log_tail"]
            .as_str()
            .unwrap()
            .contains("Ran 2 tests")
    );
    schedule(&fx).await;
    let consumer = current(&fx.boot, "consume").await;
    assert_eq!(
        consumer.status,
        TaskStatus::Running,
        "{}",
        listed(&fx).await
    );
    for (name, bytes) in FILES {
        assert_eq!(
            std::fs::read(
                workspace(&fx, &consumer)
                    .await
                    .join("inputs/source")
                    .join(name)
            )
            .unwrap(),
            bytes.as_bytes()
        );
    }
    let raw: String = sqlx::query_scalar(
        "SELECT binding_json FROM task_candidate_input_bindings WHERE attempt_id=?1",
    )
    .bind(&consumer.id)
    .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    let binding: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(binding["candidate"], evidence["candidate"]);
    assert_eq!(
        binding["verification_operation_id"],
        evidence["verification_operation_id"]
    );
    assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
    assert_eq!(current(&fx.boot, "produce").await.gate_attempt, 0);
    assert_eq!(evidence["candidate"]["source"]["task_id"], task.id);
    settle(&fx, &consumer, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_named_failure_preserves_snapshot_and_blocks_consumer() {
    let (fx, _, _, publication) = source("python3 -c 'raise SystemExit(7)'").await;
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let evidence = verified(&fx, &publication).await;
    assert_eq!(evidence["verdict"]["passed"], false);
    assert_eq!(evidence["verdict"]["exit_code"], 7);
    assert_eq!(evidence["verdict"]["failing_step"], "stdlib-tests");
    schedule(&fx).await;
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_file_candidates WHERE operation_id=?1")
            .bind(&publication)
            .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert!(
        !listed(&fx)
            .await
            .to_string()
            .contains("full test coverage passed")
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_capacity_waits_without_claiming_consumer_or_inventing_success() {
    let (fx, task, _, publication) = source(CHECK).await;
    sqlx::query("UPDATE tracks SET task_budget=0 WHERE id=?1")
        .bind(&task.track_id)
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    assert!(
        fx.state
            .operation_runtime
            .find_by_kind_and_idempotency("candidate-verify", &format!("candidate:{publication}"))
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE tracks SET task_budget=1 WHERE id=?1")
        .bind(&task.track_id)
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let admission = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            schedule(&fx).await;
            if fx
                .state
                .operation_runtime
                .find_by_kind_and_idempotency(
                    "candidate-verify",
                    &format!("candidate:{publication}"),
                )
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if let Err(error) = admission {
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as("SELECT a.operation_key,o.id,o.phase FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key WHERE a.publication_operation_id=?1")
            .bind(&publication).fetch_all(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
        panic!(
            "budget increase did not admit verification after scheduler ticks: {error}; allocation/operation state: {rows:?}"
        );
    }
    assert_eq!(verified(&fx, &publication).await["verdict"]["passed"], true);
    schedule(&fx).await;
    let consumer = current(&fx.boot, "consume").await;
    assert_eq!(consumer.status, TaskStatus::Running);
    settle(&fx, &consumer, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_release_fences_integrity_authority_and_lease_before_any_check() {
    use std::sync::Arc;
    for failure in ["integrity", "withdrawal", "lease"] {
        let (fx, task, _, publication) =
            source("python3 -c 'open(\"business-ran\",\"w\").write(\"yes\")'").await;
        let pool = fx.boot.repo.sqlite_pool().unwrap();
        let task_id = task.id.clone();
        let publication_id = publication.clone();
        let recorded_path = Arc::new(std::sync::Mutex::new(None));
        let observed = recorded_path.clone();
        let _hook = calm_server::file_delivery::install_candidate_release_hook(
            &publication,
            Arc::new(move |path| {
                let pool = pool.clone();
                let task_id = task_id.clone();
                let publication_id = publication_id.clone();
                *observed.lock().unwrap() = Some(path.clone());
                Box::pin(async move {
                    match failure {
                        "integrity" => std::fs::write(
                            path.join("input/source/project.py"),
                            b"changed after preparation",
                        )
                        .unwrap(),
                        "withdrawal" => {
                            sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
                                .bind(task_id)
                                .execute(&pool)
                                .await
                                .unwrap();
                        }
                        "lease" => {
                            sqlx::query("UPDATE operations SET lease_owner='replacement' WHERE kind='candidate-verify' AND idempotency_key=?1").bind(format!("candidate:{publication_id}")).execute(&pool).await.unwrap();
                        }
                        _ => unreachable!(),
                    }
                })
            }),
        );
        declare(&fx.boot, consumer()).await;
        schedule(&fx).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if recorded_path.lock().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let path: PathBuf = recorded_path.lock().unwrap().clone().unwrap();
        let op = verification(&fx, &publication).await;
        // A stolen lease remains recoverable rather than admitting or self-completing.
        if failure != "lease" {
            let result = fx.state.operation_runtime.wait(&op.id).await.unwrap();
            assert!(matches!(result.outcome, OperationOutcome::Failed { .. }));
        } else {
            assert!(!path.join("input/source/business-ran").exists());
            sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
                .bind(&task.id)
                .execute(&fx.boot.repo.sqlite_pool().unwrap())
                .await
                .unwrap();
            fx.state
                .operation_runtime
                .apply_recovery(fx.state.operation_runtime.recover_on_boot().await.unwrap())
                .await
                .unwrap();
        }
        assert!(
            !path.join("input/source/business-ran").exists(),
            "{failure}"
        );
        assert_ne!(
            current(&fx.boot, "consume").await.status,
            TaskStatus::Running
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_mismatched_evidence_never_qualifies_claim() {
    for field in ["snapshot", "policy", "operation"] {
        let (fx, _, _, publication) = source(CHECK).await;
        schedule(&fx).await;
        let evidence = verified(&fx, &publication).await;
        let id = evidence["verification_operation_id"].as_str().unwrap();
        let path = match field {
            "snapshot" => "$.result.candidate.snapshot",
            "policy" => "$.result.policy.timeout_secs",
            _ => "$.result.verification_operation_id",
        };
        sqlx::query(
            "UPDATE operations SET tx_output_json=json_set(tx_output_json,?1,?2) WHERE id=?3",
        )
        .bind(path)
        .bind(if field == "snapshot" {
            "0".repeat(64)
        } else {
            "different".into()
        })
        .bind(id)
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
        declare(&fx.boot, consumer()).await;
        schedule(&fx).await;
        assert_eq!(
            current(&fx.boot, "consume").await.status,
            TaskStatus::Pending,
            "{field}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_preturn_rechecks_materialized_bytes() {
    let (fx, _, _, publication) = source_scenario(CHECK, "candidate-corrupt").await;
    schedule(&fx).await;
    assert_eq!(verified(&fx, &publication).await["verdict"]["passed"], true);
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let task = current(&fx.boot, "consume").await;
    assert_eq!(task.status, TaskStatus::Failed, "{}", listed(&fx).await);
    let op = fx
        .state
        .operation_runtime
        .find_by_kind_and_idempotency("codex-isolated-worker", &task.id)
        .await
        .unwrap()
        .unwrap();
    fx.state.operation_runtime.wait(&op.id).await.unwrap();
    assert!(!workspace(&fx, &task).await.join("result.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_recovery_reexecutes_same_snapshot_and_policy_after_recorded_interruption()
 {
    let command = format!(
        "python3 -c 'import time; open(\"started\",\"w\").close(); time.sleep(1)'; {CHECK}"
    );
    let (fx, _, source, publication) = source(&command).await;
    std::fs::remove_dir_all(source).unwrap();
    schedule(&fx).await;
    let op = verification(&fx, &publication).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let original = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let raw: Option<String> = sqlx::query_scalar(
                "SELECT tx_output_json FROM operations WHERE id=?1 AND phase='parked'",
            )
            .bind(&op.id)
            .fetch_optional(&pool)
            .await
            .unwrap()
            .flatten();
            let Some(raw) = raw else {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            };
            let value: Value = serde_json::from_str(&raw).unwrap();
            if let Some(path) = value["data"]["workspace"].as_str()
                && PathBuf::from(path).join("input/source/started").exists()
            {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Model the journal window after recorded release but before park commit.
    sqlx::query("UPDATE operations SET phase='spawn_started',lease_owner=NULL,lease_until_ms=NULL WHERE id=?1").bind(&op.id).execute(&pool).await.unwrap();
    fx.state
        .operation_runtime
        .apply_recovery(fx.state.operation_runtime.recover_on_boot().await.unwrap())
        .await
        .unwrap();
    let evidence = verified(&fx, &publication).await;
    assert_eq!(evidence["candidate"], original["data"]["candidate"]);
    assert_eq!(evidence["policy"], original["data"]["policy"]);
    assert_eq!(evidence["verdict"]["passed"], true, "{evidence}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_file_candidates WHERE operation_id=?1")
            .bind(&publication)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_authority_is_frozen_declaration_and_user_release_not_worker_success()
 {
    let (fx, task, _, publication) = source(CHECK).await;
    // The producer ran under auto-declare. Changing release policy after its report
    // must not authorize host checks merely because isolated Worker execution passed.
    sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
        .bind(&task.track_id)
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    schedule(&fx).await;
    assert!(
        fx.state
            .operation_runtime
            .find_by_kind_and_idempotency("candidate-verify", &format!("candidate:{publication}"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_settlement_replays_exact_identity_once() {
    let (fx, task, _, publication) = source("false").await;
    let planner = crate::isolated_codex_retry::recovery_wake::planner(&fx).await;
    schedule(&fx).await;
    assert_eq!(
        verified(&fx, &publication).await["verdict"]["passed"],
        false
    );
    let event = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            schedule(&fx).await;
            let events = fx
                .boot
                .repo
                .events_for_track(
                    fx.boot.track_id.as_str(),
                    &["task.candidate_verification_settled"],
                    None,
                )
                .await
                .unwrap();
            if let Some(event) = events.into_iter().next() {
                break event;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..2 {
        fx.state
            .dispatcher
            .catch_up_push(fx.boot.track_id.clone(), event.event.clone(), event.id)
            .await;
    }
    tokio::time::timeout(Duration::from_secs(10),async { loop {
        if planner.snapshot().await.pending_observations().iter().any(|o| matches!(o,calm_server::harness::Observation::SystemContext { text } if text.contains("Candidate verification") && text.contains(&task.key))) { break; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }}).await.unwrap();
    let snapshot = planner.snapshot().await;
    assert_eq!(snapshot.pending_observations().iter().filter(|o| matches!(o,calm_server::harness::Observation::SystemContext { text } if text.contains("Candidate verification") && text.contains(&task.key))).count(),1);
    let calm_server::event::Event::TaskCandidateVerificationSettled { operation_id, .. } =
        event.event
    else {
        panic!("wrong event");
    };
    assert_ne!(operation_id, publication);
    assert_eq!(operation_id, verification(&fx, &publication).await.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_active_reservation_blocks_independent_worker_until_completion() {
    use std::sync::Arc;
    let (fx, _, _, publication) = source(CHECK).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let wait = resume.clone();
    let _hook = calm_server::file_delivery::install_candidate_release_hook(
        &publication,
        Arc::new(move |_| {
            let signal = signal.clone();
            let wait = wait.clone();
            Box::pin(async move {
                signal.notify_one();
                wait.notified().await;
            })
        }),
    );
    schedule(&fx).await;
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    declare(&fx.boot,json!({"key":"independent","kind":"codex","goal":"Independent bounded task","declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,"no_gate_reason":"No file delivery","context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}})).await;
    schedule(&fx).await;
    assert_eq!(
        current(&fx.boot, "independent").await.status,
        TaskStatus::Pending
    );
    resume.notify_one();
    assert_eq!(verified(&fx, &publication).await["verdict"]["passed"], true);
    schedule(&fx).await;
    let worker = current(&fx.boot, "independent").await;
    assert_eq!(worker.status, TaskStatus::Running);
    settle(&fx, &worker, true).await;
}

#[path = "candidate_verification_review.rs"]
mod review;

#[path = "candidate_authoring.rs"]
mod authoring;

#[path = "candidate_review_qualification.rs"]
mod candidate_review_qualification;
