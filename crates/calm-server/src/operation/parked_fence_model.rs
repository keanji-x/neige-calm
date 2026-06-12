//! Exhaustive-interleaving model check for the parked-operation fence
//! protocol (#653, design doc §4.4 orderings A/B/C).
//!
//! Each model step mirrors ONE atomic SQL statement in `operation/mod.rs`
//! (single UPDATEs are atomic; `complete_parked_tx` is atomic because it
//! runs inside `begin_immediate_tx`). If you change a WHERE predicate or a
//! SET list on the real queries, update the matching step here — the
//! mapping is:
//!
//! | model step          | real code                              |
//! |----------------------|----------------------------------------|
//! | `ClaimUpdateSteady`  | `claim_parked` UPDATE                  |
//! | `ClaimUpdateBoot`    | `claim_parked_for_boot` UPDATE         |
//! | `ClaimFetch`         | `fetch_claimed_parked` SELECT          |
//! | `Complete`           | `complete_parked_tx` (whole tx)        |
//! | `MarkFailed`         | `mark_failed` UPDATE                   |
//! | `SetCompensating`    | `set_compensating` UPDATE              |
//! | `ClearLeaseBoot`     | `clear_parked_lease_for_boot` UPDATE   |
//!
//! The mutant tests at the bottom re-run the same explorations with a
//! deliberately weakened predicate and assert the harness FINDS the
//! violation — each mutant is the shape of a real bug caught during the
//! #662 review rounds, so the harness is known-sensitive to this class.

use std::collections::BTreeSet;

const NOW: i64 = 1_000;
const FUTURE: i64 = NOW + 60_000;
const PAST: i64 = NOW - 1;

/// Owner ids: actor index stamps its own claims; pre-crash abandoned
/// leases use `DEAD_OWNER`.
const DEAD_OWNER: usize = usize::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ModelPhase {
    Parked,
    Succeeded,
    Failed,
    Compensating,
}

impl ModelPhase {
    fn is_terminal_or_compensating(self) -> bool {
        !matches!(self, ModelPhase::Parked)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Row {
    phase: ModelPhase,
    lease: Option<(usize, i64)>,
}

impl Row {
    fn parked_unleased() -> Self {
        Row {
            phase: ModelPhase::Parked,
            lease: None,
        }
    }

    /// Crashed enforcer left a claim whose lease_until is still in the
    /// future (the round-3/round-4 review findings' precondition).
    fn parked_abandoned_future_lease() -> Self {
        Row {
            phase: ModelPhase::Parked,
            lease: Some((DEAD_OWNER, FUTURE)),
        }
    }

    /// Same abandonment, but the lease TTL has already run out — the
    /// steady claim predicate is allowed to take over this row.
    fn parked_abandoned_expired_lease() -> Self {
        Row {
            phase: ModelPhase::Parked,
            lease: Some((DEAD_OWNER, PAST)),
        }
    }
}

/// Which predicate `ClaimFetch` uses. `LeaseAndPhase` is the shipped code;
/// `IdOnly` is the round-1 P1 bug (fetch did not validate the new lease /
/// phase, so a raced completion was returned as a successful claim).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchPredicate {
    LeaseAndPhase,
    IdOnly,
}

/// Whether `Complete` clears the lease. `true` is the shipped code; `false`
/// is the design-v1 hole (ordering B: a lease-fenced `mark_failed` would
/// overwrite a committed completion).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompleteClearsLease {
    Yes,
    No,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Semantics {
    fetch: FetchPredicate,
    complete_clears_lease: CompleteClearsLease,
}

const SHIPPED: Semantics = Semantics {
    fetch: FetchPredicate::LeaseAndPhase,
    complete_clears_lease: CompleteClearsLease::Yes,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    ClaimUpdateSteady,
    ClaimUpdateBoot,
    ClaimFetch,
    Complete { ok: bool },
    MarkFailed,
    SetCompensating,
    ClearLeaseBoot,
}

/// A terminal write that actually landed (rows_affected == 1 on a write
/// that sets a terminal/compensating phase).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct TerminalWrite {
    actor: usize,
    phase_after: u8, // discriminant for set ordering
}

