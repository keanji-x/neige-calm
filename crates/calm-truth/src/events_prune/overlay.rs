//! Overlay retention's append-only production observation and bounded delete protocol.
//! Dev replay resets IDs, but runs without the background pruner; this is not a reset/ABA fence.
use super::{BATCH_YIELD, advance_watermark};
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::model::Overlay;
use sqlx::{Executor, Sqlite, SqlitePool};
use std::collections::BTreeMap;

const SCAN_PAGE_SIZE: i64 = 256;
const VALIDATION_ATTEMPTS: usize = 3;
// json_extract SQL TEXT/NULL values, not serde keys. Arrays/objects may group with literal
// text; numeric decode failures retain the old fail-closed behavior.
type Quad = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
type LatestRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);
type CandidateRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
struct Latest {
    id: i64,
    payload: String,
    supported: bool,
}
struct Candidate {
    id: i64,
    quad: Quad,
}
struct Observation {
    frontier: i64,
    latest: BTreeMap<Quad, Latest>,
}
impl Observation {
    async fn initial(pool: &SqlitePool) -> Result<Self> {
        // Capture BEFORE GROUP. A later append must be observed by refresh_tail.
        let frontier = frontier(pool).await?;
        #[cfg(test)]
        super::writer_tests::at_phase(super::writer_tests::Phase::Frontier).await;
        let mut observed = Self {
            frontier: 0,
            latest: BTreeMap::new(),
        };
        observed.observe_range(pool, frontier).await?;
        Ok(observed)
    }
    async fn refresh_tail(&mut self, pool: &SqlitePool) -> Result<()> {
        let frontier = frontier(pool).await?;
        if frontier > self.frontier {
            self.observe_range(pool, frontier).await?;
        }
        Ok(())
    }
    async fn observe_range(&mut self, pool: &SqlitePool, upper: i64) -> Result<()> {
        #[cfg(test)]
        super::writer_tests::count_observation(self.frontier == 0);
        let rows: Vec<LatestRow> = sqlx::query_as(include_str!("overlay_latest.sql"))
            .bind(self.frontier)
            .bind(upper)
            .fetch_all(pool)
            .await?;
        #[cfg(test)]
        super::writer_tests::pause_before_overlay_interpretation().await;
        for (id, plugin, entity_kind, entity_id, kind, payload) in rows {
            let supported = match serde_json::from_str::<Overlay>(&payload) {
                Ok(overlay) => !crate::validation::should_skip_overlay(&overlay),
                Err(_) => false,
            };
            let quad = (plugin, entity_kind, entity_id, kind);
            if self.latest.get(&quad).is_none_or(|current| id > current.id) {
                self.latest.insert(
                    quad,
                    Latest {
                        id,
                        payload,
                        supported,
                    },
                );
            }
        }
        self.frontier = upper;
        Ok(())
    }
    fn witness(&self, candidate: &Candidate) -> Option<&Latest> {
        self.latest
            .get(&candidate.quad)
            .filter(|latest| latest.supported && candidate.id < latest.id)
    }
}
async fn frontier<'e>(executor: impl Executor<'e, Database = Sqlite>) -> Result<i64> {
    // All overlay IDs, including malformed JSON; the kind index includes rowid.
    Ok(
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM events WHERE kind='overlay.set'")
            .fetch_one(executor)
            .await?
            .unwrap_or(0),
    )
}
pub(super) async fn prune(
    pool: &SqlitePool,
    horizon: i64,
    batch: i64,
    deleted: &mut u64,
    batches: &mut u64,
) -> Result<()> {
    let mut observed = Observation::initial(pool).await?;
    let pass_frontier = observed.frontier;
    let batch_size = usize::try_from(batch)
        .map_err(|_| CalmError::Internal("overlay retention batch is out of range".into()))?;
    let mut cursor = 0;
    let mut pending = Vec::new();
    loop {
        let rows: Vec<CandidateRow> = sqlx::query_as(include_str!("overlay_candidates.sql"))
            .bind(cursor)
            .bind(pass_frontier)
            .bind(horizon)
            .bind(SCAN_PAGE_SIZE)
            .fetch_all(pool)
            .await?;
        let Some(last) = rows.last() else {
            if !pending.is_empty() {
                flush(pool, horizon, pending, &mut observed, deleted, batches).await?;
            }
            return Ok(());
        };
        cursor = last.0;
        for (id, plugin, entity_kind, entity_id, kind) in rows {
            let candidate = Candidate {
                id,
                quad: (plugin, entity_kind, entity_id, kind),
            };
            if observed.witness(&candidate).is_some() {
                pending.push(candidate);
            }
        }
        // Accumulate actual candidates across sparse pages. A zero deletion batch is consumed.
        while pending.len() >= batch_size {
            let remainder = pending.split_off(batch_size);
            let candidates = std::mem::replace(&mut pending, remainder);
            if !flush(pool, horizon, candidates, &mut observed, deleted, batches).await? {
                return Ok(());
            }
            tokio::time::sleep(BATCH_YIELD).await;
        }
    }
}
async fn flush(
    pool: &SqlitePool,
    horizon: i64,
    candidates: Vec<Candidate>,
    observed: &mut Observation,
    deleted: &mut u64,
    batches: &mut u64,
) -> Result<bool> {
    for _ in 0..VALIDATION_ATTEMPTS {
        observed.refresh_tail(pool).await?;
        let mut witnesses = BTreeMap::new();
        let ids: Vec<_> = candidates
            .iter()
            .filter_map(|candidate| {
                let latest = observed.witness(candidate)?;
                witnesses.insert(latest.id, latest.payload.clone());
                Some(candidate.id)
            })
            .collect();
        if ids.is_empty() {
            return Ok(true);
        }
        let candidate_ids = serde_json::to_string(&ids)?;
        let witness_ids = serde_json::to_string(&witnesses.keys().copied().collect::<Vec<_>>())?;
        #[cfg(test)]
        super::writer_tests::at_phase(super::writer_tests::Phase::BeforeWrite).await;
        let mut tx = begin_immediate_tx(pool).await?;
        *batches += 1;
        if frontier(&mut **tx).await? > observed.frontier {
            tx.rollback().await?;
            continue;
        }
        // Only exact relied-on latest witnesses are authority. A sibling pruner can remove
        // them; disappearance stops safely rather than applying an old freeze decision.
        let current: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, payload FROM events WHERE kind='overlay.set' AND id IN (SELECT value FROM json_each(?1))"
        ).bind(witness_ids).fetch_all(&mut **tx).await?;
        if current.len() != witnesses.len()
            || current
                .iter()
                .any(|(id, payload)| witnesses.get(id) != Some(payload))
        {
            tx.rollback().await?;
            tracing::info!(
                deleted = *deleted,
                "events_prune: latest overlay witness changed; deferring remaining candidates"
            );
            return Ok(false);
        }
        let ids: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM events WHERE kind='overlay.set' AND at<?1 AND id IN (SELECT value FROM json_each(?2)) RETURNING id"
        ).bind(horizon).bind(candidate_ids).fetch_all(&mut **tx).await?;
        advance_watermark(&mut tx, &ids).await?;
        tx.commit().await?;
        *deleted += ids.len() as u64;
        return Ok(true);
    }
    tracing::info!(
        deleted = *deleted,
        "events_prune: repeated overlay appends deferred remaining candidates"
    );
    Ok(false)
}
