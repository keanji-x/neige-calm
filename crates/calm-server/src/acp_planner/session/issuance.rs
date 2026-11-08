//! One issue lock owns admission, setup, receipt preparation and driver handoff.
use super::dispatch::{self, Carrier, PreparedTurn, TurnInput};
use super::*;
use base64::Engine as _;

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
                InputItem::LocalImage { path } => {
                    if !crate::harness::PlannerBackend::metadata(&params.provider)
                        .capabilities
                        .images
                    {
                        return Err(refused(
                            "ACP image input is not enabled for this managed Planner",
                        ));
                    }
                    let (format, bytes) = crate::planner_attachments::read_bound_image(
                        &params.cwd,
                        &crate::ids::CardId::from(params.card_id.clone()),
                        std::path::Path::new(path),
                    )
                    .await?;
                    blocks.push(protocol::ContentBlock::Image {
                        data: base64::engine::general_purpose::STANDARD.encode(bytes),
                        mime_type: format.mime().into(),
                    });
                }
            }
        }
        let registration = params
            .host
            .registration(&params.provider)
            .map_err(|error| refused(&error.to_string()))?;
        super::super::execution::validate_registration(registration)?;
        let digest = registration.config.native_session_digest(&params.cwd)?;
        crate::db::sqlite::acp_registration_claim(
            &pool,
            &params.worker_session_id,
            &digest,
            row.agent_session_id.is_some(),
        )
        .await
        .map_err(|error| input.registration_failure(error))?;
        let dispatch = TurnInput {
            thread,
            correlation: client,
            contract: &input,
            items: &items,
            blocks: &blocks,
            permission,
            held,
        };
        if registration.profile.shared_server {
            return super::server::issue(
                shared,
                registration,
                row.agent_session_id,
                &selection,
                dispatch,
                receipt_turn,
                track_fence,
            )
            .await;
        }
        // Neige's server under this launch's own key and, for an agent that asks before its
        // tools, the tools it lists the session now.
        let server = registration.profile.mcp_server_key();
        let tools = match registration.profile.mcp_tool_request {
            Some(_) => params.host.listed_tools(&params.worker_session_id).await?,
            None => Vec::new(),
        };
        let asks = registration.profile.asks(&server, tools);
        let engine = crate::provider_execution::engine(params.engine.as_ref())?;
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
                    .capabilities = capabilities.capabilities
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
        let mut usage = provider::acp::usage::UsageTracker::new(registration.profile.usage);
        let prepared = async {
            let mcp = [params.mcp_server(server.clone(), &token)];
            let (native, configuration) = setup::attach(
                process,
                params,
                thread,
                row.agent_session_id,
                &mcp,
                &admission,
                &mut usage,
            )
            .await?;
            {
                let owner_admission =
                    shared.admission(thread, Some(permission), Some(&native), None);
                let admission = input.admission(&owner_admission);
                shared
                    .configure(
                        process,
                        &native,
                        configuration,
                        &selection,
                        &admission,
                        &mut usage,
                    )
                    .await?;
            }
            Ok::<_, StartFailure>(native)
        }
        .await;
        let result = match prepared {
            Ok(native) => {
                dispatch::send(
                    shared,
                    dispatch,
                    PreparedTurn {
                        native,
                        turn,
                        usage,
                        asks,
                    },
                    Carrier::Managed(&mut guard),
                    receipt_turn,
                )
                .await
            }
            Err(error) => Err(error),
        };
        if result.is_err() {
            guard.finish().await?;
        }
        result
    }
}
