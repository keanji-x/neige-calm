//! Host-owned capability provisioned once and shared through explicit child arguments.
use crate::config::AppConfig;
use anyhow::Context;
use calm_session::control::execution::CapabilityToken;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{fs::OpenOptions, io::Write, path::PathBuf};

pub(crate) fn key_path(cfg: &AppConfig) -> PathBuf {
    cfg.calm_data_dir_resolved()
        .join("execution-client-key.json")
}

pub(crate) fn ensure_key_file(cfg: &AppConfig) -> anyhow::Result<()> {
    let path = key_path(cfg);
    std::fs::create_dir_all(cfg.calm_data_dir_resolved())?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            let secret = CapabilityToken::fresh()?;
            file.write_all(&serde_json::to_vec(&secret)?)?;
            file.sync_all()?;
            std::fs::File::open(cfg.calm_data_dir_resolved())?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            let metadata = file.metadata()?;
            anyhow::ensure!(
                metadata.is_file() && metadata.permissions().mode() & 0o777 == 0o600,
                "executor capability must be a regular 0600 file"
            );
            let _: CapabilityToken =
                serde_json::from_reader(file).context("read executor capability")?;
        }
        Err(error) => return Err(error).context("create executor capability"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supervisor_execution_args_and_key_file_mode() {
        let temp = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig::starter(temp.path().join("config.toml"));
        cfg.child.data_dir = Some(temp.path().join("data with spaces"));
        ensure_key_file(&cfg).unwrap();
        let key = key_path(&cfg);
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bytes = std::fs::read(&key).unwrap();
        let _: CapabilityToken = serde_json::from_slice(&bytes).unwrap();
        ensure_key_file(&cfg).unwrap();
        assert!(std::fs::read(&key).unwrap() == bytes);
        let supervisor = crate::supervisor::proc_supervisor_config(&cfg);
        let server = crate::supervisor::calm_server_supervisor_config(&cfg);
        for (flag, expected) in [
            (
                "--execution-state-dir",
                cfg.calm_data_dir_resolved()
                    .join("execution")
                    .display()
                    .to_string(),
            ),
            ("--kernel-client-key-file", key.display().to_string()),
            (
                "--stop-grace-ms",
                cfg.timing.stop_grace.as_millis().to_string(),
            ),
        ] {
            assert!(
                supervisor
                    .child_args
                    .windows(2)
                    .any(|pair| pair == [flag, &expected])
            );
        }
        assert!(
            server
                .child_args
                .windows(2)
                .any(|pair| pair == ["--kernel-client-key-file", &key.display().to_string()])
        );
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ensure_key_file(&cfg).is_err());
    }
}
