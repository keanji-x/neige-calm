//! Interactive native clients are session executions, owned by the manager.
//! The backend consumes a write capability and returns supervisor evidence.
use super::super::backend::{Backend, LaunchOutcome, Observation};
use super::super::{LaunchPermit, Record};
use crate::error::{CalmError, Result};
use crate::operation::task_launch::TaskLaunch;
use crate::terminal_renderer::{RendererConfig, TerminalRendererRegistry};
use calm_session::control::{ControlMsg, ControlReply};
use calm_session::{read_frame, write_frame};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixStream;

struct NativeSessionBackend {
    renderer: Option<Arc<TerminalRendererRegistry>>,
    supervisor: PathBuf,
    remote: Option<RemoteStopProof>,
    unmanaged_client: Arc<std::sync::atomic::AtomicBool>,
}
/// Constructed only after the manager settles its durably closed session group.
struct RemoteStopProof {
    execution: String,
    client_never_issued: bool,
}
struct SessionRequest {
    config: RendererConfig,
    task: Option<TaskLaunch>,
}

#[async_trait::async_trait]
impl Backend for NativeSessionBackend {
    type Request = SessionRequest;
    type Output = ();
    fn kind(&self) -> super::super::BackendKind {
        super::super::BackendKind::NativeSession
    }

    async fn launch(&self, permit: LaunchPermit, request: SessionRequest) -> LaunchOutcome<()> {
        let LaunchPermit::Write(permit) = permit else {
            return LaunchOutcome::NotIssued(CalmError::Conflict(
                "interactive native sessions require write authority".into(),
            ));
        };
        let Some(renderer) = self.renderer.as_ref() else {
            return LaunchOutcome::NotIssued(CalmError::Conflict(
                "session launch renderer unavailable".into(),
            ));
        };
        let terminal = permit.record.holder.clone();
        let paths = std::fs::canonicalize(&request.config.cwd).and_then(|cwd| {
            std::fs::canonicalize(&permit.record.cwd).map(|reserved| cwd == reserved)
        });
        if request.config.terminal_id != terminal
            || request.config.supervisor_sock != self.supervisor
            || !matches!(paths, Ok(true))
        {
            return LaunchOutcome::NotIssued(CalmError::Conflict(
                "interactive native launch differs from its reserved scope".into(),
            ));
        }
        let expected = request.config.clone();
        match renderer
            .ensure_for_native_session(request.config, request.task, permit)
            .await
        {
            Ok(entry)
                if entry.config().program == expected.program
                    && entry.config().args == expected.args
                    && entry.config().cwd == expected.cwd
                    && entry.config().supervisor_sock == expected.supervisor_sock =>
            {
                LaunchOutcome::Started {
                    identity: terminal,
                    output: (),
                }
            }
            Ok(_) => {
                self.unmanaged_client
                    .store(true, std::sync::atomic::Ordering::Release);
                LaunchOutcome::Uncertain(CalmError::Conflict(
                    "native client differs from its frozen managed launch".into(),
                ))
            }
            // An interrupted renderer establishment does not prove EnsureProc was never sent.
            Err(error) => LaunchOutcome::Uncertain(CalmError::Internal(error.to_string())),
        }
    }

    async fn recover(&self, record: &Record) -> Result<Observation> {
        if record.phase == "stopping" {
            return self.stop(record).await;
        }
        let mut stream = UnixStream::connect(&self.supervisor).await?;
        write_frame(
            &mut stream,
            &ControlMsg::Probe(calm_session::control::ProbeRequest {
                proc_id: format!("term:{}", record.holder),
            }),
        )
        .await
        .map_err(|error| CalmError::Conflict(error.to_string()))?;
        match read_frame::<ControlReply, _>(&mut stream)
            .await
            .map_err(|error| CalmError::Conflict(error.to_string()))?
        {
            ControlReply::ProbeOk {
                supervisor_version,
                proc_running: false,
            } if supervisor_version == calm_session::SUPERVISOR_CONTROL_VERSION => {
                self.stop(record).await
            }
            _ => Ok(Observation {
                execution: record.id.clone(),
                identity: None,
                stopped: false,
            }),
        }
    }

