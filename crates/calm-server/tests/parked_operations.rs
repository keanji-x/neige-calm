#![cfg(unix)]

use std::collections::VecDeque;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Arc;

use async_trait::async_trait;
use calm_server::db::RouteRepo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::error::Result as CalmResult;
use calm_server::event::EventBus;
use calm_server::model::{new_id, now_ms};
use calm_server::operation::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation,
    OperationCompletionBus, OperationKey, OperationOutcome, OperationRepo, OperationRuntime,
    ParkedCompletion, ParkedOutcome, ParkedRecovery, Phase, PhaseTag, ProviderAdapter,
    RecoveryMode, SpawnArtifacts, SpawnCtx, SpawnHandle, SpawnOutcome, SqlxOperationRepo, Tx,
    TxOutput, complete_parked_for_test,
};
use calm_server::proc_identity::{
    read_boot_id, read_proc_start_time, signal_process_group, verify_owned_pid,
};
use calm_server::state::DaemonClient;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[tokio::test]
async fn fake_parking_adapter_wait_returns_spliced_success_after_post_park_observer() {
    let pre_complete_phase = Arc::new(Mutex::new(None));
    let boot = TestBoot::new(vec![Arc::new(ObserverParkingAdapter {
        verdict: ObserverVerdict::Succeeded(json!({ "observer": "success" })),
        pre_complete_phase: pre_complete_phase.clone(),
    })])
    .await;

    let op_id = boot
        .runtime
        .submit("park-observer", operation_key(), json!({}))
        .await
        .unwrap();
    let result = boot.runtime.wait(&op_id).await.unwrap();

    assert!(matches!(
        result.outcome,
        OperationOutcome::Succeeded { ref result }
            if result == &json!({ "observer": "success" })
    ));
    assert_eq!(
        pre_complete_phase.lock().await.as_deref(),
        Some("parked"),
        "observer first completion attempt must run after set_parked commits"
    );
}

#[tokio::test]
async fn fake_parking_adapter_wait_returns_failure_from_parked() {
    let boot = TestBoot::new(vec![Arc::new(ObserverParkingAdapter {
        verdict: ObserverVerdict::Failed("child failed".into()),
        pre_complete_phase: Arc::new(Mutex::new(None)),
    })])
    .await;

    let op_id = boot
        .runtime
        .submit("park-observer", operation_key(), json!({}))
        .await
        .unwrap();
    let result = boot.runtime.wait(&op_id).await.unwrap();

    assert!(matches!(
        result.outcome,
        OperationOutcome::Failed {
            ref last_error,
            from_phase: PhaseTag::Parked,
            last_error_class: Some(ref class),
        } if last_error == "child failed" && class == "observer"
    ));
}

#[tokio::test]
async fn boot_recovery_live_default_leaves_parked() {
    let boot = TestBoot::new(vec![Arc::new(DefaultParkedAdapter)]).await;
    let (mut child, artifacts) = spawn_sleep_child(5);
    let op = boot
        .insert_parked("park-default", artifacts.clone(), now_ms() + 10_000)
        .await;

    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();

    let stored = boot
        .operation_repo
        .get_operation(&op.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.phase, Phase::Parked);
    assert!(verify_owned_pid(
        artifacts.pid,
        artifacts.start_time,
        &artifacts.boot_id
    ));
    signal_process_group(artifacts.pgid, libc::SIGKILL);
    let _ = child.wait();
}

#[tokio::test]
async fn boot_recovery_dead_default_fails_parked_dead() {
    let boot = TestBoot::new(vec![Arc::new(DefaultParkedAdapter)]).await;
    let op = boot
        .insert_parked("park-default", dead_artifacts(), now_ms() + 10_000)
        .await;

    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();

    let result = boot
        .operation_repo
        .operation_result(&op.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Failed {
            from_phase: PhaseTag::Parked,
            last_error_class: Some(ref class),
            ..
        } if class == "parked_dead"
    ));
}

#[tokio::test]
async fn boot_recovery_live_complete_kills_group_and_succeeds() {
    let adapter = Arc::new(QueuedRecoveryAdapter::new(vec![ParkedRecovery::Complete(
        ParkedOutcome::Succeeded {
            result: json!({ "boot": "recovered" }),
        },
    )]));
    let boot = TestBoot::new(vec![adapter]).await;
    let (mut child, artifacts) = spawn_sleep_child(5);
    let op = boot
        .insert_parked("park-recovery", artifacts.clone(), now_ms() + 10_000)
        .await;

    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();
    let _ = child.wait();

    assert!(!verify_owned_pid(
        artifacts.pid,
        artifacts.start_time,
        &artifacts.boot_id
    ));
    let result = boot
        .operation_repo
        .operation_result(&op.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Succeeded { ref result }
            if result == &json!({ "boot": "recovered" })
    ));
}

