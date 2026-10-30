//! One retained driver; END also requires the exact authoritative receipt.
use super::*;
use crate::db::sqlite::{AcpCanonicalInput, AcpSubmissionInput, AcpSubmissionState};
use crate::harness::backend::ExecutionEndTarget;
use crate::harness::retained_join::RetainedJoin;

#[derive(Clone, Copy)]
pub(super) enum Settlement {
    Completed,
    /// Unknown delivery, with confirmed stop/revoke; only a normal join observes it.
    CleanUnknown,
    Unconfirmed,
}
pub(super) enum Association {
    Historical,
    Canonical(AcpCanonicalInput),
}
enum DriverInput {
    Cleanup,
    Historical,
    Canonical(Box<CanonicalDriver>),
    Maintenance {
        target: crate::harness::backend::MaintenanceTarget,
        turn: String,
        native: String,
    },
}
struct CanonicalDriver {
    receipt: AcpSubmission,
    association: AcpCanonicalInput,
}
enum Completion {
    Driver(RetainedJoin<Settlement>),
    /// A durable Completed receipt plus confirmed recovery cleanup, established
    /// under the retained installation reservation. No synthetic task/join.
    RecoveredCompleted,
    /// A genuine prior normal join, loaded only by a new authenticated Keep.
    RecoveredUnknown,
}
impl Completion {
    fn is_finished(&self) -> bool {
        match self {
            Self::Driver(driver) => driver.is_finished(),
            Self::RecoveredCompleted | Self::RecoveredUnknown => true,
        }
    }
    async fn finish(&mut self) -> Result<Settlement> {
        match self {
            Self::Driver(driver) => driver.finish().await.copied().map_err(|()| {
                CalmError::Conflict("ACP receipt writer did not finish normally".into())
            }),
            Self::RecoveredCompleted => Ok(Settlement::Completed),
            Self::RecoveredUnknown => Ok(Settlement::CleanUnknown),
        }
    }
}
pub(super) struct DriverSlot {
    execution_id: Option<workspace_runtime::execution::ExecutionId>,
    completion: Completion,
    input: DriverInput,
}
impl DriverSlot {
    pub(super) fn with_execution(mut self, id: workspace_runtime::execution::ExecutionId) -> Self {
        self.execution_id = Some(id);
        self
    }
    pub(super) fn maintenance(
        driver: tokio::task::JoinHandle<Settlement>,
        target: crate::harness::backend::MaintenanceTarget,
        turn: String,
        native: String,
    ) -> Self {
        Self {
            execution_id: None,
            completion: Completion::Driver(RetainedJoin::new(driver)),
            input: DriverInput::Maintenance {
                target,
                turn,
                native,
            },
        }
    }