    async fn stop(&self, record: &Record) -> Result<Observation> {
        let client_never_issued = self
            .remote
            .as_ref()
            .is_some_and(|proof| proof.execution == record.id && proof.client_never_issued);
        if !client_never_issued {
            crate::terminal_renderer::confirm_terminal_stopped(&self.supervisor, &record.holder)
                .await?;
        }
        Ok(Observation {
            execution: record.id.clone(),
            identity: Some(record.holder.clone()),
            stopped: self
                .remote
                .as_ref()
                .is_some_and(|proof| proof.execution == record.id),
        })
    }
}

/// Preparation is in the owning operation transaction, after its task reservation.
/// The task's frozen reservation determines access; presentation never grants it.
pub(crate) async fn session_preparation_tx(
    tx: &mut crate::operation::Tx<'_>,
    card: &str,
    track: &str,
    terminal: &str,
    cwd: &str,
    operation: &crate::operation::Operation,
) -> Result<calm_types::worker_presentation::WorkerPresentation> {
    use calm_types::worker_presentation::WorkerPresentation;
    let operation_valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND lease_owner=?2 \
         AND kind IN ('codex-create','codex-worker') AND phase='pending')",
    )
    .bind(&operation.id)
    .bind(&operation.lease_owner)
    .fetch_one(&mut **tx)
    .await?;
    if !operation_valid {
        return Err(CalmError::Conflict(
            "native session preparation lost its operation owner".into(),
        ));
    }
    let terminal_valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM terminals terminal JOIN cards card ON card.id=terminal.card_id \
         WHERE terminal.id=?1 AND card.id=?2 AND card.track_id=?3 AND terminal.cwd=?4)"
    ).bind(terminal).bind(card).bind(track).bind(cwd).fetch_one(&mut **tx).await?;
    if !terminal_valid {
        return Err(CalmError::Conflict(
            "native session metadata differs from its prepared owner".into(),
        ));
    }
    if operation.kind == "codex-worker" {
        let access: Option<String> = sqlx::query_scalar(
            "SELECT access_mode FROM workspace_leases WHERE holder_kind='task' AND card_id=?1 \
             AND lease_owner=?2 AND state='held' AND path=?3",
        )
        .bind(card)
        .bind(&operation.id)
        .bind(cwd)
        .fetch_optional(&mut **tx)
        .await?;
        match access.as_deref() {
            Some("read_only") => return Ok(WorkerPresentation::NativeOnly),
            Some("read_write") => {}
            _ => {
                return Err(CalmError::Conflict(
                    "native session task reservation is missing".into(),
                ));
            }
        }
    }
    crate::operation::workspace_lease::execution_guard::acquire_execution_write_tx(
        tx,
        track,
        card,
        terminal,
        "terminal",
        std::path::Path::new(cwd),
    )
    .await?;
    Ok(WorkerPresentation::InteractiveTui {
        terminal_id: terminal.to_owned(),
    })
}

