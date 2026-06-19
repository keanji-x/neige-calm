#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RouteRepo;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::error::{CalmError, Result as CalmResult};
use calm_server::event::{EventBus, FieldSource, ForgeEventSpec, ForgeMergeSubject};
use calm_server::model::{NewCove, NewWave, new_id, now_ms};
use calm_server::operation::ProviderAdapter;
use calm_server::operation::forge_action_adapter::{
    FORGE_ACTION_KIND, ForgeActionAdapter, ForgeActionPayload, ProbeSpec,
};
use calm_server::operation::{
    Operation, OperationCompletionBus, OperationKey, OperationOutcome, OperationRepo,
    OperationResult, OperationRuntime, ParkedRecovery, Phase, RecoveryItem, RecoveryMode,
    SpawnArtifacts, SpawnCtx, SpawnOutcome, SqlxOperationRepo, TxOutput,
};
use calm_server::proc_identity::{read_boot_id, read_proc_start_time};
use calm_server::routes::theme::RequestTheme;
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use serde_json::{Value, json};
use tempfile::TempDir;

struct TestBoot {
    _tmp: TempDir,
    repo: Arc<SqlxRepo>,
    operation_repo: Arc<SqlxOperationRepo>,
    runtime: OperationRuntime,
    spawn_ctx: SpawnCtx,
    wave_id: String,
}

impl TestBoot {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory repo"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "forge-action".into(),
                color: "#334455".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "forge-action".into(),
                sort: None,
                cwd: tmp.path().display().to_string(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");

        let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
        let events = EventBus::new();
        let completion = OperationCompletionBus::new();
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        let runtime = OperationRuntime::new_unchecked(
            operation_repo.clone(),
            vec![Arc::new(ForgeActionAdapter::new())],
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_repo,
                operation_repo.clone(),
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events.clone(),
                completion.clone(),
            ),
        );
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        let spawn_ctx = SpawnCtx::new(
            route_repo,
            operation_repo.clone(),
            Arc::new(DaemonClient::new_stub()),
            terminal_renderer,
            events,
            completion,
        );
        Self {
            _tmp: tmp,
            repo,
            operation_repo,
            runtime,
            spawn_ctx,
            wave_id: wave.id.to_string(),
        }
    }

    fn temp_path(&self, name: &str) -> PathBuf {
        self._tmp.path().join(name)
    }

    fn cwd_lease(&self) -> PathBuf {
        self._tmp.path().to_path_buf()
    }
}

fn event_spec() -> ForgeEventSpec {
    let mut fields = BTreeMap::new();
    fields.insert(
        "merge_sha".into(),
        FieldSource::JsonField {
            path: "/oid".into(),
        },
    );
    fields.insert(
        "head_sha".into(),
        FieldSource::JsonField {
            path: "/headRefOid".into(),
        },
    );
    ForgeEventSpec {
        event_kind: "forge.pr.merged".into(),
        fields,
    }
}

fn subject() -> ForgeMergeSubject {
    ForgeMergeSubject {
        phase: "impl".into(),
        slice_id: "slice-6".into(),
        pr_number: 760,
    }
}

fn payload(boot: &TestBoot, idem_key: &str, argv: Vec<String>, result_path: PathBuf) -> Value {
    payload_with_probe(boot, idem_key, argv, result_path, None)
}

fn payload_with_probe(
    boot: &TestBoot,
    idem_key: &str,
    argv: Vec<String>,
    result_path: PathBuf,
    probe_argv: Option<Vec<String>>,
) -> Value {
    serde_json::to_value(ForgeActionPayload {
        wave_id: boot.wave_id.clone(),
        card_id: new_id(),
        subject: subject(),
        argv,
        idem_key: idem_key.into(),
        event_spec: event_spec(),
        probe: probe_argv.map(|probe_argv| ProbeSpec { probe_argv }),
        cwd_lease: boot.cwd_lease(),
        result_path,
        deadline_ms: now_ms() + 30_000,
    })
    .expect("payload serializes")
}

