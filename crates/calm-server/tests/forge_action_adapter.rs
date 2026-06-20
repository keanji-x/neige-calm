#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
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
    forge_passthrough_env_for_test,
};
use calm_server::operation::{
    Operation, OperationCompletionBus, OperationKey, OperationOutcome, OperationRepo,
    OperationResult, OperationRuntime, ParkedRecovery, Phase, RecoveryItem, RecoveryMode,
    SpawnArtifacts, SpawnCtx, SpawnOutcome, SqlxOperationRepo, TxOutput,
};
use calm_server::proc_identity::{read_boot_id, read_proc_start_time, signal_process_group};
use calm_server::routes::theme::RequestTheme;
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use serde_json::{Value, json};
use tempfile::TempDir;

static FORGE_ENV_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvVarGuard {
    key: String,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: tests that mutate forge env take FORGE_ENV_TEST_LOCK and use
        // unique keys so subprocess assertions cannot observe each other.
        unsafe { std::env::set_var(key, value) };
        Self {
            key: key.to_string(),
            previous,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see EnvVarGuard::set; the guard restores the process env while
        // the same test-level lock is still held.
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(&self.key, previous);
            } else {
                std::env::remove_var(&self.key);
            }
        }
    }
}

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
    event_spec_for("forge.pr.merged")
}

fn event_spec_for(kind: &str) -> ForgeEventSpec {
    let mut fields = BTreeMap::new();
    if kind == "forge.pr.merged" {
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
    }
    ForgeEventSpec {
        event_kind: kind.into(),
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
    payload_with_probe(boot, idem_key, argv, result_path, None, None)
}

fn payload_with_probe(
    boot: &TestBoot,
    idem_key: &str,
    argv: Vec<String>,
    result_path: PathBuf,
    probe_argv: Option<Vec<String>>,
    output_probe_argv: Option<Vec<String>>,
) -> Value {
    serde_json::to_value(ForgeActionPayload {
        wave_id: boot.wave_id.clone(),
        card_id: new_id(),
        subject: Some(subject()),
        argv,
        idem_key: idem_key.into(),
        event_spec: Some(event_spec()),
        context: Default::default(),
        probe: probe_argv.map(|probe_argv| ProbeSpec {
            probe_argv,
            output_probe_argv,
        }),
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

fn write_killable_action(path: &Path) {
    write_script(
        path,
        r#"#!/bin/sh
: > "$1"
while true; do sleep 0.02; done
"#,
    );
}

fn write_finish_action(path: &Path) {
    write_script(
        path,
        r#"#!/bin/sh
: > "$1"
while [ ! -f "$2" ]; do sleep 0.02; done
printf '%s\n' '{"oid":"action-merge","headRefOid":"action-head"}'
"#,
    );
}

fn write_http_proxy_action(path: &Path) {
    write_script(
        path,
        r#"#!/bin/sh
printf '{"oid":"%s","headRefOid":"proxy-head"}\n' "$HTTP_PROXY"
"#,
    );
}

fn write_env_json_script(path: &Path, allowed_key: &str, forbidden_key: &str) {
    let mut body =
        String::from("#!/bin/sh\nprintf '{\"oid\":\"%s\",\"headRefOid\":\"%s\"}\\n' \"$");
    body.push_str(allowed_key);
    body.push_str("\" \"$");
    body.push_str(forbidden_key);
    body.push_str("\"\n");
    write_script(path, &body);
}

fn write_missing_oid_counter_action(path: &Path) {
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
printf '%s\n' '{"headRefOid":"action-head-without-merge"}'
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

fn write_verdict_probe(path: &Path, verdict: i32, stdout: &str) {
    write_script(
        path,
        &format!(
            r#"#!/bin/sh
printf '%s\n' '{}'
exit {verdict}
"#,
            stdout.replace('\'', "'\\''")
        ),
    );
}

fn write_counting_probe(path: &Path, oid: &str, head: &str, verdict: i32) {
    write_script(
        path,
        &format!(
            r#"#!/bin/sh
counter=$1
n=0
if [ -f "$counter" ]; then n=$(cat "$counter"); fi
printf '%s\n' "$((n + 1))" > "$counter"
if [ "${{2:-}}" = "--json" ]; then
  printf '%s\n' '{{"oid":"{oid}","headRefOid":"{head}"}}'
fi
exit {verdict}
"#,
        ),
    );
}

fn output_probe_argv(probe: &Path) -> Vec<String> {
    vec![probe.display().to_string(), "--json".into()]
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

fn stage_result_files(result_path: &Path, code: &str, stdout: &str) {
    fs::write(format!("{}.code", result_path.display()), code).expect("stage result code");
    fs::write(format!("{}.stdout", result_path.display()), stdout).expect("stage result stdout");
}

fn stage_result_code(result_path: &Path, code: &str) {
    fs::write(format!("{}.code", result_path.display()), code).expect("stage result code");
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

async fn latest_event_payload(repo: &SqlxRepo, kind: &str) -> Value {
    let payload_text: String =
        sqlx::query_scalar("SELECT payload FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1")
            .bind(kind)
            .fetch_one(repo.pool())
            .await
            .expect("event payload exists");
    serde_json::from_str(&payload_text).expect("event payload parses")
}

async fn latest_event_scope(
    repo: &SqlxRepo,
    kind: &str,
) -> (String, Option<String>, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT scope_kind, scope_cove, scope_wave, scope_card \
         FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_one(repo.pool())
    .await
    .expect("event scope exists")
}

async fn forge_event_count(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind("forge.pr.merged")
        .fetch_one(repo.pool())
        .await
        .expect("event count")
}

async fn event_count(repo: &SqlxRepo, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .expect("event count")
}

async fn all_event_count(repo: &SqlxRepo) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events")
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

async fn spawn_parked_observer(
    boot: &TestBoot,
    idem: &str,
    payload: Value,
) -> CalmResult<(String, tokio::task::JoinHandle<()>)> {
    let (op_id, op, output) = claimed_spawn_started_forge_op(boot, idem, payload).await?;
    let adapter = ForgeActionAdapter::new();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = adapter
        .spawn_side_effect(&output, &op, &boot.spawn_ctx)
        .await?
    else {
        panic!("forge action must park");
    };
    let observer = spawn_observer_after_parking(boot, &op, deadline_ms, observer).await?;
    Ok((op_id, observer))
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
async fn forge_action_rejects_json_probe_without_output_probe() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let adapter = ForgeActionAdapter::new();
    let idem = "forge-missing-output-probe";
    let payload = payload_with_probe(
        &boot,
        idem,
        vec!["/bin/true".into()],
        boot.temp_path("missing-output-probe-result.json"),
        Some(vec!["/bin/true".into()]),
        None,
    );

    let err = adapter
        .validate(&payload)
        .await
        .expect_err("JsonField recovery probe must declare output_probe_argv");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action probe.output_probe_argv must be present when event_spec uses JsonField"
        ),
        "{err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn forge_action_rejects_non_forge_event_kind() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let adapter = ForgeActionAdapter::new();
    let idem = "forge-non-forge-event-kind";
    let mut payload = payload(
        &boot,
        idem,
        vec!["/bin/true".into()],
        boot.temp_path("non-forge-event-kind-result.json"),
    );
    payload["subject"] = Value::Null;
    payload["event_spec"]["event_kind"] = json!("wave.deleted");

    let err = adapter
        .validate(&payload)
        .await
        .expect_err("non-forge event kind must be rejected");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action event_kind `wave.deleted` is not a supported forge event kind"
        ),
        "{err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn forge_action_rejects_unsupported_forge_event_kind() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let adapter = ForgeActionAdapter::new();
    let idem = "forge-unsupported-event-kind";
    let mut payload = payload(
        &boot,
        idem,
        vec!["/bin/true".into()],
        boot.temp_path("unsupported-event-kind-result.json"),
    );
    payload["subject"] = Value::Null;
    payload["event_spec"]["event_kind"] = json!("forge.pr.merge");

    let err = adapter
        .validate(&payload)
        .await
        .expect_err("unsupported forge event kind must be rejected");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action event_kind `forge.pr.merge` is not a supported forge event kind"
        ),
        "{err:?}"
    );
    Ok(())
}

#[tokio::test]
async fn forge_action_rejects_missing_required_event_output_field_before_spawn() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("missing-required-field-action.sh");
    let counter = boot.temp_path("missing-required-field-counter");
    write_script(
        &action,
        r#"#!/bin/sh
n=0
if [ -f "$1" ]; then n=$(cat "$1"); fi
printf '%s\n' "$((n + 1))" > "$1"
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
"#,
    );
    let idem = "forge-missing-required-output-field";
    let mut payload = payload(
        &boot,
        idem,
        vec![action.display().to_string(), counter.display().to_string()],
        boot.temp_path("missing-required-field-result.json"),
    );
    payload["event_spec"]["fields"]
        .as_object_mut()
        .expect("event_spec.fields object")
        .remove("merge_sha");

    let err = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), payload)
        .await
        .expect_err("missing required output field must be rejected");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action event_spec for `forge.pr.merged` must populate field `merge_sha`"
        ),
        "{err:?}"
    );
    assert_eq!(read_counter(&counter), 0, "argv must not run");
    Ok(())
}

