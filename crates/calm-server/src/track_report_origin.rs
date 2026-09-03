//! #1252 S1 step 1 — the write-origin vocabulary for track-report writes.
//!
//! Today `track_report::persist_report_with_shadow` takes a hand-assembled
//! quadruple — `(actor, author, auto_promote_draft, recorder_shadow)` — and
//! **three** production sites assemble it, in two layers. Two call it directly
//! and pass all four: `decision_sink::CardDecisionSink::commit_report_op` and
//! `routes::track_report_blocks::commit`. One goes through the
//! `track_report::persist_report` wrapper and passes the first three, with the
//! wrapper's hardcoded `recorder_shadow: None` supplied on its behalf:
//! `routes::tracks::update_track_report`. Four independent arguments, four
//! chances to get one wrong, and no place where the whole set is stated per
//! caller. This module
//! introduces the type that names *who is writing* ([`WriteOrigin`]) and the
//! total function that turns it into the quadruple ([`policy_for`]).
//!
//! **#1300 brought this count from six to three.** S1 deleted a direct caller,
//! `routes::track_templates::update_track_template`, with the template editor.
//! S2 deleted the two kernel seeding sites, `seed_template_track` and
//! `restamp_template_report_if_placeholder`, by making template instantiation
//! structural initialization inside the create transaction (see
//! `routes::tracks::prepare_template_report`). What survives is exactly two
//! `RestUser` sites and one `Agent` site — every one of them with an honest
//! origin, which is the precondition S1 step 2 was blocked on.
//!
//! What backs each half of that: the *behaviour* of all three — which actor and
//! which `EditAuthor` each one persists — is asserted by
//! `tests/cases/report_write_characterization.rs`, which drives them through
//! the real router and tool registry. That there are three and not four is a
//! weaker claim: the `persist_report_call_sites` CI ratchet is a text census of
//! per-file occurrences, so it catches a call site added by someone who did not
//! know about it, and does not claim to catch one hidden on purpose. That
//! script's "KNOWN GAPS" section enumerates what it misses.
//!
//! # Status: wired into all three production call sites, in parallel
//!
//! S1 step 2 landed: each of the three call sites listed above now builds a
//! [`WriteOrigin`] from its own request context and calls
//! [`verify_legacy_write_arguments`] before it writes. Nothing was deleted —
//! `persist_report_with_shadow` still takes all four members as separate
//! parameters, and the call sites still derive three of them the old way. What
//! the check adds is in two parts:
//!
//! * `actor`, `author` and `auto_promote_draft` are **compared**: [`policy_for`]'s
//!   answer must equal what the call site derived, or the write is refused.
//! * the recorder probe is **supplied**: the check builds it from the origin and
//!   returns it, and [`LegacyWriteArguments`] is the only way the call site can
//!   get one.
//!
//! Per-field deletion is step 3. The reason the fourth member is supplied
//! rather than compared is written up on [`LegacyWriteArguments`]: comparing a
//! `bool` read off a *binding* left the argument position itself unguarded, and
//! a review mutation walked straight through it.
//!
//! **What this does not achieve.** A call site can still diverge from the
//! checked values after it receives them — `into_parts` yields four ordinary
//! bindings, and reassigning one before the persist call compiles and is
//! production-reachable. [`LegacyWriteArguments`] documents the exact witness
//! and the measurement. Single-path-carries-policy means moving the check
//! inside `persist_report_with_shadow`; that is owed to step 3 and blocked by
//! the fixture population in "What step 3 is owed" below.
//!
//! The origins are built from real request context, never from the quadruple:
//! the MCP site builds [`AgentOrigin`] out of `ToolCallIdentity` plus the
//! target track, and both REST sites are [`WriteOrigin::RestUser`] because they
//! are the REST user surface (both are gated to `X-Calm-Actor: user` by
//! `routes::track_report_blocks::require_rest_user_actor`). Deriving the origin
//! from `author`/`auto_promote_draft`/`recorder_shadow` would make the check
//! compare a value with itself.
//!
//! [`WriteOrigin::Fork`] is still unwired: the fork copies the report inside
//! the track-creation transaction (`persist_fork_report_and_project_tasks_tx`)
//! and never reaches this boundary, so there is no call site to check it at.
//!
//! # What step 3 is owed, and what blocks it
//!
//! The real convergence is not this check — it is moving the decision *inside*
//! `track_report::persist_report_with_shadow`, so that a caller hands over a
//! [`WriteOrigin`] and the boundary derives the members itself. Then there is
//! one derivation instead of two, and nothing to compare or to keep in step.
//! Both review channels agree that is where this lands, and that it could not
//! land here. It is **owed to step 3**, and this is the obstacle.
//!
//! `persist_report_with_shadow` is `pub(crate)`, so nothing outside the crate
//! calls it; but its `pub` wrapper `track_report::persist_report` has a large
//! non-production population, and every one of them would need an origin.
//! Enumerated at the time of writing — 19 non-production call sites, 17 in
//! `crates/calm-server/tests/` and 2 in `#[cfg(test)]` modules under `src/`:
//!
//! | (actor, author) passed | sites | representable? |
//! |---|---|---|
//! | `(User, User)` | 9 | yes — [`WriteOrigin::RestUser`] |
//! | `(Kernel, Kernel)` | 7 | **no** |
//! | `(Kernel, Spec)` | 4 | **no** |
//!
//! (The counts sum to 20 over 19 sites because the `report_backlinks.rs` site
//! takes its `author` as a parameter and its two callers pass
//! `EditAuthor::Kernel` and `EditAuthor::Spec`, so that one site appears in
//! both `Kernel` rows. Ten of the 19 pass `ActorId::Kernel`.)
//!
//! The two `Kernel` rows are the blocker, and they are unrepresentable *by
//! design*: #1300 removed kernel template seeding, which is why there is no
//! `KernelSeed` variant (see [`WriteOrigin`]) — and `(Kernel, Spec)` never had
//! an origin to begin with, since it pairs the kernel's actor with the spec's
//! attribution. So step 3 cannot simply thread an origin through these; it has
//! to first decide, per fixture, whether the fixture is standing in for a real
//! origin (and should be rewritten to use it) or is seeding rows directly
//! (and should stop going through the persist boundary at all). Adding a
//! `Kernel` origin to make the fixtures compile would put back exactly the
//! variant #1300 deleted, and [`policy_for`]'s exhaustiveness would stop
//! protecting anything.
//!
//! Sites, for whoever picks step 3 up — **by file and fixture, not by line
//! number**, because the #1316 renames moved every one of them and the next
//! rename will move them again. Re-derive with
//! `rg -n 'persist_report\(' crates/`, then read the fourth and fifth
//! arguments:
//!
//! * `(User, User)` — `tests/cases/mcp_assistant_report_channel.rs`,
//!   `tests/cases/track_report_fork.rs`, `tests/cases/track_template_tracks.rs`
//!   (six sites), `tests/scheduler.rs`.
//! * `(Kernel, Kernel)` — `tests/cases/track_vcs.rs` (two sites),
//!   `tests/cases/task_projection_acceptance.rs`,
//!   `tests/cases/rest_track_report.rs` (the fork-source seed),
//!   `tests/cases/mcp_report_links.rs`, and under `src/`,
//!   `track_report_read.rs`'s `assert_first_write_preserves_read_ids` fixture
//!   plus `report_backlinks.rs`'s `report_as` helper as called by `report`.
//! * `(Kernel, Spec)` — `tests/cases/mcp_assistant_tool_gate.rs`,
//!   `tests/cases/rest_track_report.rs` (the spec-authored seed),
//!   `tests/cases/track_projection_policy_patch.rs`, and `report_backlinks.rs`'s
//!   `report_as` at its `EditAuthor::Spec` caller.
//!
//! # What the policy table in this module's tests is, and is not
//!
//! The table asserted by the tests below is the **declared intent** for each
//! origin, and it locks nothing but that declaration. Every line of it comes
//! from the design ruling and from a reading of today's call sites; not one
//! line comes from observing a running system. No test *in this module*
//! compares `policy_for`'s output against what production computes.
//!
//! That comparison is [`verify_legacy_write_arguments`], and since S1 step 2 it
//! runs in production on every track-report write that reaches one of the three
//! call sites — a disagreement refuses the write. The rows it exercises are
//! only the reachable ones: `Agent` for `Spec` and `Assistant`, and `RestUser`.
//! `Fork`, and the `Worker`/`ReportCard` refusal, are still asserted by
//! declaration alone.
//!
//! # Why attribution is two shapes, not `Option<EditAuthor>`
//!
//! `routes/tracks.rs` carries an explicit #1115 contract: the fork path
//! **deliberately derives no `EditAuthor`**. It used to (`User` with no
//! `X-Calm-Actor` header, `Spec` otherwise), and handing that author to
//! `routes::tracks::fork_guard::guard_forked_blocks` made the guard a no-op for
//! the browser fork, because a browser fork sends no header. `fork_guard`
//! states both halves of that in its own words: gating on
//! `author != EditAuthor::User` "made it a no-op for the only case that
//! mattered", and the author "is no longer threaded into the fork guard at all,
//! so that gate cannot be reintroduced without re-plumbing it through
//! `prepare_fork_report`".
//!
//! An `Option<EditAuthor>` would reopen exactly that hole, because "no author"
//! and "some author" would be the same type and a future caller could pass
//! either. [`WriteAttribution`] is therefore two shapes, and only
//! [`WriteAttribution::Authored`] carries an `EditAuthor` at all. So when step 2
//! wires this up, the `author: EditAuthor` argument of
//! `track_report_edit_guard::guard_task_declarations` can only be supplied from
//! that shape, and [`WriteAttribution::Structural`] has nothing to supply it
//! with — the fork belt is entered *by type*, not by a runtime test.
//!
//! # Why there is no `Plugin` origin
//!
//! Plugin attribution does not travel through `EditAuthor` alone.
//! `EditAuthor::Plugin` is a deliberate unit variant; the plugin's actual id
//! rides in the sibling field `TrackReportEdited::author_plugin_id`, which the
//! event's own docs describe as `Some` exactly when `author == Plugin` and
//! `None` for every other author.
//!
//! [`WritePolicy`] has no field that can carry that id — it names an actor, an
//! attribution, an auto-promote verdict and a recorder requirement, and none of
//! the four is a plugin id — and `track_report::persist_report_with_shadow`
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
//! The report guards — `guard_non_prose_stomp` in the `Replace` arm,
//! `validate_body_fences` in the `Replace` and `WriteMarkdown` arms,
//! `validate_block_content` in the `UpsertBlock` arms, and
//! `guard_task_declarations` after the match on every op that got that far —
//! are plain control flow inside `track_report::apply_report_op` today, with no
//! parameter that can switch any of them off. (Which content rule runs on an
//! `UpsertBlock` does vary with the op's own `kind`: `validate_block_content`
//! calls `check_prose_markdown` when `kind` is prose, and otherwise — when, and
//! only when, `parse_fence` accepts the whole content as one canonical fence —
//! schema-validates that fence's payload. But that selector is a
//! field of the op, sitting next to the content it selects a rule for; it is not
//! a [`WriteOrigin`], and `apply_report_op` has no [`WriteOrigin`] parameter —
//! this module is not wired into it at all, see the status section above.
//! Likewise `apply_report_op` reads that `kind`/`content` pair off the caller's
//! op, so the tombstone its own task-delete rewrite synthesizes is not checked:
//! also a fact of the local control flow, derived from the op in hand rather
//! than from any caller-settable knob.)
//! Modelling them as booleans would
//! reduce "turn a guard off" to writing `false` — and an exemption that can be
//! expressed will eventually be used. Per-origin differences in CAS input
//! belong on the *constructor signatures* of step 2 (its fork constructor will
//! simply have no `expected_rev` parameter), not in a runtime flag.

