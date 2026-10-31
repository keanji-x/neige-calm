//! Storage encoding only: identities and manifest lengths describe decoded bytes.
use crate::compact_storage;
use crate::error::{CalmError, Result};

use super::{TreeManifest, store};

pub(super) const MAX_DECODED_BYTES: usize = compact_storage::MAX_BLOCK_BYTES;
const MAX_WRITER_COMPRESSION_BYTES: usize = 256 * 1024;

#[derive(sqlx::FromRow)]
pub(super) struct StoredObject {
    pub hash: String,
    pub kind: String,
    pub bytes: Vec<u8>,
    pub encoding: String,
    pub decoded_size: Option<i64>,
}

pub(super) struct EncodedObject {
    pub bytes: Vec<u8>,
    pub encoding: &'static str,
    pub decoded_size: Option<i64>,
}

fn invalid(reason: impl std::fmt::Display) -> CalmError {
    CalmError::Internal(format!("track-vcs object encoding: {reason}"))
}

/// Bound added CPU in existing synchronous writer paths. Larger objects are
/// converted by the operator outside the writer, using the same storage format.
pub(super) fn encode_for_write(bytes: &[u8]) -> Result<EncodedObject> {
    if bytes.len() > MAX_WRITER_COMPRESSION_BYTES {
        return Ok(EncodedObject {
            bytes: bytes.to_vec(),
            encoding: "raw",
            decoded_size: None,
        });
    }
    encode(bytes)
}

pub(super) fn encode(bytes: &[u8]) -> Result<EncodedObject> {
    if (1024..=MAX_DECODED_BYTES).contains(&bytes.len()) {
        let compressed = compact_storage::compress(bytes, 6)?;
        if compressed.len().saturating_add(32) < bytes.len() {
            return Ok(EncodedObject {
                bytes: compressed,
                encoding: "zlib-v1",
                decoded_size: Some(bytes.len() as i64),
            });
        }
    }
    Ok(EncodedObject {
        bytes: bytes.to_vec(),
        encoding: "raw",
        decoded_size: None,
    })
}

impl StoredObject {
    fn decoded(&self) -> Result<Vec<u8>> {
        match self.encoding.as_str() {
            "raw" if self.decoded_size.is_none() => Ok(self.bytes.clone()),
            "zlib-v1" => {
                let size = self
                    .decoded_size
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= MAX_DECODED_BYTES)
                    .ok_or_else(|| invalid("invalid decoded size"))?;
                if self.bytes.len() > MAX_DECODED_BYTES {
                    return Err(invalid("encoded size exceeds limit"));
                }
                compact_storage::decompress(&self.bytes, size)
            }
            _ => Err(invalid("unknown encoding or inconsistent raw metadata")),
        }
    }

    pub(super) fn blob(self) -> Result<Vec<u8>> {
        if self.kind != "blob" {
            return Err(invalid("expected blob"));
        }
        let bytes = self.decoded()?;
        if self.encoding != "raw" && store::hash_bytes("blob", &bytes) != self.hash {
            return Err(invalid("blob identity mismatch"));
        }
        Ok(bytes)
    }

    pub(super) fn manifest(self) -> Result<TreeManifest> {
        if self.kind != "tree" {
            return Err(invalid("expected tree"));
        }
        let manifest: TreeManifest = serde_json::from_slice(&self.decoded()?)?;
        if self.encoding != "raw" && store::hash_tree_manifest(&manifest) != self.hash {
            return Err(invalid("tree identity mismatch"));
        }
        Ok(manifest)
    }

    pub(super) fn verify_encoded(&self, encoded: &EncodedObject) -> Result<()> {
        let candidate = Self {
            hash: self.hash.clone(),
            kind: self.kind.clone(),
            bytes: encoded.bytes.clone(),
            encoding: encoded.encoding.into(),
            decoded_size: encoded.decoded_size,
        };
        if candidate.decoded()? != self.bytes {
            return Err(invalid("roundtrip mismatch"));
        }
        match candidate.kind.as_str() {
            "blob" => {
                candidate.blob()?;
            }
            "tree" => {
                candidate.manifest()?;
            }
            _ => return Err(invalid("unknown object kind")),
        }
        Ok(())
    }
}