fn op_key(idem_key: &str) -> OperationKey {
    OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(idem_key.into()),
        payload_hash: format!("forge-action-test:{idem_key}"),
    }
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).expect("write fake action");
    let mut perms = fs::metadata(path).expect("script metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod fake action");
}

async fn phase(repo: &SqlxRepo, op_id: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
        .bind(op_id)
        .fetch_one(repo.pool())
        .await
        .expect("phase query")
}

async fn wait_for_file(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {}", path.display());
}

async fn assert_absent_briefly(path: &Path) {
    for _ in 0..20 {
        assert!(
            !path.exists(),
            "{} appeared while it should have remained absent",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_operation_result(boot: &TestBoot, op_id: &str) -> OperationResult {
    for _ in 0..400 {
        if let Some(result) = boot
            .operation_repo
            .operation_result(op_id)
            .await
            .expect("operation result query")
        {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for operation {op_id} to finish");
}

fn read_counter(path: &Path) -> i64 {
    match fs::read_to_string(path) {
        Ok(text) => text.trim().parse().expect("counter parses"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("read counter {}: {e}", path.display()),
    }
}

fn write_counter_action(path: &Path) {
    write_script(
        path,
        r#"#!/bin/sh
counter=$1
sentinel=$2
finish=$3
n=0
if [ -f "$counter" ]; then n=$(cat "$counter"); fi
n=$((n + 1))
printf '%s\n' "$n" > "$counter"
: > "$sentinel"
while [ ! -f "$finish" ]; do sleep 0.02; done
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
"#,
    );
}

async fn spawn_live_counter_action(
    action: &Path,
    counter: &Path,
    sentinel: &Path,
    finish: &Path,
) -> (tokio::process::Child, SpawnArtifacts) {
    let mut cmd = tokio::process::Command::new(action);
    cmd.arg(counter)
        .arg(sentinel)
        .arg(finish)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn live fake action");
    let pid = child.id().expect("child pid") as i32;
    let artifacts = SpawnArtifacts {
        pid,
        pgid: pid,
        start_time: read_proc_start_time(pid).expect("child start time"),
        boot_id: read_boot_id().expect("current boot id"),
        log_path: None,
        extra: json!({ "test": "live-forge-reattach" }),
    };
    (child, artifacts)
}

fn write_probe(path: &Path, oid: &str, head: &str, verdict: i32) {
    write_script(
        path,
        &format!(
            r#"#!/bin/sh
if [ "${{1:-}}" = "--json" ]; then
  printf '%s\n' '{{"oid":"{oid}","headRefOid":"{head}"}}'
fi
exit {verdict}
"#,
        ),
    );
}

fn dead_artifacts() -> SpawnArtifacts {
    SpawnArtifacts {
        pid: 999_999_999,
        pgid: 999_999_999,
        start_time: 1,
        boot_id: "dead-test-boot".into(),
        log_path: None,
        extra: Value::Null,
    }
}

async fn latest_forge_event_payload(repo: &SqlxRepo) -> Value {
    let payload_text: String =
        sqlx::query_scalar("SELECT payload FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1")
            .bind("forge.pr.merged")
            .fetch_one(repo.pool())
            .await
            .expect("forge event payload exists");
    serde_json::from_str(&payload_text).expect("event payload parses")
}

async fn forge_event_count(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind("forge.pr.merged")
        .fetch_one(repo.pool())
        .await
        .expect("event count")
}

async fn insert_pending_forge_op(
    boot: &TestBoot,
    idem: &str,
    payload: Value,
) -> CalmResult<String> {
    let adapter = ForgeActionAdapter::new();
    adapter.validate(&payload).await?;
    boot.operation_repo
        .insert_operation(FORGE_ACTION_KIND, op_key(idem), payload)
        .await
}

async fn claim_one(boot: &TestBoot, expected_id: &str) -> CalmResult<Operation> {
    let claimed = boot.operation_repo.claim_drive_batch(32).await?;
    claimed
        .into_iter()
        .find(|op| op.id == expected_id)
        .ok_or_else(|| calm_server::error::CalmError::Internal("expected claimed op".into()))
}

async fn prepare_to_tx_committed(
    boot: &TestBoot,
    idem: &str,
    payload: Value,
) -> CalmResult<String> {
    let adapter = ForgeActionAdapter::new();
    let op_id = insert_pending_forge_op(boot, idem, payload).await?;
    let op = claim_one(boot, &op_id).await?;
    let prepared = boot
        .operation_repo
        .prepare_tx_and_advance(&op, &adapter)
        .await?
        .expect("operation prepares");
    assert_eq!(prepared.0.phase, Phase::TxCommitted);
    Ok(op_id)
}

async fn claimed_spawn_started_forge_op(
    boot: &TestBoot,
    idem: &str,
    payload: Value,
) -> CalmResult<(String, Operation, TxOutput)> {
    let op_id = prepare_to_tx_committed(boot, idem, payload).await?;
    let tx_committed = claim_one(boot, &op_id).await?;
    let spawn_started = boot
        .operation_repo
        .set_phase(&tx_committed, Phase::SpawnStarted)
        .await?
        .expect("set spawn_started");
    assert_eq!(spawn_started.phase, Phase::SpawnStarted);
    let spawn_claimed = claim_one(boot, &op_id).await?;
    let output = spawn_claimed.tx_output.clone().expect("tx output");
    Ok((op_id, spawn_claimed, output))
}

async fn seed_parked_forge_op(
    boot: &TestBoot,
    idem: &str,
    payload: Value,
    artifacts: SpawnArtifacts,
) -> CalmResult<String> {
    seed_parked_forge_op_with_deadline(boot, idem, payload, artifacts, now_ms() + 30_000).await
}

async fn seed_parked_forge_op_with_deadline(
    boot: &TestBoot,
    idem: &str,
    payload: Value,
    artifacts: SpawnArtifacts,
    deadline_ms: i64,
) -> CalmResult<String> {
    let (op_id, op, _output) = claimed_spawn_started_forge_op(boot, idem, payload).await?;
    boot.operation_repo
        .record_spawn_artifacts(&op, &artifacts)
        .await?;
    let parked = boot
        .operation_repo
        .set_parked(&op, deadline_ms)
        .await?
        .expect("operation parks");
    assert_eq!(parked.phase, Phase::Parked);
    Ok(op_id)
}

async fn spawn_observer_after_parking(
    boot: &TestBoot,
    op: &Operation,
    deadline_ms: i64,
    observer: calm_server::operation::ParkedObserver,
) -> CalmResult<tokio::task::JoinHandle<()>> {
    boot.operation_repo
        .set_parked(op, deadline_ms)
        .await?
        .expect("operation parks before observer");
    Ok(tokio::spawn(observer))
}

#[tokio::test]
async fn forge_action_rejects_relative_result_path() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let adapter = ForgeActionAdapter::new();
    let idem = "forge-relative-result-path";
    let payload = payload(
        &boot,
        idem,
        vec!["/bin/true".into()],
        PathBuf::from("relative-result.json"),
    );

    let err = adapter
        .validate(&payload)
        .await
        .expect_err("relative result_path must be rejected");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action result_path must be absolute"
        ),
        "{err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn forge_action_idempotency_on_resubmit_collapses_to_one_operation() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("instant-action.sh");
    write_script(
        &action,
        "#!/bin/sh\nprintf '%s\\n' '{\"oid\":\"abc123\",\"headRefOid\":\"def456\"}'\n",
    );
    let idem = "forge-idem-resubmit";
    let payload = payload(
        &boot,
        idem,
        vec![action.display().to_string()],
        boot.temp_path("instant-result.json"),
    );

    let first = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), payload.clone())
        .await?;
    let result = boot.runtime.wait(&first).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));

    let second = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), payload)
        .await?;
    assert_eq!(first, second);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
        .bind(FORGE_ACTION_KIND)
        .fetch_one(boot.repo.pool())
        .await?;
    assert_eq!(count, 1);
    Ok(())
}