#[tokio::test]
async fn forge_action_rejects_exit_code_for_required_string_field_before_spawn() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("exit-code-string-field-action.sh");
    let counter = boot.temp_path("exit-code-string-field-counter");
    write_script(
        &action,
        r#"#!/bin/sh
n=0
if [ -f "$1" ]; then n=$(cat "$1"); fi
printf '%s\n' "$((n + 1))" > "$1"
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
"#,
    );
    let idem = "forge-exit-code-string-field";
    let mut payload = payload(
        &boot,
        idem,
        vec![action.display().to_string(), counter.display().to_string()],
        boot.temp_path("exit-code-string-field-result.json"),
    );
    payload["event_spec"]["fields"]["merge_sha"] = json!("exit_code");

    let err = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), payload)
        .await
        .expect_err("exit_code source for a required string field must be rejected");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action `forge.pr.merged` field `merge_sha` must be a JSON string source, not exit_code"
        ),
        "{err:?}"
    );
    assert_eq!(read_counter(&counter), 0, "argv must not run");
    Ok(())
}

#[tokio::test]
async fn forge_action_rejects_malformed_json_pointer_before_spawn_for_any_field() -> CalmResult<()>
{
    let boot = TestBoot::new().await;
    let action = boot.temp_path("malformed-pointer-required-action.sh");
    let counter = boot.temp_path("malformed-pointer-required-counter");
    write_script(
        &action,
        r#"#!/bin/sh
n=0
if [ -f "$1" ]; then n=$(cat "$1"); fi
printf '%s\n' "$((n + 1))" > "$1"
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
"#,
    );
    let idem = "forge-malformed-pointer-required";
    let mut required_payload = payload(
        &boot,
        idem,
        vec![action.display().to_string(), counter.display().to_string()],
        boot.temp_path("malformed-pointer-required-result.json"),
    );
    required_payload["event_spec"]["fields"]["merge_sha"] =
        serde_json::to_value(FieldSource::JsonField { path: "oid".into() })?;

    let err = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), required_payload)
        .await
        .expect_err("malformed required JsonField pointer must be rejected");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action event_spec field `merge_sha` JsonField path `oid` must be a valid JSON Pointer (empty string or starting with `/`)"
        ),
        "{err:?}"
    );
    assert_eq!(read_counter(&counter), 0, "argv must not run");

    let boot = TestBoot::new().await;
    let action = boot.temp_path("malformed-pointer-extra-action.sh");
    let counter = boot.temp_path("malformed-pointer-extra-counter");
    write_script(
        &action,
        r#"#!/bin/sh
n=0
if [ -f "$1" ]; then n=$(cat "$1"); fi
printf '%s\n' "$((n + 1))" > "$1"
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
"#,
    );
    let idem = "forge-malformed-pointer-extra";
    let mut extra_payload = payload(
        &boot,
        idem,
        vec![action.display().to_string(), counter.display().to_string()],
        boot.temp_path("malformed-pointer-extra-result.json"),
    );
    extra_payload["event_spec"]["fields"]["extra"] =
        serde_json::to_value(FieldSource::JsonField {
            path: "extra".into(),
        })?;

    let err = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), extra_payload)
        .await
        .expect_err("malformed non-required JsonField pointer must be rejected");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge-action event_spec field `extra` JsonField path `extra` must be a valid JSON Pointer (empty string or starting with `/`)"
        ),
        "{err:?}"
    );
    assert_eq!(read_counter(&counter), 0, "argv must not run");
    Ok(())
}

