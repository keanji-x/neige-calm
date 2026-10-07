//! A Claude Planner's tool approvals (#2348): the adapter between the CLI's `can_use_tool` and the
//! harness's held-request channel.
//!
//! The card's permission mode is read at every spawn. Under `ask` the spawn runs with
//! `--permission-prompt-tool stdio`; each `can_use_tool` becomes an `Open` whose
//! [`ClaudeResponder`] writes the decision on the child's stdin, and each
//! `control_cancel_request` becomes a `Gone`. The spawn is the connection: the
//! [`SpawnConnection`] its read task holds pushes `ConnectionLost` when that task ends, after the
//! last stdout line was read, so no `Open` of the spawn can follow it. Under `never`
//! (`--permission-prompts none`) a `can_use_tool` is not expected and is refused where it is read.

use std::sync::Arc;

use serde_json::{Map, Value};

use super::protocol::ControlResponseOut;
use super::session::TurnSlot;
use crate::db::Repo;
use crate::error::CalmError;
use crate::event::AskQuestion;
use crate::harness::backend::TurnStartFailure;
use crate::harness::held_requests::{
    ConnectionId, HeldRequestMessage, HeldRequestSender, HeldResponder, RequestKey,
};
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::claude::permission::{CanUseTool, PermissionDecision, permission_response};

/// The options of every approval question, in the order the answer's index names them.
pub(crate) const OPTIONS: [&str; 2] = ["Allow", "Deny"];
const ALLOW: usize = 0;
/// What the model reads when the person chose Deny.
pub(crate) const DENIED_BY_USER: &str = "The user denied this tool use.";
/// What the model reads when nobody answered: the ask could not be raised, or it was withdrawn.
pub(crate) const NOT_ANSWERED: &str =
    "This tool use was not approved: nobody answered the request for it.";
/// What a `never` spawn answers a `can_use_tool` it should not have been sent.
pub(crate) const NO_APPROVAL_SURFACE: &str = "this Planner has no approval surface";

/// The card's permission mode as it stands at this spawn, read only through
/// [`crate::planner_permission_mode::read`]. A stored value that cannot be read refuses the turn
/// the way an unreadable model selection does: the reader is told and the message stays queued. It
/// is never taken for either mode.
pub(crate) async fn mode_at_spawn(
    repo: &dyn Repo,
    card_id: &str,
) -> Result<PlannerPermissionMode, TurnStartFailure> {
    let card = repo
        .card_get(card_id)
        .await
        .map_err(CalmError::from)?
        .ok_or_else(|| CalmError::NotFound(format!("planner card {card_id}")))?;
    crate::planner_permission_mode::read(&card.payload).map_err(|malformed| {
        TurnStartFailure::Refused {
            error: CalmError::Conflict(malformed.to_string()),
            reader: "This conversation's saved permission mode cannot be read. Choose a \
                     permission mode to replace it. Your message is still queued."
                .into(),
        }
    })
}

/// How one spawn answers `can_use_tool`, fixed by the permission mode read at the spawn.
pub(crate) enum Approvals {
    /// `never`: refused where it is read.
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
            PlannerPermissionMode::Never => Self::Refused,
            PlannerPermissionMode::Ask => Self::Held(SpawnConnection {
                sender: held.clone(),
                connection: ConnectionId(spawn_id.to_string()),
            }),
        }
    }
}

/// One spawn as a connection of the harness's held-request channel. Dropping it pushes
/// `ConnectionLost`, which withdraws every request of the spawn the harness still holds.
pub(crate) struct SpawnConnection {
    sender: HeldRequestSender,
    connection: ConnectionId,
}

impl SpawnConnection {
    /// Put `request` to the person; the turn keeps reading while it waits. A harness that is gone
    /// drops the message, and with it the responder, which refuses the request.
    pub(crate) fn open(&self, slot: &Arc<TurnSlot>, request_id: &str, request: &CanUseTool) {
        let responder = ClaudeResponder {
            pending: Some(Pending {
                slot: Arc::clone(slot),
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
        let _ = self.sender.send(HeldRequestMessage::Gone {
            request_key: RequestKey(request_id.to_string()),
        });
    }
}

impl Drop for SpawnConnection {
    fn drop(&mut self) {
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
    request_id: String,
    /// The runtime of the read task that opened it; `respond` and `drop` are synchronous.
    runtime: tokio::runtime::Handle,
}

impl Pending {
    fn answer(self, decision: PermissionDecision) {
        let Self {
            slot,
            request_id,
            runtime,
        } = self;
        runtime.spawn(async move {
            // A spawn that already ended has closed its stdin; nothing is waiting on the answer.
            if let Err(error) =
                write_response(&slot, &permission_response(&request_id, &decision)).await
            {
                tracing::debug!(%error, request_id, "claude planner: approval answer not written");
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
    slot.write_line(&serde_json::to_string(response)?).await
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
