//! #985 slice 6 — the bounded wave-tree query surface.
//!
//! Every recursive walk over `waves.parent_wave_id` lives here, in ONE module,
//! because all of them share a single non-negotiable property: the tree is a
//! self-referencing table with no acyclicity constraint, so a recursive CTE
//! over it terminates ONLY because of the `depth <= ?2` predicate. `UNION`
//! (as opposed to `UNION ALL`) does NOT terminate these walks — PR-A proved
//! that empirically — and a carried non-id column defeats even `UNION`'s
//! duplicate elimination. Hence: every fragment below carries `id` (plus the
//! depth counter) and nothing else, and every fragment is registered in
//! [`BOUNDED_WAVE_TREE_SQL`], which the static gate at the bottom of this file
//! walks. The gate additionally counts macro expansions in this file's own
//! source so that adding a new bounded-tree SQL constant WITHOUT registering
//! it turns the gate red — a name list with no machine link to its members is
//! exactly the shape #985 §7.1 forbids.
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

/// Whole-tree non-terminal spec inventory — enforcement point one.
pub const WAVE_TREE_SPEC_INVENTORY_SQL: &str = concat!(
    bounded_wave_descendant_cte!(),
    "SELECT count(*) FROM tasks t \
     JOIN (SELECT DISTINCT id FROM down) d ON t.wave_id = d.id \
     WHERE t.declared_by = 'spec' AND t.status NOT IN ('done', 'failed', 'canceled')"
);
/// Every bounded-tree SQL constant in this module. The static gate walks this
/// list; a fragment missing from it is a fragment nothing checks.
pub const BOUNDED_WAVE_TREE_SQL: &[(&str, &str)] = &[
    ("WAVE_ROOT_DEPTH_SQL", WAVE_ROOT_DEPTH_SQL),
    ("WAVE_BOUNDED_PATH_SQL", WAVE_BOUNDED_PATH_SQL),
    ("WAVE_TREE_MEMBERS_SQL", WAVE_TREE_MEMBERS_SQL),
    ("WAVE_TREE_SPEC_INVENTORY_SQL", WAVE_TREE_SPEC_INVENTORY_SQL),
];

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
    /// `parent_wave_id IS NULL`, no child exists, and `tree_task_budget` is
    /// NULL: the default tree term cannot tighten the default wave ceiling, so
    /// no recursive walk runs. An explicitly configured budget still returns
    /// `Share { members: 1 }` for this same shape.
    NotInTree,
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
    pub share: i64,
}

/// [`WaveTreeTerm`] plus the countable seam that proves the non-tree
/// short-circuit really skips the recursive walks.
///
/// The count is the number of *recursive* tree statements executed. It is 0
/// for a wave that is not in a tree — an assertion that can actually fail,
/// unlike "a non-tree wave behaves byte-identically", which is vacuous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveTreeTermOutcome {
    pub term: WaveTreeTerm,
    pub tree_cte_queries: u32,
}

/// Is recursion needed, and did the user explicitly configure the singleton
/// root's budget? One non-recursive statement.
async fn wave_tree_shortcut(
    conn: &mut SqliteConnection,
    wave_id: &str,
) -> Result<Option<(bool, Option<i64>)>> {
    let row: Option<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT CASE WHEN w.parent_wave_id IS NOT NULL \
           OR EXISTS(SELECT 1 FROM waves c WHERE c.parent_wave_id = w.id) \
         THEN 1 ELSE 0 END, w.tree_task_budget FROM waves w WHERE w.id = ?1",
    )
    .bind(wave_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(in_tree, budget)| (in_tree == 1, budget)))
}

