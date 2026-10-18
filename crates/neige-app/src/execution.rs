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
    provision_key_file(cfg, |file| {
        let secret = CapabilityToken::fresh()?;
        file.write_all(&serde_json::to_vec(&secret)?)?;
        Ok(())
    })
}

fn provision_key_file(
    cfg: &AppConfig,
    write_key: impl FnOnce(&mut std::fs::File) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let path = key_path(cfg);
    std::fs::create_dir_all(cfg.calm_data_dir_resolved())?;
    // Keep incomplete bytes private until the complete capability is durable.
    // NamedTempFile creates a unique 0600 file in the same filesystem and
    // removes it on write/sync/publication errors.
    let mut temporary = tempfile::Builder::new()
        .prefix(".execution-key-")
        .tempfile_in(cfg.calm_data_dir_resolved())?;
    write_key(temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    // link(2) publishes one complete inode and never replaces an established key.
    match std::fs::hard_link(temporary.path(), &path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("publish executor capability"),
    }
    temporary.close()?;
    // Also sync when losing: the winning creator may have exited after linking.
    std::fs::File::open(cfg.calm_data_dir_resolved())?.sync_all()?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o777 == 0o600,
        "executor capability must be a regular 0600 file"
    );
    let _: CapabilityToken = serde_json::from_reader(file).context("read executor capability")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_file_publish_is_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig::starter(temp.path().join("config.toml"));
        cfg.child.data_dir = Some(temp.path().join("data"));
        let key = key_path(&cfg);
        // Interrupt a creator after opening and partially writing its staging file.
        let result = provision_key_file(&cfg, |file| {
            assert!(!key.exists(), "an empty key was published before writing");
            file.write_all(b"{")?;
            assert!(!key.exists(), "a partial key was published");
            anyhow::bail!("interrupted writer")
        });
        assert!(result.is_err());
        assert!(!key.exists());
        assert_eq!(
            std::fs::read_dir(cfg.calm_data_dir_resolved())
                .unwrap()
                .count(),
            0
        );

        // A process killed before cleanup can leave staging bytes; they are never
        // considered a published capability.
        let leftover = cfg
            .calm_data_dir_resolved()
            .join(".execution-key-abandoned");
        std::fs::write(&leftover, b"{").unwrap();
        let barrier = std::sync::Barrier::new(16);
        let keys = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        ensure_key_file(&cfg).unwrap();
                        let bytes = std::fs::read(&key).unwrap();
                        let _: CapabilityToken = serde_json::from_slice(&bytes).unwrap();
                        bytes
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(keys.iter().all(|bytes| bytes == &keys[0]));
        ensure_key_file(&cfg).unwrap();
        assert_eq!(std::fs::read(&key).unwrap(), keys[0]);
        // Force a creator to lose publication after its complete write.
        provision_key_file(&cfg, |file| {
            let secret = CapabilityToken::fresh()?;
            file.write_all(&serde_json::to_vec(&secret)?)?;
            ensure_key_file(&cfg)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(&key).unwrap(), keys[0]);
        assert_eq!(std::fs::read(&leftover).unwrap(), b"{");
        assert_eq!(
            std::fs::read_dir(cfg.calm_data_dir_resolved())
                .unwrap()
                .count(),
            2
        );
    }

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
