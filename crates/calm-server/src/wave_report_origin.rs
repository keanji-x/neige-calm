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
//! call sites, and it locks nothing but that declaration. Nothing in it was
//! observed from a running system, and no test here proves that `policy_for`
//! reproduces what production does today.
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
//! # Why there is no `Plugin` origin
//!
//! Plugin attribution does not travel through `EditAuthor` alone.
//! `EditAuthor::Plugin` is a deliberate unit variant; the plugin's actual id
//! rides in the sibling field `WaveReportEdited::author_plugin_id`, which the
//! event's own docs describe as `Some` exactly when `author == Plugin` and
//! `None` for every other author.
//!
//! [`WritePolicy`] has no field that can carry that id — it names an actor, an
//! attribution, an auto-promote verdict and a recorder requirement, and none of
//! the four is a plugin id — and `wave_report::persist_report_with_shadow`
//! writes `author_plugin_id: None` unconditionally, with no parameter to
//! override it. A plugin origin threaded through this module would therefore
//! emit `{author: plugin, author_plugin_id: None}`: the one combination that
//! field says never occurs. So the origin is not merely unreachable today, it
//! is the wrong shape for the value it would have to carry.
//!
//! Declaring it anyway has a concrete cost, and it is the cost that decided
//! this. The day a real plugin write path is added, an already-present variant
//! lets it compile without touching [`policy_for`] — the exhaustiveness error
//! that should force a redesign never fires, and the impossible pair ships.
//! This module's own rule ("an exemption that can be expressed will eventually
//! be used") applies to the plugin case unchanged: a doc comment does not stop
//! it, a missing variant does.
//!
//! Adding a plugin write path later therefore means doing both halves in one
//! change: (a) add the [`WriteOrigin`] variant, so the compiler forces every
//! match here to state its policy explicitly, and (b) derive
//! `author_plugin_id` from that origin and plumb it through to
//! `persist_report_with_shadow`, so the emitted pair is consistent.
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

/// The MCP-agent identity behind an agent-channel report write.
///
/// Field types follow `mcp_server::registry::ToolCallIdentity`, the struct
/// every agent write is derived from today, but typed rather than stringly:
/// `ToolCallIdentity` stores `card_id`/`session_id` as `String` and `wave_id`
/// as `Option<String>`.
///
/// # `wave_id` is required, and it is the *target* wave
///
/// `wave_id` is **required** here, unlike on `ToolCallIdentity`. A report write
/// is wave-scoped by construction, and the recorder gate already refuses an
/// agent principal without a wave — `ToolCallIdentity::to_principal` returns
/// `None`, and `CardDecisionSinkRecorderShadowProbe::record` turns that `None`
/// into `Forbidden`.
///
/// Making the field required makes "agent write with no wave" *unrepresentable
/// in this type*, which is not the same as performing the refusal. **No code in
/// this module refuses anything on this account** — the fields are public and
/// there is no constructor. What the required field buys is that step 2's
/// constructor cannot build an `AgentOrigin` without first resolving the
/// `Option<String>`; producing the `Forbidden` for a `None` is that caller's
/// job, because only the caller holds the request context the error names.
///
/// The wave this carries is the **wave being written**, as resolved for the
/// call — `mcp_server::tools::wave_report::resolve_report_for_caller` reads it
/// from the bound spec card's `wave_id`. It is *not* interchangeable with the
/// wave that identity resolution attaches to the principal
/// (`card_identity_get_by_session`, `transport.rs`), even though both read the
/// same column today and are therefore equal. `decide_recorder` compares the
/// card's own wave against the write target
/// (`card_wave != wave`, `calm-truth/src/decision_gate.rs`), so the two are
/// opposite sides of one comparison. When step 2 threads this through, it must
/// keep supplying the recorder probe's target wave from *this* field and let
/// the principal's wave come from identity resolution; collapsing them into a
/// single value would make that comparison compare a value with itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOrigin {
    pub card_id: CardId,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: WorkerSessionId,
    /// The wave whose report is being written. See the type docs: not the
    /// principal's identity-resolved wave, even though they are equal today.
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
///
/// # Why the constructor is fallible
///
/// [`TrustedInitiator::new`] accepts `ActorId::User` and nothing else, because
/// that is the whole of what the fork route can produce. `Actor::to_actor_id`
/// (`actor.rs`) yields exactly two shapes: `User`, and `AiCodex(CardId(""))`
/// for the legacy `ai:codex` header — every other header value falls through
/// its documented defensive default to `User`. The second shape cannot complete
/// a fork either: the empty card id is rejected as
/// `RoleViolation::EmptyAiCardId` when the wave-create events emit
/// (`calm-truth/src/role_gate.rs`), so under `ai:codex` the route 403s rather
/// than reaching a report copy.
///
/// A constructor over the whole `ActorId` enum would enforce nothing its name
/// claims, and the origin it feeds is the one that matters most: `Fork` is the
/// sole [`WriteAttribution::Structural`] source, the shape that goes down the
/// fork belt *past* `guard_task_declarations`. Leaving the door open would let
/// a later caller hand an agent identity to the one write that skips the author
/// guard, silently and with no compile-time speed bump — the same failure mode
/// the missing `Plugin` variant is there to prevent. Widening this is a
/// deliberate one-line edit at a place that says why, which is the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedInitiator(ActorId);

