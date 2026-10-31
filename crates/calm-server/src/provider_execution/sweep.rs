//! Durable owner reconciliation. Inventory errors are unclassified, never negative evidence.
use super::*;
use std::collections::{HashMap, HashSet};

impl ProviderExecution {
    pub async fn sweep(&self, pool: &SqliteDb, engine: Option<&ExecutionEngine>) -> Result<()> {
        let owners = sql::list(pool.read_pool()).await?;
        let mut claims = HashMap::new();
        let mut unclassified = HashSet::new();
        for (session, row) in owners {
            match row {
                Ok(claim) => {
                    claims.insert(session, claim);
                }
                Err(error) => {
                    tracing::warn!(%session, %error, "unclassified provider owner; retaining");
                    unclassified.insert(session);
                }
            }
        }
        let inventory =
            executions::consumer_inventory(pool.read_pool(), self.owner.consumer()).await?;
        for (id, row) in inventory {
            let entry = match row {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(%id, %error, "unclassified execution; retaining");
                    continue;
                }
            };
            // A malformed owner row is not evidence of an orphan. Its typed identity
            // still identifies all possible aliases belonging to that session.
            if unclassified
                .iter()
                .any(|session| self.owner.alias(session, &id) == entry.alias)
            {
                continue;
            }
            let owned = claims.iter().find(|(session, claim)| {
                claim.record().execution_id == id && self.owner.alias(session, &id) == entry.alias
            });
            let result = async {
                let execution_id = ExecutionId::try_from(id.clone()).map_err(conflict)?;
                if let Some((session, claim)) = owned {
                    if self.drivers.contains(&execution_id) {
                        return Ok(());
                    }
                    if claim.record().decision == Decision::Observing {
                        self.stop(pool, engine, session, claim, StopReason::DriverLost)
                            .await?;
                    }
                    if entry.settled {
                        self.finish(pool, engine, session, claim, None).await?;
                    }
                } else {
                    // (a) Absent or mismatched claim, including a deleted owner.
                    let mut tx = calm_truth::db::sqlite::begin_immediate_tx(pool).await?;
                    // Re-read under the writer lock: admission and owner deletion may
                    // race the inventory. A fresh exact claim always wins.
                    for (session, row) in sql::list(&mut **tx).await? {
                        if self.owner.alias(&session, &id) != entry.alias {
                            continue;
                        }
                        match row {
                            Err(_) => return Ok(()),
                            Ok(claim) if claim.record().execution_id == id => return Ok(()),
                            Ok(_) => {}
                        }
                    }
                    executions::request(
                        &mut tx,
                        &id,
                        if entry.settled { "retire" } else { "stop" },
                    )
                    .await?;
                    tx.commit().await?;
                    crate::execution_quiesce::drive(engine, [execution_id]);
                }
                Ok::<_, CalmError>(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(%id, %error, "provider sweep row failed; retry next pass");
            }
        }
        Ok(())
    }
}

/// Pass A runs after revoke, before listener, with no Engine dependency.
pub async fn boot_pass_a(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    sessions: &[String],
) -> Result<()> {
    for session in sessions {
        let Some(claim) = sql::load(&mut **tx, session).await? else {
            continue;
        };
        if claim.record().decision == Decision::Observing {
            let sql::Cas::Applied(_) = sql::stop(
                tx,
                session,
                &claim,
                StopReason::KernelRestarted,
                crate::model::now_ms(),
            )
            .await?
            else {
                continue;
            };
        }
        executions::request(tx, &claim.record().execution_id, "stop").await?;
    }
    Ok(())
}

/// Boot-owned control; dropping a clone does not stop reconciliation.
#[derive(Clone)]
pub(crate) struct ProviderSweep(std::sync::Arc<SweepControl>);
struct SweepControl {
    cancel: tokio_util::sync::CancellationToken,
    abort: tokio::task::AbortHandle,
    join: tokio::sync::Mutex<crate::harness::retained_join::RetainedJoin<()>>,
}
impl ProviderSweep {
    fn new(cancel: tokio_util::sync::CancellationToken, task: tokio::task::JoinHandle<()>) -> Self {
        Self(std::sync::Arc::new(SweepControl {
            cancel,
            abort: task.abort_handle(),
            join: tokio::sync::Mutex::new(crate::harness::retained_join::RetainedJoin::new(
                task,
                || {},
            )),
        }))
    }

    /// The stop command is synchronous, including when another observer owns the join lock.
    pub(crate) fn stop(&self) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        self.0.cancel.cancel();
        self.0.abort.abort();
        async {
            if self.0.join.lock().await.finish().await.is_err() {
                // An aborted task is also confirmed exited. RetainedJoin does not classify errors.
                tracing::warn!(
                    "provider sweep exited with a join error (including requested abort)"
                );
            }
            Ok(())
        }
    }
}

