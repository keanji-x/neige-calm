//! Track-as-Actor PR3 (#136) — authorization gate at the single write entry.
//!
//! The gate runs inside `Repo::write_with_event` / `Repo::log_pure_event`,
//! after the closure produces an `Event`, before `event_append_in_tx`
//! commits the row. A violation rolls the txn back: no entity write,
//! no event row, no broadcast. The kernel is the only safety boundary
//! between AI-controlled cards and the track-level kernel state, so the
//! gate is deliberately strict — *deny* is the default for anything
//! ambiguous, and we re-confirm role lookups against the in-process
//! `CardRoleCache` rather than trusting the actor's claimed identity.
//!
//! ## What the gate enforces
//!
//! 1. **Empty-CardId guard.** `ActorId::AiCodex(CardId(""))`,
//!    `ActorId::AiClaude(CardId(""))`, and `ActorId::AiSpec(CardId(""))`
//!    are rejected outright. This catches
//!    the PR2 stopgap path in `crate::actor::Actor::to_actor_id` where
//!    the `X-Calm-Actor: ai:codex` header has no card context to attach.
//!    PR3 reattributes the codex bridge ingest to a real card id (see
//!    `routes::codex::ingest_hook`), so this branch ends up firing only
//!    when something else regresses — fail loud, not silent.
//!
//! 2. **`Event::TrackUpdated` is gated to spec cards.** The actor must be
//!    `User`, `Kernel`, or `AiSpec(card_id)` where the cache confirms
//!    `CardRole::Spec`. Any `AiCodex` / `AiClaude` actor — even one bound
//!    to a card — is rejected: worker cards must not edit track-level state.
//!
//! 3. **Worker/ReportCard self-scope check.** When an
//!    `AiCodex(card_id)` or `AiClaude(card_id)` actor's cached role is
//!    `Worker` or `ReportCard`, the event's
//!    `EventScope` must be the
//!    same card, its `track` field must match the card's home track
//!    (issue #232), *and* its `area` field must match the card's
//!    home area (issue #234). A worker or report-card actor that tries
//!    to emit a `Track` or `Area` scope event — or a Card scope with a
//!    spoofed `track` or `area` — is refused.
//!
//! 4. **Dispatch-request events are gated to spec cards.** Issue #583.
//!    `Event::CodexWorkerRequested` and `Event::TerminalWorkerRequested` are
//!    refused for any `AiCodex` / `AiClaude` actor, mirroring the
//!    `TrackUpdated` rule. Spec card (`AiSpec`) with cached role `Spec`
//!    passes; User / Kernel / KernelDispatcher / Plugin keep their
//!    unrestricted access for forward compatibility (no current emitter
//!    in those families).
//!
//! 5. **User / Kernel / KernelDispatcher / Plugin(_)** are unrestricted
//!    in PR3. The kernel's own writes (FSM projector, terminal sweeper,
//!    plugin callback dispatcher) and the user's REST surface continue
//!    to flow through the gate unchanged.
//!
//! 6. **Unknown card.** If the actor names a card the cache doesn't
//!    know, the write is denied. Two possible causes:
//!      * the card was deleted between the actor's request landing and
//!        the gate running (race; safe to reject),
//!      * an attacker fabricated a card id (the gate is the last line
//!        of defense, deny by default).

use crate::card_role_cache::CardRoleCache;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::CardRole;
use crate::track_area_cache::TrackAreaCache;
use crate::worker::WorkerSessionId;
use calm_types::proposal::ProposalDecision;
use thiserror::Error;

/// Reasons the gate may refuse a write. Surfaced verbatim into the
/// returned `CalmError::Forbidden` so test assertions can pattern-match
/// without parsing a free-form string.
#[derive(Debug, Error)]
pub enum RoleViolation {
    #[error("AiCodex/AiClaude/AiSpec actor has empty card id (likely from legacy AI header path)")]
    EmptyAiCardId,

    /// A session-keyed actor reached the sync gate; session→authority
    /// resolution lands in HP1-a-2 (#770) — until then session actors
    /// are denied.
    #[error(
        "session-keyed actor {session} reached sync role gate before session authority resolution"
    )]
    SessionActorUnresolved { session: WorkerSessionId },

    #[error(
        "session {session} has no live worker_sessions row (deleted / unknown / never-committed); denied fail-closed (#770)."
    )]
    SessionRowMissing { session: WorkerSessionId },

    #[error(
        "session {session} is not in an active-authority state (terminal/inactive); denied fail-closed (#770)."
    )]
    SessionNotActive { session: WorkerSessionId },

    #[error(
        "session {session} has no bound card; cardless authority lands with PR11; denied for now (#770)."
    )]
    CardlessSessionDenied { session: WorkerSessionId },

    #[error("error reading authority for session {session}; denied fail-closed (#770).")]
    SessionResolutionError { session: WorkerSessionId },

    #[error(
        "session {session} claims spec authority but its resolved card {card} is not Spec-roled (or unknown); denied fail-closed (#770)."
    )]
    SessionSpecRoleMismatch {
        session: WorkerSessionId,
        card: CardId,
    },

    #[error("only spec cards (or User/Kernel) may emit track.updated (actor={actor})")]
    NotSpecForTrack { actor: String },

    #[error("only spec cards (or User/Kernel) may emit dispatch-request events (actor={actor})")]
    NotSpecForDispatch { actor: String },

    #[error(
        "task.dispatched is a kernel-only scheduler record; no card-derived actor may emit it (actor={actor})"
    )]
    NotKernelForTaskDispatched { actor: String },

    #[error(
        "task.context_frozen is a strict kernel scheduler record; User and card-derived actors may not emit it (actor={actor})"
    )]
    NotKernelForTaskContextFrozen { actor: String },

    #[error(
        "task.context_advanced is a strict kernel context verdict; User and card-derived actors may not emit it (actor={actor})"
    )]
    NotKernelForTaskContextAdvanced { actor: String },

    #[error(
        "task.gate_result is a kernel-only gate-runner record; no card-derived actor may emit it (actor={actor})"
    )]
    NotKernelForTaskGateResult { actor: String },

    #[error("only spec cards may emit review/ratify request events (actor={actor})")]
    NotSpecForReviewRatify { actor: String },

    #[error("only User may emit ratify.resolved (actor={actor})")]
    NotUserForRatifyResolved { actor: String },

    #[error(
        "only the submitting plugin may emit proposal.submitted (actor={actor}, payload plugin_id={payload_plugin})"
    )]
    NotSubmitterPluginForProposalSubmitted {
        actor: String,
        payload_plugin: String,
    },

    #[error("only User may emit proposal.resolved with decision {decision} (actor={actor})")]
    NotUserForProposalResolved { decision: String, actor: String },

    #[error(
        "only the submitting plugin may emit proposal.resolved{{withdrawn}} (actor={actor}, payload plugin_id={payload_plugin})"
    )]
    NotSubmitterPluginForProposalWithdrawn {
        actor: String,
        payload_plugin: String,
    },

    #[error("worker card {card} is out of scope {scope}")]
    WorkerOutOfScope { card: CardId, scope: String },

    /// #1189 — an `Assistant`-roled card wrote outside the two card
    /// scopes it owns (itself, and its home track's report card).
    #[error("assistant card {card} is out of scope {scope}")]
    AssistantOutOfScope { card: CardId, scope: String },

    #[error(
        "AI worker actor references card {card} that the role cache does not know — \
         card was likely deleted or never minted; denying by default"
    )]
    UnknownCard { card: CardId },
}