#[tokio::test]
async fn forge_action_accepts_valid_json_pointer_syntax() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let adapter = ForgeActionAdapter::new();
    let idem = "forge-valid-pointer-slash";
    let slash_payload = payload(
        &boot,
        idem,
        vec!["/bin/true".into()],
        boot.temp_path("valid-pointer-slash-result.json"),
    );
    adapter.validate(&slash_payload).await?;

    let idem = "forge-valid-pointer-root";
    let mut root_payload = payload(
        &boot,
        idem,
        vec!["/bin/true".into()],
        boot.temp_path("valid-pointer-root-result.json"),
    );
    root_payload["event_spec"]["fields"]["merge_sha"] =
        serde_json::to_value(FieldSource::JsonField {
            path: String::new(),
        })?;
    adapter.validate(&root_payload).await?;
    Ok(())
}

#[tokio::test]
async fn forge_action_rejects_subject_shape_before_spawn() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let adapter = ForgeActionAdapter::new();

    let mut missing_subject = payload(
        &boot,
        "forge-merge-missing-subject",
        vec!["/bin/true".into()],
        boot.temp_path("merge-missing-subject-result.json"),
    );
    missing_subject["subject"] = Value::Null;
    let err = adapter
        .validate(&missing_subject)
        .await
        .expect_err("forge.pr.merged requires subject");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message) if message == "forge.pr.merged requires subject"
        ),
        "{err:?}"
    );

    let mut non_merge_subject = payload(
        &boot,
        "forge-scan-with-subject",
        vec!["/bin/true".into()],
        boot.temp_path("scan-with-subject-result.json"),
    );
    non_merge_subject["event_spec"] = serde_json::to_value(event_spec_for("forge.scan.completed"))?;
    let err = adapter
        .validate(&non_merge_subject)
        .await
        .expect_err("non-merge events must not carry subject");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "subject is only valid for forge.pr.merged"
        ),
        "{err:?}"
    );

    let mut resultless_subject = payload(
        &boot,
        "forge-resultless-with-subject",
        vec!["/bin/true".into()],
        boot.temp_path("resultless-with-subject-result.json"),
    );
    resultless_subject["event_spec"] = Value::Null;
    let err = adapter
        .validate(&resultless_subject)
        .await
        .expect_err("resultless events must not carry subject");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "subject is only valid for forge.pr.merged"
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

async fn assert_killed_observer_resolution(
    idem: &str,
    probe_verdict: Option<i32>,
) -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path(&format!("{idem}-action.sh"));
    let started = boot.temp_path(&format!("{idem}-started"));
    let probe = boot.temp_path(&format!("{idem}-probe.sh"));
    write_killable_action(&action);
    let payload = if let Some(verdict) = probe_verdict {
        write_probe(&probe, "killed-probe-merge", "killed-probe-head", verdict);
        payload_with_probe(
            &boot,
            idem,
            vec![action.display().to_string(), started.display().to_string()],
            boot.temp_path(&format!("{idem}-result.json")),
            Some(vec![probe.display().to_string()]),
            Some(output_probe_argv(&probe)),
        )
    } else {
        payload(
            &boot,
            idem,
            vec![action.display().to_string(), started.display().to_string()],
            boot.temp_path(&format!("{idem}-result.json")),
        )
    };
    let (op_id, observer) = spawn_parked_observer(&boot, idem, payload).await?;
    wait_for_file(&started).await;
    let parked = boot
        .operation_repo
        .get_operation(&op_id)
        .await?
        .expect("parked op exists");
    let artifacts = parked.spawn_artifacts.expect("spawn artifacts recorded");
    assert!(signal_process_group(artifacts.pgid, libc::SIGKILL));
    observer.await.expect("observer joins");
    let result = wait_for_operation_result(&boot, &op_id).await;

    match probe_verdict {
        Some(0) => {
            assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
            let event = latest_forge_event_payload(&boot.repo).await;
            assert_eq!(event["merge_sha"], json!("killed-probe-merge"));
            assert_eq!(event["head_sha"], json!("killed-probe-head"));
        }
        Some(1) => {
            assert!(
                matches!(
                    result.outcome,
                    OperationOutcome::Failed {
                        ref last_error,
                        from_phase: calm_server::operation::PhaseTag::Parked,
                        last_error_class: Some(ref class),
                    } if last_error == "forge action process dead and probe reports not landed"
                        && class == "action-not-landed"
                ),
                "{:?}",
                result.outcome
            );
            assert_eq!(forge_event_count(&boot.repo).await, 0);
        }
        None => {
            assert!(
                matches!(
                    result.outcome,
                    OperationOutcome::Failed {
                        ref last_error,
                        from_phase: calm_server::operation::PhaseTag::Parked,
                        last_error_class: Some(ref class),
                    } if last_error
                        == "gate-infra: forge wrapper killed by signal; no probe to resolve outcome"
                        && class == "gate-infra"
                ),
                "{:?}",
                result.outcome
            );
            assert_eq!(forge_event_count(&boot.repo).await, 0);
        }
        other => panic!("unexpected probe verdict {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn forge_action_observer_killed_by_signal_resolves_via_probe() -> CalmResult<()> {
    assert_killed_observer_resolution("forge-killed-probe-landed", Some(0)).await?;
    assert_killed_observer_resolution("forge-killed-probe-not-landed", Some(1)).await?;
    assert_killed_observer_resolution("forge-killed-no-probe", None).await?;
    Ok(())
}

#[tokio::test]
async fn forge_action_observer_unreadable_result_resolves_via_probe() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("unreadable-result-action.sh");
    let started = boot.temp_path("unreadable-result-started");
    let finish = boot.temp_path("unreadable-result-finish");
    let result_dir = boot.temp_path("unreadable-result-dir");
    let result_path = result_dir.join("result.json");
    let probe = boot.temp_path("unreadable-result-probe.sh");
    write_finish_action(&action);
    write_probe(&probe, "unreadable-probe-merge", "unreadable-probe-head", 0);

    let idem = "forge-unreadable-result-probe";
    let (op_id, observer) = spawn_parked_observer(
        &boot,
        idem,
        payload_with_probe(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                started.display().to_string(),
                finish.display().to_string(),
            ],
            result_path,
            Some(vec![probe.display().to_string()]),
            Some(output_probe_argv(&probe)),
        ),
    )
    .await?;
    wait_for_file(&started).await;
    let mut perms = fs::metadata(&result_dir)
        .expect("result dir metadata")
        .permissions();
    perms.set_mode(0o500);
    fs::set_permissions(&result_dir, perms).expect("make result dir unwritable");
    fs::write(&finish, "").expect("release fake action");
    observer.await.expect("observer joins");
    let mut perms = fs::metadata(&result_dir)
        .expect("result dir metadata")
        .permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&result_dir, perms).expect("restore result dir permissions");

    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("unreadable-probe-merge"));
    assert_eq!(event["head_sha"], json!("unreadable-probe-head"));

    Ok(())
}

