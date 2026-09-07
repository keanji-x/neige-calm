use crate::{Entry, Error, Materialized, Result, SlotBinding, SnapshotId};
use crate::{filesystem as disk, model, store::ArtifactStore};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Default)]
struct Plan {
    entries: BTreeMap<String, Entry>,
    sources: BTreeMap<String, SnapshotId>,
}
impl Plan {
    fn directory(&mut self, path: &str) -> Result<()> {
        match self.entries.get(path) {
            Some(Entry::Directory { .. }) => return Ok(()),
            Some(_) => return Err(Error::Invalid("file/directory collision".into())),
            None => {}
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.directory(parent)?;
        }
        self.entries.insert(
            path.to_owned(),
            Entry::Directory {
                path: path.to_owned(),
            },
        );
        Ok(())
    }
    fn add(&mut self, entry: Entry, snapshot: &SnapshotId) -> Result<()> {
        if let Entry::Directory { path } = &entry {
            return self.directory(path);
        }
        let name = entry.path().to_owned();
        if let Some((parent, _)) = name.rsplit_once('/') {
            self.directory(parent)?;
        }
        if self.entries.contains_key(&name) {
            return Err(Error::Invalid("duplicate materialized path".into()));
        }
        self.sources.insert(name.clone(), snapshot.clone());
        self.entries.insert(name, entry);
        Ok(())
    }
}

impl ArtifactStore {
    /// Materialize exact named slots under disjoint relative destinations. The
    /// caller has already authorized these bindings. Missing output paths fail;
    /// presence of a slot is not evidence that it passed a gate.
    pub fn materialize(&self, inputs: &[SlotBinding], destination: &Path) -> Result<Materialized> {
        self.publish_plan(self.slot_plan(inputs)?, destination)
    }

    /// Explicit reconciliation of a possibly published destination, before consumer
    /// start. The caller must supply the original frozen bindings and keep the
    /// destination and its ancestors protected from writers through verification
    /// and launch. Exact inventory, types, hashes, modes and single file links must
    /// match; no destination is created, rewritten or replaced. Successful checks
    /// repeat file/directory and publication fsync barriers after an uncertain rename.
    /// This checks content, not Operation ownership or consumption authority.
    pub fn verify_materialized(
        &self,
        inputs: &[SlotBinding],
        destination: &Path,
    ) -> Result<Materialized> {
        let plan = self.slot_plan(inputs)?;
        let destination = self.destination(destination)?;
        let _lock = self.lock()?;
        let root = disk::open_dir(&destination)?;
        if root.metadata()?.dev() != disk::open_dir(&self.root)?.metadata()?.dev() {
            return Err(Error::Unsupported("prepared destination filesystem".into()));
        }
        self.verify_plan(&plan, &root)?;
        disk::sync_dir(destination.parent().expect("validated destination parent"))?;
        disk::sync_dir(&self.root.join("staging"))?;
        Ok(Materialized {
            destination,
            entries: plan.entries.into_values().collect(),
        })
    }

    fn slot_plan(&self, inputs: &[SlotBinding]) -> Result<Plan> {
        if inputs.len() > self.limits.max_entries {
            return Err(Error::Limit("input bindings".into()));
        }
        for binding in inputs {
            model::path(&binding.into, &self.limits)?;
        }
        model::disjoint(&inputs.iter().map(|b| b.into.as_str()).collect::<Vec<_>>())?;
        let mut plan = Plan::default();
        for binding in inputs {
            let snapshot = self.open_snapshot(&binding.snapshot)?;
            let slot = snapshot
                .manifest
                .outputs
                .iter()
                .find(|s| s.name == binding.output)
                .ok_or_else(|| Error::Invalid(format!("unknown output {}", binding.output)))?;
            if let Some(missing) = snapshot
                .missing_outputs()
                .into_iter()
                .find(|m| m.output == binding.output)
            {
                return Err(Error::MissingOutput {
                    output: missing.output,
                    paths: missing.paths,
                });
            }
            plan.directory(&binding.into)?;
            for entry in &snapshot.manifest.entries {
                if slot.paths.iter().any(|p| model::contains(p, entry.path())) {
                    let destination = format!("{}/{}", binding.into, entry.path());
                    model::path(&destination, &self.limits)?;
                    plan.add(entry.at(destination), &binding.snapshot)?;
                }
            }
            self.check_plan(&plan)?;
        }
        self.check_plan(&plan)?;
        Ok(plan)
    }

