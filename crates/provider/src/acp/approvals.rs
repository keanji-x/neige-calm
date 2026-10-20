//! Planner approvals over ACP (#2348): how one turn answers the agent's
//! `session/request_permission`, and how each request becomes one question of a Planner harness
//! and each chosen option the ACP answer.
//!
//! The turn's permission mode is the harness's, read when it issued the turn. Under `never` and
//! `full` (#2441) every request is answered `cancelled` where it is read. Under `ask` each request
//! becomes an `Open` whose [`AcpResponder`] answers it with the chosen option's id. The caller runs
//! one agent process per turn, so the process is the connection: the [`HeldPermissions`] the turn's
//! reader owns pushes `ConnectionLost` when it is dropped, after the reader's last frame.
//!
//! Under every mode, a request for a tool of the client's own MCP server ([`OwnTools`], when the
//! agent's profile says how it asks before one) is answered with the agent's `allow_once` option
//! without asking: that server authorizes each call itself. Only the exact qualified names of the
//! tools that server lists the session match, under the server key the client registered it with.
//! A request read after the turn's fence is never approved.
//!
//! A request the turn declines (answered `cancelled` by the mode, or a reject the person chose) is
//! kept, so a turn the agent then ends `cancelled` on its own can say why ([`Approvals::declined`]),
//! when the agent's profile says its agent ends a turn so; a decline under `never` also names the
//! modes that would let the request through.
//!
//! The turn ends its requests by a fence: a cancel (`session/cancel` first), and the end of the
//! turn's read whatever ended it ([`Approvals::close`]), each answer every request still pending
//! `cancelled` and refuse every later answer. OpenCode 1.18.35 still acts on a `selected` answer
//! that arrives after its turn was cancelled or settled, so one lock orders every answer with the
//! fence: an answer either reaches the agent before it, or writes `cancelled`, or nothing when the
//! fence already answered its request. A cancel then pushes `Gone` for each request it answered.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_types::event::{ASK_MAX_TEXT_CHARS, AskQuestion, clip_ask_title};
use calm_types::harness::PlannerPermissionMode;
use serde_json::Value;
use tokio::sync::Mutex;

use super::permission::{self, PermissionRequest};
use super::{Client, Error};
use crate::held_requests::{
    ConnectionId, HeldRequestMessage, HeldRequestSender, HeldResponder, RequestKey,
};

/// How one turn answers the agent's requests to the client, fixed by the permission mode read
/// when the turn started.
pub struct Approvals {
    answers: Answers,
    agent: AgentAsks,
    /// Set by a cancel or the end of the turn's read; the turn's one reader sets and reads it.
    fenced: AtomicBool,
    /// Why the last request this turn declined was declined, until the fence.
    declined: Declined,
}

enum Answers {
    /// `never` and `full`: a permission request is answered `cancelled` where it is read, on the
    /// turn's agent process.
    Refused(Client, PlannerPermissionMode),
    /// `ask`: a permission request is put to the person through the harness.
    Held(HeldPermissions),
}

/// What the client knows of how its agent asks, as the agent's profile declares it.
#[derive(Clone, Debug, Default)]
pub struct AgentAsks {
    /// The client's own MCP server's tools, approved without asking until the fence.
    pub own_tools: Option<OwnTools>,
    /// The agent ends its turn `cancelled` when a request it asked is declined, so such an end
    /// is the decline's, not an interruption.
    pub decline_ends_turn: bool,
}

/// How an agent asks before calling one MCP tool, as its profile declares it: a request of ACP
/// kind `kind` whose title, and whose `rawInput` field `raw_input_name`, are both the tool's
/// qualified name `<server><separator><tool>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpToolRequest {
    pub kind: &'static str,
    pub separator: &'static str,
    pub raw_input_name: &'static str,
}

/// The tools of the client's own MCP server, by the exact qualified names its agent's requests
/// carry.
#[derive(Clone, Debug)]
pub struct OwnTools {
    form: McpToolRequest,
    qualified: HashSet<String>,
}

impl OwnTools {
    /// The tools `tools` of the server the client registered as `server`, as `form` qualifies
    /// them. A name holding the separator could be split at another place (`a__b` + `c` and `a` +
    /// `b__c` qualify alike), so such a server has no tools here (`None`) and such a tool is left
    /// out.
    pub fn new<T: AsRef<str>>(
        form: McpToolRequest,
        server: &str,
        tools: impl IntoIterator<Item = T>,
    ) -> Option<Self> {
        let unambiguous = |name: &str| !name.is_empty() && !name.contains(form.separator);
        unambiguous(server).then(|| Self {
            form,
            qualified: tools
                .into_iter()
                .filter(|tool| unambiguous(tool.as_ref()))
                .map(|tool| format!("{server}{}{}", form.separator, tool.as_ref()))
                .collect(),
        })
    }