#[tokio::test]
async fn sweep_past_deadline_live_default_kills_and_fails_deadline() {
    let boot = TestBoot::new(vec![Arc::new(DefaultParkedAdapter)]).await;
    let (mut child, artifacts) = spawn_sleep_child(5);
    let op = boot
        .insert_parked("park-default", artifacts.clone(), now_ms() - 1)
        .await;

    boot.runtime.sweep_parked().await.unwrap();
    let _ = child.wait();

    assert!(!verify_owned_pid(
        artifacts.pid,
        artifacts.start_time,
        &artifacts.boot_id
    ));
    let result = boot
        .operation_repo
        .operation_result(&op.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Failed {
            from_phase: PhaseTag::Parked,
            last_error_class: Some(ref class),
            ..
        } if class == "parked_deadline"
    ));
}

#[tokio::test]
async fn sweep_deadline_uses_recoverable_verdict_instead_of_deadline_failure() {
    let adapter = Arc::new(QueuedRecoveryAdapter::new(vec![ParkedRecovery::Complete(
        ParkedOutcome::Succeeded {
            result: json!({ "recovered": true }),
        },
    )]));
    let boot = TestBoot::new(vec![adapter]).await;
    let op = boot
        .insert_parked("park-recovery", dead_artifacts(), now_ms() - 1)
        .await;

    boot.runtime.sweep_parked().await.unwrap();

    let result = boot
        .operation_repo
        .operation_result(&op.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Succeeded { ref result }
            if result == &json!({ "recovered": true })
    ));
}

#[tokio::test]
async fn sweep_pre_deadline_dead_probe_completes_only_on_recovered_verdict() {
    let adapter = Arc::new(QueuedRecoveryAdapter::new(vec![
        ParkedRecovery::Complete(ParkedOutcome::Succeeded {
            result: json!({ "first": "complete" }),
        }),
        ParkedRecovery::Fail {
            reason: "ignored before deadline".into(),
        },
    ]));
    let boot = TestBoot::new(vec![adapter]).await;
    let first = boot
        .insert_parked("park-recovery", dead_artifacts(), now_ms() + 10_000)
        .await;
    let second = boot
        .insert_parked("park-recovery", dead_artifacts(), now_ms() + 10_000)
        .await;

    boot.runtime.sweep_parked().await.unwrap();

    let first_result = boot
        .operation_repo
        .operation_result(&first.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first_result.outcome,
        OperationOutcome::Succeeded { ref result }
            if result == &json!({ "first": "complete" })
    ));
    let second_stored = boot
        .operation_repo
        .get_operation(&second.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_stored.phase, Phase::Parked);
}

#[tokio::test]
async fn sweep_post_kill_recheck_can_recover_verdict() {
    let adapter = Arc::new(QueuedRecoveryAdapter::new(vec![
        ParkedRecovery::Fail {
            reason: "first pass fail".into(),
        },
        ParkedRecovery::Complete(ParkedOutcome::Succeeded {
            result: json!({ "post_kill": "verdict" }),
        }),
    ]));
    let boot = TestBoot::new(vec![adapter]).await;
    let (mut child, artifacts) = spawn_sleep_child(5);
    let op = boot
        .insert_parked("park-recovery", artifacts, now_ms() - 1)
        .await;

    boot.runtime.sweep_parked().await.unwrap();
    let _ = child.wait();

    let result = boot
        .operation_repo
        .operation_result(&op.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.outcome,
        OperationOutcome::Succeeded { ref result }
            if result == &json!({ "post_kill": "verdict" })
    ));
}

