//! Durable diagnostic checkpoint, never a credential store.
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MAX_CHECKPOINT_BYTES: u64 = 32 * 1024;
/// Version 1 also kept a stderr log cursor and a refresh-failure reason read from Codex's prose
/// (#2512); neither is a fact any more, so a version 1 checkpoint starts clear.
const VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Evidence {
    Clear,
    Confirmed,
    RetryRequested { failed_generation: u64 },
    Retried { failed_generation: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    pub version: u32,
    pub home: PathBuf,
    pub generation: u64,
    pub evidence: Evidence,
    pub revision: u64,
    pub revision_scope: uuid::Uuid,
    pub reported: bool,
}

impl Checkpoint {
    pub fn empty(home: PathBuf) -> Self {
        Self {
            version: VERSION,
            home,
            generation: 0,
            evidence: Evidence::Clear,
            revision: 0,
            revision_scope: uuid::Uuid::new_v4(),
            reported: false,
        }
    }
}

/// Only the version, read before the shape it decides.
#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

pub(super) struct Store {
    path: PathBuf,
    home: PathBuf,
}

impl Store {
    pub fn new(path: PathBuf, home: PathBuf) -> Self {
        Self { path, home }
    }
    pub fn empty(&self) -> Checkpoint {
        Checkpoint::empty(self.home.clone())
    }
    pub fn load(&self) -> std::io::Result<Checkpoint> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(self.empty()),
            Err(e) => return Err(e),
        };
        if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_CHECKPOINT_BYTES {
            return Err(std::io::Error::other(
                "invalid Codex authentication checkpoint size",
            ));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_CHECKPOINT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
            return Err(std::io::Error::other(
                "authentication checkpoint grew beyond its limit",
            ));
        }
        match serde_json::from_slice::<Versioned>(&bytes)
            .map_err(std::io::Error::other)?
            .version
        {
            1 => return Ok(self.empty()),
            VERSION => {}
            _ => {
                return Err(std::io::Error::other(
                    "unsupported authentication checkpoint version",
                ));
            }
        }
        let saved: Checkpoint = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        // A new configured home is a new authority scope; old-home evidence is not its fact.
        Ok(if saved.home == self.home {
            saved
        } else {
            self.empty()
        })
    }
    pub fn save(&self, checkpoint: &Checkpoint) -> std::io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::other("checkpoint has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        temporary.write_all(&serde_json::to_vec(checkpoint).map_err(std::io::Error::other)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.path)
            .map_err(std::io::Error::other)?;
        std::fs::File::open(parent)?.sync_all()
    }
}
