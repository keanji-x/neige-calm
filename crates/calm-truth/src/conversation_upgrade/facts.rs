//! Source coverage only. No semantic, migration or execution qualification.
use super::{CompleteBlockedAudit, RawCell};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonStep {
    Member {
        name: String,
        ordinal: usize,
        occurrence: usize,
    },
    Index(usize),
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct JsonAddress(pub Vec<JsonStep>);
impl JsonAddress {
    /// RFC6901 pointer alone is insufficient for duplicate members; retain steps.
    pub fn pointer(&self) -> String {
        self.0
            .iter()
            .map(|step| match step {
                JsonStep::Member { name, .. } => {
                    format!("/{}", name.replace('~', "~0").replace('/', "~1"))
                }
                JsonStep::Index(i) => format!("/{i}"),
            })
            .collect()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowSource {
    pub relation: String,
    /// B1's physical stable key, never relabeled as a domain id.
    pub raw_key: Vec<RawCell>,
    /// Declared SQL primary key in PK order; empty means none declared.
    pub declared_key: Vec<(String, RawCell)>,
    pub row_sha256: [u8; 32],
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceLocation {
    Encoding,
    Schema {
        ordinal: usize,
    },
    TableList {
        ordinal: usize,
    },
    ColumnMetadata {
        relation: String,
        ordinal: usize,
    },
    Relation {
        name: String,
    },
    Row(RowSource),
    Cell {
        row: RowSource,
        column_index: usize,
        column_name: String,
        cell_sha256: [u8; 32],
        json: Option<JsonAddress>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceRef {
    pub source_sha256: [u8; 32],
    pub source_len: u64,
    pub namespace: &'static str,
    pub location: SourceLocation,
    /// B1 domain-separated digest of the original tuple/row/cell/metadata.
    /// JSON nodes bind the entire original cell, not a normalized JSON value.
    pub evidence_sha256: [u8; 32],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonKind {
    Object,
    Array,
    Null,
    Bool,
    Number,
    String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    Encoding,
    Schema,
    TableList,
    ColumnMetadata,
    Relation,
    Row,
    Cell,
    SqlNull,
    DerivedNotStored,
    Json(JsonKind),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedReason {
    Unclassified,
    MalformedJson,
    InvalidJsonEncoding,
    InvalidJsonStorage,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFact {
    pub source: SourceRef,
    pub kind: SourceKind,
    /// Every P1a fact is Unclassified with a nonempty, typed blocked reason.
    pub blocked: BlockedReason,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateMember {
    pub first: SourceRef,
    pub repeated: SourceRef,
}
/// Private construction: only source_facts can attest complete traversal.
/// Borrowing also keeps every reference tied to the original immutable B1 audit.
#[derive(Debug)]
pub struct CompleteSourceFacts<'a> {
    pub(super) audit: &'a CompleteBlockedAudit,
    pub(super) nodes: Vec<SourceFact>,
    pub(super) duplicates: Vec<DuplicateMember>,
}
impl CompleteSourceFacts<'_> {
    pub fn audit(&self) -> &CompleteBlockedAudit {
        self.audit
    }
    pub fn nodes(&self) -> &[SourceFact] {
        &self.nodes
    }
    pub fn duplicates(&self) -> &[DuplicateMember] {
        &self.duplicates
    }
}
#[derive(Debug, thiserror::Error)]
pub enum FactCoverageError {
    #[error("inconsistent raw source coverage")]
    InconsistentSource,
    #[error("JSON traversal cannot cover source: {source_ref:?}")]
    JsonTraversal { source_ref: Box<SourceRef> },
    #[error("evidence encoding: {0}")]
    Evidence(#[from] serde_json::Error),
}
