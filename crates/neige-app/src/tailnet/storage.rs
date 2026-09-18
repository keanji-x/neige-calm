use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DesiredState {
    pub schema_version: u32,
    pub config_revision: u64,
    pub desired_enabled: bool,
}

pub(super) fn lock_directory(dir: &Path) -> anyhow::Result<File> {
    std::fs::create_dir_all(dir)?;
    let metadata = std::fs::symlink_metadata(dir)?;
    anyhow::ensure!(
        metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
        "Tailnet directory must be owned by this user and not a symlink"
    );
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir.join("host.lock"))?;
    anyhow::ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "Another neige-app owns this Tailnet state"
    );
    Ok(lock)
}
pub(super) fn load(dir: &Path) -> anyhow::Result<DesiredState> {
    let path = dir.join("desired.json");
    match std::fs::read(path) {
        Ok(bytes) => {
            let state: DesiredState = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                state.schema_version == 1,
                "Unsupported Tailnet desired state"
            );
            Ok(state)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DesiredState {
            schema_version: 1,
            config_revision: 1,
            desired_enabled: false,
        }),
        Err(e) => Err(e.into()),
    }
}
pub(super) fn atomic_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let temp: PathBuf = path.with_extension(format!("tmp.{}", std::process::id()));
    // A prior interrupted write is not authority; replace only this private scratch file.
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)?;
    File::open(path.parent().expect("state parent"))?.sync_all()?;
    Ok(())
}

/// The previous child has exited before this runs. Backups are never restored
/// automatically: restoring a retired identity could undo a user's logout.
pub(super) fn backup_for_binary(dir: &Path, binary: &Path) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    let current = format!("{:x}", Sha256::digest(std::fs::read(binary)?));
    let record = dir.join("binary-state.json");
    let previous = match std::fs::read(&record) {
        Ok(bytes) => Some(serde_json::from_slice::<String>(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    anyhow::ensure!(
        previous.is_some() || !dir.join("node").exists(),
        "Unrecorded Tailnet identity cannot be adopted automatically"
    );
    if previous.as_ref() == Some(&current) {
        return Ok(());
    }
    let node_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir.join("node.lock"))?;
    anyhow::ensure!(
        unsafe { libc::flock(node_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "Node state still has an active writer"
    );
    if let Some(previous) = previous
        && dir.join("node").is_dir()
    {
        anyhow::ensure!(
            previous.len() == 64 && previous.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid Tailnet binary record"
        );
        let backup = dir.join("backups").join(format!(
            "{}-{}-{}",
            previous,
            current,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&backup)?;
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(dir.join("backups"), std::fs::Permissions::from_mode(0o700))?;
        copy_private_tree(&dir.join("node"), &backup.join("node"))?;
        atomic_json(
            &backup.join("versions.json"),
            &serde_json::json!({"fromBinary":previous,"toBinary":current,"tsnetVersion":"1.102.3"}),
        )?;
    }
    atomic_json(&record, &current)
}
fn copy_private_tree(source: &Path, target: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(target)?;
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o700))?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = target.join(entry.file_name());
        if kind.is_dir() {
            copy_private_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&to)?;
            std::io::copy(&mut File::open(entry.path())?, &mut output)?;
            output.sync_all()?;
        } else {
            anyhow::bail!("Unexpected non-file in private Tailnet identity state")
        }
    }
    File::open(target)?.sync_all()?;
    Ok(())
}
