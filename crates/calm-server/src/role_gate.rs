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
//! 1. **Empty-CardId guard.** `ActorId::AiCodex(CardId(""))`,
//!    `ActorId::AiClaude(CardId(""))`, and `ActorId::AiSpec(CardId(""))`
//!    are rejected outright. This catches
//!    the PR2 stopgap path in `crate::actor::Actor::to_actor_id` where
//!    the `X-Calm-Actor: ai:codex` header has no card context to attach.
//!    PR3 reattributes the codex bridge ingest to a real card id (see
//!    `routes::codex::ingest_hook`), so this branch ends up firing only
//!    when something else regresses — fail loud, not silent.
//!
//! 2. **`Event::WaveUpdated` is gated to spec cards.** The actor must be
//!    `User`, `Kernel`, or `AiSpec(card_id)` where the cache confirms
//!    `CardRole::Spec`. Any `AiCodex` / `AiClaude` actor — even one bound
//!    to a card — is rejected: worker cards must not edit wave-level state.
//!
//! 3. **Worker-card scope check.** When an `AiCodex(card_id)` or
//!    `AiClaude(card_id)` actor's cached role is `Worker`, the event's
//!    `EventScope` must be the
//!    same card, its `wave` field must match the card's home wave
//!    (issue #232), *and* its `cove` field must match the card's
//!    home cove (issue #234). A worker that tries to emit a `Wave`
//!    or `Cove` scope event — or a Card scope with a spoofed `wave`
//!    or `cove` — is refused.
//!
//! 4. **Dispatch-request events are gated to spec cards.** Issue #583.
//!    `Event::CodexJobRequested` and `Event::TerminalJobRequested` are
//!    refused for any `AiCodex` / `AiClaude` actor, mirroring the
//!    `WaveUpdated` rule. Spec card (`AiSpec`) with cached role `Spec`
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
use crate::ids::{ActorId, CardId};
use crate::model::CardRole;
use crate::wave_cove_cache::WaveCoveCache;
use thiserror::Error;

/// Reasons the gate may refuse a write. Surfaced verbatim into the
/// returned `CalmError::Forbidden` so test assertions can pattern-match
/// without parsing a free-form string.
#[derive(Debug, Error)]
pub enum RoleViolation {
    #[error("AiCodex/AiClaude/AiSpec actor has empty card id (likely from legacy AI header path)")]
    EmptyAiCardId,

    #[error("only spec cards (or User/Kernel) may emit wave.updated (actor={actor})")]
    NotSpecForWave { actor: String },

    #[error("only spec cards (or User/Kernel) may emit dispatch-request events (actor={actor})")]
    NotSpecForDispatch { actor: String },