pub(crate) fn spawn(
    pool: SqliteDb,
    engine: ExecutionEngine,
    owners: Vec<ProviderExecution>,
) -> ProviderSweep {
    let cancel = tokio_util::sync::CancellationToken::new();
    let stop = cancel.clone();
    let task = tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = ticks.tick() => {} // pass B, then the durable 30s sweep
            }
            for owner in &owners {
                #[cfg(feature = "fixtures")]
                let pass = crate::test_seams::provider_pass::sweep(owner).await;
                if let Err(error) = owner.sweep(&pool, Some(&engine)).await {
                    tracing::warn!(%error, "provider inventory failed; retry next pass");
                }
                #[cfg(feature = "fixtures")]
                pass.returned();
            }
        }
    });
    ProviderSweep::new(cancel, task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    struct Exited(Option<oneshot::Sender<()>>);
    impl Drop for Exited {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    async fn held_sweep(idle: bool) -> (ProviderSweep, oneshot::Receiver<()>) {
        let (entered, ready) = oneshot::channel();
        let (exit, exited) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _exit = Exited(Some(exit));
            entered.send(()).unwrap();
            if idle {
                let mut ticks = tokio::time::interval(Duration::from_secs(30));
                ticks.tick().await;
                ticks.tick().await;
            } else {
                std::future::pending::<()>().await;
            }
        });
        ready.await.unwrap();
        (
            ProviderSweep::new(tokio_util::sync::CancellationToken::new(), task),
            exited,
        )
    }
    #[tokio::test]
    async fn stop_is_synchronous_before_join_poll_and_independent_of_lock() {
        let (sweep, exited) = held_sweep(false).await;
        let lock = sweep.0.join.lock().await;
        let join = sweep.stop();
        assert!(sweep.0.cancel.is_cancelled());
        exited.await.unwrap();
        drop(lock);
        join.await.unwrap();
    }
    #[tokio::test]
    async fn concurrent_stop_shares_exit_and_clone_drop_does_not_stop() {
        let (sweep, mut exited) = held_sweep(false).await;
        drop(sweep.clone());
        assert!(matches!(
            exited.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let clone = sweep.clone();
        let (a, b) = tokio::join!(sweep.stop(), clone.stop());
        a.unwrap();
        b.unwrap();
        exited.await.unwrap();
        sweep.stop().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_join_lock_wait_can_retry() {
        let (sweep, exited) = held_sweep(false).await;
        let lock = sweep.0.join.lock().await;
        let mut join = Box::pin(sweep.stop());
        std::future::poll_fn(|cx| {
            assert!(join.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(join);
        exited.await.unwrap();
        drop(lock);
        sweep.stop().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_join_observer_can_retry_and_idle_timer_is_aborted() {
        let (sweep, exited) = held_sweep(true).await;
        let mut join = Box::pin(sweep.stop());
        std::future::poll_fn(|cx| {
            assert!(join.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(join);
        sweep.stop().await.unwrap();
        exited.await.unwrap();
    }
    #[tokio::test]
    async fn recover_error_confirms_sweep_exit_before_returning() {
        use crate::{
            db::sqlite::SqlxRepo,
            event::EventBus,
            plugin_host::{PluginHost, PluginRegistry},
            state::{AppState, CodexClient, DaemonClient, WriteContext},
        };
        use clap::Parser;
        use std::sync::Arc;
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let mut state = AppState::from_parts(
            repo.clone(),
            EventBus::new(),
            Arc::new(DaemonClient::new_stub()),
            Arc::new(PluginHost::new_full(
                Arc::new(PluginRegistry::empty()),
                repo.clone(),
                std::path::PathBuf::new(),
                std::env::temp_dir(),
                Vec::new(),
                EventBus::new(),
                WriteContext::new(
                    crate::card_role_cache::CardRoleCache::new(),
                    crate::track_area_cache::TrackAreaCache::new(),
                ),
            )),
            Arc::new(CodexClient::new_stub()),
            None,
            None,
        );
        let (sweep, mut exited) = held_sweep(false).await;
        state.provider_sweep = Some(sweep.clone());
        // Fail the real fatal boot fence, after the sweep exists.
        sqlx::query("DROP TABLE area_folders")
            .execute(state.sqlite_db().unwrap().writer_pool())
            .await
            .unwrap();
        let cfg = crate::config::Config::parse_from([
            "calm-server",
            "--auth-password",
            "fixture-password",
        ]);
        let auth = crate::auth::ValidatedApplicationAuth::from_config(&cfg).unwrap();
        assert!(
            crate::application_boot::BootedApplication::recover(state, auth)
                .await
                .is_err()
        );
        let exit = exited.try_recv();
        // Clean up even under the wiring mutation so a red assertion leaves no detached test task.
        sweep.stop().await.unwrap();
        assert_eq!(
            exit,
            Ok(()),
            "recover Err must already have joined the sweep"
        );
    }
}
