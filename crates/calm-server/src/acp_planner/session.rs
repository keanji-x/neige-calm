use super::{config::AcpPlannerHost, process::Process};
use crate::db::sqlite::{
    AcpSubmission, acp_submission_finish, acp_submission_get, acp_submission_prepare,
    acp_submission_unresolved,
};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::harness::backend::TurnStartFailure;
use crate::thread_seals::ThreadSeals;
use calm_types::runtime::AgentProvider;
use calm_types::worker::{WorkerContract, WorkerProviderKind, WorkerSessionId};
use provider::acp::{
    Incoming, protocol,
    translate::{TurnContext, TurnTranslator},
};
use provider::events::{PlannerEvent, PlannerEventKind};
use provider::{InputItem, TurnModelSelection};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, watch};

pub struct SessionParams {
    pub host: Arc<AcpPlannerHost>,
    pub provider: AgentProvider,
    pub repo: Arc<dyn Repo>,
    pub seals: Arc<ThreadSeals>,
    pub worker_session_id: String,
    pub card_id: String,
    pub track_id: String,
    pub cwd: std::path::PathBuf,
    pub instructions: String,
}
struct Active {
    thread: String,
    turn: String,
    cancel: watch::Sender<bool>,
    done: watch::Sender<bool>,
}
struct State {
    active: Option<Active>,
    closed: bool,
}
struct Shared {
    params: SessionParams,
    events: broadcast::Sender<PlannerEvent>,
    state: Mutex<State>,
    issue: tokio::sync::Mutex<()>,
    installed: AtomicBool,
}
pub struct AcpPlannerSession {
    shared: Arc<Shared>,
}
impl AcpPlannerSession {
    pub fn open(params: SessionParams) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            shared: Arc::new(Shared {
                params,
                events,
                state: Mutex::new(State {
                    active: None,
                    closed: false,
                }),
                issue: tokio::sync::Mutex::new(()),
                installed: AtomicBool::new(false),
            }),
        }
    }
    pub fn provider(&self) -> AgentProvider {
        self.shared.params.provider.clone()
    }
    pub fn host(&self) -> &AcpPlannerHost {
        &self.shared.params.host
    }
    pub fn subscribe_events(&self) -> broadcast::Receiver<PlannerEvent> {
        self.shared.events.subscribe()
    }
    pub fn mark_installed(&self) {
        self.shared.installed.store(true, Ordering::SeqCst);
    }
    pub fn thread_sealed(&self, thread: &str) -> bool {
        self.shared.params.seals.is_sealed(thread)
    }
    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String> {
        self.shared
            .state
            .lock()
            .expect("ACP state")
            .active
            .as_ref()
            .filter(|active| active.thread == thread)
            .map(|active| active.turn.clone())
    }
    pub async fn turn_start(
        &self,
        thread: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        client: &str,
        claim: &[crate::harness::QueueEntry],
    ) -> std::result::Result<String, TurnStartFailure> {
        let shared = &self.shared;
        let params = &shared.params;
        let _issue = shared.issue.lock().await;
        if !shared.installed.load(Ordering::SeqCst) {
            return Err(refused("ACP harness has not been installed"));
        }
        if shared.state.lock().expect("ACP state").closed || params.seals.is_sealed(thread) {
            return Err(refused("ACP conversation is closed or sealed"));
        }
        let pool = params
            .repo
            .sqlite_pool()
            .ok_or_else(|| refused("ACP requires durable submission storage"))?;
        let row = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await
            .map_err(CalmError::from)?
            .ok_or_else(|| refused("ACP owner is missing"))?;
        if row.provider != WorkerProviderKind::OpenCode
            || row.contract != WorkerContract::Planner
            || row.card_id.as_ref().map(|id| id.as_str()) != Some(&params.card_id)
            || row.thread_id.as_deref() != Some(thread)
            || !row.state.is_active_authority()
        {
            return Err(refused("ACP owner identity changed"));
        }
        let input = serde_json::to_value(&items).map_err(CalmError::from)?;
        if let Some(receipt) = acp_submission_get(&pool, &params.worker_session_id, client)
            .await
            .map_err(CalmError::from)?
        {
            let issued: Value =
                serde_json::from_str(&receipt.input_json).map_err(CalmError::from)?;
            if receipt.thread_id != thread
                || row.agent_session_id.as_deref() != Some(&receipt.native_session_id)
                || issued.get("queue") != Some(&crate::harness::submission_claims::freeze(claim))
            {
                return Err(refused("ACP receipt ownership or original input changed"));
            }
            // Exact-key recovery retires the original queue batch without any native write.
            let turn = receipt.turn_id.clone();
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
            return Ok(turn);
        }
        if shared.state.lock().expect("ACP state").active.is_some()
            || acp_submission_unresolved(&pool, &params.worker_session_id)
                .await
                .map_err(CalmError::from)?
        {
            return Err(refused(
                "Previous ACP outcome is unknown. Reset before sending more input; nothing will be resent.",
            ));
        }
        let mut blocks = Vec::new();
        let has_receipts: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM acp_submissions WHERE worker_session_id=?1)",
        )
        .bind(&params.worker_session_id)
        .fetch_one(&pool)
        .await
        .map_err(CalmError::from)?;
        if !has_receipts {
            blocks.push(protocol::ContentBlock::text(format!(
                "Neige Planner instructions:\n{}",
                params.instructions
            )));
        }
        for item in items {
            match item {
                InputItem::Text { text } => blocks.push(protocol::ContentBlock::text(text)),
                InputItem::LocalImage { .. } => {
                    return Err(refused(
                        "ACP image input is not enabled for this managed Planner",
                    ));
                }
            }
        }
        let config = params
            .host
            .configured(&params.provider)
            .map_err(|error| refused(&error.to_string()))?;
        use sha2::Digest as _;
        let digest = hex::encode(sha2::Sha256::digest(
            serde_json::to_vec(&(config, &params.cwd)).map_err(CalmError::from)?,
        ));
        crate::db::sqlite::acp_registration_claim(
            &pool,
            &params.worker_session_id,
            &digest,
            row.agent_session_id.is_some(),
        )
        .await
        .map_err(CalmError::from)?;
        let mut process =
            Process::spawn(&params.host, config, &params.worker_session_id, &params.cwd).await?;
        let prepared = async {
            let token = mint_token(params).await?;
            let mcp = [protocol::McpServer::Stdio {
                name: crate::mcp_server::wiring::MCP_SERVER_KEY.into(),
                command: params.host.mcp_shim.to_string_lossy().into_owned(),
                args: vec![],
                env: crate::mcp_server::wiring::card_mcp_env(&params.host.mcp_socket, &token)
                    .into_iter()
                    .map(|(name, value)| protocol::EnvVariable {
                        name: name.into(),
                        value,
                    })
                    .collect(),
            }];
            let (native, mut configuration) = match row.agent_session_id {
                Some(native) => {
                    if !process.capabilities.load_session {
                        return Err(refused("ACP agent cannot load its persisted conversation"));
                    }
                    let result = setup_request(
                        &mut process,
                        "session/load",
                        json!({"sessionId":native,"cwd":params.cwd,"mcpServers":mcp}),
                    )
                    .await?;
                    (
                        native,
                        provider::acp::configuration::Configuration::from_response(&result)
                            .map_err(wire_error)?,
                    )
                }
                None => {
                    let result = setup_request(
                        &mut process,
                        "session/new",
                        json!({"cwd":params.cwd,"mcpServers":mcp}),
                    )
                    .await?;
                    let configuration =
                        provider::acp::configuration::Configuration::from_response(&result)
                            .map_err(wire_error)?;
                    let native: protocol::NewSessionResponse =
                        protocol::decode(result).map_err(wire_error)?;
                    if native.session_id.is_empty() {
                        return Err(refused("ACP agent returned an empty native identity"));
                    }
                    bind_native(params, thread, &native.session_id).await?;
                    (native.session_id, configuration)
                }
            };
            params
                .host
                .record_configuration(&params.card_id, configuration.clone());
            for (category, value) in [
                ("model", selection.model.as_deref()),
                ("thought_level", selection.effort.as_deref()),
            ] {
                let Some(value) = value else {
                    continue;
                };
                let option = configuration
                    .category(category)
                    .filter(|option| option.accepts(value))
                    .ok_or_else(|| {
                        refused("The ACP agent did not declare that model or effort choice")
                    })?;
                let result = setup_request(
                    &mut process,
                    "session/set_config_option",
                    json!({"sessionId":native,"configId":option.id,"value":value}),
                )
                .await?;
                configuration = provider::acp::configuration::Configuration::from_response(&result)
                    .map_err(wire_error)?;
                if configuration
                    .category(category)
                    .is_none_or(|option| option.current_value != value)
                {
                    return Err(refused("ACP did not confirm the selected model or effort"));
                }
                params
                    .host
                    .record_configuration(&params.card_id, configuration.clone());
            }
            if params.seals.is_sealed(thread) || shared.state.lock().expect("ACP state").closed {
                return Err(refused("ACP conversation was sealed before dispatch"));
            }
            let turn = uuid::Uuid::new_v4().to_string();
            let receipt = AcpSubmission {
                worker_session_id: params.worker_session_id.clone(),
                client_id: client.into(),
                turn_id: turn.clone(),
                thread_id: thread.into(),
                native_session_id: native.clone(),
                input_json: serde_json::to_string(&json!({"input":input,"prompt":blocks,"queue":crate::harness::submission_claims::freeze(claim)}))
                    .map_err(CalmError::from)?,
                state: crate::db::sqlite::AcpSubmissionState::Sending,
                outcome_json: None,
            };
            acp_submission_prepare(&pool, &receipt, crate::model::now_ms())
                .await
                .map_err(CalmError::from)?;
            Ok::<_, TurnStartFailure>((native, turn, receipt))
        }
        .await;
        let (native, turn, receipt) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let revoked = revoke(params).await;
                let stopped = process.stop(&params.host, &params.worker_session_id).await;
                revoked?;
                stopped?;
                return Err(error);
            }
        };
        let (cancel, cancelled) = watch::channel(false);
        let (done, _) = watch::channel(false);
        shared.state.lock().expect("ACP state").active = Some(Active {
            thread: thread.into(),
            turn: turn.clone(),
            cancel,
            done: done.clone(),
        });
        let translator = TurnTranslator::new(TurnContext {
            thread_id: thread.into(),
            turn_id: turn.clone(),
            native_session_id: native.clone(),
        });
        let _ = shared.events.send(translator.started());
        let pending = protocol::prompt(
            &process.connection.client,
            &process.capabilities,
            &native,
            &blocks,
        )
        .await;
        let shared = Arc::clone(shared);
        tokio::spawn(async move {
            drive(shared, process, receipt, translator, pending, cancelled).await;
            let _ = done.send(true);
        });
        // A durable dispatch attempt always hands ownership to the Harness, even if its pipe failed.
        Ok(turn)
    }
    pub async fn turn_interrupt(&self, thread: &str, turn: &str) -> Result<()> {
        if let Some(active) = self
            .shared
            .state
            .lock()
            .expect("ACP state")
            .active
            .as_ref()
            .filter(|a| a.thread == thread && a.turn == turn)
        {
            let _ = active.cancel.send(true);
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.shared.state.lock().expect("ACP state").closed = true;
        let _issue = self.shared.issue.lock().await;
        if !self.shared.installed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let done = {
            let state = self.shared.state.lock().expect("ACP state");
            state.active.as_ref().map(|active| {
                let _ = active.cancel.send(true);
                active.done.subscribe()
            })
        };
        let params = &self.shared.params;
        revoke(params).await?;
        crate::planner_process::stop(&params.host.instance, &params.worker_session_id).await?;
        if let Some(mut done) = done {
            let _ = tokio::time::timeout(Duration::from_secs(5), done.wait_for(|done| *done)).await;
        }
        Ok(())
    }
}
fn refused(reason: &str) -> TurnStartFailure {
    TurnStartFailure::Refused {
        error: CalmError::Conflict(reason.into()),
        reader: reason.into(),
    }
}
fn wire_error(error: provider::acp::Error) -> CalmError {
    CalmError::Conflict(error.to_string())
}
pub(super) fn unknown_outcome(turn: &str) -> Value {
    json!({"id":turn,"status":"failed","error":{"message":"ACP execution outcome is unknown. The submission will not be resent; reset the conversation to continue."}})
}
async fn setup_request(process: &mut Process, method: &str, params: Value) -> Result<Value> {
    let response = process
        .connection
        .client
        .submit(method, params)
        .await
        .map_err(wire_error)?;
    let response = response.wait(Duration::from_secs(10));
    tokio::pin!(response);
    loop {
        tokio::select! {
            result=&mut response=>return finish_setup(&process.connection.client,&mut process.connection.incoming,result).await,
            incoming=process.connection.incoming.recv()=>match incoming {
                Some(Incoming::Request{id,method,..})=>answer(&process.connection.client,id,&method).await?,
                Some(Incoming::Notification{..})=>{}, // Loaded replay never represents a fresh native prompt.
                None=>return Err(CalmError::Conflict("ACP setup connection closed".into())),
            }
        }
    }
}
async fn finish_setup(
    client: &provider::acp::Client,
    incoming: &mut tokio::sync::mpsc::Receiver<Incoming>,
    result: std::result::Result<Value, provider::acp::Error>,
) -> Result<Value> {
    // The transport delivered these frames before the response. Drain replay at this boundary,
    // even when select chooses the ready response before the incoming branch.
    while let Ok(frame) = incoming.try_recv() {
        if let Incoming::Request { id, method, .. } = frame {
            answer(client, id, &method).await?;
        }
    }
    result.map_err(wire_error)
}
async fn answer(client: &provider::acp::Client, id: Value, method: &str) -> Result<()> {
    if method == "session/request_permission" {
        client
            .respond(id, json!({"outcome":{"outcome":"cancelled"}}))
            .await
            .map_err(wire_error)
    } else {
        client.reject_method(id).await.map_err(wire_error)
    }
}

