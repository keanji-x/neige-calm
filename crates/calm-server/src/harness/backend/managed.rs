//! Ephemeral managed invocations, bound only from the existing harness owner.
use super::*;
use crate::harness::queue::QueueEntry;
use crate::harness::run_loop::Inner;
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::InputItem;

/// A sampled binding for one operation, not another session or binding store.
/// Construct and consume inside the caller's existing synchronization scope.
pub(in crate::harness) struct IssueTurnHandle<'a> {
    owner: &'a Inner,
    thread: Option<String>,
}

struct TurnInvocation<'a> {
    claim: &'a [QueueEntry],
    rewind: Option<&'a mut BackendRewind>,
}

impl<'a> IssueTurnHandle<'a> {
    pub(in crate::harness) async fn from_reconciliation(owner: &'a Inner) -> Self {
        Self {
            owner,
            thread: owner.thread_id.read().await.clone(),
        }
    }

    fn thread(&self) -> Result<&str> {
        self.thread
            .as_deref()
            .ok_or_else(|| CalmError::Internal("managed invocation has no bound thread".into()))
    }
    /// Decide how this provider removes `target` from the conversation, changing nothing. A refusal
    /// is a `Conflict` the reader is shown. Codex needs no preparation; Claude needs an anchor before
    /// the turn and a dry run of the cut.
    pub(in crate::harness) async fn prepare_rewind(
        self,
        target: NativeEditTarget,
    ) -> Result<EditPreparation<CalmError>> {
        let thread_id = self.thread()?;
        if let Some(reader) = self.owner.backend.issuance_hold() {
            return Err(CalmError::Conflict(reader));
        }
        match (&self.owner.backend.0, &target) {
            (Arm::Acp(_), _) => Ok(EditPreparation::Failed {
                error: CalmError::Conflict("ACP history replacement is unsupported".into()),
            }),
            (Arm::Codex(..), NativeEditTarget::BeforeTurn { .. }) => {
                Ok(EditPreparation::Prepared {
                    plan: NativeEditPlan { target },
                })
            }
            (
                Arm::Claude(session),
                NativeEditTarget::TranscriptCut {
                    resume_at,
                    drops_turn,
                },
            ) => {
                session
                    .check_truncation(
                        thread_id,
                        &ResumeTruncation {
                            at: *resume_at,
                            drops_turn: *drops_turn,
                        },
                    )
                    .await
            }
            (Arm::Codex(..), NativeEditTarget::TranscriptCut { .. })
            | (Arm::Claude(_), NativeEditTarget::BeforeTurn { .. }) => Err(CalmError::Internal(
                "an edit target names another provider".into(),
            )),
        }
    }

