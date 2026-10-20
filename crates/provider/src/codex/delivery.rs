//! Ephemeral delivery facts, independent of host retry and queue policy.
use super::request_attempt::{RequestCrossing, RpcAttemptFailure};

#[derive(Debug)]
pub enum StartDelivery<E> {
    NativeAck {
        turn: String,
        cleanup: Option<E>,
    },
    NotWritten {
        error: E,
    },
    Unknown {
        error: E,
        response_turn: Option<String>,
    },
}
#[derive(Debug)]
pub enum SteerDelivery<E> {
    NativeAck { turn: String },
    NotWritten { error: E },
    Unknown { error: E },
}
#[derive(Debug)]
pub enum CompactDelivery<E> {
    NativeAck,
    NotWritten { error: E },
    Unknown { error: E },
}

/// Receipt for the requested thread's durable history cut, separate from host retry policy.
#[derive(Debug)]
pub enum RevertDelivery<E> {
    NativeAck,
    NotWritten { error: E },
    Unknown { error: E },
}

impl StartDelivery<RpcAttemptFailure> {
    pub(super) fn failed(error: RpcAttemptFailure) -> Self {
        match error.crossing {
            RequestCrossing::BeforeSend => Self::NotWritten { error },
            RequestCrossing::AfterSend => Self::Unknown {
                error,
                response_turn: None,
            },
        }
    }
}
impl SteerDelivery<RpcAttemptFailure> {
    pub(super) fn failed(error: RpcAttemptFailure) -> Self {
        match error.crossing {
            RequestCrossing::BeforeSend => Self::NotWritten { error },
            RequestCrossing::AfterSend => Self::Unknown { error },
        }
    }
}
impl CompactDelivery<RpcAttemptFailure> {
    pub(super) fn failed(error: RpcAttemptFailure) -> Self {
        match error.crossing {
            RequestCrossing::BeforeSend => Self::NotWritten { error },
            RequestCrossing::AfterSend => Self::Unknown { error },
        }
    }
}

use super::{CodexAppServer, InputItem, TurnApprovals, TurnModelSelection, Value, json};

impl CodexAppServer {
    /// Managed start facts; the native identity is validated before acknowledging delivery.
    pub async fn turn_start_delivery(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        approvals: TurnApprovals,
        client_user_message_id: Option<&str>,
    ) -> StartDelivery<RpcAttemptFailure> {
        self.turn_start_delivery_gated(
            thread_id,
            input,
            selection,
            approvals,
            client_user_message_id,
            &crate::send_boundary::PreSendGate::new(super::gated_requests::allow_send),
        )
        .await
    }

    pub async fn turn_start_delivery_gated(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        approvals: TurnApprovals,
        client_user_message_id: Option<&str>,
        gate: &crate::send_boundary::PreSendGate<'_>,
    ) -> StartDelivery<RpcAttemptFailure> {
        match self
            .turn_start_attempt_gated(
                thread_id,
                input,
                selection,
                approvals,
                client_user_message_id,
                gate,
            )
            .await
        {
            Ok(result) => match result.turn_id() {
                Some(turn) if !turn.is_empty() => StartDelivery::NativeAck {
                    turn: turn.into(),
                    cleanup: None,
                },
                Some(_) | None => StartDelivery::Unknown {
                    error: RpcAttemptFailure::invalid_result(
                        "turn/start",
                        "turn/start returned no turn.id",
                    ),
                    response_turn: result.turn_id().map(ToOwned::to_owned),
                },
            },
            Err(error) => StartDelivery::failed(error),
        }
    }

    /// Managed steer facts. A schema-valid response still needs the expected native identity.
    pub async fn turn_steer_delivery(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<InputItem>,
        client_user_message_id: Option<&str>,
    ) -> SteerDelivery<RpcAttemptFailure> {
        self.turn_steer_delivery_gated(
            thread_id,
            expected_turn_id,
            input,
            client_user_message_id,
            &crate::send_boundary::PreSendGate::new(super::gated_requests::allow_send),
        )
        .await
    }

    pub async fn turn_steer_delivery_gated(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<InputItem>,
        client_user_message_id: Option<&str>,
        gate: &crate::send_boundary::PreSendGate<'_>,
    ) -> SteerDelivery<RpcAttemptFailure> {
        match self
            .turn_steer_attempt_gated(
                thread_id,
                expected_turn_id,
                input,
                client_user_message_id,
                gate,
            )
            .await
        {
            Ok(result) if !result.turn_id.is_empty() && result.turn_id == expected_turn_id => {
                SteerDelivery::NativeAck {
                    turn: result.turn_id,
                }
            }
            Ok(_) => SteerDelivery::Unknown {
                error: RpcAttemptFailure::invalid_result(
                    "turn/steer",
                    "turn/steer returned absent, empty or mismatched turnId",
                ),
            },
            Err(error) => SteerDelivery::failed(error),
        }
    }

    /// The pinned binary uses `thread/revert`, not the vendored `thread/rollback`.
    /// Only a matching, nonempty native thread receipt acknowledges the cut.
    pub async fn thread_revert_delivery(
        &self,
        thread_id: &str,
        before_turn_id: &str,
    ) -> RevertDelivery<RpcAttemptFailure> {
        match self
            .request_attempt::<super::ThreadRevertResult>(
                "thread/revert",
                json!({ "threadId": thread_id, "beforeTurnId": before_turn_id }),
            )
            .await
        {
            Ok(result) => match result.confirm_thread(thread_id) {
                Ok(()) => RevertDelivery::NativeAck,
                Err(error) => RevertDelivery::Unknown {
                    error: RpcAttemptFailure::local(
                        "thread/revert",
                        None,
                        RequestCrossing::AfterSend,
                        error,
                    ),
                },
            },
            Err(error) => match error.crossing() {
                RequestCrossing::BeforeSend => RevertDelivery::NotWritten { error },
                RequestCrossing::AfterSend => RevertDelivery::Unknown { error },
            },
        }
    }

    /// Compact's RPC acknowledgement does not establish compaction completion.
    pub async fn compact_delivery(&self, thread_id: &str) -> CompactDelivery<RpcAttemptFailure> {
        match self
            .request_attempt::<Value>("thread/compact/start", json!({ "threadId": thread_id }))
            .await
        {
            Ok(result) if result.is_object() => CompactDelivery::NativeAck,
            Ok(_) => CompactDelivery::Unknown {
                error: RpcAttemptFailure::invalid_result(
                    "thread/compact/start",
                    "thread/compact/start returned a non-object result",
                ),
            },
            Err(error) => CompactDelivery::failed(error),
        }
    }
}