/// Every renderer entry, including a surviving in-memory renderer, checks the
/// manager's frozen session plan before handing an interactive channel to UI.
pub(crate) async fn authorize_native_session_tx(
    tx: &mut crate::operation::Tx<'_>,
    terminal: &str,
    permit: Option<&super::super::WritePermit>,
) -> Result<bool> {
    let codex: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM terminals terminal JOIN cards card ON card.id=terminal.card_id \
         JOIN operations operation ON json_extract(operation.tx_output_json,'$.data.card_id')=card.id \
         WHERE terminal.id=?1 AND operation.kind IN ('codex-create','codex-worker'))"
    ).bind(terminal).fetch_one(&mut **tx).await?;
    if !codex {
        return Ok(false);
    }
    let held: Option<(String, String, String)> = sqlx::query_as(
        "SELECT lease.lease_id,lease.holder_phase,lease.path FROM workspace_leases lease \
         JOIN terminals terminal ON terminal.id=lease.holder_id AND terminal.card_id=lease.card_id \
         JOIN native_session_ingresses ingress ON ingress.session_execution_id=lease.lease_id \
         AND ingress.card_id=lease.card_id AND ingress.terminal_id=terminal.id \
         WHERE terminal.id=?1 AND lease.holder_kind='terminal' AND lease.state='held' \
         AND lease.access_mode='read_write'",
    )
    .bind(terminal)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((execution, phase, cwd)) = held else {
        return Err(CalmError::Conflict(
            "native interactive session has no managed write authority".into(),
        ));
    };
    if phase == "stopping" {
        return Err(CalmError::Conflict(
            "native interactive session is stopping".into(),
        ));
    }
    if let Some(permit) = permit
        && (permit.record.id != execution
            || permit.record.holder != terminal
            || permit.record.cwd != cwd)
    {
        return Err(CalmError::Conflict(
            "native session capability differs from its durable owner".into(),
        ));
    }
    Ok(true)
}

impl super::super::ExecutionManager {
    pub(crate) async fn prepare_native_session_command(
        &self,
        terminal: &crate::model::Terminal,
        shared: &Arc<super::SharedCodexAppServer>,
        thread: Option<&str>,
    ) -> Result<(String, String)> {
        let execution: String = sqlx::query_scalar(
            "SELECT lease_id FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1 \
             AND state='held' AND holder_phase='issuing'",
        )
        .bind(&terminal.id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| CalmError::Conflict("native session is not prepared for launch".into()))?;
        let record = super::super::storage::load(&self.pool, &execution)
            .await?
            .ok_or_else(|| CalmError::Conflict("native session disappeared".into()))?;
        let managed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM native_session_ingresses WHERE session_execution_id=?1)",
        )
        .bind(&execution)
        .fetch_one(&self.pool)
        .await?;
        if terminal.pid.is_some() && !managed {
            super::super::storage::request_stop(&self.pool, &execution).await?;
            return Err(CalmError::Conflict(
                "pre-existing native client has no frozen ingress; stopped recovery required"
                    .into(),
            ));
        }
        let uri = super::ingress::prepare(self, &record, shared.clone()).await?;
        let quote = crate::routes::codex_cards::shell_single_quote;
        let command = match thread {
            Some(thread) => format!("codex resume {} --remote {}", quote(thread), quote(&uri)),
            None => format!("codex --remote {}", quote(&uri)),
        };
        Ok((execution, command))
    }
    pub(crate) async fn launch_native_session(
        &self,
        ctx: &crate::operation::SpawnCtx,
        terminal: &crate::model::Terminal,
        shared: &Arc<super::SharedCodexAppServer>,
        thread: Option<&str>,
        env: &serde_json::Value,
        task: Option<TaskLaunch>,
    ) -> Result<crate::operation::SpawnHandle> {
        if ctx.terminal_renderer.get(&terminal.id).is_some() {
            let checkpoint: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM native_session_ingresses ingress JOIN workspace_leases lease \
                 ON lease.lease_id=ingress.session_execution_id WHERE ingress.terminal_id=?1 AND lease.state='held')"
            ).bind(&terminal.id).fetch_one(&self.pool).await?;
            if !checkpoint {
                sqlx::query("UPDATE workspace_leases SET holder_phase='stopping' WHERE holder_kind='terminal' AND holder_id=?1 AND state='held'")
                    .bind(&terminal.id).execute(&self.pool).await?;
                return Err(CalmError::Conflict(
                    "surviving native client cannot acquire a new ingress by attachment".into(),
                ));
            }
        }
        let (execution, command) = self
            .prepare_native_session_command(terminal, shared, thread)
            .await?;
        let config = crate::routes::terminal::terminal_renderer_config(
            ctx.daemon.as_ref(),
            terminal,
            &command,
            &terminal.cwd,
            env,
        )
        .await?;
        let backend = NativeSessionBackend {
            renderer: Some(ctx.terminal_renderer.clone()),
            supervisor: config.supervisor_sock.clone(),
            remote: None,
            unmanaged_client: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let launched = self
            .launch_reserved(&backend, &execution, SessionRequest { config, task })
            .await;
        if backend
            .unmanaged_client
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let mut tx = crate::db::sqlite::begin_immediate_tx(&self.pool).await?;
            super::super::storage::request_stop_tx(&mut tx, &execution).await?;
            sqlx::query("INSERT INTO native_session_client_observations(session_execution_id,proof) \
                VALUES(?1,'unmanaged-client') ON CONFLICT(session_execution_id) DO UPDATE SET proof='unmanaged-client'")
                .bind(&execution).execute(&mut *tx).await?;
            tx.commit().await?;
            if let Some((socket,)) = sqlx::query_as::<_, (String,)>(
                "SELECT socket_path FROM native_session_ingresses WHERE session_execution_id=?1",
            )
            .bind(&execution)
            .fetch_optional(&self.pool)
            .await?
            {
                super::ingress::quiesce(std::path::Path::new(&socket)).await?;
            }
        }
        launched?;
        Ok(crate::operation::SpawnHandle::Terminal {
            terminal_id: terminal.id.clone(),
            renderer_id: terminal.id.clone(),
        })
    }
}

