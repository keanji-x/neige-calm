//! Refusal and failure keys of `calm.task.replace`. The sentences live in
//! `prompts/task-replace/refusals.md`; Rust maps a refusal to its key and names its facts.

use crate::error::CalmError;

const SENTENCES: &str = include_str!("../../prompts/task-replace/refusals.md");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    StaleAttempt,
    PredecessorDispatching,
    PredecessorVerifying,
    CandidatePending,
    PredecessorChanged,
    AlreadyReplaced,
    IdempotencyConflict,
    PendingDependents,
    UnsupportedRoute,
    RequiresUserRelease,
    DerivedKeyTaken,
    DerivedKeyTooLong,
    TrackTerminal,
    PredecessorUndeclared,
    SuccessorUnschedulable,
    /// Kernel-side: a successor's dispatch found its task edited off the replaceable route.
    RouteChanged,
    /// Kernel-side: the carry merge conflicts with the upstream.
    CarryConflict,
    /// Kernel-side: the carry commit could not be computed.
    CarryInfra,
}

impl Refusal {
    #[cfg(test)]
    const ALL: [Self; 18] = [
        Self::StaleAttempt,
        Self::PredecessorDispatching,
        Self::PredecessorVerifying,
        Self::CandidatePending,
        Self::PredecessorChanged,
        Self::AlreadyReplaced,
        Self::IdempotencyConflict,
        Self::PendingDependents,
        Self::UnsupportedRoute,
        Self::RequiresUserRelease,
        Self::DerivedKeyTaken,
        Self::DerivedKeyTooLong,
        Self::TrackTerminal,
        Self::PredecessorUndeclared,
        Self::SuccessorUnschedulable,
        Self::RouteChanged,
        Self::CarryConflict,
        Self::CarryInfra,
    ];

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::StaleAttempt => "stale_attempt",
            Self::PredecessorDispatching => "predecessor_dispatching",
            Self::PredecessorVerifying => "predecessor_verifying",
            Self::CandidatePending => "candidate_pending",
            Self::PredecessorChanged => "predecessor_changed",
            Self::AlreadyReplaced => "already_replaced",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::PendingDependents => "pending_dependents",
            Self::UnsupportedRoute => "unsupported_route",
            Self::RequiresUserRelease => "requires_user_release",
            Self::DerivedKeyTaken => "derived_key_taken",
            Self::DerivedKeyTooLong => "derived_key_too_long",
            Self::TrackTerminal => "track_terminal",
            Self::PredecessorUndeclared => "predecessor_undeclared",
            Self::SuccessorUnschedulable => "successor_unschedulable",
            Self::RouteChanged => "replace-route-changed",
            Self::CarryConflict => "carry-conflict",
            Self::CarryInfra => "carry-infra",
        }
    }

    pub(crate) fn sentence(self) -> &'static str {
        SENTENCES
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('\t'))
            .find(|(key, _)| *key == self.key())
            .map_or(self.key(), |(_, sentence)| sentence)
    }

    /// `<key> (<facts>): <sentence>`, or `<key>: <sentence>` without facts.
    pub(crate) fn message(self, facts: &str) -> String {
        if facts.is_empty() {
            format!("{}: {}", self.key(), self.sentence())
        } else {
            format!("{} ({facts}): {}", self.key(), self.sentence())
        }
    }

    /// The Planner-facing refusal of a replace request (`-32409`).
    pub(crate) fn refuse(self, facts: &str) -> CalmError {
        CalmError::Conflict(self.message(facts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refusal_has_a_sentence() {
        for refusal in Refusal::ALL {
            assert_ne!(
                refusal.sentence(),
                refusal.key(),
                "{refusal:?} has no line in refusals.md"
            );
        }
    }
}
