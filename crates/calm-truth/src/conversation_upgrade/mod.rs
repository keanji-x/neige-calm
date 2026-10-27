//! Complete raw offline BlockedAudit evidence and versioned manifests.
//! No classification, qualification, or permission to start/apply a deployment.
mod manifest;
pub use manifest::{
    AuditManifest, FileEvidence, ManifestError, SourceDeclaration, evidence_digest,
};
mod rows;
mod schema;
mod snapshot;
pub use snapshot::OfflineSnapshot;
use std::path::PathBuf;

/// Caller attestation, not evidence manufactured by immutable or a file hash.
/// The operator must stop writers, make a consistent self-contained backup
/// (including committed WAL), and freeze its contents AND complete pathname
/// namespace for the entire read. This API does not defend concurrent replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenBackup {
    pub path: PathBuf,
    pub operator: String,
    pub freeze_evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub enum RawCell {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
    /// VIRTUAL generated column: no independently stored cell; never read it.
    DerivedNotStored,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RawRow {
    pub key: Vec<RawCell>,
    pub cells: Vec<RawCell>,
}

/// Names and original DDL remain in the encoding-tagged raw metadata below.
/// Strings here are losslessly decoded identifiers used for quoted SQL only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RawRelation {
    pub name: String,
    pub columns: Vec<String>,
    /// Complete table_xinfo facts, in SQLite's cid order.
    pub column_facts: Vec<Vec<RawCell>>,
    pub key_columns: Vec<String>,
    pub rows: Vec<RawRow>,
    pub source_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RawInventory {
    pub encoding: String,
    /// type, name, tbl_name, rootpage, sql; all sqlite_schema objects.
    pub schema: Vec<Vec<RawCell>>,
    /// schema, name, type, ncol, wr, strict; main namespace, including internals.
    pub table_list: Vec<Vec<RawCell>>,
    pub relations: Vec<RawRelation>,
}

/// Every semantic field remains Unclassified. Source references are
/// (relation.name, row.key, column index/name); originals include identity and
/// migrations if present. Missing relations are observable schema facts, never
/// default identities or evidence of migration success.
#[derive(Debug)]
pub struct CompleteBlockedAudit {
    inventory: RawInventory,
    input: FrozenBackup,
    source_sha256: [u8; 32],
    source_len: u64,
}
impl CompleteBlockedAudit {
    pub fn inventory(&self) -> &RawInventory {
        &self.inventory
    }
    pub fn input(&self) -> &FrozenBackup {
        &self.input
    }
    pub fn source_sha256(&self) -> &[u8; 32] {
        &self.source_sha256
    }
    pub fn source_len(&self) -> u64 {
        self.source_len
    }
    pub fn semantic_status(&self) -> &'static str {
        "BlockedAudit/Unclassified"
    }
}

/// B1a deliberately assigns no semantic eligibility. Each diagnostic refers
/// directly to a preserved source cell, including malformed identity/migrations.
#[derive(Debug, PartialEq, Eq)]
pub struct UnclassifiedSource<'a> {
    pub relation: &'a str,
    pub key: &'a [RawCell],
    pub field: &'a str,
}
impl CompleteBlockedAudit {
    pub fn unclassified_fields(&self) -> impl Iterator<Item = UnclassifiedSource<'_>> {
        self.inventory.relations.iter().flat_map(|r| {
            r.rows.iter().flat_map(move |row| {
                r.columns.iter().map(move |field| UnclassifiedSource {
                    relation: &r.name,
                    key: &row.key,
                    field,
                })
            })
        })
    }
}

/// Coverage failures never produce CompleteBlockedAudit. Partial is deliberately
/// a different type: B1b must propagate it instead of sealing it as Blocked.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("unsupported backup input: {0}")]
    UnsupportedInput(&'static str),
    #[error("partial raw audit at {scope}: {reason}")]
    Partial {
        scope: String,
        reason: PartialReason,
    },
}
#[derive(Debug, thiserror::Error)]
pub enum PartialReason {
    #[error("unsupported coverage: {0}")]
    UnsupportedCoverage(String),
    #[error("SQLite: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("source changed despite freeze attestation")]
    SourceChanged,
}
pub(super) fn partial(scope: impl Into<String>, reason: impl Into<PartialReason>) -> SnapshotError {
    SnapshotError::Partial {
        scope: scope.into(),
        reason: reason.into(),
    }
}
pub(super) fn unsupported(scope: &str, reason: &str) -> SnapshotError {
    partial(scope, PartialReason::UnsupportedCoverage(reason.into()))
}

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod raw_tests;

#[cfg(test)]
mod manifest_tests;
