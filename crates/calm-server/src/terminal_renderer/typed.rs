//! Typed terminals consume executor output; only the Store projection persists their exit.
use super::*;
use workspace_runtime::execution::{ExecutionId, ExecutionReply, ExecutionState, OutputEnd};

impl TerminalRendererRegistry {
    /// Historical ANSI evidence for the MCP persisted-exit reader. No renderer or PTY copy.
    pub async fn persisted_output(
        &self,
        repo: &dyn crate::db::RouteRepo,
        term: &crate::model::Terminal,
    ) -> anyhow::Result<(String, bool)> {
        if !term.is_typed(repo).await? {
            return Ok((term.pty_output.clone(), term.pty_output_truncated));
        }
        let id = term
            .execution_id(repo)
            .await?
            .ok_or_else(|| anyhow::anyhow!("typed output retired"))?;
        let engine = self
            .execution
            .lock()
            .unwrap()
            .engine()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("execution engine unavailable"))?;
        tokio::time::timeout(workspace_runtime::execution::QUIESCE_DEADLINE, async {
            loop {
                let generation = engine.output_generation().await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
                let (cursor, output, output_end) = match engine.output(&id, None).await.map_err(|e| anyhow::anyhow!("{e:?}"))? {
                    ExecutionReply::Observation { cursor, output, output_end, .. } => (cursor, output, output_end),
                    ExecutionReply::Retired(t) if t.outcome == workspace_runtime::execution::TombstoneOutcome::Revoked => return Ok((String::new(), false)),
                    _ => anyhow::bail!("typed output retired"),
                };
                if !matches!(output_end, OutputEnd::Open) {
                    let capture = output_capture::TerminalOutputCapture::shared(cursor.saturating_sub(output.len() as u64), &output);
                    let mut capture = capture.lock().unwrap();
                    if !matches!(output_end, OutputEnd::Complete { final_cursor } if final_cursor == cursor) { capture.mark_gap(); }
                    let snapshot = capture.snapshot();
                    return Ok((snapshot.text, snapshot.truncated));
                }
                generation.changed().await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
            }
        }).await.map_err(|_| anyhow::anyhow!("terminal output still open"))?
    }

    pub(crate) async fn ensure_typed(
        &self,
        cfg: RendererConfig,
        engine: Arc<crate::execution_boot::ExecutionEngine>,
        id: ExecutionId,
        repo: Arc<dyn crate::db::RouteRepo>,
        geometry_proven: bool,
    ) -> Result<Arc<RendererEntry>, RendererSpawnError> {
        if let Some(entry) = self.get(&cfg.terminal_id) {
            return Ok(entry);
        }
        let render_plane = Arc::new(StdMutex::new(RenderPlane::with_colors(
            cfg.cols,
            cfg.rows,
            cfg.buffer_bytes,
            SCROLLBACK_MAX_LINES,
            Some(cfg.terminal_fg),
            Some(cfg.terminal_bg),
        )));
        let model_view = ModelView::new(cfg.cols, cfg.rows, cfg.terminal_fg, cfg.terminal_bg);
        if !geometry_proven {
            model_view
                .lock()
                .unwrap()
                .invalidate("terminal renderer reattached without complete geometry history");
        }
        render_plane
            .lock()
            .unwrap()
            .install_observer(Box::new(model_view::Observer(model_view.clone())));
        let (event_tx, initial_event_rx) = broadcast::channel(2048);
        let (supervisor_tx, supervisor_rx) = mpsc::unbounded_channel();
        let (exited_tx, exited_rx) = oneshot::channel();
        let (persisted_tx, exit_persisted) = watch::channel(false);
        let exit = Arc::new(StdMutex::new(None));
        let last_output_ms = Arc::new(AtomicI64::new(0));
        let entry = Arc::new(RendererEntry {
            terminal_id: cfg.terminal_id.clone(),
            proc_id: calm_truth::db::sqlite::workspace_execution::terminal_alias(&cfg.terminal_id),
            supervisor_sock: cfg.supervisor_sock.clone(),
            handle: RendererHandle {
                model_view,
                input_barrier: Arc::new(InputBarrier::awaiting_spawn()),
                session_id: Uuid::new_v4(),
                event_rx: event_tx.subscribe(),
                event_tx: event_tx.clone(),
                render_plane: render_plane.clone(),
                owner_registry: Arc::new(StdMutex::new(OwnerRegistry::new())),
                supervisor_tx: supervisor_tx.clone(),
            },
            config: cfg.clone(),
            control_via_engine: true,
            exit: exit.clone(),
            signals: SignalRing::new(),
            last_output_ms: last_output_ms.clone(),
            initial_event_rx: StdMutex::new(Some(initial_event_rx)),
            exited_rx: StdMutex::new(Some(exited_rx)),
            attach_task: StdMutex::new(None),
            exit_persisted,
            tasks: StdMutex::new(Vec::new()),
        });
        // Install once before starting either task: a losing concurrent attach cannot feed bytes twice.
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("renderer registry poisoned"))?;
        if let Some(existing) = entries.get(&cfg.terminal_id) {
            if existing
                .attach_task
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            {
                if let Some(stale) = entries.remove(&cfg.terminal_id) {
                    stale.abort_tasks();
                }
            } else {
                return Ok(existing.clone());
            }
        }
        let reader = spawn_reader(
            engine,
            id,
            cfg.terminal_id.clone(),
            render_plane,
            exit,
            event_tx,
            supervisor_tx,
            exited_tx,
            persisted_tx,
            last_output_ms,
            self.output_wake.clone(),
            repo,
            supervisor_rx,
            TypedLifecycle {
                entry: Arc::downgrade(&entry),
                entries: Arc::downgrade(&self.entries),
            },
        );
        *entry.attach_task.lock().unwrap() = Some(reader);
        entries.insert(cfg.terminal_id, entry.clone());
        Ok(entry)
    }
}