/// Read presentation is derived from its immutable task lease, never mutable card JSON.
pub(crate) async fn native_session_presentation(
    pool: &sqlx::SqlitePool,
    terminal: &str,
) -> Result<calm_types::worker_presentation::WorkerPresentation> {
    use calm_types::worker_presentation::WorkerPresentation;
    let read: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM terminals terminal JOIN workspace_leases task ON task.card_id=terminal.card_id \
         JOIN operations operation ON operation.id=task.lease_owner \
         WHERE terminal.id=?1 AND task.holder_kind='task' AND task.access_mode='read_only' \
         AND operation.kind='codex-worker')"
    ).bind(terminal).fetch_one(pool).await?;
    Ok(if read {
        WorkerPresentation::NativeOnly
    } else {
        WorkerPresentation::InteractiveTui {
            terminal_id: terminal.to_owned(),
        }
    })
}

/// Terminal cleanup delegates managed native sessions to this manager entry.
/// `None` identifies a different backend and grants no native release authority.
pub(crate) async fn stop_managed_native_session(
    repo: &dyn crate::db::RouteRepo,
    supervisor: &std::path::Path,
    terminal: &str,
) -> Result<Option<bool>> {
    let terminal = terminal.to_owned();
    let supervisor = supervisor.to_owned();
    let socket = supervisor.clone();
    let record = crate::db::write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let execution: Option<String> = sqlx::query_scalar(
            "SELECT lease.lease_id FROM workspace_leases lease JOIN operations operation \
             ON json_extract(operation.tx_output_json,'$.data.terminal_id')=lease.holder_id \
             AND json_extract(operation.tx_output_json,'$.data.card_id')=lease.card_id \
             WHERE lease.holder_kind='terminal' AND lease.holder_id=?1 \
             AND operation.kind IN ('codex-create','codex-worker') ORDER BY lease.created_at_ms DESC LIMIT 1"
        ).bind(&terminal).fetch_optional(&mut **tx).await?;
        let Some(execution) = execution else { return Ok(None); };
        let Some(record) = super::super::storage::load_in(tx, &execution).await? else { return Ok(Some(None)); };
        let output: String = sqlx::query_scalar(
            "SELECT tx_output_json FROM operations WHERE json_extract(tx_output_json,'$.data.terminal_id')=?1 \
             AND kind IN ('codex-create','codex-worker')"
        ).bind(&terminal).fetch_one(&mut **tx).await?;
        let output: serde_json::Value = serde_json::from_str(&output)?;
        if let Some(state) = crate::operation::terminal_launch::RequestState::read(&output["data"])? {
            match state {
                crate::operation::terminal_launch::RequestState::Requested { supervisor_sock, .. }
                | crate::operation::terminal_launch::RequestState::HandedOff { supervisor_sock, .. }
                | crate::operation::terminal_launch::RequestState::Stopped { supervisor_sock, .. }
                    if supervisor_sock != socket => return Err(CalmError::Conflict("native session stop endpoint changed".into())),
                _ => {}
            }
        }
        // No launch capability exists before claim_session changes this phase.
        // This transaction fences a concurrent claim before confirming never-issued.
        if record.phase == "issuing" {
            super::super::storage::release_confirmed_tx(tx, &record, &record.holder, crate::model::now_ms()).await?;
            super::super::storage::session_stop_record_tx(tx, &record, &socket).await?;
            return Ok(Some(None));
        }
        sqlx::query("UPDATE workspace_leases SET holder_phase='stopping' WHERE lease_id=?1 AND state='held'")
            .bind(&execution).execute(&mut **tx).await?;
        Ok(Some(Some(record)))
    })).await?;
    let Some(record) = record else {
        return Ok(None);
    };
    let Some(record) = record else {
        return Ok(Some(true));
    };
    let remote = settle_session_remote(repo, &supervisor, &record).await?;
    let backend = NativeSessionBackend {
        renderer: None,
        supervisor: supervisor.clone(),
        remote: Some(remote),
        unmanaged_client: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let observation = backend.stop(&record).await?;
    if !observation.stopped {
        return Ok(Some(false));
    }
    let identity = observation
        .identity
        .ok_or_else(|| CalmError::Conflict("native session stopped without identity".into()))?;
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            require_session_settled_tx(tx, &record.id).await?;
            let released = super::super::storage::release_confirmed_tx(
                tx,
                &record,
                &identity,
                crate::model::now_ms(),
            )
            .await?;
            if released {
                super::super::storage::session_stop_record_tx(tx, &record, &supervisor).await?;
            }
            Ok(Some(true))
        })
    })
    .await
}

