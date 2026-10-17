use super::*;

impl Executor {
    pub(super) async fn launch(
        self: &Arc<Self>,
        request: ConsumeExecution,
    ) -> anyhow::Result<ExecutionReply> {
        self.launch_with_fork_barrier_hook(request, || {}).await
    }

    async fn launch_with_fork_barrier_hook(
        self: &Arc<Self>,
        request: ConsumeExecution,
        before_final_validation: impl FnOnce(),
    ) -> anyhow::Result<ExecutionReply> {
        let first = self.record(&request.execution_id)?;
        let gate = self.gate(scope_resource(&first.request.identity.scope))?;
        let _fence = gate.lock().await;
        let mut record = self.record(&request.execution_id)?;
        Self::check_ticket(&record, &request)?;
        if !matches!(record.phase, Phase::Prepared | Phase::Reserved { .. }) {
            if record.pending_persistence {
                self.persist(record.clone())?;
            }
            return Ok(reply(&record));
        }
        anyhow::ensure!(
            !self.closing.load(std::sync::atomic::Ordering::SeqCst),
            "executor admission closed"
        );
        anyhow::ensure!(
            record.request.identity.executor_boot == self.boot,
            "executor boot changed"
        );
        let root = self.resolve(&record.request.identity, true)?;
        let cwd = root.launch_cwd(scope_cwd(&record.request.identity.scope))?;
        let previous_state = record
            .request
            .handoff
            .as_ref()
            .map(|handoff| self.record(&handoff.previous_execution))
            .transpose()?;
        let version = {
            // Short global guard follows the resource gate and never spans IO/fork.
            let mut aliases = self.aliases.lock().map_err(poison)?;
            let alias = &record.request.identity.process_alias;
            let version = match aliases.get(alias) {
                None => {
                    anyhow::ensure!(
                        record.request.handoff.is_none(),
                        "handoff alias slot absent"
                    );
                    1
                }
                Some(slot)
                    if slot.execution.as_ref() == Some(&request.execution_id)
                        && matches!(record.phase, Phase::Reserved { slot_version } if slot_version == slot.version) =>
                {
                    slot.version
                }
                Some(slot) => {
                    let handoff = record.request.handoff.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("process alias occupied; explicit handoff required")
                    })?;
                    anyhow::ensure!(
                        slot.execution.as_ref() == Some(&handoff.previous_execution)
                            && slot.version == handoff.expected_slot_version,
                        "alias handoff CAS failed"
                    );
                    anyhow::ensure!(
                        previous_state
                            .as_ref()
                            .is_some_and(|previous| previous.phase.state().permits_handoff()),
                        "previous execution is not positively drained/revoked/no-child"
                    );
                    slot.version
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("alias slot version overflow"))?
                }
            };
            aliases.insert(
                alias.clone(),
                AliasSlot {
                    execution: Some(request.execution_id.clone()),
                    version,
                },
            );
            version
        };
        record.phase = Phase::Reserved {
            slot_version: version,
        };
        // An exact in-memory reservation survives every pre-fork error, including failed fsync.
        record = self.latch(record)?;
        self.persist(record.clone())?;
        let context = scope_helper::HelperContext {
            execution_id: request.execution_id.clone(),
            executor_boot: self.boot.clone(),
            nonce: CapabilityToken::fresh()?,
            mechanism: self.mechanism.clone(),
        };
        let mut forking = record.clone();
        forking.phase = Phase::Forking {
            slot_version: version,
        };
        self.persist(forking)?;
        before_final_validation();
        if let Err(error) = root.validate().and_then(|()| cwd.revalidate()) {
            record.phase = Phase::NoChild {
                slot_version: version,
                reason: format!("root identity failed before fork: {error}"),
            };
            record = self.latch(record)?;
            self.persist(record.clone())?;
            return Ok(reply(&record));
        }
        let result = process::spawn(
            &self.helper_image,
            cwd.directory(),
            &record.request.specification,
            context,
        );
        let (process, native, reports) = match result {
            Ok(spawned) => spawned,
            Err(error) => {
                record.phase = Phase::Unknown {
                    slot_version: version,
                    receipt: None,
                    reason: format!("fork/setup result requires reconciliation: {error}"),
                };
                self.persist(record.clone())?;
                return Ok(reply(&record));
            }
        };
        let receipt = ExecutionReceipt {
            identity: record.request.identity.clone(),
            slot_version: version,
            process: native,
            scope_mechanism: self.mechanism.clone(),
        };
        record.phase = Phase::Spawned(receipt.clone());
        self.processes
            .lock()
            .map_err(poison)?
            .insert(request.execution_id.clone(), process.clone());
        if let Err(error) = self.persist(record.clone()) {
            record.phase = Phase::Unknown {
                slot_version: version,
                receipt: Some(receipt),
                reason: format!("fork succeeded but receipt could not be made durable: {error}"),
            };
            // Unknown is not a durable success acknowledgement. Preserve native ownership for reconciliation.
            record = self.latch(record)?;
        }
        self.monitor(request.execution_id, reports, process);
        Ok(reply(&record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_session::control::IoMode;
    use std::os::{fd::AsFd, unix::fs::MetadataExt};
    use workspace::{execution::objects::HeldSourceObjects, identity::WorkspaceCwd};

    #[tokio::test]
    async fn execution_no_child_persistence_failure_retries_original_conclusion_without_fork() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let executor = Executor::open(ExecutorConfig {
            journal_directory: journal.clone(),
            helper_program: std::fs::canonicalize("/bin/sh").unwrap(),
            kernel_client_secret: CapabilityToken::fresh().unwrap(),
        })
        .unwrap();
        let path = temp.path().join("source");
        std::fs::create_dir(&path).unwrap();
        let stat = std::fs::metadata(&path).unwrap();
        let held = HeldSourceObjects::open(&path, false).unwrap();
        let authority = ExecutionAuthority {
            grant_id: ExecutionGrantId::fresh().unwrap(),
            authorization_binding: AuthorizationBindingId::fresh().unwrap(),
            authorization_version: 1,
        };
        let directory_id = ExternalDirectoryId::fresh().unwrap();
        let transfer = AttachRegistration {
            registration_id: ObjectRegistrationId::fresh().unwrap(),
            executor_boot: executor.boot.clone(),
            nonce: TransferNonce::fresh().unwrap(),
            incarnation: SourceObjectIncarnation::ExternalDir(
                ExternalDirectoryIncarnationId::fresh().unwrap(),
            ),
            registration: ExecutionRegistration::ExternalDir {
                directory_id: directory_id.clone(),
                root: DirectoryRegistration {
                    path: path.clone(),
                    device: stat.dev(),
                    inode: stat.ino(),
                },
                registration_version: 1,
                admission_open: true,
                authority: authority.clone(),
            },
            shape: held.shape().clone(),
        };
        let files = held
            .descriptors()
            .into_iter()
            .map(|fd| fd.as_fd().try_clone_to_owned().unwrap())
            .collect();
        assert!(matches!(
            executor
                .attach_objects(transfer.clone(), files)
                .await
                .unwrap(),
            ExecutionReply::Registered(_)
        ));
        let request = PrepareExecution {
            identity: ExecutionIdentity {
                execution_id: ExecutionId::fresh().unwrap(),
                executor_boot: executor.boot.clone(),
                ticket_id: LaunchTicketId::fresh().unwrap(),
                source_registration: transfer.registration_id.clone(),
                source_incarnation: transfer.incarnation.clone(),
                authority,
                scope: ExecutionScope::ExternalDir {
                    directory_id,
                    registration_version: 1,
                    cwd: WorkspaceCwd::root(),
                },
                operation_id: "no-child".into(),
                specification_id: SpecificationId::fresh().unwrap(),
                process_alias: "no-child".into(),
            },
            ticket: CapabilityToken::fresh().unwrap(),
            control_tokens: ExecutionControlTokens::fresh().unwrap(),
            handoff: None,
            specification: ExecutionSpecification {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "printf payload > created".into()],
                environment: vec![],
                io_mode: IoMode::Pipe,
                replay_bytes: 1024,
            },
        };
        assert!(matches!(
            executor.prepare(request.clone()).await.unwrap(),
            ExecutionReply::Prepared
        ));
        let consume = ConsumeExecution {
            execution_id: request.identity.execution_id.clone(),
            ticket_id: request.identity.ticket_id.clone(),
            ticket: request.ticket.clone(),
        };
        let moved_source = temp.path().join("moved-source");
        let moved_journal = temp.path().join("moved-journal");
        let failed = executor
            .launch_with_fork_barrier_hook(consume.clone(), || {
                std::fs::rename(&path, &moved_source).unwrap();
                std::fs::rename(&journal, &moved_journal).unwrap();
            })
            .await;
        std::fs::rename(&moved_source, &path).unwrap();
        std::fs::rename(&moved_journal, &journal).unwrap();
        assert!(failed.is_err());
        let original = executor.record(&consume.execution_id).unwrap();
        assert!(matches!(original.phase, Phase::NoChild { .. }));
        assert!(matches!(
            executor.launch(consume).await.unwrap(),
            ExecutionReply::Receipt {
                receipt: None,
                state: ExecutionState::NoChild { .. }
            }
        ));
        let durable = executor.store.load().unwrap().pop().unwrap();
        assert_eq!(
            serde_json::to_value(&durable.phase).unwrap(),
            serde_json::to_value(&original.phase).unwrap()
        );
        assert!(!path.join("created").exists());
        assert!(executor.processes.lock().unwrap().is_empty());
    }
}
