//! Durable owner reconciliation. Inventory errors are unclassified, never negative evidence.
use super::*;
use std::collections::{HashMap, HashSet};

pub fn alias(session: &str, id: &str) -> String {
    format!("planner:{session}:{id}")
}
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
                .any(|session| alias(session, &id) == entry.alias)
            {
                continue;
            }
            let owned = claims.iter().find(|(session, claim)| {
                claim.record().execution_id == id && alias(session, &id) == entry.alias
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
                        if alias(&session, &id) != entry.alias {
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

pub(crate) fn spawn(pool: SqliteDb, engine: ExecutionEngine, owners: Vec<ProviderExecution>) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await; // pass B, then the durable 30s sweep
            for owner in &owners {
                if let Err(error) = owner.sweep(&pool, Some(&engine)).await {
                    tracing::warn!(%error, "provider inventory failed; retry next pass");
                }
            }
        }
    });
}
