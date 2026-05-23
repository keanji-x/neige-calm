//! Wave-as-Actor PR3 (#136) — authorization gate at the single write entry.
//!
//! The gate runs inside `Repo::write_with_event` / `Repo::log_pure_event`,
//! after the closure produces an `Event`, before `event_append_in_tx`
//! commits the row. A violation rolls the txn back: no entity write,
//! no event row, no broadcast. The kernel is the only safety boundary
//! between AI-controlled cards and the wave-level kernel state, so the
//! gate is deliberately strict — *deny* is the default for anything
//! ambiguous, and we re-confirm role lookups against the in-process
//! `CardRoleCache` rather than trusting the actor's claimed identity.
//!
//! ## What the gate enforces
//!
//! 1. **Empty-CardId guard.** `ActorId::AiCodex(CardId(""))` and
//!    `ActorId::AiSpec(CardId(""))` are rejected outright. This catches
//!    the PR2 stopgap path in `crate::actor::Actor::to_actor_id` where
//!    the `X-Calm-Actor: ai:codex` header has no card context to attach.
//!    PR3 reattributes the codex bridge ingest to a real card id (see
//!    `routes::codex::ingest_hook`), so this branch ends up firing only
//!    when something else regresses — fail loud, not silent.
//!
//! 2. **`Event::WaveUpdated` is gated to spec cards.** The actor must be
//!    `User`, `Kernel`, or `AiSpec(card_id)` where the cache confirms
//!    `CardRole::Spec`. Any `AiCodex` actor — even one bound to a card —
//!    is rejected: codex worker cards must not edit wave-level state.
//!
//! 3. **Worker-card scope check.** When an `AiCodex(card_id)` actor's
//!    cached role is `Worker`, the event's `EventScope` must be the
//!    same card *and* its `wave` field must match the card's home
//!    wave. A worker that tries to emit a `Wave` or `Cove` scope
//!    event — or a Card scope with a spoofed `wave` — is refused
//!    (issue #232).
//!
//! 4. **User / Kernel / KernelDispatcher / Plugin(_)** are unrestricted
//!    in PR3. The kernel's own writes (FSM projector, terminal sweeper,
//!    plugin callback dispatcher) and the user's REST surface continue
//!    to flow through the gate unchanged.
//!
//! 5. **Unknown card.** If the actor names a card the cache doesn't
//!    know, the write is denied. Two possible causes:
//!      * the card was deleted between the actor's request landing and
//!        the gate running (race; safe to reject),
//!      * an attacker fabricated a card id (the gate is the last line
//!        of defense, deny by default).

use crate::card_role_cache::CardRoleCache;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::model::CardRole;
use thiserror::Error;

/// Reasons the gate may refuse a write. Surfaced verbatim into the
/// returned `CalmError::Forbidden` so test assertions can pattern-match
/// without parsing a free-form string.
#[derive(Debug, Error)]
pub enum RoleViolation {
    #[error("AiCodex/AiSpec actor has empty card id (likely from legacy `ai:codex` header path)")]
    EmptyAiCardId,

    #[error("only spec cards (or User/Kernel) may emit wave.updated (actor={actor})")]
    NotSpecForWave { actor: String },

    #[error("worker card {card} is out of scope {scope}")]
    WorkerOutOfScope { card: CardId, scope: String },