use std::sync::Arc;

use calm_types::event::EditAuthor;
use calm_types::ids::{ActorId, AreaId, CardId, TrackId};
use calm_types::model::CardRole;
use calm_types::runtime::AgentProvider;
use calm_types::worker::WorkerSessionId;

use crate::error::CalmError;
use crate::recorder_shadow::RecorderShadowProbe;

/// The MCP-agent identity behind an agent-channel report write.
///
/// Field types follow `mcp_server::registry::ToolCallIdentity`, the struct
/// every agent write is derived from today, but typed rather than stringly:
/// `ToolCallIdentity` stores `card_id`/`session_id` as `String` and `track_id`
/// as `Option<String>`.
///
/// # `track_id` is required, and it is the *target* track
///
/// `track_id` is **required** here, unlike on `ToolCallIdentity`. A report write
/// is track-scoped by construction, and the recorder gate already refuses an
/// agent principal without a track — `ToolCallIdentity::to_principal` returns
/// `None`, and `CardDecisionSinkRecorderShadowProbe::record` turns that `None`
/// into `Forbidden`.
///
/// Making the field required makes "agent write with no track" *unrepresentable
/// in this type*, which is not the same as performing the refusal. **No code in
/// this module refuses anything on this account** — the fields are public and
/// there is no constructor. What the required field buys is that step 2's
/// constructor cannot build an `AgentOrigin` without first resolving the
/// `Option<String>`; producing the `Forbidden` for a `None` is that caller's
/// job, because only the caller holds the request context the error names.
///
/// The track this carries is the **track being written**, as resolved for the
/// call: `mcp_server::tools::track_report::resolve_report_for_caller` looks up
/// the caller's *own* card by `identity.card_id` and takes that card's
/// `track_id`. It is not always a spec card — `calm.report.write_markdown` and
/// the block tools admit `CardRole::Assistant` too
/// (`require_role_any(&identity, &[CardRole::Spec, CardRole::Assistant])` in
/// `mcp_server/tools/track_report_blocks.rs`), and for such a call the track
/// comes from the Assistant card. That resolved track is what
/// `decision_sink::CardDecisionSink::commit_report_op` puts in the recorder
/// probe's `track_id` field.
///
/// It is a *different input* from the track identity resolution attaches to the
/// principal (`ToolCallIdentity::to_principal` copies `identity.track_id`, which
/// `card_identity_get_by_session` filled from `cards WHERE session_id = ?`),
/// even though both end up reading the `cards.track_id` column today and are
/// therefore equal. The gate reads only one of the two: `decide_recorder`
/// destructures `Principal::Agent { session_id, .. }` and never touches
/// `principal.track_id`. Its other side is a fresh in-transaction read —
/// `worker_sessions` row → `session.card_id` → `read_card_track` — compared
/// against the target track it was passed as an argument (`card_track != track`,
/// `calm-truth/src/decision_gate.rs`).
///
/// So the reason step 2 must keep feeding the probe's target track from *this*
/// field is not that collapsing the two would make the gate compare a value
/// with itself; it would not, because the gate's other side is that fresh read
/// either way. The reason is that this field is the side that says *which track
/// the write claims to land on*. Feed the principal's identity track instead and
/// both sides of the comparison become session-derived, so the check stops
/// saying anything about the write's target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOrigin {
    pub card_id: CardId,
    pub role: CardRole,
    pub provider: AgentProvider,
    pub session_id: WorkerSessionId,
    /// The track whose report is being written. See the type docs: not the
    /// principal's identity-resolved track, even though they are equal today.
    ///
    /// This field is **live**, not descriptive: [`recorder_probe_for_agent`]
    /// (`decision_sink.rs`) builds the recorder probe's target track out of it,
    /// and `PrincipalDecisionGate::decide_recorder` refuses the write when the
    /// acting session's card does not live on that track. So a bogus track here
    /// changes the gate's answer — which is what
    /// `report_write_origin_threading::mcp_recorder_probe_gates_on_the_track_being_written`
    /// exists to detect.
    pub track_id: TrackId,
    /// The area the acting agent is connected to, as
    /// `ToolCallIdentity::to_principal` reads it.
    ///
    /// Carried for one reason: the recorder probe's `Principal::Agent` cannot
    /// be constructed without it (`calm-types/src/worker.rs`), and since the
    /// probe is now built from this origin and from nothing else, the origin
    /// has to hold every input the probe needs. Today's recorder gate does not
    /// *read* it — `decide_recorder` destructures
    /// `Principal::Agent { session_id, .. }` — so no test can tell a wrong
    /// `area_id` from a right one. It is here because the alternative is
    /// inventing one inside the constructor, which would be a lie the day some
    /// gate does read it.
    pub area_id: AreaId,
}

