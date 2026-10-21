//! One retained driver; END also requires the exact authoritative receipt.
use super::*;
use crate::db::sqlite::{AcpCanonicalInput, AcpSubmissionInput, AcpSubmissionState};
use crate::harness::backend::ExecutionEndTarget;
use crate::harness::retained_join::RetainedJoin;

#[derive(Clone, Copy)]
pub(super) enum Settlement {
    Completed,
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
}
struct CanonicalDriver {
    receipt: AcpSubmission,
    association: AcpCanonicalInput,
}
pub(super) struct DriverSlot {
    completion: RetainedJoin<Settlement>,
    input: DriverInput,
}
impl DriverSlot {
    pub(super) fn cleanup(driver: tokio::task::JoinHandle<Settlement>) -> Self {
        Self {
            completion: RetainedJoin::new(driver),
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
            completion: RetainedJoin::new(driver),
            input,
        }
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
        self.completion
            .finish()
            .await
            .copied()
            .map_err(|()| CalmError::Conflict("ACP receipt writer did not finish normally".into()))
    }
}
fn slot(shared: &Shared) -> Option<Arc<tokio::sync::Mutex<DriverSlot>>> {
    shared.driver.lock().expect("ACP driver").clone()
}
pub(super) fn install(shared: &Shared, driver: DriverSlot) {
    *shared.driver.lock().expect("ACP driver") = Some(Arc::new(tokio::sync::Mutex::new(driver)));
}
pub(super) async fn join_driver(shared: &Shared) -> Result<()> {
    if let Some(driver) = slot(shared) {
        driver.lock().await.finish().await?;
    }
    Ok(())
}
impl AcpPlannerSession {
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
            Settlement::Unconfirmed => return Ok(None),
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
        let (card, worker, track, provider, thread, native) = (
            params.card_id.clone(),
            params.worker_session_id.clone(),
            params.track_id.clone(),
            params.provider.clone(),
            expected.thread_id.clone(),
            expected.native_session_id.clone(),
        );
        let current = write_in_tx_typed(params.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let carrier = crate::db::sqlite::conversation::current_input_fact_carrier_tx(
                    tx, &card, &worker,
                )
                .await?;
                let (binding, row) =
                    crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
                Ok(binding.provider == provider
                    && row.track_id.as_str() == track
                    && row.thread_id.as_deref() == Some(thread.as_str())
                    && row.agent_session_id.as_deref() == Some(native.as_str()))
            })
        })
        .await?;
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
        receipt
            .outcome_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(Into::into)
    }
    pub(crate) async fn interrupt_execution(
        &self,
        target: &ExecutionEndTarget,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<()> {
        if !self.owns_local_execution(target).await {
            return Err(CalmError::Conflict(
                "control names another ACP execution".into(),
            ));
        }
        admission.check_local().await?;
        let (thread, turn) = target.scope.thread_and_turn();
        let (approvals, native, signal) = {
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
            (
                approvals,
                active.native_session_id.clone(),
                active.cancel.clone(),
            )
        };
        let owner = self
            .shared
            .admission(&thread, None, Some(&native), Some(&turn));
        let combined = admission.combined(&owner);
        approvals
            .cancel(&native, combined.gate())
            .await
            .map_err(|error| combined.take_refusal().unwrap_or_else(|| wire_error(error)))?;
        // The real writer already sent cancel. The same driver owns its deadline,
        // receipt and cleanup; do not enqueue a second native cancel.
        signal
            .send(CancelRequest::Written)
            .map_err(|_| CalmError::Conflict("ACP interrupt handoff ended".into()))
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
