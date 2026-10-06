//! Mobile grant ownership: persistent, origin-bound credentials and their device registry.
//! Password sessions and pending invitations never enter this store.
use crate::auth::{Session, SessionAuthority, SessionBackend};
use crate::error::{CalmError, Result};
use calm_types::mobile_access::PairedDevice;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(super) const MAX_DEVICES: usize = 16;
const MAX_FILE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum DeviceGrant {
    Pairing(String),
    Scan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Device {
    id: String,
    device_name: String,
    grant: DeviceGrant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    origin: Option<String>,
    // Keys are SHA-256 digests. The bearer token is never written to disk.
    devices: HashMap<String, Device>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: 1,
            origin: None,
            devices: HashMap::new(),
        }
    }
}
#[derive(Debug, Default)]
struct State {
    snapshot: Snapshot,
    path: Option<PathBuf>,
    active: bool,
    failed: bool,
}
#[derive(Debug, Default)]
pub(super) struct GrantStore(Mutex<State>);

fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
fn storage_error() -> CalmError {
    CalmError::Internal(
        "Mobile credential storage unavailable; check private state permissions".into(),
    )
}
fn private_directory(parent: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(parent)?;
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { nix::libc::geteuid() }
            && metadata.permissions().mode() & 0o777 == 0o700,
        "Mobile credential directory must be private and owned by this user"
    );
    Ok(())
}
fn validate_file(metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { nix::libc::geteuid() }
            && metadata.permissions().mode() & 0o777 == 0o600
            && metadata.len() <= MAX_FILE_BYTES,
        "Mobile credential file must be a bounded private regular file"
    );
    Ok(())
}
fn persist(path: &Path, snapshot: &Snapshot) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing mobile credential directory"))?;
    private_directory(parent)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    temporary.write_all(&serde_json::to_vec(snapshot)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

impl GrantStore {
    /// Called before ingress startup; failures leave authentication suspended.
    pub fn load(&self, path: PathBuf) -> anyhow::Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Mobile credential lock poisoned"))?;
        anyhow::ensure!(
            state.path.is_none() && state.snapshot.devices.is_empty(),
            "Mobile credentials already configured"
        );
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Missing mobile credential directory"))?;
        private_directory(parent)?;
        let snapshot = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => {
                validate_file(&file.metadata()?)?;
                let mut bytes = Vec::new();
                file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
                anyhow::ensure!(
                    bytes.len() <= MAX_FILE_BYTES as usize,
                    "Mobile credential file exceeds limit"
                );
                let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
                anyhow::ensure!(
                    snapshot.version == 1 && snapshot.devices.len() <= MAX_DEVICES,
                    "Invalid mobile credential snapshot"
                );
                anyhow::ensure!(
                    snapshot.devices.is_empty() || snapshot.origin.is_some(),
                    "Mobile grants require an origin"
                );
                if let Some(origin) = &snapshot.origin {
                    anyhow::ensure!(
                        crate::auth::normalize_origin(origin).as_ref() == Some(origin),
                        "Invalid mobile credential origin"
                    );
                }
                let mut ids = std::collections::HashSet::new();
                for (digest, device) in &snapshot.devices {
                    anyhow::ensure!(
                        digest.len() == 64
                            && digest
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            && uuid::Uuid::parse_str(&device.id).is_ok()
                            && ids.insert(&device.id)
                            && !device.device_name.trim().is_empty()
                            && device.device_name.len() <= 80
                            && !device.device_name.chars().any(char::is_control),
                        "Invalid mobile device grant"
                    );
                }
                snapshot
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Snapshot::default(),
            Err(error) => return Err(error.into()),
        };
        state.snapshot = snapshot;
        state.path = Some(path);
        state.active = false;
        Ok(())
    }

    fn commit(state: &mut State, snapshot: Snapshot) -> Result<()> {
        if let Some(path) = &state.path
            && let Err(error) = persist(path, &snapshot)
        {
            state.failed = true;
            state.active = false;
            tracing::error!(%error, "mobile credential persistence failed");
            return Err(storage_error());
        }
        state.snapshot = snapshot;
        state.failed = false;
        Ok(())
    }

    /// A verified origin change revokes old grants. Same-origin restart resumes them.
    pub fn activate(&self, origin: &str) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| storage_error())?;
        if state.snapshot.origin.as_deref() != Some(origin) || state.failed {
            let mut snapshot = state.snapshot.clone();
            if snapshot.origin.as_deref() != Some(origin) {
                snapshot.devices.clear();
            }
            snapshot.origin = Some(origin.into());
            Self::commit(&mut state, snapshot)?;
        }
        state.active = true;
        Ok(())
    }

    /// Process shutdown is not an owner revocation.
    pub fn suspend(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.active = false;
        }
    }
    pub fn disable(&self) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| storage_error())?;
        state.active = false;
        if state.snapshot.origin.is_none() && state.snapshot.devices.is_empty() && !state.failed {
            return Ok(());
        }
        Self::commit(&mut state, Snapshot::default())
    }
    /// Startup/storage unavailability must not tell clients that a saved credential was revoked.
    pub fn ensure_ready(&self) -> Result<()> {
        let state = self.0.lock().map_err(|_| storage_error())?;
        if !state.active && !state.snapshot.devices.is_empty() {
            return Err(CalmError::ServiceUnavailable(
                "Mobile access is reconnecting; retry shortly".into(),
            ));
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<PairedDevice> {
        self.0
            .lock()
            .map(|state| {
                state
                    .snapshot
                    .devices
                    .values()
                    .map(|d| PairedDevice {
                        id: d.id.clone(),
                        device_name: d.device_name.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn approved(&self, id: &str) -> bool {
        self.0.lock().is_ok_and(|state| {
            state.snapshot.devices.values().any(
                |device| matches!(&device.grant, DeviceGrant::Pairing(pairing) if pairing == id),
            )
        })
    }
    pub fn mint(&self, origin: &str, name: String, grant: DeviceGrant) -> Result<String> {
        self.activate(origin)?;
        let mut state = self.0.lock().map_err(|_| storage_error())?;
        let mut snapshot = state.snapshot.clone();
        if snapshot.devices.len() >= MAX_DEVICES {
            return Err(CalmError::BadRequest("Device limit reached".into()));
        }
        let token = super::pairing::secret();
        snapshot.devices.insert(
            hash(&token),
            Device {
                id: uuid::Uuid::new_v4().to_string(),
                device_name: name,
                grant,
            },
        );
        Self::commit(&mut state, snapshot)?;
        Ok(token)
    }
    /// Invitation revocation uses this registry-owned credential/device relationship.
    pub fn owns_session(&self, id: &str, token: &str) -> bool {
        self.0.lock().is_ok_and(|state| {
            state
                .snapshot
                .devices
                .get(&hash(token))
                .is_some_and(|device| device.id == id)
        })
    }

    pub fn revoke(&self, id: &str) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| storage_error())?;
        let mut snapshot = state.snapshot.clone();
        let count = snapshot.devices.len();
        snapshot.devices.retain(|_, device| device.id != id);
        if snapshot.devices.len() == count {
            return Err(CalmError::NotFound("No paired device with this id".into()));
        }
        Self::commit(&mut state, snapshot)
    }
}

impl SessionBackend for GrantStore {
    fn get(&self, token: &str) -> Option<Session> {
        if token.len() != 64 {
            return None;
        }
        let state = self.0.lock().ok()?;
        if !state.active || state.failed {
            return None;
        }
        state.snapshot.devices.get(&hash(token))?;
        Some(Session {
            session_id: token.into(),
            authority: SessionAuthority::PairedDevice,
        })
    }
    fn remove(&self, token: &str) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| storage_error())?;
        let mut snapshot = state.snapshot.clone();
        if snapshot.devices.remove(&hash(token)).is_none() {
            return Ok(());
        }
        Self::commit(&mut state, snapshot)
    }
}

#[cfg(test)]
mod tests;