/// The actual initiator of a fork, as the server derived it — never a
/// client-claimed string.
///
/// Today `routes::tracks::create_track_structure` attributes the fork's
/// `CardAdded` events to `actor.to_actor_id()`, i.e. the `ActorId` the `Actor`
/// extractor produced from the (validated) `X-Calm-Actor` header. That is the
/// value this carries: fork emits no `TrackReportEdited`, so the initiator is
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
/// `RoleViolation::EmptyAiCardId` when the track-create events are gated
/// (`calm-truth/src/role_gate.rs`), so under `ai:codex` the route 403s and the
/// track is never created.
///
/// Note *where* that refusal lands, because it is later than it looks. The gate
/// runs on the batch the write closure returned
/// (`write_with_actor_events`, `calm-truth/src/db/sqlite/events.rs`), and the
/// fork's report copy — `routes::tracks::persist_fork_report_and_project_tasks_tx`
/// — sits inside that closure. Under `ai:codex` the copy therefore **does
/// execute**, and the whole transaction is then rolled back. The conclusion
/// holds (a non-`User` initiator cannot complete a fork and leaves nothing
/// behind), but anyone adding a side effect to the copy stage that is not
/// covered by the transaction — a file write, an outbound request, a metric —
/// must know that this path reaches their code.
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