async fn require_session_settled_tx(
    tx: &mut crate::operation::Tx<'_>,
    session: &str,
) -> Result<()> {
    let settled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM native_session_ingresses ingress JOIN workspace_leases parent \
         ON parent.lease_id=ingress.session_execution_id WHERE parent.lease_id=?1 \
         AND parent.state='held' AND parent.holder_phase='stopping' \
         AND NOT EXISTS(SELECT 1 FROM native_session_executions relation JOIN workspace_leases native \
         ON native.lease_id=relation.execution_id WHERE relation.session_execution_id=?1 \
         AND (native.state<>'released' OR native.holder_phase IS NOT 'stopped')))"
    ).bind(session).fetch_one(&mut **tx).await?;
    if !settled {
        return Err(CalmError::Conflict(
            "native session remote stop remains unconfirmed".into(),
        ));
    }
    Ok(())
}
async fn settle_session_remote(
    repo: &dyn crate::db::RouteRepo,
    supervisor: &std::path::Path,
    record: &Record,
) -> Result<RemoteStopProof> {
    let execution = record.id.clone();
    let (socket, provider, records, never_issued, unmanaged) = crate::db::write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let (socket, provider): (String, String) = sqlx::query_as(
            "SELECT socket_path,provider_socket_path FROM native_session_ingresses WHERE session_execution_id=?1"
        ).bind(&execution).fetch_optional(&mut **tx).await?
            .ok_or_else(|| CalmError::Conflict("legacy native session has no bounded remote ingress; lease retained".into()))?;
        let unmanaged: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM native_session_client_observations \
            WHERE session_execution_id=?1 AND proof='unmanaged-client')")
            .bind(&execution).fetch_one(&mut **tx).await?;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT relation.execution_id FROM native_session_executions relation JOIN workspace_leases native \
             ON native.lease_id=relation.execution_id WHERE relation.session_execution_id=?1 AND native.state='held'"
        ).bind(&execution).fetch_all(&mut **tx).await?;
        let mut records = Vec::new();
        for id in ids {
            let Some(record) = super::super::storage::load_in(tx, &id).await? else {
                return Err(CalmError::Conflict("native session lost its execution record".into()));
            };
            super::super::storage::request_stop_tx(tx, &id).await?;
            records.push(record);
        }
        let never_issued: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM native_session_client_observations WHERE session_execution_id=?1 AND proof='not-issued')"
        ).bind(&execution).fetch_one(&mut **tx).await?;
        Ok((PathBuf::from(socket), PathBuf::from(provider), records, never_issued, unmanaged))
    })).await?;
    super::ingress::quiesce(&socket).await?;
    if !never_issued {
        crate::terminal_renderer::confirm_terminal_stopped(supervisor, &record.holder).await?;
    }
    if unmanaged {
        return Err(CalmError::Conflict(
            "unmanaged native client has no complete remote stop proof; lease retained".into(),
        ));
    }
    if !records.is_empty() {
        let (client, _notifications) = super::wire::CodexAppServer::connect(provider).await?;
        client
            .initialize(super::wire::ClientInfo {
                name: "neige-session-stop".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            })
            .await?;
        let backend =
            super::supervisor::execution_backend::CodexBackend::for_ingress(Arc::new(client));
        for native in records {
            let observation = backend.stop(&native).await?;
            if observation.execution != native.id || !observation.stopped {
                return Err(CalmError::Conflict(
                    "native session remote execution remains unconfirmed".into(),
                ));
            }
            let identity = observation.identity.ok_or_else(|| {
                CalmError::Conflict("native session stop lacks execution identity".into())
            })?;
            crate::db::write_in_tx_typed(repo, move |tx| {
                Box::pin(async move {
                    super::super::storage::observe_tx(tx, &native, &identity).await?;
                    super::super::storage::release_confirmed_tx(
                        tx,
                        &native,
                        &identity,
                        crate::model::now_ms(),
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;
        }
    }
    let execution = record.id.clone();
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { require_session_settled_tx(tx, &execution).await })
    })
    .await?;
    Ok(RemoteStopProof {
        execution: record.id.clone(),
        client_never_issued: never_issued,
    })
}

/// Cancel an unclaimed session without a process endpoint. No launch permit can exist yet.
pub(crate) async fn release_unissued_native_session(
    repo: &dyn crate::db::RouteRepo,
    terminal: &str,
) -> Result<Option<bool>> {
    let terminal = terminal.to_owned();
    crate::db::write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let execution: Option<String> = sqlx::query_scalar(
            "SELECT lease.lease_id FROM workspace_leases lease JOIN operations operation \
             ON json_extract(operation.tx_output_json,'$.data.terminal_id')=lease.holder_id \
             AND json_extract(operation.tx_output_json,'$.data.card_id')=lease.card_id \
             WHERE lease.holder_kind='terminal' AND lease.holder_id=?1 \
             AND operation.kind IN ('codex-create','codex-worker') ORDER BY lease.created_at_ms DESC LIMIT 1",
        ).bind(&terminal).fetch_optional(&mut **tx).await?;
        let Some(execution) = execution else { return Ok(None); };
        let Some(record) = super::super::storage::load_in(tx, &execution).await? else { return Ok(Some(true)); };
        let never_issued: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM native_session_client_observations \
             WHERE session_execution_id=?1 AND proof='not-issued')"
        ).bind(&record.id).fetch_one(&mut **tx).await?;
        if record.phase != "issuing" && !never_issued { return Ok(Some(false)); }
        let unsettled: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM native_session_executions relation JOIN workspace_leases child \
             ON child.lease_id=relation.execution_id WHERE relation.session_execution_id=?1 \
             AND (child.state<>'released' OR child.holder_phase IS NOT 'stopped'))"
        ).bind(&record.id).fetch_one(&mut **tx).await?;
        if unsettled { return Ok(Some(false)); }
        super::super::storage::release_confirmed_tx(tx, &record, &record.holder, crate::model::now_ms()).await?;
        Ok(Some(true))
    })).await
}
