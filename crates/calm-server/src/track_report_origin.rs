//! The write-origin vocabulary for track-report writes: who is writing, how the write is attributed,
//! and whether the recorder gate is consulted.

use calm_types::event::EditAuthor;
use calm_types::ids::{ActorId, CardId, TrackId};
use calm_types::model::CardRole;
use calm_types::runtime::AgentProvider;
use calm_types::worker::WorkerSessionId;

use crate::error::CalmError;

/// The MCP-agent identity behind an agent-channel report write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOrigin {
    pub card_id: CardId,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: WorkerSessionId,
    /// The *target* track of the write, not the principal's identity-resolved track, even though they are equal today.
    pub track_id: TrackId,
}

/// The actual initiator of a fork as the server derived it; only `ActorId::User` is admitted today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedInitiator(ActorId);

impl TrustedInitiator {
    /// Refuses any initiator the fork route cannot produce.
    pub fn new(actor: ActorId) -> Result<Self, CalmError> {
        if !matches!(actor, ActorId::User) {
            return Err(CalmError::Forbidden(format!(
                "fork initiator {actor:?} is not a shape the fork route can produce"
            )));
        }
        Ok(Self(actor))
    }

    pub fn actor(&self) -> &ActorId {
        &self.0
    }
}

/// Who is performing a track-report write. Deliberately no `KernelSeed` or `Plugin` variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOrigin {
    Agent(AgentOrigin),
    RestUser,
    /// Track creation copying a source track's report into the new track; the fork's `CardAdded` is
    /// attributed to the actual initiator.
    Fork(TrustedInitiator),
}

/// How a write is attributed in the edit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAttribution {
    /// The only shape that can supply the `author` argument of `guard_task_declarations`.
    Authored(EditAuthor),
    /// A structural copy with no author. Fork only.
    Structural,
}

/// Whether the recorder gate is consulted for this write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderRequirement {
    /// Consult the recorder gate for the origin's agent principal; a `Deny` fails the write.
    AgentGate,
    /// No recorder gate. For a fork this means the path never calls `write::persist` at all — it writes
    /// inside the track-creation transaction and emits no `TrackReportEdited`.
    NotGated,
}

/// The decisions a track-report write needs, in one value; [`policy_for`] is the only constructor, so a
/// caller cannot assemble an actor with someone else's attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritePolicy {
    actor: ActorId,
    attribution: WriteAttribution,
    auto_promote_draft: bool,
    recorder: RecorderRequirement,
}

impl WritePolicy {
    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    pub fn attribution(&self) -> WriteAttribution {
        self.attribution
    }

    pub fn auto_promote_draft(&self) -> bool {
        self.auto_promote_draft
    }

    pub fn recorder(&self) -> RecorderRequirement {
        self.recorder
    }
}

/// The declared policy for each origin. Exhaustive on purpose — a new [`WriteOrigin`] variant must state
/// its own policy rather than inherit another's.
pub fn policy_for(origin: &WriteOrigin) -> Result<WritePolicy, CalmError> {
    Ok(match origin {
        WriteOrigin::Agent(agent) => {
            let (author, auto_promote_draft) = match agent.role {
                CardRole::Planner => (EditAuthor::Planner, true),
                CardRole::Assistant => (EditAuthor::Assistant, false),
                role @ (CardRole::Worker | CardRole::ReportCard) => {
                    return Err(CalmError::Forbidden(format!(
                        "card role {role:?} may not write the track report"
                    )));
                }
            };
            WritePolicy {
                actor: agent_actor(agent),
                attribution: WriteAttribution::Authored(author),
                auto_promote_draft,
                recorder: RecorderRequirement::AgentGate,
            }
        }
        WriteOrigin::RestUser => WritePolicy {
            actor: ActorId::User,
            attribution: WriteAttribution::Authored(EditAuthor::User),
            auto_promote_draft: false,
            recorder: RecorderRequirement::NotGated,
        },
        WriteOrigin::Fork(initiator) => WritePolicy {
            actor: initiator.actor().clone(),
            attribution: WriteAttribution::Structural,
            auto_promote_draft: false,
            recorder: RecorderRequirement::NotGated,
        },
    })
}

