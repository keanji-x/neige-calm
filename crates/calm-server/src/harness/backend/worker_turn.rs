//! Bound TaskWorker protocol control used by the terminal-free task adapter.
use super::*;
use crate::acp_planner::session::{AcpPlannerSession, SessionParams};

/// One worker's native session; callers persist attribution before issuing its turn.
pub(crate) struct WorkerTurn {
    backend: PlannerBackend,
    thread: String,
    // No consumer: Full turns must never enqueue a human approval.
    _held: Option<crate::harness::held_requests::HeldRequestReceiver>,
}

impl WorkerTurn {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_codex(
        daemon: Arc<SharedCodexAppServer>,
        card_id: &str,
        track_id: &str,
        worker_session_id: &str,
        cwd: String,
        instructions: String,
        socket: std::path::PathBuf,
        token: String,
    ) -> Result<(Self, crate::shared_codex_appserver::MintedThread)> {
        let backend = PlannerBackend::codex(daemon.clone(), card_id, track_id, worker_session_id)?;
        let minted = daemon
            .thread_start_mint_mcp_shell(card_id, cwd, Some(instructions), socket, token)
            .await?;
        Ok((
            Self {
                backend,
                thread: minted.thread_id.clone(),
                _held: None,
            },
            minted,
        ))
    }

    /// `params.cwd` is the task's admitted checkout, never the daemon's cwd.
    pub(crate) fn open_acp(params: SessionParams, thread: String) -> Self {
        let session = Arc::new(AcpPlannerSession::open(params));
        let (held, receiver) = crate::harness::held_requests::channel();
        session.mark_installed(held);
        Self {
            backend: PlannerBackend(Arm::Acp(session)),
            thread,
            _held: Some(receiver),
        }
    }

    /// Codex's shared notification bus outlives a daemon process; ACP emits a terminal event on exit.
    pub(crate) fn source_available(&self) -> bool {
        match &self.backend.0 {
            Arm::Codex(daemon, _) => daemon.is_running_per_readiness(),
            Arm::Acp(_) => true,
            Arm::Claude(_) => unreachable!("TaskWorker has no Claude constructor"),
        }
    }

    pub(crate) fn subscribe_events(&self) -> PlannerEvents {
        self.backend.subscribe_events()
    }

    pub(crate) async fn start(
        &self,
        input: InputRequest,
        selection: &TurnModelSelection,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> std::result::Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
        match &self.backend.0 {
            Arm::Codex(daemon, _) => {
                track_fence.release();
                codex_start_delivery(
                    daemon
                        .turn_start_control(
                            &self.thread,
                            input.items,
                            selection,
                            TurnApprovals::Unchanged,
                            Some(&input.correlation.0),
                        )
                        .await,
                )
            }
            Arm::Acp(session) => {
                session
                    .start_turn(
                        &self.thread,
                        TurnRequest {
                            input,
                            selection: selection.clone(),
                            permission: PlannerPermissionMode::Full,
                            edit: TurnEdit::NoEdit,
                        },
                        &[],
                        track_fence,
                    )
                    .await
            }
            Arm::Claude(_) => unreachable!("TaskWorker has no Claude constructor"),
        }
    }

    pub(crate) async fn interrupt(&self, turn: &str) -> InterruptOutcome<InterruptDiagnostic> {
        match &self.backend.0 {
            Arm::Codex(daemon, _) => {
                InterruptOutcome::from(daemon.interrupt_outcome(&self.thread, turn).await)
                    .map_error(|e| Box::new(e) as InterruptDiagnostic)
            }
            Arm::Acp(session) => session
                .turn_interrupt(&self.thread, turn)
                .await
                .map_error(|e| Box::new(e) as InterruptDiagnostic),
            Arm::Claude(_) => unreachable!("TaskWorker has no Claude constructor"),
        }
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::shared_codex_appserver::native_owner_tests::{
        connect, notify_thread, request, send,
    };
    use serde_json::json;

    #[tokio::test]
    async fn task_worker_codex_mints_starts_observes_and_interrupts() {
        let dir = tempfile::tempdir().unwrap();
        let owner = super::super::dispatch_fixture::seed(dir.path(), AgentProvider::Codex).await;
        let daemon = SharedCodexAppServer::new_stub(owner.repo.clone());
        let (_client, mut peer) = connect(&daemon).await;
        let open = WorkerTurn::open_codex(
            daemon.clone(),
            &owner.card,
            &owner.track,
            &owner.worker,
            dir.path().to_string_lossy().into_owned(),
            "worker instructions".into(),
            dir.path().join("mcp.sock"),
            "task-token".into(),
        );
        tokio::pin!(open);
        let serve = async {
            let call = request(&mut peer).await;
            assert_eq!(call["method"], "thread/start");
            assert_eq!(call["params"]["approvalPolicy"], "never");
            assert_eq!(
                call["params"]["developerInstructions"],
                "worker instructions"
            );
            assert_eq!(call["params"]["cwd"], dir.path().to_string_lossy().as_ref());
            send(&mut peer, json!({"id":call["id"],"result":{"thread":{"id":"worker-thread"},"model":"resolved"}})).await;
        };
        let (opened, ()) = tokio::join!(&mut open, serve);
        let (worker, minted) = opened.unwrap();
        assert_eq!(minted.thread_id, "worker-thread");
        assert_eq!(minted.resolved_model.as_deref(), Some("resolved"));
        let mut events = worker.subscribe_events();
        let selection = TurnModelSelection {
            model: Some("chosen".into()),
            effort: Some("high".into()),
        };
        let fence = worker.backend.handoff_for_test().await;
        let start = worker.start(
            InputRequest {
                items: vec![InputItem::text("work")],
                correlation: CorrelationKey("attempt".into()),
            },
            &selection,
            &fence,
        );
        let serve = async {
            let call = request(&mut peer).await;
            assert_eq!(call["method"], "turn/start");
            assert_eq!(call["params"]["model"], "chosen");
            assert_eq!(call["params"]["effort"], "high");
            assert!(call["params"].get("approvalPolicy").is_none());
            send(
                &mut peer,
                json!({"id":call["id"],"result":{"turn":{"id":"worker-turn"}}}),
            )
            .await;
        };
        let (started, ()) = tokio::join!(start, serve);
        assert!(
            matches!(started.unwrap(), TurnStartOutcome::Started { turn } if turn.as_str() == "worker-turn")
        );
        notify_thread(
            &daemon,
            &mut peer,
            "worker-thread",
            "turn/started",
            json!({"id":"worker-turn"}),
        )
        .await;
        let event = events.recv().await.unwrap();
        assert_eq!(event.thread_id.as_deref(), Some("worker-thread"));
        let interrupt = worker.interrupt("worker-turn");
        let serve = async {
            let call = request(&mut peer).await;
            assert_eq!(call["method"], "turn/interrupt");
            assert_eq!(call["params"]["threadId"], "worker-thread");
            assert_eq!(call["params"]["turnId"], "worker-turn");
            send(&mut peer, json!({"id":call["id"],"result":{}})).await;
        };
        let (interrupted, ()) = tokio::join!(interrupt, serve);
        assert!(matches!(interrupted, InterruptOutcome::NativeAck));
    }
}