    pub(super) fn cleanup(driver: tokio::task::JoinHandle<Settlement>) -> Self {
        Self {
            execution_id: None,
            completion: Completion::Driver(RetainedJoin::new(driver)),
            input: DriverInput::Cleanup,
        }
    }
    pub(super) fn new(
        driver: tokio::task::JoinHandle<Settlement>,
        receipt: AcpSubmission,
        association: Association,
    ) -> Self {
        let input = match association {
            Association::Historical => DriverInput::Historical,
            Association::Canonical(association) => {
                DriverInput::Canonical(Box::new(CanonicalDriver {
                    receipt,
                    association,
                }))
            }
        };
        Self {
            execution_id: None,
            completion: Completion::Driver(RetainedJoin::new(driver)),
            input,
        }
    }
    pub(super) fn recovered(receipt: AcpSubmission, association: AcpCanonicalInput) -> Self {
        Self {
            execution_id: None,
            completion: Completion::RecoveredCompleted,
            input: DriverInput::Canonical(Box::new(CanonicalDriver {
                receipt,
                association,
            })),
        }
    }
    pub(super) fn from_normal_join(receipt: AcpSubmission, association: AcpCanonicalInput) -> Self {
        Self {
            execution_id: None,
            completion: Completion::RecoveredUnknown,
            input: DriverInput::Canonical(Box::new(CanonicalDriver {
                receipt,
                association,
            })),
        }
    }
    fn target(&self, params: &SessionParams) -> Option<ExecutionEndTarget> {
        let DriverInput::Canonical(input) = &self.input else {
            return None;
        };
        Some(ExecutionEndTarget {
            card_id: params.card_id.clone(),
            track_id: params.track_id.clone(),
            worker_session_id: params.worker_session_id.clone(),
            logical_turn_id: input.association.logical_turn_id.clone(),
            proof: input.association.proof.clone(),
            scope: crate::harness::backend::ExecutionEndScope::ReceiptProcess {
                thread_id: uuid::Uuid::parse_str(&input.receipt.thread_id).ok()?,
                local_turn_id: uuid::Uuid::parse_str(&input.receipt.turn_id).ok()?,
                native_session_id: input.receipt.native_session_id.clone(),
            },
        })
    }
    fn matches(&self, target: &ExecutionEndTarget) -> bool {
        let DriverInput::Canonical(input) = &self.input else {
            return false;
        };
        let receipt = &input.receipt;
        let association = &input.association;
        let (thread, turn) = target.scope.thread_and_turn();
        match &target.scope {
            crate::harness::backend::ExecutionEndScope::ReceiptProcess {
                native_session_id,
                ..
            } => {
                receipt.thread_id == thread
                    && receipt.turn_id == turn
                    && receipt.native_session_id == *native_session_id
                    && association.logical_turn_id == target.logical_turn_id
                    && association.proof == target.proof
            }
            crate::harness::backend::ExecutionEndScope::LocalProcess { .. }
            | crate::harness::backend::ExecutionEndScope::NativeTurn { .. } => false,
        }
    }
    async fn finish(&mut self) -> Result<Settlement> {
        self.completion.finish().await
    }
}
fn slot(shared: &Shared) -> Option<Arc<tokio::sync::Mutex<DriverSlot>>> {
    shared.driver.lock().expect("ACP driver").clone()
}
/// Acquisition holds `issue`; recheck holds the park that excludes new issuance.
/// Release `state` before calling, and release the driver pointer lock in `slot`
/// before inspecting the slot. A join holds the slot while waiting for a task
/// that may take `state` or the SQL writer: never wait for that slot here.
pub(super) fn driver_finished(shared: &Shared) -> bool {
    let Some(driver) = slot(shared) else {
        return true;
    };
    match driver.try_lock() {
        Ok(driver) => driver.completion.is_finished(),
        Err(_) => false,
    }
}
pub(super) fn install(shared: &Shared, driver: DriverSlot) {
    *shared.driver.lock().expect("ACP driver") = Some(Arc::new(tokio::sync::Mutex::new(driver)));
}
pub(super) async fn join_driver(shared: &Shared) -> Result<()> {
    if let Some(driver) = slot(shared) {
        let mut driver = driver.lock().await;
        let observed = driver.finish().await?;
        if matches!(observed, Settlement::CleanUnknown)
            && let DriverInput::Canonical(input) = &driver.input
        {
            super::normal_join::record(&shared.params, &input.receipt).await?;
        }
    }
    Ok(())
}
pub(super) async fn maintenance_attempted(
    shared: &Shared,
    expected: &crate::harness::backend::MaintenanceTarget,
) -> bool {
    let Some(driver) = slot(shared) else {
        return false;
    };
    matches!(&driver.lock().await.input, DriverInput::Maintenance { target, .. } if target.operation_id == expected.operation_id)
}
pub(super) async fn observe_maintenance(
    shared: &Shared,
    expected: &crate::harness::backend::MaintenanceTarget,
    current_native: Option<&str>,
) -> crate::harness::backend::MaintenanceObservation {
    use crate::harness::backend::MaintenanceObservation;
    let Some(driver) = slot(shared) else {
        return MaintenanceObservation::Unconfirmed;
    };
    let mut driver = driver.lock().await;
    let turn_id = match &driver.input {
        DriverInput::Maintenance {
            target,
            turn,
            native,
        } if target == expected && current_native == Some(native.as_str()) => turn.clone(),
        _ => return MaintenanceObservation::Unconfirmed,
    };
    let settled = if driver.completion.is_finished() {
        Some(matches!(driver.finish().await, Ok(Settlement::Completed)))
    } else {
        None
    };
    MaintenanceObservation::Turn { turn_id, settled }
}
async fn current_receipt_owner(params: &SessionParams, expected: &AcpSubmission) -> Result<bool> {
    let (card, worker, track, provider, thread, native) = (
        params.card_id.clone(),
        params.worker_session_id.clone(),
        params.track_id.clone(),
        params.provider.clone(),
        expected.thread_id.clone(),
        expected.native_session_id.clone(),
    );
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier =
                crate::db::sqlite::conversation::current_input_fact_carrier_tx(tx, &card, &worker)
                    .await?;
            let (binding, row) =
                crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
            Ok(binding.provider == provider
                && row.track_id.as_str() == track
                && row.thread_id.as_deref() == Some(thread.as_str())
                && row.agent_session_id.as_deref() == Some(native.as_str()))
        })
    })
    .await
}
/// Only the exact retained driver can produce this fact; a recovered Completed
/// slot and a process sweep cannot manufacture a normal Unknown driver join.
async fn settle_kept_under_issue(
    shared: &Shared,
) -> Result<Option<crate::db::sqlite::conversation::RuntimeFrameAfterWrite>> {
    {
        let state = shared.state.lock().expect("ACP state");
        if state.closed || state.active.is_some() {
            return Ok(None);
        }
    }
    let Some(driver) = slot(shared) else {
        return Ok(None);
    };
    let receipt = {
        let mut driver = driver.lock().await;
        if !driver.completion.is_finished() {
            return Ok(None);
        }
        match driver.finish().await? {
            Settlement::CleanUnknown => {}
            Settlement::Completed | Settlement::Unconfirmed => return Ok(None),
        }
        let DriverInput::Canonical(input) = &driver.input else {
            return Ok(None);
        };
        input.receipt.clone()
    };
    let params = &shared.params;
    super::normal_join::record(params, &receipt).await?;
    let pool = params
        .repo
        .sqlite_db()
        .ok_or_else(|| CalmError::Internal("ACP journal missing".into()))?;
    let digest: String = sqlx::query_scalar(
        "SELECT registration_digest FROM acp_managed_sessions WHERE worker_session_id=?1",
    )
    .bind(&receipt.worker_session_id)
    .fetch_one(pool.read_pool())
    .await?;
    let (card, provider) = (params.card_id.clone(), params.provider.clone());
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = crate::db::sqlite::conversation::current_input_carrier_tx(
                tx,
                &card,
                &receipt.worker_session_id,
            )
            .await?;
            let (binding, _) =
                crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
            if binding.provider != provider {
                return Ok(None);
            }
            crate::db::sqlite::conversation::settle_kept_acp_receipt_tx(
                tx,
                &card,
                &receipt,
                &digest,
                crate::db::sqlite::AcpCleanupObservation::StoppedRevokedNormallyJoined,
            )
            .await
            .map_err(Into::into)
        })
    })
    .await
}
impl AcpPlannerSession {
    pub(crate) async fn settle_kept_execution(
        &self,
    ) -> Result<Option<crate::db::sqlite::conversation::RuntimeFrameAfterWrite>> {
        let _issue = self.shared.issue.lock().await;
        settle_kept_under_issue(&self.shared).await
    }
    /// ACP has no native idle query. Settlement is the exact completed receipt
    /// and its qualified cleanup, observed under issuance exclusion.
    pub(crate) async fn read_settled(
        &self,
        card: &str,
        thread: &str,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<bool> {
        let shared = &self.shared;
        let _issue = shared.issue.lock().await;
        if card != shared.params.card_id
            || shared.installed.get().is_none()
            || shared.params.seals.is_sealed(thread)
        {
            return Ok(false);
        }
        {
            let state = shared.state.lock().expect("ACP state");
            if state.closed || state.active.is_some() {
                return Ok(false);
            }
        }
        let Some(driver) = slot(shared) else {
            return Ok(false);
        };
        settle_kept_under_issue(shared).await?;
        let expected = {
            let driver = driver.lock().await;
            match &driver.input {
                DriverInput::Canonical(input) => Some(input.receipt.clone()),
                DriverInput::Cleanup
                | DriverInput::Historical
                | DriverInput::Maintenance { .. } => None,
            }
        };
        if let Some(expected) = expected {
            let pool = shared
                .params
                .repo
                .sqlite_db()
                .ok_or_else(|| CalmError::Internal("ACP journal missing".into()))?;
            if let Some(receipt) = acp_submission_get(
                pool.read_pool(),
                &expected.worker_session_id,
                &expected.client_id,
            )
            .await?
                && receipt.thread_id == thread
                && receipt.kept_cleanup.is_some()
                && current_receipt_owner(&shared.params, &receipt).await?
                && !acp_submission_unresolved(pool.read_pool(), &expected.worker_session_id).await?
            {
                admission.check_local().await?;
                return Ok(true);
            }
        }
        let Some(target) = driver.lock().await.target(&shared.params) else {
            return Ok(false);
        };
        if target.scope.thread_and_turn().0 != thread
            || self.qualify_execution_end(&target).await?.is_none()
        {
            return Ok(false);
        }
        admission.check_local().await?;
        Ok(true)
    }
    pub(crate) async fn owns_local_execution(&self, target: &ExecutionEndTarget) -> bool {
        let params = &self.shared.params;
        if !target.matches_owner(&params.card_id, &params.track_id, &params.worker_session_id) {
            return false;
        }
        match slot(&self.shared) {
            Some(driver) => driver.lock().await.matches(target),
            None => false,
        }
    }
    pub(crate) async fn qualify_execution_end(
        &self,
        target: &ExecutionEndTarget,
    ) -> Result<Option<Value>> {
        if !self.owns_local_execution(target).await {
            return Ok(None);
        }
        let Some(driver) = slot(&self.shared) else {
            return Ok(None);
        };
        let mut driver = driver.lock().await;
        if !driver.matches(target) || !driver.completion.is_finished() {
            return Ok(None);
        }
        match driver.finish().await? {
            Settlement::Unconfirmed | Settlement::CleanUnknown => return Ok(None),
            Settlement::Completed => {}
        }
        let DriverInput::Canonical(input) = &driver.input else {
            return Ok(None);
        };
        let expected = &input.receipt;
        let association = &input.association;
        let params = &self.shared.params;
        // END is returned factual ingress, including on a failed current
        // carrier. Reuse the canonical fact/contract owners, not issuance's
        // active-only admission. A park excludes sends, never this observation.
        let current = current_receipt_owner(params, expected).await?;
        if !current {
            return Ok(None);
        }
        let pool = params
            .repo
            .sqlite_db()
            .ok_or_else(|| CalmError::Internal("ACP journal missing".into()))?;
        let Some(receipt) = acp_submission_get(
            pool.read_pool(),
            &expected.worker_session_id,
            &expected.client_id,
        )
        .await?
        else {
            return Ok(None);
        };
        if receipt.thread_id != expected.thread_id
            || receipt.turn_id != expected.turn_id
            || receipt.native_session_id != expected.native_session_id
            || receipt.input_json != expected.input_json
            || !matches!(receipt.state, AcpSubmissionState::Completed)
            || AcpSubmissionInput::decode(&receipt.input_json)?
                != AcpSubmissionInput::Canonical(association.clone())
        {
            return Ok(None);
        }
        let outcome: Option<Value> = receipt
            .outcome_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(CalmError::from)?;
        // The scheduler can observe this joined receipt before the live event
        // consumer drains its items. END must not expose an unsettled history.
        if let Some(items) = outcome
            .as_ref()
            .and_then(|turn| turn.get("items"))
            .and_then(Value::as_array)
        {
            crate::harness::transcript_receipt::settle_canonical(
                params.repo.as_ref(),
                &params.worker_session_id,
                &params.card_id,
                &receipt.thread_id,
                &receipt.turn_id,
                items,
            )
            .await?;
        }
        Ok(outcome)
    }
    /// Retain the exact local owner before the canonical loop accepts Stop.
    /// This has no native or durable effect. Later writer failures remain
    /// uncertain after acceptance and never revive a failed carrier.
    pub(crate) async fn prepare_interrupt(
        &self,
        target: &ExecutionEndTarget,
    ) -> Result<PreparedInterrupt> {
        let _issue = self.shared.issue.lock().await;
        let params = &self.shared.params;
        if !target.matches_owner(&params.card_id, &params.track_id, &params.worker_session_id) {
            return Err(CalmError::Conflict(
                "control names another ACP execution".into(),
            ));
        }
        let driver = slot(&self.shared)
            .ok_or_else(|| CalmError::Conflict("ACP original driver is absent".into()))?;
        let retained = driver.lock().await;
        if !retained.matches(target) || retained.completion.is_finished() {
            return Err(CalmError::Conflict(
                "ACP original driver is no longer running".into(),
            ));
        }
        let (thread, turn) = target.scope.thread_and_turn();
        let state = self.shared.state.lock().expect("ACP state");
        let active = state
            .active
            .as_ref()
            .filter(|active| active.thread == thread && active.turn == turn)
            .ok_or_else(|| CalmError::Conflict("ACP execution already ended".into()))?;
        let approvals = active
            .approvals
            .upgrade()
            .ok_or_else(|| CalmError::Conflict("ACP approvals already closed".into()))?;
        Ok(PreparedInterrupt {
            shared: self.shared.clone(),
            driver: driver.clone(),
            approvals,
            thread,
            turn,
            native: active.native_session_id.clone(),
            signal: active.cancel.clone(),
        })
    }
}

/// A one-use control handle, never a lookup of a replacement execution.
pub(crate) struct PreparedInterrupt {
    shared: Arc<Shared>,
    driver: Arc<tokio::sync::Mutex<DriverSlot>>,
    approvals: Arc<Approvals>,
    thread: String,
    turn: String,
    native: String,
    signal: watch::Sender<CancelRequest>,
}

impl PreparedInterrupt {
    pub(crate) async fn send(
        self,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        let result = self.send_cancel(admission).await;
        // Canonical Stop was accepted before consuming this handle. A refusal
        // or an uncertain writer outcome must still bound the exact original
        // driver, without asking it to repeat a possibly crossed cancel.
        let bounded = self
            .signal
            .send(CancelRequest::Attempted)
            .map_err(|_| CalmError::Conflict("ACP interrupt handoff ended".into()));
        result.and(bounded)
    }

