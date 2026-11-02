//! Owner-local schema facts. Metadata agreement is never proof of actual DDL.
use super::*;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaReason {
    Missing,
    Null,
    StorageType,
    InvalidEncoding,
    InvalidValue,
    UnknownVersion,
    FutureVersion,
    DuplicateVersion,
    DuplicateIdentity,
    ReleasedMismatch,
    FailedMigration,
    UninterpretedDdl,
    TimelineContradiction,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Present(RawCell),
    /// Source is an existing row/parent, never a digest of an invented cell.
    Missing,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaFieldFact {
    pub source: SourceRef,
    pub expected_field: &'static str,
    pub value: FieldValue,
    pub reasons: Vec<SchemaReason>,
}
#[derive(Debug)]
pub struct MigrationFact {
    pub source: SourceRef,
    pub fields: Vec<SchemaFieldFact>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum MigrationTimeline {
    /// Only the recorded metadata matches a released prefix; no schema proof.
    KnownPrefix {
        through: i64,
    },
    Unknown,
}
#[derive(Debug, PartialEq, Eq)]
pub enum IdentityStatus {
    Unidentified,
    StoredIdentity(String),
}
#[derive(Debug)]
pub struct IdentityFact {
    pub source: TableEvidence,
    pub rows: Vec<Vec<SchemaFieldFact>>,
    pub reasons: Vec<SchemaReason>,
    pub status: IdentityStatus,
    pub missing_fields: Vec<MissingSchemaField>,
}
#[derive(Debug, Clone)]
pub enum TableEvidence {
    Present(SourceRef),
    /// Complete P1a schema container, including the empty-schema case. These
    /// are existing tuple/encoding references, not a fabricated absent-table row.
    Absent {
        expected_table: &'static str,
        encoding: SourceRef,
        schema: Vec<SourceRef>,
    },
}
#[derive(Debug)]
pub struct MissingSchemaField {
    pub parent: TableEvidence,
    pub expected_field: &'static str,
    pub reason: SchemaReason,
}
#[derive(Debug)]
pub struct MissingMigration {
    pub parent: TableEvidence,
    pub expected_version: i64,
    pub reason: SchemaReason,
}
#[derive(Debug, PartialEq, Eq)]
pub enum SchemaDisposition {
    LegacyRecoveryUnqualified,
    /// Actual presence observed, DDL semantics remain blocked.
    UnknownDdl,
    /// Absence observed against a matching metadata prefix only. No claim
    /// about prior data, a completed upgrade, or the rest of the schema.
    NotPresentAtRecordedPrefix,
    UnknownTimeline,
    Blocked,
}
#[derive(Debug)]
pub struct HistoricalTableFact {
    pub name: &'static str,
    pub source: TableEvidence,
    pub disposition: SchemaDisposition,
    pub reasons: Vec<SchemaReason>,
    /// Missing required columns contradict the owner contract. Present columns
    /// alone never prove constraints, triggers, or complete DDL equivalence.
    pub missing_columns: Vec<SchemaFieldFact>,
    pub timeline_sources: Vec<SourceRef>,
}
#[derive(Debug)]
pub struct SchemaMetadataFact {
    pub source: SourceRef,
    pub field_index: usize,
    pub field_name: &'static str,
    pub reason: SchemaReason,
}
#[derive(Debug)]
pub struct SchemaFacts<'a> {
    /// Original DDL, xinfo, all unknown tables/columns and ledger remain intact.
    pub inventory: &'a RawInventory,
    pub ddl: Vec<SchemaMetadataFact>,
    pub migrations_source: TableEvidence,
    pub migrations: Vec<MigrationFact>,
    pub missing_migration_fields: Vec<MissingSchemaField>,
    pub missing_migrations: Vec<MissingMigration>,
    pub timeline: MigrationTimeline,
    pub identity: IdentityFact,
    pub historical_tables: Vec<HistoricalTableFact>,
}

// One index over the already complete ledger: no SQL, source re-traversal or hash.
pub(super) struct References<'a> {
    encoding: &'a SourceRef,
    schema: Vec<&'a SourceRef>,
    relations: HashMap<&'a str, &'a SourceRef>,
    rows: HashMap<(&'a str, &'a [RawCell]), &'a SourceRef>,
    cells: HashMap<(&'a str, &'a [RawCell], &'a str), &'a SourceRef>,
}
impl<'a> References<'a> {
    pub(super) fn new(s: &'a CompleteSourceFacts<'_>) -> Result<Self, FactCoverageError> {
        let mut encoding = None;
        let mut out = Self {
            encoding: &s
                .nodes()
                .first()
                .ok_or(FactCoverageError::InconsistentSource)?
                .source,
            schema: Vec::new(),
            relations: HashMap::new(),
            rows: HashMap::new(),
            cells: HashMap::new(),
        };
        for n in s.nodes() {
            let source = &n.source;
            match &source.location {
                SourceLocation::Encoding => encoding = Some(source),
                SourceLocation::Schema { .. } => out.schema.push(source),
                SourceLocation::Relation { name } => {
                    out.relations.insert(name, source);
                }
                SourceLocation::Row(r) => {
                    out.rows.insert((&r.relation, &r.raw_key), source);
                }
                SourceLocation::Cell {
                    row,
                    column_name,
                    json: None,
                    ..
                } => {
                    out.cells
                        .insert((&row.relation, &row.raw_key, column_name), source);
                }
                _ => {}
            }
        }
        out.encoding = encoding.ok_or(FactCoverageError::InconsistentSource)?;
        Ok(out)
    }
    pub(super) fn table(&self, name: &'static str) -> TableEvidence {
        match self.relations.get(name) {
            Some(source) => TableEvidence::Present((*source).clone()),
            None => TableEvidence::Absent {
                expected_table: name,
                encoding: self.encoding.clone(),
                schema: self.schema.iter().map(|s| (*s).clone()).collect(),
            },
        }
    }
    pub(super) fn row(
        &self,
        r: &RawRelation,
        row: &RawRow,
    ) -> Result<SourceRef, FactCoverageError> {
        self.rows
            .get(&(r.name.as_str(), row.key.as_slice()))
            .map(|s| (*s).clone())
            .ok_or(FactCoverageError::InconsistentSource)
    }
    fn field(
        &self,
        r: &RawRelation,
        row: &RawRow,
        name: &'static str,
    ) -> Result<SchemaFieldFact, FactCoverageError> {
        let (source, value, reasons) = match r.columns.iter().position(|n| n == name) {
            Some(i) => (
                self.cells
                    .get(&(r.name.as_str(), row.key.as_slice(), name))
                    .map(|source| (*source).clone())
                    .ok_or(FactCoverageError::InconsistentSource)?,
                FieldValue::Present(row.cells[i].clone()),
                Vec::new(),
            ),
            None => (
                self.row(r, row)?,
                FieldValue::Missing,
                vec![SchemaReason::Missing],
            ),
        };
        Ok(SchemaFieldFact {
            source,
            expected_field: name,
            value,
            reasons,
        })
    }
}
fn integer(f: &SchemaFieldFact) -> Option<i64> {
    match f.value {
        FieldValue::Present(RawCell::Integer(v)) => Some(v),
        _ => None,
    }
}
fn validate(f: &mut SchemaFieldFact, encoding: &str, storage: &str) {
    let FieldValue::Present(cell) = &f.value else {
        return;
    };
    let reason = match cell {
        RawCell::Null => Some(SchemaReason::Null),
        RawCell::Integer(_) if storage == "integer" => None,
        RawCell::Blob(_) if storage == "blob" => None,
        RawCell::Text(_) if storage == "text" => {
            if schema::text(cell, encoding).is_err() {
                Some(SchemaReason::InvalidEncoding)
            } else {
                None
            }
        }
        _ => Some(SchemaReason::StorageType),
    };
    if let Some(reason) = reason {
        f.reasons.push(reason);
    }
}
fn text(f: &SchemaFieldFact, encoding: &str) -> Option<String> {
    match &f.value {
        FieldValue::Present(c) => schema::text(c, encoding).ok(),
        _ => None,
    }
}
const MIGRATION_FIELDS: [(&str, &str); 6] = [
    ("version", "integer"),
    ("description", "text"),
    ("installed_on", "text"),
    ("success", "integer"),
    ("checksum", "blob"),
    ("execution_time", "integer"),
];

/// Interpret only schema/identity ownership, consuming the sole P1a ledger.
/// No frame classification, deployment eligibility, identity mint or DB writes.
pub fn schema_facts<'a>(s: &CompleteSourceFacts<'a>) -> Result<SchemaFacts<'a>, FactCoverageError> {
    let refs = References::new(s)?;
    let inv = s.audit().inventory();
    let relation = |name| inv.relations.iter().find(|r| r.name == name);
    let mut migrations = Vec::new();
    if let Some(r) = relation("_sqlx_migrations") {
        for row in &r.rows {
            let mut fields = MIGRATION_FIELDS
                .iter()
                .map(|(name, storage)| {
                    let mut f = refs.field(r, row, name)?;
                    validate(&mut f, &inv.encoding, storage);
                    Ok(f)
                })
                .collect::<Result<Vec<_>, FactCoverageError>>()?;
            let version = integer(&fields[0]);
            let owner = crate::MIGRATOR.iter().find(|m| Some(m.version) == version);
            if fields[0].reasons.is_empty() && owner.is_none() {
                let future = crate::MIGRATOR
                    .iter()
                    .next_back()
                    .is_some_and(|m| version > Some(m.version));
                fields[0].reasons.push(if future {
                    SchemaReason::FutureVersion
                } else {
                    SchemaReason::UnknownVersion
                });
            }
            for (i, f) in fields.iter_mut().enumerate().skip(1) {
                if !f.reasons.is_empty() {
                    continue;
                }
                let reason = match i {
                    1 => match owner {
                        Some(m)
                            if text(f, &inv.encoding).as_deref()
                                == Some(m.description.as_ref()) =>
                        {
                            None
                        }
                        Some(_) => Some(SchemaReason::ReleasedMismatch),
                        None => Some(SchemaReason::UnknownVersion),
                    },
                    2 if text(f, &inv.encoding).is_some_and(|t| !valid_installed_on(&t)) => {
                        Some(SchemaReason::InvalidValue)
                    }
                    3 if integer(f) == Some(0) => Some(SchemaReason::FailedMigration),
                    3 if integer(f) != Some(1) => Some(SchemaReason::InvalidValue),
                    4 => match owner {
                        Some(m)
                            if f.value
                                == FieldValue::Present(RawCell::Blob(m.checksum.to_vec())) =>
                        {
                            None
                        }
                        Some(_) => Some(SchemaReason::ReleasedMismatch),
                        None => Some(SchemaReason::UnknownVersion),
                    },
                    // SQLx 0.8.6, sqlx-sqlite src/migrate.rs, Migrate::apply:
                    // migration SQL, success=TRUE and execution_time=-1 commit
                    // together; duration is updated outside that transaction.
                    // Process exit can leave -1 (unrecorded duration) persisted.
                    // Accept that sentinel while retaining the original cell.
                    5 if integer(f).is_some_and(|v| v < -1) => Some(SchemaReason::InvalidValue),
                    _ => None,
                };
                if let Some(reason) = reason {
                    f.reasons.push(reason);
                }
            }
            migrations.push(MigrationFact {
                source: refs.row(r, row)?,
                fields,
            });
        }
    }
    let mut counts = HashMap::new();
    for m in &migrations {
        if let Some(v) = integer(&m.fields[0]) {
            *counts.entry(v).or_insert(0) += 1;
        }
    }
    for m in &mut migrations {
        if integer(&m.fields[0]).is_some_and(|v| counts[&v] > 1) {
            m.fields[0].reasons.push(SchemaReason::DuplicateVersion);
        }
    }
    let mut ordered: Vec<_> = migrations.iter().collect();
    ordered.sort_by_key(|m| integer(&m.fields[0]));
    let prefix = !ordered.is_empty()
        && ordered
            .iter()
            .all(|m| m.fields.iter().all(|f| f.reasons.is_empty()))
        && ordered.len() <= crate::MIGRATOR.iter().count()
        && ordered
            .iter()
            .zip(crate::MIGRATOR.iter())
            .all(|(f, m)| integer(&f.fields[0]) == Some(m.version));
    let timeline = if prefix {
        MigrationTimeline::KnownPrefix {
            through: integer(&ordered.last().unwrap().fields[0]).unwrap(),
        }
    } else {
        MigrationTimeline::Unknown
    };
    // Gaps only before another *recognized recorded* migration. This diagnoses
    // metadata omissions; it never derives actual schema from a MAX(version).
    let released: Vec<_> = crate::MIGRATOR.iter().collect();
    let last_recorded = released
        .iter()
        .rposition(|m| counts.contains_key(&m.version));
    let missing_migrations = released
        .iter()
        .take(last_recorded.map_or(0, |i| i + 1))
        .filter(|m| !counts.contains_key(&m.version))
        .map(|m| MissingMigration {
            parent: refs.table("_sqlx_migrations"),
            expected_version: m.version,
            reason: SchemaReason::Missing,
        })
        .collect();
    let missing_migration_fields = MIGRATION_FIELDS
        .iter()
        .filter(|(name, _)| {
            relation("_sqlx_migrations").is_none_or(|r| !r.columns.iter().any(|c| c == name))
        })
        .map(|(name, _)| MissingSchemaField {
            parent: refs.table("_sqlx_migrations"),
            expected_field: name,
            reason: SchemaReason::Missing,
        })
        .collect();
    let applied = |v| prefix && counts.contains_key(&v);
    let mut historical_tables = Vec::new();
    // Fixed owner history, never an exhaustive inventory or growth checklist.
    for (name, introduced, removed, columns) in [
        ("planner_recovery_threads", 101, Some(127), &[][..]),
        ("planner_recovery_issuances", 101, Some(127), &[][..]),
        ("planner_recovery_turns", 101, Some(127), &[][..]),
        ("planner_recovery_calls", 101, Some(127), &[][..]),
        (
            "database_identity",
            110,
            None,
            &["singleton", "id", "minted_at_ms"][..],
        ),
        (
            "worker_session_backends",
            199,
            None,
            &["worker_session_id", "provider", "backend_id"][..],
        ),
        (
            "claude_session_launches",
            203,
            None,
            &[
                "worker_session_id",
                "card_id",
                "track_id",
                "thread_id",
                "execution_id",
                "created_at_ms",
            ][..],
        ),
    ] {
        let source = refs.table(name);
        let present = relation(name);
        let expected = applied(introduced) && !removed.is_some_and(applied);
        let mut missing_columns = Vec::new();
        if let Some(r) = present {
            for field in columns {
                if !r.columns.iter().any(|c| c == field) {
                    missing_columns.push(SchemaFieldFact {
                        source: refs.relations[name].clone(),
                        expected_field: field,
                        value: FieldValue::Missing,
                        reasons: vec![SchemaReason::Missing],
                    });
                }
            }
        }
        let mut reasons = Vec::new();
        if prefix && present.is_some() != expected {
            reasons.push(SchemaReason::TimelineContradiction);
        }
        if present.is_some() {
            reasons.push(SchemaReason::UninterpretedDdl);
        }
        let disposition = if present.is_some() && removed.is_some() {
            SchemaDisposition::LegacyRecoveryUnqualified
        } else if !missing_columns.is_empty()
            || reasons.contains(&SchemaReason::TimelineContradiction)
        {
            SchemaDisposition::Blocked
        } else if !prefix {
            SchemaDisposition::UnknownTimeline
        } else if present.is_some() {
            SchemaDisposition::UnknownDdl
        } else {
            SchemaDisposition::NotPresentAtRecordedPrefix
        };
        historical_tables.push(HistoricalTableFact {
            name,
            source,
            disposition,
            reasons,
            missing_columns,
            timeline_sources: if prefix {
                ordered.iter().map(|m| m.fields[0].source.clone()).collect()
            } else {
                Vec::new()
            },
        });
    }
    let mut identity = IdentityFact {
        source: refs.table("database_identity"),
        rows: Vec::new(),
        reasons: Vec::new(),
        status: IdentityStatus::Unidentified,
        missing_fields: Vec::new(),
    };
    match relation("database_identity") {
        None => identity.reasons.push(SchemaReason::Missing),
        Some(r) => {
            if r.rows.is_empty() {
                identity.reasons.push(SchemaReason::Missing);
            }
            if r.rows.len() > 1 {
                identity.reasons.push(SchemaReason::DuplicateIdentity);
            }
            for row in &r.rows {
                let mut fields = Vec::new();
                for (name, storage) in [
                    ("singleton", "integer"),
                    ("id", "text"),
                    ("minted_at_ms", "integer"),
                ] {
                    let mut f = refs.field(r, row, name)?;
                    validate(&mut f, &inv.encoding, storage);
                    if f.reasons.is_empty() {
                        let valid = match name {
                            "singleton" => integer(&f) == Some(1),
                            "id" => text(&f, &inv.encoding)
                                .is_some_and(|t| uuid::Uuid::parse_str(&t).is_ok()),
                            _ => integer(&f).is_some_and(|v| v >= 0),
                        };
                        if !valid {
                            f.reasons.push(SchemaReason::InvalidValue);
                        }
                    }
                    if r.rows.len() > 1 {
                        f.reasons.push(SchemaReason::DuplicateIdentity);
                    }
                    fields.push(f);
                }
                identity.rows.push(fields);
            }
            if identity.reasons.is_empty() && identity.rows[0].iter().all(|f| f.reasons.is_empty())
            {
                identity.status = IdentityStatus::StoredIdentity(
                    text(&identity.rows[0][1], &inv.encoding)
                        .ok_or(FactCoverageError::InconsistentSource)?,
                );
            }
        }
    }
    if relation("database_identity").is_none_or(|r| r.rows.is_empty()) {
        identity.missing_fields = ["singleton", "id", "minted_at_ms"]
            .into_iter()
            .map(|name| MissingSchemaField {
                parent: refs.table("database_identity"),
                expected_field: name,
                reason: SchemaReason::Missing,
            })
            .collect();
    }
    Ok(SchemaFacts {
        inventory: inv,
        ddl: refs
            .schema
            .iter()
            .map(|source| SchemaMetadataFact {
                source: (*source).clone(),
                field_index: 4,
                field_name: "sql",
                reason: SchemaReason::UninterpretedDdl,
            })
            .collect(),
        migrations_source: refs.table("_sqlx_migrations"),
        migrations,
        missing_migration_fields,
        missing_migrations,
        timeline,
        identity,
        historical_tables,
    })
}

// SQLx stores UTC timestamps; validate calendar/time through its public chrono
// type rather than equating arbitrary nonempty TEXT with a valid installed_on.
fn valid_installed_on(t: &str) -> bool {
    sqlx::types::chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f").is_ok()
        || sqlx::types::chrono::DateTime::parse_from_rfc3339(t).is_ok()
}