#[tokio::test]
async fn forge_action_parks_releases_post_park_and_persists_typed_event() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("blocking-action.sh");
    let started = boot.temp_path("started");
    let finish = boot.temp_path("finish");
    write_script(
        &action,
        r#"#!/bin/sh
: > "$1"
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
while [ ! -f "$2" ]; do sleep 0.02; done
"#,
    );
    let idem = "forge-typed-completion";
    let op_id = boot
        .runtime
        .submit(
            FORGE_ACTION_KIND,
            op_key(idem),
            payload(
                &boot,
                idem,
                vec![
                    action.display().to_string(),
                    started.display().to_string(),
                    finish.display().to_string(),
                ],
                boot.temp_path("blocking-result.json"),
            ),
        )
        .await?;

    assert_eq!(phase(&boot.repo, &op_id).await, "parked");
    wait_for_file(&started).await;
    assert_eq!(
        phase(&boot.repo, &op_id).await,
        "parked",
        "fake action has started but cannot complete before the test releases it"
    );
    let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind("forge.pr.merged")
        .fetch_one(boot.repo.pool())
        .await?;
    assert_eq!(event_count, 0, "event must not exist while op is parked");

    fs::write(&finish, "").expect("release fake action");
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(phase(&boot.repo, &op_id).await, "succeeded");

    let payload_text: String =
        sqlx::query_scalar("SELECT payload FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1")
            .bind("forge.pr.merged")
            .fetch_one(boot.repo.pool())
            .await?;
    let event_payload: Value = serde_json::from_str(&payload_text)?;
    assert_eq!(event_payload["merge_sha"], json!("abc123"));
    assert_eq!(event_payload["head_sha"], json!("def456"));
    assert_eq!(event_payload["wave_id"], json!(boot.wave_id));
    assert_eq!(event_payload["subject"]["phase"], json!("impl"));
    assert_eq!(event_payload["subject"]["slice_id"], json!("slice-6"));
    assert_eq!(event_payload["subject"]["pr_number"], json!(760));

    Ok(())
}

