//! Canonical retained recovery. Receipt identity comes from the Initial's
//! checkpoint/proof, never a latest-row heuristic or the historical queue.
use super::*;
use crate::db::sqlite::conversation::*;
use crate::db::sqlite::{AcpCanonicalInput, AcpSubmissionInput, AcpSubmissionState};
use calm_types::conversation::{InputAttemptFact, InputCorrelationFact, InputDeliveryFact};

impl AcpPlannerSession {
    /// Called under the retained install reservation, after the predecessor has
    /// left the registry. Opening a control object alone never runs recovery.
    pub(crate) async fn recover_retained(&self) -> Result<()> {
        let shared = &self.shared;
        let _issue = shared.issue.lock().await;
        if slot_exists(shared) {
            return Ok(());
        }
        let params = &shared.params;
        let (card, worker, provider) = (
            params.card_id.clone(),
            params.worker_session_id.clone(),
            params.provider.clone(),
        );
        let found = write_in_tx_typed(params.repo.as_ref(), move |tx| {
            Box::pin(async move { canonical_receipt_tx(tx, &card, &worker, &provider).await })
        })
        .await?;
        // Completed is persisted only after the original writer's confirmed
        // stop/revoke. Re-establish cleanup for this recovery process before
        // retaining that exact durable result; Sending/Unknown never qualify.
        let revoked = revoke(params).await;
        let stopped = self
            .stop_execution(crate::provider_execution::StopReason::Shutdown)
            .await;
        revoked?;
        stopped?;
        let Some((receipt, association)) = found else {
            return Ok(());
        };
        if !matches!(receipt.state, AcpSubmissionState::Completed) {
            return Ok(());
        }
        let outcome = completed_outcome(&receipt)?;
        crate::harness::turn_outcome::record(
            params.repo.as_ref(),
            &params.worker_session_id,
            &params.card_id,
            &params.track_id,
            &receipt.thread_id,
            &receipt.turn_id,
            &outcome,
            crate::harness::turn_outcome::Confirmation::ProviderTerminal,
        )
        .await?;
        execution_end::install(shared, DriverSlot::recovered(receipt, association));
        Ok(())
    }
}

fn completed_outcome(receipt: &AcpSubmission) -> Result<Value> {
    let mut outcome: Value = serde_json::from_str(
        receipt
            .outcome_json
            .as_deref()
            .ok_or_else(|| CalmError::Conflict("ACP completed receipt has no outcome".into()))?,
    )?;
    if outcome.get("id").and_then(Value::as_str) != Some(receipt.turn_id.as_str()) {
        return Err(CalmError::Conflict(
            "ACP receipt terminal names another turn".into(),
        ));
    }
    crate::harness::turn_outcome::canonical_diagnostic(&mut outcome);
    Ok(outcome)
}

fn slot_exists(shared: &Shared) -> bool {
    shared.driver.lock().expect("ACP driver").is_some()
}