#[tokio::test]
async fn forge_action_live_extraction_failure_recovers_via_probe_without_rerun() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("live-extraction-failure-action.sh");
    let counter = boot.temp_path("live-extraction-failure-counter");
    let sentinel = boot.temp_path("live-extraction-failure-sentinel");
    let finish = boot.temp_path("live-extraction-failure-finish");
    let probe = boot.temp_path("live-extraction-failure-probe.sh");
    write_missing_oid_counter_action(&action);
    write_probe(&probe, "probe-recovered-merge", "probe-recovered-head", 0);

    let idem = "forge-live-extraction-failure-probe";
    let (op_id, observer) = spawn_parked_observer(
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
            boot.temp_path("live-extraction-failure-result.json"),
            Some(vec![probe.display().to_string()]),
            Some(output_probe_argv(&probe)),
        ),
    )
    .await?;
    wait_for_file(&sentinel).await;
    fs::write(&finish, "").expect("release fake action");
    observer.await.expect("observer joins");
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    assert_eq!(
        read_counter(&counter),
        1,
        "live extraction recovery must not re-run argv"
    );
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("probe-recovered-merge"));
    assert_eq!(event["head_sha"], json!("probe-recovered-head"));
    assert_eq!(forge_event_count(&boot.repo).await, 1);
    Ok(())
}

#[tokio::test]
async fn forge_action_live_extraction_failure_without_probe_fails_gate_infra() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("live-extraction-no-probe-action.sh");
    let counter = boot.temp_path("live-extraction-no-probe-counter");
    let sentinel = boot.temp_path("live-extraction-no-probe-sentinel");
    let finish = boot.temp_path("live-extraction-no-probe-finish");
    write_missing_oid_counter_action(&action);

    let idem = "forge-live-extraction-failure-no-probe";
    let (op_id, observer) = spawn_parked_observer(
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
            boot.temp_path("live-extraction-no-probe-result.json"),
        ),
    )
    .await?;
    wait_for_file(&sentinel).await;
    fs::write(&finish, "").expect("release fake action");
    observer.await.expect("observer joins");
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(
            result.outcome,
            OperationOutcome::Failed {
                ref last_error,
                from_phase: calm_server::operation::PhaseTag::Parked,
                last_error_class: Some(ref class),
            } if last_error.contains("extraction failed")
                && last_error.contains("no probe to resolve outcome")
                && class == "gate-infra"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "live extraction failure must not re-run argv"
    );
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    Ok(())
}

#[tokio::test]
async fn forge_action_nonzero_exit_fails_action_failed_without_probe() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("nonzero-action.sh");
    let action_counter = boot.temp_path("nonzero-action-counter");
    let probe = boot.temp_path("nonzero-probe.sh");
    let probe_counter = boot.temp_path("nonzero-probe-counter");
    write_script(
        &action,
        r#"#!/bin/sh
n=0
if [ -f "$1" ]; then n=$(cat "$1"); fi
printf '%s\n' "$((n + 1))" > "$1"
printf '%s\n' '{"oid":"ignored-merge","headRefOid":"ignored-head"}'
exit 42
"#,
    );
    write_counting_probe(&probe, "probe-must-not-run", "probe-must-not-run-head", 0);

    let idem = "forge-nonzero-no-probe-consult";
    let (op_id, observer) = spawn_parked_observer(
        &boot,
        idem,
        payload_with_probe(
            &boot,
            idem,
            vec![
                action.display().to_string(),
                action_counter.display().to_string(),
            ],
            boot.temp_path("nonzero-result.json"),
            Some(vec![
                probe.display().to_string(),
                probe_counter.display().to_string(),
            ]),
            Some(vec![
                probe.display().to_string(),
                probe_counter.display().to_string(),
                "--json".into(),
            ]),
        ),
    )
    .await?;
    observer.await.expect("observer joins");
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(
            result.outcome,
            OperationOutcome::Failed {
                ref last_error,
                from_phase: calm_server::operation::PhaseTag::Parked,
                last_error_class: Some(ref class),
            } if last_error == "forge action exited with code 42" && class == "action-failed"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(read_counter(&action_counter), 1);
    assert_eq!(
        read_counter(&probe_counter),
        0,
        "nonzero action exit must not consult probe"
    );
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    Ok(())
}

