//! #1252 S1 step 1 — the write-origin vocabulary for wave-report writes.
//!
//! Today every caller of `wave_report::persist_report_with_shadow` hands it a
//! hand-assembled quadruple: `(actor, author, auto_promote_draft,
//! recorder_shadow)`. Four independent arguments, four chances to get one
//! wrong, and no place where the whole set is stated per caller. This module
//! introduces the type that names *who is writing* ([`WriteOrigin`]) and the
//! total function that turns it into the quadruple ([`policy_for`]).
//!
//! # Status: not wired into production
//!
//! Nothing in this module is called from a production path yet. S1 step 2
//! threads it through the persist boundary; step 2 is blocked on the removal
//! of kernel template seeding (hence no `KernelSeed` origin here).
//!
//! # What the policy table in this module's tests is, and is not
//!
//! The table asserted by the tests below is the **declared intent** for each
//! origin: it is written from the design ruling and from a reading of today's
//! call sites, and it locks nothing but that declaration. It is **not** a
//! captured baseline of production behaviour, and no test here proves that
//! `policy_for` reproduces what production does today.
//!
//! The behavioural-equivalence proof happens in **S1 step 2**, when
//! `policy_for`'s output is compared against the quadruples the existing
//! production call sites actually pass. Until that comparison exists and is
//! green, treat this area as *unproven*.
//!
//! # Why attribution is two shapes, not `Option<EditAuthor>`
//!
//! `routes/waves.rs` carries an explicit #1115 contract: the fork path
//! **deliberately derives no `EditAuthor`**. It used to (`User` with no
//! `X-Calm-Actor` header, `Spec` otherwise), and handing that author to
//! `routes::waves::fork_guard::guard_forked_blocks` made the guard a no-op for
//! the browser fork — the most common fork there is. `fork_guard` locks the
//! same rule from the other side: the exemption "cannot be reintroduced
//! without re-plumbing it through `prepare_fork_report`".
//!
//! An `Option<EditAuthor>` would reopen exactly that hole, because "no author"
//! and "some author" would be the same type and a future caller could pass
//! either. [`WriteAttribution`] is therefore two shapes:
//! `guard_task_declarations` takes only [`WriteAttribution::Authored`], and
//! [`WriteAttribution::Structural`] goes down the fork belt *by type*.
//!
//! # Why there are no guard/CAS booleans here
//!
//! The three report guards are unconditional control flow inside
//! `wave_report::apply_report_op` today. Modelling them as booleans would
//! reduce "turn a guard off" to writing `false` — and an exemption that can be
//! expressed will eventually be used. Per-origin differences in CAS input
//! belong on the *constructor signatures* of step 2 (the fork constructor
//! simply has no `expected_rev` parameter), not in a runtime flag.

use calm_types::event::EditAuthor;
use calm_types::ids::{ActorId, CardId, WaveId};
use calm_types::model::CardRole;
use calm_types::runtime::AgentProvider;
use calm_types::worker::WorkerSessionId;

use crate::error::CalmError;

/// A plugin's identity. `calm-types` has no `PluginId` newtype today —
/// `ActorId::Plugin` carries a bare `String` — so this is the narrowest thing
/// that keeps [`WriteOrigin::Plugin`] from being "a string".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The MCP-agent identity behind an agent-channel report write.
///
/// Field types follow `mcp_server::registry::ToolCallIdentity`, the struct
/// every agent write is derived from today, but typed rather than stringly:
/// `ToolCallIdentity` stores `card_id`/`session_id` as `String` and `wave_id`
/// as `Option<String>`.
///
/// `wave_id` is **required** here, unlike on `ToolCallIdentity`. A report
/// write is wave-scoped by construction, and the recorder gate already refuses
/// (`Forbidden`) an agent principal without a wave — see
/// `ToolCallIdentity::to_principal` returning `None` and
/// `CardDecisionSinkRecorderShadowProbe::record` rejecting that `None`. Making
/// it required moves that refusal to the point where the origin is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOrigin {
    pub card_id: CardId,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: WorkerSessionId,
    pub wave_id: WaveId,
}

