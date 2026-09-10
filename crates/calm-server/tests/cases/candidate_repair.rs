//! One linked repair through production MCP, report writer, scheduler and retained files.
use super::*;

const FINDINGS: [&str; 2] = [
    "Reject nonnumeric input explicitly",
    "Document supported numeric input",
];
const REPAIRED: &str = "def double(x):\n    if not isinstance(x, (int, float)) or isinstance(x, bool):\n        raise TypeError('numeric input required')\n    return x * 2\n";
async fn request(
    fx: &Fixture,
    reason: &str,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(
        &fx.boot,
        "calm.task.repair",
        planner_identity(&fx.boot),
        json!({"producer":"produce","reason":reason}),
    )
    .await
}
async fn rejected() -> (Fixture, Task, Task, String, Value) {
    rejected_scenario("controlled").await
}
async fn rejected_scenario(scenario: &str) -> (Fixture, Task, Task, String, Value) {
    let fx = fixture(scenario).await;
    fx.state.dispatcher.abort_event_listener_for_test();
    let mut p = producer(CHECK);
    p["goal"] = json!("Implement the numeric double API");
    p["acceptance"] = json!(
        "Double numeric input; explicitly reject unsupported input and document the contract"
    );
    p["context"]["neige_execution"]["file_delivery"]["policy"]["scope"] = json!("review-required");
    p["context"]["neige_execution"]["file_delivery"]["policy"]["reviewer"] = json!("review");
    declare(&fx.boot, p).await;
    schedule(&fx).await;
    let producer = current(&fx.boot, "produce").await;
    let path = workspace(&fx, &producer).await;
    for (name, bytes) in FILES {
        std::fs::write(path.join(name), bytes).unwrap();
    }
    settle(&fx, &producer, true).await;
    let publication = publish(&fx, &producer).await;
    std::fs::remove_dir_all(path).unwrap();
    let mut r = reviewer();
    r["goal"] = json!("Independently audit numeric API behavior and documentation");
    r["acceptance"] =
        json!("Check edge cases including strings and bool, and require clear documentation");
    declare(&fx.boot, r).await;
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    let machine = verified(&fx, &publication).await;
    schedule(&fx).await;
    crate::mcp_task_dispatch::bind_planner(&fx.boot, &planner_identity(&fx.boot).session_id, false)
        .await;
    let review = current(&fx.boot, "review").await;
    std::fs::write(
        workspace(&fx, &review).await.join("report-result.json"),
        json!({"passed":false,"blocking_findings":FINDINGS}).to_string(),
    )
    .unwrap();
    settle(&fx, &review, true).await;
    (fx, producer, review, publication, machine)
}
async fn count(fx: &Fixture) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM task_candidate_repairs), (SELECT count(*) FROM events), (SELECT count(*) FROM task_attempt_allocations)")
        .fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap()
}
async fn receipt(fx: &Fixture) -> Value {
    let raw: String = sqlx::query_scalar("SELECT receipt_json FROM task_candidate_repairs")
        .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}