fn apply(sem: Semantics, row: &mut Row, actor: usize, step: Step) -> (bool, Option<TerminalWrite>) {
    match step {
        // UPDATE … WHERE phase='parked' AND (lease IS NULL OR until < now)
        Step::ClaimUpdateSteady => {
            let free = match row.lease {
                None => true,
                Some((_, until)) => until < NOW,
            };
            if row.phase == ModelPhase::Parked && free {
                row.lease = Some((actor, FUTURE));
                (true, None)
            } else {
                (false, None)
            }
        }
        // UPDATE … WHERE phase='parked'  (boot ignores abandoned leases)
        Step::ClaimUpdateBoot => {
            if row.phase == ModelPhase::Parked {
                row.lease = Some((actor, FUTURE));
                (true, None)
            } else {
                (false, None)
            }
        }
        // SELECT … WHERE id AND lease_owner=<mine> AND phase='parked'
        Step::ClaimFetch => match sem.fetch {
            FetchPredicate::LeaseAndPhase => (
                row.phase == ModelPhase::Parked && row.lease.map(|(o, _)| o) == Some(actor),
                None,
            ),
            // round-1 P1 mutant: the row exists, so the fetch "succeeds"
            // regardless of who owns it or whether it is still parked.
            FetchPredicate::IdOnly => (true, None),
        },
        // begin_immediate_tx { SELECT; UPDATE … WHERE phase='parked' }
        Step::Complete { ok } => {
            if row.phase == ModelPhase::Parked {
                row.phase = if ok {
                    ModelPhase::Succeeded
                } else {
                    ModelPhase::Failed
                };
                if sem.complete_clears_lease == CompleteClearsLease::Yes {
                    row.lease = None;
                }
                (
                    true,
                    Some(TerminalWrite {
                        actor,
                        phase_after: row.phase as u8,
                    }),
                )
            } else {
                // AlreadyResolved: the call "succeeds" but writes nothing.
                (true, None)
            }
        }
        // UPDATE … SET phase='failed', lease=NULL WHERE lease_owner=<mine>
        Step::MarkFailed => {
            if row.lease.map(|(o, _)| o) == Some(actor) {
                row.phase = ModelPhase::Failed;
                row.lease = None;
                (
                    true,
                    Some(TerminalWrite {
                        actor,
                        phase_after: row.phase as u8,
                    }),
                )
            } else {
                (false, None)
            }
        }
        // UPDATE … SET phase='compensating', lease=NULL WHERE lease_owner=<mine>
        Step::SetCompensating => {
            if row.lease.map(|(o, _)| o) == Some(actor) {
                row.phase = ModelPhase::Compensating;
                row.lease = None;
                (
                    true,
                    Some(TerminalWrite {
                        actor,
                        phase_after: row.phase as u8,
                    }),
                )
            } else {
                (false, None)
            }
        }
        // UPDATE … SET lease=NULL WHERE phase='parked'
        Step::ClearLeaseBoot => {
            if row.phase == ModelPhase::Parked {
                row.lease = None;
            }
            (true, None)
        }
    }
}

#[derive(Clone)]
struct Actor {
    script: Vec<Step>,
    pc: usize,
    aborted: bool,
}

impl Actor {
    fn new(script: Vec<Step>) -> Self {
        Actor {
            script,
            pc: 0,
            aborted: false,
        }
    }

    fn done(&self) -> bool {
        self.aborted || self.pc >= self.script.len()
    }
}

/// One fully-explored leaf: the final row plus every terminal write that
/// landed along the way, in landing order.
struct Leaf {
    row: Row,
    terminal_writes: Vec<TerminalWrite>,
}

fn explore(sem: Semantics, row: Row, actors: Vec<Actor>) -> Vec<Leaf> {
    let mut leaves = Vec::new();
    let mut writes = Vec::new();
    explore_rec(sem, row, actors, &mut writes, &mut leaves);
    leaves
}

fn explore_rec(
    sem: Semantics,
    row: Row,
    actors: Vec<Actor>,
    writes: &mut Vec<TerminalWrite>,
    leaves: &mut Vec<Leaf>,
) {
    let mut any = false;
    for i in 0..actors.len() {
        if actors[i].done() {
            continue;
        }
        any = true;
        let mut row2 = row.clone();
        let mut actors2 = actors.clone();
        let step = actors2[i].script[actors2[i].pc];
        let (ok, write) = apply(sem, &mut row2, i, step);
        if ok {
            actors2[i].pc += 1;
        } else {
            // Real callers treat a missed fence as "lost the race" and yield.
            actors2[i].aborted = true;
        }
        // Global invariant, checked on every reachable state: a settled row
        // never carries a lease (every settling write clears it). The
        // design-v1 mutant intentionally violates this.
        if sem == SHIPPED && row2.phase.is_terminal_or_compensating() {
            assert!(
                row2.lease.is_none(),
                "settled row still leased after {step:?}: {row2:?}"
            );
        }
        if let Some(w) = write {
            writes.push(w);
        }
        explore_rec(sem, row2, actors2, writes, leaves);
        if write.is_some() {
            writes.pop();
        }
    }
    if !any {
        leaves.push(Leaf {
            row,
            terminal_writes: writes.clone(),
        });
    }
}

