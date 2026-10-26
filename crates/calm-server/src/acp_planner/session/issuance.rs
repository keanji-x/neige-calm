//! One issue lock owns admission, setup, receipt preparation and driver handoff.
use super::*;

impl AcpPlannerSession {
    pub(super) async fn issue(
        &self,
        thread: &str,
        request: provider::control::TurnRequest,
        input: InputContract<'_, '_>,
        receipt_turn: &mut ReceiptPresence,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> std::result::Result<ReceiptAttempt, StartFailure> {
        let provider::control::TurnRequest {
            input: provider::control::InputRequest { items, correlation },
            selection,
            permission,
            edit: _, // The owner-bound managed entry already checked the stored rewind.
        } = request;
        let client = &correlation.0;
        let shared = &self.shared;
        let params = &shared.params;
        let _issue = shared.issue.lock().await;
        if shared.parked.is_parked() {
            return Err(refused("ACP carrier is parked"));
        }
        let pool = params
            .repo
            .sqlite_db()
            .ok_or_else(|| refused("ACP requires durable submission storage"))?;
        let existing = acp_submission_get(pool.read_pool(), &params.worker_session_id, client)
            .await
            .map_err(CalmError::from)?;
        *receipt_turn = match &existing {
            Some(receipt) => ReceiptPresence::Present(ReceiptAttempt {
                turn: receipt.turn_id.clone(),
                native_session_id: receipt.native_session_id.clone(),
            }),
            None => ReceiptPresence::Absent,
        };
        let Some(held) = shared.installed.get().cloned() else {
            return Err(refused("ACP harness has not been installed"));
        };
        if shared.state.lock().expect("ACP state").closed || params.seals.is_sealed(thread) {
            return Err(refused("ACP conversation is closed or sealed"));
        }
        let row = shared.current_owner(thread, Some(permission)).await?;
        match &input {
            InputContract::Historical(_) => {
                crate::conversation_inputs::require_historical_storage(
                    params.repo.as_ref(),
                    &params.card_id,
                )
                .await?
            }
            InputContract::Canonical { .. } => {}
        }
        if let InputContract::Canonical { .. } = &input
            && !super::super::wiring::canonical_inputs(&params.provider, row.contract)
        {
            return Err(refused("ACP canonical input requires an eligible binding"));
        }
        if let Some(receipt) = existing {
            let issued = crate::db::sqlite::AcpSubmissionInput::decode(&receipt.input_json)
                .map_err(CalmError::from)?;
            if receipt.thread_id != thread
                || row.agent_session_id.as_deref() != Some(&receipt.native_session_id)
                || !input.matches(&issued)
            {
                return Err(refused("ACP receipt ownership or original input changed"));
            }
            // Exact-key recovery retires the original queue batch without any native write.
            let turn = receipt.turn_id.clone();
            match &input {
                InputContract::Historical(_) => {
                    let outcome = receipt
                        .outcome_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(CalmError::from)?
                        .unwrap_or_else(|| unknown_outcome(&turn));
                    let _ = shared.events.send(PlannerEvent {
                        thread_id: Some(thread.into()),
                        kind: PlannerEventKind::TurnStarted {
                            turn_id: turn.clone(),
                        },
                    });
                    let _ = shared.events.send(PlannerEvent {
                        thread_id: Some(thread.into()),
                        kind: PlannerEventKind::TurnCompleted { turn: outcome },
                    });
                }
                InputContract::Canonical { .. } => {}
            }
            return Ok(ReceiptAttempt {
                turn,
                native_session_id: receipt.native_session_id,
            });
        }
        // Exact receipt replay above starts no process. Every new issue, historical or
        // canonical, shares the provider fence under this session's issue lock.
        crate::provider_execution::admission_gate(
            &pool,
            params.engine.as_ref(),
            &params.worker_session_id,
            Some(&params.host.provider_execution),
        )
        .await?;
        let engine = crate::provider_execution::engine(params.engine.as_ref())?;
        input.check_source().await?;
        if shared.state.lock().expect("ACP state").active.is_some()
            || acp_submission_unresolved(pool.read_pool(), &params.worker_session_id)
                .await
                .map_err(CalmError::from)?
        {
            return Err(input.unresolved_failure());
        }
        join_driver(shared).await?;
        let mut blocks = Vec::new();
        let has_receipts: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM acp_submissions WHERE worker_session_id=?1)",
        )
        .bind(&params.worker_session_id)
        .fetch_one(pool.read_pool())
        .await
        .map_err(CalmError::from)?;
        if !has_receipts {
            blocks.push(protocol::ContentBlock::text(format!(
                "Neige conversation instructions:\n{}",
                params.instructions
            )));
        }
        for item in &items {
            match item {
                InputItem::Text { text } => blocks.push(protocol::ContentBlock::text(text.clone())),
                InputItem::LocalImage { .. } => {
                    return Err(refused(
                        "ACP image input is not enabled for this managed Planner",
                    ));
                }
            }
        }
        let registration = params
            .host
            .registration(&params.provider)
            .map_err(|error| refused(&error.to_string()))?;
        super::super::execution::validate_registration(registration)?;
        let digest = registration.config.native_session_digest(&params.cwd)?;
        // Neige's server under this launch's own key and, for an agent that asks before its
        // tools, the tools it lists the session now.
        let server = registration.profile.mcp_server_key();
        let tools = match registration.profile.mcp_tool_request {
            Some(_) => params.host.listed_tools(&params.worker_session_id).await?,
            None => Vec::new(),
        };
        let asks = registration.profile.asks(&server, tools);
        crate::db::sqlite::acp_registration_claim(
            &pool,
            &params.worker_session_id,
            &digest,
            row.agent_session_id.is_some(),
        )
        .await
        .map_err(|error| input.registration_failure(error))?;
        let mut guard = IssueGuard::new(shared);
        #[cfg(all(test, feature = "fixtures"))]
        super::super::test_seams::wait_issue(
            &params.worker_session_id,
            super::super::test_seams::IssueStage::BeforeMint,
        )
        .await;
        let token = match mint_token(params).await {
            Ok(token) => token,
            Err(error) => {
                drop(guard);
                join_driver(shared).await?;
                return Err(error.into());
            }
        };
        let turn = uuid::Uuid::new_v4().to_string();
        if let Err(error) = guard
            .launch(
                engine,
                registration,
                &token,
                permission,
                crate::provider_execution::Purpose::Turn {
                    turn_id: turn.clone(),
                },
                track_fence,
            )
            .await
        {
            guard.finish().await?;
            return Err(error.into());
        }
        let negotiated = super::super::process::negotiate(
            &guard
                .process
                .as_ref()
                .expect("admitted process")
                .connection
                .client,
            registration,
            registration.profile.compaction(row.contract)
                == Some(super::super::profile::CompactionWire::StandardPrompt),
        )
        .await;
        match negotiated {
            Ok(capabilities) => {
                guard
                    .process
                    .as_mut()
                    .expect("admitted process")
                    .capabilities = capabilities
            }
            Err(refusal) => {
                guard.finish().await?;
                return Err(StartFailure::Failed(CalmError::Conflict(refusal)));
            }
        }
        #[cfg(all(test, feature = "fixtures"))]
        super::super::test_seams::wait_issue(
            &params.worker_session_id,
            super::super::test_seams::IssueStage::AfterSpawn,
        )
        .await;
        let process = guard.process.as_mut().expect("spawned process");
        let original_native = row.agent_session_id.clone();
        let owner_admission =
            shared.admission(thread, Some(permission), original_native.as_deref(), None);
        let admission = input.admission(&owner_admission);
        let prepared = async {
            let mcp = [params.mcp_server(server.clone(), &token)];
            let (native, configuration) = setup::attach(
                process,
                params,
                thread,
                row.agent_session_id,
                &mcp,
                &admission,
            )
            .await?;
            {
                let owner_admission =
                    shared.admission(thread, Some(permission), Some(&native), None);
                let admission = input.admission(&owner_admission);
                shared
                    .configure(process, &native, configuration, &selection, &admission)
                    .await?;
            }
            if params.seals.is_sealed(thread) || shared.state.lock().expect("ACP state").closed {
                return Err(refused("ACP conversation was sealed before dispatch"));
            }
            let receipt = AcpSubmission {
                worker_session_id: params.worker_session_id.clone(),
                client_id: client.into(),
                turn_id: turn.clone(),
                thread_id: thread.into(),
                native_session_id: native.clone(),
                input_json: input.receipt(&items, &blocks)?,
                state: crate::db::sqlite::AcpSubmissionState::Sending,
                kept_cleanup: None,
                outcome_json: None,
            };
            guard.receipt = Some(receipt.clone());
            #[cfg(all(test, feature = "fixtures"))]
            super::super::test_seams::wait_issue(
                &params.worker_session_id,
                super::super::test_seams::IssueStage::BeforeReceipt,
            )
            .await;
            match acp_submission_prepare(&pool, &receipt, crate::model::now_ms()).await {
                Ok(()) => {
                    *receipt_turn = ReceiptPresence::Present(ReceiptAttempt {
                        turn: turn.clone(),
                        native_session_id: native.clone(),
                    })
                }
                Err(crate::db::sqlite::AcpPrepareError::BeforeReceipt(error)) => {
                    return Err(CalmError::from(error).into());
                }
                Err(crate::db::sqlite::AcpPrepareError::CommitUncertain(error)) => {
                    *receipt_turn = ReceiptPresence::Present(ReceiptAttempt {
                        turn: turn.clone(),
                        native_session_id: native.clone(),
                    });
                    return Err(CalmError::from(error).into());
                }
            }
            Ok::<_, StartFailure>((native, turn))
        }
        .await;
        let (native, turn) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                guard.finish().await?;
                return Err(error);
            }
        };
        #[cfg(all(test, feature = "fixtures"))]
        super::super::test_seams::wait_issue(
            &params.worker_session_id,
            super::super::test_seams::IssueStage::AfterReceipt,
        )
        .await;
        #[cfg(all(test, feature = "fixtures"))]
        if let Some(connection) =
            super::super::test_transport::connection(&params.worker_session_id)
        {
            guard.process.as_mut().expect("prepared process").connection = connection;
        }
        let (process, receipt, claim, driver_guard) = guard.handoff();
        let execution_id = crate::provider_execution::execution_id(claim.record())?;
        #[cfg(feature = "fixtures")]
        let held = super::super::test_seams::tap_held(&params.worker_session_id, held);
        let approvals = Arc::new(Approvals::for_turn(
            permission,
            &held,
            &turn,
            &process.connection.client,
            asks,
        ));
        let (cancel, cancelled) = watch::channel(CancelRequest::Running);
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            "carrier-quiescence-acp-issuance",
            &params.worker_session_id,
        )
        .await;
        shared.state.lock().expect("ACP state").active = Some(Active {
            thread: thread.into(),
            turn: turn.clone(),
            cancel,
            approvals: Arc::downgrade(&approvals),
            native_session_id: native.clone(),
            execution_id: execution_id.clone(),
        });
        let translator = TurnTranslator::new(TurnContext {
            thread_id: thread.into(),
            turn_id: turn.clone(),
            native_session_id: native.clone(),
        });
        let _ = shared.events.send(translator.started());
        let client = process.connection.client.clone();
        let capabilities = process.capabilities.clone();
        let (send_pending, pending) = tokio::sync::oneshot::channel();
        let retained_receipt = receipt.clone();
        let association = input.association();
        let task_shared = Arc::clone(shared);
        let driver = tokio::spawn(async move {
            drive(
                task_shared,
                process,
                receipt,
                translator,
                pending,
                cancelled,
                approvals,
                claim,
                driver_guard,
            )
            .await
        });
        execution_end::install(
            shared,
            DriverSlot::new(driver, retained_receipt, association).with_execution(execution_id),
        );
        #[cfg(all(test, feature = "fixtures"))]
        super::super::test_seams::wait_issue(
            &params.worker_session_id,
            super::super::test_seams::IssueStage::DriverInstalled,
        )
        .await;
        let pending = {
            let owner_admission =
                shared.admission(thread, Some(permission), Some(&native), Some(&turn));
            let admission = input.admission(&owner_admission);
            protocol::prompt(&client, &capabilities, &native, &blocks, admission.gate()).await
        };
        let _ = send_pending.send(pending);
        // A durable dispatch attempt always hands ownership to the Harness, even if its pipe failed.
        Ok(ReceiptAttempt {
            turn,
            native_session_id: native,
        })
    }
}