    /// The agent's `allow_once` option for `request` when it is for one of these tools.
    fn allow_once<'a>(&self, request: &'a PermissionRequest) -> Option<&'a str> {
        let call = &request.tool_call;
        let name = call
            .title
            .as_deref()
            .filter(|_| call.kind.as_deref() == Some(self.form.kind))
            .filter(|title| self.qualified.contains(*title))?;
        let input = call
            .raw_input
            .as_ref()
            .and_then(|input| input.get(self.form.raw_input_name))
            .and_then(Value::as_str);
        if input != Some(name) {
            return None;
        }
        request
            .options
            .iter()
            .find(|option| option.kind == "allow_once")
            .map(|option| option.option_id.as_str())
    }
}

/// Why the agent ended the turn, after the last request of the turn was declined.
type Declined = Arc<std::sync::Mutex<Option<String>>>;

fn decline(declined: &Declined, reason: String) {
    *declined.lock().expect("ACP declined request") =
        Some(format!("The agent ended the turn because {reason}"));
}

/// What a person is told a request was about: its title, or its id without one.
fn subject(request: &PermissionRequest) -> String {
    let call = &request.tool_call;
    let what = call.title.as_deref().unwrap_or(&call.tool_call_id);
    let mut subject: String = what.chars().take(200).collect();
    if subject.len() < what.len() {
        subject.push('…');
    }
    subject
}

fn mode_name(mode: PlannerPermissionMode) -> &'static str {
    match mode {
        PlannerPermissionMode::Never => "never",
        PlannerPermissionMode::Ask => "ask",
        PlannerPermissionMode::Full => "full",
    }
}

/// What a person can change so that a request `mode` declined is not declined again.
fn after_decline(mode: PlannerPermissionMode) -> &'static str {
    match mode {
        PlannerPermissionMode::Never => {
            " Switch the permission mode to `ask` to approve such a request, or to `full` to allow it."
        }
        PlannerPermissionMode::Ask | PlannerPermissionMode::Full => "",
    }
}

impl Approvals {
    /// The approvals of the turn `turn` under `mode`, reporting to the harness's `held` and
    /// answering on `client`, the turn's agent process, which asks as `agent` says.
    pub fn for_turn(
        mode: PlannerPermissionMode,
        held: &HeldRequestSender,
        turn: &str,
        client: &Client,
        agent: AgentAsks,
    ) -> Self {
        let declined = Declined::default();
        let answers = match mode {
            PlannerPermissionMode::Never | PlannerPermissionMode::Full => {
                Answers::Refused(client.clone(), mode)
            }
            PlannerPermissionMode::Ask => Answers::Held(HeldPermissions {
                sender: held.clone(),
                connection: ConnectionId(turn.to_string()),
                client: client.clone(),
                requests: Arc::new(Mutex::new(Requests::default())),
                declined: Arc::clone(&declined),
            }),
        };
        Self {
            answers,
            agent,
            fenced: AtomicBool::new(false),
            declined,
        }
    }

    fn client(&self) -> &Client {
        match &self.answers {
            Answers::Refused(client, _) => client,
            Answers::Held(held) => &held.client,
        }
    }

    /// A request the agent sent during the turn.
    pub async fn request(&self, id: Value, method: &str, params: Value) -> Result<(), Error> {
        let fenced = self.fenced.load(Ordering::SeqCst);
        let request = (method == permission::METHOD)
            .then(|| PermissionRequest::decode(params.clone()).ok())
            .flatten();
        if let Some(request) = &request
            && let Some(own_tools) = &self.agent.own_tools
            && !fenced
            && let Some(option) = own_tools.allow_once(request)
        {
            return self
                .client()
                .respond(id, permission::selected(option))
                .await;
        }
        match &self.answers {
            Answers::Held(held) if method == permission::METHOD => held.open(id, params).await,
            Answers::Held(held) => refuse(&held.client, id, method).await,
            Answers::Refused(client, mode) => {
                if let Some(request) = request.as_ref().filter(|_| !fenced) {
                    decline(
                        &self.declined,
                        format!(
                            "Neige declined `{}` under the `{}` permission mode.{}",
                            subject(request),
                            mode_name(*mode),
                            after_decline(*mode)
                        ),
                    );
                }
                refuse(client, id, method).await
            }
        }
    }

