//! Activation of an already accepted canonical carrier by the original start
//! owner. Only this invocation holds the live source; durable rows hold facts.
use super::*;
use crate::conversation_inputs::InputSource;
use crate::db::sqlite::conversation::{self, RuntimeExecutionFence};
use crate::harness::snapshot::CanonicalHarnessSnapshot;
use crate::mcp_server::wiring::mint_card_mcp_token_pair;

mod binding;
mod recovery;

#[derive(Clone)]
pub(super) enum StartInvocation {
    Registered,
    HistoricalHuman(crate::conversation_inputs::HistoricalHumanSource),
    Retained {
        source: InputSource,
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
    binding: crate::harness::profile::PlannerBinding,
    inputs: crate::conversation_inputs::SourceInputs,
    snapshot: CanonicalHarnessSnapshot,
    fence: RuntimeExecutionFence,
    activation_operation: Option<String>,
}

impl PlannerHarnessStartAdapter {
    fn retained_source(&self) -> Result<(&InputSource, &std::path::Path)> {
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
        source.require_repo(self.repo.as_ref())?;
        let carrier =
            conversation::current_input_carrier_tx(tx, &wire.card_id, &wire.worker_session_id)
                .await?;
        crate::conversation_inputs::authorize_resource_tx(tx, source, &carrier).await?;
        let (binding, row) =
            crate::harness::profile::input_carrier_binding_tx(tx, &carrier).await?;
        if !matches!(
            binding.profile,
            HarnessProfile::Planner | HarnessProfile::Assistant
        ) {
            return Err(CalmError::Conflict(
                "retained activation is unavailable for this provider/profile".into(),
            ));
        }
        let inputs = crate::conversation_inputs::source_inputs_tx(tx, source, &carrier).await?;
        let snapshot: CanonicalHarnessSnapshot =
            serde_json::from_value(carrier.snapshot().clone())?;
        if snapshot.header.last_thread_id != row.thread_id {
            return Err(CalmError::Conflict(
                "retained native association changed".into(),
            ));
        }
        Ok(Target {
            track_id: row.track_id.to_string(),
            binding,
            inputs,
            snapshot,
            fence: RuntimeExecutionFence::from_carrier(&carrier),
            activation_operation: row.spawn_op_id,
        })
    }