/// The reader owns the complete typed lifecycle, including cancellation of its children.
/// Weak references avoid a registry/task cycle and make removal safe against replacement.
struct TypedLifecycle {
    entry: std::sync::Weak<RendererEntry>,
    entries: std::sync::Weak<StdMutex<HashMap<String, Arc<RendererEntry>>>>,
}
impl Drop for TypedLifecycle {
    fn drop(&mut self) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        for task in entry.tasks.lock().unwrap().iter() {
            task.abort();
        }
        if let Some(entries) = self.entries.upgrade() {
            let mut entries = entries.lock().unwrap();
            if entries
                .get(&entry.terminal_id)
                .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                entries.remove(&entry.terminal_id);
            }
        }
    }
}

async fn before_spawn(
    item: SupervisorControl,
    size: &mut Option<(u16, u16)>,
    engine: &Arc<crate::execution_boot::ExecutionEngine>,
    id: &ExecutionId,
    repo: &Arc<dyn crate::db::RouteRepo>,
) {
    match item {
        SupervisorControl::Resize { cols, rows } if cols > 0 && rows > 0 => {
            *size = Some((cols, rows))
        }
        SupervisorControl::Write(write) => {
            if let Some(ack) = write.ack {
                let _ = ack.send(DaemonMsg::ProtocolError {
                    code: calm_session::ProtocolErrorCode::NotOwner,
                    message: client_pump::PRE_READY_REFUSAL.into(),
                    expected_version: None,
                });
            }
        }
        SupervisorControl::Signal(_) => {
            if let Err(error) = control_writer::request_typed_stop(engine, id, repo).await {
                tracing::warn!(%error, "typed terminal stop request failed");
            }
        }
        SupervisorControl::Resize { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_reader(
    engine: Arc<crate::execution_boot::ExecutionEngine>,
    id: ExecutionId,
    terminal_id: String,
    plane: SharedRenderPlane,
    exit: SharedExitState,
    events: broadcast::Sender<DaemonMsg>,
    controls: mpsc::UnboundedSender<SupervisorControl>,
    exited: oneshot::Sender<Option<i32>>,
    persisted: watch::Sender<bool>,
    stamp: Arc<AtomicI64>,
    wake: OutputWake,
    repo: Arc<dyn crate::db::RouteRepo>,
    mut input: mpsc::UnboundedReceiver<SupervisorControl>,
    lifecycle: TypedLifecycle,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut source = attach_reader::ObservationSource {
            plane: plane.clone(),
            exited: false,
        };
        if let Some(entry) = lifecycle.entry.upgrade() {
            entry
                .tasks
                .lock()
                .unwrap()
                .push(child_ready::spawn_child_ready_poller(
                    plane.clone(),
                    events.clone(),
                ));
        }
        let mut writer = None;
        let mut size = None;
        let mut cursor = 0;
        let mut exited = Some(exited);
        loop {
            // Snapshot before Inspect/Observe: failures retry only after a new generation.
            let generation = match engine.output_generation().await {
                Ok(generation) => generation,
                Err(_) => break,
            };
            match engine.output(&id, Some(cursor)).await {
                Ok(ExecutionReply::Observation {
                    state,
                    receipt,
                    cursor: next,
                    output,
                    output_end,
                }) => {
                    if next.saturating_sub(output.len() as u64) != cursor {
                        plane
                            .lock()
                            .unwrap()
                            .invalidate_observation("terminal output gap");
                    }
                    cursor = next;
                    if !output.is_empty() {
                        record_output(&stamp, &wake, &terminal_id);
                        let effects = plane.lock().unwrap().on_pty_chunk(output);
                        client_pump::apply_broadcaster_effects(&events, &controls, effects);
                    }
                    if writer.is_none()
                        && receipt.is_some()
                        && matches!(state, ExecutionState::Spawned)
                    {
                        // Consume everything accepted before readiness, retaining only the latest
                        // geometry. Queue it first, so subsequent input cannot overtake it.
                        while let Ok(item) = input.try_recv() {
                            before_spawn(item, &mut size, &engine, &id, &repo).await;
                        }
                        let (tx, rx) = mpsc::unbounded_channel();
                        if let Some((cols, rows)) = size.take() {
                            let _ = tx.send(SupervisorControl::Resize { cols, rows });
                        }
                        let task = control_writer::spawn_typed_control_writer(
                            engine.clone(),
                            id.clone(),
                            repo.clone(),
                            rx,
                        );
                        if let Some(entry) = lifecycle.entry.upgrade() {
                            entry.tasks.lock().unwrap().push(task);
                            entry.handle.input_barrier.spawned();
                            let _ = events.send(DaemonMsg::OwnerChanged {
                                owner_client_id: entry
                                    .handle
                                    .owner_registry
                                    .lock()
                                    .unwrap()
                                    .current_owner(),
                            });
                        } else {
                            task.abort();
                            return;
                        }
                        writer = Some(tx);
                    }
                    // Publish final bytes/exit once, but keep watching until authoritative Retired.
                    if exited.is_some()
                        && state.permits_handoff()
                        && !matches!(output_end, OutputEnd::Open)
                    {
                        if !matches!(output_end, OutputEnd::Complete { final_cursor } if final_cursor == cursor)
                        {
                            plane
                                .lock()
                                .unwrap()
                                .invalidate_observation("terminal output incomplete");
                        }
                        let code = match state {
                            ExecutionState::Drained { status } => status,
                            _ => None,
                        };
                        publish_exit(&plane, &exit, &events, &controls, code);
                        source.exited = true;
                        let _ = exited.take().unwrap().send(code);
                        if engine.wait_settled(&id).await.is_ok() {
                            let _ = persisted.send(true);
                        }
                        wake_projector(&wake, &terminal_id);
                    }
                }
                Ok(ExecutionReply::Retired(tombstone)) => {
                    if exited.is_some()
                        && tombstone.outcome
                            == workspace_runtime::execution::TombstoneOutcome::Revoked
                    {
                        publish_exit(&plane, &exit, &events, &controls, None);
                        source.exited = true;
                        let _ = exited.take().unwrap().send(None);
                        if engine.wait_settled(&id).await.is_ok() {
                            let _ = persisted.send(true);
                        }
                        wake_projector(&wake, &terminal_id);
                    }
                    return; // TypedLifecycle removes this entry and ends its children.
                }
                Err(workspace_runtime::execution::DriveError::Executor(
                    workspace_runtime::execution::ExecError::Refused(_)
                    | workspace_runtime::execution::ExecError::Transport(_),
                )) => {} // Read-only failures cannot create an uncertain write or advance cursor.
                other => {
                    tracing::warn!(?other, %terminal_id, "typed renderer output unavailable");
                    break;
                }
            }
            // Control traffic does not cause extra Inspect calls or replace the generation fence.
            let changed = generation.changed();
            tokio::pin!(changed);
            loop {
                tokio::select! {
                    result = &mut changed => {
                        if result.is_err() { return; }
                        break;
                    }
                    _ = async {
                        match &writer {
                            Some(writer) => writer.closed().await,
                            None => std::future::pending().await,
                        }
                    }, if !input.is_closed() => input.close(),
                    Some(item) = input.recv() => {
                        if let Some(writer) = &writer {
                            if writer.send(item).is_err() { input.close(); }
                        } else {
                            before_spawn(item, &mut size, &engine, &id, &repo).await;
                        }
                    }
                }
            }
        }
        plane
            .lock()
            .unwrap()
            .invalidate_observation("terminal output source disconnected");
    })
}

fn publish_exit(
    plane: &SharedRenderPlane,
    exit: &SharedExitState,
    events: &broadcast::Sender<DaemonMsg>,
    controls: &mpsc::UnboundedSender<SupervisorControl>,
    code: Option<i32>,
) {
    let effects = {
        let mut rp = plane.lock().unwrap();
        let effects = rp.on_child_exit(code);
        *exit.lock().unwrap() = Some(TerminalExitInfo {
            code,
            pty_seq: rp.pty_seq(),
            render_rev: rp.render_rev(),
            exited_at: std::time::SystemTime::now(),
        });
        effects
    };
    client_pump::apply_broadcaster_effects(events, controls, effects);
}
