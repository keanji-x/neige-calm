//! The shared turn and compaction Engine admission and first protocol line.
use super::*;

impl ClaudePlannerSession {
    /// The submission contract for either kind of spawn; see the module docs.
    pub(in crate::claude_planner) async fn start_spawn(
        &self,
        thread: &str,
        input: SpawnInput<'_>,
        selection: &TurnModelSelection,
        mode: PlannerPermissionMode,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<Uuid, SpawnFailure> {
        let purpose = match &input {
            SpawnInput::Turn { .. } | SpawnInput::Canonical { .. } => SpawnPurpose::Turn,
            SpawnInput::Compaction | SpawnInput::CanonicalCompaction(_) => SpawnPurpose::Compaction,
        };
        let shared = &self.shared;
        let params = &shared.params;
        let _issue = shared.issue.lock().await;
        self.provider_admission_gate().await?;
        let engine = crate::provider_execution::engine(params.engine.as_ref())?;
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        if shared.parked.is_parked() {
            return Err(CalmError::Conflict("Claude carrier is parked".into()).into());
        }
        let Some(held) = shared.installed.get() else {
            return Err(
                CalmError::Conflict("claude planner harness is not installed yet".into()).into(),
            );
        };
        let (start, row_bound, token, prior_total_tokens, settle_after_stop) = {
            let state = shared.state();
            if state.shutting_down {
                return Err(
                    CalmError::Conflict("claude planner session is shutting down".into()).into(),
                );
            }
            if state.active.is_some() {
                return Err(
                    CalmError::Conflict("a claude planner turn is still running".into()).into(),
                );
            }
            (
                state.start,
                state.row_bound,
                state.mcp_token.clone(),
                state.total_tokens,
                state.settle_after_stop,
            )
        };
        execution_end::join_driver(shared).await?;
        if purpose == SpawnPurpose::Compaction && start == SessionStart::New {
            return Err(CalmError::BadRequest(
                "this Claude conversation has no session to compact yet".into(),
            )
            .into());
        }
        let thread_uuid = Uuid::try_parse(thread).map_err(|_| {
            CalmError::BadRequest(format!("claude planner thread {thread} is not a UUID"))
        })?;
        let local_turn = Uuid::new_v4();
        let turn_id = local_turn.to_string();
        if params.seals.is_sealed(thread) {
            return Err(sealed(thread).into());
        }
        let live = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await
            .map_err(CalmError::from)?
            .ok_or_else(|| CalmError::Conflict("Claude owner is missing".into()))?;
        let binding = crate::harness::profile::local_binding(
            params.repo.as_ref(),
            &live,
            &calm_types::runtime::AgentProvider::Claude,
            &params.card_id,
            &params.track_id,
            thread,
        )
        .await?
        .ok_or_else(|| CalmError::Conflict("Claude live owner identity changed".into()))?;
        if binding.profile == crate::harness::profile::HarnessProfile::Assistant
            && mode != PlannerPermissionMode::Never
        {
            return Err(CalmError::Conflict("Claude live owner identity changed".into()).into());
        }
        let host = &params.host;
        let config = host
            .configured()
            .map_err(|_| needs_an_operator(unavailable_message()))?;
        let env = spawn::session_env(params, &config.config_dir)?;
        match config.version_problem(&env).await {
            None => {}
            Some(super::super::config::VersionProblem::NotPinned(problem)) => {
                return Err(needs_an_operator(problem).into());
            }
            Some(super::super::config::VersionProblem::NoAnswer(problem)) => {
                return Err(SpawnFailure::failed(CalmError::Conflict(problem)));
            }
        }
        // A shutdown or a deletion seal may have landed while `--version` ran; nothing is minted
        // for a session that can no longer start a turn.
        if shared.state().shutting_down {
            return Err(
                CalmError::Conflict("claude planner session is shutting down".into()).into(),
            );
        }
        if params.seals.is_sealed(thread) {
            return Err(sealed(thread).into());
        }
        // A revocation under a live harness (e.g. one an aborted deletion's recovery could not
        // replace, §5.1 item 4) nulls the row's hash; the next spawn must not carry a credential that
        // no longer authenticates.
        let token = match token {
            Some(token) if self.row_hash_present().await? => token,
            _ => self.mint_mcp_token().await?,
        };
        let mut driver_slot = shared.driver.lock().await;

        let prepared = async {
            let (line, client_id, items, truncation, execution) = match input {
                SpawnInput::Turn {
                    items,
                    client_id,
                    truncation,
                } => {
                    let line = invocation::user_line(thread_uuid, client_id, &items).await?;
                    (
                        line,
                        client_id.to_string(),
                        items,
                        truncation,
                        execution_end::Association::Historical,
                    )
                }
                SpawnInput::Canonical {
                    items,
                    client_id,
                    execution,
                } => {
                    let line = invocation::user_line(thread_uuid, client_id, &items).await?;
                    (
                        line,
                        client_id.to_string(),
                        items,
                        None,
                        execution_end::Association::Work(execution),
                    )
                }
                // No client sent this line: its uuid is fresh, and nothing of it is stored.
                SpawnInput::Compaction => {
                    let uuid = Uuid::new_v4();
                    let line = UserLine::compact(thread_uuid, uuid);
                    (
                        line,
                        uuid.simple().to_string(),
                        Vec::new(),
                        None,
                        execution_end::Association::Historical,
                    )
                }
                SpawnInput::CanonicalCompaction(target) => {
                    let uuid = Uuid::new_v4();
                    let line = UserLine::compact(thread_uuid, uuid);
                    (
                        line,
                        uuid.simple().to_string(),
                        Vec::new(),
                        None,
                        execution_end::Association::Maintenance(target),
                    )
                }
            };
            let line = serde_json::to_string(&line).map_err(|e| SpawnFailure::failed(e.into()))?;
            let translator = TurnTranslator::new(
                TurnContext {
                    thread_id: thread.to_string(),
                    turn_id: turn_id.clone(),
                    client_id,
                    input: items,
                    cwd: params.cwd.to_string_lossy().into_owned(),
                    prior_total_tokens,
                },
                params.calm_tools.clone(),
            )
            .map_err(|e| SpawnFailure::failed(CalmError::BadRequest(e.to_string())))?;
            let instructions = InstructionsFile::write(
                &host.instructions_dir,
                &params.worker_session_id,
                &turn_id,
                &params.instructions,
                #[cfg(feature = "fixtures")]
                shared
                    .hooks
                    .lock()
                    .expect("hooks")
                    .instructions_fault
                    .clone(),
            )
            .map_err(|failure| failure.map_error(SpawnFailure::from))?;
            let argv = spawn::argv(
                thread_uuid,
                purpose,
                start,
                truncation,
                selection,
                mode,
                &params.cwd,
                &host.mcp_shim,
                instructions.path(),
            );
            let argv = match argv {
                Ok(argv) => argv,
                Err(error) => {
                    return Err(instructions.abort(error).map_error(SpawnFailure::failed));
                }
            };
            Ok((line, translator, instructions, argv, execution))
        }
        .await;
        let (line, mut translator, instructions, argv, execution) =
            process::finish_preparation(&mut driver_slot, prepared)?;
        use crate::provider_execution::{
            self as execution_owner, Purpose, stderr, stdio::EngineStdio,
        };
        use workspace_runtime::execution::ExecutionId;
        let id = ExecutionId::fresh().map_err(execution_owner::conflict)?;
        let dir = stderr::create(&host.instructions_dir, id.as_str())?;
        let prepared = super::super::execution::intent(
            params,
            &id,
            &dir,
            &config.claude_binary,
            argv,
            env,
            Some(token),
        )
        .await;
        let (intent, payload) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                stderr::cleanup(&host.instructions_dir, id.as_str());
                return Err(error.into());
            }
        };
        let owner = &host.provider_execution;
        let purpose_record = match purpose {
            SpawnPurpose::Turn => Purpose::Turn {
                turn_id: turn_id.clone(),
            },
            SpawnPurpose::Compaction => Purpose::Compaction {
                turn_id: turn_id.clone(),
            },
        };
        // Admission can commit before its caller observes the result. Only this
        // boundary discards prior evidence; the intent preparation above is process-free.
        let previous = driver_slot.take();
        let (claim, guard) = match owner
            .admit(
                &pool,
                engine,
                &params.worker_session_id,
                crate::provider_execution::PreparedIntent(intent),
                purpose_record,
            )
            .await
        {
            Ok(admitted) => admitted,
            Err(error) => {
                stderr::cleanup(&host.instructions_dir, id.as_str());
                *driver_slot = previous;
                return Err(error.into());
            }
        };
        drop(previous);
        let mut cancellation = execution_owner::Cancellation {
            owner: owner.clone(),
            pool: pool.clone(),
            engine: engine.clone(),
            session: params.worker_session_id.clone(),
            claim: claim.clone(),
            armed: true,
        };
        if let Err(error) = execution_owner::release::wait_spawned(engine, &id).await {
            return Err(self
                .abort_before_ok(&claim, instructions, error, StopReason::SetupFailed)
                .await
                .0
                .into());
        }
        #[cfg(feature = "fixtures")]
        {
            let hook = shared.hooks.lock().expect("hooks").after_spawn.clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        if params.seals.is_sealed(thread) {
            return Err(self
                .abort_before_ok(
                    &claim,
                    instructions,
                    sealed(thread),
                    StopReason::SetupFailed,
                )
                .await
                .0
                .into());
        }
        if let Err(error) = execution_owner::release::send(
            &pool,
            engine,
            &params.worker_session_id,
            &claim,
            &payload,
            &crate::provider_execution::release::NoAuthority,
        )
        .await
        {
            return Err(self
                .abort_before_ok(&claim, instructions, error, StopReason::ReleaseRefused)
                .await
                .0
                .into());
        }
        let stdin = EngineStdio::new(engine.clone(), id.clone());
        let progress = stdin.write_progress();
        let stdout = EngineStdio::new(engine.clone(), id.clone());
        let slot = Arc::new(TurnSlot::new(stdin, settle_after_stop));
        #[cfg(feature = "fixtures")]
        {
            shared.hooks.lock().expect("hooks").user_line_writes += 1;
        }
        #[cfg(feature = "fixtures")]
        fixtures::prepare_writer(shared, &slot).await;
        if let Err(error) = slot.write_line(&line, admission.gate()).await {
            use provider::claude::line_writer::LineWriteError;
            return Err(match error {
                LineWriteError::BeforeWrite(message) => {
                    let error = admission
                        .take_refusal()
                        .unwrap_or(CalmError::Conflict(message));
                    SpawnFailure::failed(
                        self.abort_before_ok(&claim, instructions, error, StopReason::SetupFailed)
                            .await
                            .0,
                    )
                }
                LineWriteError::CrossedWrite(error) => {
                    let error =
                        super::super::turn_slot::write_error(LineWriteError::CrossedWrite(error));
                    let crossed = progress.crossed();
                    let error = self
                        .abort_before_ok(
                            &claim,
                            instructions,
                            error,
                            if crossed {
                                StopReason::StdinTorn
                            } else {
                                StopReason::SetupFailed
                            },
                        )
                        .await
                        .0;
                    if crossed {
                        SpawnFailure::CrossedWrite { error, local_turn }
                    } else {
                        SpawnFailure::failed(error)
                    }
                }
            });
        }

        {
            // One lock with `finish_turn`'s emit, so this `TurnStarted` never overtakes the
            // previous turn's `TurnCompleted`.
            let mut state = shared.state();
            state.active = Some(ActiveTurn {
                thread_id: thread.to_string(),
                turn_id: turn_id.clone(),
                slot: Arc::clone(&slot),
                finished: FinishedTurn::default(),
            });
            let _ = shared
                .events
                .send(translator.turn_started(crate::model::now_ms()));
        }
        cancellation.armed = false;
        let driver = tokio::spawn(drive(
            Arc::clone(shared),
            TurnRun {
                claim,
                guard,
                stdout,
                slot,
                translator,
                instructions,
                thread: thread_uuid,
                purpose,
                bind: !row_bound,
                version: config.claude_version.clone(),
                approvals: Approvals::for_spawn(mode, held, &turn_id),
            },
        ));
        *driver_slot = Some(execution_end::DriverSlot::new(
            driver,
            thread_uuid,
            local_turn,
            execution,
            id,
        ));
        drop(driver_slot);
        #[cfg(feature = "fixtures")]
        fixtures::after_install(shared).await;
        Ok(local_turn)
    }
}