    /// Prepare the FULL candidate for an already-authorized repair. This includes
    /// failed/unvalidated work and missing-output candidates, but never `.git`.
    pub fn materialize_candidate(
        &self,
        id: &SnapshotId,
        destination: &Path,
    ) -> Result<Materialized> {
        let snapshot = self.open_snapshot(id)?;
        let mut plan = Plan::default();
        for entry in snapshot.manifest.entries {
            plan.add(entry, id)?;
        }
        self.publish_plan(plan, destination)
    }

    fn check_plan(&self, plan: &Plan) -> Result<()> {
        model::validate_entries(
            &plan.entries.values().cloned().collect::<Vec<_>>(),
            &self.limits,
        )
    }
    fn destination(&self, destination: &Path) -> Result<PathBuf> {
        disk::absolute(destination)?;
        let parent = destination
            .parent()
            .ok_or_else(|| Error::Invalid("destination parent".into()))?;
        disk::open_dir(parent)?;
        let parent = fs::canonicalize(parent)?;
        let destination = parent.join(
            destination
                .file_name()
                .ok_or_else(|| Error::Invalid("destination name".into()))?,
        );
        if disk::overlaps(&destination, &self.root) {
            return Err(Error::Invalid(
                "destination and store must not overlap".into(),
            ));
        }
        Ok(destination)
    }

    fn publish_plan(&self, plan: Plan, destination: &Path) -> Result<Materialized> {
        self.check_plan(&plan)?;
        let destination = self.destination(destination)?;
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Err(Error::DestinationExists(destination)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let _lock = self.lock()?;
        // Preparation uses the store's private staging area, so startup can
        // reclaim abandoned data without traversing a worker destination. Atomic
        // publication requires the destination to be on this same filesystem.
        let stage = tempfile::Builder::new()
            .prefix("prepare-")
            .permissions(fs::Permissions::from_mode(0o700))
            .rand_bytes(12)
            .tempdir_in(self.root.join("staging"))?;
        for entry in plan.entries.values() {
            match entry {
                Entry::Directory { path } => disk::private_dir(&stage.path().join(path))?,
                Entry::File {
                    path,
                    digest,
                    bytes,
                    executable,
                } => {
                    let snapshot_id = plan.sources.get(path).ok_or_else(|| {
                        Error::Integrity("missing prepared source binding".into())
                    })?;
                    let root = disk::open_dir(&self.snapshot_path(snapshot_id))?;
                    let source = disk::open_beneath(&root, &format!("objects/{digest}"))?;
                    let mut target = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(stage.path().join(path))?;
                    let actual = disk::copy_hash(source, &mut target, *bytes)?;
                    disk::verify_identity(&actual, digest, *bytes)?;
                    disk::set_mode(&target, if *executable { 0o700 } else { 0o600 })?;
                }
            }
        }
        self.verify_plan(&plan, &disk::open_dir(stage.path())?)?;
        disk::rename_new(stage.path(), &destination)?;
        disk::sync_dir(destination.parent().expect("validated destination parent"))?;
        disk::sync_dir(&self.root.join("staging"))?;
        Ok(Materialized {
            destination,
            entries: plan.entries.into_values().collect(),
        })
    }

    fn verify_plan(&self, plan: &Plan, root: &File) -> Result<()> {
        let mut seen = BTreeSet::new();
        let directories =
            std::iter::once("").chain(plan.entries.values().filter_map(|entry| match entry {
                Entry::Directory { path } => Some(path.as_str()),
                _ => None,
            }));
        // Enumerate only expected directories through pinned FDs. Unknown entries
        // fail immediately; no unbounded walk or traversal into an extra directory.
        for relative in directories {
            let directory = if relative.is_empty() {
                root.try_clone()?
            } else {
                disk::open_beneath(root, relative)?
            };
            let meta = directory.metadata()?;
            if !meta.is_dir() || meta.mode() & 0o7777 != 0o700 {
                return Err(Error::Integrity("prepared directory metadata".into()));
            }
            for item in fs::read_dir(disk::fd_path(&directory))? {
                let name = item?
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::Integrity("prepared non-UTF-8 entry".into()))?;
                let path = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                if !plan.entries.contains_key(&path) || !seen.insert(path) {
                    return Err(Error::Integrity("unexpected prepared entry".into()));
                }
            }
        }
        if seen.len() != plan.entries.len() {
            return Err(Error::Integrity("missing prepared entry".into()));
        }
        // Verify and sync leaves before their parent directories. Hash the actual
        // prepared bytes with the same bounded streaming verifier as capture.
        for entry in plan.entries.values().rev() {
            let file = disk::open_beneath(root, entry.path())?;
            if let Entry::File {
                digest,
                bytes,
                executable,
                ..
            } = entry
            {
                let meta = file.metadata()?;
                if !meta.is_file()
                    || meta.nlink() != 1
                    || meta.mode() & 0o7777 != if *executable { 0o700 } else { 0o600 }
                {
                    return Err(Error::Integrity("prepared file metadata".into()));
                }
                let actual = disk::copy_hash(file.try_clone()?, &mut std::io::sink(), *bytes)?;
                disk::verify_identity(&actual, digest, *bytes)?;
            }
            file.sync_all()?;
        }
        root.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileArtifactPath, FileCaptureRequest, Limits};