/// Resolve `wave_id`'s tree term.
pub async fn wave_tree_term(
    conn: &mut SqliteConnection,
    wave_id: &str,
) -> Result<WaveTreeTermOutcome> {
    if let Some((false, configured_budget)) = wave_tree_shortcut(&mut *conn, wave_id).await? {
        let term = match configured_budget {
            None => WaveTreeTerm::NotInTree,
            Some(budget) => WaveTreeTerm::Share(TreeShare {
                root_id: wave_id.to_owned(),
                budget: budget.max(0),
                members: 1,
                share: budget.max(0),
            }),
        };
        return Ok(WaveTreeTermOutcome {
            term,
            tree_cte_queries: 0,
        });
    }
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
    let members: Vec<(String, i64)> = sqlx::query_as(WAVE_TREE_MEMBERS_SQL)
        .bind(&root_id)
        .bind(MAX_WAVE_TREE_DEPTH + 1)
        .fetch_all(&mut *conn)
        .await?;
    queries += 1;
    // Poisoned data: a member deeper than the legal bound, or a tree that does
    // not contain the wave we started from. Both mean the shape we would
    // divide the budget over is not the shape the wave actually lives in.
    let budget = wave_tree_budget(&mut *conn, &root_id).await?;
    let term = tree_share_from_members(root_id, wave_id, budget, &members);
    Ok(WaveTreeTermOutcome {
        term,
        tree_cte_queries: queries,
    })
}

fn tree_share_from_members(
    root_id: String,
    wave_id: &str,
    budget: i64,
    members: &[(String, i64)],
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
        share: deterministic_share(budget, count, index as i64),
    })
}

/// The root's configured budget, or the kernel default when unset.
pub async fn wave_tree_budget(conn: &mut SqliteConnection, root_id: &str) -> Result<i64> {
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT tree_task_budget FROM waves WHERE id = ?1")
            .bind(root_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row
        .and_then(|(budget,)| budget)
        .unwrap_or(DEFAULT_TREE_TASK_BUDGET)
        .max(0))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered fragment keeps its only cycle-termination guard.
    #[test]
    fn bounded_tree_sql_keeps_its_only_cycle_termination_guard() {
        assert!(!BOUNDED_WAVE_TREE_SQL.is_empty());
        for (_, sql) in BOUNDED_WAVE_TREE_SQL {
            let bounded =
                sql.contains("WHERE up.depth <= ?2") || sql.contains("WHERE down.depth <= ?2");
            assert!(bounded, "unbounded recursive tree SQL: {sql}");
            assert!(sql.contains("UNION ALL"), "{sql}");
        }
    }

    #[test]
    fn quota_member_sql_keeps_its_total_order_definition() {
        assert!(
            WAVE_TREE_MEMBERS_SQL.contains("ORDER BY w.created_at, w.id"),
            "quota membership lost its deterministic (created_at, id) order"
        );
    }

    /// Parse the Rust file, enumerate every const whose initializer expands a
    /// bounded CTE, and require exact set equality with the registry. Source
    /// formatting, including a whole declaration on one line, is irrelevant.
    #[test]
    fn every_bounded_tree_cte_expansion_is_registered() {
        let source: syn::File = syn::parse_str(include_str!("wave_tree.rs")).unwrap();
        let actual = source
            .items
            .into_iter()
            .filter_map(|item| match item {
                syn::Item::Const(item) => match *item.expr {
                    syn::Expr::Macro(expression)
                        if expression.mac.path.is_ident("concat")
                            && expression.mac.tokens.to_string().contains("bounded_wave_") =>
                    {
                        Some(item.ident.to_string())
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let registered = BOUNDED_WAVE_TREE_SQL
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, registered, "bounded-tree SQL registry drifted");
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
    fn enforcement_points_are_compatible_for_every_budget_and_member_count() {
        for budget in 0..=64 {
            for members in 1..=64 {
                if !can_add_tree_member(budget, members) {
                    continue;
                }
                let after = members + 1;
                assert!(
                    (0..after).all(|index| deterministic_share(budget, after, index) > 0),
                    "point one admitted a zero-share tree: budget={budget}, members={after}"
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
