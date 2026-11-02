//! Read-only interpretation; completeness is source coverage, never eligibility.
use super::*;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactReason {
    Unclassified,
    UnknownField,
    UnsupportedVersion,
    MissingFrame,
    Missing,
    Null,
    Malformed,
    DuplicateMember,
    DuplicateIdentity,
    UnequalLengths,
    MissingOwnerDecoder,
    MissingProviderAuthority,
    DeferredReleased,
    DeferredObservationInterpretation,
    OwnerDiagnosticNotLeaf,
    AmbiguousRepresentation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Missing,
    Null,
    Present,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Interpretation {
    Blocked(FactReason),
    /// A source value checked by its leaf owner, not an execution proof.
    Stored(Value),
    ArrayLength(usize),
    Presence {
        required: bool,
        presence: Presence,
    },
}
#[derive(Debug, Clone, PartialEq)]
pub struct FieldFact {
    pub source: SourceRef,
    /// Missing fields bind the nearest existing ancestor and this expected path.
    pub expected_path: Option<String>,
    pub interpretation: Interpretation,
}
#[derive(Debug, PartialEq, Eq)]
pub enum WorkerKey {
    Stored(RawCell),
    Unidentified {
        original: FieldValue,
        reason: FactReason,
    },
}
#[derive(Debug)]
pub struct WorkerFacts {
    pub source: SourceRef,
    /// SQL storage class and original bytes, never a synthesized identity.
    pub logical_key: WorkerKey,
    pub fields: Vec<FieldFact>,
    pub frame: Option<FrameFacts>,
}
impl WorkerFacts {
    fn record(
        &mut self,
        source: SourceRef,
        expected_path: Option<String>,
        interpretation: Interpretation,
    ) {
        self.fields.push(FieldFact {
            source,
            expected_path,
            interpretation,
        });
    }
}
#[derive(Debug)]
pub enum FrameFacts {
    LegacyV1,
    /// Released bytes and nodes remain in the ledger; interpretation is deferred.
    DeferredReleased {
        original_version: u64,
    },
}
#[derive(Debug)]
pub struct CompleteFactAudit<'a> {
    pub(super) sources: CompleteSourceFacts<'a>,
    pub(super) workers: Vec<WorkerFacts>,
    pub worker_store: TableEvidence,
    pub(super) schema: super::schema_facts::SchemaFacts<'a>,
}
impl CompleteFactAudit<'_> {
    pub fn sources(&self) -> &CompleteSourceFacts<'_> {
        &self.sources
    }
    pub fn schema(&self) -> &super::schema_facts::SchemaFacts<'_> {
        &self.schema
    }
    pub fn workers(&self) -> &[WorkerFacts] {
        &self.workers
    }
    pub fn semantic_status(&self) -> &'static str {
        "BlockedAudit/NeedsQualification"
    }
}