async fn bind_native(params: &SessionParams, thread: &str, native: &str) -> Result<()> {
    let worker = params.worker_session_id.clone();
    let thread = thread.to_owned();
    let native = native.to_owned();
    let provider = params.provider.clone();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let row = crate::db::sqlite::session_get_tx(tx, &WorkerSessionId(worker.clone()))
                .await?
                .ok_or_else(|| CalmError::NotFound("ACP owner".into()))?;
            if !row.state.is_active_authority() || row.thread_id.as_deref() != Some(&thread) {
                return Err(CalmError::Conflict("ACP native owner changed".into()));
            }
            crate::db::sqlite::session_bind_attribution_tx(
                tx,
                &worker,
                crate::session_projection_repo::ThreadAttribution {
                    worker_session_id: worker.clone(),
                    provider,
                    thread_id: Some(thread),
                    session_id: Some(native),
                    active_turn_id: row.active_turn_id,
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
}
async fn mint_token(params: &SessionParams) -> Result<String> {
    let card = params.card_id.clone();
    let worker = params.worker_session_id.clone();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let owner: Option<String> =
                sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
                    .bind(&card)
                    .fetch_optional(&mut **tx)
                    .await?
                    .flatten();
            if owner.as_deref() != Some(&worker) {
                return Err(CalmError::Conflict("ACP credential carrier changed".into()));
            }
            crate::mcp_server::wiring::mint_and_persist_managed_planner_token(tx, &card, &worker)
                .await
        })
    })
    .await
}
async fn revoke(params: &SessionParams) -> Result<()> {
    let worker = params.worker_session_id.clone();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
                .bind(worker)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
}
async fn drive(
    shared: Arc<Shared>,
    mut process: Process,
    receipt: AcpSubmission,
    mut translator: TurnTranslator,
    pending: std::result::Result<provider::acp::PendingResponse, provider::acp::Error>,
    mut cancelled: watch::Receiver<bool>,
) {
    let params = &shared.params;
    let mut retained = std::collections::BTreeMap::<String, Value>::new();
    let outcome=async {
        let pending=pending.map_err(wire_error)?;
        let response=pending.wait(Duration::from_secs(3600));tokio::pin!(response);
        let mut stop_at=None;
        loop {tokio::select! {
            result=&mut response=>{
                while let Ok(incoming)=process.connection.incoming.try_recv() {
                    match incoming {
                        Incoming::Notification{method,params:frame} if method=="session/update"=>{
                            for event in translator.update(&frame,crate::model::now_ms()).map_err(wire_error)? {retain_item(&mut retained,&event);let _=shared.events.send(event);}
                        },
                        Incoming::Request{..}=>return Err(CalmError::Conflict("ACP completed while a client request was still pending".into())),
                        _=>{},
                    }
                }
                let response:protocol::PromptResponse=protocol::decode(result.map_err(wire_error)?).map_err(wire_error)?;
                return Ok::<_,CalmError>(translator.finish(response.stop_reason,crate::model::now_ms()));
            },
            changed=cancelled.changed(),if stop_at.is_none()=>{
                if changed.is_err() || *cancelled.borrow() {
                    stop_at=Some(tokio::time::Instant::now()+Duration::from_secs(5));
                    tokio::time::timeout(Duration::from_secs(3),async {
                        protocol::cancel(&process.connection.client,&receipt.native_session_id).await.map_err(wire_error)
                    }).await.map_err(|_|CalmError::Conflict("ACP cancellation writes timed out".into()))??;
                }
            },
            _=async{match stop_at{Some(at)=>tokio::time::sleep_until(at).await,None=>std::future::pending().await}}=>return Err(CalmError::Conflict("ACP cancellation outcome is unknown".into())),
            incoming=process.connection.incoming.recv()=>match incoming {
                Some(Incoming::Notification{method,params}) if method=="session/update"=>{
                    for event in translator.update(&params,crate::model::now_ms()).map_err(wire_error)? {retain_item(&mut retained,&event);let _=shared.events.send(event);}
                },
                Some(Incoming::Request{id,method,..})=>answer(&process.connection.client,id,&method).await?,
                Some(Incoming::Notification{..})=>{},
                None=>return Err(CalmError::Conflict("ACP prompt connection closed".into())),
            }
        }}
    }.await;
    let revoked = revoke(params).await;
    if let Err(error) = &revoked {
        tracing::error!(%error,"ACP credential revocation failed");
    }
    let stopped = process.stop(&params.host, &params.worker_session_id).await;
    let (events, known) = match outcome {
        Ok(events) if stopped.is_ok() && revoked.is_ok() => (events, true),
        _ => (
            vec![PlannerEvent {
                thread_id: Some(receipt.thread_id.clone()),
                kind: PlannerEventKind::TurnCompleted {
                    turn: unknown_outcome(&receipt.turn_id),
                },
            }],
            false,
        ),
    };
    let turn = events
        .iter()
        .find_map(|event| match &event.kind {
            PlannerEventKind::TurnCompleted { turn } => Some(turn.clone()),
            _ => None,
        })
        .expect("ACP terminal event");
    for event in &events {
        retain_item(&mut retained, event);
    }
    let mut durable = turn.clone();
    durable["items"] = Value::Array(retained.values().cloned().collect());
    let settle = async {
        crate::harness::turn_outcome::record(
            params.repo.as_ref(),
            &params.worker_session_id,
            &params.card_id,
            &params.track_id,
            &receipt.thread_id,
            &receipt.turn_id,
            &turn,
        )
        .await?;
        acp_submission_finish(
            &params
                .repo
                .sqlite_pool()
                .ok_or_else(|| CalmError::Internal("ACP journal pool missing".into()))?,
            &receipt.worker_session_id,
            &receipt.client_id,
            known
                .then(|| serde_json::to_string(&durable))
                .transpose()?
                .as_deref(),
        )
        .await
    }
    .await;
    if let Err(error) = settle {
        tracing::error!(%error,"ACP settlement failed; durable submission remains fenced");
    }
    let mut state = shared.state.lock().expect("ACP state");
    state.active = None;
    for event in events {
        let _ = shared.events.send(event);
    }
}
fn retain_item(retained: &mut std::collections::BTreeMap<String, Value>, event: &PlannerEvent) {
    if let PlannerEventKind::Item { params, phase, .. } = &event.kind
        && let Some(id) = params["item"]["id"].as_str()
    {
        retained.insert(
            id.to_owned(),
            json!({"method":phase.method(),"params":params}),
        );
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn ready_setup_response_drains_prior_history_before_live_turn_installation() {
        let (kernel, peer) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(kernel);
        let mut connection = provider::acp::Connection::new(read, write);
        let pending = connection
            .client
            .submit("session/load", json!({"sessionId":"native"}))
            .await
            .unwrap();
        let (read, mut write) = tokio::io::split(peer);
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let history = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"native","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"historic reply"}}}});
        let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{}});
        write
            .write_all(format!("{history}\n{response}\n").as_bytes())
            .await
            .unwrap();
        let result = pending.wait(Duration::from_secs(2)).await;
        assert_eq!(
            connection.incoming.len(),
            1,
            "history precedes its load response on the real transport"
        );
        finish_setup(&connection.client, &mut connection.incoming, result)
            .await
            .unwrap();
        assert!(
            matches!(
                connection.incoming.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "load history must not leak into the next live translator"
        );
    }
}