    async fn send_cancel(
        &self,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        #[cfg(all(test, feature = "fixtures"))]
        crate::acp_planner::test_seams::wait_before_interrupt_send(
            &self.shared.params.worker_session_id,
        )
        .await;
        admission.check_local().await?;
        let owner = self
            .shared
            .admission(&self.thread, None, Some(&self.native), Some(&self.turn));
        let combined = admission.combined(&owner);
        let gate = combined.gate_with_final_check(|| {
            // The native cancel is session-scoped: never let an old handle
            // cancel a successor, including when the native session is reused.
            let same =
                slot(&self.shared).is_some_and(|current| Arc::ptr_eq(&current, &self.driver));
            if !same {
                return Err(CalmError::Conflict(
                    "ACP original control driver changed".into(),
                ));
            }
            Ok(())
        });
        self.approvals
            .cancel(&self.native, &gate)
            .await
            .map_err(|error| combined.take_refusal().unwrap_or_else(|| wire_error(error)))
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn canonical_acp_panicked_exact_driver_is_not_end() {
        let (session, _repo, thread) = super::super::admission_tests::fixture().await;
        let thread = uuid::Uuid::parse_str(&thread).unwrap().to_string();
        let proof:calm_types::conversation::SubmissionDispatchProof=serde_json::from_value(json!({
            "version":"v2","attempt":{"submission_id":"11111111111111111111111111111111","attempt_id":"22222222222222222222222222222222"},
            "input_refs":[{"kind":"system","observation_id":"33333333333333333333333333333333"}],
            "render_plan":{"version":"v1","submission_id":"11111111111111111111111111111111","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
        })).unwrap();
        let params = &session.shared.params;
        let turn = uuid::Uuid::new_v4();
        let association = AcpCanonicalInput {
            logical_turn_id:
                calm_types::conversation::LogicalTurnId::parse(&crate::model::new_id()).unwrap(),
            proof,
        };
        let target = ExecutionEndTarget {
            card_id: params.card_id.clone(),
            track_id: params.track_id.clone(),
            worker_session_id: params.worker_session_id.clone(),
            logical_turn_id: association.logical_turn_id.clone(),
            proof: association.proof.clone(),
            scope: crate::harness::backend::ExecutionEndScope::ReceiptProcess {
                thread_id: uuid::Uuid::parse_str(&thread).unwrap(),
                local_turn_id: turn,
                native_session_id: "native".into(),
            },
        };
        let receipt = AcpSubmission {
            worker_session_id: params.worker_session_id.clone(),
            client_id: association.proof.attempt().submission_id.as_str().into(),
            turn_id: turn.to_string(),
            thread_id: thread,
            native_session_id: "native".into(),
            input_json: AcpSubmissionInput::Canonical(association.clone())
                .encode()
                .unwrap(),
            state: AcpSubmissionState::Completed,
            kept_cleanup: None,
            outcome_json: Some(json!({"id":turn,"status":"completed"}).to_string()),
        };
        let driver = tokio::spawn(async { panic!("driver panic") });
        while !driver.is_finished() {
            tokio::task::yield_now().await;
        }
        install(
            &session.shared,
            DriverSlot::new(driver, receipt, Association::Canonical(association)),
        );
        assert!(matches!(
            session.qualify_execution_end(&target).await,
            Ok(None) | Err(_)
        ));
        assert!(matches!(
            session.qualify_execution_end(&target).await,
            Ok(None) | Err(_)
        ));
    }
}

mod lost;

#[cfg(feature = "fixtures")]
impl AcpPlannerSession {
    pub fn driver_finished_for_test(&self) -> bool {
        driver_finished(&self.shared)
    }
    #[cfg(test)]
    pub(crate) async fn lock_driver_for_test(&self) -> impl Send {
        slot(&self.shared)
            .expect("retained ACP driver")
            .lock_owned()
            .await
    }
    pub async fn abort_driver_for_test(&self) {
        if let Some(slot) = slot(&self.shared) {
            let slot = slot.lock().await;
            if let Completion::Driver(RetainedJoin::Running(driver)) = &slot.completion {
                driver.abort();
            }
        }
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod lost_maintenance_tests;