#[tokio::test]
async fn forge_action_pre_park_dropped_observer_leaves_action_unrun_then_redrive_runs_once()
-> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("pre-park-action.sh");
    let counter = boot.temp_path("pre-park-counter");
    let sentinel = boot.temp_path("pre-park-sentinel");
    let finish = boot.temp_path("pre-park-finish");
    let result_path = boot.temp_path("pre-park-result.json");
    write_counter_action(&action);

    let idem = "forge-pre-park-drop";
    let payload = payload(
        &boot,
        idem,
        vec![
            action.display().to_string(),
            counter.display().to_string(),
            sentinel.display().to_string(),
            finish.display().to_string(),
        ],
        result_path.clone(),
    );
    let (op_id, op, output) = claimed_spawn_started_forge_op(&boot, idem, payload).await?;
    let adapter = ForgeActionAdapter::new();

    let SpawnOutcome::Parked { observer, .. } = adapter
        .spawn_side_effect(&output, &op, &boot.spawn_ctx)
        .await?
    else {
        panic!("forge action must park");
    };
    drop(observer);

    assert_absent_briefly(&result_path).await;
    assert!(
        !sentinel.exists(),
        "dropped pre-park observer must not release the held action"
    );
    assert_eq!(read_counter(&counter), 0);

    let redrive_op = boot
        .operation_repo
        .get_operation(&op_id)
        .await?
        .expect("op exists after first spawn");
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = adapter
        .spawn_side_effect(&output, &redrive_op, &boot.spawn_ctx)
        .await?
    else {
        panic!("forge action must park on redrive");
    };
    let observer = spawn_observer_after_parking(&boot, &redrive_op, deadline_ms, observer).await?;
    wait_for_file(&sentinel).await;
    fs::write(&finish, "").expect("release fake action");
    observer.await.expect("observer task joins");
    let result = boot.runtime.wait(&op_id).await?;

    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(read_counter(&counter), 1);
    assert_eq!(forge_event_count(&boot.repo).await, 1);

    Ok(())
}