/// Run the role gate. Returns `Ok(())` on success, `Err(RoleViolation)`
/// to refuse the write. Caller wraps the error into a transactional
/// rollback — see the `write_with_event` / `log_pure_event` impls in
/// `db::sqlite`.
///
/// The function is intentionally side-effect-free: it never mutates the
/// cache (only the card-create / -delete paths do), and it never reads
/// the database. That's why the gate is cheap enough to run inline at
/// every write site.
pub fn enforce_role(
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
    cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
) -> Result<(), RoleViolation> {
    // --- (1) Empty-CardId guard. ---
    //
    // PR2's `Actor::to_actor_id` returns `AiCodex(CardId(""))` for the
    // legacy `X-Calm-Actor: ai:codex` header path because there's no
    // card context at the REST entry. PR3 must not silently match an
    // empty CardId against any real card — that would be a
    // gate-bypass. We reject loud and let the call site (the codex
    // bridge ingest in routes/codex.rs) attribute a real card.
    if let ActorId::AiCodex(c) | ActorId::AiClaude(c) | ActorId::AiSpec(c) = actor
        && c.as_str().is_empty()
    {
        return Err(RoleViolation::EmptyAiCardId);
    }
    if let ActorId::AiSpecSession(s) | ActorId::AiCodexSession(s) | ActorId::AiClaudeSession(s) =
        actor
        && s.as_str().is_empty()
    {
        return Err(RoleViolation::SessionActorUnresolved { session: s.clone() });
    }

    // --- (2) `TrackUpdated` is spec-only. ---
    //
    // The track-level authority decision: only the spec card (PR6) is
    // allowed to update the track row. User + Kernel keep their
    // unrestricted authority (the user is *the* authority; Kernel is
    // the FSM projector / sweeper / plugin dispatcher, which writes
    // server-internal lifecycle the user implicitly authorized at
    // boot).
    if matches!(event, Event::TrackUpdated(_)) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(_) => {
                // Plugins are unrestricted in PR3 — see the
                // `RouteRepo` capability split docs. Plugin-driven
                // track edits are rare in practice (the surface for
                // them lives in the plugin host callback dispatcher,
                // which is server-internal). If PR4+ tightens this,
                // it lands here.
            }
            ActorId::AiSpec(card_id) => {
                let role = cache.get(card_id);
                if role != Some(CardRole::Spec) {
                    return Err(RoleViolation::NotSpecForTrack {
                        actor: format!("AiSpec({card_id})"),
                    });
                }
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                // Even an AI worker actor whose card happens to be
                // `Spec`-roled (impossible in PR3 — spec cards are
                // bound to `AiSpec`) is rejected here. The actor
                // variant is the wire-level claim; the gate sticks
                // to it rather than re-binding via the cache.
                return Err(RoleViolation::NotSpecForTrack {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiSpecSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // --- (2.5) Dispatch-request + plan-revision events are spec-only. ---
    //
    // Issue #583. `calm.task.dispatch` is gated to Spec at the MCP
    // soft gate (`emit.rs::dispatch_request`), but the in-tx gate must
    // also refuse worker AI actors from emitting these events to provide
    // real kernel-level defense-in-depth — otherwise an internal caller
    // that reaches `write_with_event_typed` with an AiCodex/AiClaude
    // worker actor + a dispatch event can still commit a recursive
    // worker-tree mint. Mirrors section (2)'s shape.
    //
    // Issue #644 — `Event::PlanUpdated` joins the list: the task plan is
    // track-level authority (the PR-B scheduler dispatches whatever the
    // plan says), so a worker actor writing plan revisions would be the
    // same recursive-mint hole one hop removed.
    if matches!(
        event,
        Event::CodexWorkerRequested { .. }
            | Event::TerminalWorkerRequested { .. }
            | Event::PlanUpdated { .. }
    ) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(_) => {}
            ActorId::AiSpec(card_id) => {
                let role = cache.get(card_id);
                if role != Some(CardRole::Spec) {
                    return Err(RoleViolation::NotSpecForDispatch {
                        actor: format!("AiSpec({card_id})"),
                    });
                }
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotSpecForDispatch {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiSpecSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // --- (2.6) `task.dispatched` is kernel-only. ---
    //
    // Issue #644 PR-B. The scheduler appends `Event::TaskDispatched`
    // inside its claim tx as the projection's dispatch record (§5.6).
    // It is a *kernel observation* of plan execution, not a card
    // authority: a spec forging it could fabricate "the kernel claimed
    // this task" records that desynchronize the runs projection from
    // the tasks table, and a worker forging it is the #583 recursive
    // hole again. Every card-derived actor (AiSpec included) AND
    // plugins are refused — unlike sections (2)/(2.5), a plugin has no
    // business writing the scheduler's claim record, so this gate is
    // narrower: only User / Kernel / KernelDispatcher pass.
    if matches!(event, Event::TaskDispatched { .. }) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(name) => {
                return Err(RoleViolation::NotKernelForTaskDispatched {
                    actor: format!("Plugin({name})"),
                });
            }
            ActorId::AiSpec(card_id) => {
                return Err(RoleViolation::NotKernelForTaskDispatched {
                    actor: format!("AiSpec({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskDispatched {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiSpecSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // Issue #985 PR3a-i. Context freeze and advancement records are facts
    // produced by the scheduler kernel. Unlike the older "kernel-only"
    // gates, these records are strict: a plain User cannot forge either the
    // frozen set or its stale verdict.
    if matches!(event, Event::TaskContextFrozen { .. }) {
        match actor {
            ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::User => {
                return Err(RoleViolation::NotKernelForTaskContextFrozen {
                    actor: "User".into(),
                });
            }
            ActorId::Plugin(name) => {
                return Err(RoleViolation::NotKernelForTaskContextFrozen {
                    actor: format!("Plugin({name})"),
                });
            }
            ActorId::AiSpec(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextFrozen {
                    actor: format!("AiSpec({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextFrozen {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiSpecSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    if matches!(event, Event::TaskContextAdvanced { .. }) {
        match actor {
            ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::User => {
                return Err(RoleViolation::NotKernelForTaskContextAdvanced {
                    actor: "User".into(),
                });
            }
            ActorId::Plugin(name) => {
                return Err(RoleViolation::NotKernelForTaskContextAdvanced {
                    actor: format!("Plugin({name})"),
                });
            }
            ActorId::AiSpec(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextAdvanced {
                    actor: format!("AiSpec({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextAdvanced {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiSpecSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // --- (2.7) `task.gate_result` is kernel-only. ---
    //
    // Issue #644 PR-C. The gate runner appends `Event::TaskGateResult`
    // in the same tx as the `verifying → done|failed` tasks-row flip.
    // It is the kernel's *machine verdict* for a verification gate — a
    // card forging it could fabricate "the gate passed" evidence that
    // the spec (and the lifecycle promotion) treats as ground truth.
    // Same narrow gate as (2.6): only User / Kernel / KernelDispatcher
    // pass; every card-derived actor AND plugins are refused.
    if matches!(event, Event::TaskGateResult { .. }) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(name) => {
                return Err(RoleViolation::NotKernelForTaskGateResult {
                    actor: format!("Plugin({name})"),
                });
            }
            ActorId::AiSpec(card_id) => {
                return Err(RoleViolation::NotKernelForTaskGateResult {
                    actor: format!("AiSpec({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskGateResult {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiSpecSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // --- (2.8) `review.round` + `ratify.requested` are spec-only. ---
    //
    // Issue #760 slice 5b. These are policy records authored by the spec
    // agent after it has correlated reviewer channels or decided a human
    // ratify gate is needed. Unlike the older track-write/dispatch arms,
    // User/Kernel/Plugin do NOT pass here: letting any non-spec actor forge
    // `converged=true` or a ratify request would bypass the review protocol.
    if matches!(
        event,
        Event::ReviewRound { .. } | Event::RatifyRequested { .. }
    ) {
        match actor {
            ActorId::AiSpec(card_id) => {
                if cache.get(card_id) != Some(CardRole::Spec) {
                    return Err(RoleViolation::NotSpecForReviewRatify {
                        actor: actor.to_string(),
                    });
                }
            }
            _ => {
                return Err(RoleViolation::NotSpecForReviewRatify {
                    actor: actor.to_string(),
                });
            }
        }
    }

    // --- (2.9) `ratify.resolved` is User-only. ---
    //
    // The grant/deny decision is the human half of the ratify gate. It must
    // not be forgeable by the spec, workers, plugins, or the kernel, or an
    // AI actor could self-approve the pause.
    if matches!(event, Event::RatifyResolved { .. }) {
        match actor {
            ActorId::User => {}
            _ => {
                return Err(RoleViolation::NotUserForRatifyResolved {
                    actor: actor.to_string(),
                });
            }
        }
    }

    // --- (2.10) `proposal.submitted` is submitting-plugin-only. ---
    //
    // Issue #955 §5.4. The proposal channel's authority model: only a
    // plugin may open a proposal, and only for itself — the payload's
    // `plugin_id` (kernel-injected at the callback layer) must equal
    // the envelope actor's plugin id. The gate is a pure function over
    // `(actor, event, scope)`, so this field comparison IS the in-tx
    // hard clause; the connection-injection itself happens upstream.
    // Every other actor family (User, Kernel, spec, workers, sessions)
    // is refused — a proposal forged by anything but the named plugin
    // would corrupt the channel's attribution invariant (§5.3).
    if let Event::ProposalSubmitted { plugin_id, .. } = event {
        match actor {
            ActorId::Plugin(id) if id == plugin_id => {}
            _ => {
                return Err(RoleViolation::NotSubmitterPluginForProposalSubmitted {
                    actor: actor.to_string(),
                    payload_plugin: plugin_id.clone(),
                });
            }
        }
    }

    // --- (2.11) `proposal.resolved` splits by decision. ---
    //
    // Issue #955 §5.4, mirroring the ratify rule in (2.9):
    //   * `accepted` / `rejected` / `stale` are the human half of the
    //     adjudication (stale is the accept attempt whose in-tx
    //     anchoring checks failed — still user-triggered). User-only:
    //     a plugin, spec, or the kernel must never self-approve.
    //   * `withdrawn` is the submitting plugin reclaiming its own
    //     pending slot — `ActorId::Plugin(id)` with `id` equal to the
    //     payload's submitter `plugin_id`, nothing else. The
    //     "pending AND actually owned by this plugin" *factual* check
    //     lived with the withdraw handler inside the same write tx before
    //     the channel was withdrawn in #973; this clause pins the identity
    //     half for historical events.
    if let Event::ProposalResolved {
        plugin_id,
        decision,
        ..
    } = event
    {
        match decision {
            ProposalDecision::Withdrawn => match actor {
                ActorId::Plugin(id) if id == plugin_id => {}
                _ => {
                    return Err(RoleViolation::NotSubmitterPluginForProposalWithdrawn {
                        actor: actor.to_string(),
                        payload_plugin: plugin_id.clone(),
                    });
                }
            },
            ProposalDecision::Accepted | ProposalDecision::Rejected | ProposalDecision::Stale => {
                match actor {
                    ActorId::User => {}
                    _ => {
                        return Err(RoleViolation::NotUserForProposalResolved {
                            decision: decision.as_str().to_string(),
                            actor: actor.to_string(),
                        });
                    }
                }
            }
        }
    }

    // --- (3) Worker/ReportCard self-scope check + (5) unknown-card deny. ---
    //
    // For AI worker actors: confirm the cache knows the card, and if
    // the cached role is `Worker` or `ReportCard`, refuse anything
    // broader than that card's own scope. The check is three-pronged:
    //   * `scope.card == self_card` — the actor only writes into its
    //     own card scope;
    //   * `scope.track == cache.track_of(self_card)` — the supplied
    //     `track` field must match the worker's home track (closes
    //     issue #232: a Worker could otherwise forge `track: <ANY>`
    //     and the kernel would route the event to that track's
    //     subscribers).
    //   * `scope.area == track_area_cache.area_of(home_track)` — the
    //     supplied `area` must match the home track's persisted area
    //     (closes issue #234: same fan-out spoof shape as #232 but
    //     one level up). Area is immutable per track so the lookup is
    //     stable for the card's lifetime.
    //
    if let ActorId::AiSpecSession(s) | ActorId::AiCodexSession(s) | ActorId::AiClaudeSession(s) =
        actor
    {
        return Err(RoleViolation::SessionActorUnresolved { session: s.clone() });
    }

    if let ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) = actor {
        match cache.get(card_id) {
            None => {
                return Err(RoleViolation::UnknownCard {
                    card: card_id.clone(),
                });
            }
            Some(CardRole::Worker) => {
                enforce_card_self_scope(card_id, scope, cache, track_area_cache)?;
            }
            // Lifecycle carveout — hook bridges run as subprocesses of
            // their worker regardless of the card's role, and the REST
            // spec-input route may receive the legacy `ai:codex` header
            // before route context rebinds it to the spec card. These
            // events are pure card-scoped observations, *not* track-level
            // authority claims, so we accept them from an AI-worker
            // spec-card actor as long as the scope matches the card's own
            // home (card_id + track + area cached values — same shape as
            // the Worker arm). Anything else from that actor is still
            // refused; write authority for spec-roled cards lives with
            // `AiSpec`. Note that `Event::TrackUpdated` is already gated
            // in section (2) above and unconditionally refuses any AI
            // worker actor, so this carveout cannot regress the
            // track-authority invariant.
            Some(CardRole::Spec) if is_own_worker_lifecycle_event(actor, event) => {
                enforce_card_self_scope(card_id, scope, cache, track_area_cache)?;
            }
            // PR3 invariant: spec cards are bound to AiSpec, not an AI
            // worker actor. Anything other than the hook carveout above
            // (which is a stateless bridge ingest path) from a
            // worker-variant spec-card actor is rejected.
            Some(CardRole::Spec) => {
                return Err(RoleViolation::NotSpecForTrack {
                    actor: format!(
                        "{} — card is Spec-roled but actor variant is not AiSpec",
                        ai_worker_actor_label(actor, card_id),
                    ),
                });
            }
            // Issue #679 PR7b-ii — ReportCard-bound actors have no
            // cross-card/track authority; mirror the Worker self-scope
            // rule for non-track-update/non-dispatch events.
            Some(CardRole::ReportCard) => {
                enforce_card_self_scope(card_id, scope, cache, track_area_cache)?;
            }
            // #1189 — Assistant cards are the Worker self-scope rule
            // loosened by exactly one card: their home track's report
            // card. Everything else (Track/Area/System scope, another
            // track's report card, someone else's worker card) is
            // refused, which is what pins "an assistant can neither
            // advance the lifecycle nor dispatch a task".
            Some(CardRole::Assistant) => {
                enforce_assistant_scope(card_id, scope, cache, track_area_cache)?;
            }
        }
    }

    // --- (4) User / Kernel / KernelDispatcher / Plugin: unrestricted. ---
    //
    // The match above already let them through. Documented here as a
    // gate decision, not as code, so the policy is greppable.

    Ok(())
}

/// Cross-check that `scope` describes the card's own home — `card`
/// matches, `track` matches the cached home track, `area` matches the
/// home track's persisted area. Shared between the Worker and ReportCard
/// arms (which use it for *every* event) and the Spec arm's `CodexHook`
/// carveout (bug A — the codex bridge ingest path for a spec card).
///
/// Returns `Err(RoleViolation::WorkerOutOfScope)` on any mismatch. The
/// variant name is historical (the check originated in the Worker
/// path); the semantic — "this AiCodex actor is writing outside its
/// own card scope" — applies equally to both call sites.
fn enforce_card_self_scope(
    card_id: &CardId,
    scope: &EventScope,
    cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
) -> Result<(), RoleViolation> {
    enforce_card_scope(
        card_id,
        scope,
        cache,
        track_area_cache,
        &|target, _home| target == card_id,
        &|card, scope| RoleViolation::WorkerOutOfScope { card, scope },
    )
}

/// #1189 — [`enforce_card_self_scope`] loosened by exactly one card.
///
/// An `Assistant`-roled card may write into its own card scope **or**
/// into the scope of its home track's report card (`role == ReportCard`
/// **and** same home track — the report card of *another* track is
/// refused). The `track` / `area` cross-checks are the same #232 / #234
/// anti-spoof checks the Worker arm runs, so an assistant can neither
/// fan an event out to a foreign track nor claim a foreign area.
///
/// Everything else is refused, including every non-`Card` scope. That
/// last clause is what pins the two §2 non-capabilities: `Track`-scoped
/// events (lifecycle transitions, dispatch requests) never reach an
/// assistant-authored write.
fn enforce_assistant_scope(
    card_id: &CardId,
    scope: &EventScope,
    cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
) -> Result<(), RoleViolation> {
    enforce_card_scope(
        card_id,
        scope,
        cache,
        track_area_cache,
        &|target, home_track| {
            target == card_id
                || (cache.get(target) == Some(CardRole::ReportCard)
                    && cache.track_of(target).as_ref() == Some(home_track))
        },
        &|card, scope| RoleViolation::AssistantOutOfScope { card, scope },
    )
}

/// Shared body of [`enforce_card_self_scope`] and
/// [`enforce_assistant_scope`]: the scope must be `EventScope::Card`,
/// its `card` must satisfy `target_allowed`, and its `track` / `area`
/// must match the acting card's home track and that track's persisted
/// area.
fn enforce_card_scope(
    card_id: &CardId,
    scope: &EventScope,
    cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
    target_allowed: &dyn Fn(&CardId, &TrackId) -> bool,
    violation: &dyn Fn(CardId, String) -> RoleViolation,
) -> Result<(), RoleViolation> {
    // `get()` (in the caller) and `track_of()` are two independent DashMap
    // lookups, so a card deleted between them makes `track_of` return
    // `None`. Every denial below therefore has to be reachable without a
    // successful `track_of`: the non-Card scopes are refused before it is
    // consulted, and the lookup itself is fail-closed (see below) so that
    // "Card scope naming the wrong card" — the out-of-bounds write path —
    // is a clean violation under the same delete race, exactly as it was
    // before the check was split into variant + target halves.
    let EventScope::Card {
        card: target,
        track: scope_track,
        area: scope_area,
    } = scope
    else {
        return Err(violation(
            card_id.clone(),
            format!("scope.card mismatch: {scope:?}"),
        ));
    };
    // Fail closed: the acting card losing its cache entry between the
    // caller's `get()` and this lookup means we can no longer prove the
    // scope is the card's own home, and "cannot prove" is a denial, never
    // a panic inside the kernel gate.
    let Some(home_track) = cache.track_of(card_id) else {
        return Err(violation(
            card_id.clone(),
            format!("scope.card mismatch: {scope:?}"),
        ));
    };
    if !target_allowed(target, &home_track) {
        return Err(violation(
            card_id.clone(),
            format!("scope.card mismatch: {scope:?}"),
        ));
    }
    // Target accepted. Now cross-check `scope.track` against the acting
    // card's immutable home track.
    if scope_track != &home_track {
        return Err(violation(
            card_id.clone(),
            format!("scope.track mismatch: home={home_track}, scope={scope:?}"),
        ));
    }
    // #234 — cross-check `scope.area` against the home track's persisted
    // area. The track→area cache is write-through-populated in
    // `track_create_tx`, so a missing entry under a known track id is a
    // hard invariant break worth failing loudly on (rather than the
    // silent "deny by default" of the role cache miss, which has its
    // own race-with-delete semantics covered elsewhere).
    let home_area = track_area_cache.area_of(&home_track).expect(
        "track_area_cache must be populated for any track with a known card — \
         track_create_tx writes through unconditionally",
    );
    if scope_area != &home_area {
        return Err(violation(
            card_id.clone(),
            format!("scope.area mismatch: home={home_area}, scope={scope:?}"),
        ));
    }
    Ok(())
}

fn ai_worker_actor_label(actor: &ActorId, card_id: &CardId) -> String {
    match actor {
        ActorId::AiCodex(_) => format!("AiCodex({card_id})"),
        ActorId::AiClaude(_) => format!("AiClaude({card_id})"),
        _ => unreachable!("only AI worker actors call ai_worker_actor_label"),
    }
}

fn is_own_worker_lifecycle_event(actor: &ActorId, event: &Event) -> bool {
    matches!(
        (actor, event),
        (ActorId::AiCodex(_), Event::CodexHook { .. })
            | (ActorId::AiClaude(_), Event::ClaudeHook { .. })
            | (
                ActorId::AiCodex(_) | ActorId::AiClaude(_),
                Event::HarnessUserMessageEnqueued { .. }
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AreaId, TrackId};
    use crate::model::{Area, AreaKind, Track, TrackLifecycle};

    fn track(id: &str, area: &str) -> Track {
        Track {
            id: TrackId::from(id),
            area_id: AreaId::from(area),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: TrackLifecycle::Draft,
            cwd_wire_alias: String::new(),
            template_id: None,
            plugin_scope: None,
            purpose: None,
            template_input: None,
            terminal_at: None,
            workspace: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card_scope(card: &str, track: &str, area: &str) -> EventScope {
        EventScope::Card {
            card: CardId::from(card),
            track: TrackId::from(track),
            area: AreaId::from(area),
        }
    }

    fn track_scope(track: &str, area: &str) -> EventScope {
        EventScope::Track {
            track: TrackId::from(track),
            area: AreaId::from(area),
        }
    }

    fn track_updated() -> Event {
        Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
            track("w", "c"),
            None,
        ))
    }

    fn area_updated() -> Event {
        Event::AreaUpdated(Area {
            id: AreaId::from("c"),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: AreaKind::User,
            created_at: 0,
            updated_at: 0,
        })
    }

    /// Pre-seeded track→area cache the Worker tests use: track `w` lives
    /// in area `c`. Tests that exercise mismatch paths override this
    /// per-test (#234).
    fn seeded_wcc() -> TrackAreaCache {
        let c = TrackAreaCache::new();
        c.insert(TrackId::from("w"), AreaId::from("c"));
        c
    }

    #[test]
    fn user_can_update_track() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::User,
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "user should be allowed to update track: {res:?}"
        );
    }

    #[test]
    fn kernel_can_update_track() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::Kernel,
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn ai_spec_with_spec_role_can_update_track() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let spec_id = CardId::from("spec-1");
        cache.insert(spec_id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(spec_id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiSpec(spec-card) should update track: {res:?}"
        );
    }

    #[test]
    fn ai_spec_without_spec_role_cannot_update_track() {
        // An AiSpec actor whose cached role is `Worker` (mismatch
        // between wire claim + persisted truth) is denied.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let id = CardId::from("c1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::NotSpecForTrack { .. })));
    }

    #[test]
    fn ai_codex_cannot_update_track_even_with_known_card() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForTrack { .. })),
            "AiCodex must never emit track.updated regardless of role: {res:?}",
        );
    }

    /// Belt-and-suspenders companion to the Worker test above: the
    /// CodexHook carveout added for spec cards must not let `TrackUpdated`
    /// through. Section 2 (`TrackUpdated` is spec-only via `AiSpec`) runs
    /// before section 3's `Some(CardRole::Spec) if CodexHook` arm, so
    /// the invariant is structural — this test pins it explicitly so a
    /// future refactor that reorders the sections can't silently regress
    /// it.
    #[test]
    fn spec_codex_cannot_update_track() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForTrack { .. })),
            "AiCodex(spec_card) must still be refused on track.updated even after the CodexHook carveout: {res:?}",
        );
    }

    #[test]
    fn worker_in_card_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        // Worker's home track is "w" — scope below must use the same.
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            // A non-track-updated event (AreaUpdated chosen because it
            // also has no card semantics — but the scope is what we
            // assert on, the event variant is irrelevant after the
            // track-updated branch). Use a card-scoped event:
            // OverlaySet would also work; AreaUpdated lets us exercise
            // the scope check independent of payload shape.
            &area_updated(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "worker in own card scope: {res:?}");
    }

    #[test]
    fn worker_out_of_card_scope_rejected() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        // Track scope when caller is a worker → reject.
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &area_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::WorkerOutOfScope { .. })));
    }

    #[test]
    fn worker_in_different_card_scope_rejected() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &area_updated(),
            &card_scope("not-my-card", "w", "c"),
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::WorkerOutOfScope { .. })));
    }

    #[test]
    fn worker_with_mismatched_scope_track_rejected() {
        // Issue #232: even with `scope.card == self`, the gate must
        // reject a `scope.track` that doesn't match the Worker card's
        // home track. Without this check, a Worker could forge any
        // track id and the kernel would route the event to that track's
        // subscribers.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("home-track"), AreaId::from("c"));
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("home-track"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &area_updated(),
            // Same card, but a different track — must reject.
            &card_scope(id.as_str(), "other-track", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(
                res,
                Err(RoleViolation::WorkerOutOfScope { ref scope, .. })
                    if scope.contains("scope.track mismatch")
            ),
            "Worker forging scope.track must be refused: {res:?}",
        );
    }

    #[test]
    fn worker_with_mismatched_scope_area_rejected() {
        // Issue #234: even with `scope.card == self` and
        // `scope.track == home_track`, the gate must reject a
        // `scope.area` that doesn't match the home track's persisted
        // area. Without this check, a Worker could forge any area id
        // and the kernel would route the event to that area's
        // subscribers — cross-area isolation break, same shape as #232
        // one level up.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("home-track"), AreaId::from("home-area"));
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("home-track"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &area_updated(),
            // Same card + same track, but a different area — must
            // reject before the event row lands.
            &card_scope(id.as_str(), "home-track", "forged-area"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(
                res,
                Err(RoleViolation::WorkerOutOfScope { ref scope, .. })
                    if scope.contains("scope.area mismatch")
            ),
            "Worker forging scope.area must be refused: {res:?}",
        );
    }

    #[test]
    fn empty_codex_card_id_rejected() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("")),
            &area_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn empty_aispec_card_id_rejected() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiSpec(CardId::from("")),
            &area_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn unknown_codex_card_rejected() {
        // Defense-in-depth: an AiCodex actor whose card is not in the
        // cache is denied. Covers two real cases — card was deleted
        // between request and gate, or the id was fabricated.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("never-seen")),
            &area_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::UnknownCard { .. })));
    }

    /// Build a `CodexHook` event payload — used by the bug-A carveout
    /// tests below. Shape mirrors what `routes::codex::ingest_hook`
    /// constructs (kind=`hook.codex.<event_name>`, opaque payload).
    fn codex_hook(card: &str) -> Event {
        Event::CodexHook {
            card_id: CardId::from(card),
            kind: "hook.codex.permission_request".into(),
            hook_idempotency_key: "hook-codex".into(),
            payload: serde_json::json!({}),
        }
    }

    fn claude_hook(card: &str) -> Event {
        Event::ClaudeHook {
            card_id: CardId::from(card),
            kind: "hook.claude.pre_tool_use".into(),
            hook_idempotency_key: "hook-claude".into(),
            payload: serde_json::json!({}),
        }
    }

    fn harness_user_message_enqueued(card: &str, track: &str) -> Event {
        Event::HarnessUserMessageEnqueued {
            runtime_id: "rt-1".into(),
            card_id: CardId::from(card),
            track_id: TrackId::from(track),
            char_count: 3,
        }
    }

    #[test]
    fn spec_codex_hook_in_own_scope_ok() {
        // Bug A regression unit. The codex bridge runs as a subprocess
        // of codex regardless of the card's role; for a spec card, the
        // bridge still surfaces hook events through the
        // `AiCodex(spec_card)` actor. The gate accepts `Event::CodexHook`
        // from that actor as a pure lifecycle observation, scoped to the
        // card's own home (card_id + track + area). Mirror of
        // `worker_in_card_scope_ok` for the Spec arm.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_hook(id.as_str()),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiCodex(spec) CodexHook in own card scope should be accepted: {res:?}",
        );
    }

    #[test]
    fn spec_codex_harness_user_message_in_own_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &harness_user_message_enqueued(id.as_str(), "w"),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiCodex(spec) HarnessUserMessageEnqueued in own card scope should be accepted: {res:?}",
        );
    }

    #[test]
    fn spec_codex_non_hook_event_still_rejected() {
        // The Spec-arm carveout is intentionally limited to pure
        // lifecycle observations. Anything else from
        // `AiCodex(spec_card)` is still refused — write authority for
        // spec-roled cards lives with `AiSpec`, not `AiCodex`.
        // AreaUpdated chosen because it's outside that lifecycle set and
        // is not a track-updated event variant.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &area_updated(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForTrack { .. })),
            "AiCodex(spec) non-hook event must still be refused: {res:?}",
        );
    }

    #[test]
    fn spec_codex_hook_out_of_scope_rejected() {
        // The carveout reuses the same scope cross-check as the Worker
        // arm — an `AiCodex(spec_card)` CodexHook with a forged track id
        // is still refused. This pins that the new helper is wired into
        // the Spec arm, not just nominally accepted.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("home-track"), AreaId::from("c"));
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("home-track"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_hook(id.as_str()),
            // Same card, but a different track — must reject even on
            // the carveout path.
            &card_scope(id.as_str(), "other-track", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(
                res,
                Err(RoleViolation::WorkerOutOfScope { ref scope, .. })
                    if scope.contains("scope.track mismatch")
            ),
            "AiCodex(spec) CodexHook with forged scope.track must be refused: {res:?}",
        );
    }

    #[test]
    fn ai_claude_cannot_update_track_even_with_known_card() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("claude-worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForTrack { .. })),
            "AiClaude must never emit track.updated regardless of role: {res:?}",
        );
    }

    #[test]
    fn claude_worker_in_card_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("claude-worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id.clone()),
            &area_updated(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "Claude worker in own card scope: {res:?}");
    }

    #[test]
    fn claude_worker_out_of_card_scope_rejected() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("claude-worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id),
            &area_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::WorkerOutOfScope { .. })));
    }

    #[test]
    fn spec_claude_hook_in_own_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-claude-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id.clone()),
            &claude_hook(id.as_str()),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiClaude(spec) ClaudeHook in own card scope should be accepted: {res:?}",
        );
    }

    #[test]
    fn empty_claude_card_id_rejected() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiClaude(CardId::from("")),
            &area_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn plugin_actor_unrestricted() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::Plugin("hello-world".into()),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn kernel_dispatcher_unrestricted() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::KernelDispatcher,
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok());
    }

    // ---- PR4 of #136: new Event variants flow through enforce_role ------
    //
    // PR4 was schema-only — but the dispatcher (PR5) and its push
    // delivery path (#293) rely on the gate's existing logic to route +
    // authorize them. These tests lock in that behavior:
    //
    //   * a worker card emitting `codex.worker_requested` within its own
    //     card scope is permitted (PR5's job request fan-out path);
    //   * a worker card emitting `task.completed` within its own card
    //     scope is permitted (the dispatcher push delivery path);
    //   * an AiSpec actor with an empty CardId is rejected via the
    //     section-1 guard, even when the payload is a new variant — the
    //     guard is variant-agnostic by design;
    //   * the same goes for AiCodex with empty CardId.
    //
    // None of these write paths exist in PR4. The tests are forward-only:
    // they assert what the gate *will* permit/reject when PR5 starts
    // emitting these variants, so PR5 doesn't have to re-discover the
    // contract from scratch.

    use crate::event::ArtifactRef;

    fn codex_worker_requested() -> Event {
        Event::CodexWorkerRequested {
            idempotency_key: "idem-1".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        }
    }

    fn terminal_worker_requested() -> Event {
        Event::TerminalWorkerRequested {
            idempotency_key: "idem-1".into(),
            cmd: "echo hi".into(),
            cwd: None,
            agent_message: None,
        }
    }

    fn task_completed() -> Event {
        Event::TaskCompleted {
            idempotency_key: "idem-1".into(),
            result: serde_json::Value::Null,
            artifacts: vec![ArtifactRef::from("a-1")],
            agent_message: None,
        }
    }

    fn review_round() -> Event {
        Event::ReviewRound {
            track_id: TrackId::from("w"),
            subject: crate::event::ReviewSubject {
                phase: "impl".into(),
                slice_id: "5b".into(),
                pr_number: Some(760),
            },
            head_sha: Some("abc123".into()),
            n: 1,
            cap: 3,
            converged: true,
            channels: vec![
                crate::event::ChannelVerdict {
                    role: "reviewer-a".into(),
                    verdict: crate::event::ChannelVerdictKind::Approved,
                },
                crate::event::ChannelVerdict {
                    role: "reviewer-b".into(),
                    verdict: crate::event::ChannelVerdictKind::Approved,
                },
            ],
            root_cause: None,
            idempotency_key: "review.round:w:impl:5b:760:1".into(),
        }
    }

    fn ratify_requested() -> Event {
        Event::RatifyRequested {
            track_id: TrackId::from("w"),
            reason: "cap_exhausted".into(),
        }
    }

    fn ratify_resolved_grant() -> Event {
        Event::RatifyResolved {
            track_id: TrackId::from("w"),
            decision: crate::event::RatifyDecision::Grant,
        }
    }

    fn session_actors() -> [(ActorId, &'static str); 3] {
        [
            (
                ActorId::AiSpecSession(WorkerSessionId::from("sess-spec")),
                "sess-spec",
            ),
            (
                ActorId::AiCodexSession(WorkerSessionId::from("sess-codex")),
                "sess-codex",
            ),
            (
                ActorId::AiClaudeSession(WorkerSessionId::from("sess-claude")),
                "sess-claude",
            ),
        ]
    }

    fn assert_session_unresolved(
        res: Result<(), RoleViolation>,
        expected_session: &str,
        context: &str,
    ) {
        match res {
            Err(RoleViolation::SessionActorUnresolved { session }) => {
                assert_eq!(
                    session,
                    WorkerSessionId::from(expected_session),
                    "{context}"
                );
            }
            other => panic!("{context}: expected SessionActorUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn session_actors_are_deny_closed_for_sync_role_gate() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let dispatch = codex_worker_requested();
        let worker_event = task_completed();

        for (actor, session) in session_actors() {
            assert_session_unresolved(
                enforce_role(
                    &actor,
                    &track_updated(),
                    &track_scope("w", "c"),
                    &cache,
                    &wcc,
                ),
                session,
                "track.updated must deny unresolved session actor",
            );
            assert_session_unresolved(
                enforce_role(&actor, &dispatch, &track_scope("w", "c"), &cache, &wcc),
                session,
                "dispatch request must deny unresolved session actor",
            );
            assert_session_unresolved(
                enforce_role(
                    &actor,
                    &worker_event,
                    &card_scope("worker-1", "w", "c"),
                    &cache,
                    &wcc,
                ),
                session,
                "card-scoped worker event must deny unresolved session actor",
            );
        }
    }

    #[test]
    fn empty_session_actor_id_is_rejected_as_unresolved() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiCodexSession(WorkerSessionId::from("")),
            &area_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert_session_unresolved(
            res,
            "",
            "empty session actor id must use session-unresolved denial",
        );
    }

    #[test]
    fn worker_cannot_emit_codex_worker_requested_after_583() {
        // Issue #583. Section (2.5) of `enforce_role` now rejects any
        // Worker-actor `CodexWorkerRequested` regardless of scope. Replaces
        // the pre-#583 positive `worker_can_emit_codex_worker_requested_in_own_scope`
        // which encoded the leaky pre-#583 behavior.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let err = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_worker_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("worker AI actor must be refused codex.worker_requested");
        assert!(
            matches!(err, RoleViolation::NotSpecForDispatch { .. }),
            "expected NotSpecForDispatch, got {err:?}",
        );
    }

    #[test]
    fn worker_cannot_emit_terminal_worker_requested_after_583() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let err = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &terminal_worker_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("worker AI actor must be refused terminal.worker_requested");
        assert!(
            matches!(err, RoleViolation::NotSpecForDispatch { .. }),
            "expected NotSpecForDispatch, got {err:?}",
        );
    }

    #[test]
    fn spec_can_emit_codex_worker_requested_in_own_scope() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(id.clone()),
            &codex_worker_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "spec emitting codex.worker_requested: {res:?}");
    }

    #[test]
    fn worker_cannot_emit_plan_updated_644() {
        // Issue #644. `plan.updated` joins the section-(2.5) spec-only
        // list: a worker AI actor must not commit task-plan revisions
        // (the PR-B scheduler dispatches whatever the plan says, so this
        // would be the #583 recursive-mint hole one hop removed).
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let err = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &Event::PlanUpdated {
                track_id: TrackId::from("w"),
                changed_keys: vec!["impl-parser".into()],
                agent_message: None,
            },
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("worker AI actor must be refused plan.updated");
        assert!(
            matches!(err, RoleViolation::NotSpecForDispatch { .. }),
            "expected NotSpecForDispatch, got {err:?}",
        );
    }

    #[test]
    fn spec_can_emit_plan_updated_in_own_track() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(id.clone()),
            &Event::PlanUpdated {
                track_id: TrackId::from("w"),
                changed_keys: vec!["impl-parser".into()],
                agent_message: None,
            },
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "spec emitting plan.updated: {res:?}");
    }

    #[test]
    fn task_dispatched_is_kernel_only_644_pr_b() {
        // Issue #644 PR-B. `task.dispatched` is the scheduler's claim
        // record — every card-derived actor is refused, spec included
        // (it is a kernel observation, not a card authority), while the
        // kernel families pass.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let spec = CardId::from("spec-1");
        let worker = CardId::from("worker-1");
        cache.insert(spec.clone(), CardRole::Spec, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = Event::TaskDispatched {
            idempotency_key: "w:impl-parser".into(),
            kind: "codex".into(),
            agent_message: None,
        };

        for (actor, label) in [
            (ActorId::AiSpec(spec.clone()), "AiSpec(spec)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (ActorId::Plugin("p".into()), "Plugin(p)"),
        ] {
            let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                .expect_err(&format!("{label} must be refused task.dispatched"));
            assert!(
                matches!(err, RoleViolation::NotKernelForTaskDispatched { .. }),
                "{label}: expected NotKernelForTaskDispatched, got {err:?}",
            );
        }

        for actor in [ActorId::User, ActorId::Kernel, ActorId::KernelDispatcher] {
            let res = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc);
            assert!(res.is_ok(), "{actor:?} emitting task.dispatched: {res:?}");
        }
    }

    #[test]
    fn task_gate_result_is_kernel_only_644_pr_c() {
        // Issue #644 PR-C. `task.gate_result` is the gate runner's
        // machine verdict — every card-derived actor (spec included)
        // and plugins are refused; the kernel families pass.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let spec = CardId::from("spec-1");
        let worker = CardId::from("worker-1");
        cache.insert(spec.clone(), CardRole::Spec, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = Event::TaskGateResult {
            task_id: "w:impl-parser".into(),
            idempotency_key: "w:impl-parser".into(),
            passed: true,
            failing_step: None,
            exit_code: Some(0),
            log_tail: String::new(),
            log_path: "/tmp/gate.log".into(),
            attempt: 1,
            agent_message: None,
        };

        for (actor, label) in [
            (ActorId::AiSpec(spec.clone()), "AiSpec(spec)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (ActorId::Plugin("p".into()), "Plugin(p)"),
        ] {
            let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                .expect_err(&format!("{label} must be refused task.gate_result"));
            assert!(
                matches!(err, RoleViolation::NotKernelForTaskGateResult { .. }),
                "{label}: expected NotKernelForTaskGateResult, got {err:?}",
            );
        }

        for actor in [ActorId::User, ActorId::Kernel, ActorId::KernelDispatcher] {
            let res = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc);
            assert!(res.is_ok(), "{actor:?} emitting task.gate_result: {res:?}");
        }
    }

    #[test]
    fn task_context_frozen_is_kernel_only_985_pr3a() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let worker = CardId::from("worker-1");
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = Event::TaskContextFrozen {
            track_id: TrackId::default(),
            task_key: String::new(),
            idempotency_key: String::new(),
            task_id: "w:legacy".into(),
            refs: Vec::new(),
            doc_revs: Default::default(),
            truncated: false,
        };
        let err = enforce_role(
            &ActorId::AiCodex(worker),
            &event,
            &track_scope("w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("worker must not forge task.context_frozen");
        assert!(matches!(
            err,
            RoleViolation::NotKernelForTaskContextFrozen { .. }
        ));
        let err = enforce_role(&ActorId::User, &event, &track_scope("w", "c"), &cache, &wcc)
            .expect_err("User must not forge task.context_frozen");
        assert!(matches!(
            err,
            RoleViolation::NotKernelForTaskContextFrozen { .. }
        ));
    }

    #[test]
    fn task_context_advanced_is_kernel_only_985_pr3a() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let event = Event::TaskContextAdvanced {
            track_id: Default::default(),
            task_key: String::new(),
            task_id: "w:legacy".into(),
            changed_refs: Vec::new(),
            verdict: "material".into(),
            rationale: String::new(),
        };
        let err = enforce_role(
            &ActorId::Plugin("forger".into()),
            &event,
            &track_scope("w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("plugin must not forge task.context_advanced");
        assert!(matches!(
            err,
            RoleViolation::NotKernelForTaskContextAdvanced { .. }
        ));
        let err = enforce_role(&ActorId::User, &event, &track_scope("w", "c"), &cache, &wcc)
            .expect_err("User must not forge task.context_advanced");
        assert!(matches!(
            err,
            RoleViolation::NotKernelForTaskContextAdvanced { .. }
        ));
    }

    #[test]
    fn review_round_and_ratify_requested_are_spec_only_760() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let spec = CardId::from("spec-1");
        let worker = CardId::from("worker-1");
        cache.insert(spec.clone(), CardRole::Spec, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));

        for event in [review_round(), ratify_requested()] {
            let res = enforce_role(
                &ActorId::AiSpec(spec.clone()),
                &event,
                &track_scope("w", "c"),
                &cache,
                &wcc,
            );
            assert!(
                res.is_ok(),
                "spec should emit {}: {res:?}",
                event.kind_tag()
            );

            for (actor, label) in [
                (ActorId::Plugin("p".into()), "Plugin(p)"),
                (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
                (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
                (ActorId::User, "User"),
                (ActorId::Kernel, "Kernel"),
                (ActorId::KernelDispatcher, "KernelDispatcher"),
                (
                    ActorId::AiSpecSession(WorkerSessionId::from("sess-unresolved")),
                    "AiSpecSession(unresolved)",
                ),
            ] {
                let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                    .expect_err(&format!("{label} must be refused {}", event.kind_tag()));
                assert!(
                    matches!(err, RoleViolation::NotSpecForReviewRatify { .. }),
                    "{label}: expected NotSpecForReviewRatify, got {err:?}",
                );
            }
        }
    }

    #[test]
    fn ratify_resolved_is_user_only_760() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let spec = CardId::from("spec-1");
        let worker = CardId::from("worker-1");
        cache.insert(spec.clone(), CardRole::Spec, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = ratify_resolved_grant();

        let res = enforce_role(&ActorId::User, &event, &track_scope("w", "c"), &cache, &wcc);
        assert!(res.is_ok(), "User should emit ratify.resolved: {res:?}");

        for (actor, label) in [
            (ActorId::AiSpec(spec.clone()), "AiSpec(spec)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (ActorId::Plugin("p".into()), "Plugin(p)"),
            (ActorId::Kernel, "Kernel"),
            (ActorId::KernelDispatcher, "KernelDispatcher"),
            (
                ActorId::AiSpecSession(WorkerSessionId::from("sess-spec")),
                "AiSpecSession(unresolved)",
            ),
        ] {
            let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                .expect_err(&format!("{label} must be refused ratify.resolved"));
            assert!(
                matches!(err, RoleViolation::NotUserForRatifyResolved { .. }),
                "{label}: expected NotUserForRatifyResolved, got {err:?}",
            );
        }
    }

    #[test]
    fn worker_can_emit_task_completed_in_own_scope() {
        // The dispatcher push delivery path: workers report
        // task.completed scoped to themselves.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &task_completed(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "worker reporting its own task completion: {res:?}",
        );
    }

    #[test]
    fn reportcard_can_emit_task_completed_in_own_scope() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("report-1");
        cache.insert(id.clone(), CardRole::ReportCard, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &task_completed(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "report card actor writing its own card scope should stay allowed: {res:?}",
        );
    }

    #[test]
    fn reportcard_task_completed_cross_card_rejected() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("report-1");
        cache.insert(id.clone(), CardRole::ReportCard, TrackId::from("w"));
        let err = enforce_role(
            &ActorId::AiCodex(id),
            &task_completed(),
            &card_scope("worker-1", "w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("AiCodex(ReportCard) cross-card task.completed must be refused");
        assert!(
            matches!(&err, RoleViolation::WorkerOutOfScope { .. }),
            "expected out-of-scope violation, got {err:?}",
        );
        assert!(
            err.to_string().contains("out of scope"),
            "denial must surface out-of-scope text, got {err}",
        );
    }

    #[test]
    fn reportcard_task_completed_cross_track_rejected() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("home-track"), AreaId::from("c"));
        let id = CardId::from("report-1");
        cache.insert(
            id.clone(),
            CardRole::ReportCard,
            TrackId::from("home-track"),
        );
        let err = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &task_completed(),
            &card_scope(id.as_str(), "other-track", "c"),
            &cache,
            &wcc,
        )
        .expect_err("AiCodex(ReportCard) cross-track task.completed must be refused");
        assert!(
            matches!(
                &err,
                RoleViolation::WorkerOutOfScope { scope, .. }
                    if scope.contains("scope.track mismatch")
            ),
            "expected scope.track out-of-scope violation, got {err:?}",
        );
        assert!(
            err.to_string().contains("out of scope"),
            "denial must surface out-of-scope text, got {err}",
        );
    }

    #[test]
    fn empty_codex_card_id_rejected_on_new_variant() {
        // The section-1 empty-CardId guard is variant-agnostic — it
        // refuses any payload from an AiCodex actor whose CardId is
        // empty, including the new PR4 variants. Locks the contract so
        // a future refactor can't accidentally route the empty case
        // around the guard for a "harmless" new variant.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("")),
            &task_completed(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn empty_aispec_card_id_rejected_on_new_variant() {
        // Mirror of the AiCodex case for AiSpec — when PR5 wires the
        // spec card as the requester of codex.worker_requested, the empty
        // CardId path must still be rejected.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiSpec(CardId::from("")),
            &codex_worker_requested(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    // ---- Issue #955 §5.4: proposal-channel hard clauses ------------------

    use calm_types::proposal::{ProposalDecision, ProposalOp};

    fn proposal_submitted(plugin: &str) -> Event {
        Event::ProposalSubmitted {
            track_id: TrackId::from("w"),
            proposal_id: "pp-1".into(),
            plugin_id: plugin.into(),
            subject_kind: "report".into(),
            base_doc_heads: "ah1:deadbeef".into(),
            ops: vec![ProposalOp::DeleteBlock {
                block_id: "b_0001".into(),
                if_rev: 1,
            }],
            note: "why".into(),
            idem_key: "idem-1".into(),
        }
    }

    fn proposal_resolved(plugin: &str, decision: ProposalDecision) -> Event {
        Event::ProposalResolved {
            track_id: TrackId::from("w"),
            proposal_id: "pp-1".into(),
            plugin_id: plugin.into(),
            decision,
        }
    }

    #[test]
    fn proposal_submitted_allows_only_the_named_plugin() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let event = proposal_submitted("dev.neige.invest");

        // The submitting plugin itself passes.
        let res = enforce_role(
            &ActorId::Plugin("dev.neige.invest".into()),
            &event,
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "submitting plugin must pass: {res:?}");

        // A DIFFERENT plugin is refused — actor/payload id mismatch.
        let err = enforce_role(
            &ActorId::Plugin("dev.neige.other".into()),
            &event,
            &track_scope("w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("mismatched plugin must be refused proposal.submitted");
        assert!(
            matches!(
                err,
                RoleViolation::NotSubmitterPluginForProposalSubmitted { .. }
            ),
            "expected NotSubmitterPluginForProposalSubmitted, got {err:?}",
        );
    }

    #[test]
    fn proposal_submitted_denies_every_non_plugin_actor() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let spec = CardId::from("spec-1");
        let worker = CardId::from("worker-1");
        cache.insert(spec.clone(), CardRole::Spec, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = proposal_submitted("dev.neige.invest");

        for (actor, label) in [
            (ActorId::User, "User"),
            (ActorId::Kernel, "Kernel"),
            (ActorId::KernelDispatcher, "KernelDispatcher"),
            (ActorId::AiSpec(spec.clone()), "AiSpec(spec)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (
                ActorId::AiSpecSession(WorkerSessionId::from("sess-spec")),
                "AiSpecSession",
            ),
        ] {
            let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                .expect_err(&format!("{label} must be refused proposal.submitted"));
            assert!(
                matches!(
                    err,
                    RoleViolation::NotSubmitterPluginForProposalSubmitted { .. }
                ),
                "{label}: expected NotSubmitterPluginForProposalSubmitted, got {err:?}",
            );
        }
    }

    #[test]
    fn proposal_resolved_adjudications_are_user_only() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let spec = CardId::from("spec-1");
        let worker = CardId::from("worker-1");
        cache.insert(spec.clone(), CardRole::Spec, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));

        for decision in [
            ProposalDecision::Accepted,
            ProposalDecision::Rejected,
            ProposalDecision::Stale,
        ] {
            let event = proposal_resolved("dev.neige.invest", decision);

            let res = enforce_role(&ActorId::User, &event, &track_scope("w", "c"), &cache, &wcc);
            assert!(
                res.is_ok(),
                "User must resolve {}: {res:?}",
                decision.as_str()
            );

            // Everyone else is refused — INCLUDING the submitting
            // plugin (no self-approval, §5.1 authority model).
            for (actor, label) in [
                (
                    ActorId::Plugin("dev.neige.invest".into()),
                    "Plugin(submitter)",
                ),
                (ActorId::Plugin("dev.neige.other".into()), "Plugin(other)"),
                (ActorId::Kernel, "Kernel"),
                (ActorId::KernelDispatcher, "KernelDispatcher"),
                (ActorId::AiSpec(spec.clone()), "AiSpec(spec)"),
                (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
                (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
                (
                    ActorId::AiSpecSession(WorkerSessionId::from("sess-spec")),
                    "AiSpecSession",
                ),
            ] {
                let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                    .expect_err(&format!(
                        "{label} must be refused proposal.resolved{{{}}}",
                        decision.as_str()
                    ));
                assert!(
                    matches!(err, RoleViolation::NotUserForProposalResolved { .. }),
                    "{label}/{}: expected NotUserForProposalResolved, got {err:?}",
                    decision.as_str(),
                );
            }
        }
    }

    #[test]
    fn proposal_withdrawn_is_submitter_plugin_only() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let spec = CardId::from("spec-1");
        let worker = CardId::from("worker-1");
        cache.insert(spec.clone(), CardRole::Spec, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = proposal_resolved("dev.neige.invest", ProposalDecision::Withdrawn);

        // The submitter reclaims its own pending slot.
        let res = enforce_role(
            &ActorId::Plugin("dev.neige.invest".into()),
            &event,
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "submitter must withdraw: {res:?}");

        // Everyone else is refused — including the USER (withdraw is
        // the plugin's exit; the user's exits are reject/accept) and a
        // different plugin.
        for (actor, label) in [
            (ActorId::User, "User"),
            (ActorId::Plugin("dev.neige.other".into()), "Plugin(other)"),
            (ActorId::Kernel, "Kernel"),
            (ActorId::KernelDispatcher, "KernelDispatcher"),
            (ActorId::AiSpec(spec.clone()), "AiSpec(spec)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (
                ActorId::AiSpecSession(WorkerSessionId::from("sess-spec")),
                "AiSpecSession",
            ),
        ] {
            let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                .expect_err(&format!(
                    "{label} must be refused proposal.resolved{{withdrawn}}"
                ));
            assert!(
                matches!(
                    err,
                    RoleViolation::NotSubmitterPluginForProposalWithdrawn { .. }
                ),
                "{label}: expected NotSubmitterPluginForProposalWithdrawn, got {err:?}",
            );
        }
    }

    /// #1189 review round 2 — the delete race must be a *denial* on every
    /// branch, not a panic on one of them.
    ///
    /// `enforce_role` looks the acting card up with `cache.get()`; this
    /// helper then looks the same card up again with `cache.track_of()`.
    /// The two are independent DashMap lookups, so a card deleted in
    /// between makes the second one return `None`. Before the check was
    /// split into "scope variant" + "target card" halves, BOTH refusals
    /// were reached before `track_of` was ever consulted, so neither could
    /// blow up under that race — the split must not cost that.
    ///
    /// The empty cache below is exactly that race's end state (the entry
    /// is simply gone), so every scope shape must come back `Err`. A
    /// `.expect()` on `track_of` fails this test by panicking on the
    /// Card-scope rows.
    #[test]
    fn card_scope_is_fail_closed_when_the_acting_card_vanished() {
        let vanished = CardRoleCache::new();
        let wcc = seeded_wcc();
        let acting = CardId::from("worker-1");
        let self_only = |target: &CardId, _home: &TrackId| target == &acting;
        for scope in [
            // Non-Card scope — refused before `track_of` in every version.
            track_scope("w", "c"),
            // Card scope naming someone else's card: the actual
            // out-of-bounds write path, and the one the split regressed.
            card_scope("someone-elses-card", "w", "c"),
            // Card scope naming the acting card itself — still
            // unprovable once the cache entry is gone.
            card_scope("worker-1", "w", "c"),
        ] {
            let result = enforce_card_scope(
                &acting,
                &scope,
                &vanished,
                &wcc,
                &self_only,
                &|card, scope| RoleViolation::WorkerOutOfScope { card, scope },
            );
            assert!(
                matches!(result, Err(RoleViolation::WorkerOutOfScope { .. })),
                "a vanished acting card must deny {scope:?}, got {result:?}"
            );
        }
    }
}
