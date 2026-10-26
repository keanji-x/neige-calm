use super::*;

impl Executor {
    pub(super) async fn reconcile_interrupted(&self, id: &ExecutionId) -> anyhow::Result<()> {
        // macOS restart has no qualified native absence observation in this contract.
        #[cfg(target_os = "linux")]
        {
            let gate = self.id_gate(id)?;
            let _id = gate.lock().await;
            let first = self.records.lock().map_err(poison)?.get(id).cloned();
            let Some(first) = first else {
                return Ok(());
            };
            let gate = self.gate(scope_resource(&first.request.identity.scope))?;
            let _fence = gate.lock().await;
            let mut record = self.record(id)?;
            if record.pending_persistence || self.processes.lock().map_err(poison)?.contains_key(id)
            {
                return Ok(());
            }
            let Phase::Unknown {
                slot_version,
                receipt: Some(receipt),
                ..
            } = &record.phase
            else {
                return Ok(());
            };
            record.validate()?;
            if receipt.slot_version != *slot_version
                || record.host_boot_id.is_empty()
                || record.host_boot_id != self.host_boot_id
            {
                return Ok(());
            }
            if !native_recovery::quiescent(&receipt.process, &receipt.scope_mechanism) {
                return Ok(());
            }
            let proof = InterruptedQuiescenceProof {
                host_boot_id: record.host_boot_id.clone(),
                helper_process: receipt.process.clone(),
                mechanism: receipt.scope_mechanism.clone(),
                scope: InterruptedScope::LinuxOriginalGroupAndSessionEmpty,
            };
            record.phase = Phase::InterruptedQuiescent {
                receipt: receipt.clone(),
                reason: ExecutionInterruption::ExecutorInterrupted,
                proof,
            };
            record.version = journal::RECOVERY_JOURNAL_VERSION;
            // No positive state is published before this durability barrier succeeds.
            self.persist(record)?;
        }
        #[cfg(target_os = "macos")]
        let _ = id;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn unknown(temp: &tempfile::TempDir) -> (Arc<Executor>, ExecutionId, ExecutionReceipt) {
        let executor = Executor::open(ExecutorConfig {
            journal_directory: temp.path().join("journal"),
            helper_program: std::fs::canonicalize("/bin/sh").unwrap(),
            kernel_client_secret: CapabilityToken::fresh().unwrap(),
        })
        .unwrap();
        let (request, _) = protocol_tests::private(&executor, temp.path());
        let mut child = std::process::Command::new("/bin/true").spawn().unwrap();
        let pid = child.id();
        let ticks = native_recovery::start_time(pid).unwrap();
        child.wait().unwrap();
        let receipt = ExecutionReceipt {
            identity: request.identity.clone(),
            slot_version: 1,
            process: NativeProcessIdentity::LinuxHelper(LinuxProcessIdentity {
                pid,
                start_time_ticks: ticks,
                nonce: CapabilityToken::fresh().unwrap(),
            }),
            scope_mechanism: executor.mechanism.clone(),
        };
        let id = request.identity.execution_id.clone();
        executor
            .persist(Journal {
                version: journal::JOURNAL_VERSION,
                host_boot_id: executor.host_boot_id.clone(),
                output_end: OutputEnd::Incomplete {
                    reason: "executor_restarted".into(),
                },
                request,
                phase: Phase::Unknown {
                    slot_version: 1,
                    receipt: Some(receipt.clone()),
                    reason: "executor_restarted".into(),
                },
                interactive_revoked: false,
                pending_persistence: false,
            })
            .unwrap();
        (executor, id, receipt)
    }

    async fn reconcile(executor: &Executor, id: &ExecutionId) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                executor.reconcile_interrupted(id).await.unwrap();
                if executor.record(id).unwrap().phase.state().permits_handoff() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn recovery_3192_persistence_failure_never_publishes_terminal() {
        for fault in [1, 2] {
            let temp = tempfile::tempdir().unwrap();
            let (executor, id, _) = unknown(&temp);
            executor
                .store
                .fail_execution_write
                .store(fault, std::sync::atomic::Ordering::SeqCst);
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if executor.reconcile_interrupted(&id).await.is_err() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(matches!(
                executor.inspect(&id).unwrap(),
                ExecutionReply::Inspected {
                    state: ExecutionState::Unknown { .. },
                    ..
                }
            ));
            reconcile(&executor, &id).await;
            assert!(executor.record(&id).unwrap().durably_settled());
            assert!(matches!(
                executor.store.load().unwrap()[0].phase,
                Phase::InterruptedQuiescent { .. }
            ));
        }
    }

    #[tokio::test]
    async fn recovery_3192_late_reports_cannot_demote_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let (executor, id, receipt) = unknown(&temp);
        reconcile(&executor, &id).await;
        let terminal = executor.record(&id).unwrap().phase.state();
        for event in [
            scope_helper::HelperEvent::LeaderExited { status: Some(0) },
            scope_helper::HelperEvent::Unknown {
                reason: "late report".into(),
            },
        ] {
            executor
                .apply_event(
                    &id,
                    scope_helper::HelperReport {
                        context: scope_helper::HelperContext {
                            execution_id: id.clone(),
                            executor_boot: receipt.identity.executor_boot.clone(),
                            nonce: receipt.process.nonce().clone(),
                            mechanism: receipt.scope_mechanism.clone(),
                        },
                        event,
                    },
                )
                .await
                .unwrap();
            assert_eq!(executor.record(&id).unwrap().phase.state(), terminal);
        }
    }

