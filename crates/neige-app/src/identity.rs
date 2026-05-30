use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::installed::write_json_atomic;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpawnIdentity {
    /// Canonical binary path used for the spawn.
    pub binary_path: PathBuf,
    /// SHA-256 of the binary bytes at spawn time.
    pub binary_sha256: String,
    /// Semver parsed from `<binary> --version`, when available.
    pub crate_version: Option<String>,
    /// RFC3339 timestamp for when the identity was captured.
    pub captured_at: String,
}

pub(crate) fn capture(bin: &Path) -> anyhow::Result<SpawnIdentity> {
    let binary_path =
        fs::canonicalize(bin).with_context(|| format!("canonicalize {}", bin.display()))?;
    let binary_sha256 =
        sha256_file(&binary_path).with_context(|| format!("sha256 {}", binary_path.display()))?;
    let crate_version = capture_version(bin);
    Ok(SpawnIdentity {
        binary_path,
        binary_sha256,
        crate_version,
        captured_at: chrono::Utc::now().to_rfc3339(),
    })
}

pub(crate) fn supervisor_identity_path(data_dir: &Path) -> PathBuf {
    data_dir.join("state").join("supervisor-identity.json")
}

pub(crate) fn write_supervisor_identity(
    data_dir: &Path,
    identity: &SpawnIdentity,
) -> anyhow::Result<()> {
    write_json_atomic(&supervisor_identity_path(data_dir), identity)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("write to String");
    }
    out
}

fn capture_version(bin: &Path) -> Option<String> {
    let mut child = match Command::new(bin)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(binary = %bin.display(), error = %err, "failed to run --version");
            return None;
        }
    };

    let deadline = Instant::now() + Duration::from_millis(750);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take()
                    && let Err(err) = pipe.read_to_string(&mut stdout)
                {
                    tracing::warn!(binary = %bin.display(), error = %err, "failed to read --version stdout");
                    return None;
                }
                if !status.success() {
                    tracing::warn!(binary = %bin.display(), status = %status, "--version exited unsuccessfully");
                    return None;
                }
                return parse_version_output(&stdout);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!(binary = %bin.display(), "--version timed out");
                return None;
            }
            Err(err) => {
                tracing::warn!(binary = %bin.display(), error = %err, "failed to wait for --version");
                return None;
            }
        }
    }
}

fn parse_version_output(output: &str) -> Option<String> {
    let mut tokens = output.split_whitespace();
    let _binary_name = tokens.next()?;
    let version = tokens.next()?;
    if looks_like_semver(version) {
        Some(version.to_string())
    } else {
        tracing::warn!(output = %output.trim(), "failed to parse crate semver from --version");
        None
    }
}

fn looks_like_semver(value: &str) -> bool {
    let without_build = value.split_once('+').map(|(core, _)| core).unwrap_or(value);
    let core = without_build
        .split_once('-')
        .map(|(core, _)| core)
        .unwrap_or(without_build);
    let mut parts = core.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [major, minor, patch]
            .into_iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_output_handles_current_clap_format() {
        assert_eq!(
            parse_version_output("calm-server 0.1.0\n"),
            Some("0.1.0".into())
        );
        assert_eq!(
            parse_version_output("calm-proc-supervisor 0.5.1\n"),
            Some("0.5.1".into())
        );
    }

    #[test]
    fn parse_version_output_accepts_build_metadata() {
        assert_eq!(
            parse_version_output("calm-server 0.1.0+sha.deadbeef\n"),
            Some("0.1.0+sha.deadbeef".into())
        );
        assert_eq!(
            parse_version_output("calm-server 0.1.0-rc.1+sha.deadbeef\n"),
            Some("0.1.0-rc.1+sha.deadbeef".into())
        );
    }

    #[test]
    fn capture_parses_version_from_executable() {
        let root = test_temp_dir("identity-version");
        let bin = root.join("calm-server");
        write_script(&bin, "printf 'calm-server 0.1.0\\n'\nexit 0\n");

        let identity = capture(&bin).expect("capture");

        assert_eq!(
            identity.binary_path,
            fs::canonicalize(&bin).expect("canonicalize")
        );
        assert_eq!(identity.crate_version, Some("0.1.0".into()));
        assert_eq!(identity.binary_sha256.len(), 64);
    }

    #[test]
    fn capture_returns_none_when_version_output_unparseable() {
        let root = test_temp_dir("identity-version-unparseable");
        let bin = root.join("calm-server");
        write_script(&bin, "printf 'garbage\\n'\nexit 0\n");

        let identity = capture(&bin).expect("capture still succeeds");

        assert_eq!(
            identity.binary_path,
            fs::canonicalize(&bin).expect("canonicalize")
        );
        assert_eq!(identity.crate_version, None);
        assert_eq!(identity.binary_sha256.len(), 64);
    }

    #[test]
    fn capture_returns_none_when_version_times_out() {
        let root = test_temp_dir("identity-version-timeout");
        let bin = root.join("calm-server");
        write_script(&bin, "sleep 2\nprintf 'calm-server 0.1.0\\n'\nexit 0\n");

        let started = Instant::now();
        let identity = capture(&bin).expect("capture still succeeds");

        assert!(started.elapsed() < Duration::from_millis(1000));
        assert_eq!(identity.crate_version, None);
        assert_eq!(identity.binary_sha256.len(), 64);
    }

    #[test]
    fn capture_returns_err_when_binary_path_does_not_exist() {
        let err = capture(Path::new("/no/such/binary")).expect_err("missing path must fail");

        assert!(err.to_string().contains("canonicalize"));
    }

    #[tokio::test]
    async fn capture_is_callable_from_spawn_blocking_inside_tokio() {
        let root = test_temp_dir("identity-spawn-blocking");
        let bin = root.join("calm-server");
        write_script(&bin, "printf 'calm-server 0.1.0\\n'\nexit 0\n");

        let identity = tokio::task::spawn_blocking(move || capture(&bin))
            .await
            .expect("spawn_blocking")
            .expect("capture");

        assert_eq!(identity.crate_version, Some("0.1.0".into()));
    }

    #[test]
    fn capture_returns_none_when_version_command_fails() {
        let root = test_temp_dir("identity-version-fails");
        let bin = root.join("calm-proc-supervisor");
        write_script(&bin, "exit 7\n");

        let identity = capture(&bin).expect("capture still succeeds");

        assert_eq!(identity.crate_version, None);
        assert_eq!(identity.binary_sha256.len(), 64);
    }

    fn write_script(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}")).expect("write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("chmod");
        }
    }

    fn test_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("neige-app-{name}-{}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path).expect("remove stale temp dir");
        }
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
