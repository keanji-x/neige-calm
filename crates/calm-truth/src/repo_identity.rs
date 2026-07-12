//! Bounded, credential-safe discovery of a folder's Git origin.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(2);
const OUTPUT_CAP: usize = 8 * 1024;

/// Probe `origin` without a shell. Identities preserve the remote's case:
/// Git hosting paths may be case-sensitive, while consumers that target a
/// case-insensitive forge can compare them accordingly.
pub fn probe_repo_identity(path: &Path) -> Option<String> {
    if run_git(path, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return None;
    }
    normalize_repo_identity(&run_git(path, &["remote", "get-url", "origin"])?)
}

fn run_git(path: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(path)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain concurrently so a malicious remote value cannot fill the pipe and
    // deadlock the child; retain only the hard-capped prefix.
    let reader = thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0_u8; 1024];
        let mut oversized = false;
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            let room = OUTPUT_CAP.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
            oversized |= n > room;
        }
        (!oversized).then_some(kept)
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let bytes = reader.join().ok()??;
    status
        .success()
        .then(|| String::from_utf8(bytes).ok())
        .flatten()
}

/// Normalize common URL and scp syntaxes to `owner/name`.
///
/// Userinfo (including embedded credentials) and the host are discarded before
/// constructing the result, so neither can reach storage or error messages.
pub fn normalize_repo_identity(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() || remote.contains(['\n', '\r']) {
        return None;
    }
    let path = if let Some(scheme) = remote.find("://") {
        let after_scheme = &remote[scheme + 3..];
        let slash = after_scheme.find('/')?;
        if slash == 0 {
            return None;
        }
        &after_scheme[slash + 1..]
    } else {
        // scp-like: `[user@]host:owner/name`. Reject local paths and Windows
        // drive prefixes; neither denotes a hosted owner/name identity.
        if remote.starts_with(['/', '.']) {
            return None;
        }
        let colon = remote.find(':')?;
        let host = &remote[..colon];
        if host.is_empty() || (host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic()) {
            return None;
        }
        &remote[colon + 1..]
    };
    let path = path
        .trim_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_matches('/'));
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() || !valid_component(owner) || !valid_component(name) {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_and_redaction_table() {
        let cases = [
            ("https://github.com/Owner/Repo.git", Some("Owner/Repo")),
            (
                "https://user:secret@github.com/owner/repo.git",
                Some("owner/repo"),
            ),
            ("ssh://git@github.com/owner/repo", Some("owner/repo")),
            ("git@github.com:owner/repo.git", Some("owner/repo")),
            ("host:owner/repo", Some("owner/repo")),
            ("/tmp/owner/repo", None),
            ("file:///tmp/owner/repo", None),
            ("github.com:owner/repo/extra", None),
            ("https://github.com/owner", None),
            ("https://github.com/owner/re po", None),
            ("https://token@github.com/owner/repo\nsecret", None),
            ("C:\\owner\\repo", None),
            ("", None),
        ];
        for (remote, expected) in cases {
            let actual = normalize_repo_identity(remote);
            assert_eq!(actual.as_deref(), expected, "remote was {remote:?}");
            assert!(!actual.as_deref().unwrap_or_default().contains("secret"));
            assert!(!actual.as_deref().unwrap_or_default().contains("token"));
        }
    }
}