impl TrustedInitiator {
    /// Refuses any initiator the fork route cannot produce. See the type docs
    /// for why the reachable set is `{ActorId::User}` today.
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

/// Who is performing a wave-report write.
///
/// There is deliberately **no `KernelSeed` variant**: kernel template seeding
/// (`routes::waves::seed_template_wave` /
/// `restamp_template_report_if_placeholder`) is being removed, and its removal
/// is the blocking prerequisite for S1 step 2.
///
/// There is deliberately **no `Plugin` variant** either — see the module docs
/// for why the shape is wrong and what adding one later has to include.
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
    ///
    /// Note what that does and does not buy today. [`TrustedInitiator`] admits
    /// one shape (`ActorId::User`), so "pass the initiator through" and "return
    /// the constant `ActorId::User`" produce identical output for every input,
    /// and **no test can tell them apart**. The payload is kept because the
    /// initiator is a real input of the fork's `CardAdded` attribution, so a
    /// later widening of the admitted set lands as one edit in
    /// `TrustedInitiator::new` and needs no change in [`policy_for`].
    Fork(TrustedInitiator),
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
/// So this enum has two variants, not three.
///
/// # Why the probe payload is not carried here
///
/// Not because it can be reconstructed from [`AgentOrigin`] — it cannot. The
/// probe's `principal` is a `Principal::Agent`, which needs a `cove_id`
/// (`calm-types/src/worker.rs`) that `AgentOrigin` does not have; production
/// builds it in `ToolCallIdentity::to_principal` from `identity.cove_id`.
///
/// The reason is narrower and true: what the gate actually reads of the
/// principal is its `session_id` and nothing else — `decide_recorder`
/// destructures `Principal::Agent { session_id, .. }` and takes the target wave
/// as a separate argument (`calm-truth/src/decision_gate.rs`). So an origin
/// carrying `session_id` plus the write's target wave determines the same gate
/// outcome as today for every input, and leaving the assembled probe out of
/// this type avoids a second place where the principal could drift away from
/// the origin. If a future gate reads `cove_id`, this stops holding and the
/// origin has to grow the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderRequirement {
    /// Consult the recorder gate for the origin's agent principal; a `Deny`
    /// fails the write.
    AgentGate,
    /// No recorder gate on this write.
    ///
    /// For the REST-user sites this is a statement about a real
    /// `persist_report_with_shadow` call that passes `None`. For
    /// [`WriteOrigin::Fork`] it is not: **the fork path never calls
    /// `persist_report_with_shadow` at all.** It writes the copied report
    /// inside the wave-creation transaction via `card_update_with_crdt_tx`
    /// (`routes::waves::persist_fork_report_and_project_tasks_tx`) and emits no
    /// `WaveReportEdited`. `NotGated` therefore records that a fork is subject
    /// to no recorder decision — not that some fork call site was observed
    /// passing `None`.
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

    fn user_fork() -> TrustedInitiator {
        TrustedInitiator::new(ActorId::User).expect("a user is a reachable fork initiator")
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

    // There is deliberately no `fork_carries_the_initiator_through` test. With
    // `TrustedInitiator` admitting only `ActorId::User`, "carries the initiator
    // through" and "hardcodes `ActorId::User`" have the same output on every
    // constructible input, so such a test would assert nothing beyond the
    // `fork by user` row of the table above. See the `WriteOrigin::Fork` docs.

    /// `TrustedInitiator` is the fork's author-guard bypass in type form, so it
    /// admits only the initiator shape the fork route can actually produce.
    /// The refused shapes below are the ones a future caller would most
    /// plausibly reach for.
    #[test]
    fn a_trusted_initiator_refuses_every_shape_the_fork_route_cannot_produce() {
        let refused = [
            ActorId::AiCodex(CardId::from("c_2".to_string())),
            ActorId::AiCodex(CardId::from(String::new())),
            ActorId::AiClaudeSession(WorkerSessionId::from("sess_fork".to_string())),
            ActorId::AiSpecSession(WorkerSessionId::from("sess_fork".to_string())),
            ActorId::Kernel,
            ActorId::Plugin("git-forge".to_string()),
        ];
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

    /// Structural attribution must not be reachable for anything but a fork:
    /// it is the shape that skips `guard_task_declarations`.
    #[test]
    fn structural_attribution_belongs_to_the_fork_origin_alone() {
        let origins = [
            agent(CardRole::Spec, AgentProvider::Codex),
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
        for origin in [WriteOrigin::RestUser, WriteOrigin::Fork(user_fork())] {
            assert_eq!(
                policy_for(&origin).unwrap().recorder(),
                RecorderRequirement::NotGated,
                "{origin:?}"
            );
        }
    }
}
