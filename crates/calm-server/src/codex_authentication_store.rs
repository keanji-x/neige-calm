//! Durable diagnostic checkpoint, never a credential store.
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

use provider::codex::AuthenticationFailure;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_CHECKPOINT_BYTES: u64 = 32 * 1024;
const LOG_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogCursor {
    identity: Option<(u64, u64)>,
    offset: u64,
    anchor: String,
    skip_line: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Evidence {
    Clear,
    Confirmed {
        failure: AuthenticationFailure,
    },
    RetryRequested {
        failure: AuthenticationFailure,
        failed_generation: u64,
    },
    Retried {
        failure: AuthenticationFailure,
        failed_generation: u64,
    },
}
impl Evidence {
    pub fn confirmed(&self) -> Option<AuthenticationFailure> {
        match self {
            Self::Confirmed { failure } => Some(*failure),
            _ => None,
        }
    }
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
    pub cursor: LogCursor,
}

impl Checkpoint {
    pub fn empty(home: PathBuf) -> Self {
        Self {
            version: 1,
            home,
            generation: 0,
            evidence: Evidence::Clear,
            revision: 0,
            revision_scope: uuid::Uuid::new_v4(),
            reported: false,
            cursor: LogCursor::default(),
        }
    }
}

pub(super) struct Store {
    path: PathBuf,
    home: PathBuf,
    stderr: PathBuf,
}

impl Store {
    pub fn new(path: PathBuf, home: PathBuf, stderr: PathBuf) -> Self {
        Self { path, home, stderr }
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
        let saved: Checkpoint = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        if saved.version != 1 {
            return Err(std::io::Error::other(
                "unsupported authentication checkpoint version",
            ));
        }
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
    pub fn boundary(&self, source: Option<&std::fs::File>) -> std::io::Result<LogCursor> {
        let mut file = match self.open_log(source)? {
            Some(f) => f,
            None => return Ok(LogCursor::default()),
        };
        let metadata = file.metadata()?;
        let offset = metadata.len();
        let skip_line = if offset == 0 {
            false
        } else {
            file.seek(SeekFrom::Start(offset - 1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            last[0] != b'\n'
        };
        Ok(LogCursor {
            identity: Some((metadata.dev(), metadata.ino())),
            offset,
            anchor: anchor(&mut file, offset)?,
            skip_line,
        })
    }
    /// One bounded read. Cursor points only to complete records, unless discarding an oversized record.
    pub fn scan(
        &self,
        cursor: &mut LogCursor,
        source: Option<&std::fs::File>,
    ) -> std::io::Result<bool> {
        let mut file = match self.open_log(source)? {
            Some(f) => f,
            None => return Ok(false),
        };
        let metadata = file.metadata()?;
        let identity = Some((metadata.dev(), metadata.ino()));
        if cursor.identity != identity
            || metadata.len() < cursor.offset
            || anchor(&mut file, cursor.offset)? != cursor.anchor
        {
            *cursor = LogCursor {
                identity,
                ..LogCursor::default()
            };
        }
        file.seek(SeekFrom::Start(cursor.offset))?;
        let mut bytes = vec![0; LOG_CHUNK_BYTES];
        let count = file.read(&mut bytes)?;
        bytes.truncate(count);
        let mut consumed = 0;
        let mut reported = false;
        for (index, byte) in bytes.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            if !cursor.skip_line
                && let Ok(line) = std::str::from_utf8(&bytes[consumed..index])
            {
                reported |= AuthenticationFailure::from_stderr_line(line).is_some();
            }
            cursor.skip_line = false;
            consumed = index + 1;
        }
        if count == LOG_CHUNK_BYTES && consumed == 0 {
            cursor.skip_line = true;
            consumed = count;
        }
        cursor.offset += consumed as u64;
        cursor.anchor = anchor(&mut file, cursor.offset)?;
        Ok(reported)
    }
    fn open_log(&self, source: Option<&std::fs::File>) -> std::io::Result<Option<std::fs::File>> {
        if let Some(file) = source {
            return file.try_clone().map(Some);
        }
        match std::fs::File::open(&self.stderr) {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn anchor(file: &mut std::fs::File, offset: u64) -> std::io::Result<String> {
    if offset == 0 {
        return Ok(String::new());
    }
    let start = offset.saturating_sub(64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; (offset - start) as usize];
    file.read_exact(&mut bytes)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}
