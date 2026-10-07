//! Planner approvals over ACP (#2348): how one turn answers the agent's
//! `session/request_permission`, and how each request becomes one question of a Planner harness
//! and each chosen option the ACP answer.
//!
//! The turn's permission mode is the harness's, read when it issued the turn. Under `never` every
//! request is answered `cancelled` where it is read. Under `ask` each request becomes an `Open`
//! whose [`AcpResponder`] answers it with the chosen option's id. The caller runs one agent
//! process per turn, so the process is the connection: the [`HeldPermissions`] the turn's reader
//! owns pushes `ConnectionLost` when it is dropped, after the reader's last frame.
//!
//! A cancel answers every request still pending `cancelled` right after `session/cancel`, and
//! pushes `Gone` for each. OpenCode 1.18.35 still acts on a `selected` answer that arrives after
//! its turn was cancelled, so one lock orders every answer with the cancel: an answer either
//! reaches the agent before `session/cancel`, or the request was already answered `cancelled`.

use std::collections::HashMap;
use std::sync::Arc;

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
pub enum Approvals {
    /// `never`: a permission request is answered `cancelled` where it is read.
    Refused,
    /// `ask`: a permission request is put to the person through the harness.
    Held(HeldPermissions),
}

impl Approvals {
    /// The approvals of the turn `turn` under `mode`, reporting to the harness's `held` and
    /// answering on `client`, the turn's agent process.
    pub fn for_turn(
        mode: PlannerPermissionMode,
        held: &HeldRequestSender,
        turn: &str,
        client: &Client,
    ) -> Self {
        match mode {
            PlannerPermissionMode::Never => Self::Refused,
            PlannerPermissionMode::Ask => Self::Held(HeldPermissions {
                sender: held.clone(),
                connection: ConnectionId(turn.to_string()),
                client: client.clone(),
                requests: Arc::new(Mutex::new(Requests::default())),
            }),
        }
    }

    /// A request the agent sent during the turn.
    pub async fn request(
        &self,
        client: &Client,
        id: Value,
        method: &str,
        params: Value,
    ) -> Result<(), Error> {
        match self {
            Self::Held(held) if method == permission::METHOD => held.open(id, params).await,
            _ => refuse(client, id, method).await,
        }
    }

    /// Send `session/cancel` for the native session `native`, and answer what is still pending.
    pub async fn cancel(&self, client: &Client, native: &str) -> Result<(), Error> {
        match self {
            Self::Refused => super::protocol::cancel(client, native).await,
            Self::Held(held) => held.cancel(native).await,
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
    /// Set once `session/cancel` was sent; a request after it is answered `cancelled` at once.
    cancelled: bool,
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
}

impl HeldPermissions {
    /// Put the request `id` to the person; the turn keeps reading while it waits. A malformed
    /// request, or one after the cancel, is answered `cancelled` at once. A harness that is gone
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
            if requests.cancelled {
                drop(requests);
                return self.answer_now(id).await;
            }
            requests.pending.insert(request_key.clone(), id.clone());
        }
        let responder = AcpResponder {
            pending: Some(Pending {
                request_key: request_key.clone(),
                id,
                option_ids: request
                    .options
                    .iter()
                    .map(|option| option.option_id.clone())
                    .collect(),
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
        requests.cancelled = true;
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
}

impl Drop for HeldPermissions {
    fn drop(&mut self) {
        let _ = self.sender.send(HeldRequestMessage::ConnectionLost {
            connection: self.connection.clone(),
        });
    }
}

/// The one question a permission request asks: what the tool call is (its kind and title) and the
/// files it names. Only the call is cut when the whole is over the kernel's bound, so the files
/// stay in view. The options are the agent's, by name, in its order.
fn question(request: &PermissionRequest) -> AskQuestion {
    let call = &request.tool_call;
    let head = match (&call.kind, &call.title) {
        (Some(kind), Some(title)) => format!("{kind}: {title}"),
        (None, Some(title)) => title.clone(),
        (Some(kind), None) => format!("{kind}: {}", call.tool_call_id),
        (None, None) => call.tool_call_id.clone(),
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

/// Answers one permission request: the chosen option's id, or `cancelled` when dropped
/// unanswered. A request the cancel already answered gets nothing more.
struct AcpResponder {
    /// `None` once the answer is on its way.
    pending: Option<Pending>,
}

struct Pending {
    request_key: RequestKey,
    id: Value,
    /// The agent's option ids, in the order of the question's options.
    option_ids: Vec<String>,
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
            option_ids,
            client,
            requests,
            runtime,
        } = self;
        runtime.spawn(async move {
            let mut requests = requests.lock().await;
            if requests.pending.remove(&request_key).is_none() {
                return;
            }
            let result = match option.and_then(|option| option_ids.get(option)) {
                Some(option_id) => permission::selected(option_id),
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