    #[tokio::test]
    async fn recovery_3192_empty_boot_stays_unknown_after_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let (executor, id, _) = unknown(&temp);
        let mut record = executor.record(&id).unwrap();
        record.host_boot_id.clear();
        executor.persist(record).unwrap();
        let config = executor.config.clone();
        drop(executor);
        let executor = Executor::open(config).unwrap();
        executor.reconcile_interrupted(&id).await.unwrap();
        assert!(matches!(
            executor.inspect(&id).unwrap(),
            ExecutionReply::Inspected {
                state: ExecutionState::Unknown { .. },
                ..
            }
        ));
    }
    #[tokio::test]
    async fn recovery_3192_receipt_slot_mechanism_and_proof_bindings_are_required() {
        let temp = tempfile::tempdir().unwrap();
        let (executor, id, _) = unknown(&temp);
        let original = executor.record(&id).unwrap();
        let mut record = original.clone();
        if let Phase::Unknown { slot_version, .. } = &mut record.phase {
            *slot_version += 1;
        }
        executor.persist(record).unwrap();
        executor.reconcile_interrupted(&id).await.unwrap();
        assert!(
            !executor
                .record(&id)
                .unwrap()
                .phase
                .state()
                .permits_handoff()
        );
        let mut record = original.clone();
        if let Phase::Unknown {
            receipt: Some(receipt),
            ..
        } = &mut record.phase
            && let ScopeMechanismIdentity::LinuxHelper(helper) = &mut receipt.scope_mechanism
        {
            helper.protocol_version += 1;
        }
        executor.persist(record).unwrap();
        executor.reconcile_interrupted(&id).await.unwrap();
        assert!(
            !executor
                .record(&id)
                .unwrap()
                .phase
                .state()
                .permits_handoff()
        );
        executor.persist(original).unwrap();
        reconcile(&executor, &id).await;
        let terminal = executor.record(&id).unwrap();
        terminal.validate().unwrap();
        for mismatch in [0, 1, 2] {
            let mut record = terminal.clone();
            if let Phase::InterruptedQuiescent { proof, .. } = &mut record.phase {
                match mismatch {
                    0 => proof.host_boot_id.clear(),
                    1 => record.version = journal::JOURNAL_VERSION,
                    _ => {
                        if let NativeProcessIdentity::LinuxHelper(p) = &mut proof.helper_process {
                            p.start_time_ticks += 1;
                        }
                    }
                }
            }
            assert!(record.validate().is_err());
        }
    }
}
