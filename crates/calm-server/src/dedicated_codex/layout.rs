use super::{ControllerConfig, Error, HomeReceipt, Result};
use crate::codex_appserver::{CodexAppServer, NotificationStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// Canonicalize the existing ancestor without publishing any private directory.
fn destination(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path
        .components()
        .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::Configuration(
            "private root must use a normalized path".into(),
        ));
    }
    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    loop {
        match ancestor.symlink_metadata() {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.file_name().ok_or_else(|| {
                    Error::Configuration("private root has no existing ancestor".into())
                })?);
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| Error::Configuration("private root has no parent".into()))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut result = ancestor.canonicalize()?;
    for part in missing.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}

pub(super) fn private_sources(config: &mut ControllerConfig) -> Result<()> {
    config.private_root = destination(&config.private_root)?;
    config.codex_binary = config.codex_binary.canonicalize()?;
    config.mcp_shim = config.mcp_shim.canonicalize()?;
    config.sandbox_bwrap = config.sandbox_bwrap.canonicalize()?;
    let binary_directory = config
        .codex_binary
        .parent()
        .ok_or_else(|| Error::Configuration("provider binary directory required".into()))?;
    // Runtime binds /usr; /bin,/sbin,/lib,/lib64 are aliases INTO that mount.
    // Also fence all explicit readonly provider inputs, including TLS directories.
    let mut sources = vec![
        binary_directory.to_path_buf(),
        config.mcp_shim.clone(),
        config.sandbox_bwrap.clone(),
    ];
    for path in [
        "/usr",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/ssl/certs",
        "/etc/codex/requirements.toml",
        "/etc/codex/managed_config.toml",
    ] {
        let path = Path::new(path);
        if path.try_exists()? {
            sources.push(path.canonicalize()?);
        }
    }
    if sources.iter().any(|source| {
        source.starts_with(&config.private_root) || config.private_root.starts_with(source)
    }) {
        return Err(Error::Configuration(
            "private root overlaps a provider/code-readable mount source".into(),
        ));
    }
    Ok(())
}

pub(super) async fn connect(home: &HomeReceipt) -> Result<(CodexAppServer, NotificationStream)> {
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&home.control)?;
    let socket = PathBuf::from(format!(
        "/proc/self/fd/{}/app-server.sock",
        directory.as_raw_fd()
    ));
    // The owned directory must stay alive through connect AND the WS handshake.
    let result = CodexAppServer::connect(&socket)
        .await
        .map_err(|_| Error::Unknown("dedicated socket connection unavailable".into()));
    drop(directory);
    result
}
