//! #985 slice 6 — the bounded wave-tree query surface.
//!
//! Every recursive walk over `waves.parent_wave_id` lives here, in ONE module,
//! because all of them share a single non-negotiable property: the tree is a
//! self-referencing table with no acyclicity constraint, so a recursive CTE
//! over it terminates ONLY because of the `depth <= ?2` predicate. `UNION`
//! (as opposed to `UNION ALL`) does NOT terminate these walks — PR-A proved
//! that empirically — and a carried non-id column defeats even `UNION`'s
//! duplicate elimination. Hence: every fragment below carries `id` (plus the
//! depth counter) and nothing else. A production-scope property test scans the
//! Rust strings and `.sql` files in both executing crates and rejects a
//! recursive member touching `parent_wave_id` unless its ON/WHERE predicate
//! upper-bounds that CTE alias's own depth. There is no registry to remember to
//! update: the property, rather than a list of known declarations, is the gate.
//!
//! PR-B adds the two downward walks: the creation-admission inventory count
//! (`child-wave`'s `prepare_tx`) and the tree membership enumeration that
//! feeds the deterministic quota split in `evaluate_schedulability`.

use sqlx::SqliteConnection;

use crate::error::Result;

/// Maximum legal wave-tree depth. A root sits at depth 0, so a legal tree has
/// at most four levels.
pub const MAX_WAVE_TREE_DEPTH: i64 = 3;

/// Kernel default for `waves.tree_task_budget` (the column is `NULL`-by-
/// default on purpose; see migration 0072).
pub const DEFAULT_TREE_TASK_BUDGET: i64 = 32;

/// Largest configurable tree budget. Member admission requires `N <= B`, so
/// this also puts a hard ceiling on the amount of work an in-transaction
/// whole-tree reprojection may perform.
pub const MAX_TREE_TASK_BUDGET: i64 = 64;

/// Kernel default for `waves.spec_task_ceiling`.
pub(crate) const DEFAULT_SPEC_TASK_CEILING: i64 = 32;

/// Decode nullable persisted limits at every enforcement point.
///
/// Keeping the NULL fallback and non-negative clamp here matters more than it
/// first appears: every enforcement point must decode nullable limits the
/// same way. If one path decodes the bare SQL column on its own, SQLite/sqlx
/// can turn NULL into a different value and silently remove an upper bound.
pub(crate) fn effective_limit(value: Option<i64>, default: i64) -> i64 {
    value.unwrap_or(default).max(0)
}

/// Both ancestor queries must expand this exact bounded fragment. `UNION`
/// cannot terminate a CTE carrying depth; the depth predicate is the only
/// cycle-termination guarantee.
macro_rules! bounded_wave_ancestor_cte {
    () => {
        r#"
WITH RECURSIVE up(id, parent_wave_id, depth) AS (
  SELECT id, parent_wave_id, 0 FROM waves WHERE id = ?1
  UNION ALL
  SELECT w.id, w.parent_wave_id, up.depth + 1
    FROM waves w JOIN up ON w.id = up.parent_wave_id
   WHERE up.depth <= ?2
)
"#
    };
}

/// The downward twin. Same rule, same reason: only `id` is carried, and the
/// `depth <= ?2` predicate is the sole termination guarantee. A 2-cycle
/// (`a.parent = b`, `b.parent = a`) walks downward forever without it.
macro_rules! bounded_wave_descendant_cte {
    () => {
        r#"
WITH RECURSIVE down(id, depth) AS (
  SELECT id, 0 FROM waves WHERE id = ?1
  UNION ALL
  SELECT w.id, down.depth + 1
    FROM waves w JOIN down ON w.parent_wave_id = down.id
   WHERE down.depth <= ?2
)
"#
    };
}

pub const WAVE_ROOT_DEPTH_SQL: &str = concat!(
    bounded_wave_ancestor_cte!(),
    "SELECT id AS root_id, depth AS parent_depth FROM up WHERE parent_wave_id IS NULL"
);

pub const WAVE_BOUNDED_PATH_SQL: &str = concat!(
    bounded_wave_ancestor_cte!(),
    "SELECT id, depth FROM up ORDER BY depth"
);

/// Tree membership in the deterministic `(created_at, id)` order the quota
/// split is defined over. `created_at` is read by the OUTER join, never
/// carried through the recursion.
pub const WAVE_TREE_MEMBERS_SQL: &str = concat!(
    bounded_wave_descendant_cte!(),
    "SELECT w.id, d.depth FROM waves w \
     JOIN (SELECT id, min(depth) AS depth FROM down GROUP BY id) d ON w.id = d.id \
     ORDER BY w.created_at, w.id"
);

