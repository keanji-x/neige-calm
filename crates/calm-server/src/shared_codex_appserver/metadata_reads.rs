//! One transport implementation for existing operator/historical reads and
//! source-bound selection reads. This layer invokes admission, not Human policy.
use super::*;
use futures_util::future::BoxFuture;

// Existing public read entry points retain their caller-owned authorization.
// Canonical selection uses the required admitted entry points instead; there
// is no optional context or default success in SelectionSource.
fn existing_caller_read() -> BoxFuture<'static, Result<()>> {
    Box::pin(async { Ok(()) })
}

impl SharedCodexAppServer {
    /// `model/list`, drained across codex's pagination cursor. Read-only and connection-only:
    /// it never spawns or heals the daemon.
    pub async fn model_list(&self, deadline: tokio::time::Instant) -> Result<Vec<CodexModel>> {
        self.model_list_admitted(
            deadline,
            &NativePromptAdmission::new(|| Ok(()), existing_caller_read),
        )
        .await
    }

    /// The actual effect owner checks the caller after connecting and before
    /// every page. A successful prior page is not admission for the next one.
    pub(crate) async fn model_list_admitted(
        &self,
        deadline: tokio::time::Instant,
        admission: &NativePromptAdmission<'_>,
    ) -> Result<Vec<CodexModel>> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref()
            && fake.reject_model_list.load(Ordering::SeqCst)
        {
            admission.check_local().await?;
            return Err(CalmError::CodexRefused(
                "model/list failed: catalog unavailable for this account (code -32603)".into(),
            ));
        }
        let client = self.connected_client().await?;
        let gate = admission;
        let mut models: Vec<CodexModel> = Vec::new();
        let mut skipped = 0usize;
        let mut cursor: Option<String> = None;
        // A server that echoes a cursor forever would otherwise pin this loop.
        for _ in 0..MODEL_LIST_MAX_PAGES {
            let page = gate.finish(
                client
                    .model_list_gated(cursor.as_deref(), deadline, gate.gate())
                    .await,
            )?;
            // Entries are decoded ONE AT A TIME; a malformed one is skipped rather than failing the
            // page, because an all-or-nothing decode would present a fabricated outage.
            for entry in page.data {
                match serde_json::from_value::<CodexModel>(entry) {
                    Ok(model) => models.push(model),
                    Err(e) => {
                        skipped += 1;
                        tracing::warn!(
                            target = "shared_codex_daemon::model_list",
                            error = %e,
                            "skipping a model/list entry this build cannot decode"
                        );
                    }
                }
            }
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => {
                    if skipped > 0 {
                        tracing::warn!(
                            target = "shared_codex_daemon::model_list",
                            skipped,
                            kept = models.len(),
                            "model/list returned entries this build cannot decode"
                        );
                    }
                    return Ok(models);
                }
            }
        }
        // The peer never cleared `nextCursor`. A partial catalog is never presented as a whole
        // one, which is also why `?` above abandons the pages already collected.
        Err(CalmError::CodexAppServer(format!(
            "model/list did not terminate its pagination within {MODEL_LIST_MAX_PAGES} pages"
        )))
    }

    /// `config/read` — the layer-merged effective config, narrowed to the model defaults;
    /// `cwd` selects the project layers.
    pub async fn config_read(
        &self,
        cwd: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<CodexConfig> {
        self.config_read_admitted(
            cwd,
            deadline,
            &NativePromptAdmission::new(|| Ok(()), existing_caller_read),
        )
        .await
    }

    /// The caller's bound cwd is revalidated by the same admission after the
    /// connected-client gate. Neither a lock wait nor a prior read is a grant.
    pub(crate) async fn config_read_admitted(
        &self,
        cwd: Option<&str>,
        deadline: tokio::time::Instant,
        admission: &NativePromptAdmission<'_>,
    ) -> Result<CodexConfig> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            admission.check_local().await?;
            fake.config_read_cwds
                .lock()
                .expect("fake shared codex config-read cwds mutex poisoned")
                .push(cwd.map(str::to_string));
            if fake.reject_config_read.load(Ordering::SeqCst) {
                return Err(CalmError::CodexRefused(
                    "config/read failed: no such workspace (code -32602)".into(),
                ));
            }
            if let Some(config) = fake
                .config_read
                .lock()
                .expect("fake shared codex config-read mutex poisoned")
                .clone()
            {
                return Ok(config);
            }
        }
        let client = self.connected_client().await?;
        let gate = admission;
        Ok(gate
            .finish(client.config_read_gated(cwd, deadline, gate.gate()).await)?
            .config)
    }
}
