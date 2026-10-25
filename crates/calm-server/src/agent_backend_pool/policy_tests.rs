use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use calm_types::runtime::AgentProvider;

use super::BackendId;
use super::config::{BackendCredentials, BackendProfile};
use super::policy::{BackendRejection, Decision, IneligibleReason, SelectionPolicy, select};
use super::signals::*;

fn profile(id: &str, weight: u32) -> BackendProfile {
    BackendProfile {
        provider: AgentProvider::Codex,
        id: BackendId::try_from(id.to_owned()).unwrap(),
        weight: NonZeroU32::new(weight).unwrap(),
        max_inflight: None,
        accepts_new_sessions: true,
        credentials: BackendCredentials::CodexHome {
            path: format!("/homes/{id}").into(),
        },
    }
}

fn observation(profile: &BackendProfile, inflight: u32) -> BackendObservation {
    BackendObservation {
        backend: profile.backend_ref(),
        connection_generation: 1,
        observed_at: SystemTime::UNIX_EPOCH,
        signals: BackendSignals {
            health: Observed::Known(BackendHealth::Ready),
            inflight: Observed::Known(inflight),
            cooldown: Observed::Known(None),
            quota: Observed::Known(QuotaStatus::Available),
        },
    }
}

#[test]
fn agent_backend_pool_weighted_least_inflight_order() {
    let profiles = [profile("a", 1), profile("b", 4), profile("c", 2)];
    let signals = [
        observation(&profiles[0], 2),
        observation(&profiles[1], 3),
        observation(&profiles[2], 2),
    ];
    assert_eq!(
        select(
            &profiles,
            &signals,
            SelectionPolicy::WeightedLeastInflight,
            SystemTime::UNIX_EPOCH
        ),
        Decision::Candidate(profiles[1].backend_ref())
    );
}

#[test]
fn agent_backend_pool_weighted_fractional_scores_and_large_counts() {
    let profiles = [profile("a", u32::MAX - 1), profile("b", u32::MAX)];
    let signals = [
        observation(&profiles[0], u32::MAX),
        observation(&profiles[1], u32::MAX),
    ];
    assert_eq!(
        select(
            &profiles,
            &signals,
            SelectionPolicy::WeightedLeastInflight,
            SystemTime::UNIX_EPOCH
        ),
        Decision::Candidate(profiles[1].backend_ref())
    );
}

#[test]
fn agent_backend_pool_ties_keep_configuration_order() {
    let profiles = [profile("z", 2), profile("a", 4)];
    for counts in [[1, 2], [0, 0]] {
        let signals = [
            observation(&profiles[1], counts[1]),
            observation(&profiles[0], counts[0]),
        ];
        for policy in [
            SelectionPolicy::WeightedLeastInflight,
            SelectionPolicy::Ordered,
        ] {
            assert_eq!(
                select(&profiles, &signals, policy, SystemTime::UNIX_EPOCH),
                Decision::Candidate(profiles[0].backend_ref())
            );
        }
    }
}

#[test]
fn agent_backend_pool_ordered_ignores_scores_and_skips_ineligible() {
    let mut profiles = [profile("a", 1), profile("b", 4)];
    let signals = [observation(&profiles[0], 100), observation(&profiles[1], 0)];
    assert_eq!(
        select(
            &profiles,
            &signals,
            SelectionPolicy::Ordered,
            SystemTime::UNIX_EPOCH
        ),
        Decision::Candidate(profiles[0].backend_ref())
    );
    profiles[0].accepts_new_sessions = false;
    assert_eq!(
        select(
            &profiles,
            &signals,
            SelectionPolicy::Ordered,
            SystemTime::UNIX_EPOCH
        ),
        Decision::Candidate(profiles[1].backend_ref())
    );
}

