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
//! written by the failed start itself ([`is_this_failure`]): under systemd,
//! when its invocation id equals the `$MONITOR_INVOCATION_ID` systemd (251
//! and later) gives the `OnFailure=` unit; otherwise when it was written
//! within [`STATE_FRESH_MS`]. A capture that wrote none (one that
//! refused its environment, or was killed by the unit's timeout) is
//! reported as `unknown`. A capture whose run completes clears what was
//! last sent ([`clear_sent`]), so a failure after a recovery always alerts.
//!
//! `notify` builds no SDK client, so it does not run the environment guard:
//! it alerts even when the capture refused its environment. It does need
//! the config, for `notify_command`: with an unreadable or invalid config it
//! cannot alert at all, and exits `FATAL`.
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

use crate::config::Config;
use crate::exit;
use crate::files;
use crate::state::{Invocation, LastExit};
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

    /// A regular file of bounded size only ([`files::read_regular`]).
    fn read(data_dir: &Path) -> Option<NotifyState> {
        let bytes = files::read_regular(&Self::path(data_dir)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// Forget what was last sent, so the next failure alerts whatever its kind.
/// A capture calls this when a run completes.
pub fn clear_sent(data_dir: &Path) {
    match std::fs::remove_file(NotifyState::path(data_dir)) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(error = %err, "cannot clear notify_state.json"),
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

/// Whether `last` is the failed start's own exit state. Under systemd the
/// alert unit gets the failed start's invocation id (`monitor`, from
/// `$MONITOR_INVOCATION_ID`): the state counts only when it carries the
/// same id. Without one (a manual run), the state counts when it is at most
/// [`STATE_FRESH_MS`] old. Either way it must be non-zero.
pub fn is_this_failure(last: &LastExit, monitor: Option<&Invocation>, now: UnixMs) -> bool {
    let matches = match monitor {
        Some(id) => last.invocation.as_ref() == Some(id),
        None => (0..=STATE_FRESH_MS).contains(&(now.0 - last.at)),
    };
    last.code != 0 && matches
}

/// The failure kind and message for `unit` at `now`.
pub fn message(
    unit: &str,
    last: Option<&LastExit>,
    monitor: Option<&Invocation>,
    now: UnixMs,
) -> (String, String) {
    let fresh = last.filter(|l| is_this_failure(l, monitor, now));
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
            for dangling in &l.closed_dangling {
                text.push_str(&format!(
                    ", ended the left-open run of {} partial",
                    dangling.as_of
                ));
                if let Some(failure) = &dangling.failure {
                    text.push_str(&format!(" (most frequent failure {failure})"));
                }
            }
            if l.missed_scan_incomplete {
                text.push_str(", the missed-day scan is incomplete (calendar back-fill failed)");
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
pub fn run(config: &Config, unit: &str, monitor: Option<&Invocation>, now: UnixMs) -> i32 {
    if !valid_unit(unit) {
        tracing::error!("--failed-unit must be a systemd unit name");
        return exit::FATAL;
    }
    let last = LastExit::read(&config.data_dir);
    let (kind, text) = message(unit, last.as_ref(), monitor, now);
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
            if let Err(err) = files::replace(&NotifyState::path(&config.data_dir), &bytes) {
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
            invocation: None,
            code,
            kind,
            at,
            as_of: Some("2026-10-12".into()),
            failure: None,
            closed_dangling: Vec::new(),
            missed_scan_incomplete: false,
        }
    }

    #[test]
    fn only_a_fresh_failing_state_names_the_kind() {
        let now = UnixMs(100 * 60_000);
        let auth = exit_state(75, ExitKind::AuthUnavailable, now.0 - 1_000);
        let (kind, text) = message("u.service", Some(&auth), None, now);
        assert_eq!(kind, "auth_unavailable");
        assert!(
            text.contains("u.service failed: auth_unavailable (exit 75)"),
            "{text}"
        );
        assert!(text.contains("trading day 2026-10-12"), "{text}");
        let stale = exit_state(75, ExitKind::AuthUnavailable, now.0 - STATE_FRESH_MS - 1);
        let ok = exit_state(0, ExitKind::Complete, now.0);
        for last in [None, Some(&stale), Some(&ok)] {
            assert_eq!(message("u.service", last, None, now).0, "unknown");
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

    #[test]
    fn under_systemd_only_the_failed_starts_own_state_counts() {
        let now = UnixMs(100 * 60_000);
        let this = Invocation::parse("0123456789abcdef0123456789abcdef").unwrap();
        let other = Invocation::parse("fedcba9876543210fedcba9876543210").unwrap();
        let mut state = exit_state(75, ExitKind::AuthUnavailable, now.0 - 1_000);
        // Fresh, but written by another start: not this failure.
        state.invocation = Some(other);
        assert!(!is_this_failure(&state, Some(&this), now));
        assert!(!is_this_failure(
            &exit_state(75, ExitKind::AuthUnavailable, now.0),
            Some(&this),
            now
        ));
        // The same start, however old.
        state.invocation = Some(this.clone());
        state.at = 0;
        assert!(is_this_failure(&state, Some(&this), now));
        // A manual run: the freshness rule.
        assert!(!is_this_failure(&state, None, now));
        state.at = now.0 - 1_000;
        assert!(is_this_failure(&state, None, now));
        state.code = 0;
        assert!(!is_this_failure(&state, Some(&this), now));
    }
}