    #[error("worker card {card} is out of scope {scope}")]
    WorkerOutOfScope { card: CardId, scope: String },

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
    wave_cove_cache: &WaveCoveCache,
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
            ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) => {
                // Even an AI worker actor whose card happens to be
                // `Spec`-roled (impossible in PR3 — spec cards are
                // bound to `AiSpec`) is rejected here. The actor
                // variant is the wire-level claim; the gate sticks
                // to it rather than re-binding via the cache.
                return Err(RoleViolation::NotSpecForWave {
                    actor: ai_worker_actor_label(actor, card_id),
                });
            }
        }
    }

    // --- (2.5) Dispatch-request events are spec-only. ---
    //
    // Issue #583. `calm.dispatch_request` is gated to Spec at the MCP
    // soft gate (`emit.rs::dispatch_request`), but the in-tx gate must
    // also refuse worker AI actors from emitting these events to provide
    // real kernel-level defense-in-depth — otherwise an internal caller
    // that reaches `write_with_event_typed` with an AiCodex/AiClaude
    // worker actor + a dispatch event can still commit a recursive
    // worker-tree mint. Mirrors section (2)'s shape.
    if matches!(
        event,
        Event::CodexJobRequested { .. } | Event::TerminalJobRequested { .. }
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
        }
    }

    // --- (3) Worker-card scope check + (5) unknown-card deny. ---
    //
    // For AI worker actors: confirm the cache knows the card, and if
    // the cached role is `Worker`, refuse anything broader than that
    // card's own scope. The check is three-pronged:
    //   * `scope.card == self_card` — the worker only writes into its
    //     own card scope;
    //   * `scope.wave == cache.wave_of(self_card)` — the supplied
    //     `wave` field must match the worker's home wave (closes
    //     issue #232: a Worker could otherwise forge `wave: <ANY>`
    //     and the kernel would route the event to that wave's
    //     subscribers).
    //   * `scope.cove == wave_cove_cache.cove_of(home_wave)` — the
    //     supplied `cove` must match the home wave's persisted cove
    //     (closes issue #234: same fan-out spoof shape as #232 but
    //     one level up). Cove is immutable per wave so the lookup is
    //     stable for the card's lifetime.
    //
    if let ActorId::AiCodex(card_id) | ActorId::AiClaude(card_id) = actor {
        match cache.get(card_id) {
            None => {
                return Err(RoleViolation::UnknownCard {
                    card: card_id.clone(),
                });
            }
            Some(CardRole::Worker) => {
                enforce_card_self_scope(card_id, scope, cache, wave_cove_cache)?;
            }
            // Bug A carveout — hook bridges run as subprocesses of their
            // worker regardless of the card's role; they can't easily know
            // at fire time whether the card is Spec- or Worker-roled.
            // For the worker's own hook event (a pure lifecycle
            // observation, *not* a wave-level authority claim) we accept
            // the write from an AI-worker spec-card actor as long as the
            // scope matches the card's own home (card_id + wave + cove
            // cached values — same shape as the Worker arm). Anything
            // else from that actor is still refused; write authority for
            // spec-roled cards lives with `AiSpec`. Note that
            // `Event::WaveUpdated` is already gated in section (2) above
            // and unconditionally refuses any AI worker actor, so this
            // carveout cannot regress the wave-authority invariant.
            Some(CardRole::Spec) if is_own_worker_hook_event(actor, event) => {
                enforce_card_self_scope(card_id, scope, cache, wave_cove_cache)?;
            }
            // PR3 invariant: spec cards are bound to AiSpec, not an AI
            // worker actor. Anything other than the hook carveout above
            // (which is a stateless bridge ingest path) from a
            // worker-variant spec-card actor is rejected.
            Some(CardRole::Spec) => {
                return Err(RoleViolation::NotSpecForWave {
                    actor: format!(
                        "{} — card is Spec-roled but actor variant is not AiSpec",
                        ai_worker_actor_label(actor, card_id),
                    ),
                });
            }
            // Issue #229 PR A — ReportCard has no wave-level authority.
            // The wave-update branch above already refuses any AiCodex
            // actor (which is what a report-card-bound MCP connection
            // would surface as) from emitting `WaveUpdated`, so the
            // existing report-card gate behavior remains unchanged here.
            Some(CardRole::ReportCard) => {}
        }
    }

    // --- (4) User / Kernel / KernelDispatcher / Plugin: unrestricted. ---
    //
    // The match above already let them through. Documented here as a
    // gate decision, not as code, so the policy is greppable.

    Ok(())
}