fn completer(ok: bool) -> Vec<Step> {
    vec![Step::Complete { ok }]
}

/// Sweep past-deadline arm / boot Fail arm (kill has no row effect).
fn steady_enforcer() -> Vec<Step> {
    vec![Step::ClaimUpdateSteady, Step::ClaimFetch, Step::MarkFailed]
}

fn boot_enforcer() -> Vec<Step> {
    vec![Step::ClaimUpdateBoot, Step::ClaimFetch, Step::MarkFailed]
}

fn canceler() -> Vec<Step> {
    vec![
        Step::ClaimUpdateSteady,
        Step::ClaimFetch,
        Step::SetCompensating,
    ]
}

/// Every leaf must contain exactly one settling write, and the final phase
/// must be the one that write produced.
fn assert_single_winner(leaves: &[Leaf]) {
    assert!(!leaves.is_empty());
    for leaf in leaves {
        assert_eq!(
            leaf.terminal_writes.len(),
            1,
            "expected exactly one settling write, got {:?} (row {:?})",
            leaf.terminal_writes,
            leaf.row
        );
        assert!(leaf.row.phase.is_terminal_or_compensating());
        assert_eq!(leaf.row.phase as u8, leaf.terminal_writes[0].phase_after);
        assert!(leaf.row.lease.is_none());
    }
}

fn count_violations(sem: Semantics, row: Row, actors: Vec<Actor>) -> usize {
    explore(sem, row, actors)
        .iter()
        .filter(|leaf| {
            leaf.terminal_writes.len() != 1
                || !leaf.row.phase.is_terminal_or_compensating()
                || leaf.row.phase as u8 != leaf.terminal_writes[0].phase_after
        })
        .count()
}

#[test]
fn completer_vs_steady_enforcer_single_winner() {
    // Orderings A, B, C from doc §4.4, exhaustively.
    for ok in [true, false] {
        let leaves = explore(
            SHIPPED,
            Row::parked_unleased(),
            vec![Actor::new(completer(ok)), Actor::new(steady_enforcer())],
        );
        assert_single_winner(&leaves);
        // Both outcomes are reachable: completion wins in some interleaving,
        // enforcement in another.
        let winners: BTreeSet<usize> = leaves.iter().map(|l| l.terminal_writes[0].actor).collect();
        assert_eq!(winners.len(), 2, "both actors must be able to win");
    }
}

#[test]
fn completer_vs_canceler_single_winner() {
    let leaves = explore(
        SHIPPED,
        Row::parked_unleased(),
        vec![Actor::new(completer(true)), Actor::new(canceler())],
    );
    assert_single_winner(&leaves);
    for leaf in leaves {
        // Either the completion landed (succeeded) or the cancel did
        // (compensating) — never a failed/half state.
        assert!(matches!(
            leaf.row.phase,
            ModelPhase::Succeeded | ModelPhase::Compensating
        ));
    }
}

#[test]
fn completer_vs_boot_enforcer_over_abandoned_lease() {
    let leaves = explore(
        SHIPPED,
        Row::parked_abandoned_future_lease(),
        vec![Actor::new(completer(true)), Actor::new(boot_enforcer())],
    );
    assert_single_winner(&leaves);
}

#[test]
fn double_complete_exactly_one() {
    let leaves = explore(
        SHIPPED,
        Row::parked_unleased(),
        vec![Actor::new(completer(true)), Actor::new(completer(false))],
    );
    assert_single_winner(&leaves);
}

#[test]
fn two_steady_enforcers_single_claim_single_terminal() {
    let leaves = explore(
        SHIPPED,
        Row::parked_unleased(),
        vec![Actor::new(steady_enforcer()), Actor::new(steady_enforcer())],
    );
    assert_single_winner(&leaves);
}