    /// Why the agent ended the turn `cancelled` on its own: the last request the turn declined,
    /// when the agent's profile says it ends a turn on a decline. `None` otherwise, and for a turn
    /// that declined nothing.
    pub fn declined(&self) -> Option<String> {
        if !self.agent.decline_ends_turn {
            return None;
        }
        self.declined.lock().expect("ACP declined request").clone()
    }

    /// Send `session/cancel` for the native session `native`, and answer what is still pending.
    pub async fn cancel(&self, native: &str) -> Result<(), Error> {
        self.fenced.store(true, Ordering::SeqCst);
        match &self.answers {
            Answers::Refused(client, _) => super::protocol::cancel(client, native).await,
            Answers::Held(held) => held.cancel(native).await,
        }
    }

    /// The turn read its last frame from the agent: answer every request still pending
    /// `cancelled`, and refuse every later answer. Call it before anything else of the turn's
    /// teardown; nothing the person chooses reaches the agent after it.
    pub async fn close(&self) {
        self.fenced.store(true, Ordering::SeqCst);
        if let Answers::Held(held) = &self.answers {
            held.close().await;
        }
    }
}

/// Answer a client request without asking anyone: a permission request is `cancelled`, any other
/// method is not supported.
pub async fn refuse(client: &Client, id: Value, method: &str) -> Result<(), Error> {
    if method == permission::METHOD {
        client.respond(id, permission::cancelled()).await
    } else {
        client.reject_method(id).await
    }
}

/// The requests of one agent process that no one has answered yet.
#[derive(Default)]
struct Requests {
    /// Set by the fence (a cancel, or the end of the turn's read): every answer after it is
    /// `cancelled`, and a request after it is answered `cancelled` without asking.
    fenced: bool,
    /// The JSON-RPC id of each request still waiting, by its key.
    pending: HashMap<RequestKey, Value>,
}

/// One agent process as a connection of the harness's held-request channel. Dropping it pushes
/// `ConnectionLost`, which withdraws every request of the process the harness still holds.
pub struct HeldPermissions {
    sender: HeldRequestSender,
    connection: ConnectionId,
    client: Client,
    requests: Arc<Mutex<Requests>>,
    /// The turn's record of its last declined request.
    declined: Declined,
}

impl HeldPermissions {
    /// Put the request `id` to the person; the turn keeps reading while it waits. A malformed
    /// request, or one after the fence, is answered `cancelled` at once. A harness that is gone
    /// drops the message, and with it the responder, which answers `cancelled`.
    async fn open(&self, id: Value, params: Value) -> Result<(), Error> {
        let request = match PermissionRequest::decode(params) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(%error, "ACP: malformed permission request refused");
                return self.answer_now(id).await;
            }
        };
        let request_key = RequestKey(format!("{}/{id}", self.connection.0));
        {
            let mut requests = self.requests.lock().await;
            if requests.fenced {
                drop(requests);
                return self.answer_now(id).await;
            }
            requests.pending.insert(request_key.clone(), id.clone());
        }
        let responder = AcpResponder {
            pending: Some(Pending {
                request_key: request_key.clone(),
                id,
                options: request
                    .options
                    .iter()
                    .map(|option| (option.option_id.clone(), option.kind.clone()))
                    .collect(),
                subject: subject(&request),
                declined: Arc::clone(&self.declined),
                client: self.client.clone(),
                requests: Arc::clone(&self.requests),
                runtime: tokio::runtime::Handle::current(),
            }),
        };
        let _ = self.sender.send(HeldRequestMessage::Open {
            request_key,
            connection: self.connection.clone(),
            questions: vec![question(&request)],
            responder: Box::new(responder),
        });
        Ok(())
    }

    async fn answer_now(&self, id: Value) -> Result<(), Error> {
        self.client.respond(id, permission::cancelled()).await
    }

    /// `session/cancel`, then `cancelled` for every request still pending, under the lock every
    /// answer takes; then `Gone` for each, so the harness withdraws their asks.
    async fn cancel(&self, native: &str) -> Result<(), Error> {
        let mut requests = self.requests.lock().await;
        requests.fenced = true;
        super::protocol::cancel(&self.client, native).await?;
        let pending = std::mem::take(&mut requests.pending);
        for id in pending.values() {
            self.client
                .respond(id.clone(), permission::cancelled())
                .await?;
        }
        drop(requests);
        for request_key in pending.into_keys() {
            let _ = self.sender.send(HeldRequestMessage::Gone { request_key });
        }
        Ok(())
    }

    /// The fence at the end of the turn's read: `cancelled` for every request still pending, under
    /// the lock every answer takes. A write that fails is not retried: the process is ending.
    async fn close(&self) {
        let mut requests = self.requests.lock().await;
        requests.fenced = true;
        for (_, id) in requests.pending.drain() {
            if let Err(error) = self.client.respond(id, permission::cancelled()).await {
                tracing::debug!(%error, "ACP: closing answer not written");
            }
        }
    }
}