/// Cross-check that `scope` describes the card's own home — `card`
/// matches, `wave` matches the cached home wave, `cove` matches the
/// home wave's persisted cove. Shared between the Worker arm (which
/// uses it for *every* event) and the Spec arm's `CodexHook` carveout
/// (bug A — the codex bridge ingest path for a spec card).
///
/// Returns `Err(RoleViolation::WorkerOutOfScope)` on any mismatch. The
/// variant name is historical (the check originated in the Worker
/// path); the semantic — "this AiCodex actor is writing outside its
/// own card scope" — applies equally to both call sites.
fn enforce_card_self_scope(
    card_id: &CardId,
    scope: &EventScope,
    cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
) -> Result<(), RoleViolation> {
    let card_matches = matches!(scope, EventScope::Card { card, .. } if card == card_id);
    if !card_matches {
        return Err(RoleViolation::WorkerOutOfScope {
            card: card_id.clone(),
            scope: format!("scope.card mismatch: {scope:?}"),
        });
    }
    // Card matched. Now cross-check `scope.wave` against the card's
    // immutable home wave. `wave_of` returning None at this point is
    // impossible — the caller just got `Some(_)` for the same card
    // from the same cache — but the explicit `.expect` documents the
    // invariant for a future refactor.
    let home_wave = cache
        .wave_of(card_id)
        .expect("wave_of must be Some when get() returned Some — same cache entry");
    let (scope_wave, scope_cove) = match scope {
        EventScope::Card { wave, cove, .. } => (wave, cove),
        // Unreachable: `card_matches` above already pinned the variant
        // to `EventScope::Card`.
        _ => unreachable!("card_matches guarantees Card variant"),
    };
    if scope_wave != &home_wave {
        return Err(RoleViolation::WorkerOutOfScope {
            card: card_id.clone(),
            scope: format!("scope.wave mismatch: home={home_wave}, scope={scope:?}"),
        });
    }
    // #234 — cross-check `scope.cove` against the home wave's persisted
    // cove. The wave→cove cache is write-through-populated in
    // `wave_create_tx`, so a missing entry under a known wave id is a
    // hard invariant break worth failing loudly on (rather than the
    // silent "deny by default" of the role cache miss, which has its
    // own race-with-delete semantics covered elsewhere).
    let home_cove = wave_cove_cache.cove_of(&home_wave).expect(
        "wave_cove_cache must be populated for any wave with a known card — \
         wave_create_tx writes through unconditionally",
    );
    if scope_cove != &home_cove {
        return Err(RoleViolation::WorkerOutOfScope {
            card: card_id.clone(),
            scope: format!("scope.cove mismatch: home={home_cove}, scope={scope:?}"),
        });
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

fn is_own_worker_hook_event(actor: &ActorId, event: &Event) -> bool {
    matches!(
        (actor, event),
        (ActorId::AiCodex(_), Event::CodexHook { .. })
            | (ActorId::AiClaude(_), Event::ClaudeHook { .. })
    )
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
            pinned_at: None,
            lifecycle: WaveLifecycle::Draft,
            cwd: String::new(),
            terminal_at: None,
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

    /// Pre-seeded wave→cove cache the Worker tests use: wave `w` lives
    /// in cove `c`. Tests that exercise mismatch paths override this
    /// per-test (#234).
    fn seeded_wcc() -> WaveCoveCache {
        let c = WaveCoveCache::new();
        c.insert(WaveId::from("w"), CoveId::from("c"));
        c
    }

    #[test]
    fn user_can_update_wave() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::User,
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            res.is_ok(),
            "user should be allowed to update wave: {res:?}"
        );
    }

    #[test]
    fn kernel_can_update_wave() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::Kernel,
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn ai_spec_with_spec_role_can_update_wave() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let spec_id = CardId::from("spec-1");
        cache.insert(spec_id.clone(), CardRole::Spec, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(spec_id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "AiSpec(spec-card) should update wave: {res:?}");
    }

    #[test]
    fn ai_spec_without_spec_role_cannot_update_wave() {
        // An AiSpec actor whose cached role is `Worker` (mismatch
        // between wire claim + persisted truth) is denied.
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let id = CardId::from("c1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::NotSpecForWave { .. })));
    }

    #[test]
    fn ai_codex_cannot_update_wave_even_with_known_card() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForWave { .. })),
            "AiCodex must never emit wave.updated regardless of role: {res:?}",
        );
    }

    /// Belt-and-suspenders companion to the Worker test above: the
    /// CodexHook carveout added for spec cards must not let `WaveUpdated`
    /// through. Section 2 (`WaveUpdated` is spec-only via `AiSpec`) runs
    /// before section 3's `Some(CardRole::Spec) if CodexHook` arm, so
    /// the invariant is structural — this test pins it explicitly so a
    /// future refactor that reorders the sections can't silently regress
    /// it.
    #[test]
    fn spec_codex_cannot_update_wave() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForWave { .. })),
            "AiCodex(spec_card) must still be refused on wave.updated even after the CodexHook carveout: {res:?}",
        );
    }

    #[test]
    fn worker_in_card_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
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
            &wcc,
        );
        assert!(res.is_ok(), "worker in own card scope: {res:?}");
    }

    #[test]
    fn worker_out_of_card_scope_rejected() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        // Wave scope when caller is a worker → reject.
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &cove_updated(),
            &wave_scope("w", "c"),
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
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id),
            &cove_updated(),
            &card_scope("not-my-card", "w", "c"),
            &cache,
            &wcc,
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
        let wcc = WaveCoveCache::new();
        wcc.insert(WaveId::from("home-wave"), CoveId::from("c"));
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("home-wave"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &cove_updated(),
            // Same card, but a different wave — must reject.
            &card_scope(id.as_str(), "other-wave", "c"),
            &cache,
            &wcc,
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
    fn worker_with_mismatched_scope_cove_rejected() {
        // Issue #234: even with `scope.card == self` and
        // `scope.wave == home_wave`, the gate must reject a
        // `scope.cove` that doesn't match the home wave's persisted
        // cove. Without this check, a Worker could forge any cove id
        // and the kernel would route the event to that cove's
        // subscribers — cross-cove isolation break, same shape as #232
        // one level up.
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        wcc.insert(WaveId::from("home-wave"), CoveId::from("home-cove"));
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("home-wave"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &cove_updated(),
            // Same card + same wave, but a different cove — must
            // reject before the event row lands.
            &card_scope(id.as_str(), "home-wave", "forged-cove"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(
                res,
                Err(RoleViolation::WorkerOutOfScope { ref scope, .. })
                    if scope.contains("scope.cove mismatch")
            ),
            "Worker forging scope.cove must be refused: {res:?}",
        );
    }

    #[test]
    fn empty_codex_card_id_rejected() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("")),
            &cove_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn empty_aispec_card_id_rejected() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::AiSpec(CardId::from("")),
            &cove_updated(),
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
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::AiCodex(CardId::from("never-seen")),
            &cove_updated(),
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

    #[test]
    fn spec_codex_hook_in_own_scope_ok() {
        // Bug A regression unit. The codex bridge runs as a subprocess
        // of codex regardless of the card's role; for a spec card, the
        // bridge still surfaces hook events through the
        // `AiCodex(spec_card)` actor. The gate accepts `Event::CodexHook`
        // from that actor as a pure lifecycle observation, scoped to the
        // card's own home (card_id + wave + cove). Mirror of
        // `worker_in_card_scope_ok` for the Spec arm.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, WaveId::from("w"));
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
    fn spec_codex_non_hook_event_still_rejected() {
        // The Spec-arm carveout is intentionally limited to
        // `Event::CodexHook`. Anything else from `AiCodex(spec_card)`
        // is still refused — write authority for spec-roled cards lives
        // with `AiSpec`, not `AiCodex`. CoveUpdated chosen because it's
        // a non-hook, non-wave-updated event variant.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &cove_updated(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForWave { .. })),
            "AiCodex(spec) non-hook event must still be refused: {res:?}",
        );
    }

    #[test]
    fn spec_codex_hook_out_of_scope_rejected() {
        // The carveout reuses the same scope cross-check as the Worker
        // arm — an `AiCodex(spec_card)` CodexHook with a forged wave id
        // is still refused. This pins that the new helper is wired into
        // the Spec arm, not just nominally accepted.
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        wcc.insert(WaveId::from("home-wave"), CoveId::from("c"));
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, WaveId::from("home-wave"));
        let res = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_hook(id.as_str()),
            // Same card, but a different wave — must reject even on
            // the carveout path.
            &card_scope(id.as_str(), "other-wave", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(
                res,
                Err(RoleViolation::WorkerOutOfScope { ref scope, .. })
                    if scope.contains("scope.wave mismatch")
            ),
            "AiCodex(spec) CodexHook with forged scope.wave must be refused: {res:?}",
        );
    }

    #[test]
    fn ai_claude_cannot_update_wave_even_with_known_card() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("claude-worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(
            matches!(res, Err(RoleViolation::NotSpecForWave { .. })),
            "AiClaude must never emit wave.updated regardless of role: {res:?}",
        );
    }

    #[test]
    fn claude_worker_in_card_scope_ok() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("claude-worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id.clone()),
            &cove_updated(),
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
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiClaude(id),
            &cove_updated(),
            &wave_scope("w", "c"),
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
        cache.insert(id.clone(), CardRole::Spec, WaveId::from("w"));
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
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::AiClaude(CardId::from("")),
            &cove_updated(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }

    #[test]
    fn plugin_actor_unrestricted() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::Plugin("hello-world".into()),
            &wave_updated(),
            &wave_scope("w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn kernel_dispatcher_unrestricted() {
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::KernelDispatcher,
            &wave_updated(),
            &wave_scope("w", "c"),
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
    //   * a worker card emitting `codex.job_requested` within its own
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

    fn codex_job_requested() -> Event {
        Event::CodexJobRequested {
            idempotency_key: "idem-1".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
        }
    }

    fn terminal_job_requested() -> Event {
        Event::TerminalJobRequested {
            idempotency_key: "idem-1".into(),
            cmd: "echo hi".into(),
            cwd: None,
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
    fn worker_cannot_emit_codex_job_requested_after_583() {
        // Issue #583. Section (2.5) of `enforce_role` now rejects any
        // Worker-actor `CodexJobRequested` regardless of scope. Replaces
        // the pre-#583 positive `worker_can_emit_codex_job_requested_in_own_scope`
        // which encoded the leaky pre-#583 behavior.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let err = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &codex_job_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("worker AI actor must be refused codex.job_requested");
        assert!(
            matches!(err, RoleViolation::NotSpecForDispatch { .. }),
            "expected NotSpecForDispatch, got {err:?}",
        );
    }

    #[test]
    fn worker_cannot_emit_terminal_job_requested_after_583() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
        let err = enforce_role(
            &ActorId::AiCodex(id.clone()),
            &terminal_job_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .expect_err("worker AI actor must be refused terminal.job_requested");
        assert!(
            matches!(err, RoleViolation::NotSpecForDispatch { .. }),
            "expected NotSpecForDispatch, got {err:?}",
        );
    }

    #[test]
    fn spec_can_emit_codex_job_requested_in_own_scope() {
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("spec-1");
        cache.insert(id.clone(), CardRole::Spec, WaveId::from("w"));
        let res = enforce_role(
            &ActorId::AiSpec(id.clone()),
            &codex_job_requested(),
            &card_scope(id.as_str(), "w", "c"),
            &cache,
            &wcc,
        );
        assert!(res.is_ok(), "spec emitting codex.job_requested: {res:?}");
    }

    #[test]
    fn worker_can_emit_task_completed_in_own_scope() {
        // The dispatcher push delivery path: workers report
        // task.completed scoped to themselves.
        let cache = CardRoleCache::new();
        let wcc = seeded_wcc();
        let id = CardId::from("worker-1");
        cache.insert(id.clone(), CardRole::Worker, WaveId::from("w"));
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
    fn empty_codex_card_id_rejected_on_new_variant() {
        // The section-1 empty-CardId guard is variant-agnostic — it
        // refuses any payload from an AiCodex actor whose CardId is
        // empty, including the new PR4 variants. Locks the contract so
        // a future refactor can't accidentally route the empty case
        // around the guard for a "harmless" new variant.
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
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
        // spec card as the requester of codex.job_requested, the empty
        // CardId path must still be rejected.
        let cache = CardRoleCache::new();
        let wcc = WaveCoveCache::new();
        let res = enforce_role(
            &ActorId::AiSpec(CardId::from("")),
            &codex_job_requested(),
            &EventScope::System,
            &cache,
            &wcc,
        );
        assert!(matches!(res, Err(RoleViolation::EmptyAiCardId)));
    }
}
