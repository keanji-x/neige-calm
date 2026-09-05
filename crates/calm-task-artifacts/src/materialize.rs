use crate::{Entry, Error, Materialized, Result, SlotBinding, SnapshotId};
use crate::{filesystem as disk, model, store::ArtifactStore};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
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
        self.publish_plan(plan, destination)
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
    fn publish_plan(&self, plan: Plan, destination: &Path) -> Result<Materialized> {
        self.check_plan(&plan)?;
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
        // Verify the actual prepared bytes and executable metadata, then fsync
        // directory entries from leaves to root before publishing the directory.
        let prepared_root = disk::open_dir(stage.path())?;
        for entry in plan.entries.values().rev() {
            let file = disk::open_beneath(&prepared_root, entry.path())?;
            match entry {
                Entry::Directory { .. } => file.sync_all()?,
                Entry::File {
                    digest,
                    bytes,
                    executable,
                    ..
                } => {
                    let mode = file.metadata()?.mode() & 0o777;
                    if mode != if *executable { 0o700 } else { 0o600 } {
                        return Err(Error::Integrity("prepared executable metadata".into()));
                    }
                    let actual = disk::copy_hash(file, &mut std::io::sink(), *bytes)?;
                    disk::verify_identity(&actual, digest, *bytes)?;
                }
            }
        }
        prepared_root.sync_all()?;
        disk::rename_new(stage.path(), &destination)?;
        disk::sync_dir(&parent)?;
        disk::sync_dir(&self.root.join("staging"))?;
        Ok(Materialized {
            destination,
            entries: plan.entries.into_values().collect(),
        })
    }
}