#[tokio::test]
async fn forge_action_boot_recovery_redrives_non_terminal_phases_and_probes_parked()
-> CalmResult<()> {
    for (phase_name, expected_phase) in [
        ("pending", Phase::Pending),
        ("tx_committed", Phase::TxCommitted),
        ("spawn_started", Phase::SpawnStarted),
    ] {
        let boot = TestBoot::new().await;
        let action = boot.temp_path(&format!("{phase_name}-action.sh"));
        let counter = boot.temp_path(&format!("{phase_name}-counter"));
        let sentinel = boot.temp_path(&format!("{phase_name}-sentinel"));
        let finish = boot.temp_path(&format!("{phase_name}-finish"));
        let result_path = boot.temp_path(&format!("{phase_name}-result.json"));
        write_counter_action(&action);
        let idem = format!("forge-boot-{phase_name}");
        let payload = payload(
            &boot,
            &idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            result_path.clone(),
        );

        let op_id = match phase_name {
            "pending" => insert_pending_forge_op(&boot, &idem, payload).await?,
            "tx_committed" => prepare_to_tx_committed(&boot, &idem, payload).await?,
            "spawn_started" => {
                let (op_id, op, output) =
                    claimed_spawn_started_forge_op(&boot, &idem, payload).await?;
                let adapter = ForgeActionAdapter::new();
                let SpawnOutcome::Parked { observer, .. } = adapter
                    .spawn_side_effect(&output, &op, &boot.spawn_ctx)
                    .await?
                else {
                    panic!("forge action must park before simulated crash");
                };
                drop(observer);
                assert_absent_briefly(&result_path).await;
                assert_eq!(
                    read_counter(&counter),
                    0,
                    "pre-boot held launcher for spawn_started must not run"
                );
                op_id
            }
            _ => unreachable!(),
        };

        let plan = boot.runtime.recover_on_boot().await?;
        assert!(
            matches!(
                plan.items.as_slice(),
                [RecoveryItem::Recover { from_phase, .. }] if from_phase == &expected_phase
            ),
            "phase {phase_name} should use generic recovery re-drive: {:?}",
            plan.items
        );
        boot.runtime.apply_recovery(plan).await?;
        wait_for_file(&sentinel).await;
        assert_eq!(read_counter(&counter), 1);
        fs::write(&finish, "").expect("release recovered action");
        let result = boot.runtime.wait(&op_id).await?;
        assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
        assert_eq!(forge_event_count(&boot.repo).await, 1);
    }

    let boot = TestBoot::new().await;
    let action = boot.temp_path("parked-probe-action.sh");
    let counter = boot.temp_path("parked-probe-counter");
    let sentinel = boot.temp_path("parked-probe-sentinel");
    let finish = boot.temp_path("parked-probe-finish");
    let probe = boot.temp_path("parked-probe.sh");
    write_counter_action(&action);
    write_probe(&probe, "probe123", "probehead", 0);
    let idem = "forge-boot-parked-probe";
    let op_id = seed_parked_forge_op(
        &boot,
        idem,
        payload_with_probe(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            boot.temp_path("parked-probe-result.json"),
            Some(vec![probe.display().to_string()]),
        ),
        dead_artifacts(),
    )
    .await?;
    let plan = boot.runtime.recover_on_boot().await?;
    assert!(
        matches!(
            plan.items.as_slice(),
            [RecoveryItem::VerifyParked { op_id: item_op_id }] if item_op_id == &op_id
        ),
        "parked op should use recover_parked: {:?}",
        plan.items
    );
    boot.runtime.apply_recovery(plan).await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(
        read_counter(&counter),
        0,
        "parked probe recovery must not re-run argv"
    );
    assert!(!sentinel.exists());
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("probe123"));
    assert_eq!(event["head_sha"], json!("probehead"));
    let boot = TestBoot::new().await;
    let action = boot.temp_path("parked-no-probe-action.sh");
    let counter = boot.temp_path("parked-no-probe-counter");
    let sentinel = boot.temp_path("parked-no-probe-sentinel");
    let finish = boot.temp_path("parked-no-probe-finish");
    write_counter_action(&action);
    let idem = "forge-boot-parked-no-probe";
    let op_id = seed_parked_forge_op(
        &boot,
        idem,
        payload(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            boot.temp_path("parked-no-probe-result.json"),
        ),
        dead_artifacts(),
    )
    .await?;
    let plan = boot.runtime.recover_on_boot().await?;
    assert!(matches!(
        plan.items.as_slice(),
        [RecoveryItem::VerifyParked { .. }]
    ));
    boot.runtime.apply_recovery(plan).await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(
        matches!(
            result.outcome,
            OperationOutcome::Failed {
                ref last_error,
                from_phase: calm_server::operation::PhaseTag::Parked,
                last_error_class: Some(ref class),
            } if last_error.contains("gate-infra") && class == "parked_dead"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(
        read_counter(&counter),
        0,
        "parked no-probe failure must not re-run argv"
    );
    assert!(!sentinel.exists());
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    Ok(())
}

#[tokio::test]
async fn forge_action_boot_live_reattach_completes_via_probe_and_no_probe_fails() -> CalmResult<()>
{
    let boot = TestBoot::new().await;
    let action = boot.temp_path("live-reattach-action.sh");
    let counter = boot.temp_path("live-reattach-counter");
    let sentinel = boot.temp_path("live-reattach-sentinel");
    let finish = boot.temp_path("live-reattach-finish");
    let probe = boot.temp_path("live-reattach-probe.sh");
    write_counter_action(&action);
    write_probe(&probe, "live-probe-merge", "live-probe-head", 0);
    let (mut child, artifacts) =
        spawn_live_counter_action(&action, &counter, &sentinel, &finish).await;
    wait_for_file(&sentinel).await;
    assert_eq!(read_counter(&counter), 1);

    let idem = "forge-live-reattach-probe";
    let op_id = seed_parked_forge_op(
        &boot,
        idem,
        payload_with_probe(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            boot.temp_path("live-reattach-result.json"),
            Some(vec![probe.display().to_string()]),
        ),
        artifacts,
    )
    .await?;

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(
        matches!(
            plan.items.as_slice(),
            [RecoveryItem::VerifyParked { op_id: item_op_id }] if item_op_id == &op_id
        ),
        "live parked op should use recover_parked: {:?}",
        plan.items
    );
    boot.runtime.apply_recovery(plan).await?;
    assert_eq!(phase(&boot.repo, &op_id).await, "parked");
    assert_eq!(forge_event_count(&boot.repo).await, 0);

    fs::write(&finish, "").expect("release live fake action");
    let status = child.wait().await.expect("wait live fake action");
    assert!(status.success());
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(
        read_counter(&counter),
        1,
        "live reattach must not re-run argv"
    );
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("live-probe-merge"));
    assert_eq!(event["head_sha"], json!("live-probe-head"));

    let boot = TestBoot::new().await;
    let action = boot.temp_path("live-no-probe-action.sh");
    let counter = boot.temp_path("live-no-probe-counter");
    let sentinel = boot.temp_path("live-no-probe-sentinel");
    let finish = boot.temp_path("live-no-probe-finish");
    write_counter_action(&action);
    let (mut child, artifacts) =
        spawn_live_counter_action(&action, &counter, &sentinel, &finish).await;
    wait_for_file(&sentinel).await;
    assert_eq!(read_counter(&counter), 1);

    let idem = "forge-live-reattach-no-probe";
    let op_id = seed_parked_forge_op(
        &boot,
        idem,
        payload(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            boot.temp_path("live-no-probe-result.json"),
        ),
        artifacts,
    )
    .await?;

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(
        matches!(
            plan.items.as_slice(),
            [RecoveryItem::VerifyParked { op_id: item_op_id }] if item_op_id == &op_id
        ),
        "live no-probe parked op should use recover_parked: {:?}",
        plan.items
    );
    boot.runtime.apply_recovery(plan).await?;
    assert_eq!(phase(&boot.repo, &op_id).await, "parked");
    fs::write(&finish, "").expect("release live no-probe fake action");
    let status = child.wait().await.expect("wait live no-probe fake action");
    assert!(status.success());
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(
            result.outcome,
            OperationOutcome::Failed {
                ref last_error,
                from_phase: calm_server::operation::PhaseTag::Parked,
                last_error_class: Some(ref class),
            } if last_error == "forge action process dead with no probe; gate-infra"
                && class == "gate-infra"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "live no-probe reattach must not re-run argv"
    );
    assert_eq!(forge_event_count(&boot.repo).await, 0);

    Ok(())
}

