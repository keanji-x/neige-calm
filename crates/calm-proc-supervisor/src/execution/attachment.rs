use super::*;

impl Executor {
    pub(super) async fn attach_objects(
        &self,
        transfer: AttachRegistration,
        descriptors: Vec<std::os::fd::OwnedFd>,
    ) -> anyhow::Result<ExecutionReply> {
        anyhow::ensure!(
            transfer.executor_boot == self.boot,
            "attachment executor boot mismatch"
        );
        let objects = workspace::execution::objects::HeldSourceObjects::receive(
            transfer.shape.clone(),
            descriptors,
        )?;
        let registration = transfer.registration.clone();
        let gate = self.gate(registration_resource(&registration))?;
        let _fence = gate.lock().await;
        anyhow::ensure!(
            !self
                .retired
                .lock()
                .map_err(poison)?
                .contains_key(&transfer.registration_id),
            "registration lease retired; immutable history cannot restore live objects"
        );
        let key = registration_key(&registration);
        let mut source_changed = false;
        {
            let known = self.registration_facts.lock().map_err(poison)?;
            if let Some(previous) = known.get(&key) {
                anyhow::ensure!(
                    source_successor(previous, &transfer),
                    "stale/conflicting binding or source recovery version"
                );
            }
            let resource = registration_resource(&registration);
            let current_version = known
                .values()
                .filter(|previous| registration_resource(&previous.registration) == resource)
                .map(|previous| source_recovery_version(&previous.registration))
                .max();
            for previous in known.values() {
                if registration_resource(&previous.registration) != resource {
                    continue;
                }
                anyhow::ensure!(
                    !older_runtime(&registration, &previous.registration),
                    "resource generation/epoch regressed"
                );
                // Older bindings retain audit fences, not a permanent veto on a recovered source.
                if Some(source_recovery_version(&previous.registration)) == current_version
                    && !same_source(previous, &transfer)
                {
                    anyhow::ensure!(
                        directory_recovery_version(&previous.registration, &registration),
                        "object incarnation/layout replacement needs explicit recovery version"
                    );
                    source_changed = true;
                }
            }
        }
        let receipt = {
            // The nonce reserves one immutable receipt before IO; pending fsync retries keep its ID.
            let mut transfers = self.transfers.lock().map_err(poison)?;
            if let Some(existing) = transfers.get(&transfer.nonce) {
                anyhow::ensure!(
                    existing.transfer == transfer,
                    "transfer nonce reused with different immutable objects"
                );
                existing.clone()
            } else {
                anyhow::ensure!(
                    !transfers
                        .values()
                        .any(|receipt| receipt.id == transfer.registration_id),
                    "registration ID already belongs to another immutable transfer"
                );
                let receipt = ObjectRegistrationReceipt {
                    contract: EXECUTION_ENVELOPE_VERSION,
                    id: transfer.registration_id.clone(),
                    transfer,
                };
                transfers.insert(receipt.transfer.nonce.clone(), receipt.clone());
                receipt
            }
        };
        let incoming = Arc::new(RegisteredRoot::receive(receipt, objects)?);
        let retained = self
            .live_objects
            .lock()
            .map_err(poison)?
            .get(&incoming.receipt.id)
            .cloned();
        let root = if let Some(retained) = retained {
            anyhow::ensure!(
                retained.receipt == incoming.receipt,
                "registration ID has a different immutable receipt"
            );
            // A replay must not replace the original open descriptions with freshly opened
            // objects merely because their serialized stat/layout facts happen to compare equal.
            retained.validate()?;
            retained
        } else {
            incoming
        };
        // Generation/source changes require every old execution to have positively settled.
        for record in self.records.lock().map_err(poison)?.values() {
            if scope_resource(&record.request.identity.scope)
                == registration_resource(&root.registration)
                && !record.durably_settled()
                && (source_changed
                    || !same_runtime(&record.request.identity.scope, &root.registration))
            {
                anyhow::bail!("resource still occupied by an old execution/ticket");
            }
        }
        // Reserve validated source facts before IO: a failed fsync may already have published
        // this immutable receipt. Exact retries keep the claim, without creating a live lease.
        self.registration_facts
            .lock()
            .map_err(poison)?
            .insert(key.clone(), root.receipt.transfer.clone());
        self.store.write(
            &format!("{}.registration", root.receipt.id.as_str()),
            &root.receipt,
        )?;
        let receipt = root.receipt.clone();
        self.live_objects
            .lock()
            .map_err(poison)?
            .insert(receipt.id.clone(), root.clone());
        self.registrations
            .lock()
            .map_err(poison)?
            .insert(key, receipt.clone());
        Ok(ExecutionReply::Registered(receipt))
    }
}

