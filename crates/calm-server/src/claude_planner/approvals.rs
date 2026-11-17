//! A Claude Planner's tool approvals (#2348): the adapter between the CLI's `can_use_tool` and the
//! harness's held-request channel.
//!
//! Every spawn runs under the permission mode the harness read for its turn. Under `ask` it runs
//! with `--permission-prompt-tool stdio`; each `can_use_tool` becomes an `Open` whose
//! [`ClaudeResponder`] writes the decision on the child's stdin, and each
//! `control_cancel_request` becomes a `Gone`. The spawn is the connection: the
//! [`SpawnConnection`] its read task holds pushes `ConnectionLost` when that task ends, after the
//! last stdout line was read, so no `Open` of the spawn can follow it. Under `never` and `full`
//! (`--permission-prompts none`) a `can_use_tool` is not expected and is refused where it is read.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

use serde_json::{Map, Value};

use super::turn_slot::TurnSlot;
use crate::event::AskQuestion;
use crate::harness::held_requests::{
    ConnectionId, HeldRequestMessage, HeldRequestSender, HeldResponder, RequestKey,
};
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::claude::permission::{CanUseTool, PermissionDecision, permission_response};
use provider::claude::protocol::ControlResponseOut;

/// The options of every approval question, in the order the answer's index names them.
pub(crate) const OPTIONS: [&str; 2] = ["Allow", "Deny"];
const ALLOW: usize = 0;
/// What the model reads when the person chose Deny.
pub(crate) const DENIED_BY_USER: &str = "The user denied this tool use.";
/// What the model reads when nobody answered: the ask could not be raised, or it was withdrawn.
pub(crate) const NOT_ANSWERED: &str =
    "This tool use was not approved: nobody answered the request for it.";
/// What a `never` or `full` spawn answers a `can_use_tool` it should not have been sent.
pub(crate) const NO_APPROVAL_SURFACE: &str = "this Planner has no approval surface";

/// How one spawn answers `can_use_tool`, fixed by the permission mode read at the spawn.
pub(crate) enum Approvals {
    /// `never` and `full`: refused where it is read.
    Refused,
    /// `ask`: put to the person through the harness that holds this session.
    Held(SpawnConnection),
}

impl Approvals {
    /// The approvals of the spawn `spawn_id` under `mode`, reporting to the harness's `held`.
    pub(crate) fn for_spawn(
        mode: PlannerPermissionMode,
        held: &HeldRequestSender,
        spawn_id: &str,
    ) -> Self {
        match mode {
            PlannerPermissionMode::Never | PlannerPermissionMode::Full => Self::Refused,
            PlannerPermissionMode::Ask => Self::Held(SpawnConnection {
                sender: held.clone(),
                connection: ConnectionId(spawn_id.to_string()),
                requests: Mutex::new(HashMap::new()),
            }),
        }
    }
}

/// One spawn as a connection of the harness's held-request channel. Dropping it pushes
/// `ConnectionLost`, which withdraws every request of the spawn the harness still holds.
pub(crate) struct SpawnConnection {
    sender: HeldRequestSender,
    connection: ConnectionId,
    requests: Mutex<HashMap<String, Weak<AtomicBool>>>,
}

impl SpawnConnection {
    /// Put `request` to the person; the turn keeps reading while it waits. A harness that is gone
    /// drops the message, and with it the responder, which refuses the request.
    pub(crate) fn open(&self, slot: &Arc<TurnSlot>, request_id: &str, request: &CanUseTool) {
        let live = Arc::new(AtomicBool::new(true));
        let mut requests = self.requests.lock().expect("Claude approval requests");
        requests.retain(|_, live| live.strong_count() > 0);
        if let Some(previous) = requests
            .insert(request_id.into(), Arc::downgrade(&live))
            .and_then(|old| old.upgrade())
        {
            previous.store(false, Ordering::SeqCst);
        }
        drop(requests);
        let responder = ClaudeResponder {
            pending: Some(Pending {
                slot: Arc::clone(slot),
                live,
                request_id: request_id.to_string(),
                runtime: tokio::runtime::Handle::current(),
            }),
        };
        let _ = self.sender.send(HeldRequestMessage::Open {
            request_key: RequestKey(request_id.to_string()),
            connection: self.connection.clone(),
            questions: vec![question(request)],
            responder: Box::new(responder),
        });
    }