#[tokio::test]
async fn forge_action_persists_non_merge_event_from_context() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("scan-completed-action.sh");
    write_script(&action, "#!/bin/sh\nprintf '%s\\n' 'scan complete'\n");

    let idem = "forge-scan-completed-context";
    let mut input = payload(
        &boot,
        idem,
        vec![action.display().to_string()],
        boot.temp_path("scan-completed-result.json"),
    );
    input["subject"] = Value::Null;
    input["event_spec"] = serde_json::to_value(event_spec_for("forge.scan.completed"))?;
    input["context"] = json!({ "overlapping_prs": [1, 2] });

    let op_id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), input)
        .await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{:?}",
        result.outcome
    );

    let event = latest_event_payload(&boot.repo, "forge.scan.completed").await;
    assert_eq!(event["wave_id"], json!(boot.wave_id));
    assert_eq!(event["overlapping_prs"], json!([1, 2]));
    assert!(event.get("subject").is_none());
    assert_eq!(event_count(&boot.repo, "forge.scan.completed").await, 1);
    Ok(())
}

#[tokio::test]
async fn forge_action_scopes_worktree_events_to_card_and_forge_events_to_wave() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("scoped-event-action.sh");
    write_script(&action, "#!/bin/sh\nprintf '%s\\n' 'ok'\n");

    let worktree_idem = "forge-worktree-provisioned-scope";
    let mut worktree_input = payload(
        &boot,
        worktree_idem,
        vec![action.display().to_string()],
        boot.temp_path("worktree-provisioned-scope-result.json"),
    );
    worktree_input["subject"] = Value::Null;
    worktree_input["event_spec"] = serde_json::to_value(event_spec_for("worktree.provisioned"))?;
    let card_id = worktree_input["card_id"]
        .as_str()
        .expect("payload card_id")
        .to_string();
    worktree_input["context"] = json!({
        "card_id": card_id,
        "path": "/tmp/neige/worktrees/card-1"
    });

    let op_id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(worktree_idem), worktree_input)
        .await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));

    let worktree_event = latest_event_payload(&boot.repo, "worktree.provisioned").await;
    assert_eq!(worktree_event["wave_id"], json!(boot.wave_id));
    assert_eq!(worktree_event["card_id"], json!(card_id));
    let (scope_kind, scope_cove, scope_wave, scope_card) =
        latest_event_scope(&boot.repo, "worktree.provisioned").await;
    assert_eq!(scope_kind, "card");
    assert_eq!(scope_card.as_deref(), Some(card_id.as_str()));
    assert_eq!(scope_wave.as_deref(), Some(boot.wave_id.as_str()));
    assert!(scope_cove.is_some(), "card scope carries cove");

    let forge_idem = "forge-scan-completed-wave-scope";
    let mut forge_input = payload(
        &boot,
        forge_idem,
        vec![action.display().to_string()],
        boot.temp_path("scan-completed-wave-scope-result.json"),
    );
    forge_input["subject"] = Value::Null;
    forge_input["event_spec"] = serde_json::to_value(event_spec_for("forge.scan.completed"))?;
    forge_input["context"] = json!({ "overlapping_prs": [7] });

    let op_id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(forge_idem), forge_input)
        .await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));

    let (scope_kind, scope_cove, scope_wave, scope_card) =
        latest_event_scope(&boot.repo, "forge.scan.completed").await;
    assert_eq!(scope_kind, "wave");
    assert_eq!(scope_wave.as_deref(), Some(boot.wave_id.as_str()));
    assert!(scope_cove.is_some(), "wave scope carries cove");
    assert!(scope_card.is_none(), "forge.* events remain wave-scoped");
    Ok(())
}

#[tokio::test]
async fn forge_action_resultless_succeeds_without_event_row() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("resultless-action.sh");
    write_script(&action, "#!/bin/sh\nprintf '%s\\n' 'committed'\n");

    let before = all_event_count(&boot.repo).await;
    let idem = "forge-resultless";
    let mut input = payload(
        &boot,
        idem,
        vec![action.display().to_string()],
        boot.temp_path("resultless-result.json"),
    );
    input["subject"] = Value::Null;
    input["event_spec"] = Value::Null;

    let op_id = boot
        .runtime
        .submit(FORGE_ACTION_KIND, op_key(idem), input)
        .await?;
    let result = boot.runtime.wait(&op_id).await?;
    match result.outcome {
        OperationOutcome::Succeeded { result } => {
            assert_eq!(result["exit_code"], json!(0));
            assert_eq!(result["event_kind"], Value::Null);
            assert_eq!(result["event"], Value::Null);
        }
        other => panic!("expected resultless success, got {other:?}"),
    }
    assert_eq!(
        all_event_count(&boot.repo).await,
        before,
        "resultless success must not append an event"
    );
    Ok(())
}

#[tokio::test]
async fn forge_action_rejects_reserved_event_keys_before_spawn() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("reserved-context-action.sh");
    let counter = boot.temp_path("reserved-context-counter");
    write_script(
        &action,
        r#"#!/bin/sh
n=0
if [ -f "$1" ]; then n=$(cat "$1"); fi
printf '%s\n' "$((n + 1))" > "$1"
printf '%s\n' '{"oid":"abc123","headRefOid":"def456"}'
"#,
    );

    let mut context_wave = payload(
        &boot,
        "forge-reserved-context-wave",
        vec![action.display().to_string(), counter.display().to_string()],
        boot.temp_path("reserved-context-wave-result.json"),
    );
    context_wave["context"] = json!({ "wave_id": "plugin-wave" });
    let err = boot
        .runtime
        .submit(
            FORGE_ACTION_KIND,
            op_key("forge-reserved-context-wave"),
            context_wave,
        )
        .await
        .expect_err("context.wave_id must be rejected before spawn");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge event context/output may not set reserved key `wave_id`"
        ),
        "{err:?}"
    );

    let mut context_subject = payload(
        &boot,
        "forge-reserved-context-subject",
        vec![action.display().to_string(), counter.display().to_string()],
        boot.temp_path("reserved-context-subject-result.json"),
    );
    context_subject["context"] = json!({ "subject": { "phase": "impl" } });
    let err = boot
        .runtime
        .submit(
            FORGE_ACTION_KIND,
            op_key("forge-reserved-context-subject"),
            context_subject,
        )
        .await
        .expect_err("context.subject must be rejected before spawn");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge event context/output may not set reserved key `subject`"
        ),
        "{err:?}"
    );

    let mut field_wave = payload(
        &boot,
        "forge-reserved-field-wave",
        vec![action.display().to_string(), counter.display().to_string()],
        boot.temp_path("reserved-field-wave-result.json"),
    );
    field_wave["event_spec"]["fields"]["wave_id"] = json!("exit_code");
    let err = boot
        .runtime
        .submit(
            FORGE_ACTION_KIND,
            op_key("forge-reserved-field-wave"),
            field_wave,
        )
        .await
        .expect_err("event_spec.fields.wave_id must be rejected before spawn");
    assert!(
        matches!(
            err,
            CalmError::BadRequest(ref message)
                if message == "forge event context/output may not set reserved key `wave_id`"
        ),
        "{err:?}"
    );

    assert_eq!(read_counter(&counter), 0, "argv must not run");
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
async fn forge_action_settings_proxy_reaches_subprocess_env() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let proxy = "http://forge.proxy.local:3128";
    boot.repo.settings_upsert("http_proxy", proxy).await?;
    let action = boot.temp_path("settings-proxy-env-action.sh");
    write_http_proxy_action(&action);

    let idem = "forge-settings-proxy-env";
    let (op_id, observer) = spawn_parked_observer(
        &boot,
        idem,
        payload(
            &boot,
            idem,
            vec![action.display().to_string()],
            boot.temp_path("settings-proxy-env-result.json"),
        ),
    )
    .await?;
    observer.await.expect("observer joins");
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{:?}",
        result.outcome
    );
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!(proxy));
    assert_eq!(event["head_sha"], json!("proxy-head"));
    Ok(())
}