/// Who is performing a track-report write.
///
/// There is deliberately **no `KernelSeed` variant**: kernel template seeding
/// (`routes::tracks::seed_template_track` /
/// `restamp_template_report_if_placeholder`) was the only thing that would have
/// needed one, and #1300 S2 removed it rather than naming it. Nothing reaches
/// `persist_report_with_shadow` on the kernel's behalf any more.
///
/// There is deliberately **no `Plugin` variant** either — see the module docs
/// for why the shape is wrong and what adding one later has to include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOrigin {
    /// An MCP agent writing through `decision_sink::CardDecisionSink`.
    Agent(AgentOrigin),
    /// The browser / REST user surface.
    RestUser,
    /// Track creation copying a source track's report into the new track.
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
    ///
    /// Whoever makes that edit owes a second one: restore a pass-through test.
    /// While the admitted set has one element, the `initiator.actor().clone()`
    /// in [`policy_for`] has no behavioural coverage at all — replacing it with
    /// a hardcoded `ActorId::User` keeps every test in this crate green. As
    /// soon as a second initiator shape is admitted that mutation becomes
    /// detectable, and the test that detects it has to exist.
    Fork(TrustedInitiator),
}

/// How a write is attributed in the edit log.
///
/// Two shapes on purpose — see the module docs. Not `EditAuthor`, and not
/// `Option<EditAuthor>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAttribution {
    /// The write has an author. This is the only shape that can supply the
    /// `author` argument of `guard_task_declarations`; see the module docs.
    Authored(EditAuthor),
    /// The write is a structural copy with no author. Fork only.
    Structural,
}

/// Whether the recorder gate is consulted for this write.
///
/// Shaped to the variation today's call sites carry, and no wider. Across every
/// production path that reaches `persist_report_with_shadow`, the
/// `recorder_shadow` argument takes exactly two values:
///
/// * `Some(CardDecisionSinkRecorderShadowProbe { principal, track_id })` — the
///   one agent funnel, `decision_sink::CardDecisionSink::commit_report_op`;
/// * `None` — `routes::track_report_blocks::commit`, which calls it directly,
///   plus `routes::tracks::update_track_report`, which goes through the
///   `track_report::persist_report` wrapper — and that wrapper hardcodes `None`
///   with no parameter to vary it.
///
/// So this enum has two variants, not three.
///
/// # Where the probe payload lives
///
/// Not in this enum — [`AgentGate`](RecorderRequirement::AgentGate) is a unit
/// variant. The probe is **built from [`AgentOrigin`]**, by
/// [`recorder_probe_for_agent`] (`decision_sink.rs`), and
/// [`verify_legacy_write_arguments`] is what calls it: the call site never
/// assembles a probe of its own, so there is no second place for one to drift
/// away from the origin.
///
/// That construction needs every input `Principal::Agent` has —
/// `session_id`, `track_id`, `area_id` (`calm-types/src/worker.rs`) — which is
/// why [`AgentOrigin`] carries all three. What the *gate* reads back out is
/// narrower: `decide_recorder` destructures
/// `Principal::Agent { session_id, .. }` and takes the target track as a
/// separate argument (`calm-truth/src/decision_gate.rs`), which the probe
/// supplies from [`AgentOrigin::track_id`]. So `area_id` is carried and not
/// read; see its field docs.
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
    /// inside the track-creation transaction via `card_update_with_crdt_tx`
    /// (`routes::tracks::persist_fork_report_and_project_tasks_tx`) and emits no
    /// `TrackReportEdited`. `NotGated` therefore records that a fork is subject
    /// to no recorder decision — not that some fork call site was observed
    /// passing `None`.
    NotGated,
}

