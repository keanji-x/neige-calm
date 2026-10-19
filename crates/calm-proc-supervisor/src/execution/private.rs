use super::*;
use std::os::fd::OwnedFd;
use workspace::execution::objects::HeldSourceObjects;

impl Executor {
    /// Called only under the resource gate, after durable settlement (or a tombstone).
    /// Removing the native lease and its lifecycle projection has exactly one owner.
    pub(super) fn release_private(&self, id: &ExecutionId) -> anyhow::Result<()> {
        let registration = {
            let records = self.records.lock().map_err(poison)?;
            match records.get(id) {
                Some(record) if record.durably_settled() => match &record.request.source {
                    PreparedSource::Private(transfer) => Some(transfer.registration_id.clone()),
                    PreparedSource::Registered => None,
                },
                Some(_) => None,
                None => self
                    .tombstones
                    .lock()
                    .map_err(poison)?
                    .get(id)
                    .filter(|tombstone| tombstone.directory.is_some())
                    .and_then(|tombstone| tombstone.registration.clone()),
            }
        };
        if let Some(registration) = registration {
            self.live_objects
                .lock()
                .map_err(poison)?
                .remove(&registration);
            self.registrations
                .lock()
                .map_err(poison)?
                .retain(|_, receipt| receipt.id != registration);
        }
        Ok(())
    }

    pub(super) fn private_directory_available(
        &self,
        directory: &ExternalDirectoryId,
        id: Option<&ExecutionId>,
    ) -> anyhow::Result<()> {
        for record in self.records.lock().map_err(poison)?.values() {
            if matches!(&record.request.source, PreparedSource::Private(_))
                && Some(&record.request.identity.execution_id) != id
                && matches!(&record.request.identity.scope, ExecutionScope::ExternalDir { directory_id, .. } if directory_id == directory)
            {
                anyhow::bail!("private directory belongs to another execution");
            }
        }
        anyhow::ensure!(
            self.tombstones
                .lock()
                .map_err(poison)?
                .values()
                .all(|t| t.directory.as_ref() != Some(directory) || Some(&t.execution_id) == id),
            "private directory belongs to a retired execution"
        );
        Ok(())
    }

    pub(super) fn stage_private(
        &self,
        request: &PrepareExecution,
        transfer: &AttachRegistration,
        descriptors: Vec<OwnedFd>,
    ) -> anyhow::Result<Arc<RegisteredRoot>> {
        // Pre-upgrade shared executions have no recoverable native handles. Until the existing
        // boot/settlement rules prove them settled, do not admit any source reuse by inference.
        anyhow::ensure!(
            self.records
                .lock()
                .map_err(poison)?
                .values()
                .all(
                    |record| !matches!(record.request.source, PreparedSource::Registered)
                        || record.durably_settled()
                ),
            "unsettled pre-upgrade shared execution requires positive settlement"
        );
        let ExecutionRegistration::ExternalDir {
            directory_id,
            registration_version: 1,
            admission_open: true,
            ..
        } = &transfer.registration
        else {
            anyhow::bail!(
                "private source requires a fresh open ExternalDir registration at version 1"
            );
        };
        self.private_directory_available(directory_id, Some(&request.identity.execution_id))?;
        anyhow::ensure!(
            transfer.executor_boot == self.boot
                && transfer.registration_id == request.identity.source_registration
                && transfer.incarnation == request.identity.source_incarnation
                && registration_matches(&transfer.registration, &request.identity, true),
            "private transfer does not bind execution identity"
        );
        anyhow::ensure!(
            !self.transfers.lock().map_err(poison)?.values().any(|r| r.id
                == transfer.registration_id
                || r.transfer.nonce == transfer.nonce
                || registration_resource(&r.transfer.registration)
                    == registration_resource(&transfer.registration)),
            "private transfer overlaps an existing registration"
        );
        anyhow::ensure!(
            !self
                .records
                .lock()
                .map_err(poison)?
                .values()
                .any(
                    |r| r.request.identity.execution_id != request.identity.execution_id
                        && match &r.request.source {
                            PreparedSource::Private(t) =>
                                t.registration_id == transfer.registration_id
                                    || t.nonce == transfer.nonce,
                            PreparedSource::Registered => false,
                        }
                ),
            "private transfer identity already bound"
        );
        anyhow::ensure!(
            self.tombstones
                .lock()
                .map_err(poison)?
                .values()
                .all(|t| t.registration.as_ref() != Some(&transfer.registration_id)),
            "private registration identity belongs to a tombstone"
        );
        let objects = HeldSourceObjects::receive(transfer.shape.clone(), descriptors)?;
        let root = Arc::new(RegisteredRoot::receive(
            ObjectRegistrationReceipt {
                contract: PERSISTED_RECEIPT_CONTRACT_VERSION,
                id: transfer.registration_id.clone(),
                transfer: transfer.clone(),
            },
            objects,
        )?);
        let _cwd = root.launch_cwd(scope_cwd(&request.identity.scope))?;
        Ok(root)
    }

    pub(super) fn publish_private(&self, root: Arc<RegisteredRoot>) {
        // All fallible work is complete. Poison would be a fatal executor programming error.
        self.live_objects
            .lock()
            .expect("executor state poisoned")
            .insert(root.receipt.id.clone(), root.clone());
        self.registrations
            .lock()
            .expect("executor state poisoned")
            .insert(registration_key(&root.registration), root.receipt.clone());
    }
}
