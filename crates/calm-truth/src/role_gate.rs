//! Authorization gate at the single write entry, run inside `write_with_event`
//! before the event row commits; a violation rolls the txn back. Deny is the
//! default for anything ambiguous, and role lookups are re-confirmed against
//! the in-process `CardRoleCache` rather than the actor's claimed identity.

use crate::card_role_cache::CardRoleCache;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::CardRole;
use crate::track_area_cache::TrackAreaCache;
use crate::worker::WorkerSessionId;
use calm_types::proposal::ProposalDecision;
use thiserror::Error;

/// Surfaced verbatim into `CalmError::Forbidden` so tests can pattern-match.
#[derive(Debug, Error)]
pub enum RoleViolation {
    #[error(
        "AiCodex/AiClaude/AiPlanner actor has empty card id (likely from legacy AI header path)"
    )]
    EmptyAiCardId,

    /// A session-keyed actor reached the sync gate unresolved; denied.
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
        "session {session} claims planner authority but its resolved card {card} is not Planner-roled (or unknown); denied fail-closed (#770)."
    )]
    SessionPlannerRoleMismatch {
        session: WorkerSessionId,
        card: CardId,
    },

    #[error("only planner cards (or User/Kernel) may emit track.updated (actor={actor})")]
    NotPlannerForTrack { actor: String },

    #[error("only planner cards (or User/Kernel) may emit dispatch-request events (actor={actor})")]
    NotPlannerForDispatch { actor: String },

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

    #[error("task.execution_settled requires a kernel actor (actor={actor})")]
    NotKernelForTaskExecutionSettled { actor: String },

    #[error(
        "task.gate_result is a kernel-only gate-runner record; no card-derived actor may emit it (actor={actor})"
    )]
    NotKernelForTaskGateResult { actor: String },

    #[error("only planner cards may emit review/ratify request events (actor={actor})")]
    NotPlannerForReviewRatify { actor: String },

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

    /// An `Assistant`-roled card wrote outside the two card scopes it owns
    /// (itself, and its home track's report card).
    #[error("assistant card {card} is out of scope {scope}")]
    AssistantOutOfScope { card: CardId, scope: String },

    #[error(
        "AI worker actor references card {card} that the role cache does not know — \
         card was likely deleted or never minted; denying by default"
    )]
    UnknownCard { card: CardId },

    /// A role/track/area lookup the gate needs could not be read, or read
    /// nothing. "Cannot prove" is a denial.
    #[error("role lookup failed for {subject}; denying by default")]
    RoleLookupFailed { subject: String },
}