#[test]
fn boot_lease_clear_is_benign_alongside_completion() {
    let leaves = explore(
        SHIPPED,
        Row::parked_abandoned_future_lease(),
        vec![
            Actor::new(completer(true)),
            Actor::new(vec![Step::ClearLeaseBoot]),
        ],
    );
    for leaf in &leaves {
        assert_eq!(leaf.row.phase, ModelPhase::Succeeded);
        assert!(leaf.row.lease.is_none());
    }
    assert_single_winner(&leaves);
}

/// Round-3 review finding, pinned in both directions: a boot enforcer must
/// settle an op whose lease was abandoned by a crashed process; a STEADY
/// enforcer must NOT get past such a lease (it is how a live enforcer is
/// protected from being stomped). The second half doubles as the round-3
/// mutant: a boot arm wrongly using the steady claim is exactly a steady
/// enforcer here, and it stalls.
#[test]
fn abandoned_lease_boot_vs_steady_liveness() {
    let boot = explore(
        SHIPPED,
        Row::parked_abandoned_future_lease(),
        vec![Actor::new(boot_enforcer())],
    );
    assert_single_winner(&boot);

    let steady = explore(
        SHIPPED,
        Row::parked_abandoned_future_lease(),
        vec![Actor::new(steady_enforcer())],
    );
    for leaf in steady {
        assert!(leaf.terminal_writes.is_empty());
        assert_eq!(leaf.row.phase, ModelPhase::Parked);
    }

    // Once the abandoned lease's TTL runs out, the steady claim may take
    // over — the stall above is bounded, not a deadlock.
    let steady_expired = explore(
        SHIPPED,
        Row::parked_abandoned_expired_lease(),
        vec![Actor::new(steady_enforcer())],
    );
    assert_single_winner(&steady_expired);
}

/// Round-4 review finding: after the boot LeaveParked clear, a steady
/// enforcer (or canceler) proceeds immediately instead of waiting out the
/// abandoned lease.
#[test]
fn boot_clear_unblocks_steady_enforcement() {
    let mut row = Row::parked_abandoned_future_lease();
    let (ok, _) = apply(SHIPPED, &mut row, 9, Step::ClearLeaseBoot);
    assert!(ok);
    let leaves = explore(SHIPPED, row, vec![Actor::new(steady_enforcer())]);
    assert_single_winner(&leaves);
}

// ---- mutants: the harness must DETECT the historical bug shapes ----

/// Round-1 P1, pinned as the exact interleaving that bit: claim UPDATE
/// lands, a completion settles the row, THEN the claim's fetch runs. The
/// shipped fetch (lease+phase predicate) must report the claim lost; the
/// id-only mutant reports the settled row as a successful claim — the
/// caller would then make cancel/kill decisions on an op that is already
/// terminal. Mirrors the `claim_parked` regression test in mod.rs.
#[test]
fn mutant_fetch_id_only_returns_settled_row_as_claimed() {
    for sem in [
        SHIPPED,
        Semantics {
            fetch: FetchPredicate::IdOnly,
            ..SHIPPED
        },
    ] {
        let mut row = Row::parked_unleased();
        let (claimed, _) = apply(sem, &mut row, 1, Step::ClaimUpdateSteady);
        assert!(claimed);
        let (completed, write) = apply(sem, &mut row, 0, Step::Complete { ok: true });
        assert!(completed && write.is_some());
        let (fetch_ok, _) = apply(sem, &mut row, 1, Step::ClaimFetch);
        match sem.fetch {
            FetchPredicate::LeaseAndPhase => assert!(
                !fetch_ok,
                "shipped fetch must treat a raced completion as a lost claim"
            ),
            FetchPredicate::IdOnly => assert!(
                fetch_ok,
                "mutant did not reproduce the round-1 bug shape — model drifted?"
            ),
        }
    }
}

/// Design-v1 hole (doc §11 round-1 finding): if complete_parked_tx does
/// NOT clear the lease, ordering B (claim → complete → mark_failed)
/// lets the lease-fenced mark_failed overwrite a committed completion —
/// two settling writes, final phase contradicts the first write.
#[test]
fn mutant_complete_keeps_lease_is_caught() {
    let sem = Semantics {
        complete_clears_lease: CompleteClearsLease::No,
        ..SHIPPED
    };
    let violations = count_violations(
        sem,
        Row::parked_unleased(),
        vec![Actor::new(completer(true)), Actor::new(steady_enforcer())],
    );
    assert!(
        violations > 0,
        "harness failed to detect the ordering-B lease hole"
    );
}