    #[test]
    fn materialized_reconciliation_repairs_uncertain_publication_sync() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let limits = Limits {
            max_entries: 16,
            max_file_bytes: 1024,
            max_total_bytes: 1024,
            max_manifest_bytes: 4096,
            max_path_bytes: 128,
            max_depth: 8,
        };
        let store = ArtifactStore::open_files(&root, limits.clone()).unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"original").unwrap();
        let path = FileArtifactPath::new("result", &limits).unwrap();
        let receipt = store
            .capture_file(
                FileCaptureRequest {
                    key: "capture",
                    boundary_id: "stopped",
                    output: "result",
                    path: &path,
                },
                || {
                    Ok(OpenOptions::new()
                        .read(true)
                        .custom_flags(nix::libc::O_NONBLOCK)
                        .open(&source)?)
                },
            )
            .unwrap();
        let bindings = [SlotBinding {
            snapshot: receipt.snapshot,
            output: "result".into(),
            into: "input".into(),
        }];
        let destination = temp.path().join("consumer");
        let fail = || {
            let parent = temp.path().to_path_buf();
            move |at: &Path| {
                if at == parent {
                    Err(std::io::Error::other("publication fsync interrupted").into())
                } else {
                    Ok(())
                }
            }
        };
        assert!(matches!(
            disk::faults::with_sync(fail(), || store.materialize(&bindings, &destination)),
            Err(Error::Io(_))
        ));
        assert!(
            destination.is_dir(),
            "actual rename must precede the injected failure"
        );
        assert!(matches!(
            store.materialize(&bindings, &destination),
            Err(Error::DestinationExists(_))
        ));
        assert!(
            matches!(
                disk::faults::with_sync(fail(), || store
                    .verify_materialized(&bindings, &destination)),
                Err(Error::Io(_))
            ),
            "reconciliation must finish publication durability before success"
        );
        fs::remove_file(source).unwrap();
        let reopened = ArtifactStore::open_files(&root, limits).unwrap();
        let verified = reopened
            .verify_materialized(&bindings, &destination)
            .unwrap();
        assert_eq!(verified.destination, destination);
        assert_eq!(
            fs::read(destination.join("input/result")).unwrap(),
            b"original"
        );
    }
}
