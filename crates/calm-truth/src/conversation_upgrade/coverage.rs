use super::{facts::*, strict_json, *};
use std::collections::HashSet;

/// The only complete-facts entry: no SQL, selection or caller-made inventory.
pub fn source_facts(
    audit: &CompleteBlockedAudit,
) -> Result<CompleteSourceFacts<'_>, FactCoverageError> {
    let mut out = CompleteSourceFacts {
        audit,
        nodes: Vec::new(),
        duplicates: Vec::new(),
    };
    let inventory = audit.inventory();
    out.push(
        SourceLocation::Encoding,
        SourceKind::Encoding,
        "encoding",
        &inventory.encoding,
    )?;
    for (ordinal, tuple) in inventory.schema.iter().enumerate() {
        out.push(
            SourceLocation::Schema { ordinal },
            SourceKind::Schema,
            "schema",
            tuple,
        )?;
    }
    for (ordinal, tuple) in inventory.table_list.iter().enumerate() {
        out.push(
            SourceLocation::TableList { ordinal },
            SourceKind::TableList,
            "table_list",
            tuple,
        )?;
    }
    for relation in &inventory.relations {
        out.push(
            SourceLocation::Relation {
                name: relation.name.clone(),
            },
            SourceKind::Relation,
            "relation",
            &(
                &relation.name,
                &relation.columns,
                &relation.key_columns,
                relation.source_count,
            ),
        )?;
        for (ordinal, tuple) in relation.column_facts.iter().enumerate() {
            out.push(
                SourceLocation::ColumnMetadata {
                    relation: relation.name.clone(),
                    ordinal,
                },
                SourceKind::ColumnMetadata,
                "xinfo",
                tuple,
            )?;
        }
        if relation.column_facts.len() != relation.columns.len()
            || relation.source_count != relation.rows.len() as i64
        {
            return Err(FactCoverageError::InconsistentSource);
        }
        let mut primary = Vec::new();
        for (i, tuple) in relation.column_facts.iter().enumerate() {
            if tuple.len() != 7 {
                return Err(FactCoverageError::InconsistentSource);
            }
            if let RawCell::Integer(order) = tuple[5]
                && order > 0
            {
                primary.push((order, i));
            }
        }
        primary.sort();
        for row in &relation.rows {
            if row.cells.len() != relation.columns.len() {
                return Err(FactCoverageError::InconsistentSource);
            }
            let row_source = RowSource {
                relation: relation.name.clone(),
                raw_key: row.key.clone(),
                declared_key: primary
                    .iter()
                    .map(|(_, i)| (relation.columns[*i].clone(), row.cells[*i].clone()))
                    .collect(),
                row_sha256: evidence_digest("row", row)?,
            };
            out.push(
                SourceLocation::Row(row_source.clone()),
                SourceKind::Row,
                "row",
                row,
            )?;
            for (column_index, (column_name, cell)) in
                relation.columns.iter().zip(&row.cells).enumerate()
            {
                let location = SourceLocation::Cell {
                    row: row_source.clone(),
                    column_index,
                    column_name: column_name.clone(),
                    cell_sha256: evidence_digest("cell", cell)?,
                    json: None,
                };
                let kind = match cell {
                    RawCell::Null => SourceKind::SqlNull,
                    RawCell::DerivedNotStored => SourceKind::DerivedNotStored,
                    _ => SourceKind::Cell,
                };
                out.push(location, kind, "cell", cell)?;
                // Closed field contracts, not TEXT shape or a suffix heuristic.
                // session_row::worker_session_from_row / session_projection write
                // arbitrary JSON state; provider_execution::Claim owns Record.
                if relation.name == "worker_sessions"
                    && matches!(
                        column_name.as_str(),
                        "handle_state_json" | "provider_execution_json"
                    )
                    && !matches!(cell, RawCell::Null | RawCell::DerivedNotStored)
                {
                    out.json(cell, &inventory.encoding)?;
                }
            }
        }
    }
    // Duplicated source identities cannot silently collapse into a complete set.
    let mut seen = HashSet::new();
    if out.nodes.iter().any(|n| !seen.insert(&n.source.location)) {
        return Err(FactCoverageError::InconsistentSource);
    }
    Ok(out)
}
impl CompleteSourceFacts<'_> {
    fn push(
        &mut self,
        location: SourceLocation,
        kind: SourceKind,
        domain: &str,
        original: &impl serde::Serialize,
    ) -> Result<(), FactCoverageError> {
        self.nodes.push(SourceFact {
            source: SourceRef {
                source_sha256: *self.audit.source_sha256(),
                source_len: self.audit.source_len(),
                namespace: "main",
                location,
                evidence_sha256: evidence_digest(domain, original)?,
            },
            kind,
            blocked: BlockedReason::Unclassified,
        });
        Ok(())
    }
    fn json(&mut self, cell: &RawCell, encoding: &str) -> Result<(), FactCoverageError> {
        let field = self
            .nodes
            .last_mut()
            .ok_or(FactCoverageError::InconsistentSource)?;
        if !matches!(cell, RawCell::Text(_)) {
            field.blocked = BlockedReason::InvalidJsonStorage;
            return Ok(());
        }
        let Ok(text) = schema::text(cell, encoding) else {
            field.blocked = BlockedReason::InvalidJsonEncoding;
            return Ok(());
        };
        let parsed = match strict_json::parse(&text) {
            Ok(parsed) => parsed,
            Err(strict_json::JsonFailure::Syntax) => {
                field.blocked = BlockedReason::MalformedJson;
                return Ok(());
            }
            Err(strict_json::JsonFailure::Incomplete) => {
                return Err(FactCoverageError::JsonTraversal {
                    source_ref: Box::new(field.source.clone()),
                });
            }
        };
        let source = field.source.clone();
        let with_path = |path| {
            let mut reference = source.clone();
            if let SourceLocation::Cell { json, .. } = &mut reference.location {
                *json = Some(path);
            }
            reference
        };
        self.duplicates
            .extend(
                parsed
                    .duplicates
                    .into_iter()
                    .map(|(first, repeated)| DuplicateMember {
                        first: with_path(first),
                        repeated: with_path(repeated),
                    }),
            );
        for (path, kind) in parsed.nodes {
            self.nodes.push(SourceFact {
                source: with_path(path),
                kind: SourceKind::Json(kind),
                blocked: BlockedReason::Unclassified,
            });
        }
        Ok(())
    }
}
