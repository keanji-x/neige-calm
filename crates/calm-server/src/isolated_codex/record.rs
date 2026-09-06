//! Private physical evidence retained in the task's existing Operation output.
use crate::dedicated_codex::{DedicatedRequest, RequestPhase, SessionRecord, StopState};
use crate::error::{CalmError, Result};
use crate::operation::TxOutput;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordVersion {
    #[serde(rename = "isolated-run-v1")]
    V1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    Open,
    Closed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "record",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ProviderRecord {
    Unprepared,
    Prepared(Box<SessionRecord>),
}

/// Never expose private home, launch environment, or native token as card data.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub version: RecordVersion,
    pub request: DedicatedRequest,
    pub track_id: String,
    pub native_token: String,
    pub admission: Admission,
    pub provider: ProviderRecord,
}
impl std::fmt::Debug for RunRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRecord")
            .field("identity", &self.request.identity)
            .field("admission", &self.admission)
            .finish_non_exhaustive()
    }
}
impl RunRecord {
    pub fn from_output(output: &TxOutput) -> Result<Self> {
        let record: Self =
            serde_json::from_value(output.data.get("isolated_execution").cloned().ok_or_else(
                || CalmError::Conflict("isolated execution receipt is missing".into()),
            )?)?;
        if output.target_type != "card"
            || output.target_id.as_deref() != Some(&record.request.identity.card_id)
        {
            return Err(CalmError::Conflict(
                "isolated execution target changed".into(),
            ));
        }
        Ok(record)
    }
    pub fn session(&self) -> Result<&SessionRecord> {
        match &self.provider {
            ProviderRecord::Prepared(session) => Ok(session),
            ProviderRecord::Unprepared => Err(CalmError::Conflict(
                "isolated provider has no prepared receipt".into(),
            )),
        }
    }
}

/// Only pre-effect transitions ask current start permission. Acknowledgements and
/// closing evidence remain writable after task completion or withdrawn intent.
pub fn needs_start_permission(expected: &SessionRecord, next: &SessionRecord) -> Result<bool> {
    let error = || CalmError::Conflict("invalid isolated controller transition".into());
    if expected.endpoint != next.endpoint {
        return Err(error());
    }
    if expected.phase == next.phase {
        return match (&expected.stop, &next.stop) {
            (StopState::Open | StopState::Requested, StopState::Requested) => Ok(false),
            (StopState::Requested, StopState::Quiesced(proof))
                if proof.handle == expected.endpoint.boundary =>
            {
                Ok(false)
            }
            (StopState::Open, StopState::Open)
                if expected.phase == RequestPhase::ProviderStarting =>
            {
                Ok(true)
            }
            _ => Err(error()),
        };
    }
    if expected.stop != StopState::Open || next.stop != StopState::Open {
        return Err(error());
    }
    use RequestPhase::*;
    match (&expected.phase, &next.phase) {
        (Prepared, ProviderStarting) | (Connected, CreatingThread) => Ok(true),
        (
            ThreadReady { thread_id: a },
            IssuingTurn {
                thread_id: b,
                request_key,
                prompt_digest,
            },
        ) if a == b && !request_key.is_empty() && !prompt_digest.is_empty() => Ok(true),
        (ProviderStarting, Connected) => Ok(false),
        (CreatingThread, ThreadReady { thread_id }) if !thread_id.is_empty() => Ok(false),
        (
            IssuingTurn {
                thread_id: a,
                request_key: ka,
                prompt_digest: da,
            },
            TurnActive {
                thread_id: b,
                turn_id,
                request_key: kb,
                prompt_digest: db,
            },
        ) if a == b && ka == kb && da == db && !turn_id.is_empty() => Ok(false),
        _ => Err(error()),
    }
}