impl Drop for HeldPermissions {
    /// The fence again, for a turn whose teardown did not reach [`Approvals::close`]: a later
    /// answer writes `cancelled`. Then `ConnectionLost`.
    fn drop(&mut self) {
        match self.requests.try_lock() {
            Ok(mut requests) => requests.fenced = true,
            Err(_) => {
                let requests = Arc::clone(&self.requests);
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    runtime.spawn(async move { requests.lock().await.fenced = true });
                }
            }
        }
        let _ = self.sender.send(HeldRequestMessage::ConnectionLost {
            connection: self.connection.clone(),
        });
    }
}

/// The one question a permission request asks: what the tool call does (its kind in a plain word,
/// and its title) and the files it names. Only the call is cut when the whole is over the kernel's
/// bound, so the files stay in view. The options are the agent's, by name, in its order.
fn question(request: &PermissionRequest) -> AskQuestion {
    let call = &request.tool_call;
    let what = call.title.as_deref().unwrap_or(&call.tool_call_id);
    let head = match call.kind.as_deref().and_then(kind_word) {
        Some(word) => format!("{word}: {what}"),
        None => what.to_string(),
    };
    let paths: String = call
        .locations
        .iter()
        .flatten()
        .filter(|location| call.title.as_deref() != Some(location.path.as_str()))
        .map(|location| format!("\nPath: {}", location.path))
        .collect();
    let room = ASK_MAX_TEXT_CHARS.saturating_sub(paths.chars().count());
    let head = head.trim();
    let head = if head.chars().count() <= room {
        head.to_string()
    } else {
        let mut cut: String = head.chars().take(room.saturating_sub(1)).collect();
        cut.push('…');
        cut
    };
    AskQuestion {
        title: clip_ask_title(&format!("{head}{paths}")),
        options: request
            .options
            .iter()
            .map(|option| option.name.clone())
            .collect(),
    }
}

/// The word a person is shown for an ACP tool kind, one of the protocol's closed set. `other`, and
/// a kind this client does not know, show none: the raw kind means nothing to a reader, and the
/// call's own title still says what it does (#2452).
fn kind_word(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "read" => "Read",
        "edit" => "Edit",
        "delete" => "Delete",
        "move" => "Move",
        "search" => "Search",
        "execute" => "Run",
        "think" => "Think",
        "fetch" => "Fetch",
        "switch_mode" => "Switch mode",
        _ => return None,
    })
}

/// Answers one permission request: the chosen option's id, or `cancelled` when dropped
/// unanswered. A request the cancel already answered gets nothing more.
struct AcpResponder {
    /// `None` once the answer is on its way.
    pending: Option<Pending>,
}

struct Pending {
    request_key: RequestKey,
    id: Value,
    /// The agent's option ids and kinds, in the order of the question's options.
    options: Vec<(String, String)>,
    /// What the request was about, for the record of a reject the person chose.
    subject: String,
    declined: Declined,
    client: Client,
    requests: Arc<Mutex<Requests>>,
    /// The runtime of the driver that opened it; `respond` and `drop` are synchronous.
    runtime: tokio::runtime::Handle,
}

impl Pending {
    /// Answer with option `option`'s id, or `cancelled` for `None`.
    fn answer(self, option: Option<usize>) {
        let Self {
            request_key,
            id,
            options,
            subject,
            declined,
            client,
            requests,
            runtime,
        } = self;
        runtime.spawn(async move {
            let mut requests = requests.lock().await;
            if requests.pending.remove(&request_key).is_none() {
                return;
            }
            // A cancel cut short by its caller's timeout fenced without answering its requests.
            let chosen = option
                .filter(|_| !requests.fenced)
                .and_then(|option| options.get(option));
            let result = match chosen {
                Some((option_id, kind)) => {
                    if kind.starts_with("reject") {
                        decline(&declined, format!("the person declined `{subject}`."));
                    }
                    permission::selected(option_id)
                }
                None => permission::cancelled(),
            };
            // A process that already ended has closed its pipe; nothing waits on the answer.
            if let Err(error) = client.respond(id, result).await {
                tracing::debug!(%error, "ACP: permission answer not written");
            }
        });
    }
}

impl HeldResponder for AcpResponder {
    fn respond(mut self: Box<Self>, option: usize) {
        if let Some(pending) = self.pending.take() {
            pending.answer(Some(option));
        }
    }
}

impl Drop for AcpResponder {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.answer(None);
        }
    }
}

#[cfg(test)]
#[path = "approvals_tests.rs"]
mod tests;