    /// The CLI cancelled its request `request_id`.
    pub(crate) fn gone(&self, request_id: &str) {
        if let Some(live) = self
            .requests
            .lock()
            .expect("Claude approval requests")
            .remove(request_id)
            .and_then(|live| live.upgrade())
        {
            live.store(false, Ordering::SeqCst);
        }
        let _ = self.sender.send(HeldRequestMessage::Gone {
            request_key: RequestKey(request_id.to_string()),
        });
    }
}

impl Drop for SpawnConnection {
    fn drop(&mut self) {
        for live in self
            .requests
            .get_mut()
            .expect("Claude approval requests")
            .values()
            .filter_map(Weak::upgrade)
        {
            live.store(false, Ordering::SeqCst);
        }
        let _ = self.sender.send(HeldRequestMessage::ConnectionLost {
            connection: self.connection.clone(),
        });
    }
}

/// The one question an approval asks: the tool and what it would act on, then the path and the
/// reason the CLI gives, cut to the kernel's bound.
pub(crate) fn question(request: &CanUseTool) -> AskQuestion {
    let mut title = format!("{}: {}", request.tool_name, input_summary(&request.input));
    if let Some(path) = &request.blocked_path {
        title.push_str("\nBlocked path: ");
        title.push_str(path);
    }
    if let Some(reason) = &request.decision_reason {
        title.push_str("\nReason: ");
        title.push_str(reason);
    }
    AskQuestion {
        detail: None,
        title: crate::ask::clip_title(&title),
        options: OPTIONS.map(String::from).to_vec(),
    }
}

/// The input field a person recognises the call by, or the whole input when it has none.
fn input_summary(input: &Map<String, Value>) -> String {
    [
        "command",
        "file_path",
        "notebook_path",
        "url",
        "pattern",
        "path",
    ]
    .into_iter()
    .find_map(|key| input.get(key).and_then(Value::as_str))
    .map(str::to_string)
    .unwrap_or_else(|| Value::Object(input.clone()).to_string())
}

/// The decision the person's option stands for; anything but Allow denies.
pub(crate) fn decision_for(option: usize) -> PermissionDecision {
    if option == ALLOW {
        PermissionDecision::Allow
    } else {
        PermissionDecision::Deny {
            message: DENIED_BY_USER.into(),
        }
    }
}

/// Answers one `can_use_tool` on the spawn's stdin: the chosen option, or a denial when it is
/// dropped unanswered.
struct ClaudeResponder {
    /// `None` once the decision is on its way.
    pending: Option<Pending>,
}

struct Pending {
    slot: Arc<TurnSlot>,
    live: Arc<AtomicBool>,
    request_id: String,
    /// The runtime of the read task that opened it; `respond` and `drop` are synchronous.
    runtime: tokio::runtime::Handle,
}

impl Pending {
    fn answer(self, decision: PermissionDecision) {
        let Self {
            slot,
            live,
            request_id,
            runtime,
        } = self;
        runtime.spawn(async move {
            let response = permission_response(&request_id, &decision);
            if !matches!(decision, PermissionDecision::Allow) {
                if let Err(error) = write_response(&slot, &response).await {
                    tracing::debug!(%error, request_id, "Claude approval cleanup not written");
                }
                return;
            }
            let line = match serde_json::to_string(&response) {
                Ok(line) => line,
                Err(error) => {
                    tracing::error!(%error, "Claude approval response cannot serialize");
                    return;
                }
            };
            let revoked = AtomicBool::new(false);
            let gate = provider::send_boundary::PreSendGate::new(|| {
                Box::pin(async {
                    if !live.load(Ordering::SeqCst)
                        || slot.cause().is_some()
                        || *slot.settled.borrow()
                    {
                        revoked.store(true, Ordering::SeqCst);
                        return Err(provider::send_boundary::SendRefusal(
                            "native approval request ended".into(),
                        ));
                    }
                    Ok(())
                })
            });
            if let Err(error) = slot.write_line(&line, &gate).await {
                if !revoked.load(Ordering::SeqCst) {
                    slot.stdin_failed(&error);
                }
                tracing::debug!(%error, request_id, "Claude approval answer not written");
            }
        });
    }
}

