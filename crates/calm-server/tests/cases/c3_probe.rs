use super::c3_execution::*;
use super::*;

#[tokio::test]
async fn c3_credentials_never_enter_intent() {
    let _lock = FORGE_ENV_TEST_LOCK.lock().await;
    let _auth = EnvVarGuard::set("GH_TOKEN", "c3-private-credential");
    let boot = TestBoot::new().await;
    let completion = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "forge-before-completion",
        "credential-intents",
        completion.clone(),
    );
    let mut input = c3_merge_input(&boot, "credential-intents", "exit 7");
    input["probe"] = json!({"probe_argv":["/bin/sh","-c","test -n \"$GH_TOKEN\""],"output_probe_argv":["/bin/sh","-c","test -n \"$GH_TOKEN\" && echo '{\"oid\":\"abc\",\"headRefOid\":\"def\"}'"]});
    let id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key("credential-intents"), input)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), completion.entered.notified())
        .await
        .unwrap();
    // Sample all PrepareExecution requests before completion can retire their journals.
    let mut requests = 0;
    for file in walk_journal(boot._execution.key.parent().unwrap().join("journal")) {
        let raw = fs::read(&file).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("c3-private-credential"));
        // Match Store::load: atomic writes can leave a staging copy beside the
        // canonical journal. Scan its bytes above, but do not count it twice.
        if file
            .extension()
            .is_some_and(|extension| extension == "execution")
            && let Ok(journal) = serde_json::from_slice::<Value>(&raw)
            && journal.get("request").is_some()
        {
            requests += 1;
        }
    }
    assert_eq!(
        requests, 3,
        "action and both probe PrepareExecution journals"
    );
    completion.release.notify_one();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    assert_eq!(c3_count(&boot, &id).await, 3);
    let rows: Vec<String> = sqlx::query_scalar("SELECT intent_json FROM workspace_executions")
        .fetch_all(boot.repo.pool())
        .await
        .unwrap();
    for raw in rows {
        assert!(!raw.contains("c3-private-credential"));
        let intent: Value = serde_json::from_str(&raw).unwrap();
        for pair in intent["program"]["environment"].as_array().unwrap() {
            assert!(
                !calm_server::operation::forge_action_adapter::FORGE_CREDENTIAL_ENV_KEYS
                    .contains(&pair[0].as_str().unwrap())
            );
        }
    }
    // Read the executor's own durable journal, including retired history.
    for file in walk_journal(boot._execution.key.parent().unwrap().join("journal")) {
        let raw = fs::read(file).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("c3-private-credential"));
    }
}
fn walk_journal(root: PathBuf) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = fs::read_dir(root).expect("read execution journal directory");
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk_journal(path));
        } else {
            files.push(path);
        }
    }
    files
}
mod descendants {
    use super::*;
    async fn descendant_drain(probe: bool) {
        let boot = TestBoot::new().await;
        let descendant = boot.temp_path("descendant");
        let script = format!(
            "sh -c 'trap \"\" TERM; echo $$ > \"$1\"; sleep 30' sh '{}' >/dev/null & while [ ! -f '{}' ]; do sleep .01; done; echo '{{\"oid\":\"abc\",\"headRefOid\":\"def\"}}'",
            descendant.display(),
            descendant.display()
        );
        let mut input = c3_input(
            &boot,
            "descendant-drain",
            if probe { "exit 7" } else { &script },
        );
        if probe {
            input["probe"] = json!({"probe_argv":["/bin/sh","-c",script],"output_probe_argv":null});
        }
        let id = boot
            .runtime
            .submit(FORGE_ACTION_KIND, op_key("descendant-drain"), input)
            .await
            .unwrap();
        wait_for_file(&descendant).await;
        let record = c3_record(&c3_op(&boot, &id).await);
        let execution = if probe {
            record["probes"]["verdict"]["execution_id"]
                .as_str()
                .unwrap()
        } else {
            record["execution_id"].as_str().unwrap()
        };
        let base = boot.temp_path("descendant-drain");
        let code = if probe {
            PathBuf::from(format!("{}.probe.{}.code", base.display(), execution))
        } else {
            PathBuf::from(format!("{}.code", base.display()))
        };
        wait_for_file(&code).await;
        let settlement: Option<String> = sqlx::query_scalar(
            "SELECT settlement_json FROM workspace_executions WHERE execution_id=?",
        )
        .bind(execution)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
        assert!(
            settlement.is_none(),
            "leader evidence must precede positive descendant drain"
        );
        assert!(
            boot.operation_repo
                .operation_result(&id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(phase(&boot.repo, &id).await, "parked");
        assert!(matches!(
            c3_result(&boot, &id).await.outcome,
            OperationOutcome::Succeeded { .. }
        ));
        let pid = fs::read_to_string(descendant).unwrap();
        assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
        c3_all_retired(&boot, &id).await;
    }
    #[tokio::test]
    async fn c3_leader_exit_drains_descendants_before_verdict() {
        descendant_drain(false).await;
    }
    #[tokio::test]
    async fn c3_probe_settlement_precedes_verdict() {
        descendant_drain(true).await;
    }
}

#[tokio::test]
async fn c3_landed_verdict_persists_before_output_probe() {
    let boot = TestBoot::new().await;
    let mut input = c3_merge_input(&boot, "landed-output", "exit 7");
    input["probe"] = json!({"probe_argv":["/bin/sh","-c",format!("echo verdict >> '{}'",boot.temp_path("verdict-count").display())],"output_probe_argv":["/bin/sh","-c",format!("touch '{}'; i=0; while [ ! -f '{}' ] && [ \"$i\" -lt 1500 ]; do i=$((i+1)); sleep .02; done; echo '{{\"oid\":\"abc\",\"headRefOid\":\"def\"}}'",boot.temp_path("output-started").display(),boot.temp_path("output-finish").display())]});
    let id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key("landed-output"), input)
        .await
        .unwrap();
    wait_for_file(&boot.temp_path("output-started")).await;
    c3_wait(async || {
        c3_record(&c3_op(&boot, &id).await)["probes"]["output"]["release"]
            .get("Released")
            .is_some()
    })
    .await;
    let before = c3_record(&c3_op(&boot, &id).await);
    assert_eq!(before["verdict"], "Landed");
    assert!(!before["probes"]["output"].is_null());
    // Durable Landed makes the old verdict file irrelevant to all future claimants.
    let verdict = before["probes"]["verdict"]["execution_id"]
        .as_str()
        .unwrap();
    fs::write(
        boot.temp_path(&format!("landed-output.probe.{verdict}.code")),
        "1",
    )
    .unwrap();
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    fs::write(boot.temp_path("output-finish"), "").unwrap();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Succeeded { .. }
    ));
    assert_eq!(
        fs::read_to_string(boot.temp_path("verdict-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(c3_count(&boot, &id).await, 3);
    c3_all_retired(&boot, &id).await;
}
#[tokio::test]
async fn c3_park_write_failure_retains_owned_execution() {
    let boot = TestBoot::new().await;
    sqlx::query("CREATE TRIGGER fail_forge_park BEFORE UPDATE OF phase ON operations WHEN NEW.phase='parked' \
        BEGIN SELECT RAISE(ABORT,'injected park failure'); END").execute(boot.repo.pool()).await.unwrap();
    let input = c3_input(&boot, "park-failure", "touch ran");
    let _ = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key("park-failure"), input)
        .await;
    let op = boot
        .runtime
        .find_by_kind_and_idempotency(FORGE_ACTION_KIND, "park-failure")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.phase, Phase::SpawnStarted);
    assert_eq!(c3_count(&boot, &op.id).await, 1);
    assert_eq!(c3_record(&op)["release"], "Held");
    assert!(!boot.temp_path("ran").exists());
    sqlx::query("DROP TRIGGER fail_forge_park")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    assert!(matches!(
        c3_result(&boot, &op.id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    c3_all_retired(&boot, &op.id).await;
}

pub(super) async fn output_admission_ready(boot: &TestBoot) -> String {
    use workspace_runtime::execution::*;
    let mut input = c3_merge_input(boot, "exclusive-output", "exit 7");
    input["probe"] = json!({"probe_argv":["/bin/true"],"output_probe_argv":["/bin/true"]});
    let (id, op, output) = claimed_spawn_started_forge_op(boot, "exclusive-output", input)
        .await
        .unwrap();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = ForgeActionAdapter::new()
        .spawn_side_effect(&output, &op, &boot.spawn_ctx)
        .await
        .unwrap()
    else {
        panic!()
    };
    drop(observer);
    boot.operation_repo
        .set_parked(&op, deadline_ms, Some("forge_execution"))
        .await
        .unwrap()
        .unwrap();
    c3_settle_held(boot, &id).await;
    let claimed = boot
        .operation_repo
        .claim_parked(&id)
        .await
        .unwrap()
        .unwrap();
    ForgeActionAdapter::new()
        .recover_owned_parked(&claimed, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
        .await
        .unwrap();
    let mut record = c3_record(&c3_op(boot, &id).await);
    let execution: ExecutionId =
        serde_json::from_value(record["probes"]["verdict"]["execution_id"].clone()).unwrap();
    let engine = boot.spawn_ctx.execution.engine().unwrap();
    c3_wait(async || {
        matches!(
            engine.output(&execution, None).await,
            Ok(ExecutionReply::Observation {
                state: ExecutionState::Spawned,
                ..
            })
        )
    })
    .await;
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let ticket = engine
        .request(&mut tx, &execution, Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
    tokio::time::timeout(Duration::from_secs(10), engine.wait_settled(&execution))
        .await
        .unwrap()
        .unwrap();
    record["probes"]["verdict"]["release"] = json!({"Released":{"authorized_at_ms":now_ms()}});
    stage_result_files(
        &boot.temp_path(&format!("exclusive-output.probe.{}", execution.as_str())),
        "0",
        "",
    );
    c3_rewrite(boot, &id, &record).await;
    id
}

#[tokio::test]
async fn c3_pipe_window_does_not_truncate_application_evidence() {
    for probe in [false, true] {
        let boot = TestBoot::new().await;
        let script = r#"printf '{"oid":"abc","headRefOid":"def","padding":"'; head -c 32768 /dev/zero | tr '\000' x; printf '"}'"#;
        let mut input = c3_merge_input(
            &boot,
            "window-evidence",
            if probe { "exit 7" } else { script },
        );
        if probe {
            input["probe"] =
                json!({"probe_argv":["/bin/true"],"output_probe_argv":["/bin/sh","-c",script]});
        }
        let id = boot
            .runtime
            .submit(FORGE_ACTION_KIND, op_key("window-evidence"), input)
            .await
            .unwrap();
        assert!(matches!(
            c3_result(&boot, &id).await.outcome,
            OperationOutcome::Succeeded { .. }
        ));
        let record = c3_record(&c3_op(&boot, &id).await);
        let path = if probe {
            boot.temp_path(&format!(
                "window-evidence.probe.{}.stdout",
                record["probes"]["output"]["execution_id"].as_str().unwrap()
            ))
        } else {
            boot.temp_path("window-evidence.stdout")
        };
        let json: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(json["padding"].as_str().unwrap().len(), 32768);
    }
}

#[tokio::test]
async fn c3_releasing_deadline_refuses_without_resend() {
    use workspace_runtime::execution::*;
    for probe in [false, true] {
        let boot = TestBoot::new().await;
        let name = if probe {
            "release-expired-probe"
        } else {
            "release-expired-action"
        };
        let pause = calm_server::test_seams::PausePoint::default();
        let id = if probe {
            let id = c3_probe_ready(&boot, name, "touch ran").await;
            // Keep the admission observer from releasing before the synthetic deadline.
            calm_server::test_seams::install_pause_for_test(
                "forge-observer-before-read",
                &id,
                pause.clone(),
            );
            let op = boot
                .operation_repo
                .claim_parked(&id)
                .await
                .unwrap()
                .unwrap();
            ForgeActionAdapter::new()
                .recover_owned_parked(&op, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
                .await
                .unwrap();
            id
        } else {
            let (id, _, observer) = c3_held(&boot, name).await;
            drop(observer);
            id
        };
        let mut record = c3_record(&c3_op(&boot, &id).await);
        let state = if probe {
            &mut record["probes"]["verdict"]
        } else {
            &mut record
        };
        let execution: ExecutionId = serde_json::from_value(state["execution_id"].clone()).unwrap();
        let engine = boot.spawn_ctx.execution.engine().unwrap();
        c3_wait(async || {
            matches!(
                engine.output(&execution, None).await,
                Ok(ExecutionReply::Observation {
                    state: ExecutionState::Spawned,
                    ..
                })
            )
        })
        .await;
        state["release"] = json!({"Releasing":{"authorized_at_ms":now_ms()-61_000}});
        let path = if probe {
            boot.temp_path(&format!("{name}.probe.{}", execution.as_str()))
        } else {
            boot.temp_path(name)
        };
        stage_result_files(&path, "0", "");
        c3_rewrite(&boot, &id, &record).await;
        boot.runtime.sweep_parked().await.unwrap();
        let latest = c3_record(&c3_op(&boot, &id).await);
        let state = if probe {
            &latest["probes"]["verdict"]
        } else {
            &latest
        };
        assert_eq!(
            state["decision"],
            json!({"Stopping":{"reason":"ReleaseRefused"}})
        );
        let plan = boot.runtime.recover_on_boot().await.unwrap();
        boot.runtime.apply_recovery(plan).await.unwrap();
        pause.release.notify_one();
        assert!(
            matches!(c3_result(&boot,&id).await.outcome,OperationOutcome::Failed {last_error_class:Some(ref class),..} if class=="gate-infra")
        );
        assert!(!boot.temp_path("ran").exists());
        assert_eq!(c3_count(&boot, &id).await, if probe { 2 } else { 1 });
    }
}

#[tokio::test]
async fn c3_held_action_past_deadline_never_releases() {
    let boot = TestBoot::new().await;
    let (id, _, observer) = c3_held(&boot, "expired-held").await;
    sqlx::query("UPDATE operations SET parked_deadline_ms=0 WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    boot.runtime.sweep_parked().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), observer)
        .await
        .unwrap();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
    let record = c3_record(&c3_op(&boot, &id).await);
    assert_eq!(record["release"], "Held");
    assert_eq!(record["decision"], json!({"Stopping":{"reason":"Timeout"}}));
    assert!(!boot.temp_path("ran").exists());
}
#[tokio::test]
async fn c3_settled_held_probe_never_waits_for_release() {
    use workspace_runtime::execution::*;
    let boot = TestBoot::new().await;
    let (id, observer) = c3_probe_ready_with_observer(&boot, "settled-held", "touch ran").await;
    let op = boot
        .operation_repo
        .claim_parked(&id)
        .await
        .unwrap()
        .unwrap();
    ForgeActionAdapter::new()
        .recover_owned_parked(&op, RecoveryMode::PreDeadlineProbe, &boot.spawn_ctx)
        .await
        .unwrap();
    let record = c3_record(&c3_op(&boot, &id).await);
    let execution: ExecutionId =
        serde_json::from_value(record["probes"]["verdict"]["execution_id"].clone()).unwrap();
    let engine = boot.spawn_ctx.execution.engine().unwrap();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    let ticket = engine
        .request(&mut tx, &execution, Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
    tokio::time::timeout(Duration::from_secs(10), engine.wait_settled(&execution))
        .await
        .unwrap()
        .unwrap();
    // Resume the original observer directly: a boot sweep could finish this settled
    // probe first and bypass the release guard this regression must exercise.
    c3_rewrite(&boot, &id, &c3_record(&c3_op(&boot, &id).await)).await;
    tokio::time::timeout(Duration::from_secs(10), observer)
        .await
        .expect("a settled Held probe must not wait for release");
    assert!(
        matches!(c3_result(&boot,&id).await.outcome,OperationOutcome::Failed {last_error_class:Some(ref class),..} if class=="gate-infra")
    );
    assert_eq!(
        c3_record(&c3_op(&boot, &id).await)["probes"]["verdict"]["release"],
        "Held"
    );
    assert!(!boot.temp_path("ran").exists());
}

#[tokio::test]
async fn c3_observer_alone_never_times_out_expired_action() {
    let boot = TestBoot::new().await;
    let (id, _, observer) = c3_held(&boot, "observer-expired").await;
    sqlx::query("UPDATE operations SET parked_deadline_ms=0 WHERE id=?")
        .bind(&id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), observer)
        .await
        .unwrap();
    assert_eq!(c3_op(&boot, &id).await.phase, Phase::Parked);
    let record = c3_record(&c3_op(&boot, &id).await);
    assert_eq!(record["decision"], "Observing");
    assert!(!boot.temp_path("ran").exists());
    boot.runtime.sweep_parked().await.unwrap();
    assert!(matches!(
        c3_result(&boot, &id).await.outcome,
        OperationOutcome::Failed { .. }
    ));
}