/// Membership plus fixed (non-cullable in this projection) spec occupancy.
/// The outer correlated count preserves the same recursive shape/order while
/// detecting an upgrade member already above its deterministic share. Block
/// pending rows are excluded because they re-enter projection as candidates;
/// all non-block live rows (notably legacy) are immutable occupancy.
pub const WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL: &str = concat!(
    bounded_wave_descendant_cte!(),
    "SELECT w.id, d.depth, (SELECT count(*) FROM tasks t \
       WHERE t.wave_id=w.id AND t.declared_by='spec' AND ( \
         (t.origin='block' AND t.status IN ('dispatched','running','verifying')) \
         OR (t.origin!='block' AND t.status NOT IN ('done','failed','canceled')) \
       )) AS fixed_live \
     FROM waves w \
     JOIN (SELECT id, min(depth) AS depth FROM down GROUP BY id) d ON w.id = d.id \
     ORDER BY w.created_at, w.id"
);

/// Whole-tree non-terminal spec inventory — enforcement point one.
pub const WAVE_TREE_SPEC_INVENTORY_SQL: &str = concat!(
    bounded_wave_descendant_cte!(),
    "SELECT count(*) FROM tasks t \
     JOIN (SELECT DISTINCT id FROM down) d ON t.wave_id = d.id \
     WHERE t.declared_by = 'spec' AND t.status NOT IN ('done', 'failed', 'canceled')"
);
/// The deterministic share of `budget` handed to the member at `index` of a
/// tree with `members` waves, ordered by `(created_at, id)`.
///
/// `floor(B / N)` for everyone, and the remainder `r = B mod N` distributed
/// one apiece to the first `r` members. `Σ share = B` exactly — that identity
/// is what makes `Σ_v live_spec(v) ≤ B` the real tree bound.
///
/// Purely a function of the tree's SHAPE. It reads no projection output (no
/// `pending` row, no sibling admission), which is precisely why the tree term
/// leaves "rebuild ≡ incremental" (D.1 #11) intact.
pub fn deterministic_share(budget: i64, members: i64, index: i64) -> i64 {
    if members <= 0 {
        return 0;
    }
    let budget = budget.max(0);
    let base = budget / members;
    let remainder = budget % members;
    base + i64::from(index < remainder)
}

/// Whether enforcement point one may add a member without enforcement point
/// two assigning a zero share to any wave.
pub fn can_add_tree_member(budget: i64, members: i64) -> bool {
    members.saturating_add(1) <= budget.max(0)
}

/// The tree contribution to a wave's effective ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveTreeTerm {
    /// The wave IS in a tree but its root could not be resolved (broken parent
    /// link, cycle, or a chain deeper than [`MAX_WAVE_TREE_DEPTH`]). Callers
    /// must fail closed: a single broken link would otherwise leave a whole
    /// subtree unbounded.
    RootUnresolved,
    Share(TreeShare),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeShare {
    pub root_id: String,
    pub budget: i64,
    pub members: i64,
    /// Zero-based position in the deterministic `(created_at, id)` order.
    /// Diagnostics use it to name the first B that increases THIS member's
    /// share instead of assuming `B + 1` helps every remainder position.
    pub member_index: i64,
    pub share: i64,
    /// An upgrade/corruption state has at least one member whose immutable
    /// occupancy exceeds its share. No member may admit a new block until the
    /// excess terminates; otherwise a less-full sibling could grow Σ above B.
    pub admission_frozen: bool,
    /// First legal B at which every member's immutable occupancy fits its
    /// deterministic share. `None` means either no freeze or no such B within
    /// [`MAX_TREE_TASK_BUDGET`].
    pub minimum_budget_to_unfreeze: Option<i64>,
}

/// [`WaveTreeTerm`] plus the countable seam used by whole-tree reprojection to
/// reject an accidental per-member recursive walk. Ordinary evaluation uses
/// two bounded recursive statements even for a singleton; each CTE then has
/// one row, so singleton work remains O(1) without a separate semantic path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveTreeTermOutcome {
    pub term: WaveTreeTerm,
    pub tree_cte_queries: u32,
}

