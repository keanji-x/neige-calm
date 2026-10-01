//! Releasing a thread from the shared daemon (#1853). `thread/start` and `thread/resume`
//! subscribe the kernel's one connection, and codex keeps a thread (and its MCP servers)
//! loaded while any connection is subscribed. A later `thread/resume` reloads a released
//! thread, so releasing one whose Card is gone or whose session ended is safe.
use super::*;

/// Reply-wait budget for one release pass, shared by every thread in it: a pass runs after a
/// delete commits and on the sweeper tick (under `resume_replay_serial`).
const THREAD_UNSUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(5);

impl SharedCodexAppServer {
    /// Drop the threads' attribution and unsubscribe the kernel connection from them. The
    /// tombstone keeps a later `thread/started` for one from binding to a pending Card.
    async fn release_threads(&self, thread_ids: &[String]) {
        {
            let mut forgotten = self.forgotten_threads.lock().await;
            for thread_id in thread_ids {
                self.thread_cache.remove(thread_id);
                forgotten.remember(thread_id);
            }
        }
        self.unsubscribe_threads(thread_ids).await;
    }

    /// Best effort, with one [`THREAD_UNSUBSCRIBE_TIMEOUT`] for the whole batch: a failure is
    /// logged, with no connection there is nothing to unsubscribe, and the threads the budget
    /// does not reach are logged once and left loaded.
    pub(super) async fn unsubscribe_threads(&self, thread_ids: &[String]) {
        if thread_ids.is_empty() {
            return;
        }
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.unsubscribed_threads
                .lock()
                .expect("fake unsubscribed threads mutex")
                .extend(thread_ids.iter().cloned());
            return;
        }
        let deadline = tokio::time::Instant::now() + THREAD_UNSUBSCRIBE_TIMEOUT;
        let Ok(Some(client)) = tokio::time::timeout_at(deadline, self.running_client()).await
        else {
            return;
        };
        let mut unreached = 0_usize;
        for thread_id in thread_ids {
            if tokio::time::Instant::now() >= deadline {
                unreached += 1;
                continue;
            }
            // The deadline bounds the reply waits (a reply timeout leaves the connection usable).
            // Like every other RPC on the shared connection, a daemon that stops reading blocks
            // the send itself; cancelling a partial send would poison the connection for good.
            match client.thread_unsubscribe(thread_id, deadline).await {
                Ok(response) => tracing::info!(
                    target: "shared_codex_daemon::release_thread",
                    %thread_id,
                    status = %response.status,
                    "released shared codex thread"
                ),
                Err(error) => tracing::warn!(
                    target: "shared_codex_daemon::release_thread",
                    %thread_id,
                    %error,
                    "thread/unsubscribe failed; codex may keep the thread loaded"
                ),
            }
        }
        if unreached > 0 {
            tracing::warn!(
                target: "shared_codex_daemon::release_thread",
                unreached,
                "thread/unsubscribe budget spent; codex may keep these threads loaded"
            );
        }
    }

    /// Release every cached thread whose session POSITIVELY ended (`codex_threads_ended`:
    /// exited or failed); a superseded thread, or one no session row names yet, stays.
    /// Holds `resume_replay_serial` from the read through the RPCs, so a system-error recovery,
    /// which revives a Failed row under the same lock, lands wholly before or after this pass.
    pub async fn release_ended_threads(&self) -> Result<usize> {
        let _replay_guard = self.resume_replay_serial.lock().await;
        let cached: Vec<String> = self
            .thread_cache
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let ended = self.repo.codex_threads_ended(&cached).await?;
        self.release_threads(&ended).await;
        Ok(ended.len())
    }

    #[cfg(feature = "fixtures")]
    pub fn unsubscribed_threads_for_test(&self) -> Vec<String> {
        self.fake
            .as_ref()
            .expect("fake daemon")
            .unsubscribed_threads
            .lock()
            .expect("fake unsubscribed threads mutex")
            .clone()
    }
}