/// Run the role gate; the caller turns `Err` into a rollback. Side-effect-free:
/// never mutates the cache, never reads the database.
pub fn enforce_role(
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
    cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
) -> Result<(), RoleViolation> {
    // (1) Empty-CardId guard: an empty id must never match a real card.
    if let ActorId::AiCodex(c) | ActorId::AiClaude(c) | ActorId::AiPlanner(c) = actor
        && c.as_str().is_empty()
    {
        return Err(RoleViolation::EmptyAiCardId);
    }
    if let ActorId::AiPlannerSession(s) | ActorId::AiCodexSession(s) | ActorId::AiClaudeSession(s) =
        actor
        && s.as_str().is_empty()
    {
        return Err(RoleViolation::SessionActorUnresolved { session: s.clone() });
    }

    // (2) `TrackUpdated` is planner-only; User/Kernel keep unrestricted authority.
    if matches!(event, Event::TrackUpdated(_)) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(_) => {
                // Plugins are unrestricted here.
            }
            ActorId::AiPlanner(card_id) => {
                let role = cache.get(card_id);
                if role != Some(CardRole::Planner) {
                    return Err(RoleViolation::NotPlannerForTrack {
                        actor: format!("AiPlanner({card_id})"),
                    });
                }
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                // The actor variant is the wire-level claim; the gate sticks to it rather
                // than re-binding via the cache.
                return Err(RoleViolation::NotPlannerForTrack {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiPlannerSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // (2.5) Dispatch-request + plan-revision events are planner-only: a worker
    // actor committing one could mint a recursive worker tree.
    if matches!(
        event,
        Event::CodexWorkerRequested { .. }
            | Event::TerminalWorkerRequested { .. }
            | Event::PlanUpdated { .. }
    ) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(_) => {}
            ActorId::AiPlanner(card_id) => {
                let role = cache.get(card_id);
                if role != Some(CardRole::Planner) {
                    return Err(RoleViolation::NotPlannerForDispatch {
                        actor: format!("AiPlanner({card_id})"),
                    });
                }
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotPlannerForDispatch {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiPlannerSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // (2.6) `task.dispatched` is a kernel observation, not a card authority:
    // narrower than (2)/(2.5), plugins are refused too.
    if matches!(event, Event::TaskDispatched { .. }) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(name) => {
                return Err(RoleViolation::NotKernelForTaskDispatched {
                    actor: format!("Plugin({name})"),
                });
            }
            ActorId::AiPlanner(card_id) => {
                return Err(RoleViolation::NotKernelForTaskDispatched {
                    actor: format!("AiPlanner({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskDispatched {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiPlannerSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // Context freeze/advancement records are strict kernel facts: even a plain
    // User cannot forge them.
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
            ActorId::AiPlanner(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextFrozen {
                    actor: format!("AiPlanner({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextFrozen {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiPlannerSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    if matches!(
        event,
        Event::TaskExecutionSettled { .. }
            | Event::TaskCandidateVerificationSettled { .. }
            | Event::TaskGitDeliverySettled { .. }
            | Event::TaskFilePublicationSettled { .. }
    ) && !matches!(actor, ActorId::Kernel | ActorId::KernelDispatcher)
    {
        return Err(RoleViolation::NotKernelForTaskExecutionSettled {
            actor: format!("{actor:?}"),
        });
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
            ActorId::AiPlanner(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextAdvanced {
                    actor: format!("AiPlanner({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskContextAdvanced {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiPlannerSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // (2.7) `task.gate_result` is the kernel's machine verdict; same narrow gate as (2.6).
    if matches!(event, Event::TaskGateResult { .. }) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(name) => {
                return Err(RoleViolation::NotKernelForTaskGateResult {
                    actor: format!("Plugin({name})"),
                });
            }
            ActorId::AiPlanner(card_id) => {
                return Err(RoleViolation::NotKernelForTaskGateResult {
                    actor: format!("AiPlanner({card_id})"),
                });
            }
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                return Err(RoleViolation::NotKernelForTaskGateResult {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
            ActorId::AiPlannerSession(session)
            | ActorId::AiCodexSession(session)
            | ActorId::AiClaudeSession(session) => {
                return Err(RoleViolation::SessionActorUnresolved {
                    session: session.clone(),
                });
            }
        }
    }

    // (2.8) `review.round` + `ratify.requested` are planner-only; User/Kernel/Plugin
    // do NOT pass, or a forged `converged=true` would bypass the review protocol.
    if matches!(
        event,
        Event::ReviewRound { .. } | Event::RatifyRequested { .. }
    ) {
        match actor {
            ActorId::AiPlanner(card_id) => {
                if cache.get(card_id) != Some(CardRole::Planner) {
                    return Err(RoleViolation::NotPlannerForReviewRatify {
                        actor: actor.to_string(),
                    });
                }
            }
            _ => {
                return Err(RoleViolation::NotPlannerForReviewRatify {
                    actor: actor.to_string(),
                });
            }
        }
    }

    // (2.9) `ratify.resolved` is User-only: the human half of the ratify gate.
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

    // (2.10) `proposal.submitted`: only a plugin, and only for itself — the
    // payload's `plugin_id` must equal the envelope actor's.
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

    // (2.11) `proposal.resolved`: `accepted`/`rejected`/`stale` are User-only;
    // `withdrawn` is the submitting plugin only.
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

    // (3) Worker/ReportCard self-scope + (5) unknown-card deny. The scope's card,
    // track AND area must all match the card's home: a forged `track` or `area`
    // would fan the event out to another track's or area's subscribers.
    if let ActorId::AiPlannerSession(s) | ActorId::AiCodexSession(s) | ActorId::AiClaudeSession(s) =
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
            // Lifecycle carveout: hook bridges run as subprocesses of their worker
            // regardless of role, and these events are pure card-scoped observations, not
            // authority claims. `TrackUpdated` is already refused in (2).
            Some(CardRole::Planner) if is_own_worker_lifecycle_event(actor, event) => {
                enforce_card_self_scope(card_id, scope, cache, track_area_cache)?;
            }
            // Planner cards are bound to AiPlanner, not an AI worker actor.
            Some(CardRole::Planner) => {
                return Err(RoleViolation::NotPlannerForTrack {
                    actor: format!(
                        "{} — card is Planner-roled but actor variant is not AiPlanner",
                        ai_worker_actor_label(actor, card_id),
                    ),
                });
            }
            // ReportCard-bound actors have no cross-card/track authority.
            Some(CardRole::ReportCard) => {
                enforce_card_self_scope(card_id, scope, cache, track_area_cache)?;
            }
            // Assistant cards are the Worker self-scope rule loosened by exactly one
            // card: their home track's report card.
            Some(CardRole::Assistant) => {
                enforce_assistant_scope(card_id, scope, cache, track_area_cache)?;
            }
        }
    }

    // (4) User / Kernel / KernelDispatcher / Plugin: unrestricted.

    Ok(())
}

/// Cross-check that `scope` describes the card's own home: `card`, `track`
/// and `area` all match. The `WorkerOutOfScope` variant name is historical.
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

/// [`enforce_card_self_scope`] loosened by exactly one card: an `Assistant`
/// may also write into its home track's report card scope. Every non-`Card`
/// scope is refused, so an assistant can neither advance the lifecycle nor
/// dispatch a task.
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

/// Shared body: the scope must be `EventScope::Card`, its `card` must satisfy
/// `target_allowed`, and `track` / `area` must match the acting card's home.
fn enforce_card_scope(
    card_id: &CardId,
    scope: &EventScope,
    cache: &CardRoleCache,
    track_area_cache: &TrackAreaCache,
    target_allowed: &dyn Fn(&CardId, &TrackId) -> bool,
    violation: &dyn Fn(CardId, String) -> RoleViolation,
) -> Result<(), RoleViolation> {
    // `get()` (in the caller) and `track_of()` are independent DashMap lookups,
    // so a card deleted between them makes `track_of` return `None`; every
    // denial below must be reachable without it.
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
    // Fail closed: "cannot prove" is a denial, never a panic inside the kernel gate.
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
    if scope_track != &home_track {
        return Err(violation(
            card_id.clone(),
            format!("scope.track mismatch: home={home_track}, scope={scope:?}"),
        ));
    }
    // Fail closed on a miss. A deleted track cascades its `cards` rows in SQL but
    // does not clear `CardRoleCache`, so a known card can outlive its track's
    // area entry; under the tx-hydrated substrate the gate runs inside the write
    // transaction, so panicking would abort a write mid-transaction.
    let Some(home_area) = track_area_cache.area_of(&home_track) else {
        return Err(RoleViolation::RoleLookupFailed {
            subject: format!("tracks.area_id({home_track})"),
        });
    };
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
            recipe_id: None,
            recipe_revision: None,
            claude_permissions_policy: None,
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
            default_template_id: None,
            default_cwd: None,
            created_at: 0,
            updated_at: 0,
        })
    }

    /// Track `w` lives in area `c`; mismatch tests override per-test.
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
    fn ai_planner_with_planner_role_can_update_track() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let planner_id = CardId::from("planner-1");
        cache.insert(planner_id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiPlanner(planner_id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiPlanner(planner-card) should update track: {res:?}"
        );
    }

    #[test]
    fn ai_planner_without_planner_role_cannot_update_track() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let id = CardId::from("c1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiPlanner(id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::NotPlannerForTrack { .. })));
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
            matches!(res, Err(RoleViolation::NotPlannerForTrack { .. })),
            "AiCodex must never emit track.updated regardless of role: {res:?}",
        );
    }

    /// Section 2 runs before section 3's Planner carveout arm, so the invariant
    /// is structural; this pins it against a reorder.
    #[test]
    fn planner_codex_cannot_update_track() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("planner-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &track_updated(),
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotPlannerForTrack { .. })),
            "AiCodex(planner_card) must still be refused on track.updated even after the CodexHook carveout: {res:?}",
        );
    }

    #[test]
    fn worker_in_card_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
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
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("home-track"), AreaId::from("c"));
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("home-track"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &area_updated(),
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
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("home-track"), AreaId::from("home-area"));
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("home-track"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &area_updated(),
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
    fn missing_home_track_area_denies_instead_of_panicking() {
        // A known card can outlive its track's area entry, and the gate runs inside
        // the caller's write transaction, so the miss must be a denial, not a panic.
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, TrackId::from("home-track"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &area_updated(),
            &card_scope(id.as_str(), "home-track", "home-area"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(
                res,
                Err(RoleViolation::RoleLookupFailed { ref subject })
                    if subject == "tracks.area_id(home-track)"
            ),
            "missing track→area entry must deny with RoleLookupFailed: {res:?}",
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
            &ActorId::AiPlanner(CardId::from("")),
            &area_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn unknown_codex_card_rejected() {
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

    /// Shape mirrors what `routes::codex::ingest_hook` constructs.
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
            worker_session_id: "rt-1".into(),
            card_id: CardId::from(card),
            track_id: TrackId::from(track),
            char_count: 3,
        }
    }

    #[test]
    fn planner_codex_hook_in_own_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("planner-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_hook(id.as_str()),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiCodex(planner) CodexHook in own card scope should be accepted: {res:?}",
        );
    }

    #[test]
    fn planner_codex_harness_user_message_in_own_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("planner-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &harness_user_message_enqueued(id.as_str(), "w"),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiCodex(planner) HarnessUserMessageEnqueued in own card scope should be accepted: {res:?}",
        );
    }

    #[test]
    fn planner_codex_non_hook_event_still_rejected() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("planner-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &area_updated(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotPlannerForTrack { .. })),
            "AiCodex(planner) non-hook event must still be refused: {res:?}",
        );
    }

    #[test]
    fn planner_codex_hook_out_of_scope_rejected() {
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        wcc.insert(TrackId::from("home-track"), AreaId::from("c"));
        let id = CardId::from("planner-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("home-track"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_hook(id.as_str()),
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
            "AiCodex(planner) CodexHook with forged scope.track must be refused: {res:?}",
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
            matches!(res, Err(RoleViolation::NotPlannerForTrack { .. })),
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
    fn planner_claude_hook_in_own_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("planner-claude-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id.clone()),
            &claude_hook(id.as_str()),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "AiClaude(planner) ClaudeHook in own card scope should be accepted: {res:?}",
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
                ActorId::AiPlannerSession(WorkerSessionId::from("sess-planner")),
                "sess-planner",
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
            matches!(err, RoleViolation::NotPlannerForDispatch { .. }),
            "expected NotPlannerForDispatch, got {err:?}",
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
            matches!(err, RoleViolation::NotPlannerForDispatch { .. }),
            "expected NotPlannerForDispatch, got {err:?}",
        );
    }

    #[test]
    fn planner_can_emit_codex_worker_requested_in_own_scope() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("planner-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiPlanner(id.clone()),
            &codex_worker_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "planner emitting codex.worker_requested: {res:?}"
        );
    }

    #[test]
    fn worker_cannot_emit_plan_updated_644() {
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
            matches!(err, RoleViolation::NotPlannerForDispatch { .. }),
            "expected NotPlannerForDispatch, got {err:?}",
        );
    }

    #[test]
    fn planner_can_emit_plan_updated_in_own_track() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("planner-1");
        cache.insert(id.clone(), CardRole::Planner, TrackId::from("w"));
        let res = enforce_role(
            &ActorId::AiPlanner(id.clone()),
            &Event::PlanUpdated {
                track_id: TrackId::from("w"),
                changed_keys: vec!["impl-parser".into()],
                agent_message: None,
            },
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "planner emitting plan.updated: {res:?}");
    }

    #[test]
    fn task_dispatched_is_kernel_only_644_pr_b() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let planner = CardId::from("planner-1");
        let worker = CardId::from("worker-1");
        cache.insert(planner.clone(), CardRole::Planner, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = Event::TaskDispatched {
            idempotency_key: "w:impl-parser".into(),
            kind: "codex".into(),
            agent_message: None,
        };

        for (actor, label) in [
            (ActorId::AiPlanner(planner.clone()), "AiPlanner(planner)"),
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
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let planner = CardId::from("planner-1");
        let worker = CardId::from("worker-1");
        cache.insert(planner.clone(), CardRole::Planner, TrackId::from("w"));
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
            (ActorId::AiPlanner(planner.clone()), "AiPlanner(planner)"),
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
    fn task_execution_settled_is_kernel_only() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let event = Event::TaskExecutionSettled {
            task_id: "w:attempt".into(),
            operation_id: "op".into(),
        };
        for actor in [ActorId::Kernel, ActorId::KernelDispatcher] {
            enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).unwrap();
        }
        for actor in [
            ActorId::User,
            ActorId::Plugin("p".into()),
            ActorId::AiPlanner("planner".into()),
            ActorId::AiCodex("worker".into()),
            ActorId::AiClaude("worker".into()),
            ActorId::AiPlannerSession("planner-session".into()),
            ActorId::AiCodexSession("worker-session".into()),
            ActorId::AiClaudeSession("worker-session".into()),
        ] {
            assert!(
                enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).is_err(),
                "{actor:?}"
            );
        }
    }

    #[test]
    fn task_file_publication_settled_is_kernel_only() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let event = Event::TaskFilePublicationSettled {
            task_id: "a".into(),
            operation_id: "publication".into(),
        };
        for actor in [ActorId::Kernel, ActorId::KernelDispatcher] {
            enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).unwrap();
        }
        for actor in [
            ActorId::User,
            ActorId::Plugin("p".into()),
            ActorId::AiPlanner("p".into()),
            ActorId::AiCodex("w".into()),
            ActorId::AiPlannerSession("p".into()),
            ActorId::AiCodexSession("w".into()),
        ] {
            assert!(
                enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).is_err(),
                "{actor:?}"
            );
        }
    }

    #[test]
    fn task_candidate_verification_settled_is_kernel_only() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let event = Event::TaskCandidateVerificationSettled {
            task_id: "a".into(),
            operation_id: "verification".into(),
        };
        for actor in [ActorId::Kernel, ActorId::KernelDispatcher] {
            enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).unwrap();
        }
        for actor in [
            ActorId::User,
            ActorId::Plugin("p".into()),
            ActorId::AiPlanner("p".into()),
            ActorId::AiCodex("w".into()),
            ActorId::AiPlannerSession("p".into()),
            ActorId::AiCodexSession("w".into()),
        ] {
            assert!(
                enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).is_err(),
                "{actor:?}"
            );
        }
    }

    #[test]
    fn task_git_delivery_settled_is_kernel_only() {
        use calm_types::git_candidate::{
            DeliveryFailureCode, DeliverySettlement, DeliveryWakeReason,
        };
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let event = Event::TaskGitDeliverySettled {
            task_id: "a".into(),
            idempotency_key: "a".into(),
            track_id: TrackId::from("w"),
            card_id: CardId::from("c"),
            delivery_id: "d".into(),
            ordinal: 1,
            result: DeliverySettlement::Failed {
                code: DeliveryFailureCode::Unresolved,
                reason: "probe unknown".into(),
                retry_allowed: true,
            },
            wake_reason: DeliveryWakeReason::Failed,
        };
        for actor in [ActorId::Kernel, ActorId::KernelDispatcher] {
            enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).unwrap();
        }
        for actor in [
            ActorId::User,
            ActorId::Plugin("p".into()),
            ActorId::AiPlanner("p".into()),
            ActorId::AiCodex("w".into()),
            ActorId::AiClaude("w".into()),
            ActorId::AiPlannerSession("p".into()),
            ActorId::AiCodexSession("w".into()),
        ] {
            assert!(
                enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc).is_err(),
                "{actor:?}"
            );
        }
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
    fn review_round_and_ratify_requested_are_planner_only_760() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let planner = CardId::from("planner-1");
        let worker = CardId::from("worker-1");
        cache.insert(planner.clone(), CardRole::Planner, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));

        for event in [review_round(), ratify_requested()] {
            let res = enforce_role(
                &ActorId::AiPlanner(planner.clone()),
                &event,
                &track_scope("w", "c"),
                &cache,
                &wcc,
            );
            assert!(
                res.is_ok(),
                "planner should emit {}: {res:?}",
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
                    ActorId::AiPlannerSession(WorkerSessionId::from("sess-unresolved")),
                    "AiPlannerSession(unresolved)",
                ),
            ] {
                let err = enforce_role(&actor, &event, &track_scope("w", "c"), &cache, &wcc)
                    .expect_err(&format!("{label} must be refused {}", event.kind_tag()));
                assert!(
                    matches!(err, RoleViolation::NotPlannerForReviewRatify { .. }),
                    "{label}: expected NotPlannerForReviewRatify, got {err:?}",
                );
            }
        }
    }

    #[test]
    fn ratify_resolved_is_user_only_760() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let planner = CardId::from("planner-1");
        let worker = CardId::from("worker-1");
        cache.insert(planner.clone(), CardRole::Planner, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = ratify_resolved_grant();

        let res = enforce_role(&ActorId::User, &event, &track_scope("w", "c"), &cache, &wcc);
        assert!(res.is_ok(), "User should emit ratify.resolved: {res:?}");

        for (actor, label) in [
            (ActorId::AiPlanner(planner.clone()), "AiPlanner(planner)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (ActorId::Plugin("p".into()), "Plugin(p)"),
            (ActorId::Kernel, "Kernel"),
            (ActorId::KernelDispatcher, "KernelDispatcher"),
            (
                ActorId::AiPlannerSession(WorkerSessionId::from("sess-planner")),
                "AiPlannerSession(unresolved)",
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
        let cache = CardRoleCache::new();
        let wcc = TrackAreaCache::new();
        let res = enforce_role(
            &ActorId::AiPlanner(CardId::from("")),
            &codex_worker_requested(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

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

        let res = enforce_role(
            &ActorId::Plugin("dev.neige.invest".into()),
            &event,
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "submitting plugin must pass: {res:?}");

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
        let planner = CardId::from("planner-1");
        let worker = CardId::from("worker-1");
        cache.insert(planner.clone(), CardRole::Planner, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = proposal_submitted("dev.neige.invest");

        for (actor, label) in [
            (ActorId::User, "User"),
            (ActorId::Kernel, "Kernel"),
            (ActorId::KernelDispatcher, "KernelDispatcher"),
            (ActorId::AiPlanner(planner.clone()), "AiPlanner(planner)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (
                ActorId::AiPlannerSession(WorkerSessionId::from("sess-planner")),
                "AiPlannerSession",
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
        let planner = CardId::from("planner-1");
        let worker = CardId::from("worker-1");
        cache.insert(planner.clone(), CardRole::Planner, TrackId::from("w"));
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

            // Everyone else is refused — INCLUDING the submitting plugin (no self-approval).
            for (actor, label) in [
                (
                    ActorId::Plugin("dev.neige.invest".into()),
                    "Plugin(submitter)",
                ),
                (ActorId::Plugin("dev.neige.other".into()), "Plugin(other)"),
                (ActorId::Kernel, "Kernel"),
                (ActorId::KernelDispatcher, "KernelDispatcher"),
                (ActorId::AiPlanner(planner.clone()), "AiPlanner(planner)"),
                (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
                (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
                (
                    ActorId::AiPlannerSession(WorkerSessionId::from("sess-planner")),
                    "AiPlannerSession",
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
        let planner = CardId::from("planner-1");
        let worker = CardId::from("worker-1");
        cache.insert(planner.clone(), CardRole::Planner, TrackId::from("w"));
        cache.insert(worker.clone(), CardRole::Worker, TrackId::from("w"));
        let event = proposal_resolved("dev.neige.invest", ProposalDecision::Withdrawn);

        let res = enforce_role(
            &ActorId::Plugin("dev.neige.invest".into()),
            &event,
            &track_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "submitter must withdraw: {res:?}");

        // Everyone else is refused — including the USER (withdraw is the plugin's exit).
        for (actor, label) in [
            (ActorId::User, "User"),
            (ActorId::Plugin("dev.neige.other".into()), "Plugin(other)"),
            (ActorId::Kernel, "Kernel"),
            (ActorId::KernelDispatcher, "KernelDispatcher"),
            (ActorId::AiPlanner(planner.clone()), "AiPlanner(planner)"),
            (ActorId::AiCodex(worker.clone()), "AiCodex(worker)"),
            (ActorId::AiClaude(worker.clone()), "AiClaude(worker)"),
            (
                ActorId::AiPlannerSession(WorkerSessionId::from("sess-planner")),
                "AiPlannerSession",
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

    /// The delete race must be a *denial* on every branch, not a panic: the empty
    /// cache is exactly that race's end state, so every scope shape must be `Err`.
    #[test]
    fn card_scope_is_fail_closed_when_the_acting_card_vanished() {
        let vanished = CardRoleCache::new();
        let wcc = seeded_wcc();
        let acting = CardId::from("worker-1");
        let self_only = |target: &CardId, _home: &TrackId| target == &acting;
        for scope in [
            // Non-Card scope — refused before `track_of` in every version.
            track_scope("w", "c"),
            // Card scope naming someone else's card: the actual out-of-bounds write path.
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