/// The actual initiator of a fork, as the server derived it — never a
/// client-claimed string.
///
/// Today `routes::waves::create_wave_structure` attributes the fork's
/// `CardAdded` events to `actor.to_actor_id()`, i.e. the `ActorId` the `Actor`
/// extractor produced from the (validated) `X-Calm-Actor` header. That is the
/// value this carries: fork emits no `WaveReportEdited`, so the initiator is
/// the *only* place the fork's "who" survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedInitiator(ActorId);

impl TrustedInitiator {
    pub fn new(actor: ActorId) -> Self {
        Self(actor)
    }

    pub fn actor(&self) -> &ActorId {
        &self.0
    }
}

/// Who is performing a wave-report write.
///
/// There is deliberately **no `KernelSeed` variant**: kernel template seeding
/// (`routes::waves::seed_template_wave` /
/// `restamp_template_report_if_placeholder`) is being removed, and its removal
/// is the blocking prerequisite for S1 step 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOrigin {
    /// An MCP agent writing through `decision_sink::CardDecisionSink`.
    Agent(AgentOrigin),
    /// The browser / REST user surface.
    RestUser,
    /// Wave creation copying a source wave's report into the new wave.
    ///
    /// Data-carrying, not a unit variant: the fork's `CardAdded` must be
    /// attributed to the actual initiator.
    Fork(TrustedInitiator),
    /// A plugin-authored write.
    Plugin(PluginId),
}

/// How a write is attributed in the edit log.
///
/// Two shapes on purpose — see the module docs. Not `EditAuthor`, and not
/// `Option<EditAuthor>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAttribution {
    /// The write has an author, and `guard_task_declarations` applies.
    Authored(EditAuthor),
    /// The write is a structural copy with no author. Fork only.
    Structural,
}

/// Whether the recorder gate is consulted for this write.
///
/// Shaped to today's observed variation and no wider. Across every production
/// call site of `persist_report_with_shadow`, the `recorder_shadow` argument
/// takes exactly two values:
///
/// * `Some(CardDecisionSinkRecorderShadowProbe { principal, wave_id })` — the
///   one agent funnel, `decision_sink::CardDecisionSink::commit_report_op`;
/// * `None` — every REST-user site (`routes::wave_report_blocks::commit`,
///   `routes::wave_templates`, `routes::waves::update_wave_report`) and the
///   kernel template-seeding sites.
///
/// So this enum has two variants, not three. The probe's payload is not
/// carried here: it is derivable from [`AgentOrigin`]'s `session_id` /
/// `wave_id`, and duplicating it would create a second place where the
/// principal could disagree with the origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderRequirement {
    /// Consult the recorder gate for the origin's agent principal; a `Deny`
    /// fails the write.
    AgentGate,
    /// No recorder gate on this write.
    NotGated,
}

/// The decisions a wave-report write needs, all of them, in one value.
///
/// Fields are private and there is no public constructor: [`policy_for`] is
/// the only way to obtain one. That is the point — a caller must not be able
/// to assemble an actor with someone else's attribution.
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