#[tokio::test]
async fn forge_action_subprocess_env_passes_auth_and_strips_forbidden() -> CalmResult<()> {
    let _lock = FORGE_ENV_TEST_LOCK.lock().await;
    let _auth = EnvVarGuard::set("GH_TOKEN", "action-auth-token-e2e");
    let _forbidden = EnvVarGuard::set("FORGE_ACTION_FORBIDDEN_ENV_E2E", "must-not-pass");

    let boot = TestBoot::new().await;
    let action = boot.temp_path("action-env-auth.sh");
    write_env_json_script(&action, "GH_TOKEN", "FORGE_ACTION_FORBIDDEN_ENV_E2E");

    let idem = "forge-action-env-auth";
    let (op_id, observer) = spawn_parked_observer(
        &boot,
        idem,
        payload(
            &boot,
            idem,
            vec![action.display().to_string()],
            boot.temp_path("action-env-auth-result.json"),
        ),
    )
    .await?;
    observer.await.expect("observer joins");
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{:?}",
        result.outcome
    );

    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("action-auth-token-e2e"));
    assert_eq!(event["head_sha"], json!(""));
    Ok(())
}

#[tokio::test]
async fn forge_action_recovery_probe_env_passes_auth_and_strips_forbidden() -> CalmResult<()> {
    let _lock = FORGE_ENV_TEST_LOCK.lock().await;
    let _auth = EnvVarGuard::set("GH_TOKEN", "probe-auth-token-e2e");
    let _forbidden = EnvVarGuard::set("FORGE_PROBE_FORBIDDEN_ENV_E2E", "must-not-pass");

    let boot = TestBoot::new().await;
    let action = boot.temp_path("probe-env-action.sh");
    let counter = boot.temp_path("probe-env-counter");
    let sentinel = boot.temp_path("probe-env-sentinel");
    let finish = boot.temp_path("probe-env-finish");
    let verdict_probe = boot.temp_path("probe-env-verdict.sh");
    let output_probe = boot.temp_path("probe-env-output.sh");
    write_counter_action(&action);
    write_verdict_probe(&verdict_probe, 0, "landed");
    write_env_json_script(&output_probe, "GH_TOKEN", "FORGE_PROBE_FORBIDDEN_ENV_E2E");

    let idem = "forge-probe-env-auth";
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
            boot.temp_path("probe-env-result.json"),
            Some(vec![verdict_probe.display().to_string()]),
            Some(output_probe_argv(&output_probe)),
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
    assert_eq!(event["merge_sha"], json!("probe-auth-token-e2e"));
    assert_eq!(event["head_sha"], json!(""));
    assert_eq!(
        read_counter(&counter),
        0,
        "probe recovery must not re-run argv"
    );
    assert!(!sentinel.exists());
    Ok(())
}

#[test]
fn forge_action_auth_env_allowlist_passes_auth_and_strips_unknown() {
    let env: BTreeMap<_, _> = forge_passthrough_env_for_test(|key| match key {
        "GH_TOKEN" => Some("gh-token-from-test".into()),
        "GH_ENTERPRISE_TOKEN" => Some("gh-enterprise-token-from-test".into()),
        "GITHUB_ENTERPRISE_TOKEN" => Some("github-enterprise-token-from-test".into()),
        "FORGE_TEST_SECRET" => Some("must-not-pass".into()),
        _ => None,
    })
    .into_iter()
    .collect();

    assert_eq!(
        env.get("GH_TOKEN").map(String::as_str),
        Some("gh-token-from-test")
    );
    assert_eq!(
        env.get("GH_ENTERPRISE_TOKEN").map(String::as_str),
        Some("gh-enterprise-token-from-test")
    );
    assert_eq!(
        env.get("GITHUB_ENTERPRISE_TOKEN").map(String::as_str),
        Some("github-enterprise-token-from-test")
    );
    assert!(!env.contains_key("FORGE_TEST_SECRET"));
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
async fn forge_action_dead_parked_no_probe_recovers_from_result_files_once() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("dead-result-no-probe-action.sh");
    let counter = boot.temp_path("dead-result-no-probe-counter");
    let sentinel = boot.temp_path("dead-result-no-probe-sentinel");
    let finish = boot.temp_path("dead-result-no-probe-finish");
    let result_path = boot.temp_path("dead-result-no-probe-result.json");
    write_counter_action(&action);

    let idem = "forge-dead-result-no-probe";
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
            result_path.clone(),
        ),
        dead_artifacts(),
    )
    .await?;
    stage_result_files(
        &result_path,
        "0\n",
        r#"{"oid":"file-merge","headRefOid":"file-head"}"#,
    );

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(
        matches!(
            plan.items.as_slice(),
            [RecoveryItem::VerifyParked { op_id: item_op_id }] if item_op_id == &op_id
        ),
        "dead parked op should use recover_parked: {:?}",
        plan.items
    );
    boot.runtime.apply_recovery(plan).await?;
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{:?}",
        result.outcome
    );
    assert_eq!(forge_event_count(&boot.repo).await, 1);
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("file-merge"));
    assert_eq!(event["head_sha"], json!("file-head"));
    assert_eq!(
        read_counter(&counter),
        0,
        "result-file recovery must not re-run argv"
    );
    assert!(!sentinel.exists());
    Ok(())
}

