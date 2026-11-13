//! Legacy disposal uses the existing renderer reader and completion owner.
use super::*;
use legacy_client::{LegacyAbsent, open_attachment, signal_child_checked};

pub(super) async fn signal(
    entry: &RendererEntry,
    sig: ProcSignal,
    strict: bool,
) -> anyhow::Result<()> {
    if strict {
        timeout(
            Duration::from_secs(1),
            signal_child_checked(&entry.supervisor_sock, &entry.proc_id, sig),
        )
        .await?
    } else {
        entry.shutdown_signal(sig).await;
        Ok(())
    }
}

impl TerminalRendererRegistry {
    /// Caller holds deletion admission fences. Absence is only the current registry contract.
    pub(crate) async fn quiesce_legacy(&self, cfg: RendererConfig) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.repo.is_some(),
            "legacy disposal reader requires repository"
        );
        let (entry, running) = if let Some(entry) = self.get(&cfg.terminal_id) {
            anyhow::ensure!(
                !entry.control_via_engine,
                "typed renderer requires execution owner"
            );
            anyhow::ensure!(
                entry.supervisor_sock == cfg.supervisor_sock,
                "renderer owner endpoint changed"
            );
            let Some((_observation, attached)) = timeout(
                Duration::from_secs(2),
                open_attachment(&cfg.supervisor_sock, &entry.proc_id),
            )
            .await??
            else {
                if entry
                    .exit
                    .lock()
                    .map_err(|_| anyhow::anyhow!("exit state poisoned"))?
                    .is_some()
                {
                    anyhow::ensure!(
                        entry.await_exit_persisted(EXIT_PERSIST_GRACE).await
                            == ExitPersistWait::Persisted,
                        "known exit persistence failure"
                    );
                }
                return Ok(());
            };
            (entry, attached.running)
        } else {
            let established = match ensure_entry(
                cfg,
                self.repo.clone(),
                self.task_hook(),
                None,
                self.output_wake.clone(),
                Establish::Disposal,
            )
            .await
            {
                Ok(established) => established,
                Err(error) if error.is::<LegacyAbsent>() => return Ok(()),
                Err(error) => return Err(error),
            };
            let entry = Arc::new(established.entry);
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| anyhow::anyhow!("renderer registry poisoned"))?;
            // Admission fences exclude new viewers/launches; never replace a reader if violated.
            if entries.contains_key(&entry.terminal_id) {
                entry.abort_tasks();
                anyhow::bail!("renderer changed during disposal");
            }
            entries.insert(entry.terminal_id.clone(), entry.clone());
            (entry, established.running)
        };
        let persisted = if running {
            self.stop_legacy(&entry, true).await? == RendererDropOutcome::ExitPersisted
        } else {
            // running=false can also mean poisoned supervisor state. Only the reader's explicit
            // Exited arm plus every required write/callback may grant completion.
            entry.await_exit_persisted(EXIT_PERSIST_GRACE).await == ExitPersistWait::Persisted
        };
        anyhow::ensure!(persisted, "legacy terminal exit persistence unverified");
        Ok(())
    }

    /// Commit-only detach. Typed subscriptions remain owned by their Retired lifecycle.
    pub(crate) fn finish_legacy_deletion(&self, terminal: &str, card: &str) {
        if let Ok(mut entries) = self.entries.lock()
            && entries
                .get(terminal)
                .is_some_and(|entry| !entry.control_via_engine)
            && let Some(entry) = entries.remove(terminal)
        {
            entry.abort_tasks();
        }
        self.remove_hook_settings(card);
    }
}