impl HeldResponder for ClaudeResponder {
    fn respond(mut self: Box<Self>, option: usize) {
        if let Some(pending) = self.pending.take() {
            pending.answer(decision_for(option));
        }
    }
}

impl Drop for ClaudeResponder {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.answer(PermissionDecision::Deny {
                message: NOT_ANSWERED.into(),
            });
        }
    }
}

/// Write one control response on the spawn's stdin.
pub(crate) async fn write_response(
    slot: &TurnSlot,
    response: &ControlResponseOut,
) -> crate::error::Result<()> {
    slot.write_internal_line(
        &serde_json::to_string(response)?,
        super::turn_slot::InternalLine::ControlReply,
    )
    .await
    .map_err(super::turn_slot::write_error)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request(value: Value) -> CanUseTool {
        CanUseTool::from_request(&value).unwrap().unwrap()
    }

    #[test]
    fn the_question_names_the_tool_its_target_and_the_cli_reason() {
        let asked = question(&request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "input": {"command": "cargo test", "description": "run the tests"},
            "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash"}]}],
            "blocked_path": "/home/owner/.cargo",
            "decision_reason": "needs to leave the sandbox",
        })));
        assert_eq!(
            asked,
            AskQuestion {
                detail: None,
                title: "Bash: cargo test\nBlocked path: /home/owner/.cargo\nReason: needs to leave the sandbox".into(),
                options: vec!["Allow".into(), "Deny".into()],
            }
        );
        let edit = question(&request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Edit",
            "input": {"file_path": "/ws/a.rs", "old_string": "a", "new_string": "b"},
        })));
        assert_eq!(edit.title, "Edit: /ws/a.rs");
        let other = question(&request(json!({
            "subtype": "can_use_tool",
            "tool_name": "mcp__x__y",
            "input": {"n": 1},
        })));
        assert_eq!(other.title, r#"mcp__x__y: {"n":1}"#);
    }

    #[test]
    fn a_long_command_is_cut_to_the_kernel_bound_not_refused() {
        let asked = question(&request(json!({
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "input": {"command": "x".repeat(10_000)},
        })));
        assert_eq!(asked.title.chars().count(), crate::ask::MAX_TEXT_CHARS);
        assert!(crate::ask::validate_questions(vec![asked]).is_ok());
    }

    #[test]
    fn only_allow_allows() {
        assert_eq!(decision_for(0), PermissionDecision::Allow);
        for option in [1, 2, usize::MAX] {
            assert_eq!(
                decision_for(option),
                PermissionDecision::Deny {
                    message: DENIED_BY_USER.into()
                }
            );
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn approval_cancel_fences_an_allow_waiting_for_the_original_stdin() {
        let (stdin, output) = tokio::io::duplex(4096);
        let slot = Arc::new(TurnSlot::new(stdin, Duration::from_secs(1)));
        let (sender, mut held) = crate::harness::held_requests::channel();
        let Approvals::Held(connection) =
            Approvals::for_spawn(PlannerPermissionMode::Ask, &sender, "spawn")
        else {
            panic!("held");
        };
        let request = CanUseTool::from_request(&serde_json::json!({"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"echo test"}})).unwrap().unwrap();
        connection.open(&slot, "rpc", &request);
        let HeldRequestMessage::Open { responder, .. } = held.recv().await.unwrap() else {
            panic!("open");
        };
        let writer = slot.stdin.lock().await;
        responder.respond(0);
        tokio::task::yield_now().await;
        connection.gone("rpc");
        drop(writer);
        // The original response task was queued behind the writer above. Join
        // its writer interval before the sentinel, using the FIFO writer lock.
        drop(slot.stdin.lock().await);
        write_response(
            &slot,
            &permission_response(
                "sentinel",
                &PermissionDecision::Deny {
                    message: NOT_ANSWERED.into(),
                },
            ),
        )
        .await
        .unwrap();
        let mut line = String::new();
        let mut reader = BufReader::new(output);
        tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            frame["response"]["request_id"], "sentinel",
            "a cancelled request cannot receive a queued Allow: {frame}"
        );
    }
}