    #[error(
        "AiCodex actor references card {card} that the role cache does not know — \
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
) -> Result<(), RoleViolation> {
    // --- (1) Empty-CardId guard. ---
    //
    // PR2's `Actor::to_actor_id` returns `AiCodex(CardId(""))` for the
    // legacy `X-Calm-Actor: ai:codex` header path because there's no
    // card context at the REST entry. PR3 must not silently match an
    // empty CardId against any real card — that would be a
    // gate-bypass. We reject loud and let the call site (the codex
    // bridge ingest in routes/codex.rs) attribute a real card.
    if let ActorId::AiCodex(c) | ActorId::AiSpec(c) = actor
        && c.as_str().is_empty()
    {
        return Err(RoleViolation::EmptyAiCardId);
    }

    // --- (2) `WaveUpdated` is spec-only. ---
    //
    // The wave-level authority decision: only the spec card (PR6) is
    // allowed to update the wave row. User + Kernel keep their
    // unrestricted authority (the user is *the* authority; Kernel is
    // the FSM projector / sweeper / plugin dispatcher, which writes
    // server-internal lifecycle the user implicitly authorized at
    // boot).
    if matches!(event, Event::WaveUpdated(_)) {
        match actor {
            ActorId::User | ActorId::Kernel | ActorId::KernelDispatcher => {}
            ActorId::Plugin(_) => {
                // Plugins are unrestricted in PR3 — see the
                // `RouteRepo` capability split docs. Plugin-driven
                // wave edits are rare in practice (the surface for
                // them lives in the plugin host callback dispatcher,
                // which is server-internal). If PR4+ tightens this,
                // it lands here.
            }
            ActorId::AiSpec(card_id) => {
                let role = cache.get(card_id);
                if role != Some(CardRole::Spec) {
                    return Err(RoleViolation::NotSpecForWave {
                        actor: format!("AiSpec({card_id})"),
                    });
                }
            }
            ActorId::AiCodex(card_id) => {
                // Even an AiCodex actor whose card happens to be
                // `Spec`-roled (impossible in PR3 — spec cards are
                // bound to `AiSpec`) is rejected here. The actor
                // variant is the wire-level claim; the gate sticks
                // to it rather than re-binding via the cache.
                return Err(RoleViolation::NotSpecForWave {
                    actor: format!("AiCodex({card_id})"),
                });
            }
        }
    }

    // --- (3) Worker-card scope check + (5) unknown-card deny. ---
    //
    // For `AiCodex` actors: confirm the cache knows the card, and if
    // the cached role is `Worker`, refuse anything broader than that
    // card's own scope. The check is two-pronged:
    //   * `scope.card == self_card` — the worker only writes into its
    //     own card scope;
    //   * `scope.wave == cache.wave_of(self_card)` — the supplied
    //     `wave` field must match the worker's home wave (closes
    //     issue #232: a Worker could otherwise forge `wave: <ANY>`
    //     and the kernel would route the event to that wave's
    //     subscribers).
    //
    // TODO(#232 followup): the same shape would gate `cove` if the
    // cache also tracked the home cove. Card holds `wave_id` directly,
    // but `cove_id` lives on the parent `waves` row — adding it
    // requires a join in `seed_from_db` and either an extra read in
    // `card_create_with_id_tx` or threading the cove through call
    // sites. Deferred until there's a concrete cove-spoof attack to
    // motivate the plumbing; wave is the primary fan-out axis.
    //
    // Plain-role codex cards (the path users hit today before the
    // dispatcher introduces Worker cards in earnest) have no extra
    // scope restriction.
    if let ActorId::AiCodex(card_id) = actor {
        match cache.get(card_id) {
            None => {
                return Err(RoleViolation::UnknownCard {
                    card: card_id.clone(),
                });
            }
            Some(CardRole::Worker) => {
                let card_matches =
                    matches!(scope, EventScope::Card { card, .. } if card == card_id);
                if !card_matches {
                    return Err(RoleViolation::WorkerOutOfScope {
                        card: card_id.clone(),
                        scope: format!("scope.card mismatch: {scope:?}"),
                    });
                }
                // Card matched. Now cross-check `scope.wave` against
                // the card's immutable home wave. `wave_of` returning
                // None at this point is impossible — we just got
                // `Some(CardRole::Worker)` from the same entry — but
                // the explicit `.expect` documents the invariant for
                // a future refactor.
                let home_wave = cache
                    .wave_of(card_id)
                    .expect("wave_of must be Some when get() returned Some — same cache entry");
                let scope_wave = match scope {
                    EventScope::Card { wave, .. } => wave,
                    // Unreachable: `card_matches` above already
                    // pinned the variant to `EventScope::Card`.
                    _ => unreachable!("card_matches guarantees Card variant"),
                };
                if scope_wave != &home_wave {
                    return Err(RoleViolation::WorkerOutOfScope {
                        card: card_id.clone(),
                        scope: format!("scope.wave mismatch: home={home_wave}, scope={scope:?}"),
                    });
                }
            }
            // PR3 invariant: spec cards are bound to AiSpec, not
            // AiCodex. If the cache claims an AiCodex actor's card is
            // Spec-roled, something has gone wrong upstream — fail
            // loud. This branch unreachable today.
            Some(CardRole::Spec) => {
                return Err(RoleViolation::NotSpecForWave {
                    actor: format!(
                        "AiCodex({card_id}) — card is Spec-roled but actor variant is AiCodex"
                    ),
                });
            }
            // Plain: no extra restrictions from this arm. Wave-update
            // was already handled in step (2) above.
            Some(CardRole::Plain) => {}
            // Issue #229 PR A — ReportCard behaves like Plain for the
            // gate's purposes: it has no scope-broadening authority
            // beyond its own card. The wave-update branch above
            // already refuses any AiCodex actor (which is what a
            // report-card-bound MCP connection would surface as) from
            // emitting `WaveUpdated`, so we don't need a separate
            // check here. PR B introduces the actual card kind and
            // payload; until then this arm only exists to satisfy
            // exhaustiveness.
            Some(CardRole::ReportCard) => {}
        }
    }

    // --- (4) User / Kernel / KernelDispatcher / Plugin: unrestricted. ---
    //
    // The match above already let them through. Documented here as a
    // gate decision, not as code, so the policy is greppable.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CoveId, WaveId};
    use crate::model::{Cove, CoveKind, Wave, WaveLifecycle};

    fn wave(id: &str, cove: &str) -> Wave {
        Wave {
            id: WaveId::from(id),
            cove_id: CoveId::from(cove),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            lifecycle: WaveLifecycle::Draft,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card_scope(card: &str, wave: &str, cove: &str) -> EventScope {
        EventScope::Card {
            card: CardId::from(card),
            wave: WaveId::from(wave),
            cove: CoveId::from(cove),
        }
    }

    fn wave_scope(wave: &str, cove: &str) -> EventScope {
        EventScope::Wave {
            wave: WaveId::from(wave),
            cove: CoveId::from(cove),
        }
    }

    fn wave_updated() -> Event {
        Event::WaveUpdated(wave("w", "c"))
    }

    fn cove_updated() -> Event {
        Event::CoveUpdated(Cove {
            id: CoveId::from("c"),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: CoveKind::User,
            created_at: 0,
            updated_at: 0,
        })
    }

    #[test]
    fn user_can_update_wave() {
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::User,
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(
            res.is_ok(),
            "user should be allowed to update wave: {res:?}"
        );
    }

    #[test]
    fn kernel_can_update_wave() {
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::Kernel,
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn ai_spec_with_spec_role_can_update_wave() {
        let cache = CardRoleCache::new();
        let spec_id = CardId::from("spec-1");
        cache.insert(spec_id.clone(), CardRole::Spec, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(spec_id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(res.is_ok(), "AiSpec(spec-card) should update wave: {res:?}");
    }

    #[test]
    fn ai_spec_without_spec_role_cannot_update_wave() {
        // An AiSpec actor whose cached role is `Plain` (mismatch
        // between wire claim + persisted truth) is denied.
        let cache = CardRoleCache::new();
        let id = CardId::from("c1");
        cache.insert(id.clone(), CardRole::Plain, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::NotSpecForWave { .. })));
    }

    #[test]
    fn ai_codex_cannot_update_wave_even_with_known_card() {
        let cache = CardRoleCache::new();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForWave { .. })),
            "AiCodex must never emit wave.updated regardless of role: {res:?}",
        );
    }

    #[test]
    fn worker_in_card_scope_ok() {
        let cache = CardRoleCache::new();
        let id = CardId::from("worker-1");
        // Worker's home wave is "w" — scope below must use the same.
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            // A non-wave-updated event (CoveUpdated chosen because it
            // also has no card semantics — but the scope is what we
            // assert on, the event variant is irrelevant after the
            // wave-updated branch). Use a card-scoped event:
            // OverlaySet would also work; CoveUpdated lets us exercise
            // the scope check independent of payload shape.
            &cove_updated(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
        );
        assert!(res.is_ok(), "worker in own card scope: {res:?}");
    }

    #[test]
    fn worker_out_of_card_scope_rejected() {
        let cache = CardRoleCache::new();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        // Wave scope when caller is a worker → reject.
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &cove_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::WorkerOutOfScope { .. })));
    }

    #[test]
    fn worker_in_different_card_scope_rejected() {
        let cache = CardRoleCache::new();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &cove_updated(),
            &card_scope("not-my-card", "w", "c"),
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::WorkerOutOfScope { .. })));
    }

    #[test]
    fn worker_with_mismatched_scope_wave_rejected() {
        // Issue #232: even with `scope.card == self`, the gate must
        // reject a `scope.wave` that doesn't match the Worker card's
        // home wave. Without this check, a Worker could forge any
        // wave id and the kernel would route the event to that wave's
        // subscribers.
        let cache = CardRoleCache::new();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("home-wave"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &cove_updated(),
            // Same card, but a different wave — must reject.
            &card_scope(id.as_str(), "other-wave", "c"),
            &cache,
        );
        assert!(
            matches!(
                res,
                Err(RoleViolation::WorkerOutOfScope { ref scope, .. })
                    if scope.contains("scope.wave mismatch")
            ),
            "Worker forging scope.wave must be refused: {res:?}",
        );
    }

    #[test]
    fn empty_codex_card_id_rejected() {
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("")),
            &cove_updated(),
            &EventScope::System,
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn empty_aispec_card_id_rejected() {
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::AiSpec(CardId::from("")),
            &cove_updated(),
            &EventScope::System,
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn unknown_codex_card_rejected() {
        // Defense-in-depth: an AiCodex actor whose card is not in the
        // cache is denied. Covers two real cases — card was deleted
        // between request and gate, or the id was fabricated.
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("never-seen")),
            &cove_updated(),
            &EventScope::System,
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::UnknownCard { .. })));
    }

    #[test]
    fn plain_codex_card_unrestricted_in_card_scope() {
        // The pre-PR5 flow: codex cards exist with role=Plain (PR5
        // will introduce dispatcher-spawned Worker cards). Plain
        // codex cards can emit anything in their own scope. The
        // gate's job at this stage is to *catch* the PR5+ role
        // transitions, not to lock down the existing pre-PR5
        // behavior.
        let cache = CardRoleCache::new();
        let id = CardId::from("codex-plain");
        cache.insert(id.clone(), CardRole::Plain, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &cove_updated(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
        );
        assert!(res.is_ok(), "plain codex card in own scope: {res:?}");
    }

    #[test]
    fn plugin_actor_unrestricted() {
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::Plugin("hello-world".into()),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn kernel_dispatcher_unrestricted() {
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::KernelDispatcher,
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
        );
        assert!(res.is_ok());
    }

    // ---- PR4 of #136: new Event variants flow through enforce_role ------
    //
    // PR4 is schema-only — there are no emitters of these variants yet,
    // but PR5 (dispatcher) and PR8 (wait_for_events) will rely on the
    // gate's existing logic to route + authorize them. These tests lock
    // in the behavior PR5 will depend on:
    //
    //   * a worker card emitting `codex.job_requested` within its own
    //     card scope is permitted (PR5's job request fan-out path);
    //   * a worker card emitting `task.completed` within its own card
    //     scope is permitted (PR8's wait_for_events delivery path);
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

    fn codex_job_requested() -> Event {
        Event::CodexJobRequested {
            idempotency_key: "idem-1".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
        }
    }

    fn task_completed() -> Event {
        Event::TaskCompleted {
            idempotency_key: "idem-1".into(),
            result: serde_json::Value::Null,
            artifacts: vec![ArtifactRef::from("a-1")],
        }
    }

    #[test]
    fn worker_can_emit_codex_job_requested_in_own_scope() {
        // PR5's dispatcher will surface this path when a worker card
        // fans out a sub-job request. Scope must be the worker's own
        // card (else the section-3 worker-scope check fires).
        let cache = CardRoleCache::new();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_job_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
        );
        assert!(
            res.is_ok(),
            "worker in own card scope can request a codex job: {res:?}",
        );
    }

    #[test]
    fn worker_can_emit_task_completed_in_own_scope() {
        // PR8's wait_for_events delivery path: workers report
        // task.completed scoped to themselves.
        let cache = CardRoleCache::new();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &task_completed(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
        );
        assert!(
            res.is_ok(),
            "worker reporting its own task completion: {res:?}",
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
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("")),
            &task_completed(),
            &EventScope::System,
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn empty_aispec_card_id_rejected_on_new_variant() {
        // Mirror of the AiCodex case for AiSpec — when PR5 wires the
        // spec card as the requester of codex.job_requested, the empty
        // CardId path must still be rejected.
        let cache = CardRoleCache::new();
        let res = enforce_role(
            &ActorId::AiSpec(CardId::from("")),
            &codex_job_requested(),
            &EventScope::System,
            &cache,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }
}