/// Resolve `wave_id`'s tree term.
pub async fn wave_tree_term(
    conn: &mut SqliteConnection,
    wave_id: &str,
) -> Result<WaveTreeTermOutcome> {
    let mut queries = 0u32;
    let roots: Vec<(String, i64)> = sqlx::query_as(WAVE_ROOT_DEPTH_SQL)
        .bind(wave_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await?;
    queries += 1;
    let [(root_id, depth)] = roots.as_slice() else {
        return Ok(WaveTreeTermOutcome {
            term: WaveTreeTerm::RootUnresolved,
            tree_cte_queries: queries,
        });
    };
    if *depth > MAX_WAVE_TREE_DEPTH {
        return Ok(WaveTreeTermOutcome {
            term: WaveTreeTerm::RootUnresolved,
            tree_cte_queries: queries,
        });
    }
    let root_id = root_id.clone();
    let members: Vec<(String, i64, i64)> = sqlx::query_as(WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL)
        .bind(&root_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await?;
    queries += 1;
    // Poisoned data: a member deeper than the legal bound, or a tree that does
    // not contain the wave we started from. Both mean the shape we would
    // divide the budget over is not the shape the wave actually lives in.
    let budget = wave_tree_budget(&mut *conn, &root_id).await?;
    let term = tree_share_from_member_inventory(root_id, wave_id, budget, &members);
    Ok(WaveTreeTermOutcome {
        term,
        tree_cte_queries: queries,
    })
}

#[cfg(test)]
fn tree_share_from_members(
    root_id: String,
    wave_id: &str,
    budget: i64,
    members: &[(String, i64)],
) -> WaveTreeTerm {
    tree_share_from_members_with_freeze(root_id, wave_id, budget, members, false, None)
}

fn tree_share_from_member_inventory(
    root_id: String,
    wave_id: &str,
    budget: i64,
    members: &[(String, i64, i64)],
) -> WaveTreeTerm {
    let shape = members
        .iter()
        .map(|(id, depth, _)| (id.clone(), *depth))
        .collect::<Vec<_>>();
    let fixed_live = members
        .iter()
        .map(|(_, _, fixed_live)| *fixed_live)
        .collect::<Vec<_>>();
    let (admission_frozen, minimum_budget_to_unfreeze) = tree_admission_freeze(budget, &fixed_live);
    tree_share_from_members_with_freeze(
        root_id,
        wave_id,
        budget,
        &shape,
        admission_frozen,
        minimum_budget_to_unfreeze,
    )
}

/// Derive the tree-wide freeze from fixed per-member inventory in the same
/// deterministic member order used for shares. Whole-tree rebuilds reuse this
/// pure calculation after reading all members once, so their precomputed terms
/// cannot accidentally exempt an over-share legacy transfer.
pub fn tree_admission_freeze(budget: i64, fixed_live: &[i64]) -> (bool, Option<i64>) {
    let count = fixed_live.len() as i64;
    let admission_frozen = fixed_live
        .iter()
        .enumerate()
        .any(|(index, fixed)| *fixed > deterministic_share(budget, count, index as i64));
    let minimum_budget_to_unfreeze = admission_frozen.then(|| {
        (budget.saturating_add(1)..=MAX_TREE_TASK_BUDGET).find(|candidate| {
            fixed_live.iter().enumerate().all(|(index, fixed)| {
                *fixed <= deterministic_share(*candidate, count, index as i64)
            })
        })
    });
    (admission_frozen, minimum_budget_to_unfreeze.flatten())
}

fn tree_share_from_members_with_freeze(
    root_id: String,
    wave_id: &str,
    budget: i64,
    members: &[(String, i64)],
    admission_frozen: bool,
    minimum_budget_to_unfreeze: Option<i64>,
) -> WaveTreeTerm {
    let over_deep = members
        .iter()
        .any(|(_, depth)| *depth > MAX_WAVE_TREE_DEPTH);
    let index = members.iter().position(|(id, _)| id == wave_id);
    let (Some(index), false) = (index, over_deep) else {
        return WaveTreeTerm::RootUnresolved;
    };
    let count = members.len() as i64;
    WaveTreeTerm::Share(TreeShare {
        root_id,
        budget,
        members: count,
        member_index: index as i64,
        share: deterministic_share(budget, count, index as i64),
        admission_frozen,
        minimum_budget_to_unfreeze,
    })
}

/// The root's configured budget, or the kernel default when unset.
pub async fn wave_tree_budget(conn: &mut SqliteConnection, root_id: &str) -> Result<i64> {
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT tree_task_budget FROM waves WHERE id = ?1")
            .bind(root_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(effective_limit(
        row.and_then(|(budget,)| budget),
        DEFAULT_TREE_TASK_BUDGET,
    ))
}

/// Whole-tree non-terminal `declared_by='spec'` row count, rooted at `root_id`.
pub async fn wave_tree_spec_inventory(conn: &mut SqliteConnection, root_id: &str) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as(WAVE_TREE_SPEC_INVENTORY_SQL)
        .bind(root_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_one(&mut *conn)
        .await?;
    Ok(count)
}

/// Number of waves in the bounded member set rooted at `root_id`.
pub async fn wave_tree_member_count(conn: &mut SqliteConnection, root_id: &str) -> Result<i64> {
    let members: Vec<(String, i64)> = sqlx::query_as(WAVE_TREE_MEMBERS_SQL)
        .bind(root_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await?;
    Ok(members.len() as i64)
}

/// Per-member whole-tree non-terminal `declared_by='spec'` inventory.
///
/// The whole-tree reprojection seam uses this after deleting excess pending
/// rows. A remaining member over its new share can only be over because of
/// already in-flight work; callers then reject the shape/budget change rather
/// than committing a tree for which `sum(live_spec) <= B` is false.
pub async fn wave_tree_spec_inventory_by_member(
    conn: &mut SqliteConnection,
    root_id: &str,
) -> Result<Vec<(String, i64)>> {
    Ok(sqlx::query_as(concat!(
        bounded_wave_descendant_cte!(),
        "SELECT d.id, count(t.id) FROM (SELECT DISTINCT id FROM down) d \
         LEFT JOIN tasks t ON t.wave_id=d.id AND t.declared_by='spec' \
           AND t.status NOT IN ('done','failed','canceled') \
         GROUP BY d.id ORDER BY d.id"
    ))
    .bind(root_id)
    .bind(MAX_WAVE_TREE_DEPTH + 1)
    .fetch_all(&mut *conn)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_member_sql_keeps_its_total_order_definition() {
        let without_line_comments = WAVE_TREE_MEMBERS_SQL
            .lines()
            .map(|line| line.split("--").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let mut without_comments = String::new();
        let mut rest = without_line_comments.as_str();
        while let Some(start) = rest.find("/*") {
            without_comments.push_str(&rest[..start]);
            let Some(end) = rest[start + 2..].find("*/") else {
                rest = "";
                break;
            };
            rest = &rest[start + 2 + end + 2..];
        }
        without_comments.push_str(rest);
        let normalized = without_comments
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            normalized.contains("ORDER BY w.created_at, w.id"),
            "quota membership lost its deterministic (created_at, id) order"
        );
    }

    #[test]
    fn shares_sum_to_the_budget_including_the_remainder() {
        for (budget, members) in [
            (32, 1),
            (32, 3),
            (32, 5),
            (32, 10),
            (7, 3),
            (2, 5),
            (0, 4),
            (1, 1),
        ] {
            let total: i64 = (0..members)
                .map(|index| deterministic_share(budget, members, index))
                .sum();
            assert_eq!(total, budget, "budget={budget} members={members}");
            // The remainder goes to a PREFIX of the order, so shares are
            // non-increasing and differ by at most one.
            let shares: Vec<i64> = (0..members)
                .map(|index| deterministic_share(budget, members, index))
                .collect();
            assert!(shares.windows(2).all(|w| w[0] >= w[1]), "{shares:?}");
            assert!(shares[0] - shares[members as usize - 1] <= 1, "{shares:?}");
        }
    }

    #[test]
    fn every_declaration_sequence_within_member_shares_respects_whole_tree_budget() {
        fn visit(shares: &[i64], index: usize, live_total: i64, budget: i64) {
            if index == shares.len() {
                assert!(
                    live_total <= budget,
                    "live_total={live_total} budget={budget} shares={shares:?}"
                );
                return;
            }
            // Any declaration/claim order ends in one of these per-member live
            // counts because projection never admits above the member share.
            for live in 0..=shares[index] {
                visit(shares, index + 1, live_total + live, budget);
            }
        }

        for budget in 0..=12 {
            for members in 1..=12 {
                let shares = (0..members)
                    .map(|index| deterministic_share(budget, members, index))
                    .collect::<Vec<_>>();
                visit(&shares, 0, 0, budget);
            }
        }
    }

    #[test]
    fn enforcement_points_are_compatible_for_every_budget_and_member_count() {
        for budget in 0..=64 {
            for members in 1..=64 {
                let after = members + 1;
                let every_member_gets_a_share =
                    (0..after).all(|index| deterministic_share(budget, after, index) > 0);
                assert_eq!(
                    can_add_tree_member(budget, members),
                    every_member_gets_a_share,
                    "admission and quota split disagree: budget={budget}, members={after}"
                );
            }
        }
    }

    #[test]
    fn a_resolved_member_set_that_omits_the_caller_fails_closed() {
        let members = vec![("root".to_owned(), 0), ("sibling".to_owned(), 1)];
        assert_eq!(
            tree_share_from_members("root".to_owned(), "caller", 8, &members),
            WaveTreeTerm::RootUnresolved
        );
    }
}