#[test]
fn agent_backend_pool_unknown_and_ineligible_reasons_for_both_policies() {
    let mut profile = profile("a", 1);
    profile.max_inflight = NonZeroU32::new(2);
    let ready = observation(&profile, 2);
    let mut cases = Vec::new();
    for (health, reason) in [
        (Observed::Unknown, IneligibleReason::UnknownHealth),
        (
            Observed::Known(BackendHealth::NotConfigured),
            IneligibleReason::Health(BackendHealth::NotConfigured),
        ),
        (
            Observed::Known(BackendHealth::Unavailable(UnavailableReason::NotRunning)),
            IneligibleReason::Health(BackendHealth::Unavailable(UnavailableReason::NotRunning)),
        ),
    ] {
        let mut o = ready.clone();
        o.signals.health = health;
        cases.push((o, reason));
    }
    let mut o = ready.clone();
    o.signals.inflight = Observed::Unknown;
    cases.push((o, IneligibleReason::UnknownInflight));
    let mut o = ready.clone();
    o.signals.cooldown = Observed::Unknown;
    cases.push((o, IneligibleReason::UnknownCooldown));
    let mut o = ready.clone();
    o.signals.cooldown = Observed::Known(Some(Cooldown {
        until: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        evidence: CooldownEvidence::RateLimited,
        generation: 3,
    }));
    cases.push((o, IneligibleReason::CoolingDown));
    let mut o = ready.clone();
    o.signals.quota = Observed::Unknown;
    cases.push((o, IneligibleReason::UnknownQuota));
    let mut o = ready.clone();
    o.signals.quota = Observed::Known(QuotaStatus::Exhausted);
    cases.push((o, IneligibleReason::QuotaExhausted));
    cases.push((ready, IneligibleReason::SoftInflightLimit));
    for policy in [
        SelectionPolicy::WeightedLeastInflight,
        SelectionPolicy::Ordered,
    ] {
        for (signals, reason) in &cases {
            assert_eq!(
                select(
                    std::slice::from_ref(&profile),
                    std::slice::from_ref(signals),
                    policy,
                    SystemTime::UNIX_EPOCH
                ),
                Decision::NoEligibleBackend(vec![BackendRejection {
                    backend: profile.backend_ref(),
                    reason: reason.clone()
                }])
            );
        }
    }
}

#[test]
fn agent_backend_pool_missing_observations_and_rejections_keep_backend_identity() {
    let mut profiles = [profile("a", 1), profile("b", 1)];
    profiles[1].accepts_new_sessions = false;
    let mut foreign = observation(&profiles[0], 0);
    foreign.backend.provider = AgentProvider::Claude;
    for policy in [
        SelectionPolicy::WeightedLeastInflight,
        SelectionPolicy::Ordered,
    ] {
        assert_eq!(
            select(
                &profiles,
                std::slice::from_ref(&foreign),
                policy,
                SystemTime::UNIX_EPOCH
            ),
            Decision::NoEligibleBackend(vec![
                BackendRejection {
                    backend: profiles[0].backend_ref(),
                    reason: IneligibleReason::UnknownHealth
                },
                BackendRejection {
                    backend: profiles[1].backend_ref(),
                    reason: IneligibleReason::NotAcceptingNewSessions
                },
            ])
        );
        assert_eq!(
            select(&[], &[], policy, SystemTime::UNIX_EPOCH),
            Decision::NoEligibleBackend(vec![])
        );
    }
}

#[test]
fn agent_backend_pool_soft_limit_and_expired_cooldown_do_not_reserve() {
    let mut profile = profile("a", 1);
    profile.max_inflight = NonZeroU32::new(2);
    let mut observation = observation(&profile, 1);
    observation.signals.cooldown = Observed::Known(Some(Cooldown {
        until: SystemTime::UNIX_EPOCH,
        evidence: CooldownEvidence::QuotaExhausted,
        generation: 5,
    }));
    let before = observation.clone();
    for policy in [
        SelectionPolicy::WeightedLeastInflight,
        SelectionPolicy::Ordered,
    ] {
        // Repeated selection returns the same candidate: no capacity or half-open lease allocation.
        for _ in 0..2 {
            assert_eq!(
                select(
                    std::slice::from_ref(&profile),
                    std::slice::from_ref(&observation),
                    policy,
                    SystemTime::UNIX_EPOCH
                ),
                Decision::Candidate(profile.backend_ref())
            );
        }
        assert_eq!(observation, before);
    }
    for count in [2, 3] {
        observation.signals.inflight = Observed::Known(count);
        assert_eq!(
            select(
                std::slice::from_ref(&profile),
                std::slice::from_ref(&observation),
                SelectionPolicy::Ordered,
                SystemTime::UNIX_EPOCH
            ),
            Decision::NoEligibleBackend(vec![BackendRejection {
                backend: profile.backend_ref(),
                reason: IneligibleReason::SoftInflightLimit
            }])
        );
    }
    profile.max_inflight = None;
    assert_eq!(
        select(
            std::slice::from_ref(&profile),
            &[observation],
            SelectionPolicy::Ordered,
            SystemTime::UNIX_EPOCH
        ),
        Decision::Candidate(profile.backend_ref())
    );
}