    pub(super) async fn retained_preparation_track(
        &self,
        wire: &RetainedPayload,
    ) -> Result<String> {
        Ok(self.retained_target(wire).await?.track_id)
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
        source: InputSource,
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
        // Association belongs to the accepted worker/input lifecycle. A caller
        // that cannot restore another source's active claim still retains its
        // own exact accepted tail, without authorizing that preceding claim.
        let (source, _) = bound.retained_source()?;
        source.check_current()?;
        let sources = self.harness_registry.canonical_sources(
            &wire.worker_session_id,
            &TrackId::from(target.track_id.clone()),
        );
        for input in target.inputs.identities() {
            sources.observe(input.clone(), source.clone());
        }
        if self.harness_registry.get(&wire.worker_session_id).is_none() {
            if target.snapshot.header.last_thread_id.is_none() {
                if target.activation_operation.is_some() {
                    return Err(CalmError::Conflict(
                        "retained native activation is unresolved".into(),
                    ));
                }
                // A down backend must not bind an activation marker or fail the
                // accepted carrier. A later real POST may re-admit this head.
                PlannerFactory::new(
                    Arc::downgrade(&bound.daemon),
                    &bound.claude_host,
                    &bound.acp_host,
                )
                .ready_retained(&target.binding.provider)
                .await?;
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
                bound
                    .recover_retained(&wire, events, runtime.track_delete_locks())
                    .await?;
            }
        }
        // Re-read all current scope after native/handle awaits. This binds only
        // the exact actual head in the caller's namespace; Unknown is not freed.
        bound.retained_target(&wire).await?;
        self.harness_registry
            .get(&wire.worker_session_id)
            .ok_or_else(|| CalmError::Conflict("retained handle is not installed".into()))?;
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
        PlannerFactory::new(
            Arc::downgrade(&self.daemon),
            &self.claude_host,
            &self.acp_host,
        )
        .ready_retained(&target.binding.provider)
        .await?;
        let (_, root) = self.retained_source()?;
        let track = self
            .repo
            .track_get(&target.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("track".into()))?;
        self.retained_target(wire).await?;
        workspace_runtime::selection::materialize_workspace(
            &track.workspace,
            root,
            &target.track_id,
        )?;
        self.retained_target(wire).await?;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("retained-workspace-ready", &wire.card_id).await;
        match PlannerFactory::retained_identity(&target.binding)? {
            crate::harness::backend::factory::RetainedIdentityPreparation::LocalSession => {
                return self
                    .retained_bind_local(wire, output, op, &target.binding, uuid::Uuid::new_v4())
                    .await;
            }
            crate::harness::backend::factory::RetainedIdentityPreparation::NativeThread => {}
        }
        let instructions = crate::harness::instructions::profile_instructions(
            self.repo.as_ref(),
            &self.plugin,
            &target.track_id,
            &wire.card_id,
            &target.binding,
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
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("retained-claim-committed", &wire.card_id).await;
        // All fallible post-claim admission belongs to the typed mint result:
        // BeforeSend releases this claim at the single match below.
        let (raw, hashed) = mint_card_mcp_token_pair();
        let admitted = self.clone();
        let admitted_wire = wire.clone();
        let admitted_operation = op.clone();
        let thread = PlannerFactory::new(
            Arc::downgrade(&self.daemon),
            &self.claude_host,
            &self.acp_host,
        )
        .prepare_codex_thread(
            &wire.card_id,
            target
                .binding
                .profile
                .mcp_role()
                .ok_or_else(|| CalmError::Conflict("retained profile has no role".into()))?,
            &target.track_id,
            track.workspace.agent_cwd().to_string(),
            Some(instructions),
            target.binding.profile.mcp_role(),
            PathBuf::from(socket_path),
            raw,
            true,
            &crate::harness::native_admission::NativePromptAdmission::new(
                || self.retained_source()?.0.check_current(),
                move || {
                    let admitted = admitted.clone();
                    let wire = admitted_wire.clone();
                    let operation = admitted_operation.clone();
                    let repo = admitted.repo.clone();
                    #[cfg(feature = "fixtures")]
                    let card = wire.card_id.clone();
                    Box::pin(async move {
                        write_in_tx_typed(repo.as_ref(), move |tx| {
                            Box::pin(async move {
                                super::super::repo_sqlite::require_live_claim_tx(tx, &operation)
                                    .await?;
                                admitted.retained_target_tx(tx, &wire).await?;
                                Ok(())
                            })
                        })
                        .await?;
                        #[cfg(feature = "fixtures")]
                        crate::test_seams::pause_point("retained-admission-committed", &card).await;
                        Ok(())
                    })
                },
            ),
        )
        .await;
        let thread = match thread {
            Ok(thread) => thread,
            Err(crate::shared_codex_appserver::ThreadMintFailure::BeforeSend(error)) => {
                self.retained_not_sent(wire, op, output).await?;
                return Err(error);
            }
            Err(crate::shared_codex_appserver::ThreadMintFailure::Unknown(_)) => {
                return Err(CalmError::Conflict(
                    "retained native activation outcome is unknown".into(),
                ));
            }
        };
        // The driver's existing failure journal must retain a returned native
        // identity even if binding/its event cannot commit. It remains held;
        // neither that error nor a later request authorizes another mint.
        let previous_output = output.clone();
        output.data = json!({"activation":"native_known", "thread_id":thread});
        let repo = self.repo.clone();
        let provider = target.binding.provider.clone();
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
                    binding::bind_identity_tx(tx, &wire, &op, &expected, provider, &thread_tx)
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

    // A transport BeforeSend proof permits scoped cleanup even after revocation.
    // Clear the claim and its crossing journal atomically; input facts stay pending.
    async fn retained_not_sent(
        &self,
        wire: &RetainedPayload,
        op: &Operation,
        output: &mut TxOutput,
    ) -> Result<()> {
        let wire = wire.clone();
        let op = op.clone();
        let previous = output.clone();
        let mut prepared = output.clone();
        prepared.data = json!({"activation":"prepared"});
        let checkpoint = prepared.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                super::super::repo_sqlite::require_live_claim_tx(tx, &op).await?;
                if !crate::db::sqlite::session_release_unstarted_operation_tx(
                    tx,
                    &wire.card_id,
                    &wire.worker_session_id,
                    &op.id,
                )
                .await?
                {
                    return Err(CalmError::Conflict(
                        "retained activation owner changed".into(),
                    ));
                }
                checkpoint_app_server_interact_tx(
                    tx,
                    &op,
                    AppServerInteractKind::MintAndAwait { thread_id: None },
                    &previous,
                    &checkpoint,
                )
                .await?;
                Ok(())
            })
        })
        .await?;
        *output = prepared;
        Ok(())
    }

    pub(super) async fn retained_spawn(
        &self,
        wire: &RetainedPayload,
        events: &crate::event::EventBus,
        track_delete_locks: &crate::per_card_lock::KeyedLocks,
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
        let backend = PlannerFactory::new(
            Arc::downgrade(&self.daemon),
            &self.claude_host,
            &self.acp_host,
        )
        .open(
            self.seals.clone(),
            &self.planner_wiring(),
            self.repo.clone(),
            PlannerRow {
                binding: target.binding.clone(),
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
        backend.recover_retained().await?;
        // Recovery may refine a lost private report. Load the committed frame,
        // retaining the same source, worker and workspace fences.
        let target = self.retained_target(wire).await?;
        let handle = PlannerHarness::run(PlannerHarnessParams {
            track_delete_locks: track_delete_locks.clone(),
            worker_session_id: wire.worker_session_id.clone(),
            track_id: TrackId::from(target.track_id),
            card_id: CardId::from(wire.card_id.clone()),
            thread_id: Some(thread),
            repo: self.repo.clone(),
            events: events.clone(),
            card_role_cache: self.card_role_cache.clone(),
            track_area_cache: self.track_area_cache.clone(),
            backend,
            registry: self.harness_registry.clone(),
            config: HarnessConfig::default(),
            snapshot: crate::harness::HarnessRuntimeSnapshot::decode(serde_json::to_value(
                target.snapshot,
            )?)?,
        })
        .await?;
        let admitted = self
            .retained_target(wire)
            .await
            .and_then(|_| self.retained_source()?.0.check_current());
        if let Err(error) = admitted {
            handle.shutdown().await?;
            return Err(error);
        }
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
