//! `<data_dir>/last_exit.json`: how the last capture that held the lock
//! ended. The capture writes it atomically just before it exits, while it
//! still holds the lock; `notify` reads it. It holds only enums, numbers and
//! a date: no token, no server text.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exit;
use crate::files;
use crate::types::{AsOf, UnixMs};

/// How a capture ended. Each kind has one exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitKind {
    /// The run ended `complete`; every planned pair has an `ok`.
    Complete,
    /// The run ended `complete`, but some pairs ended with their attempt cap
    /// exhausted and no `ok`. See `failure` for the most frequent cause.
    CompleteWithFailures,
    /// The run ended `complete` with fewer than
    /// [`DEGRADED_BELOW`](crate::capture::DEGRADED_BELOW) of its planned pairs
    /// having an `ok`. Alerts; see `failure`.
    Degraded,
    /// The day's run had already ended; nothing to do.
    AlreadyEnded,
    /// The attempt timeout came while the window is still open; the run
    /// stays open and the next start resumes it.
    Resumable,
    /// No capture window is open.
    NoWindow,
    /// Another process holds the lock. Never written to the file.
    Busy,
    /// A window closed with its run ended `partial`: this run's, or a
    /// dangling one closed at start.
    Partial,
    /// No usable token; the run, if any, stays open.
    AuthUnavailable,
    /// Anything else that stopped the job.
    Fatal,
}

impl ExitKind {
    pub fn code(self) -> i32 {
        match self {
            ExitKind::Complete
            | ExitKind::CompleteWithFailures
            | ExitKind::AlreadyEnded
            | ExitKind::Resumable => exit::OK,
            ExitKind::Degraded => exit::DEGRADED,
            ExitKind::NoWindow => exit::NO_WINDOW,
            ExitKind::Busy => exit::BUSY,
            ExitKind::Partial => exit::PARTIAL,
            ExitKind::AuthUnavailable => exit::AUTH_UNAVAILABLE,
            ExitKind::Fatal => exit::FATAL,
        }
    }

    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            ExitKind::Complete => "complete",
            ExitKind::CompleteWithFailures => "complete_with_failures",
            ExitKind::Degraded => "degraded",
            ExitKind::AlreadyEnded => "already_ended",
            ExitKind::Resumable => "resumable",
            ExitKind::NoWindow => "no_window",
            ExitKind::Busy => "busy",
            ExitKind::Partial => "partial",
            ExitKind::AuthUnavailable => "auth_unavailable",
            ExitKind::Fatal => "fatal",
        }
    }
}

/// A dangling run this start ended `partial`: its day and its most frequent
/// counted failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dangling {
    pub as_of: AsOf,
    pub failure: Option<String>,
}

/// The capture's result: the kind, the trading day it concerns, for an
/// ending with failures the most frequent failure as `error_class:error_code`
/// (for example `envelope:401004`), and a dangling run closed at the start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub kind: ExitKind,
    pub as_of: Option<AsOf>,
    pub failure: Option<String>,
    pub closed_dangling: Option<Dangling>,
}

impl Outcome {
    pub fn new(kind: ExitKind) -> Outcome {
        Outcome {
            kind,
            as_of: None,
            failure: None,
            closed_dangling: None,
        }
    }

    pub fn of(kind: ExitKind, as_of: AsOf) -> Outcome {
        Outcome {
            kind,
            as_of: Some(as_of),
            failure: None,
            closed_dangling: None,
        }
    }

    pub fn code(&self) -> i32 {
        self.kind.code()
    }
}

/// The file's content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastExit {
    pub code: i32,
    pub kind: ExitKind,
    /// Wall time of the exit, Unix ms.
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// The day of a dangling run this start ended `partial`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_dangling: Option<String>,
    /// That run's most frequent counted failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_dangling_failure: Option<String>,
}

impl LastExit {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("last_exit.json")
    }

    pub fn from_outcome(outcome: &Outcome, at: UnixMs) -> LastExit {
        LastExit {
            code: outcome.code(),
            kind: outcome.kind,
            at: at.0,
            as_of: outcome.as_of.map(|d| d.to_string()),
            failure: outcome.failure.clone(),
            closed_dangling: outcome
                .closed_dangling
                .as_ref()
                .map(|d| d.as_of.to_string()),
            closed_dangling_failure: outcome
                .closed_dangling
                .as_ref()
                .and_then(|d| d.failure.clone()),
        }
    }

    /// Write the file atomically. A failure is logged, not fatal: the exit
    /// code still tells systemd.
    pub fn write(&self, data_dir: &Path) {
        let bytes = serde_json::to_vec_pretty(self).expect("an exit state always serializes");
        if let Err(err) = files::replace(&Self::path(data_dir), &bytes) {
            tracing::error!(error = %err, "cannot write last_exit.json");
        }
    }

    /// The file, or `None` when it is missing or unreadable (logged). Only
    /// a regular file of bounded size is read ([`files::read_regular`]).
    pub fn read(data_dir: &Path) -> Option<LastExit> {
        let path = Self::path(data_dir);
        let bytes = match files::read_regular(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::warn!(error = %err, "cannot read last_exit.json");
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(state) => Some(state),
            Err(err) => {
                tracing::warn!(
                    line = err.line(),
                    column = err.column(),
                    "last_exit.json is not valid"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_have_one_code_and_round_trip() {
        let all = [
            ExitKind::Complete,
            ExitKind::CompleteWithFailures,
            ExitKind::Degraded,
            ExitKind::AlreadyEnded,
            ExitKind::Resumable,
            ExitKind::NoWindow,
            ExitKind::Busy,
            ExitKind::Partial,
            ExitKind::AuthUnavailable,
            ExitKind::Fatal,
        ];
        for kind in all {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<ExitKind>(&json).unwrap(), kind);
        }
        let alerting: Vec<_> = all.iter().filter(|k| k.code() != 0).collect();
        assert_eq!(
            alerting,
            [
                &ExitKind::Degraded,
                &ExitKind::Partial,
                &ExitKind::AuthUnavailable,
                &ExitKind::Fatal
            ]
        );
    }

    #[test]
    fn the_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(LastExit::read(dir.path()), None);
        let state = LastExit {
            code: 3,
            kind: ExitKind::Partial,
            at: 42,
            as_of: Some("2026-10-12".into()),
            failure: Some("envelope:401004".into()),
            closed_dangling: Some("2026-10-09".into()),
            closed_dangling_failure: Some("http_status:503".into()),
        };
        state.write(dir.path());
        assert_eq!(LastExit::read(dir.path()), Some(state));
    }

    #[test]
    fn a_fifo_in_place_of_the_file_is_not_waited_on() {
        let dir = tempfile::tempdir().unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(LastExit::path(dir.path()))
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let path = dir.path().to_owned();
        std::thread::spawn(move || tx.send(LastExit::read(&path)));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(None));
    }
}