pub fn classify_facts(
    audit: &CompleteBlockedAudit,
) -> Result<CompleteFactAudit<'_>, FactCoverageError> {
    let sources = source_facts(audit)?;
    let schema = schema_facts(&sources)?;
    let refs = super::schema_facts::References::new(&sources)?;
    let worker_store = refs.table("worker_sessions");
    let mut workers = Vec::new();
    for relation in audit
        .inventory()
        .relations
        .iter()
        .filter(|r| r.name == "worker_sessions")
    {
        for row in &relation.rows {
            let source = refs.row(relation, row)?;
            let column_cell = |name: &str| {
                relation
                    .columns
                    .iter()
                    .position(|c| c == name)
                    .map(|i| &row.cells[i])
            };
            let logical_key = match column_cell("id") {
                Some(c)
                    if matches!(
                        text_fact(c, "id", &audit.inventory().encoding),
                        Interpretation::Stored(_)
                    ) =>
                {
                    WorkerKey::Stored(c.clone())
                }
                value => WorkerKey::Unidentified {
                    original: value.map_or(FieldValue::Missing, |c| FieldValue::Present(c.clone())),
                    reason: if value.is_some() {
                        FactReason::Malformed
                    } else {
                        FactReason::Missing
                    },
                },
            };
            let mut worker = WorkerFacts {
                source,
                logical_key,
                fields: Vec::new(),
                frame: None,
            };
            for node in sources.nodes().iter().filter(|n| matches!(&n.source.location,
                SourceLocation::Cell { row: r, .. } if r.relation == relation.name && r.raw_key == row.key)) {
                worker.record(node.source.clone(), None, Interpretation::Blocked(FactReason::Unclassified));
                if super::root::deferred_observation_source(&node.source) {
                    worker.record(node.source.clone(), None, Interpretation::Blocked(FactReason::DeferredObservationInterpretation));
                }
            }
            for column in [
                "id",
                "state",
                "card_id",
                "track_id",
                "thread_id",
                "agent_session_id",
                "provider",
                "contract",
                "mode",
                "handle_state_json",
                "provider_execution_json",
            ] {
                let raw = column_cell(column);
                let node = sources.nodes().iter().find(|n| {
                    matches!(&n.source.location,
                    SourceLocation::Cell { row: r, column_name, json: None, .. }
                    if r.relation == relation.name && r.raw_key == row.key && column_name == column)
                });
                let cell = node.map(|n| n.source.clone());
                let anchor = cell.clone().unwrap_or_else(|| worker.source.clone());
                let expected = cell.is_none().then(|| column.into());
                for reason in [
                    (column == "provider_execution_json")
                        .then_some(FactReason::MissingProviderAuthority),
                    (column == "handle_state_json" && raw.is_none_or(|c| *c == RawCell::Null))
                        .then_some(FactReason::MissingFrame),
                    raw.is_none().then_some(FactReason::Missing),
                ]
                .into_iter()
                .flatten()
                {
                    worker.record(
                        anchor.clone(),
                        expected.clone(),
                        Interpretation::Blocked(reason),
                    );
                }
                let Some(raw) = raw else {
                    continue;
                };
                let source = cell.ok_or(FactCoverageError::InconsistentSource)?;
                if column != "handle_state_json" && column != "provider_execution_json" {
                    worker.record(
                        source,
                        None,
                        text_fact(raw, column, &audit.inventory().encoding),
                    );
                    continue;
                }
                let node = node.ok_or(FactCoverageError::InconsistentSource)?;
                let duplicate = sources
                    .duplicates()
                    .iter()
                    .any(|d| same_cell(&source, &d.first));
                if duplicate
                    || node.blocked != BlockedReason::Unclassified
                    || !matches!(*raw, RawCell::Text(_))
                {
                    worker.record(
                        source,
                        None,
                        Interpretation::Blocked(if duplicate {
                            FactReason::DuplicateMember
                        } else if *raw == RawCell::Null {
                            FactReason::Null
                        } else {
                            FactReason::Malformed
                        }),
                    );
                    continue;
                }
                // Only a validated, duplicate-free original cell enters Value.
                let text = schema::text(raw, &audit.inventory().encoding)
                    .map_err(|_| FactCoverageError::InconsistentSource)?;
                let value: Value = serde_json::from_str(&text)?;
                let mut frame = FrameView {
                    worker: &mut worker,
                    cell: source,
                    value: &value,
                    harness_sequence: column == "handle_state_json" && value.is_array(),
                };
                if column == "provider_execution_json" {
                    super::nested::provider(&mut frame, &text)?;
                } else {
                    if !value.is_object() && !value.is_array() {
                        frame.block(
                            "",
                            if value.is_null() {
                                FactReason::Null
                            } else {
                                FactReason::Malformed
                            },
                        )?;
                        continue;
                    }
                    frame.check::<u32>("/schema_version")?;
                    match frame.value_at("/schema_version").and_then(Value::as_u64) {
                        Some(1) => {
                            super::legacy::facts(&mut frame)?;
                            frame.worker.frame = Some(FrameFacts::LegacyV1);
                        }
                        Some(2..=5) if value.is_array() => {
                            frame.block("", FactReason::AmbiguousRepresentation)?;
                        }
                        Some(version @ 2..=5) => {
                            frame.block("", FactReason::DeferredReleased)?;
                            frame.worker.frame = Some(FrameFacts::DeferredReleased {
                                original_version: version,
                            });
                        }
                        Some(_) => {
                            frame.block("/schema_version", FactReason::UnsupportedVersion)?
                        }
                        None => frame.check::<u64>("/schema_version")?,
                    }
                }
            }
            workers.push(worker);
        }
    }
    Ok(CompleteFactAudit {
        sources,
        worker_store,
        workers,
        schema,
    })
}
pub(super) fn same_cell(a: &SourceRef, b: &SourceRef) -> bool {
    matches!((&a.location, &b.location),
        (SourceLocation::Cell { row: ar, column_index: ai, .. }, SourceLocation::Cell { row: br, column_index: bi, .. }) if ar == br && ai == bi)
}
pub(super) struct FrameView<'a> {
    pub worker: &'a mut WorkerFacts,
    pub cell: SourceRef,
    pub value: &'a Value,
    pub harness_sequence: bool,
}
impl<'a> FrameView<'a> {
    pub fn path(&self, path: &str) -> String {
        super::root::path(path, self.harness_sequence)
    }
    pub fn value_at(&self, path: &str) -> Option<&'a Value> {
        self.value.pointer(&self.path(path))
    }
    pub fn record(
        &mut self,
        path: &str,
        interpretation: Interpretation,
    ) -> Result<(), FactCoverageError> {
        let path = self.path(path);
        let path = path.as_str();
        let exact = self.source_at(path);
        if exact.is_none() && self.value_at(path).is_some() {
            return Err(FactCoverageError::InconsistentSource);
        }
        let source = exact.clone().unwrap_or_else(|| self.nearest_anchor(path));
        let expected_path = exact.is_none().then(|| path.into());
        self.worker.record(source, expected_path, interpretation);
        Ok(())
    }
    pub fn block(&mut self, path: &str, reason: FactReason) -> Result<(), FactCoverageError> {
        self.record(path, Interpretation::Blocked(reason))
    }
    pub fn source_at(&self, path: &str) -> Option<SourceRef> {
        let path = self.path(path);
        self.worker.fields.iter().find(|f| same_cell(&f.source, &self.cell)
            && matches!(&f.source.location, SourceLocation::Cell { json: Some(a), .. } if a.pointer() == path))
            .map(|f| f.source.clone())
    }
    pub fn nearest_anchor(&self, path: &str) -> SourceRef {
        let mut parent = path;
        loop {
            if let Some(source) = self.source_at(parent) {
                return source;
            }
            if parent.is_empty() {
                return self.cell.clone();
            }
            parent = parent.rsplit_once('/').map_or("", |(p, _)| p);
        }
    }
    pub fn check<T: serde::de::DeserializeOwned>(
        &mut self,
        path: &str,
    ) -> Result<(), FactCoverageError> {
        match self.value_at(path) {
            None => self.block(path, FactReason::Missing),
            Some(Value::Null) => self.block(path, FactReason::Null),
            Some(v) => self.decode::<T>(path, v),
        }
    }
    pub fn decode<T: serde::de::DeserializeOwned>(
        &mut self,
        path: &str,
        v: &Value,
    ) -> Result<(), FactCoverageError> {
        self.decode_with_success::<T>(path, v, true)
    }
    pub fn diagnose<T: serde::de::DeserializeOwned>(
        &mut self,
        path: &str,
    ) -> Result<(), FactCoverageError> {
        match self.value_at(path) {
            None => self.block(path, FactReason::Missing),
            Some(Value::Null) => self.block(path, FactReason::Null),
            Some(v) => self.decode_with_success::<T>(path, v, false),
        }
    }
    fn decode_with_success<T: serde::de::DeserializeOwned>(
        &mut self,
        path: &str,
        v: &Value,
        store: bool,
    ) -> Result<(), FactCoverageError> {
        self.decoded(path, serde_path_to_error::deserialize::<_, T>(v), store)
    }
    // Provider's live reader uses JSON text. Fragment text only matches that
    // codec; source presence, addresses and stored values still use the ledger.
    pub fn provider_check<T: serde::de::DeserializeOwned>(
        &mut self,
        path: &str,
        original: Option<&str>,
    ) -> Result<(), FactCoverageError> {
        let Some(v) = self.value_at(path).filter(|v| !v.is_null()) else {
            return self.check::<T>(path);
        };
        let fragment = v.to_string();
        let mut decoder = serde_json::Deserializer::from_str(original.unwrap_or(&fragment));
        self.decoded(
            path,
            serde_path_to_error::deserialize::<_, T>(&mut decoder),
            true,
        )
    }
    fn decoded<T>(
        &mut self,
        path: &str,
        result: Result<T, serde_path_to_error::Error<serde_json::Error>>,
        store: bool,
    ) -> Result<(), FactCoverageError> {
        match result {
            Ok(_) if !store => Ok(()),
            Ok(_) => self.record(
                path,
                Interpretation::Stored(
                    self.value_at(path)
                        .ok_or(FactCoverageError::InconsistentSource)?
                        .clone(),
                ),
            ),
            Err(error) => {
                self.block(path, FactReason::Malformed)?;
                let path = self.path(path);
                let mut leaf = path.clone();
                for segment in error.path() {
                    use serde_path_to_error::Segment;
                    let name = match segment {
                        Segment::Seq { index } => index.to_string(),
                        Segment::Map { key } => key.clone(),
                        Segment::Enum { variant } => variant.clone(),
                        Segment::Unknown => break,
                    };
                    leaf.push('/');
                    leaf.push_str(&name.replace('~', "~0").replace('/', "~1"));
                }
                if leaf != path {
                    if self.source_at(&leaf).is_some() {
                        self.block(&leaf, FactReason::Malformed)?;
                    } else {
                        self.block(&leaf, FactReason::OwnerDiagnosticNotLeaf)?;
                    }
                }
                if self
                    .value_at(&leaf)
                    .is_some_and(|v| v.is_object() || v.is_array())
                {
                    self.worker.record(
                        self.nearest_anchor(&leaf),
                        Some(leaf),
                        Interpretation::Blocked(FactReason::OwnerDiagnosticNotLeaf),
                    );
                }
                Ok(())
            }
        }
    }
}

fn text_fact(cell: &RawCell, column: &str, encoding: &str) -> Interpretation {
    use calm_types::worker::*;
    if *cell == RawCell::Null {
        return Interpretation::Blocked(FactReason::Null);
    }
    if let Ok(text) = schema::text(cell, encoding) {
        let valid = match column {
            "contract" => WorkerContract::try_from(text.clone()).is_ok(),
            "mode" => SessionMode::try_from(text.clone()).is_ok(),
            "provider" => WorkerProviderKind::try_from(text.clone()).is_ok(),
            "state" => WorkerSessionState::try_from(text.clone()).is_ok(),
            _ => !text.is_empty(),
        };
        if valid {
            return Interpretation::Stored(Value::String(text));
        }
    }
    Interpretation::Blocked(FactReason::Malformed)
}
