use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkspaceRequirement {
    ProtectedConfigDirectory,
}
impl std::fmt::Display for WorkspaceRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a real .codex directory")
    }
}

use super::{ControllerConfig, Error, Result};
use calm_worker_runtime::Mount;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(super) fn workspace_and_tools(config: &ControllerConfig, workspace: &Path) -> Result<()> {
    let valid = workspace
        .join(".codex")
        .symlink_metadata()
        .is_ok_and(|entry| entry.is_dir() && !entry.file_type().is_symlink());
    if !valid {
        return Err(Error::WorkspacePrecondition(
            WorkspaceRequirement::ProtectedConfigDirectory,
        ));
    }
    for executable in [
        &config.codex_binary,
        &config.code_mode_host_binary,
        &config.sandbox_bwrap,
        &config.mcp_shim,
    ] {
        use std::os::unix::fs::PermissionsExt;
        let metadata = executable.metadata()?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::Configuration(
                "trusted executable must be an executable regular file".into(),
            ));
        }
        if executable.starts_with(workspace) {
            return Err(Error::Configuration(
                "trusted executable is inside writable workspace".into(),
            ));
        }
    }
    Ok(())
}

/// Probe only a trusted, immutable infrastructure executable, before credentials
/// exist. This is not a Worker/code process and does not attest Worker quiescence.
pub(super) async fn helper_capabilities(executable: &Path) -> Result<()> {
    let mut child = tokio::process::Command::new(executable)
        .arg("--help")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Error::Unsupported("cannot probe selected sandbox bwrap".into()))?;
    let mut output = child
        .stdout
        .take()
        .expect("configured piped stdout")
        .take(65_537);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).await?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, bytes))
    })
    .await;
    match result {
        Ok(Ok((status, bytes))) if status.success() && bytes.len() <= 65_536 => {
            let text = String::from_utf8_lossy(&bytes);
            for required in [
                "--argv0",
                "--perms",
                "--ro-bind",
                "--unshare-user",
                "--unshare-net",
            ] {
                if !text.split_whitespace().any(|word| word == required) {
                    return Err(Error::Unsupported(format!(
                        "selected sandbox bwrap lacks {required}"
                    )));
                }
            }
            Ok(())
        }
        _ => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(Error::Unsupported(
                "sandbox bwrap capability probe failed or exceeded bound".into(),
            ))
        }
    }
}

/// Names the codex binary is reachable under inside `/provider-bin`.
const CODEX_ALIASES: [&str; 4] = [
    "codex",
    "codex-linux-sandbox",
    "apply_patch",
    "codex-execve-wrapper",
];
const CODE_MODE_HOST_NAME: &str = "codex-code-mode-host";
const BWRAP_NAME: &str = "bwrap";
/// Every executable the kernel injects under `/provider-bin`; nothing else in
/// the executor's PATH is provided by the kernel.
pub(super) const PROVIDER_BIN_NAMES: [&str; 6] = [
    CODEX_ALIASES[0],
    CODEX_ALIASES[1],
    CODEX_ALIASES[2],
    CODEX_ALIASES[3],
    CODE_MODE_HOST_NAME,
    BWRAP_NAME,
];

pub(super) fn executable_mounts(config: &ControllerConfig) -> Vec<Mount> {
    let mut mounts = Vec::new();
    for name in CODEX_ALIASES {
        mounts.push(Mount {
            source: config.codex_binary.clone(),
            destination: format!("{}/{name}", super::policy::PROVIDER_BIN).into(),
            writable: false,
        });
    }
    mounts.push(Mount {
        source: config.code_mode_host_binary.clone(),
        destination: format!("{}/{CODE_MODE_HOST_NAME}", super::policy::PROVIDER_BIN).into(),
        writable: false,
    });
    mounts.push(Mount {
        source: config.sandbox_bwrap.clone(),
        destination: format!("{}/{BWRAP_NAME}", super::policy::PROVIDER_BIN).into(),
        writable: false,
    });
    mounts.push(Mount {
        source: config.mcp_shim.clone(),
        destination: "/mcp-shim".into(),
        writable: false,
    });
    mounts
}
