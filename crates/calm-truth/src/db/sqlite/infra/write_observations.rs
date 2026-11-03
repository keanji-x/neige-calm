//! Process-local rollup of managed SQLite write observations.
use std::collections::BTreeMap;
use std::sync::Mutex;

pub const WRITE_BUCKET_MS: i64 = 5 * 60 * 1000;
pub const WRITE_BUCKETS: i64 = 12;

#[derive(Clone, serde::Serialize, utoipa::ToSchema)]
pub struct WriteObservationBucket {
    pub started_at_ms: i64,
    pub transactions: u64,
    pub failures: u64,
    pub retries: u64,
    pub mean_wait_ms: f64,
}

#[derive(Default)]
pub(super) struct WriteObservations(Mutex<BTreeMap<i64, Bucket>>);

#[derive(Default)]
struct Bucket {
    transactions: u64,
    failures: u64,
    retries: u64,
    total_wait_ms: f64,
}

fn current_window(now_ms: i64) -> (i64, i64) {
    let end = now_ms.div_euclid(WRITE_BUCKET_MS) * WRITE_BUCKET_MS;
    (end - (WRITE_BUCKETS - 1) * WRITE_BUCKET_MS, end)
}

impl WriteObservations {
    pub(super) fn record(&self, now_ms: i64, wait_ms: f64, failed: bool, retries: usize) {
        let (_, time) = current_window(now_ms);
        let mut buckets = self.0.lock().expect("write observation lock poisoned");
        // A completion timestamp taken before locking can arrive after a newer bucket.
        let latest = buckets
            .last_key_value()
            .map_or(time, |(&latest, _)| latest.max(time));
        let (start, _) = current_window(latest);
        if time < start {
            return;
        }
        buckets.retain(|time, _| *time >= start);
        let bucket = buckets.entry(time).or_default();
        bucket.transactions += 1;
        bucket.failures += u64::from(failed);
        bucket.retries += retries as u64;
        bucket.total_wait_ms += wait_ms;
    }

    pub(super) fn snapshot(&self, now_ms: i64) -> Vec<WriteObservationBucket> {
        let (start, end) = current_window(now_ms);
        self.0
            .lock()
            .expect("write observation lock poisoned")
            .range(start..=end)
            .map(|(&started_at_ms, bucket)| WriteObservationBucket {
                started_at_ms,
                transactions: bucket.transactions,
                failures: bucket.failures,
                retries: bucket.retries,
                mean_wait_ms: bucket.total_wait_ms / bucket.transactions as f64,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_observation_rollup_keeps_newer_bucket_on_late_completion() {
        let observations = WriteObservations::default();
        observations.record(WRITE_BUCKET_MS, 7.0, false, 0);
        observations.record(WRITE_BUCKET_MS - 1, 2.0, true, 1);
        let buckets = observations.snapshot(WRITE_BUCKET_MS);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].failures, 1);
        assert_eq!(buckets[1].mean_wait_ms, 7.0);
    }

    #[test]
    fn write_observation_rollup_retains_counts_and_expires_old_buckets() {
        let observations = WriteObservations::default();
        observations.record(0, 2.0, false, 0);
        observations.record(1, 8.0, true, 2);
        let buckets = observations.snapshot(1);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].transactions, 2);
        assert_eq!(buckets[0].failures, 1);
        assert_eq!(buckets[0].retries, 2);
        assert_eq!(buckets[0].mean_wait_ms, 5.0);
        assert!(
            observations
                .snapshot(WRITE_BUCKETS * WRITE_BUCKET_MS)
                .is_empty()
        );
    }
}