/// Mirrors `ToolCallIdentity::to_actor_id`; `Worker` / `ReportCard` are refused by [`policy_for`] first.
fn agent_actor(agent: &AgentOrigin) -> ActorId {
    let session_id = agent.session_id.clone();
    match agent.role {
        CardRole::Planner => ActorId::AiPlannerSession(session_id),
        CardRole::Assistant | CardRole::Worker | CardRole::ReportCard => match agent.provider {
            AgentProvider::Codex => ActorId::AiCodexSession(session_id),
            AgentProvider::Claude => ActorId::AiClaudeSession(session_id),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(role: CardRole, provider: AgentProvider) -> WriteOrigin {
        WriteOrigin::Agent(AgentOrigin {
            card_id: CardId::from("c_1".to_string()),
            role,
            provider,
            session_id: WorkerSessionId::from("sess_1".to_string()),
            track_id: TrackId::from("w_1".to_string()),
        })
    }

    fn user_fork() -> TrustedInitiator {
        TrustedInitiator::new(ActorId::User).expect("a user is a reachable fork initiator")
    }

    #[test]
    fn policy_for_declares_the_intended_policy_table() {
        let cases: Vec<(
            &str,
            WriteOrigin,
            ActorId,
            WriteAttribution,
            bool,
            RecorderRequirement,
        )> = vec![
            (
                "agent/planner/codex",
                agent(CardRole::Planner, AgentProvider::Codex),
                ActorId::AiPlannerSession(WorkerSessionId::from("sess_1".to_string())),
                WriteAttribution::Authored(EditAuthor::Planner),
                true,
                RecorderRequirement::AgentGate,
            ),
            (
                "agent/planner/claude",
                agent(CardRole::Planner, AgentProvider::Claude),
                ActorId::AiPlannerSession(WorkerSessionId::from("sess_1".to_string())),
                WriteAttribution::Authored(EditAuthor::Planner),
                true,
                RecorderRequirement::AgentGate,
            ),
            (
                "agent/assistant/codex",
                agent(CardRole::Assistant, AgentProvider::Codex),
                ActorId::AiCodexSession(WorkerSessionId::from("sess_1".to_string())),
                WriteAttribution::Authored(EditAuthor::Assistant),
                false,
                RecorderRequirement::AgentGate,
            ),
            (
                "agent/assistant/claude",
                agent(CardRole::Assistant, AgentProvider::Claude),
                ActorId::AiClaudeSession(WorkerSessionId::from("sess_1".to_string())),
                WriteAttribution::Authored(EditAuthor::Assistant),
                false,
                RecorderRequirement::AgentGate,
            ),
            (
                "rest user",
                WriteOrigin::RestUser,
                ActorId::User,
                WriteAttribution::Authored(EditAuthor::User),
                false,
                RecorderRequirement::NotGated,
            ),
            (
                "fork by user",
                WriteOrigin::Fork(user_fork()),
                ActorId::User,
                WriteAttribution::Structural,
                false,
                RecorderRequirement::NotGated,
            ),
        ];

        for (name, origin, actor, attribution, auto_promote_draft, recorder) in cases {
            let policy = policy_for(&origin).unwrap_or_else(|error| {
                panic!("{name}: policy_for returned an error: {error:?}");
            });
            assert_eq!(policy.actor(), &actor, "{name}: actor");
            assert_eq!(policy.attribution(), attribution, "{name}: attribution");
            assert_eq!(
                policy.auto_promote_draft(),
                auto_promote_draft,
                "{name}: auto_promote_draft"
            );
            assert_eq!(policy.recorder(), recorder, "{name}: recorder");
        }
    }

    #[test]
    fn policy_for_refuses_the_two_agent_roles_that_may_not_write_the_report() {
        for role in [CardRole::Worker, CardRole::ReportCard] {
            let error = policy_for(&agent(role, AgentProvider::Codex))
                .expect_err("worker/report-card must be refused");
            assert!(
                matches!(error, CalmError::Forbidden(_)),
                "{role:?}: expected Forbidden, got {error:?}"
            );
        }
    }

    // No `fork_carries_the_initiator_through` test: with only `ActorId::User` admitted, it could not
    // distinguish pass-through from a hardcoded `ActorId::User`. Add it when the admitted set widens.

    /// Bump this alongside a new arm in [`actor_variant_label`].
    const ACTOR_ID_NON_USER_VARIANTS: usize = 9;

    /// No `_` arm: adding an `ActorId` variant fails to compile here.
    fn actor_variant_label(actor: &ActorId) -> &'static str {
        match actor {
            ActorId::User => "User",
            ActorId::Kernel => "Kernel",
            ActorId::KernelDispatcher => "KernelDispatcher",
            ActorId::Plugin(_) => "Plugin",
            ActorId::AiPlanner(_) => "AiPlanner",
            ActorId::AiCodex(_) => "AiCodex",
            ActorId::AiClaude(_) => "AiClaude",
            ActorId::AiPlannerSession(_) => "AiPlannerSession",
            ActorId::AiCodexSession(_) => "AiCodexSession",
            ActorId::AiClaudeSession(_) => "AiClaudeSession",
        }
    }

    /// One value for every non-`User` `ActorId` variant; checked against [`ACTOR_ID_NON_USER_VARIANTS`].
    fn every_non_user_actor() -> Vec<ActorId> {
        let samples = vec![
            ActorId::Kernel,
            ActorId::KernelDispatcher,
            ActorId::Plugin("git-forge".to_string()),
            ActorId::AiPlanner(CardId::from("c_2".to_string())),
            ActorId::AiCodex(CardId::from("c_2".to_string())),
            ActorId::AiClaude(CardId::from("c_2".to_string())),
            ActorId::AiPlannerSession(WorkerSessionId::from("sess_fork".to_string())),
            ActorId::AiCodexSession(WorkerSessionId::from("sess_fork".to_string())),
            ActorId::AiClaudeSession(WorkerSessionId::from("sess_fork".to_string())),
        ];
        let mut labels: Vec<&'static str> = samples.iter().map(actor_variant_label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(
            labels.len(),
            ACTOR_ID_NON_USER_VARIANTS,
            "one sample per non-`User` `ActorId` variant is required; got {labels:?}"
        );
        samples
    }

    /// Also refuses the empty-card-id `AiCodex` that `Actor::to_actor_id` emits for the legacy `ai:codex` header.
    #[test]
    fn a_trusted_initiator_refuses_every_shape_the_fork_route_cannot_produce() {
        let mut refused = every_non_user_actor();
        refused.push(ActorId::AiCodex(CardId::from(String::new())));
        for actor in refused {
            let error = TrustedInitiator::new(actor.clone())
                .err()
                .unwrap_or_else(|| panic!("{actor:?} must not be accepted as a fork initiator"));
            assert!(
                matches!(error, CalmError::Forbidden(_)),
                "{actor:?}: expected Forbidden, got {error:?}"
            );
        }
        assert_eq!(
            TrustedInitiator::new(ActorId::User)
                .expect("a user forks")
                .actor(),
            &ActorId::User
        );
    }

    /// Structural attribution carries no `EditAuthor` for `guard_task_declarations` to judge; fork only.
    #[test]
    fn structural_attribution_belongs_to_the_fork_origin_alone() {
        let origins = [
            agent(CardRole::Planner, AgentProvider::Codex),
            agent(CardRole::Assistant, AgentProvider::Codex),
            WriteOrigin::RestUser,
        ];
        for origin in origins {
            let policy = policy_for(&origin).unwrap();
            assert!(
                matches!(policy.attribution(), WriteAttribution::Authored(_)),
                "{origin:?} must be authored, not structural"
            );
        }
    }

    #[test]
    fn only_the_agent_origin_declares_a_recorder_gate() {
        assert_eq!(
            policy_for(&agent(CardRole::Planner, AgentProvider::Codex))
                .unwrap()
                .recorder(),
            RecorderRequirement::AgentGate
        );
        for origin in [WriteOrigin::RestUser, WriteOrigin::Fork(user_fork())] {
            assert_eq!(
                policy_for(&origin).unwrap().recorder(),
                RecorderRequirement::NotGated,
                "{origin:?}"
            );
        }
    }
}