pub(super) async fn canonical_receipt_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    worker: &str,
    provider: &AgentProvider,
) -> Result<Option<(AcpSubmission, AcpCanonicalInput)>> {
    let mut carrier = current_input_fact_carrier_tx(tx, card, worker).await?;
    let (binding, row) = crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
    if binding.provider != *provider {
        return Err(CalmError::Conflict("ACP recovery binding changed".into()));
    }
    let active = carrier.checkpoint().active.as_ref();
    let initial: Option<String> = match active {
        Some(_) => original_initial_submission_tx(tx, &carrier)
            .await?
            .map(|id| id.as_str().to_owned()),
        None => {
            let turn = carrier
                .snapshot()
                .get("last_turn_id")
                .and_then(Value::as_str);
            sqlx::query_scalar(
                "SELECT client_id FROM acp_submissions WHERE worker_session_id=?1 AND turn_id=?2",
            )
            .bind(worker)
            .bind(turn)
            .fetch_optional(&mut **tx)
            .await?
        }
    };
    let Some(initial) = initial else {
        return Ok(None);
    };
    let Some(receipt) = acp_submission_get(&mut **tx, worker, &initial).await? else {
        return Ok(None);
    };
    let AcpSubmissionInput::Canonical(association) =
        AcpSubmissionInput::decode(&receipt.input_json)?
    else {
        return Err(CalmError::Conflict(
            "canonical ACP recovery found a historical receipt".into(),
        ));
    };
    let attempt = association.proof.attempt();
    let stored = submission_plan_tx(tx, &attempt.submission_id).await?;
    let identity = submission_identity_tx(tx, &attempt.submission_id).await?;
    if receipt.client_id != initial
        || attempt.submission_id.as_str() != initial
        || identity.card_id != card
        || identity.logical_turn_id != association.logical_turn_id
        || active.is_some_and(|active| {
            association.logical_turn_id != active.logical_turn_id
                || !active.submission_ids.contains(&attempt.submission_id)
        })
        || carrier
            .checkpoint()
            .attempt(&attempt.submission_id)
            .is_none_or(|fact| fact.attempt != *attempt)
        || association.proof.render_plan().sha256 != stored.original_digest
        || association.proof.input_refs()
            != stored
                .plan
                .input_refs()
                .map_err(|error| CalmError::Conflict(error.into()))?
        || carrier
            .snapshot()
            .get("last_thread_id")
            .and_then(Value::as_str)
            != Some(receipt.thread_id.as_str())
        || row.thread_id.as_deref() != Some(receipt.thread_id.as_str())
        || row.agent_session_id.as_deref() != Some(receipt.native_session_id.as_str())
    {
        return Err(CalmError::Conflict(
            "ACP recovery receipt association changed".into(),
        ));
    }
    let fact = carrier
        .checkpoint()
        .attempt(&attempt.submission_id)
        .expect("checked attempt");
    match fact.delivery {
        InputDeliveryFact::CrossingUnknown => {
            // Recover a lost private report through the existing fact owner.
            // This refines correlation only; receipt existence never proves delivery.
            record_input_attempt_fact_tx(
                tx,
                &mut carrier,
                InputAttemptFact {
                    attempt: attempt.clone(),
                    correlation: InputCorrelationFact {
                        provider: provider.clone(),
                        thread_id: receipt.thread_id.clone(),
                        native_session_id: Some(receipt.native_session_id.clone()),
                        native_turn_id: None,
                        local_turn_id: Some(receipt.turn_id.clone()),
                        rpc_request_id: None,
                    },
                    delivery: InputDeliveryFact::Unknown,
                },
            )
            .await?;
        }
        InputDeliveryFact::Unknown | InputDeliveryFact::Accepted { .. } => {
            if fact.correlation.provider != *provider
                || fact.correlation.thread_id != receipt.thread_id
                || fact.correlation.local_turn_id.as_deref() != Some(receipt.turn_id.as_str())
                || fact.correlation.native_session_id.as_deref()
                    != Some(receipt.native_session_id.as_str())
                || fact.correlation.native_turn_id.is_some()
            {
                return Err(CalmError::Conflict(
                    "ACP recovery correlation changed".into(),
                ));
            }
        }
        InputDeliveryFact::NotDispatched { .. } => {
            return Err(CalmError::Conflict(
                "ACP receipt contradicts dispatch refusal".into(),
            ));
        }
    }
    Ok(Some((receipt, association)))
}
/// Reconcile pre-restart claims before serving ingress. A receipt-backed release
/// requires proof that the exact ACP owner exited.
pub(in crate::acp_planner) async fn recover_shared_after_boot(
    repo: &dyn Repo,
    provider: AgentProvider,
    legacy_sweep: Option<&crate::provider_execution::PreC4Sweep>,
) -> Result<()> {
    let policy = super::super::profile::AcpAgentProfile::of(&provider)
        .and_then(|profile| profile.resident)
        .expect("shared ACP recovery profile");
    let inventory = provider.clone();
    let candidates = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            shared_acp_restart_candidates_tx(tx, &inventory)
                .await
                .map_err(CalmError::from)
        })
    })
    .await?;
    for (card, worker) in candidates {
        let legacy_swept = legacy_sweep.is_some_and(|proof| proof.proves_absent(&worker));
        if let Err(error) = recover_shared_claim(
            repo,
            &card,
            &worker,
            &provider,
            policy.release_unknown_on_restart,
            legacy_swept,
        )
        .await
        {
            tracing::warn!(%worker, %error, "Shared ACP restart release retained its fence");
        }
    }
    Ok(())
}

