use super::*;

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
        if let ExecutionRequest::Prepare(prepare) = request {
            let reply = match self.prepare_objects(prepare, descriptors).await {
                Ok(reply) => reply,
                Err(error) => refused(&error.to_string()),
            };
            rights::send(stream, &ControlReply::Execution(Box::new(reply)), &[]).await?;
            return Ok(());
        }
        if matches!(request, ExecutionRequest::Watch) {
            anyhow::ensure!(descriptors.is_empty(), "descriptors on Watch");
            let (watching, mut changes) = self.watch()?;
            rights::send(stream, &ControlReply::Execution(Box::new(watching)), &[]).await?;
            loop {
                // Lag is a transport disconnect: a reconnect snapshot forces a fresh Inspect.
                let (seq, id) = tokio::select! {
                    changed = changes.recv() => changed?,
                    incoming = rights::receive::<ControlMsg>(stream) => {
                        incoming?;
                        anyhow::bail!("Watch is a dedicated executor stream");
                    }
                };
                rights::send(
                    stream,
                    &ControlReply::Execution(Box::new(ExecutionReply::Changed { seq, id })),
                    &[],
                )
                .await?;
            }
        }
        anyhow::ensure!(
            descriptors.is_empty(),
            "descriptors on a non-attachment request"
        );
        let answer = self.dispatch(secret, request).await;
        rights::send(stream, &ControlReply::Execution(Box::new(answer)), &[]).await?;
        Ok(())
    }
}