async fn assert_dead_probe_failure(verdict: i32, expected_error: &str) -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path(&format!("probe-fail-{verdict}-action.sh"));
    let counter = boot.temp_path(&format!("probe-fail-{verdict}-counter"));
    let sentinel = boot.temp_path(&format!("probe-fail-{verdict}-sentinel"));
    let finish = boot.temp_path(&format!("probe-fail-{verdict}-finish"));
    let probe = boot.temp_path(&format!("probe-fail-{verdict}.sh"));
    write_counter_action(&action);
    write_probe(&probe, "ignored-merge", "ignored-head", verdict);
    let idem = format!("forge-probe-fail-{verdict}");
    let op_id = seed_parked_forge_op(
        &boot,
        &idem,
        payload_with_probe(
            &boot,
            &idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            boot.temp_path(&format!("probe-fail-{verdict}-result.json")),
            Some(vec![probe.display().to_string()]),
        ),
        dead_artifacts(),
    )
    .await?;

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(matches!(
        plan.items.as_slice(),
        [RecoveryItem::VerifyParked { .. }]
    ));
    boot.runtime.apply_recovery(plan).await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(
        matches!(
            result.outcome,
            OperationOutcome::Failed {
                ref last_error,
                from_phase: calm_server::operation::PhaseTag::Parked,
                last_error_class: Some(ref class),
            } if last_error == expected_error && class == "parked_dead"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(
        read_counter(&counter),
        0,
        "dead parked probe failure must not re-run argv"
    );
    assert!(!sentinel.exists());
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    Ok(())
}

#[tokio::test]
async fn forge_action_probe_not_landed_fails_dead_parked_without_rerun() -> CalmResult<()> {
    assert_dead_probe_failure(1, "forge action process dead and probe reports not landed").await
}

#[tokio::test]
async fn forge_action_probe_unknown_fails_dead_parked_without_rerun() -> CalmResult<()> {
    assert_dead_probe_failure(2, "forge action probe verdict unknown; gate-infra").await
}

#[tokio::test]
async fn forge_action_crash_then_probe_reextracts_probe_typed_output_fields() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("probe-reextract-action.sh");
    let counter = boot.temp_path("probe-reextract-counter");
    let sentinel = boot.temp_path("probe-reextract-sentinel");
    let finish = boot.temp_path("probe-reextract-finish");
    let result_path = boot.temp_path("probe-reextract-result.json");
    let probe = boot.temp_path("probe-reextract.sh");
    write_counter_action(&action);
    write_probe(&probe, "probe-sha", "probe-head", 0);

    let idem = "forge-probe-reextract";
    let op_id = seed_parked_forge_op(
        &boot,
        idem,
        payload_with_probe(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            result_path,
            Some(vec![probe.display().to_string()]),
        ),
        dead_artifacts(),
    )
    .await?;

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(matches!(
        plan.items.as_slice(),
        [RecoveryItem::VerifyParked { .. }]
    ));
    boot.runtime.apply_recovery(plan).await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));

    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("probe-sha"));
    assert_eq!(event["head_sha"], json!("probe-head"));
    assert_eq!(event["wave_id"], json!(boot.wave_id));
    assert_eq!(event["subject"], serde_json::to_value(subject())?);
    assert_eq!(
        read_counter(&counter),
        0,
        "probe recovery must not re-run argv"
    );
    assert!(!sentinel.exists());

    Ok(())
}

