//! Pure new-session ranking. The caller supplies one provider's profiles in config
//! order and a snapshot; this module neither reserves capacity nor acquires leases.

use std::time::SystemTime;

use serde::Deserialize;

use super::BackendRef;
use super::config::BackendProfile;
use super::signals::{BackendHealth, BackendObservation, BackendSignals, Observed, QuotaStatus};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    #[default]
    WeightedLeastInflight,
    Ordered,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IneligibleReason {
    NotAcceptingNewSessions,
    UnknownHealth,
    Health(BackendHealth),
    UnknownInflight,
    UnknownCooldown,
    CoolingDown,
    UnknownQuota,
    QuotaExhausted,
    SoftInflightLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendRejection {
    pub backend: BackendRef,
    pub reason: IneligibleReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Candidate(BackendRef),
    NoEligibleBackend(Vec<BackendRejection>),
}

fn eligible(
    profile: &BackendProfile,
    signals: &BackendSignals,
    now: SystemTime,
) -> Result<u32, IneligibleReason> {
    if !profile.accepts_new_sessions {
        return Err(IneligibleReason::NotAcceptingNewSessions);
    }
    match &signals.health {
        Observed::Known(BackendHealth::Ready) => {}
        Observed::Known(health) => return Err(IneligibleReason::Health(health.clone())),
        Observed::Unknown => return Err(IneligibleReason::UnknownHealth),
    }
    match &signals.cooldown {
        Observed::Unknown => return Err(IneligibleReason::UnknownCooldown),
        Observed::Known(Some(cooldown)) if cooldown.until > now => {
            return Err(IneligibleReason::CoolingDown);
        }
        Observed::Known(_) => {}
    }
    match signals.quota {
        Observed::Unknown => return Err(IneligibleReason::UnknownQuota),
        Observed::Known(QuotaStatus::Exhausted) => return Err(IneligibleReason::QuotaExhausted),
        Observed::Known(QuotaStatus::Available) => {}
    }
    let inflight = match signals.inflight {
        Observed::Unknown => return Err(IneligibleReason::UnknownInflight),
        Observed::Known(count) => count,
    };
    if profile
        .max_inflight
        .is_some_and(|limit| inflight >= limit.get())
    {
        return Err(IneligibleReason::SoftInflightLimit);
    }
    Ok(inflight)
}

/// `observations` is the owner's current snapshot, with at most one accepted
/// observation per backend. Source-generation validation belongs to that owner.
pub fn select(
    profiles: &[BackendProfile],
    observations: &[BackendObservation],
    policy: SelectionPolicy,
    now: SystemTime,
) -> Decision {
    let mut best: Option<(&BackendProfile, u32)> = None;
    let mut rejections = Vec::new();
    for profile in profiles {
        let backend = profile.backend_ref();
        let unknown = BackendSignals::default();
        let signals = observations
            .iter()
            .find(|observation| observation.backend == backend)
            .map_or(&unknown, |observation| &observation.signals);
        match eligible(profile, signals, now) {
            Ok(inflight) => {
                if policy == SelectionPolicy::Ordered {
                    return Decision::Candidate(backend);
                }
                // Exact rational ordering without floating-point ties or u32 overflow.
                // Strict less-than retains config order when scores are equal.
                if best.is_none_or(|(current, count)| {
                    u64::from(inflight) * u64::from(current.weight.get())
                        < u64::from(count) * u64::from(profile.weight.get())
                }) {
                    best = Some((profile, inflight));
                }
            }
            Err(reason) => rejections.push(BackendRejection { backend, reason }),
        }
    }
    match best {
        Some((profile, _)) => Decision::Candidate(profile.backend_ref()),
        None => Decision::NoEligibleBackend(rejections),
    }
}
