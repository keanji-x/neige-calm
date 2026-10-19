use super::*;

impl Executor {
    pub(super) async fn retire_registration(
        &self,
        receipt: ObjectRegistrationReceipt,
    ) -> anyhow::Result<ExecutionReply> {
        anyhow::ensure!(
            receipt.transfer.executor_boot == self.boot,
            "retirement executor boot mismatch"
        );
        let gate = self.gate(registration_resource(&receipt.transfer.registration))?;
        let _fence = gate.lock().await;
        let known = self
            .transfers
            .lock()
            .map_err(poison)?
            .get(&receipt.transfer.nonce)
            .cloned();
        anyhow::ensure!(
            known.as_ref() == Some(&receipt),
            "retirement requires exact immutable receipt"
        );
        if let Some(retired) = self.retired.lock().map_err(poison)?.get(&receipt.id) {
            anyhow::ensure!(*retired == receipt, "retirement receipt changed");
            return Ok(ExecutionReply::RegistrationRetired {
                registration: receipt.id,
            });
        }
        anyhow::ensure!(
            self.records
                .lock()
                .map_err(poison)?
                .values()
                .all(
                    |record| record.request.identity.source_registration != receipt.id
                        || record.durably_settled()
                ),
            "registration still owns a pending or live execution"
        );
        // Hold the live lookup lock through retirement: no new borrower can acquire a lease.
        let mut live = self.live_objects.lock().map_err(poison)?;
        let root = live
            .get(&receipt.id)
            .ok_or_else(|| anyhow::anyhow!("registration is Unwitnessed"))?;
        anyhow::ensure!(root.receipt == receipt, "retirement live receipt mismatch");
        // The map is the only persistent Arc owner: the lifecycle projection holds only facts.
        // While this lock is held no new borrower can start. Every extra Arc is an actual
        // retained source lease (for example, a prepared execution).
        // Concurrent borrower drops can only turn Busy into a safe later retry.
        anyhow::ensure!(
            Arc::strong_count(root) == 1,
            "registration has an in-flight source lease"
        );
        // Preserve immutable receipts and request/ACK history; only native storage ownership ends.
        self.store
            .write(&format!("{}.retired", receipt.id.as_str()), &receipt)?;
        self.retired
            .lock()
            .map_err(poison)?
            .insert(receipt.id.clone(), receipt.clone());
        live.remove(&receipt.id);
        Ok(ExecutionReply::RegistrationRetired {
            registration: receipt.id,
        })
    }
}