#[tokio::test]
async fn recovery_mode_plumbing_distinguishes_boot_probe_and_past_deadline() {
    let adapter = Arc::new(QueuedRecoveryAdapter::new(vec![
        ParkedRecovery::LeaveParked,
        ParkedRecovery::LeaveParked,
        ParkedRecovery::Fail {
            reason: "deadline".into(),
        },
    ]));
    let boot = TestBoot::new(vec![adapter.clone()]).await;
    let (mut child, live_artifacts) = spawn_sleep_child(5);
    let live = boot
        .insert_parked("park-recovery", live_artifacts.clone(), now_ms() + 10_000)
        .await;
    let _pre_deadline_dead = boot
        .insert_parked("park-recovery", dead_artifacts(), now_ms() + 10_000)
        .await;
    let past_deadline = boot
        .insert_parked("park-recovery", dead_artifacts(), now_ms() - 1)
        .await;

    let plan = boot.runtime.recover_on_boot().await.unwrap();
    boot.runtime.apply_recovery(plan).await.unwrap();

    let modes = adapter.modes.lock().await.clone();
    assert!(modes.contains(&RecoveryMode::Boot));
    assert!(modes.contains(&RecoveryMode::PreDeadlineProbe));
    assert!(modes.contains(&RecoveryMode::PastDeadline));

    let live_stored = boot
        .operation_repo
        .get_operation(&live.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live_stored.phase, Phase::Parked);
    let past = boot
        .operation_repo
        .operation_result(&past_deadline.id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(past.outcome, OperationOutcome::Failed { .. }));
    signal_process_group(live_artifacts.pgid, libc::SIGKILL);
    let _ = child.wait();

    let expired_adapter = Arc::new(QueuedRecoveryAdapter::new(vec![ParkedRecovery::Fail {
        reason: "expired".into(),
    }]));
    let expired_boot = TestBoot::new(vec![expired_adapter.clone()]).await;
    expired_boot
        .insert_parked("park-recovery", dead_artifacts(), now_ms() - 1)
        .await;
    let plan = expired_boot.runtime.recover_on_boot().await.unwrap();
    expired_boot.runtime.apply_recovery(plan).await.unwrap();
    assert_eq!(
        expired_adapter.modes.lock().await.as_slice(),
        &[RecoveryMode::PastDeadline],
        "expired-at-boot op must use PastDeadline, not Boot"
    );
}

struct TestBoot {
    runtime: OperationRuntime,
    operation_repo: Arc<SqlxOperationRepo>,
    route_repo: Arc<SqlxRepo>,
}

impl TestBoot {
    async fn new(adapters: Vec<Arc<dyn ProviderAdapter>>) -> Self {
        let route_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let operation_repo = Arc::new(SqlxOperationRepo::new(route_repo.pool().clone()));
        let events = EventBus::new();
        let completion = OperationCompletionBus::new();
        let route_dyn: Arc<dyn RouteRepo> = route_repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_dyn.clone());
        let runtime = OperationRuntime::new_unchecked(
            operation_repo.clone(),
            adapters,
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                route_dyn,
                operation_repo.clone(),
                Arc::new(DaemonClient::new_stub()),
                terminal_renderer,
                events,
                completion,
            ),
        );
        Self {
            runtime,
            operation_repo,
            route_repo,
        }
    }

    async fn insert_parked(
        &self,
        kind: &str,
        artifacts: SpawnArtifacts,
        deadline_ms: i64,
    ) -> Operation {
        let op_id = self
            .operation_repo
            .insert_operation(kind, operation_key(), json!({}))
            .await
            .unwrap();
        let mut claimed = self.operation_repo.claim_drive_batch(1).await.unwrap();
        assert_eq!(claimed.len(), 1);
        let op = claimed.pop().unwrap();
        assert!(op.lease_owner.is_some());
        let output = TxOutput::new("unknown", None, json!({ "prepared": true }));
        sqlx::query(
            r#"UPDATE operations
               SET phase = 'spawn_started',
                   tx_output_json = ?1,
                   target_json = '{"type":"unknown","id":null}'
               WHERE id = ?2"#,
        )
        .bind(serde_json::to_string(&output).unwrap())
        .bind(&op_id)
        .execute(self.route_repo.pool())
        .await
        .unwrap();
        let op = self
            .operation_repo
            .get_operation(&op_id)
            .await
            .unwrap()
            .unwrap();
        self.operation_repo
            .record_spawn_artifacts(&op, &artifacts)
            .await
            .unwrap();
        self.operation_repo
            .set_parked(&op, deadline_ms)
            .await
            .unwrap()
            .unwrap()
    }
}

#[derive(Clone)]
struct DefaultParkedAdapter;

#[async_trait]
impl ProviderAdapter for DefaultParkedAdapter {
    fn kind(&self) -> &'static str {
        "park-default"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        PARK_PHASES
    }

    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        Ok(TxOutput::new("unknown", None, Value::Null))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: Vec::new(),
        })
    }

    async fn compensate_step(
        &self,
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Ok(())
    }
}