#[tokio::test]
async fn forge_action_dead_past_deadline_uses_probe_and_no_probe_times_out() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("past-deadline-probe-action.sh");
    let counter = boot.temp_path("past-deadline-probe-counter");
    let sentinel = boot.temp_path("past-deadline-probe-sentinel");
    let finish = boot.temp_path("past-deadline-probe-finish");
    let probe = boot.temp_path("past-deadline-probe.sh");
    write_counter_action(&action);
    write_probe(&probe, "past-deadline-merge", "past-deadline-head", 0);

    let idem = "forge-past-deadline-dead-probe";
    let op_id = seed_parked_forge_op_with_deadline(
        &boot,
        idem,
        payload_with_probe(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            boot.temp_path("past-deadline-probe-result.json"),
            Some(vec![probe.display().to_string()]),
        ),
        dead_artifacts(),
        now_ms() - 1,
    )
    .await?;

    let op = boot
        .operation_repo
        .get_operation(&op_id)
        .await?
        .expect("parked op exists");
    let adapter = ForgeActionAdapter::new();
    let recovery = adapter
        .recover_parked(
            &op,
            &dead_artifacts(),
            false,
            RecoveryMode::PastDeadline,
            &boot.spawn_ctx,
        )
        .await?;
    assert!(matches!(recovery, ParkedRecovery::LeaveParked));
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("past-deadline-merge"));
    assert_eq!(event["head_sha"], json!("past-deadline-head"));
    assert_eq!(
        read_counter(&counter),
        0,
        "dead past-deadline probe recovery must not re-run argv"
    );
    assert!(!sentinel.exists());

    let boot = TestBoot::new().await;
    let action = boot.temp_path("past-deadline-no-probe-action.sh");
    let counter = boot.temp_path("past-deadline-no-probe-counter");
    let sentinel = boot.temp_path("past-deadline-no-probe-sentinel");
    let finish = boot.temp_path("past-deadline-no-probe-finish");
    write_counter_action(&action);
    let idem = "forge-past-deadline-dead-no-probe";
    let op_id = seed_parked_forge_op_with_deadline(
        &boot,
        idem,
        payload(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                counter.display().to_string(),
                sentinel.display().to_string(),
                finish.display().to_string(),
            ],
            boot.temp_path("past-deadline-no-probe-result.json"),
        ),
        dead_artifacts(),
        now_ms() - 1,
    )
    .await?;

    let op = boot
        .operation_repo
        .get_operation(&op_id)
        .await?
        .expect("parked op exists");
    let adapter = ForgeActionAdapter::new();
    let recovery = adapter
        .recover_parked(
            &op,
            &dead_artifacts(),
            false,
            RecoveryMode::PastDeadline,
            &boot.spawn_ctx,
        )
        .await?;
    assert!(
        matches!(
            recovery,
            ParkedRecovery::Fail { ref reason } if reason == "action-timeout"
        ),
        "{recovery:?}"
    );
    assert_eq!(
        read_counter(&counter),
        0,
        "dead past-deadline no-probe recovery must not re-run argv"
    );
    assert!(!sentinel.exists());
    assert_eq!(forge_event_count(&boot.repo).await, 0);

    Ok(())
}
