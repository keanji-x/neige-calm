//! Control-plane messages between calm-server and calm-proc-supervisor.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMsg {
    EnsureProc(EnsureProcRequest),
    Attach(AttachRequest),
    WriteStdin(WriteStdinRequest),
    ResizePty(ResizePtyRequest),
    Signal(SignalRequest),
    Cleanup(CleanupRequest),
    Probe(ProbeRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureProcRequest {
    pub proc_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub envs: Vec<(String, String)>,
    pub cwd: String,
    pub ready_timeout_ms: u64,
    pub io_mode: IoMode,
    pub replay_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachRequest {
    pub proc_id: String,
    pub from_cursor: Option<u64>,
    pub reader_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteStdinRequest {
    pub proc_id: String,
    pub bytes: Vec<u8>,
    pub write_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizePtyRequest {
    pub proc_id: String,
    pub cols: u16,
    pub rows: u16,
    pub pixel_w: u16,
    pub pixel_h: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalRequest {
    pub proc_id: String,
    pub sig: ProcSignal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupRequest {
    pub proc_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub proc_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attached {
    pub proc_id: String,
    pub running: bool,
    pub cursor_head: u64,
    pub cursor_tail: u64,
    pub replay: Vec<u8>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlErrorKind {
    UnknownProc,
    WrongState,
    BadRequest,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IoMode {
    Pipe,
    Pty { cols: u16, rows: u16 },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcSignal {
    Term,
    Kill,
    Hup,
}

/// Evidence about this EnsureProc attempt, independent of child reaping.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpawnFailedDisposition {
    /// No executable child was created by this request.
    NoChildCreated,
    /// A child may have been created; this is never disposal permission.
    Unknown,
}

/// Two-phase reply: Spawned precedes readiness; SpawnFailed has required creation evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlReply {
    /// Process forked; pid is final. Client persists pid + handle now.
    Spawned {
        pid: u32,
    },
    /// Daemon wrote its ready signal. Spawn fully succeeded.
    Ready,
    /// Readiness failed after spawn (child exited early or the ready-fd backstop timed out); `pid` is still valid for rollback reap.
    ReadyFailed {
        error: String,
        child_already_reaped: bool,
    },
    /// No Spawned acknowledgement. Only the required disposition classifies creation evidence.
    SpawnFailed {
        error: String,
        child_already_reaped: bool,
        disposition: SpawnFailedDisposition,
    },
    AttachOk(Attached),
    WriteAck {
        write_seq: u64,
    },
    ResizeOk,
    SignalOk,
    /// "This proc is scheduled for reclaim", **not** "already gone from the registry": removal can be deferred
    /// to a later periodic sweep, so clients must not assert the entry is absent.
    CleanupOk,
    ProbeOk {
        supervisor_version: u32,
        proc_running: bool,
    },
    Error {
        kind: ControlErrorKind,
        message: String,
    },
    Output {
        proc_id: String,
        cursor: u64,
        bytes: Vec<u8>,
    },
    Gap {
        earliest_cursor: u64,
        requested_cursor: u64,
    },
    Exited {
        proc_id: String,
        status: Option<i32>,
        signalled: bool,
        cursor: u64,
    },
}

#[cfg(test)]
mod spawn_failed_tests {
    use super::*;

    #[tokio::test]
    async fn spawn_failed_required_disposition_roundtrips() {
        for disposition in [
            SpawnFailedDisposition::NoChildCreated,
            SpawnFailedDisposition::Unknown,
        ] {
            let (mut send, mut receive) = tokio::io::duplex(4096);
            crate::write_frame(
                &mut send,
                &ControlReply::SpawnFailed {
                    error: "failed".into(),
                    child_already_reaped: false,
                    disposition,
                },
            )
            .await
            .unwrap();
            assert!(
                matches!(crate::read_frame::<ControlReply, _>(&mut receive).await.unwrap(),
                ControlReply::SpawnFailed { disposition: observed, .. } if observed == disposition)
            );
        }
    }

    #[tokio::test]
    async fn spawn_failed_legacy_receipt_fails_closed() {
        // Exact v1 enum discriminants and SpawnFailed shape; no policy is emulated.
        #[derive(Serialize)]
        #[allow(dead_code)]
        enum LegacyReply {
            Spawned {
                pid: u32,
            },
            Ready,
            ReadyFailed {
                error: String,
                child_already_reaped: bool,
            },
            SpawnFailed {
                error: String,
                child_already_reaped: bool,
            },
        }
        for error in ["", "x", "pty cwd is not a directory"] {
            let (mut send, mut receive) = tokio::io::duplex(4096);
            crate::write_frame(
                &mut send,
                &LegacyReply::SpawnFailed {
                    error: error.into(),
                    child_already_reaped: false,
                },
            )
            .await
            .unwrap();
            assert!(
                crate::read_frame::<ControlReply, _>(&mut receive)
                    .await
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<ControlReply>(serde_json::json!({"SpawnFailed": {
                "error":"unknown", "child_already_reaped":false, "disposition":"future"
            }}))
            .is_err()
        );
    }
}