/// The declared policy for each origin.
///
/// Exhaustive on purpose — **no `_` arm**. A new [`WriteOrigin`] variant must
/// fail to compile here until someone states its actor, its attribution, its
/// auto-promote verdict and its recorder requirement, rather than silently
/// inheriting another origin's.
///
/// The only error today mirrors `decision_sink::report_op_attribution`: a
/// `Worker` or `ReportCard` agent may not write the wave report.
///
/// Again: what this function returns is the *declared* policy. It is not
/// asserted anywhere yet to equal what production computes; that comparison is
/// S1 step 2's job.
pub fn policy_for(origin: &WriteOrigin) -> Result<WritePolicy, CalmError> {
    Ok(match origin {
        WriteOrigin::Agent(agent) => {
            let (author, auto_promote_draft) = match agent.role {
                CardRole::Spec => (EditAuthor::Spec, true),
                CardRole::Assistant => (EditAuthor::Assistant, false),
                role @ (CardRole::Worker | CardRole::ReportCard) => {
                    return Err(CalmError::Forbidden(format!(
                        "card role {role:?} may not write the wave report"
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
        WriteOrigin::Plugin(plugin_id) => WritePolicy {
            actor: ActorId::Plugin(plugin_id.as_str().to_string()),
            attribution: WriteAttribution::Authored(EditAuthor::Plugin),
            auto_promote_draft: false,
            recorder: RecorderRequirement::NotGated,
        },
    })
}

/// Mirrors `ToolCallIdentity::to_actor_id`: MCP writes are keyed by worker
/// session, and the spec role gets its own actor rather than a provider one.
/// `Worker` / `ReportCard` never reach here — [`policy_for`] refuses them
/// first — so this function only has to cover the two roles that may write.
fn agent_actor(agent: &AgentOrigin) -> ActorId {
    let session_id = agent.session_id.clone();
    match agent.role {
        CardRole::Spec => ActorId::AiSpecSession(session_id),
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
            wave_id: WaveId::from("w_1".to_string()),
        })
    }

    /// The declared intent for each origin, stated once. This asserts what
    /// this module *says*; it does not compare against production, which is
    /// what S1 step 2 will do.
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
                "agent/spec/codex",
                agent(CardRole::Spec, AgentProvider::Codex),
                ActorId::AiSpecSession(WorkerSessionId::from("sess_1".to_string())),
                WriteAttribution::Authored(EditAuthor::Spec),
                true,
                RecorderRequirement::AgentGate,
            ),
            (
                "agent/spec/claude",
                agent(CardRole::Spec, AgentProvider::Claude),
                ActorId::AiSpecSession(WorkerSessionId::from("sess_1".to_string())),
                WriteAttribution::Authored(EditAuthor::Spec),
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
                WriteOrigin::Fork(TrustedInitiator::new(ActorId::User)),
                ActorId::User,
                WriteAttribution::Structural,
                false,
                RecorderRequirement::NotGated,
            ),
            (
                "fork by agent",
                WriteOrigin::Fork(TrustedInitiator::new(ActorId::AiCodex(CardId::from(
                    "c_2".to_string(),
                )))),
                ActorId::AiCodex(CardId::from("c_2".to_string())),
                WriteAttribution::Structural,
                false,
                RecorderRequirement::NotGated,
            ),
            (
                "plugin",
                WriteOrigin::Plugin(PluginId::new("git-forge")),
                ActorId::Plugin("git-forge".to_string()),
                WriteAttribution::Authored(EditAuthor::Plugin),
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

    /// Same refusal `decision_sink::report_op_attribution` already makes: these
    /// two roles are rejected outright rather than folded in with `Spec`.
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

    /// The fork is the one origin whose actor is not a constant: it must be
    /// the initiator that was handed in, because the fork's `CardAdded` is the
    /// only record of who forked.
    #[test]
    fn fork_carries_the_initiator_through_rather_than_flattening_it() {
        let initiator = ActorId::AiClaudeSession(WorkerSessionId::from("sess_fork".to_string()));
        let policy =
            policy_for(&WriteOrigin::Fork(TrustedInitiator::new(initiator.clone()))).unwrap();
        assert_eq!(policy.actor(), &initiator);
    }

    /// Structural attribution must not be reachable for anything but a fork:
    /// it is the shape that skips `guard_task_declarations`.
    #[test]
    fn structural_attribution_belongs_to_the_fork_origin_alone() {
        let origins = [
            agent(CardRole::Spec, AgentProvider::Codex),
            agent(CardRole::Assistant, AgentProvider::Codex),
            WriteOrigin::RestUser,
            WriteOrigin::Plugin(PluginId::new("git-forge")),
        ];
        for origin in origins {
            let policy = policy_for(&origin).unwrap();
            assert!(
                matches!(policy.attribution(), WriteAttribution::Authored(_)),
                "{origin:?} must be authored, not structural"
            );
        }
    }

    /// The recorder gate is the agent funnel's, and only the agent funnel's —
    /// which is the whole of today's variation at the production call sites.
    #[test]
    fn only_the_agent_origin_declares_a_recorder_gate() {
        assert_eq!(
            policy_for(&agent(CardRole::Spec, AgentProvider::Codex))
                .unwrap()
                .recorder(),
            RecorderRequirement::AgentGate
        );
        for origin in [
            WriteOrigin::RestUser,
            WriteOrigin::Fork(TrustedInitiator::new(ActorId::User)),
            WriteOrigin::Plugin(PluginId::new("git-forge")),
        ] {
            assert_eq!(
                policy_for(&origin).unwrap().recorder(),
                RecorderRequirement::NotGated,
                "{origin:?}"
            );
        }
    }
}