fn passed() -> Value {
    json!({"passed":true,"blocking_findings":[],"finding_responses":[
        {"finding_index":0,"status":"resolved","evidence":"project.py rejects strings and bool with TypeError"},
        {"finding_index":1,"status":"resolved","evidence":"README describes numeric-only API"}]})
}
async fn produce_c2(fx: &Fixture, pair: &Value) -> (Task, String, Value, Task) {
    eprintln!("repair stage: launching C2 producer");
    schedule(fx).await;
    let task = current(&fx.boot, pair["receipt"]["repair_key"].as_str().unwrap()).await;
    assert_eq!(task.status, TaskStatus::Running, "{}", listed(fx).await);
    let path = workspace(fx, &task).await;
    for (name, bytes) in FILES {
        assert_eq!(
            std::fs::read(path.join("inputs/source").join(name)).unwrap(),
            bytes.as_bytes()
        );
        let output = match name {
            "project.py" => REPAIRED,
            "README.md" => "Numeric input only. bool and strings raise TypeError.\n",
            _ => bytes,
        };
        std::fs::write(path.join(name), output).unwrap();
    }
    settle(fx, &task, true).await;
    eprintln!("repair stage: publishing C2");
    let publication = publish(fx, &task).await;
    schedule(fx).await;
    let machine = verified(fx, &publication).await;
    schedule(fx).await;
    eprintln!("repair stage: C2 checked, R2 launched");
    let review = current(&fx.boot, pair["receipt"]["review_key"].as_str().unwrap()).await;
    assert_eq!(review.status, TaskStatus::Running, "{}", listed(fx).await);
    assert_eq!(
        std::fs::read(
            workspace(fx, &review)
                .await
                .join("inputs/source/project.py")
        )
        .unwrap(),
        REPAIRED.as_bytes()
    );
    (task, publication, machine, review)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_exact_c1_to_c2_delivery_with_fresh_review_and_stable_pair() {
    let (fx, producer, r1, publication, machine) = rejected().await;
    assert!(verdict(&fx, &producer, "accepted").await.is_err());
    let before = crate::mcp_task_dispatch::payload(&fx.boot).await;
    let pair = request(&fx, "Resolve both original findings")
        .await
        .unwrap();
    let committed = count(&fx).await;
    assert_eq!(committed.0, 1);
    let replay = request(&fx, "Resolve both original findings")
        .await
        .unwrap();
    assert_eq!(pair["receipt"], replay["receipt"]);
    assert_eq!(count(&fx).await, committed);
    assert!(request(&fx, "Different reason").await.is_err());
    let saved = receipt(&fx).await;
    assert_eq!(saved["input"]["candidate"], machine["candidate"]);
    assert_eq!(
        saved["review"]["report"]["blocking_findings"],
        json!(FINDINGS)
    );
    let after = crate::mcp_task_dispatch::payload(&fx.boot).await;
    for original in before.blocks.as_ref().unwrap() {
        assert_eq!(
            after
                .blocks
                .as_ref()
                .unwrap()
                .iter()
                .find(|b| b.id == original.id)
                .unwrap(),
            original
        );
    }
    assert_eq!(
        after.blocks.as_ref().unwrap().len(),
        before.blocks.as_ref().unwrap().len() + 2
    );
    for (original, derived) in [
        ("source_payload", "repair"),
        ("reviewer_payload", "reviewer"),
    ] {
        for field in ["goal", "acceptance"] {
            assert_eq!(saved[original][field], saved[derived]["payload"][field]);
        }
    }
    let (c2, c2pub, c2machine, r2) = produce_c2(&fx, &pair).await;
    assert_ne!(c2pub, publication);
    assert_ne!(
        c2machine["verification_operation_id"],
        machine["verification_operation_id"]
    );
    assert_eq!(c2machine["verdict"]["passed"], true);
    assert!(verdict(&fx, &c2, "accepted").await.is_err());
    std::fs::write(
        workspace(&fx, &r2).await.join("report-result.json"),
        passed().to_string(),
    )
    .unwrap();
    settle(&fx, &r2, true).await;
    let view = listed(&fx).await;
    let c2view = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == c2.key)
        .unwrap();
    assert_eq!(
        c2view["file_delivery"]["candidate"]["publication_operation_id"],
        c2pub
    );
    assert_eq!(
        c2view["file_delivery"]["input"]["publication_operation_id"],
        publication
    );
    assert_eq!(c2view["file_delivery"]["qualified"], false);
    let compact = call_tool(
        &fx.boot,
        "calm.plan.list",
        planner_identity(&fx.boot),
        json!({"detail":"summary","key":c2.key}),
    )
    .await
    .unwrap();
    assert_eq!(compact["tasks"].as_array().unwrap().len(), 1);
    let delivery = &compact["tasks"][0]["file_delivery"];
    for field in ["candidate", "publication", "input", "qualification"] {
        assert_eq!(delivery[field], c2view["file_delivery"][field]);
    }
    assert_eq!(
        delivery["repair"]["snapshot"],
        c2view["file_delivery"]["repair"]["snapshot"]
    );
    assert_eq!(
        delivery["review"]["operation"],
        c2view["file_delivery"]["review"]["operation"]
    );
    assert!(delivery["review"].get("finding_responses").is_none());
    assert!(delivery["repair"].get("blocking_findings").is_none());
    assert!(compact.to_string().len() <= 12 * 1024);

    verdict(&fx, &r2, "accepted").await.unwrap();
    let mut consume = consumer();
    consume["key"] = json!("consume-c2");
    consume["context"]["neige_execution"]["file_delivery"]["producer"] = json!(c2.key);
    declare(&fx.boot, consume).await;
    schedule(&fx).await;
    assert_eq!(
        current(&fx.boot, "consume-c2").await.status,
        TaskStatus::Pending
    );
    eprintln!("repair stage: accepting C2 then launching consumer");
    verdict(&fx, &c2, "accepted").await.unwrap();
    schedule(&fx).await;
    let consumer = current(&fx.boot, "consume-c2").await;
    assert_eq!(
        consumer.status,
        TaskStatus::Running,
        "{}",
        listed(&fx).await
    );
    assert_eq!(
        binding(&fx, &consumer).await["candidate"],
        c2machine["candidate"]
    );
    let path = workspace(&fx, &consumer).await;
    assert_eq!(
        std::fs::read(path.join("inputs/source/project.py")).unwrap(),
        REPAIRED.as_bytes()
    );
    eprintln!(
        "repair stage: C2 delivered; settling {} ({}) before read audits",
        consumer.key, consumer.id
    );
    settle(&fx, &consumer, true).await;
    // C1 input remains the original exact bytes after repair and consumer delivery.
    for (name, bytes) in FILES {
        assert_eq!(
            std::fs::read(workspace(&fx, &c2).await.join("inputs/source").join(name)).unwrap(),
            bytes.as_bytes()
        );
    }
    assert_eq!(current(&fx.boot, "produce").await.id, producer.id);
    assert_eq!(current(&fx.boot, "review").await.id, r1.id);
    assert_eq!(current(&fx.boot, "produce").await.status, TaskStatus::Done);
    assert_eq!(current(&fx.boot, "review").await.status, TaskStatus::Done);
    assert_eq!(
        current(&fx.boot, "consume").await.status,
        TaskStatus::Pending
    );
    let view = listed(&fx).await.to_string();
    assert!(
        view.contains("finding_responses") && view.contains("repair_key"),
        "{view}"
    );
    assert!(
        !view.contains(fx.root.path().to_str().unwrap()),
        "private path leaked: {view}"
    );
    assert!(
        call_tool(
            &fx.boot,
            "calm.task.repair",
            planner_identity(&fx.boot),
            json!({"producer":c2.key,"reason":"recursive"})
        )
        .await
        .is_err()
    );
    // Already-saved consumer acceptance must still recheck original C1/R1 authority.
    for withdrawn in [&producer.id, &r1.id] {
        sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
            .bind(withdrawn)
            .execute(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
        let view = listed(&fx).await;
        let bound = view["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["key"] == "consume-c2")
            .unwrap();
        assert_eq!(
            bound["file_delivery"]["qualified"], false,
            "original authority withdrawn: {withdrawn}"
        );
        assert!(
            bound["file_delivery"]["input"]["decision_event_id"].is_i64(),
            "saved acceptance must remain visible"
        );
        sqlx::query("UPDATE tasks SET context_stale_at_ms=NULL WHERE id=?1")
            .bind(withdrawn)
            .execute(&fx.boot.repo.sqlite_pool().unwrap())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_concurrent_replay_atomic_rollback_and_current_auth() {
    let (fx, _, _, _, _) = rejected().await;
    let before = count(&fx).await;
    let doc = crate::mcp_task_dispatch::payload(&fx.boot).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    sqlx::query("CREATE TRIGGER refuse_repair BEFORE INSERT ON task_candidate_repairs BEGIN SELECT RAISE(ABORT,'injected repair receipt failure'); END").execute(&pool).await.unwrap();
    assert!(
        request(&fx, "Fix findings")
            .await
            .unwrap_err()
            .message
            .contains("injected repair receipt failure")
    );
    assert_eq!(count(&fx).await, before);
    assert_eq!(crate::mcp_task_dispatch::payload(&fx.boot).await, doc);
    sqlx::query("DROP TRIGGER refuse_repair")
        .execute(&pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(request(&fx, "Fix findings"), request(&fx, "Fix findings"));
    assert_eq!(a.unwrap()["receipt"], b.unwrap()["receipt"]);
    let committed = count(&fx).await;
    assert_eq!(committed.0, 1);
    assert!(
        sqlx::query("UPDATE task_candidate_repairs SET receipt_json='{}'")
            .execute(&pool)
            .await
            .is_err()
    );
    for role in [
        calm_server::model::CardRole::Worker,
        calm_server::model::CardRole::Assistant,
    ] {
        let mut identity = planner_identity(&fx.boot);
        identity.role = role;
        assert!(
            call_tool(
                &fx.boot,
                "calm.task.repair",
                identity,
                json!({"producer":"produce","reason":"Fix findings"})
            )
            .await
            .is_err()
        );
    }
    let mut identity = planner_identity(&fx.boot);
    identity.area_id = "foreign-area".into();
    assert!(
        call_tool(
            &fx.boot,
            "calm.task.repair",
            identity,
            json!({"producer":"produce","reason":"Fix findings"})
        )
        .await
        .is_err()
    );
    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?1")
        .bind(&planner_identity(&fx.boot).session_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(request(&fx, "Fix findings").await.unwrap_err().code, -32403);
    assert_eq!(count(&fx).await, committed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_r2_report_requires_exact_complete_responses_and_fresh_event() {
    let (fx, _, r1, _, _) = rejected().await;
    let pair = request(&fx, "Fix findings").await.unwrap();
    let (c2, _, _, r2) = produce_c2(&fx, &pair).await;
    let identity = review_identity(&fx, &r2).await;
    let mut bad = Vec::new();
    let mut value = passed();
    value.as_object_mut().unwrap().remove("finding_responses");
    bad.push(value);
    for responses in [
        json!([]),
        json!([passed()["finding_responses"][0]]),
        json!([
            passed()["finding_responses"][0],
            passed()["finding_responses"][0]
        ]),
    ] {
        let mut value = passed();
        value["finding_responses"] = responses;
        bad.push(value);
    }
    for (pointer, value) in [
        ("/finding_responses/1/finding_index", json!(2)),
        ("/finding_responses/1/status", json!("unresolved")),
        ("/finding_responses/1/evidence", json!(" ")),
        ("/blocking_findings", json!(["new blocker"])),
    ] {
        let mut report = passed();
        *report.pointer_mut(pointer).unwrap() = value;
        bad.push(report);
    }
    for result in bad {
        let response = call_tool(
            &fx.boot,
            "calm.task.complete",
            identity.clone(),
            json!({"idempotency_key":r2.id,"result":result,"artifacts":[]}),
        )
        .await;
        assert!(
            response.is_err(),
            "accepted invalid repair report: {response:?}"
        );
    }
    assert!(
        call_tool(
            &fx.boot,
            "calm.task.complete",
            review_identity(&fx, &r1).await,
            json!({"idempotency_key":r2.id,"result":passed(),"artifacts":[]})
        )
        .await
        .is_err()
    );
    // Fault injection at the persisted current task: deleting the optional reference
    // cannot turn this registered R2 into an ordinary two-field reviewer.
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET context_json=json_remove(context_json,'$.neige_execution.repair') WHERE id=?1")
        .bind(&r2.id).execute(&pool).await.unwrap();
    let downgrade = call_tool(&fx.boot,"calm.task.complete",identity.clone(),json!({"idempotency_key":r2.id,"result":{"passed":true,"blocking_findings":[]},"artifacts":[]})).await.unwrap_err();
    assert!(
        downgrade.message.contains("repair reference"),
        "{downgrade:?}"
    );
    assert_eq!(current(&fx.boot, &r2.key).await.status, TaskStatus::Running);
    sqlx::query("UPDATE tasks SET context_json=?1 WHERE id=?2")
        .bind(&r2.context_json)
        .bind(&r2.id)
        .execute(&pool)
        .await
        .unwrap();
    // An old R1 replay is still an R1 report and cannot qualify C2.
    call_tool(&fx.boot,"calm.task.complete",review_identity(&fx,&r1).await,json!({"idempotency_key":r1.id,"result":{"passed":false,"blocking_findings":FINDINGS},"artifacts":[]})).await.unwrap();
    assert!(verdict(&fx, &c2, "accepted").await.is_err());
    std::fs::write(
        workspace(&fx, &r2).await.join("report-result.json"),
        passed().to_string(),
    )
    .unwrap();
    settle(&fx, &r2, true).await;
    verdict(&fx, &c2, "accepted").await.unwrap();
    // Directly tamper the persisted event to exercise the independent evidence reader.
    let report_event:i64=sqlx::query_scalar("SELECT json_extract(event_json,'$.data.result.candidate_acceptance.review.report_event_id') FROM task_candidate_decisions ORDER BY event_id DESC LIMIT 1").fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
    sqlx::query(
        "UPDATE events SET payload=json_remove(payload,'$.result.finding_responses') WHERE id=?1",
    )
    .bind(report_event)
    .execute(&fx.boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
    assert!(verdict(&fx, &c2, "accepted").await.is_err());
    let view = listed(&fx).await;
    let task = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == c2.key)
        .unwrap();
    assert_eq!(task["file_delivery"]["qualified"], false, "{view}");
}

async fn edit_task(fx: &Fixture, key: &str, pointer: &str, replacement: Value) {
    let report = crate::mcp_task_dispatch::payload(&fx.boot).await;
    let block = report
        .blocks
        .unwrap()
        .into_iter()
        .find(|b| b.payload["key"] == key)
        .unwrap();
    let mut payload = block.payload;
    *payload.pointer_mut(pointer).unwrap() = replacement;
    call_tool(
        &fx.boot,
        "calm.report.blocks.upsert",
        planner_identity(&fx.boot),
        json!({"id":block.id,"kind":"task","payload":payload,"if_rev":block.rev}),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_original_contract_drift_cannot_become_new_contract() {
    for (key, pointer) in [
        ("produce", "/goal"),
        ("produce", "/acceptance"),
        ("review", "/goal"),
        ("review", "/acceptance"),
    ] {
        let (fx, _, _, _, _) = rejected().await;
        edit_task(&fx, key, pointer, json!("Changed after execution")).await;
        assert!(
            request(&fx, "Fix findings").await.is_err(),
            "{key} {pointer}"
        );
        assert_eq!(count(&fx).await.0, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_copied_reference_and_derived_contract_drift_fail_claim() {
    for defect in [
        "copy",
        "goal",
        "acceptance",
        "remove-reference",
        "checks",
        "reviewer-goal",
    ] {
        let (fx, _, _, _, _) = rejected().await;
        let pair = request(&fx, "Fix findings").await.unwrap();
        let saved = receipt(&fx).await;
        let key = pair["receipt"]["repair_key"].as_str().unwrap();
        let review_key = pair["receipt"]["review_key"].as_str().unwrap();
        match defect {
            "copy" => {
                let mut forged = saved["repair"]["payload"].clone();
                forged["key"] = json!("forged-repair");
                declare(&fx.boot, forged).await;
                // Keep the actual pair idle; the copied reference is the only attempted launch.
                edit_task(&fx, key, "/ready", json!(false)).await;
            }
            "goal" => edit_task(&fx, key, "/goal", json!("Different task")).await,
            "acceptance" => edit_task(&fx, key, "/acceptance", json!("Different acceptance")).await,
            "checks" => {
                edit_task(
                    &fx,
                    key,
                    "/context/neige_execution/file_delivery/policy/steps/0/cmd",
                    json!("true"),
                )
                .await
            }
            "remove-reference" => {
                // A valid normal empty CandidateProducer must still be fenced by registered identity.
                let report = crate::mcp_task_dispatch::payload(&fx.boot).await;
                let block = report
                    .blocks
                    .unwrap()
                    .into_iter()
                    .find(|b| b.payload["key"] == key)
                    .unwrap();
                let mut payload = block.payload;
                payload["context"]["neige_execution"]
                    .as_object_mut()
                    .unwrap()
                    .remove("repair");
                payload["context"]["neige_execution"]["workspace"] = json!("empty");
                call_tool(
                    &fx.boot,
                    "calm.report.blocks.upsert",
                    planner_identity(&fx.boot),
                    json!({"id":block.id,"kind":"task","payload":payload,"if_rev":block.rev}),
                )
                .await
                .unwrap();
            }
            "reviewer-goal" => {
                edit_task(&fx, review_key, "/goal", json!("Different review")).await;
                // Finish C2; R2's own receipt check must keep the edited reviewer pending.
                schedule(&fx).await;
                let task = current(&fx.boot, key).await;
                assert_eq!(task.status, TaskStatus::Running);
                for (name, bytes) in FILES {
                    std::fs::write(workspace(&fx, &task).await.join(name), bytes).unwrap();
                }
                settle(&fx, &task, true).await;
                let publication = publish(&fx, &task).await;
                schedule(&fx).await;
                verified(&fx, &publication).await;
            }
            _ => unreachable!(),
        }
        schedule(&fx).await;
        let refused = match defect {
            "copy" => "forged-repair",
            "reviewer-goal" => review_key,
            _ => key,
        };
        assert_eq!(
            current(&fx.boot, refused).await.status,
            TaskStatus::Pending,
            "{defect}"
        );
        let operations:i64=sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1").bind(current(&fx.boot,refused).await.id).fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
        assert_eq!(
            operations, 0,
            "{defect}: refused before allocation of a Worker Operation"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_technical_recovery_preserves_exact_c1_and_single_round() {
    let (fx, _, _, _, _) = rejected().await;
    let pair = request(&fx, "Fix findings").await.unwrap();
    schedule(&fx).await;
    let key = pair["receipt"]["repair_key"].as_str().unwrap();
    let first = current(&fx.boot, key).await;
    assert_eq!(first.status, TaskStatus::Running);
    let original = binding(&fx, &first).await;
    settle(&fx, &first, false).await;
    call_tool(&fx.boot,"calm.plan.recover",planner_identity(&fx.boot),json!({"key":key,"expected_attempt_id":first.id,"idempotency_key":"repair-technical-retry","reason":"Retry the same repair input"})).await.unwrap();
    schedule(&fx).await;
    let second = current(&fx.boot, key).await;
    assert_eq!(second.status, TaskStatus::Running, "{}", listed(&fx).await);
    assert_ne!(second.id, first.id);
    assert_eq!(binding(&fx, &second).await, original);
    assert_eq!(
        request(&fx, "Fix findings").await.unwrap()["receipt"],
        pair["receipt"]
    );
    assert_eq!(count(&fx).await.0, 1);
    for (name, bytes) in FILES {
        assert_eq!(
            std::fs::read(
                workspace(&fx, &second)
                    .await
                    .join("inputs/source")
                    .join(name)
            )
            .unwrap(),
            bytes.as_bytes()
        );
    }
    settle(&fx, &second, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_preturn_revalidates_receipt_authority_and_exact_input_bytes() {
    for defect in [
        "bytes",
        "source-withdrawal",
        "review-withdrawal",
        "derived-acceptance",
    ] {
        let (fx, producer, r1, _, _) = rejected_scenario("candidate-repair-preturn").await;
        let pair = request(&fx, "Fix findings").await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(20),
            fx.state
                .dispatcher
                .scheduler()
                .schedule_track(fx.boot.track_id.clone()),
        )
        .await
        .expect("preturn setup scheduling must be bounded");
        let task = current(&fx.boot, pair["receipt"]["repair_key"].as_str().unwrap()).await;
        let path = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let op = fx
                    .state
                    .operation_runtime
                    .find_by_kind_and_idempotency("codex-isolated-worker", &task.id)
                    .await
                    .unwrap();
                if let Some(output) = op.and_then(|o| o.tx_output)
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
        let original = binding(&fx, &task).await;
        match defect {
            "bytes" => {
                std::fs::write(path.join("inputs/source/project.py"), b"wrong C1 bytes").unwrap()
            }
            "source-withdrawal" | "review-withdrawal" => {
                sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
                    .bind(if defect == "source-withdrawal" {
                        &producer.id
                    } else {
                        &r1.id
                    })
                    .execute(&fx.boot.repo.sqlite_pool().unwrap())
                    .await
                    .unwrap();
            }
            "derived-acceptance" => {
                edit_task(
                    &fx,
                    &task.key,
                    "/acceptance",
                    json!("Altered after preparation"),
                )
                .await
            }
            _ => unreachable!(),
        }
        std::fs::write(path.join("resume-preturn"), b"").unwrap();
        schedule(&fx).await;
        assert_eq!(
            current(&fx.boot, &task.key).await.status,
            TaskStatus::Failed,
            "{defect}"
        );
        assert!(!path.join("result.txt").exists(), "{defect}: no first turn");
        assert_eq!(binding(&fx, &task).await, original);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_repair_done_report_without_successful_settlement_cannot_admit_or_qualify() {
    let (fx, producer, _, _) = review_source().await;
    crate::mcp_task_dispatch::bind_planner(&fx.boot, &planner_identity(&fx.boot).session_id, false)
        .await;
    let r1 = current(&fx.boot, "review").await;
    // An authenticated report is accepted while the controlled provider is still alive.
    call_tool(&fx.boot,"calm.task.complete",review_identity(&fx,&r1).await,json!({"idempotency_key":r1.id,"result":{"passed":false,"blocking_findings":FINDINGS},"artifacts":[]})).await.unwrap();
    assert_eq!(current(&fx.boot, "review").await.status, TaskStatus::Done);
    assert!(request(&fx, "Fix findings").await.is_err());
    std::fs::write(
        workspace(&fx, &r1).await.join("report-result.json"),
        json!({"passed":false,"blocking_findings":FINDINGS}).to_string(),
    )
    .unwrap();
    settle(&fx, &r1, true).await;
    let pair = request(&fx, "Fix findings").await.unwrap();
    let (c2, _, _, r2) = produce_c2(&fx, &pair).await;
    call_tool(
        &fx.boot,
        "calm.task.complete",
        review_identity(&fx, &r2).await,
        json!({"idempotency_key":r2.id,"result":passed(),"artifacts":[]}),
    )
    .await
    .unwrap();
    assert_eq!(current(&fx.boot, &r2.key).await.status, TaskStatus::Done);
    assert!(verdict(&fx, &c2, "accepted").await.is_err());
    assert!(verdict(&fx, &producer, "accepted").await.is_err());
    // Keep the full accepted report identical when the provider repeats it on stop.
    std::fs::write(
        workspace(&fx, &r2).await.join("report-result.json"),
        passed().to_string(),
    )
    .unwrap();
    settle(&fx, &r2, true).await;
    verdict(&fx, &c2, "accepted").await.unwrap();
}

#[path = "candidate_repair_review.rs"]
mod review_fixes;