    /// The Claude arm passes the stored model and effort as `--model=` and `--effort=` (#1810,
    /// #1822 6′); no catalog is consulted here, and the CLI judges the model. `rewind` is applied
    /// first: Codex reverts the thread before `turn/start`, Claude cuts the session in the spawn.
    /// `permission` is the conversation's permission mode as the card holds it now (#2348), read
    /// once by the run loop for every arm: Codex sends its approval settings with every turn, the
    /// Claude spawn runs under it, and ACP answers its permission requests by it.
    pub(in crate::harness) async fn turn_start(
        self,
        request: TurnRequest,
        claim: &[QueueEntry],
        rewind: Option<&mut BackendRewind>,
    ) -> std::result::Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
        if let Some(reader) = self.owner.backend.issuance_hold() {
            return Err(TurnStartFailure::AwaitingRecovery {
                error: CalmError::ServiceUnavailable(reader.clone()),
                reader,
            });
        }
        let thread_id = self.thread().map_err(TurnStartFailure::Transient)?;
        let invocation = TurnInvocation { claim, rewind };
        // The host's stored marker and the request are one invocation, never independent authority.
        if request.edit
            != invocation
                .rewind
                .as_deref()
                .map_or(TurnEdit::NoEdit, BackendRewind::turn_edit)
        {
            return Err(mismatched_rewind());
        }
        match &self.owner.backend.0 {
            Arm::Acp(session) => {
                if invocation.rewind.is_some() {
                    return Err(mismatched_rewind());
                }
                session
                    .start_turn(thread_id, request, invocation.claim)
                    .await
            }
            Arm::Codex(daemon, approvals) => {
                let TurnRequest {
                    input,
                    selection,
                    permission,
                    edit,
                } = request;
                let InputRequest { items, correlation } = input;
                if let Some(rewind) = invocation.rewind {
                    match (&mut rewind.0, edit) {
                        (RewindArm::Codex { reverted: true, .. }, TurnEdit::NoEdit) => {}
                        (
                            RewindArm::Codex { reverted, .. },
                            TurnEdit::ApplyNativePlan(NativeEditPlan {
                                target: NativeEditTarget::BeforeTurn { before_turn_id },
                            }),
                        ) => {
                            if let Some(failure) =
                                revert_codex_thread(daemon, thread_id, &before_turn_id).await?
                            {
                                return Ok(failure);
                            }
                            *reverted = true;
                        }
                        (RewindArm::Claude { .. }, _) | (RewindArm::Codex { .. }, _) => {
                            return Err(mismatched_rewind());
                        }
                    }
                }
                approvals.route(daemon, thread_id);
                codex_start_result(
                    daemon
                        .turn_start_control(
                            thread_id,
                            items,
                            &selection,
                            TurnApprovals::Explicit(permission),
                            Some(&correlation.0),
                        )
                        .await,
                )
            }
            Arm::Claude(session) => {
                let TurnRequest {
                    input,
                    selection,
                    permission,
                    edit,
                } = request;
                let InputRequest { items, correlation } = input;
                if matches!(
                    invocation.rewind.as_deref().map(|rewind| &rewind.0),
                    Some(RewindArm::Codex { .. })
                ) {
                    return Err(mismatched_rewind());
                }
                let truncation = match edit {
                    TurnEdit::NoEdit => None,
                    TurnEdit::ApplyNativePlan(NativeEditPlan {
                        target:
                            NativeEditTarget::TranscriptCut {
                                resume_at,
                                drops_turn,
                            },
                    }) => Some(ResumeTruncation {
                        at: resume_at,
                        drops_turn,
                    }),
                    TurnEdit::ApplyNativePlan(NativeEditPlan {
                        target: NativeEditTarget::BeforeTurn { .. },
                    }) => return Err(mismatched_rewind()),
                };
                session
                    .turn_start_control(
                        thread_id,
                        items,
                        &selection,
                        permission,
                        &correlation.0,
                        truncation.as_ref(),
                    )
                    .await
            }
        }
    }

    pub(in crate::harness) async fn turn_steer(
        self,
        request: SteerRequest,
    ) -> Result<SteerOutcome<CalmError>> {
        let thread_id = self.thread()?;
        match &self.owner.backend.0 {
            Arm::Codex(daemon, _) => Ok(steer_outcome(
                daemon
                    .turn_steer(
                        thread_id,
                        request.turn.as_str(),
                        request.input.items,
                        Some(&request.input.correlation.0),
                    )
                    .await,
            )),
            Arm::Claude(_) | Arm::Acp(_) => Err(CalmError::Internal(
                "a Claude Planner cannot steer; the run loop checks supports_steer first".into(),
            )),
        }
    }

    pub(in crate::harness) async fn compact_start(
        self,
        request: CompactRequest,
    ) -> Result<CompactSubmission<CalmError>> {
        let thread_id = self.thread()?;
        match &self.owner.backend.0 {
            Arm::Codex(daemon, _) => Ok(codex_compact_submission(
                daemon.thread_compact_start(thread_id).await,
            )),
            Arm::Claude(session) => Ok(session.compact_start(thread_id, &request.selection).await),
            Arm::Acp(_) => Err(CalmError::BadRequest(
                "Manual context compaction is available for Codex and Claude conversations.".into(),
            )),
        }
    }

    pub(in crate::harness) async fn turn_interrupt(
        self,
        request: InterruptRequest,
    ) -> InterruptOutcome<InterruptDiagnostic> {
        let Some(thread_id) = self.thread.as_deref() else {
            return InterruptOutcome::NoTarget;
        };
        let turn_id = request.turn.as_str();
        match &self.owner.backend.0 {
            Arm::Acp(session) => session
                .turn_interrupt(thread_id, turn_id)
                .await
                .map_error(|e| Box::new(e) as InterruptDiagnostic),
            Arm::Codex(daemon, _) => daemon
                .interrupt_outcome(thread_id, turn_id)
                .await
                .map_error(|e| Box::new(e) as InterruptDiagnostic),
            Arm::Claude(session) => session
                .turn_interrupt(thread_id, turn_id)
                .await
                .map_error(|e| Box::new(e) as InterruptDiagnostic),
        }
    }

    /// Stop this harness's turns. `Err` is the last step that failed, for strict callers; every
    /// failure is logged here. `last_turn_id` is sampled by the harness under its issuance boundary.
    pub(in crate::harness) async fn shutdown(self) -> Result<()> {
        let worker_session_id = &self.owner.worker_session_id;
        let thread_id = self.thread.as_deref();
        let mut interrupt_error = None;
        match &self.owner.backend.0 {
            Arm::Acp(session) => return session.shutdown().await,
            // #1791 §5.1: the running turn is recorded `Interrupted` and this waits for `stop`,
            // whether or not a turn runs or a thread is known.
            Arm::Claude(session) => {
                if let Err(e) = session.shutdown().await {
                    tracing::warn!(
                        worker_session_id = %worker_session_id,
                        error = %e,
                        "planner harness shutdown: the Claude Planner stop did not confirm"
                    );
                    interrupt_error = Some(e);
                }
            }
            Arm::Codex(daemon, approvals) => {
                // The thread's approvals have nowhere to go once this harness stops.
                approvals.release();
                if let Some(thread_id) = thread_id {
                    let last_turn_id = self.owner.shutdown_last_turn().await;
                    interrupt_codex_thread(
                        daemon,
                        thread_id,
                        last_turn_id.as_deref(),
                        |step, e| {
                            match step {
                                CodexInterruptStep::ActiveTurn => tracing::warn!(
                                    thread_id,
                                    error = %e,
                                    "planner harness shutdown thread interrupt failed"
                                ),
                                CodexInterruptStep::FallbackTurn(last_turn_id) => tracing::warn!(
                                    thread_id,
                                    turn_id = %last_turn_id,
                                    error = %e,
                                    "planner harness shutdown last-known turn interrupt failed"
                                ),
                            }
                            interrupt_error = Some(e);
                        },
                    )
                    .await;
                }
            }
        }
        match interrupt_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl IssueTurnHandle<'_> {
    /// `selection` and `permission` are required: a default would silently answer which model
    /// runs this turn, or what it may do without asking. `client_user_message_id` is the
    /// projection row's key; codex hands it back as `item.clientId`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::harness) async fn issue(
        self,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        permission: PlannerPermissionMode,
        client_user_message_id: &str,
        claim: &[QueueEntry],
        pending_rewind: Option<&mut BackendRewind>,
    ) -> std::result::Result<String, TurnStartFailure> {
        let edit = pending_rewind.as_deref().map_or(
            provider::control::TurnEdit::NoEdit,
            BackendRewind::turn_edit,
        );
        crate::harness::backend::ordinary_start_result(
            self.turn_start(
                provider::control::TurnRequest {
                    input: provider::control::InputRequest {
                        items: input,
                        correlation: provider::control::CorrelationKey(
                            client_user_message_id.into(),
                        ),
                    },
                    selection: selection.clone(),
                    permission,
                    edit,
                },
                claim,
                pending_rewind,
            )
            .await,
        )
    }
}