impl Executor {
    pub(crate) async fn serve_registered(
        self: &Arc<Self>,
        stream: &tokio::net::UnixStream,
        secret: &CapabilityToken,
        mode: &ExecutionProtocolMode,
        request: ExecutionRequest,
        descriptors: Vec<std::os::fd::OwnedFd>,
    ) -> anyhow::Result<()> {
        use calm_session::control::{ControlMsg, ControlReply};
        anyhow::ensure!(
            self.authenticates(secret),
            "unregistered object-transfer client"
        );
        if let ExecutionRequest::Attach(transfer) = request {
            let reply = match self.attach_objects(transfer, descriptors).await {
                Ok(reply) => reply,
                Err(error) => refused(&error.to_string()),
            };
            rights::send(stream, &ControlReply::Execution(Box::new(reply)), &[]).await?;
            return Ok(());
        }
        if let ExecutionRequest::ObserveTransferredDirectories(request) = request {
            let reply = match self
                .observe_transferred_directories(*request, descriptors)
                .await
            {
                Ok(reply) => reply,
                Err(error) => qualification_or_refusal(&error),
            };
            rights::send(stream, &ControlReply::Execution(Box::new(reply)), &[]).await?;
            return Ok(());
        }
        anyhow::ensure!(
            descriptors.is_empty(),
            "descriptors on a non-attachment request"
        );
        if let ExecutionRequest::Export(export) = request {
            anyhow::ensure!(
                export.executor_boot == self.boot,
                "export from a different executor boot"
            );
            let root = self
                .live_objects
                .lock()
                .map_err(poison)?
                .get(&export.registration)
                .cloned();
            let Some(root) = root else {
                if *mode != ExecutionProtocolMode::RegisteredV3QueryV1 {
                    anyhow::bail!("registration is Unwitnessed; no held objects to export");
                }
                rights::send(
                    stream,
                    &ControlReply::Execution(Box::new(ExecutionReply::RegistrationUnwitnessed {
                        executor_boot: self.boot.clone(),
                        registration: export.registration,
                        nonce: export.nonce,
                    })),
                    &[],
                )
                .await?;
                return Ok(());
            };
            root.validate()?;
            rights::send(
                stream,
                &ControlReply::Execution(Box::new(ExecutionReply::Exported {
                    receipt: root.receipt.clone(),
                    nonce: export.nonce.clone(),
                })),
                &root.objects.descriptors(),
            )
            .await?;
            // This Arc retains every old descriptor even if another registration changes its binding.
            let (ack, received) = rights::receive::<ControlMsg>(stream).await?;
            anyhow::ensure!(received.is_empty(), "unexpected rights on export ACK");
            anyhow::ensure!(
                matches!(ack,ControlMsg::Execution(request) if matches!(request.as_ref(), ExecutionRequest::ExportAck(ack)
                    if ack.executor_boot == self.boot && ack.registration == root.receipt.id && ack.nonce == export.nonce)),
                "export ACK provenance mismatch"
            );
            drop(root);
            rights::send(
                stream,
                &ControlReply::Execution(Box::new(ExecutionReply::ExportAcknowledged)),
                &[],
            )
            .await?;
            return Ok(());
        }
        let answer = self.dispatch(secret, request).await;
        rights::send(stream, &ControlReply::Execution(Box::new(answer)), &[]).await?;
        Ok(())
    }
}