struct ObserverParkingAdapter {
    verdict: ObserverVerdict,
    pre_complete_phase: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ProviderAdapter for ObserverParkingAdapter {
    fn kind(&self) -> &'static str {
        "park-observer"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        PARK_PHASES
    }

    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        Ok(TxOutput::new("unknown", None, json!({ "prepared": true })))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        let (child, artifacts) = spawn_short_child();
        ctx.record_spawn_artifacts(op, &artifacts).await?;

        let pool = ctx.operation_repo.sqlite_pool();
        let completion = ctx.completion.clone();
        let op_id = op.id.clone();
        let verdict = self.verdict.clone();
        let pre_complete_phase = self.pre_complete_phase.clone();
        Ok(SpawnOutcome::Parked {
            deadline_ms: now_ms() + 10_000,
            observer: Box::pin(async move {
                let phase: String =
                    sqlx::query_scalar("SELECT phase FROM operations WHERE id = ?1")
                        .bind(&op_id)
                        .fetch_one(&pool)
                        .await
                        .expect("observer reads phase");
                *pre_complete_phase.lock().await = Some(phase);
                let _ = tokio::task::spawn_blocking(move || {
                    let mut child = child;
                    child.wait()
                })
                .await;
                let outcome = match verdict {
                    ObserverVerdict::Succeeded(result) => ParkedOutcome::Succeeded { result },
                    ObserverVerdict::Failed(reason) => ParkedOutcome::Failed {
                        last_error: reason,
                        last_error_class: Some("observer".into()),
                    },
                };
                match complete_parked_for_test(&pool, &op_id, &outcome)
                    .await
                    .expect("complete parked")
                {
                    ParkedCompletion::Completed(result) => completion.complete(result),
                    ParkedCompletion::AlreadyResolved { .. } => {}
                }
            }),
        })
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: Vec::new(),
        })
    }

    async fn compensate_step(
        &self,
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Ok(())
    }
}

struct QueuedRecoveryAdapter {
    recoveries: Mutex<VecDeque<ParkedRecovery>>,
    modes: Mutex<Vec<RecoveryMode>>,
}

impl QueuedRecoveryAdapter {
    fn new(recoveries: Vec<ParkedRecovery>) -> Self {
        Self {
            recoveries: Mutex::new(VecDeque::from(recoveries)),
            modes: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ProviderAdapter for QueuedRecoveryAdapter {
    fn kind(&self) -> &'static str {
        "park-recovery"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        PARK_PHASES
    }

    async fn validate(&self, _input: &Value) -> CalmResult<()> {
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        _input: &Value,
        _op: &Operation,
    ) -> CalmResult<TxOutput> {
        Ok(TxOutput::new("unknown", None, Value::Null))
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn recover_parked(
        &self,
        _op: &Operation,
        _artifacts: &SpawnArtifacts,
        _alive: bool,
        mode: RecoveryMode,
        _ctx: &SpawnCtx,
    ) -> CalmResult<ParkedRecovery> {
        self.modes.lock().await.push(mode);
        Ok(self
            .recoveries
            .lock()
            .await
            .pop_front()
            .unwrap_or(ParkedRecovery::LeaveParked))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> CalmResult<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: Vec::new(),
        })
    }

    async fn compensate_step(
        &self,
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> CalmResult<()> {
        Ok(())
    }
}

const PARK_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::Parked,
    PhaseTag::Compensating,
    PhaseTag::Failed,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
enum ObserverVerdict {
    Succeeded(Value),
    Failed(String),
}

fn operation_key() -> OperationKey {
    OperationKey {
        operation_key: new_id(),
        idempotency_key: None,
        payload_hash: "hash".into(),
    }
}

fn spawn_short_child() -> (Child, SpawnArtifacts) {
    spawn_child("sleep 0.05; exit 0")
}

fn spawn_sleep_child(seconds: u64) -> (Child, SpawnArtifacts) {
    spawn_child(&format!("sleep {seconds}"))
}

fn spawn_child(script: &str) -> (Child, SpawnArtifacts) {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script).process_group(0);
    let child = command.spawn().expect("spawn child");
    let pid = child.id() as i32;
    let start_time = read_proc_start_time(pid).expect("child start time");
    let boot_id = read_boot_id().expect("boot id");
    (
        child,
        SpawnArtifacts {
            pid,
            pgid: pid,
            start_time,
            boot_id,
            log_path: None,
            extra: Value::Null,
        },
    )
}

fn dead_artifacts() -> SpawnArtifacts {
    SpawnArtifacts {
        pid: 999_999,
        pgid: 999_999,
        start_time: 1,
        boot_id: read_boot_id().unwrap_or_else(|| "boot".into()),
        log_path: None,
        extra: Value::Null,
    }
}