#[tokio::test]
async fn forge_action_dead_parked_result_file_wins_over_probe() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("dead-result-wins-action.sh");
    let counter = boot.temp_path("dead-result-wins-counter");
    let sentinel = boot.temp_path("dead-result-wins-sentinel");
    let finish = boot.temp_path("dead-result-wins-finish");
    let result_path = boot.temp_path("dead-result-wins-result.json");
    let probe = boot.temp_path("dead-result-wins-probe.sh");
    let probe_counter = boot.temp_path("dead-result-wins-probe-counter");
    write_counter_action(&action);
    write_counting_probe(&probe, "probe-merge", "probe-head", 0);

    let idem = "forge-dead-result-wins-probe";
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
            result_path.clone(),
            Some(vec![
                probe.display().to_string(),
                probe_counter.display().to_string(),
            ]),
            Some(vec![
                probe.display().to_string(),
                probe_counter.display().to_string(),
                "--json".into(),
            ]),
        ),
        dead_artifacts(),
    )
    .await?;
    stage_result_files(
        &result_path,
        "0\n",
        r#"{"oid":"file-merge","headRefOid":"file-head"}"#,
    );

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(
        matches!(
            plan.items.as_slice(),
            [RecoveryItem::VerifyParked { op_id: item_op_id }] if item_op_id == &op_id
        ),
        "dead parked op should use recover_parked: {:?}",
        plan.items
    );
    boot.runtime.apply_recovery(plan).await?;
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{:?}",
        result.outcome
    );
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("file-merge"));
    assert_eq!(event["head_sha"], json!("file-head"));
    assert_eq!(forge_event_count(&boot.repo).await, 1);
    assert_eq!(read_counter(&probe_counter), 0, "probe must not run");
    assert_eq!(
        read_counter(&counter),
        0,
        "result-file recovery must not re-run argv"
    );
    assert!(!sentinel.exists());
    Ok(())
}

#[tokio::test]
async fn forge_action_dead_parked_result_file_nonzero_fails_action_failed() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("dead-result-nonzero-action.sh");
    let counter = boot.temp_path("dead-result-nonzero-counter");
    let sentinel = boot.temp_path("dead-result-nonzero-sentinel");
    let finish = boot.temp_path("dead-result-nonzero-finish");
    let result_path = boot.temp_path("dead-result-nonzero-result.json");
    write_counter_action(&action);

    let idem = "forge-dead-result-nonzero";
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
            result_path.clone(),
        ),
        dead_artifacts(),
    )
    .await?;
    stage_result_files(
        &result_path,
        "42\n",
        r#"{"oid":"file-merge","headRefOid":"file-head"}"#,
    );

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(matches!(
        plan.items.as_slice(),
        [RecoveryItem::VerifyParked { .. }]
    ));
    boot.runtime.apply_recovery(plan).await?;
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(
            result.outcome,
            OperationOutcome::Failed {
                ref last_error,
                from_phase: calm_server::operation::PhaseTag::Parked,
                last_error_class: Some(ref class),
            } if last_error == "forge action exited with code 42" && class == "action-failed"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    assert_eq!(
        read_counter(&counter),
        0,
        "result-file recovery must not re-run argv"
    );
    assert!(!sentinel.exists());
    Ok(())
}

#[tokio::test]
async fn forge_action_dead_parked_unparseable_result_code_falls_back_to_probe_or_parked_dead()
-> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("bad-code-probe-action.sh");
    let counter = boot.temp_path("bad-code-probe-counter");
    let sentinel = boot.temp_path("bad-code-probe-sentinel");
    let finish = boot.temp_path("bad-code-probe-finish");
    let result_path = boot.temp_path("bad-code-probe-result.json");
    let probe = boot.temp_path("bad-code-probe.sh");
    write_counter_action(&action);
    write_probe(&probe, "bad-code-probe-merge", "bad-code-probe-head", 0);

    let idem = "forge-bad-code-probe";
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
            result_path.clone(),
            Some(vec![probe.display().to_string()]),
            Some(output_probe_argv(&probe)),
        ),
        dead_artifacts(),
    )
    .await?;
    stage_result_code(&result_path, "\n");

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(matches!(
        plan.items.as_slice(),
        [RecoveryItem::VerifyParked { .. }]
    ));
    boot.runtime.apply_recovery(plan).await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("bad-code-probe-merge"));
    assert_eq!(event["head_sha"], json!("bad-code-probe-head"));
    assert_eq!(read_counter(&counter), 0);
    assert!(!sentinel.exists());

    let boot = TestBoot::new().await;
    let action = boot.temp_path("bad-code-no-probe-action.sh");
    let counter = boot.temp_path("bad-code-no-probe-counter");
    let sentinel = boot.temp_path("bad-code-no-probe-sentinel");
    let finish = boot.temp_path("bad-code-no-probe-finish");
    let result_path = boot.temp_path("bad-code-no-probe-result.json");
    write_counter_action(&action);

    let idem = "forge-bad-code-no-probe";
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
            result_path.clone(),
        ),
        dead_artifacts(),
    )
    .await?;
    stage_result_code(&result_path, "\n");

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
            } if last_error == "forge action process dead with no probe; gate-infra"
                && class == "parked_dead"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(read_counter(&counter), 0);
    assert!(!sentinel.exists());
    assert_eq!(forge_event_count(&boot.repo).await, 0);
    Ok(())
}

