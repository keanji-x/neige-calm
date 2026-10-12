//! `research-archive notify --failed-unit <unit>`: the `OnFailure=` alert.
//!
//! It reads `last_exit.json`, builds a one-line message from it, and runs
//! the configured `notify_command` with the message as one more argument.
//! It sends only when the failure kind differs from the last one notified,
//! or when [`RESEND_AFTER_MS`] has passed since that notification, so a dead
//! login alerts once rather than on every tick. What was last sent is kept
//! in `<data_dir>/notify_state.json`, written only after the command
//! succeeded.
//!
//! The exit state counts for this failure only when it is non-zero and was
//! written within [`STATE_FRESH_MS`]. A failure that wrote none (a refused
//! environment, a bad config, a kill by the unit's timeout) is reported as
//! `unknown`.
//!
//! The command runs without a shell, with stdin closed and an environment
//! built from [`ENV_ALLOWLIST`] alone: the job's own environment is not
//! inherited, and no token data is passed.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::atomic_file;
use crate::config::Config;
use crate::exit;
use crate::state::LastExit;
use crate::types::UnixMs;

/// A notification of the same kind is repeated after six hours.
pub const RESEND_AFTER_MS: i64 = 6 * 3_600_000;

/// An exit state older than this at notify time belongs to an earlier start.
pub const STATE_FRESH_MS: i64 = 15 * 60_000;

/// The longest the notify command may run.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// The variables passed to the notify command, when set: a command search
/// path, a home and locale for the tool, its runtime directory, and an HTTP
/// proxy for reaching the alert service.
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "TZ",
    "XDG_RUNTIME_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

/// What was last sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyState {
    pub kind: String,
    /// Wall time of the send, Unix ms.
    pub at: i64,
}

impl NotifyState {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("notify_state.json")
    }

    fn read(data_dir: &Path) -> Option<NotifyState> {
        let bytes = std::fs::read(Self::path(data_dir)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// A systemd unit name as `%n` gives it: `[A-Za-z0-9@._:-]`, at most 256.
pub fn valid_unit(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b':' | b'-'))
}

/// The failure kind and message for `unit` at `now`.
pub fn message(unit: &str, last: Option<&LastExit>, now: UnixMs) -> (String, String) {
    let fresh = last.filter(|l| l.code != 0 && (0..=STATE_FRESH_MS).contains(&(now.0 - l.at)));
    match fresh {
        Some(l) => {
            let at = chrono::DateTime::from_timestamp_millis(l.at)
                .map(|t| t.to_rfc3339())
                .unwrap_or_default();
            let mut text = format!(
                "research-archive: {unit} failed: {} (exit {}) at {at}",
                l.kind.as_str(),
                l.code
            );
            if let Some(as_of) = &l.as_of {
                text.push_str(&format!(", trading day {as_of}"));
            }
            if let Some(failure) = &l.failure {
                text.push_str(&format!(", most frequent failure {failure}"));
            }
            (l.kind.as_str().to_owned(), text)
        }
        None => (
            "unknown".to_owned(),
            format!("research-archive: {unit} failed: unknown (it recorded no exit state)"),
        ),
    }
}

/// Whether to send `kind` at `now` after `last` was sent.
pub fn due(kind: &str, last: Option<&NotifyState>, now: UnixMs) -> bool {
    match last {
        None => true,
        Some(sent) => sent.kind != kind || now.0 - sent.at >= RESEND_AFTER_MS || now.0 < sent.at,
    }
}

/// The notify command. Returns the exit code.
pub fn run(config: &Config, unit: &str, now: UnixMs) -> i32 {
    if !valid_unit(unit) {
        tracing::error!("--failed-unit must be a systemd unit name");
        return exit::FATAL;
    }
    let last = LastExit::read(&config.data_dir);
    let (kind, text) = message(unit, last.as_ref(), now);
    if !due(&kind, NotifyState::read(&config.data_dir).as_ref(), now) {
        tracing::info!(%kind, "already notified for this failure kind; not repeated");
        return exit::OK;
    }
    let env = std::env::vars_os().filter(|(name, _)| ENV_ALLOWLIST.iter().any(|a| name == *a));
    match send(&config.notify_command, &text, env) {
        Ok(()) => {
            let state = NotifyState { kind, at: now.0 };
            let bytes =
                serde_json::to_vec_pretty(&state).expect("a notify state always serializes");
            if let Err(err) = atomic_file::replace(&NotifyState::path(&config.data_dir), &bytes) {
                tracing::error!(error = %err, "cannot write notify_state.json");
            }
            exit::OK
        }
        Err(reason) => {
            tracing::error!(%reason, "the notify command failed");
            exit::FATAL
        }
    }
}

/// Run `argv` plus `text` with only `env`, stdin closed, and wait at most
/// [`COMMAND_TIMEOUT`].
fn send(
    argv: &[String],
    text: &str,
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<(), String> {
    let (program, args) = argv.split_first().ok_or("notify_command is empty")?;
    let mut child = Command::new(program)
        .args(args)
        .arg(text)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start it: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => return Err(format!("it exited with {status}")),
            None if started.elapsed() >= COMMAND_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("it ran longer than {COMMAND_TIMEOUT:?}"));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ExitKind;

    fn exit_state(code: i32, kind: ExitKind, at: i64) -> LastExit {
        LastExit {
            code,
            kind,
            at,
            as_of: Some("2026-10-12".into()),
            failure: None,
        }
    }

    #[test]
    fn only_a_fresh_failing_state_names_the_kind() {
        let now = UnixMs(100 * 60_000);
        let auth = exit_state(75, ExitKind::AuthUnavailable, now.0 - 1_000);
        let (kind, text) = message("u.service", Some(&auth), now);
        assert_eq!(kind, "auth_unavailable");
        assert!(
            text.contains("u.service failed: auth_unavailable (exit 75)"),
            "{text}"
        );
        assert!(text.contains("trading day 2026-10-12"), "{text}");
        let stale = exit_state(75, ExitKind::AuthUnavailable, now.0 - STATE_FRESH_MS - 1);
        let ok = exit_state(0, ExitKind::Complete, now.0);
        for last in [None, Some(&stale), Some(&ok)] {
            assert_eq!(message("u.service", last, now).0, "unknown");
        }
    }

    #[test]
    fn a_kind_repeats_only_after_six_hours() {
        let sent = NotifyState {
            kind: "partial".into(),
            at: 1_000,
        };
        assert!(due("partial", None, UnixMs(1_000)));
        assert!(!due(
            "partial",
            Some(&sent),
            UnixMs(1_000 + RESEND_AFTER_MS - 1)
        ));
        assert!(due("partial", Some(&sent), UnixMs(1_000 + RESEND_AFTER_MS)));
        assert!(due("auth_unavailable", Some(&sent), UnixMs(1_001)));
    }

    #[test]
    fn unit_names_are_systemd_names() {
        assert!(valid_unit("research-archive-capture.service"));
        assert!(valid_unit("a@b:c_d.service"));
        for bad in ["", "a b", "a;b", "a\nb", "a/b"] {
            assert!(!valid_unit(bad), "{bad:?}");
        }
    }
}
