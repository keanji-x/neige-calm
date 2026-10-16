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
        let key = registration_key(&registration);
        if let Some(previous) = self.registration_facts.lock().map_err(poison)?.get(&key) {
            anyhow::ensure!(
                registration_successor(previous, &registration),
                "stale/conflicting registration or binding version"
            );
        }
        let mut directory_changed = false;
        for known in self.registration_facts.lock().map_err(poison)?.values() {
            if registration_resource(known) == registration_resource(&registration) {
                anyhow::ensure!(
                    !older_runtime(&registration, known),
                    "resource generation/epoch regressed"
                );
                if !same_directory(known, &registration) {
                    anyhow::ensure!(
                        directory_recovery_version(known, &registration),
                        "directory replacement needs explicit recovery version"
                    );
                    directory_changed = true;
                }
            }
        }
        for previous in self.registrations.lock().map_err(poison)?.values() {
            if registration_resource(&previous.registration) == registration_resource(&registration)
                && (previous.receipt.transfer.incarnation != transfer.incarnation
                    || previous.receipt.transfer.shape != transfer.shape)
            {
                anyhow::ensure!(
                    directory_recovery_version(&previous.registration, &registration),
                    "object incarnation/layout replacement needs explicit recovery version"
                );
                directory_changed = true;
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
                && !record.phase.state().permits_handoff()
                && (directory_changed
                    || !same_runtime(&record.request.identity.scope, &root.registration))
            {
                anyhow::bail!("resource still occupied by an old execution/ticket");
            }
        }
        self.store.write(
            &format!("{}.registration", root.receipt.id.as_str()),
            &root.receipt,
        )?;
        self.registration_facts
            .lock()
            .map_err(poison)?
            .insert(key.clone(), root.registration.clone());
        let receipt = root.receipt.clone();
        self.live_objects
            .lock()
            .map_err(poison)?
            .insert(receipt.id.clone(), root.clone());
        self.registrations.lock().map_err(poison)?.insert(key, root);
        Ok(ExecutionReply::Registered(receipt))
    }
}

impl Executor {
    pub(crate) async fn serve_registered(
        self: &Arc<Self>,
        stream: &tokio::net::UnixStream,
        secret: &CapabilityToken,
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
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("registration is Unwitnessed; no held objects to export")
                })?;
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
            rights::send(
                stream,
                &ControlReply::Execution(Box::new(ExecutionReply::ExportAcknowledged)),
                &[],
            )
            .await?;
            drop(root);
            return Ok(());
        }
        let answer = self.dispatch(secret, request).await;
        rights::send(stream, &ControlReply::Execution(Box::new(answer)), &[]).await?;
        Ok(())
    }
}
