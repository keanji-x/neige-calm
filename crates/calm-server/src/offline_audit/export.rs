//! Offline composition only; never constructs application state or a repository.
use super::*;
use calm_truth::conversation_upgrade::{
    FileEvidence, FrozenBackup, OfflineSnapshot, SourceDeclaration,
};
use std::{
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct AuditInput {
    pub backup: FrozenBackup,
    /// Explicit external attestation of the complete owned root set, including
    /// an explicitly empty set. All roots share the backup's stopped window.
    pub complete_set_evidence: String,
    pub sources: Vec<SourceDeclaration>,
}
#[derive(Debug)]
pub enum ExportError {
    UnsupportedInput,
    Snapshot(calm_truth::conversation_upgrade::SnapshotError),
    Io(io::Error),
    Partial(PartialRead),
    Json(serde_json::Error),
    Manifest(calm_truth::conversation_upgrade::ManifestError),
    Publication(PublicationError),
}
impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for ExportError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<PublicationError> for ExportError {
    fn from(e: PublicationError) -> Self {
        Self::Publication(e)
    }
}

fn roots(input: &mut AuditInput) -> Result<Vec<TrustedReadRoot>, ExportError> {
    if input.complete_set_evidence.trim().is_empty()
        || input
            .sources
            .iter()
            .any(|s| s.ownership.trim().is_empty() || s.freeze_evidence.trim().is_empty())
    {
        return Err(ExportError::UnsupportedInput);
    }
    input.sources.sort_by(|a, b| a.path.cmp(&b.path));
    if input.sources.windows(2).any(|s| s[0].path == s[1].path) {
        return Err(ExportError::UnsupportedInput);
    }
    input
        .sources
        .iter()
        .map(|s| {
            TrustedReadRoot::open_absolute(Path::new(std::ffi::OsStr::from_bytes(&s.path)))
                .map_err(Into::into)
        })
        .collect()
}

// One production inventory composition for scan and export. The synchronous
// caller owns a runtime outside its entered context; no nested runtime block_on.
fn collect(
    runtime: &tokio::runtime::Runtime,
    input: &AuditInput,
    roots: &[TrustedReadRoot],
    mut writer: Option<&mut BuildWriter<'_>>,
) -> Result<([u8; 32], Vec<u8>), ExportError> {
    let audit = runtime
        .block_on(async {
            OfflineSnapshot::open(input.backup.clone())
                .await?
                .inventory()
                .await
        })
        .map_err(ExportError::Snapshot)?;
    let mut files = Vec::new();
    for (source, root) in roots.iter().enumerate() {
        let mut artifacts = std::collections::BTreeMap::new();
        let mut copy =
            |writer: Option<&mut BuildWriter<'_>>, path: &Path, bytes: &mut dyn io::Read| {
                let artifact = format!("file-{}-{}", source, artifacts.len());
                if let Some(writer) = writer {
                    writer
                        .write_file(Path::new(&artifact), bytes)
                        .map_err(|e| io::Error::other(format!("{e:?}")))?;
                } else {
                    io::copy(bytes, &mut io::sink())?;
                }
                artifacts.insert(path.to_path_buf(), artifact);
                Ok(())
            };
        let entries = if let Some(w) = writer.as_deref_mut() {
            w.inventory_owned_tree(root, |w, p, b| copy(Some(w), p, b))
        } else {
            inventory_owned_tree(root, |p, b| copy(None, p, b))
        }
        .map_err(ExportError::Partial)?;
        for entry in entries {
            files.push(FileEvidence {
                source,
                path: entry.relative.as_os_str().as_bytes().to_vec(),
                kind: format!("{:?}", entry.kind),
                mode: entry.identity.mode,
                length: if entry.kind == EntryKind::Regular {
                    entry.identity.size
                } else {
                    0
                },
                sha256: entry.sha256,
                artifact: artifacts.remove(&entry.relative),
                diagnostics: entry.diagnostics.iter().map(|d| format!("{d:?}")).collect(),
            });
        }
    }
    let manifest = audit
        .manifest(
            input.backup.path.as_os_str().as_bytes().to_vec(),
            &input.complete_set_evidence,
            input.sources.clone(),
            files,
        )
        .map_err(ExportError::Manifest)?;
    let root = manifest.root()?;
    Ok((root, serde_json::to_vec_pretty(&manifest)?))
}

/// Call from a synchronous thread, with an unentered runtime. Scan performs
/// the exact inventory and digest path, draining bytes without creating output.
pub fn scan(
    runtime: &tokio::runtime::Runtime,
    mut input: AuditInput,
) -> Result<Vec<u8>, ExportError> {
    let roots = roots(&mut input)?;
    collect(runtime, &input, &roots, None).map(|(_, manifest)| manifest)
}
pub fn export(
    runtime: &tokio::runtime::Runtime,
    mut input: AuditInput,
    output: PathBuf,
) -> Result<[u8; 32], BuildError<ExportError>> {
    let roots = roots(&mut input).map_err(BuildError::Build)?;
    let db_parent = TrustedReadRoot::open_absolute(
        input
            .backup
            .path
            .parent()
            .ok_or(BuildError::Build(ExportError::UnsupportedInput))?,
    )
    .map_err(|e| BuildError::Build(ExportError::Io(e)))?;
    // The frozen pathname must name a regular file in this watched DB parent.
    db_parent
        .regular(Path::new(
            input
                .backup
                .path
                .file_name()
                .ok_or(BuildError::Build(ExportError::UnsupportedInput))?,
        ))
        .map_err(|e| BuildError::Build(ExportError::Io(e)))?;
    let mut sources = roots.iter().collect::<Vec<_>>();
    sources.push(&db_parent);
    let parent = output
        .parent()
        .ok_or(BuildError::Build(ExportError::UnsupportedInput))?;
    let name = output
        .file_name()
        .ok_or(BuildError::Build(ExportError::UnsupportedInput))?;
    let publisher = Publisher::new(
        TrustedReadRoot::open_absolute(parent)
            .map_err(|e| BuildError::Build(ExportError::Io(e)))?,
        name,
        &sources,
    )
    .map_err(BuildError::Publication)?;
    publisher
        .build(|writer| {
            let (root, manifest) = collect(runtime, &input, &roots, Some(writer))?;
            writer.write_file(Path::new("manifest.json"), &mut manifest.as_slice())?;
            writer.write_file(Path::new("root.seal"), &mut root.as_slice())?;
            Ok(root)
        })
        .map(|(root, _durable)| root)
}