/// The decisions a track-report write needs, all of them, in one value.
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
/// `Worker` or `ReportCard` agent may not write the track report.
///
/// What this function returns is the *declared* policy. Since S1 step 2 it is
/// asserted to equal what production computes, on every write that reaches one
/// of the three call sites, by [`verify_legacy_write_arguments`].
pub fn policy_for(origin: &WriteOrigin) -> Result<WritePolicy, CalmError> {
    Ok(match origin {
        WriteOrigin::Agent(agent) => {
            let (author, auto_promote_draft) = match agent.role {
                CardRole::Spec => (EditAuthor::Spec, true),
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

/// The three call-site labels a mismatch error can name. One per surviving
/// decision point; each constant is used at exactly one call site, and the
/// string is that call site's path.
pub const SITE_MCP_DECISION_SINK: &str = "decision_sink::CardDecisionSink::commit_report_op";
pub const SITE_REST_REPORT_BLOCKS: &str = "routes::track_report_blocks::commit";
pub const SITE_REST_REPORT_DOCUMENT: &str = "routes::tracks::update_track_report";

/// The arguments a checked call site must hand to
/// `track_report::persist_report{,_with_shadow}` — and the only ones it can.
///
/// # Why this type exists
///
/// The first cut of S1 step 2 had [`verify_legacy_write_arguments`] *read* the
/// call site's arguments and compare them. Reading is not holding: for the
/// recorder probe the check took a `recorder_shadow_passed: bool` computed as
/// `recorder_shadow.is_some()` on a **binding**, while the value that actually
/// reached `persist_report_with_shadow` was a separate expression written later
/// in the argument list. A review mutation exploited exactly that gap — leave
/// the binding alone and pass
/// `if identity.role == CardRole::Assistant { None } else { recorder_shadow }`
/// — and the whole package stayed green while the assistant wrote ungated.
///
/// So the check no longer inspects a proxy for the argument; it **produces the
/// argument**. Fields are private, there is no public constructor, and the only
/// way out is [`into_parts`](Self::into_parts), which consumes `self`. A call
/// site that wants to pass something else now has to visibly drop a value it
/// was handed, rather than write an expression in the obvious place.
///
/// # What this does not do: divergence is still expressible, and reachable
///
/// This type does not make disagreement impossible, and no wording here should
/// be read as saying it does. [`into_parts`](Self::into_parts) yields four
/// ordinary bindings; anything a call site writes between the destructure and
/// the persist call still compiles. The concrete shape, from review, inserted
/// straight after the destructure in
/// `decision_sink::CardDecisionSink::commit_report_op`:
///
/// ```ignore
/// let author = if identity.thread_id == "card-bound" {
///     EditAuthor::User
/// } else {
///     author
/// };
/// ```
///
/// That is not a contrived witness. `mcp_server::transport.rs` sets exactly
/// `thread_id: "card-bound"` on the `ToolCallIdentity` it builds for every
/// card-bound connection that carries no `threadId`, so the branch is taken in
/// production.
///
/// Measured, both halves. Driving an assistant block write with an identity
/// carrying that sentinel records `author: "assistant"` on the unmutated tree
/// and `author: "user"` with the mutation applied — the write lands either way.
/// And with the mutation applied the whole `mcp_integration_suite` stays green
/// at **212 passed, 0 failed**, including all seven
/// `report_write_origin_threading` tests and all eight
/// `report_write_characterization` tests: no fixture in it carries the
/// sentinel, so nothing observes the substitution.
///
/// The same is true of reassigning `actor`, `auto_promote_draft` or the probe
/// after unpacking, and `persist_report_with_shadow` is still `pub(crate)`, so
/// an in-crate caller can skip this boundary altogether.
///
/// A divergence of that kind is caught only where some test happens to assert
/// on the member it moved — by a test, never by the type.
///
/// # What closes it, and why not here
///
/// The closure is moving the decision *inside* `persist_report_with_shadow`, so
/// the boundary derives the members from a [`WriteOrigin`] and compares what it
/// actually received rather than what a caller promised to pass. That is
/// **owed to step 3**, and it is blocked by the 19 non-production call sites of
/// `track_report::persist_report` enumerated in this module's docs — in
/// particular by the two identity pairs no [`WriteOrigin`] variant can express,
/// `(Kernel, Kernel)` (7 sites) and `(Kernel, Spec)` (4 sites).
///
/// This step's contract was narrower and is met: keep the old parameters, and
/// assert at runtime that [`policy_for`]'s output equals them. What this type
/// adds on top is that the recorder probe is supplied rather than compared, and
/// that diverging from any member requires visibly discarding a value the
/// caller was handed.
pub(crate) struct LegacyWriteArguments {
    actor: ActorId,
    author: EditAuthor,
    auto_promote_draft: bool,
    recorder_shadow: Option<Arc<dyn RecorderShadowProbe>>,
}

/// Hand-written because `Arc<dyn RecorderShadowProbe>` is not `Debug` — the
/// trait is an async gate call, not a value. The probe is reported as present
/// or absent, which is what a refusal message needs to say about it.
impl std::fmt::Debug for LegacyWriteArguments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyWriteArguments")
            .field("actor", &self.actor)
            .field("author", &self.author)
            .field("auto_promote_draft", &self.auto_promote_draft)
            .field("recorder_shadow", &self.recorder_shadow.is_some())
            .finish()
    }
}

impl LegacyWriteArguments {
    /// Consume the verified bundle into the persist call's arguments, in the
    /// order `persist_report_with_shadow` takes them.
    ///
    /// Consuming on purpose: it cannot be called twice, so a call site cannot
    /// keep a spare copy around to pass a doctored variant of. That is the
    /// whole of what consuming buys — the four bindings it yields can still be
    /// reassigned before the persist call; see the type docs.
    pub(crate) fn into_parts(
        self,
    ) -> (
        ActorId,
        EditAuthor,
        bool,
        Option<Arc<dyn RecorderShadowProbe>>,
    ) {
        (
            self.actor,
            self.author,
            self.auto_promote_draft,
            self.recorder_shadow,
        )
    }
}

/// S1 step 2 — the check each production call site runs before it writes, and
/// the source of the arguments it then writes with.
///
/// Two different jobs, deliberately in one function:
///
/// * **Compared.** `actor`, `author` and `auto_promote_draft` are still
///   computed by the call site the old way (`identity.to_actor_id()`,
///   `report_op_attribution`, a literal), so there are two independent
///   derivations to hold against each other. [`policy_for`]'s answer must
///   **equal** what the call site derived, or the write is refused
///   (`CalmError::Internal`) rather than logged. Deleting those parameters is
///   step 3's job; this is the runtime evidence that deleting them would be a
///   no-op.
/// * **Supplied.** The recorder probe is not compared, because there is nothing
///   independent left to compare it *to*: this function builds it, from the
///   origin and from nothing else ([`recorder_probe_for_agent`]), and the call
///   site's only access to it is through [`LegacyWriteArguments::into_parts`].
///   Presence-comparison is what the first cut did, and a presence comparison
///   against a binding is what the review mutation walked through; see
///   [`LegacyWriteArguments`], whose docs also record what this shape still
///   leaves expressible.
///
/// Building the probe here also makes [`AgentOrigin::track_id`] load-bearing.
/// Before, the probe's target track was a second expression at the call site,
/// so pointing it at `identity.track_id` while the origin kept `track.id` was
/// invisible — and equally, a bogus `track_id` in the origin changed nothing.
/// Now they are one value, and the track the origin names is the track
/// `decide_recorder` checks the acting session's card against.
///
/// The error names the call site, the field, the declared value and the passed
/// value, because "the arguments differ" is not actionable without all four.
pub(crate) fn verify_legacy_write_arguments(
    site: &str,
    origin: &WriteOrigin,
    actor: &ActorId,
    author: EditAuthor,
    auto_promote_draft: bool,
) -> Result<LegacyWriteArguments, CalmError> {
    let policy = policy_for(origin)?;
    let mismatch = |field: &str, declared: String, passed: String| {
        CalmError::Internal(format!(
            "{site}: write-origin mismatch on {field}: origin {origin:?} declares {declared}, \
             call site passes {passed}"
        ))
    };

    if policy.actor() != actor {
        return Err(mismatch(
            "actor",
            format!("{:?}", policy.actor()),
            format!("{actor:?}"),
        ));
    }
    // Binds what it checks: the `EditAuthor` handed back below is the one this
    // arm just proved equal to the call site's, taken off the policy.
    let policy_author = match policy.attribution() {
        WriteAttribution::Authored(declared) if declared == author => declared,
        WriteAttribution::Authored(declared) => {
            return Err(mismatch(
                "author",
                format!("{declared:?}"),
                format!("{author:?}"),
            ));
        }
        // No production call site is a `Fork` today, so this arm is the
        // fail-closed answer to a caller that starts passing one: `Structural`
        // has no `EditAuthor` to compare against, and the fork path does not go
        // through this boundary at all (see [`RecorderRequirement::NotGated`]).
        WriteAttribution::Structural => {
            return Err(mismatch(
                "author",
                "structural (no author)".to_string(),
                format!("{author:?}"),
            ));
        }
    };
    if policy.auto_promote_draft() != auto_promote_draft {
        return Err(mismatch(
            "auto_promote_draft",
            format!("{}", policy.auto_promote_draft()),
            format!("{auto_promote_draft}"),
        ));
    }
    // Supplied, not compared — see the function docs. Exhaustive on the
    // requirement rather than on the origin, so a future variant whose policy
    // says `AgentGate` cannot silently obtain `None` by not being an
    // `Agent`: it fails closed here instead.
    let recorder_shadow: Option<Arc<dyn RecorderShadowProbe>> = match policy.recorder() {
        RecorderRequirement::AgentGate => {
            let WriteOrigin::Agent(agent) = origin else {
                return Err(CalmError::Internal(format!(
                    "{site}: origin {origin:?} declares {:?} but carries no agent to build the \
                     recorder probe from",
                    policy.recorder()
                )));
            };
            Some(crate::decision_sink::recorder_probe_for_agent(agent))
        }
        RecorderRequirement::NotGated => None,
    };

    // The values handed back are the *policy's*, not the caller's, even though
    // the comparisons above just proved them equal. That is the point: the
    // caller's copies stop being reachable at the persist call.
    Ok(LegacyWriteArguments {
        actor: policy.actor().clone(),
        author: policy_author,
        auto_promote_draft: policy.auto_promote_draft(),
        recorder_shadow,
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
            track_id: TrackId::from("w_1".to_string()),
            area_id: AreaId::from("ar_1".to_string()),
        })
    }

    fn user_fork() -> TrustedInitiator {
        TrustedInitiator::new(ActorId::User).expect("a user is a reachable fork initiator")
    }

    /// The declared intent for each origin, stated once. This asserts what
    /// this module *says*; comparing it against what production computes is
    /// [`verify_legacy_write_arguments`]'s job, exercised below.
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
    // `fork by user` row of the table above. Whoever widens the admitted set
    // must add it back — that is the point at which the mutation becomes
    // detectable. See the `WriteOrigin::Fork` docs.

    fn spec_session() -> ActorId {
        ActorId::AiSpecSession(WorkerSessionId::from("sess_1".to_string()))
    }

    fn message(error: CalmError) -> String {
        match error {
            CalmError::Internal(message) => message,
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// The legacy triple each of the three production call sites hand-writes
    /// today, checked against the origin that call site builds. These rows are
    /// the unit-level statement of the same equality the call sites assert at
    /// runtime; the integration proof that the call sites really run it is
    /// `tests/cases/report_write_origin_threading.rs`.
    ///
    /// The returned bundle is asserted too, because it — not the arguments —
    /// is what the call site goes on to write with.
    #[test]
    fn every_production_call_site_triple_matches_its_origin() {
        let (actor, author, auto_promote_draft, recorder_shadow) = verify_legacy_write_arguments(
            SITE_MCP_DECISION_SINK,
            &agent(CardRole::Spec, AgentProvider::Codex),
            &spec_session(),
            EditAuthor::Spec,
            true,
        )
        .expect("the MCP funnel's spec triple")
        .into_parts();
        assert_eq!(actor, spec_session());
        assert_eq!(author, EditAuthor::Spec);
        assert!(auto_promote_draft);
        assert!(
            recorder_shadow.is_some(),
            "a spec agent write is gated, so the check must hand back a probe"
        );

        let (actor, author, auto_promote_draft, recorder_shadow) = verify_legacy_write_arguments(
            SITE_MCP_DECISION_SINK,
            &agent(CardRole::Assistant, AgentProvider::Codex),
            &ActorId::AiCodexSession(WorkerSessionId::from("sess_1".to_string())),
            EditAuthor::Assistant,
            false,
        )
        .expect("the MCP funnel's assistant triple")
        .into_parts();
        assert_eq!(
            actor,
            ActorId::AiCodexSession(WorkerSessionId::from("sess_1".to_string()))
        );
        assert_eq!(author, EditAuthor::Assistant);
        assert!(!auto_promote_draft);
        // The mutation this replaces the old `recorder_shadow_passed: bool`
        // with: an assistant arm that drops the probe. It cannot be written at
        // the call site any more, and it cannot be written here either — the
        // probe comes from `policy_for`'s `AgentGate`, which is declared for
        // both agent roles.
        assert!(
            recorder_shadow.is_some(),
            "the assistant arm is gated exactly as the spec arm is"
        );

        for site in [SITE_REST_REPORT_BLOCKS, SITE_REST_REPORT_DOCUMENT] {
            let (actor, author, auto_promote_draft, recorder_shadow) =
                verify_legacy_write_arguments(
                    site,
                    &WriteOrigin::RestUser,
                    &ActorId::User,
                    EditAuthor::User,
                    false,
                )
                .unwrap_or_else(|error| panic!("{site}: {error:?}"))
                .into_parts();
            assert_eq!(actor, ActorId::User, "{site}");
            assert_eq!(author, EditAuthor::User, "{site}");
            assert!(!auto_promote_draft, "{site}");
            assert!(
                recorder_shadow.is_none(),
                "{site}: the REST user surface is `NotGated`, so no probe is handed out"
            );
        }
    }

    /// One wrong member at a time, so each comparison is exercised alone, and
    /// the message has to name the site and the field — that is what makes a
    /// production refusal locatable.
    ///
    /// Three members, not four: the recorder probe is no longer an argument to
    /// disagree with. It is produced by this function, and
    /// [`every_production_call_site_triple_matches_its_origin`] asserts what it
    /// produces.
    #[test]
    fn one_wrong_member_of_the_triple_is_refused_and_named() {
        let origin = agent(CardRole::Spec, AgentProvider::Codex);
        let cases: Vec<(&str, ActorId, EditAuthor, bool)> = vec![
            ("actor", ActorId::User, EditAuthor::Spec, true),
            ("author", spec_session(), EditAuthor::Assistant, true),
            (
                "auto_promote_draft",
                spec_session(),
                EditAuthor::Spec,
                false,
            ),
        ];
        for (field, actor, author, auto_promote_draft) in cases {
            let error = verify_legacy_write_arguments(
                SITE_MCP_DECISION_SINK,
                &origin,
                &actor,
                author,
                auto_promote_draft,
            )
            .expect_err("a mismatched triple must fail closed");
            let message = message(error);
            assert!(
                message.contains(SITE_MCP_DECISION_SINK),
                "{field}: message names the call site; got {message}"
            );
            assert!(
                message.contains(&format!("mismatch on {field}")),
                "{field}: message names the field; got {message}"
            );
        }
    }

    /// The role branches differ in three of the four members, so feeding one
    /// role's origin the other role's arguments is refused. This is the
    /// unit-level half of gap 3 of the characterization suite: a funnel that
    /// keeps one arm's decisions while taking the other arm's origin cannot
    /// pass.
    #[test]
    fn the_two_agent_roles_may_not_borrow_each_others_arguments() {
        let error = verify_legacy_write_arguments(
            SITE_MCP_DECISION_SINK,
            &agent(CardRole::Assistant, AgentProvider::Codex),
            &spec_session(),
            EditAuthor::Spec,
            true,
        )
        .expect_err("an assistant origin may not carry the spec's arguments");
        assert!(message(error).contains("mismatch on actor"));
    }

    /// A `Fork` origin has no `EditAuthor` to compare against, so it cannot be
    /// reconciled with any arguments this boundary is handed — the fork path
    /// does not come through here at all.
    #[test]
    fn a_structural_origin_cannot_satisfy_an_authored_call_site() {
        let error = verify_legacy_write_arguments(
            SITE_REST_REPORT_DOCUMENT,
            &WriteOrigin::Fork(user_fork()),
            &ActorId::User,
            EditAuthor::User,
            false,
        )
        .expect_err("structural attribution has no author");
        let message = message(error);
        assert!(message.contains("mismatch on author"), "got {message}");
        assert!(message.contains("structural"), "got {message}");
    }

    /// A refused role is refused before any comparison happens: the error is
    /// `policy_for`'s `Forbidden`, not a mismatch.
    #[test]
    fn a_refused_role_fails_closed_as_forbidden_not_as_a_mismatch() {
        let error = verify_legacy_write_arguments(
            SITE_MCP_DECISION_SINK,
            &agent(CardRole::Worker, AgentProvider::Codex),
            &ActorId::User,
            EditAuthor::User,
            false,
        )
        .expect_err("a worker may not write the report");
        assert!(
            matches!(error, CalmError::Forbidden(_)),
            "expected Forbidden, got {error:?}"
        );
    }

    /// Bump this alongside a new arm in [`actor_variant_label`].
    const ACTOR_ID_NON_USER_VARIANTS: usize = 9;

    /// A label per `ActorId` variant. The `match` has **no `_` arm**: adding a
    /// variant to `ActorId` fails to compile here, which is what brings the
    /// author to [`every_non_user_actor`] below.
    fn actor_variant_label(actor: &ActorId) -> &'static str {
        match actor {
            ActorId::User => "User",
            ActorId::Kernel => "Kernel",
            ActorId::KernelDispatcher => "KernelDispatcher",
            ActorId::Plugin(_) => "Plugin",
            ActorId::AiSpec(_) => "AiSpec",
            ActorId::AiCodex(_) => "AiCodex",
            ActorId::AiClaude(_) => "AiClaude",
            ActorId::AiSpecSession(_) => "AiSpecSession",
            ActorId::AiCodexSession(_) => "AiCodexSession",
            ActorId::AiClaudeSession(_) => "AiClaudeSession",
        }
    }

    /// One value for every non-`User` `ActorId` variant — the enumeration is
    /// the point, so it is checked rather than assumed: the labels the samples
    /// produce must be [`ACTOR_ID_NON_USER_VARIANTS`] distinct ones.
    fn every_non_user_actor() -> Vec<ActorId> {
        let samples = vec![
            ActorId::Kernel,
            // The actor `operation::child_track_adapter` uses when it creates a
            // track — the most plausible future non-`User` fork initiator.
            ActorId::KernelDispatcher,
            ActorId::Plugin("git-forge".to_string()),
            ActorId::AiSpec(CardId::from("c_2".to_string())),
            ActorId::AiCodex(CardId::from("c_2".to_string())),
            ActorId::AiClaude(CardId::from("c_2".to_string())),
            ActorId::AiSpecSession(WorkerSessionId::from("sess_fork".to_string())),
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

    /// `TrustedInitiator` is the fork's author-guard bypass in type form, so it
    /// admits only the initiator shape the fork route can actually produce.
    /// The refused set is *every* non-`User` variant of `ActorId`, plus the
    /// empty-card-id `AiCodex` that `Actor::to_actor_id` emits for the legacy
    /// `ai:codex` header — the one non-`User` shape the fork route can hand in.
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

    /// Structural attribution must not be reachable for anything but a fork:
    /// it is the shape that carries no `EditAuthor`, so once step 2 wires this
    /// up it is the one that reaches the write without `guard_task_declarations`
    /// having an author to judge.
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
