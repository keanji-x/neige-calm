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
    NativeAck {
        turn: String,
    },
    NotWritten {
        error: E,
    },
    Unknown {
        error: E,
        /// A decoded but invalid ID is retained for the C0 compatibility projection.
        response_turn: Option<String>,
    },
}
#[derive(Debug)]
pub enum CompactDelivery<E> {
    NativeAck,
    NotWritten {
        error: E,
    },
    Unknown {
        error: E,
        /// An RPC result was received, but was not a valid compact acknowledgement object.
        result_received: bool,
    },
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
            RequestCrossing::AfterSend => Self::Unknown {
                error,
                response_turn: None,
            },
        }
    }
}
impl CompactDelivery<RpcAttemptFailure> {
    pub(super) fn failed(error: RpcAttemptFailure) -> Self {
        match error.crossing {
            RequestCrossing::BeforeSend => Self::NotWritten { error },
            RequestCrossing::AfterSend => Self::Unknown {
                error,
                result_received: false,
            },
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
        match self
            .turn_start_attempt(
                thread_id,
                input,
                selection,
                approvals,
                client_user_message_id,
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
        match self
            .turn_steer_attempt(thread_id, expected_turn_id, input, client_user_message_id)
            .await
        {
            Ok(result) if !result.turn_id.is_empty() && result.turn_id == expected_turn_id => {
                SteerDelivery::NativeAck {
                    turn: result.turn_id,
                }
            }
            Ok(result) => SteerDelivery::Unknown {
                error: RpcAttemptFailure::invalid_result(
                    "turn/steer",
                    "turn/steer returned absent, empty or mismatched turnId",
                ),
                response_turn: Some(result.turn_id),
            },
            Err(error) => SteerDelivery::failed(error),
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
                result_received: true,
            },
            Err(error) => CompactDelivery::failed(error),
        }
    }
}
