//! Activation of an already accepted canonical carrier by the original start
//! owner. Only this invocation holds the live source; durable rows hold facts.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::{self, RuntimeExecutionFence};
use crate::harness::snapshot::CanonicalHarnessSnapshot;

mod recovery;

#[derive(Clone)]
pub(super) enum StartInvocation {
    Registered,
    HistoricalHuman(crate::conversation_inputs::HistoricalHumanSource),
    Retained {
        source: HumanInputSource,
        workspace_root: PathBuf,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetainedPayload {
    canonical_activation_version: u32,
    card_id: String,
    worker_session_id: String,
}

impl RetainedPayload {
    pub(super) fn card_id(&self) -> &str {
        &self.card_id
    }

    pub(super) fn decode(value: &Value) -> Result<Option<Self>> {
        if value.get("canonical_activation_version").is_none() {
            return Ok(None);
        }
        let payload: Self = serde_json::from_value(value.clone())?;
        if payload.canonical_activation_version != 2
            || payload.card_id.is_empty()
            || payload.worker_session_id.is_empty()
        {
            return Err(CalmError::Conflict(
                "unsupported retained activation".into(),
            ));
        }
        Ok(Some(payload))
    }
}

pub(super) struct Target {
    track_id: String,
    inputs: crate::conversation_inputs::HumanSourceInputs,
    snapshot: CanonicalHarnessSnapshot,
    fence: RuntimeExecutionFence,
    activation_operation: Option<String>,
}

impl PlannerHarnessStartAdapter {
    fn retained_source(&self) -> Result<(&HumanInputSource, &std::path::Path)> {
        match &self.invocation {
            StartInvocation::Retained {
                source,
                workspace_root,
            } => Ok((source, workspace_root)),
            StartInvocation::Registered | StartInvocation::HistoricalHuman(_) => Err(
                CalmError::Conflict("retained activation requires a current human request".into()),
            ),
        }
    }

    async fn retained_target_tx(&self, tx: &mut Tx<'_>, wire: &RetainedPayload) -> Result<Target> {
        let (source, _) = self.retained_source()?;
        let carrier =
            conversation::current_input_carrier_tx(tx, &wire.card_id, &wire.worker_session_id)
                .await?;
        crate::conversation_inputs::authorize_human_resource_tx(tx, source, &carrier).await?;
        let (binding, row) =
            crate::harness::profile::input_carrier_binding_tx(tx, &carrier).await?;
        // This first spine consumes the actual existing Codex personal environment.
        // Other provider/profile startup remains with its existing owner.
        if binding.profile != HarnessProfile::Planner || binding.provider != AgentProvider::Codex {
            return Err(CalmError::Conflict(
                "retained activation is unavailable for this provider/profile".into(),
            ));
        }
        let inputs =
            crate::conversation_inputs::human_source_inputs_tx(tx, source, &carrier).await?;
        let snapshot: CanonicalHarnessSnapshot =
            serde_json::from_value(carrier.snapshot().clone())?;
        if snapshot.header.last_thread_id != row.thread_id {
            return Err(CalmError::Conflict(
                "retained native association changed".into(),
            ));
        }
        Ok(Target {
            track_id: row.track_id.to_string(),
            inputs,
            snapshot,
            fence: RuntimeExecutionFence::from_carrier(&carrier),
            activation_operation: row.spawn_op_id,
        })
    }

    async fn retained_target(&self, wire: &RetainedPayload) -> Result<Target> {
        let this = self.clone();
        let wire = wire.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { this.retained_target_tx(tx, &wire).await })
        })
        .await
    }

    /// The fence owns this complete invocation, including cancellation of its
    /// waiter. The current head is read from SQL, never from an old creation ACK.
    pub(crate) async fn wake_retained(
        self: &Arc<Self>,
        runtime: &Arc<super::super::OperationRuntime>,
        card_id: &CardId,
        source: HumanInputSource,
        workspace_root: PathBuf,
        events: &crate::event::EventBus,
    ) -> Result<()> {
        let mut bound = self.as_ref().clone();
        bound.invocation = StartInvocation::Retained {
            source,
            workspace_root,
        };
        let actual_card = card_id.to_string();
        let worker_session_id = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                sqlx::query_scalar::<_, Option<String>>("SELECT session_id FROM cards WHERE id=?1")
                    .bind(&actual_card)
                    .fetch_optional(&mut **tx)
                    .await?
                    .flatten()
                    .ok_or_else(|| {
                        CalmError::Conflict("accepted conversation has no current carrier".into())
                    })
            })
        })
        .await?;
        let wire = RetainedPayload {
            canonical_activation_version: 2,
            card_id: card_id.to_string(),
            worker_session_id,
        };
        let target = bound.retained_target(&wire).await?;
        if self.harness_registry.get(&wire.worker_session_id).is_none() {
            if target.snapshot.header.last_thread_id.is_none() {
                if target.activation_operation.is_some() {
                    return Err(CalmError::Conflict(
                        "retained native activation is unresolved".into(),
                    ));
                }
                // A down backend must not bind an activation marker or fail the
                // accepted carrier. A later real POST may re-admit this head.
                bound.check_ready(&AgentProvider::Codex).await?;
                let payload = serde_json::to_value(&wire)?;
                let key = super::super::OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload)?,
                };
                runtime
                    .execute_with_adapter(self.clone(), Arc::new(bound.clone()), key, payload)
                    .await?;
            } else {
                // The existing lifecycle order is card -> operation drive. This
                // recovery creates no operation and cannot re-enter the driver.
                let _operations = runtime.lock_for_track_delete().await;
                bound.recover_retained(&wire, events).await?;
            }
        }
        // Re-read all current scope after native/handle awaits. This binds only
        // the exact actual head in the caller's namespace; Unknown is not freed.
        let current = bound.retained_target(&wire).await?;
        let handle = self
            .harness_registry
            .get(&wire.worker_session_id)
            .ok_or_else(|| CalmError::Conflict("retained handle is not installed".into()))?;
        let (source, _) = bound.retained_source()?;
        for input in current.inputs.refs() {
            handle
                .observe_canonical_source(input.clone(), source.clone())
                .await?;
        }
        Ok(())
    }

    pub(super) async fn retained_validate(&self, wire: &RetainedPayload) -> Result<()> {
        self.retained_target(wire).await?;
        Ok(())
    }

    pub(super) async fn retained_prepare(
        &self,
        tx: &mut Tx<'_>,
        wire: &RetainedPayload,
    ) -> Result<TxOutput> {
        let target = self.retained_target_tx(tx, wire).await?;
        if target.snapshot.header.last_thread_id.is_some()
            || target.snapshot.header.phase != HarnessPhaseTag::Idle
            || target.snapshot.canonical_inputs.active.is_some()
            || target.snapshot.header.pending_rewind.is_some()
            || target.snapshot.header.interruption_intent.is_some()
            || target.snapshot.header.wedged_reason.is_some()
        {
            return Err(CalmError::Conflict(
                "retained native activation is not ready".into(),
            ));
        }
        let claimed: Option<String> =
            sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
                .bind(&wire.worker_session_id)
                .fetch_one(&mut **tx)
                .await?;
        if claimed.is_some() {
            return Err(CalmError::Conflict(
                "retained native activation is unresolved".into(),
            ));
        }
        let mut output = TxOutput::new(
            "card",
            Some(wire.card_id.clone()),
            json!({"worker_session_id": wire.worker_session_id}),
        );
        output.data = json!({"activation": "prepared"});
        Ok(output)
    }

    pub(super) async fn retained_interact(
        &self,
        wire: &RetainedPayload,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let target = self.retained_target(wire).await?;
        if let Some(thread) = target.snapshot.header.last_thread_id {
            return Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id: thread });
        }
        if output.data.get("activation").and_then(Value::as_str) != Some("prepared") {
            return Err(CalmError::Conflict(
                "retained native activation outcome is unresolved".into(),
            ));
        }
        self.check_ready(&AgentProvider::Codex).await?;
        let (_, root) = self.retained_source()?;
        let track = self
            .repo
            .track_get(&target.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("track".into()))?;
        self.retained_target(wire).await?;
        crate::workspace_service::workspace_materialize::materialize_workspace(
            &track.workspace,
            root,
            &target.track_id,
        )?;
        self.retained_target(wire).await?;
        let instructions = crate::harness::instructions::profile_instructions(
            self.repo.as_ref(),
            &self.plugin,
            &target.track_id,
            &wire.card_id,
            &crate::harness::profile::PlannerBinding {
                profile: HarnessProfile::Planner,
                provider: AgentProvider::Codex,
            },
        )
        .await?;
        let socket_path = self.mcp_socket_path_for_thread()?;
        let appserver_sock = self.daemon.remote_uri();
        let _mint = self.lock_card_mint(&wire.card_id).await;
        let this = self.clone();
        let wire_tx = wire.clone();
        let op_tx = op.clone();
        let mut checkpoint = output.clone();
        checkpoint.data = json!({"activation": "crossing_unknown"});
        let checkpoint_tx = checkpoint.clone();
        let previous_output = output.clone();
        let expected = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let target = this.retained_target_tx(tx, &wire_tx).await?;
                if target.snapshot.header.last_thread_id.is_some() {
                    return Err(CalmError::Conflict("retained activation changed".into()));
                }
                if !crate::db::sqlite::session_claim_start_operation_tx(
                    tx,
                    &wire_tx.card_id,
                    &wire_tx.worker_session_id,
                    &op_tx.id,
                )
                .await?
                {
                    return Err(CalmError::Conflict(
                        "retained activation already crossed".into(),
                    ));
                }
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_tx,
                    AppServerInteractKind::MintAndAwait { thread_id: None },
                    &previous_output,
                    &checkpoint_tx,
                )
                .await?;
                Ok(target.fence)
            })
        })
        .await?;
        *output = checkpoint;
        // No await separates the fresh source check and beginning this effect.
        self.retained_target(wire).await?;
        self.retained_source()?.0.current_principal()?;
        let (raw, hashed) = mint_card_mcp_token_pair();
        let params = codex_start_params(
            HarnessProfile::Planner,
            track.workspace.agent_cwd().to_string(),
            Some(instructions),
            raw,
            PathBuf::from(socket_path),
        );
        let admitted = self.clone();
        let admitted_wire = wire.clone();
        let admitted_operation = op.clone();
        let thread = self
            .daemon
            .thread_start_mint_admitted_for_card(&wire.card_id, params, move || {
                let admitted = admitted.clone();
                let wire = admitted_wire.clone();
                let operation = admitted_operation.clone();
                let repo = admitted.repo.clone();
                async move {
                    write_in_tx_typed(repo.as_ref(), move |tx| {
                        Box::pin(async move {
                            super::super::repo_sqlite::require_live_claim_tx(tx, &operation)
                                .await?;
                            admitted.retained_target_tx(tx, &wire).await?;
                            Ok(())
                        })
                    })
                    .await
                }
            })
            .await
            .map_err(|_| {
                CalmError::Conflict("retained native activation outcome is unknown".into())
            })?;
        // The driver's existing failure journal must retain a returned native
        // identity even if binding/its event cannot commit. It remains held;
        // neither that error nor a later request authorizes another mint.
        let previous_output = output.clone();
        output.data = json!({"activation":"native_known", "thread_id":thread});
        let repo = self.repo.clone();
        let wire = wire.clone();
        let op = op.clone();
        let thread_tx = thread.clone();
        let mut checkpoint = output.clone();
        checkpoint.data = json!({"activation":"bound", "thread_id":thread});
        // Recording a completed effect is not new execution admission. No new
        // login is needed to retain its exact native result and original carrier.
        let scope = card_scope(
            ctx.repo.as_ref(),
            CardId::from(wire.card_id.clone()),
            TrackId::from(target.track_id),
        )
        .await?;
        let write = WriteContext::new(self.card_role_cache.clone(), self.track_area_cache.clone());
        write_with_event_typed(
            repo.as_ref(),
            ActorId::Kernel,
            scope,
            None,
            &ctx.events,
            &write,
            move |tx| {
                Box::pin(async move {
                    let current = conversation::current_input_carrier_tx(
                        tx,
                        &wire.card_id,
                        &wire.worker_session_id,
                    )
                    .await?;
                    let mut frame: CanonicalHarnessSnapshot =
                        serde_json::from_value(current.snapshot().clone())?;
                    frame.header.last_thread_id = Some(thread_tx.clone());
                    let mut header = serde_json::to_value(frame)?;
                    header
                        .as_object_mut()
                        .expect("canonical frame")
                        .remove(calm_types::conversation::CONVERSATION_INPUT_CHECKPOINT_KEY);
                    conversation::persist_runtime_frame_tx(
                        tx,
                        &wire.card_id,
                        &wire.worker_session_id,
                        &expected,
                        header,
                    )
                    .await?;
                    let owner: Option<String> =
                        sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
                            .bind(&wire.worker_session_id)
                            .fetch_one(&mut **tx)
                            .await?;
                    if owner.as_deref() != Some(op.id.as_str()) {
                        return Err(CalmError::Conflict(
                            "retained activation owner changed".into(),
                        ));
                    }
                    session_bind_attribution_tx(
                        tx,
                        &wire.worker_session_id,
                        ThreadAttribution {
                            worker_session_id: wire.worker_session_id.clone(),
                            provider: AgentProvider::Codex,
                            thread_id: Some(thread_tx.clone()),
                            session_id: None,
                            active_turn_id: None,
                        },
                    )
                    .await?;
                    persist_card_mcp_token_hash(tx, &wire.card_id, &hashed).await?;
                    mirror_session_mcp_token(tx, &wire.worker_session_id, &hashed).await?;
                    let card = card_apply_harness_start_payload_tx(
                        tx,
                        &wire.card_id,
                        &thread_tx,
                        &appserver_sock,
                    )
                    .await?;
                    checkpoint_app_server_interact_tx(
                        tx,
                        &op,
                        AppServerInteractKind::MintAndAwait {
                            thread_id: Some(thread_tx),
                        },
                        &previous_output,
                        &checkpoint,
                    )
                    .await?;
                    Ok(((), Event::CardUpdated(card)))
                })
            },
        )
        .await?;
        output.data = json!({"activation":"bound", "thread_id":thread});
        Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id: thread })
    }

    pub(super) async fn retained_spawn(
        &self,
        wire: &RetainedPayload,
        events: &crate::event::EventBus,
    ) -> Result<SpawnOutcome> {
        let target = self.retained_target(wire).await?;
        let thread = target
            .snapshot
            .header
            .last_thread_id
            .clone()
            .ok_or_else(|| {
                CalmError::Conflict("retained native activation is unresolved".into())
            })?;
        if self.harness_registry.get(&wire.worker_session_id).is_some() {
            return Ok(SpawnOutcome::Ready(SpawnHandle::Harness {
                worker_session_id: wire.worker_session_id.clone(),
            }));
        }
        let reservation = self
            .harness_registry
            .try_reserve(wire.worker_session_id.clone())
            .ok_or_else(|| {
                CalmError::Conflict("retained handle installation is in progress".into())
            })?;
        self.retained_target(wire).await?;
        let backend = PlannerBackend::open(
            AgentProvider::Codex,
            self.daemon.clone(),
            self.seals.clone(),
            &self.claude_wiring(),
            self.repo.clone(),
            ClaudePlannerRow {
                binding: crate::harness::profile::PlannerBinding {
                    profile: HarnessProfile::Planner,
                    provider: AgentProvider::Codex,
                },
                worker_session_id: &wire.worker_session_id,
                card_id: &wire.card_id,
                track_id: &target.track_id,
                prior_total_tokens: target
                    .snapshot
                    .header
                    .token_usage
                    .as_ref()
                    .map_or(0, |usage| usage.total_tokens),
            },
        )
        .await?;
        self.retained_target(wire).await?;
        let handle = PlannerHarness::run(PlannerHarnessParams {
            worker_session_id: wire.worker_session_id.clone(),
            track_id: TrackId::from(target.track_id),
            card_id: CardId::from(wire.card_id.clone()),
            thread_id: Some(thread),
            repo: self.repo.clone(),
            events: events.clone(),
            card_role_cache: self.card_role_cache.clone(),
            track_area_cache: self.track_area_cache.clone(),
            backend,
            live_replies: self.harness_registry.live_replies().clone(),
            config: HarnessConfig::default(),
            snapshot: crate::harness::HarnessRuntimeSnapshot::decode(serde_json::to_value(
                target.snapshot,
            )?)?,
        })
        .await?;
        if !reservation.install(handle.clone()) {
            handle.shutdown().await?;
            return Err(CalmError::Conflict(
                "retained handle installation changed".into(),
            ));
        }
        Ok(SpawnOutcome::Ready(SpawnHandle::Harness {
            worker_session_id: wire.worker_session_id.clone(),
        }))
    }
}