#[tokio::test]
async fn forge_action_dead_parked_missing_stdout_falls_back_to_output_probe() -> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("missing-stdout-action.sh");
    let counter = boot.temp_path("missing-stdout-counter");
    let sentinel = boot.temp_path("missing-stdout-sentinel");
    let finish = boot.temp_path("missing-stdout-finish");
    let result_path = boot.temp_path("missing-stdout-result.json");
    let probe = boot.temp_path("missing-stdout-probe.sh");
    write_counter_action(&action);
    write_probe(
        &probe,
        "missing-stdout-probe-merge",
        "missing-stdout-probe-head",
        0,
    );

    let idem = "forge-missing-stdout-probe";
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
            result_path.clone(),
            Some(vec![probe.display().to_string()]),
            Some(output_probe_argv(&probe)),
        ),
        dead_artifacts(),
    )
    .await?;
    stage_result_code(&result_path, "0\n");

    let plan = boot.runtime.recover_on_boot().await?;
    assert!(matches!(
        plan.items.as_slice(),
        [RecoveryItem::VerifyParked { .. }]
    ));
    boot.runtime.apply_recovery(plan).await?;
    let result = boot.runtime.wait(&op_id).await?;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));

    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("missing-stdout-probe-merge"));
    assert_eq!(event["head_sha"], json!("missing-stdout-probe-head"));
    assert_eq!(read_counter(&counter), 0);
    assert!(!sentinel.exists());
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
            Some(output_probe_argv(&probe)),
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
async fn forge_action_boot_live_reattach_completes_from_result_files_without_probe()
-> CalmResult<()> {
    let boot = TestBoot::new().await;
    let action = boot.temp_path("live-reattach-result-action.sh");
    let counter = boot.temp_path("live-reattach-result-counter");
    let sentinel = boot.temp_path("live-reattach-result-sentinel");
    let finish = boot.temp_path("live-reattach-result-finish");
    let result_path = boot.temp_path("live-reattach-result.json");
    let probe = boot.temp_path("live-reattach-result-probe.sh");
    let probe_counter = boot.temp_path("live-reattach-result-probe-counter");
    write_counter_action(&action);
    write_counting_probe(&probe, "probe-must-not-win", "probe-must-not-win-head", 0);
    let (mut child, artifacts) =
        spawn_live_counter_action(&action, &counter, &sentinel, &finish).await;
    wait_for_file(&sentinel).await;
    assert_eq!(read_counter(&counter), 1);
    let artifacts_for_kill = artifacts.clone();

    let idem = "forge-live-reattach-result-files";
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
            result_path.clone(),
            Some(vec![
                probe.display().to_string(),
                probe_counter.display().to_string(),
            ]),
            Some(vec![
                probe.display().to_string(),
                probe_counter.display().to_string(),
                "--json".into(),
            ]),
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

    stage_result_files(
        &result_path,
        "0\n",
        r#"{"oid":"reattach-file-merge","headRefOid":"reattach-file-head"}"#,
    );
    assert!(signal_process_group(artifacts_for_kill.pgid, libc::SIGKILL));
    let _ = child.wait().await.expect("wait killed live fake action");
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Succeeded { .. }),
        "{:?}",
        result.outcome
    );

    let event = latest_forge_event_payload(&boot.repo).await;
    assert_eq!(event["merge_sha"], json!("reattach-file-merge"));
    assert_eq!(event["head_sha"], json!("reattach-file-head"));
    assert_eq!(read_counter(&probe_counter), 0, "probe must not run");
    assert_eq!(
        read_counter(&counter),
        1,
        "live reattach must not re-run argv"
    );
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
            Some(output_probe_argv(&probe)),
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
    let action = boot.temp_path("live-not-landed-action.sh");
    let counter = boot.temp_path("live-not-landed-counter");
    let sentinel = boot.temp_path("live-not-landed-sentinel");
    let finish = boot.temp_path("live-not-landed-finish");
    let probe = boot.temp_path("live-not-landed-probe.sh");
    write_counter_action(&action);
    write_probe(&probe, "ignored-merge", "ignored-head", 1);
    let (mut child, artifacts) =
        spawn_live_counter_action(&action, &counter, &sentinel, &finish).await;
    wait_for_file(&sentinel).await;
    assert_eq!(read_counter(&counter), 1);

    let idem = "forge-live-reattach-not-landed";
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
            boot.temp_path("live-not-landed-result.json"),
            Some(vec![probe.display().to_string()]),
            Some(output_probe_argv(&probe)),
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
        "live not-landed parked op should use recover_parked: {:?}",
        plan.items
    );
    boot.runtime.apply_recovery(plan).await?;
    assert_eq!(phase(&boot.repo, &op_id).await, "parked");
    fs::write(&finish, "").expect("release live not-landed fake action");
    let status = child
        .wait()
        .await
        .expect("wait live not-landed fake action");
    assert!(status.success());
    let result = wait_for_operation_result(&boot, &op_id).await;
    assert!(
        matches!(
            result.outcome,
            OperationOutcome::Failed {
                ref last_error,
                from_phase: calm_server::operation::PhaseTag::Parked,
                last_error_class: Some(ref class),
            } if last_error == "forge action process dead and probe reports not landed"
                && class == "action-not-landed"
        ),
        "{:?}",
        result.outcome
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "live not-landed reattach must not re-run argv"
    );
    assert_eq!(forge_event_count(&boot.repo).await, 0);

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
            } if last_error == "gate-infra: forge action process dead; no probe to resolve outcome"
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
            Some(output_probe_argv(&probe)),
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
    let verdict_probe = boot.temp_path("probe-reextract-verdict.sh");
    let output_probe = boot.temp_path("probe-reextract-output.sh");
    write_counter_action(&action);
    write_verdict_probe(&verdict_probe, 0, "true");
    write_probe(&output_probe, "output-probe-sha", "output-probe-head", 0);

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
            Some(vec![
                verdict_probe.display().to_string(),
                "--json".into(),
                "state".into(),
                "-q".into(),
                ".state==\"MERGED\"".into(),
            ]),
            Some(output_probe_argv(&output_probe)),
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
    assert_eq!(event["merge_sha"], json!("output-probe-sha"));
    assert_eq!(event["head_sha"], json!("output-probe-head"));
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
            Some(output_probe_argv(&probe)),
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