async fn recover_shared_claim(
    repo: &dyn Repo,
    card: &str,
    worker: &str,
    provider: &AgentProvider,
    release_unknown_on_restart: bool,
    legacy_swept: bool,
) -> Result<()> {
    let completed = write_in_tx_typed(repo, {
        let (card, worker, provider) = (card.to_owned(), worker.to_owned(), provider.clone());
        move |tx| {
            Box::pin(async move {
                let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
                let (binding, row) =
                    crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
                if binding.provider != provider
                    || row.mcp_token_hash.is_some()
                    || calm_truth::db::sqlite::provider_execution::retained(tx, &worker).await?
                {
                    return Ok(None);
                }
                let found = canonical_receipt_tx(tx, &card, &worker, &provider).await?;
                let mut client = None;
                if let Some((receipt, association)) = &found {
                    if matches!(receipt.state, AcpSubmissionState::Completed) {
                        let header = serde_json::from_value::<
                            calm_types::harness::snapshot::CanonicalHarnessSnapshot,
                        >(carrier.snapshot().clone())?
                        .header;
                        return Ok(header.interruption_intent.is_some().then(|| {
                            (
                                receipt.clone(),
                                association.clone(),
                                row.track_id.to_string(),
                            )
                        }));
                    }
                    let owner = crate::db::sqlite::acp_shared_receipt_process_tx(
                        tx,
                        &worker,
                        &receipt.client_id,
                    )
                    .await?;
                    if !restart_owner_stopped(tx, &provider, legacy_swept, owner).await? {
                        return Ok(None);
                    }
                    // Stopping the resident proves there can be no later END, but
                    // cannot tell whether its prompt reached the provider. Preserve
                    // the active input so explicit Keep or Abandon owns the decision.
                    if !release_unknown_on_restart {
                        if matches!(receipt.state, AcpSubmissionState::Sending) {
                            crate::db::sqlite::acp_submission_finish_tx(
                                tx,
                                &worker,
                                &receipt.client_id,
                                None,
                            )
                            .await?;
                        }
                        return Ok(None);
                    }
                    client = Some(receipt.client_id.as_str());
                }
                release_shared_acp_restart_tx(tx, &card, &worker, row.thread_id.as_deref(), client)
                    .await?;
                Ok(None)
            })
        }
    })
    .await?;
    if let Some((receipt, association, track)) = completed {
        recover_completed_stop(repo, card, worker, &track, receipt, association).await?;
    }
    Ok(())
}

async fn restart_owner_stopped(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    provider: &AgentProvider,
    legacy_swept: bool,
    owner: Option<crate::db::sqlite::AcpSharedRestartOwner>,
) -> Result<bool> {
    use crate::db::sqlite::AcpSharedRestartOwner;
    match owner {
        Some(AcpSharedRestartOwner::Process(owner)) if legacy_swept => Ok(
            crate::proc_identity::observe_owned_pid(owner.pid, owner.start_time, &owner.boot_id)
                .leader_exited(),
        ),
        Some(AcpSharedRestartOwner::Execution { execution_id }) => {
            super::super::resident_execution::restart_owner_exited_tx(tx, provider, &execution_id)
                .await
        }
        _ => Ok(false),
    }
}

async fn recover_completed_stop(
    repo: &dyn Repo,
    card: &str,
    worker: &str,
    track: &str,
    receipt: AcpSubmission,
    association: AcpCanonicalInput,
) -> Result<()> {
    let outcome = completed_outcome(&receipt)?;
    if let Some(items) = outcome.get("items").and_then(Value::as_array) {
        crate::harness::transcript_receipt::settle_canonical(
            repo,
            worker,
            card,
            &receipt.thread_id,
            &receipt.turn_id,
            items,
        )
        .await?;
    }
    crate::harness::turn_outcome::record(
        repo,
        worker,
        card,
        track,
        &receipt.thread_id,
        &receipt.turn_id,
        &outcome,
        crate::harness::turn_outcome::Confirmation::ProviderTerminal,
    )
    .await?;
    let (card, worker) = (card.to_owned(), worker.to_owned());
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            let Some(original) = original_input_execution_tx(tx, &carrier).await? else {
                return Ok(());
            };
            if original.proof == association.proof
                && original.turn_id() == Some(receipt.turn_id.as_str())
            {
                complete_input_execution_tx(
                    tx,
                    &card,
                    &worker,
                    &RuntimeExecutionFence::from_carrier(&carrier),
                    &original,
                )
                .await?;
            }
            Ok(())
        })
    })
    .await
}

#[cfg(all(test, target_os = "linux"))]
#[tokio::test]
async fn legacy_process_receipt_requires_marker_sweep_and_matching_exit() {
    use crate::db::sqlite::AcpSharedRestartOwner;
    let mut child = tokio::process::Command::new("/bin/sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = i32::try_from(child.id().unwrap()).unwrap();
    let legacy = serde_json::json!({
        "pid": pid,
        "start_time": crate::proc_identity::read_proc_start_time(pid).unwrap(),
        "boot_id": crate::proc_identity::read_boot_id().unwrap(),
    });
    let owner: AcpSharedRestartOwner = serde_json::from_value(legacy).unwrap();
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let provider = AgentProvider::Antigravity;
    assert!(
        !restart_owner_stopped(&mut tx, &provider, true, Some(owner.clone()))
            .await
            .unwrap()
    );
    child.kill().await.unwrap();
    assert!(
        !restart_owner_stopped(&mut tx, &provider, false, Some(owner.clone()))
            .await
            .unwrap()
    );
    assert!(
        restart_owner_stopped(&mut tx, &provider, true, Some(owner))
            .await
            .unwrap()
    );
}
