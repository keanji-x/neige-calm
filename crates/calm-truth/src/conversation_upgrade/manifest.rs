//! Versioned raw evidence protocol. No semantic decoder participates in this root.
use super::{CompleteBlockedAudit, RawInventory};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid or duplicate evidence reference")]
    InvalidEvidence,
    #[error("manifest encoding: {0}")]
    Encoding(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceDeclaration {
    pub path: Vec<u8>,
    pub ownership: String,
    pub freeze_evidence: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct FileEvidence {
    pub source: usize,
    pub path: Vec<u8>,
    pub kind: String,
    pub mode: u32,
    pub length: u64,
    pub sha256: Option<[u8; 32]>,
    pub artifact: Option<String>,
    pub diagnostics: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct RelationDigest {
    pub name: String,
    pub count: i64,
    pub ordered_rows: Vec<[u8; 32]>,
}
#[derive(Debug, Serialize)]
pub struct AuditManifest<'a> {
    pub protocol: &'static str,
    pub status: &'static str,
    pub database_path: Vec<u8>,
    pub operator: &'a str,
    pub freeze_evidence: &'a str,
    pub source_sha256: [u8; 32],
    pub source_len: u64,
    pub complete_set_evidence: &'a str,
    pub sources: Vec<SourceDeclaration>,
    pub files: Vec<FileEvidence>,
    pub inventory: RawInventory,
    pub relations: Vec<RelationDigest>,
    /// Every raw cell remains blocked, referenced by relation/key/column.
    pub blocked_reason: &'static str,
    pub coverage: &'static str,
}

/// Each node is tagged; lengths/counts are u64 big endian. Object fields sort
/// by UTF-8 bytes. RawCell enum tags preserve storage classes, NULL and empty.
/// Raw TEXT/BLOB are byte arrays, never parsed or normalized as JSON text.
fn encode(value: &serde_json::Value, out: &mut Vec<u8>) {
    use serde_json::Value;
    fn bytes(tag: u8, bytes: &[u8], out: &mut Vec<u8>) {
        out.push(tag);
        out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    match value {
        Value::Null => out.push(0),
        Value::Bool(v) => out.extend_from_slice(&[1, u8::from(*v)]),
        Value::Number(v) => bytes(2, v.to_string().as_bytes(), out),
        Value::String(v) => bytes(3, v.as_bytes(), out),
        Value::Array(v) => {
            out.push(4);
            out.extend_from_slice(&(v.len() as u64).to_be_bytes());
            for item in v {
                encode(item, out);
            }
        }
        Value::Object(v) => {
            out.push(5);
            out.extend_from_slice(&(v.len() as u64).to_be_bytes());
            let mut fields = v.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(key, _)| key.as_bytes());
            for (key, item) in fields {
                bytes(3, key.as_bytes(), out);
                encode(item, out);
            }
        }
    }
}
pub fn evidence_digest(
    domain: &str,
    value: &impl Serialize,
) -> Result<[u8; 32], serde_json::Error> {
    let mut bytes = b"neige.raw-blocked-audit.v1\0".to_vec();
    bytes.extend_from_slice(&(domain.len() as u64).to_be_bytes());
    bytes.extend_from_slice(domain.as_bytes());
    encode(&serde_json::to_value(value)?, &mut bytes);
    Ok(Sha256::digest(bytes).into())
}
impl CompleteBlockedAudit {
    pub fn manifest<'a>(
        &'a self,
        database_path: Vec<u8>,
        complete_set_evidence: &'a str,
        sources: Vec<SourceDeclaration>,
        mut files: Vec<FileEvidence>,
    ) -> Result<AuditManifest<'a>, ManifestError> {
        let mut paths = std::collections::HashSet::new();
        let mut artifacts = std::collections::HashSet::new();
        if sources.iter().any(|s| !paths.insert(&s.path)) {
            return Err(ManifestError::InvalidEvidence);
        }
        let mut entries = std::collections::HashSet::new();
        if files.iter().any(|f| {
            f.source >= sources.len()
                || !entries.insert((f.source, &f.path))
                || f.artifact.as_ref().is_some_and(|a| !artifacts.insert(a))
        }) {
            return Err(ManifestError::InvalidEvidence);
        }
        // Source indices refer to caller-sorted declarations; preserve that mapping.
        files.sort_by(|a, b| (a.source, &a.path).cmp(&(b.source, &b.path)));
        let mut inventory = self.inventory().clone();
        inventory.schema.sort_by_cached_key(|row| {
            let mut bytes = Vec::new();
            encode(
                &serde_json::to_value(row).expect("raw cells serialize"),
                &mut bytes,
            );
            bytes
        });
        inventory
            .relations
            .sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        let mut relations = self
            .inventory()
            .relations
            .iter()
            .map(|r| {
                Ok(RelationDigest {
                    name: r.name.clone(),
                    count: r.source_count,
                    ordered_rows: r
                        .rows
                        .iter()
                        .map(|row| evidence_digest("row", row))
                        .collect::<Result<_, _>>()?,
                })
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        relations.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        Ok(AuditManifest {
            protocol: "neige.raw-blocked-audit.v1",
            status: self.semantic_status(),
            database_path,
            operator: &self.input().operator,
            freeze_evidence: &self.input().freeze_evidence,
            source_sha256: *self.source_sha256(),
            source_len: self.source_len(),
            complete_set_evidence,
            sources,
            files,
            inventory,
            relations,
            blocked_reason: "Unclassified: every schema object, relation/key/column and owned path; no qualification or provider proof",
            coverage: "Complete stored database inventory and all operator-declared owned trees; symlink/other targets not read",
        })
    }
}
impl AuditManifest<'_> {
    pub fn root(&self) -> Result<[u8; 32], serde_json::Error> {
        evidence_digest("root", self)
    }
}
