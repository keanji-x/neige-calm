use sqlx::Sqlite;
use sqlx::Transaction;

use super::infra::next_sort_scoped_in_tx;
use super::session_row::{
    WorkerSessionDeleteScope, clear_wave_root_session_refs_for_worker_session_delete_tx,
};
use crate::error::{CalmError, Result};
use crate::ids::WaveId;
use crate::model::*;
use crate::wave_cove_cache::WaveCoveCache;

use super::wave_tree::MAX_TREE_TASK_BUDGET;
use super::wave_workspace::wave_workspace_write_tx;
use crate::db::rows::WAVE_SELECT_COLUMNS;

/// Issue #1147 S2 — how a freshly minted wave gets its workspace.
///
/// The `NewWave.cwd` field can only ever describe an *attached* workspace: it
/// is a path the caller already knows, i.e. a directory somebody else created.
/// A managed workspace's path is derived from the wave id, which does not
/// exist until this function mints it, so the caller hands in the root and
/// the derivation happens here — there is no point at which both the id and
/// the caller are in scope outside this function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaveWorkspacePlan {
    /// Use `NewWave.cwd` verbatim, `kind = Attached`, frozen at creation.
    ///
    /// Frozen because `attached → *` is not a legal transition (design D6), so
    /// an unfrozen attached row has no legal use — and it is exactly the row a
    /// future PATCH branch that forgot to check `kind` would relocate, i.e.
    /// would move a real user repository (D9).
    AttachedFromCwd,
    /// Derive `<root>/<cove_id>/<wave_id>`, `kind = Managed`, **not** frozen.
    ///
    /// Unfrozen is the point: design §2.3 makes the workspace a *default* —
    /// re-assignable until work actually happens (S3's PATCH). `NewWave.cwd`
    /// is ignored on this branch.
    ManagedUnder(std::path::PathBuf),
    /// Derive `<root>/<cove_id>/<wave_id>`, `kind = Managed`, **frozen at
    /// creation**. The child-wave path (design D7).
    ///
    /// Same derivation as [`Self::ManagedUnder`], opposite freeze decision, and
    /// the difference is the whole point of S4: a child wave is machine-created
    /// inside a running spec, so the first thing that happens to it is a
    /// harness bootstrap at this exact path. Design §"更换与冻结" requires the
    /// freeze *before* any non-re-anchorable cwd consumer exists, and child
    /// creation is named there explicitly.
    ///
    /// This variant REPLACED an `InheritFrozen(WaveWorkspace)` that copied the
    /// parent's kind AND path, whichever they were. That one had to go, not
    /// just stop being called: while it existed, "two wave rows, one *managed*
    /// directory" stayed a constructible state, and S5 recycles by
    /// `kind = managed` + path — so any future caller of it would re-arm
    /// "deleting the child deletes the parent's repository" (issue #1147 N11).
    ManagedFrozenUnder(std::path::PathBuf),
    /// Point at an existing **attached** path, `kind = Attached`, frozen at
    /// creation. The child of an attached parent (design D7, S4 amendment).
    ///
    /// Deliberately NOT the same variant as the managed sibling above, and
    /// deliberately not `AttachedFromCwd` reading `NewWave.cwd`: this is the
    /// one place in the codebase where inheriting another wave's path is
    /// correct, so it says so in its own name and carries the path itself. A
    /// caller cannot reach it by accident, and a reader looking for "who can
    /// still share a directory" finds exactly this variant.
    ///
    /// Sharing is safe here for one reason only, and it is a property of S5:
    /// recycling touches `kind = managed` directories exclusively, so an
    /// attached path is never created, moved or deleted by the server no
    /// matter how many rows point at it. Multiple waves on one attached
    /// repository is also a pre-existing, legal production state — the same
    /// checkout is routinely opened by several waves.
    ///
    /// The payload is [`AttachedInheritedPath`], not a bare `String`, because
    /// that reasoning has one hole and the constructor closes it — see there.
    InheritAttachedFrozen(AttachedInheritedPath),
}

/// A path that may be inherited as an `attached` workspace: **proven to be
/// outside the managed workspace root**.
///
/// The check exists because "attached rows are never recycled" is a statement
/// about the ROW, and S5 recycles by DIRECTORY. An attached row whose path sits
/// under `<workspace-root>` — say `<root>/<cove>/<some-managed-wave>` — is
/// removed as collateral when that managed wave is deleted, and the attached
/// wave silently loses its workspace. Nothing in the tree can produce that
/// today (the only caller feeds an attached parent's own path, and an attached
/// path under the root is itself an invariant violation caught by
/// `every_managed_wave_lives_under_the_workspace_root`'s sibling), but this
/// enum is `pub` and constructible from any crate, so the guard lives in the
/// type rather than in a comment about who calls it.
///
/// Constructing this is the only way to reach
/// [`WaveWorkspacePlan::InheritAttachedFrozen`], so the check cannot be
/// skipped by a future caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedInheritedPath(String);

impl AttachedInheritedPath {
    /// `Err` if `path` resolves inside `workspace_root`.
    ///
    /// Both a lexical and a canonicalized comparison, because they fail in
    /// opposite directions: the lexical one misses a symlink pointing into the
    /// root, and the canonical one is unavailable when the path does not exist
    /// yet. Either verdict of "inside" refuses.
    pub fn new(path: String, workspace_root: &std::path::Path) -> Result<Self> {
        let candidate = std::path::Path::new(&path);
        let lexically_inside = candidate.starts_with(workspace_root);
        let physically_inside = match (
            std::fs::canonicalize(candidate),
            std::fs::canonicalize(workspace_root),
        ) {
            (Ok(real_path), Ok(real_root)) => real_path.starts_with(&real_root),
            _ => false,
        };
        if lexically_inside || physically_inside {
            return Err(CalmError::Internal(format!(
                "refusing to inherit {path} as an attached workspace: it is inside the managed \
                 workspace root {}. Recycling works on directories, not rows, so this attached \
                 wave would lose its workspace when the managed wave that owns that directory is \
                 deleted.",
                workspace_root.display()
            )));
        }
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub async fn wave_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewWave,
    purpose: Option<&str>,
    workspace_plan: &WaveWorkspacePlan,
    wave_cove_cache: &WaveCoveCache,
) -> Result<Wave> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
        .bind(p.cove_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("cove {}", p.cove_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "waves", "WHERE cove_id = ?1", Some(p.cove_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let id = new_id();
    // Issue #145 — new waves seed at `lifecycle = 'draft'`. The DB
    // DEFAULT in migration 0012 also pins this, but stamping it
    // explicitly here matches the "required field, no Option" model:
    // every wave-create path declares the seed lifecycle in code so a
    // future change to the seed value can't be reached by skipping
    // the column from the INSERT list.
    let lifecycle = crate::model::WaveLifecycle::Draft;
    // Issue #250 PR 2 — the route layer (`POST /api/waves`) already validated
    // absolute-path shape + cove-folder ownership; this writer stays
    // mechanical.
    //
    // Issue #1147 S1 — the workspace is not part of this INSERT. It is written
    // a few lines down by `wave_workspace_write_tx` in this same transaction,
    // so kind/path/frozen_at are always decided together.
    //
    // `terminal_at` is `NULL` on every fresh wave (Draft is non-terminal
    // by construction; `WaveLifecycle::is_terminal` returns false for it).
    // Issue #985 slice 6 PR-B — `tree_task_budget` is stamped NULL by every
    // wave-create path, the same "declare it in code, never reach it by
    // omitting the column" rule as `lifecycle` above. It matters more here:
    // the budget is single-source, meaningful only on a tree root, and the
    // `child-wave` operation creates children through this very function. A
    // child that inherited a budget of its own (which a DB DEFAULT would have
    // given it) would hand each sub-wave a fresh tree budget and make the
    // whole-tree bound vacuous.
    sqlx::query(
        r#"INSERT INTO waves
           (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, template_id, plugin_scope, purpose, template_input, terminal_at, tree_task_budget, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, ?8, ?9, NULL, NULL, ?10, ?11)"#,
    )
    .bind(&id)
    .bind(p.cove_id.as_str())
    .bind(&p.title)
    .bind(sort)
    .bind(lifecycle.as_db_str())
    .bind(p.template_id.as_deref())
    .bind(p.plugin_scope.as_deref())
    .bind(purpose)
    .bind(p.template_input.as_ref().map(|v| v.to_string()))
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // Issue #1147 S2 — the caller declares the workspace shape; the derivation
    // of a managed path needs the wave id, which only exists here. See
    // [`WaveWorkspacePlan`] for why each variant freezes (or does not).
    // The launchpad wave does not come through this function at all; it is the
    // documented D9 exception — see `routes/today.rs::launchpad_workspace`.
    let workspace = match workspace_plan {
        WaveWorkspacePlan::AttachedFromCwd => WaveWorkspace {
            kind: WaveWorkspaceKind::Attached,
            path: p.cwd.clone(),
            frozen_at: Some(now),
        },
        WaveWorkspacePlan::ManagedUnder(root) => WaveWorkspace {
            kind: WaveWorkspaceKind::Managed,
            path: root
                .join(p.cove_id.as_str())
                .join(&id)
                .to_string_lossy()
                .into_owned(),
            frozen_at: None,
        },
        WaveWorkspacePlan::ManagedFrozenUnder(root) => WaveWorkspace {
            kind: WaveWorkspaceKind::Managed,
            path: root
                .join(p.cove_id.as_str())
                .join(&id)
                .to_string_lossy()
                .into_owned(),
            frozen_at: Some(now),
        },
        WaveWorkspacePlan::InheritAttachedFrozen(path) => WaveWorkspace {
            kind: WaveWorkspaceKind::Attached,
            path: path.as_str().to_string(),
            frozen_at: Some(now),
        },
    };
    wave_workspace_write_tx(tx, &id, &workspace).await?;
    // #234 — write-through into the wave→cove cache. Same semantics as
    // the `card_role_cache` write-through in `card_create_with_id_tx`: a
    // follow-up emit inside the same `write_with_event` closure can
    // see the freshly-minted binding via `enforce_role`'s lookup.
    let wave_id: WaveId = id.clone().into();
    wave_cove_cache.insert(wave_id.clone(), p.cove_id.clone());
    Ok(Wave {
        id: wave_id,
        cove_id: p.cove_id,
        title: p.title,
        sort,
        archived_at: None,
        pinned_at: None,
        lifecycle,
        cwd_wire_alias: workspace.path.clone(),
        template_id: p.template_id,
        plugin_scope: p.plugin_scope,
        purpose: purpose.map(str::to_owned),
        template_input: p.template_input,
        terminal_at: None,
        workspace,
        created_at: now,
        updated_at: now,
    })
}

pub async fn wave_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: WavePatch,
) -> Result<Wave> {
    let mut w = sqlx::query_as::<_, crate::db::rows::WaveRow>(&format!(
        "SELECT {WAVE_SELECT_COLUMNS} FROM waves WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Wave::from)
    .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;

    if let Some(v) = p.title {
        w.title = v;
    }
    if let Some(v) = p.sort {
        w.sort = v;
    }
    if let Some(v) = p.archived_at {
        w.archived_at = v;
    }
    if let Some(v) = p.pinned_at {
        w.pinned_at = v;
    }
    // Issue #145 — `WavePatch.lifecycle` is applied here, but the
    // transition is validated by `validate_transition` at the call
    // site (REST handler / MCP tool), *outside* the DB layer. Routing
    // the validator through the route boundary (rather than this
    // function) keeps `wave_update_tx` a pure mechanical row write
    // and avoids threading `ActorId` through every call site that
    // patches the row. Production code paths that mutate
    // `lifecycle` must call `validate_transition` first.
    //
    // Issue #250 PR 2 — `terminal_at` rides on the lifecycle column:
    // when this patch advances the wave into a terminal state we
    // stamp the current time; when it reopens a terminal wave
    // (terminal → planning, the only legal reopen edge today) we
    // clear `terminal_at` back to NULL. A patch that doesn't touch
    // `lifecycle` leaves `terminal_at` alone — that matches the
    // archive precedent (changing `title` doesn't bump `archived_at`).
    // The stamp happens inside the same transaction as the wave row
    // update and the caller's `WaveLifecycleChanged` event, so a
    // mid-tx crash leaves none of them behind.
    if let Some(new_lifecycle) = p.lifecycle {
        if w.lifecycle.is_terminal() && !new_lifecycle.is_terminal() {
            let parent: Option<(String, String)> =
                sqlx::query_as("SELECT wave_id, key FROM tasks WHERE child_wave_id = ?1 LIMIT 1")
                    .bind(id)
                    .fetch_optional(&mut **tx)
                    .await?;
            if let Some((parent_wave_id, parent_key)) = parent {
                return Err(CalmError::Conflict(format!(
                    "wave {id} is child of task {parent_wave_id}:{parent_key} and cannot be reopened"
                )));
            }
        }
        if new_lifecycle != w.lifecycle {
            if new_lifecycle.is_terminal() {
                w.terminal_at = Some(now_ms());
            } else if w.lifecycle.is_terminal() {
                // Reopen (terminal → non-terminal). Today the only
                // legal edge here is `terminal → planning` (user-
                // driven, gated by `validate_transition`). Clearing
                // the stamp ensures a reopened wave doesn't render
                // with a stale terminal date on the calendar.
                w.terminal_at = None;
            }
        }
        w.lifecycle = new_lifecycle;
    }
    w.updated_at = now_ms();

    sqlx::query(
        r#"UPDATE waves
           SET title = ?1, sort = ?2, archived_at = ?3, pinned_at = ?4,
               lifecycle = ?5, terminal_at = ?6, updated_at = ?7
           WHERE id = ?8"#,
    )
    .bind(&w.title)
    .bind(w.sort)
    .bind(w.archived_at)
    .bind(w.pinned_at)
    .bind(w.lifecycle.as_db_str())
    .bind(w.terminal_at)
    .bind(w.updated_at)
    .bind(w.id.as_str())
    .execute(&mut **tx)
    .await?;

    // #1147 S3 — freeze point 3 of 4 (design §更换与冻结): "the wave leaves
    // Draft". Draft is the state in which nothing has been dispatched, so it
    // is the last moment at which the workspace is provably free of durable
    // consumers. The instant the wave starts planning/executing, the
    // scheduler, the forge and every worker take the path as a given.
    //
    // The condition is `w.lifecycle != Draft`, not `p.lifecycle == Some(x)`:
    // an already-non-Draft wave being patched for any other reason is *also*
    // past the point of no return, and a predicate that only fires on the
    // transition would leave every wave whose transition happened before this
    // slice unfrozen forever. The freeze is idempotent and monotonic, so
    // re-asserting it on every non-Draft patch costs one no-op UPDATE.
    //
    // This is the low-level entry: `routes/waves.rs::update_wave`, the MCP
    // tool and `wave_lifecycle.rs` all funnel through here.
    if w.lifecycle != WaveLifecycle::Draft {
        super::wave_workspace::wave_workspace_freeze_tx(tx, w.id.as_str(), w.updated_at).await?;
    }

    // Issue #644 — scheduler budget + gate policy (migration 0041).
    // These columns deliberately do NOT live on the `Wave` struct while
    // the plan is inert (PR-A): keeping them off the struct leaves every
    // `SELECT` column list, the `WaveUpdated` wire payload, and the
    // ts-rs export untouched. Targeted single-column writes here are the
    // whole PATCH surface; the PR-B scheduler reads the columns by SQL.
    if let Some(budget) = p.task_budget {
        sqlx::query("UPDATE waves SET task_budget = ?1 WHERE id = ?2")
            .bind(budget)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(require_gates) = p.require_task_gates {
        sqlx::query("UPDATE waves SET require_task_gates = ?1 WHERE id = ?2")
            .bind(require_gates)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(ceiling) = p.spec_task_ceiling {
        sqlx::query("UPDATE waves SET spec_task_ceiling = ?1 WHERE id = ?2")
            .bind(ceiling)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(policy) = p.automation_policy {
        sqlx::query("UPDATE waves SET automation_policy = ?1 WHERE id = ?2")
            .bind(policy)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    // Issue #985 slice 6 PR-B — root-only, enforced HERE rather than at the
    // route: this in-tx helper is the single writer every entry point shares,
    // and a route-only guard is exactly the shape §7 #17/#20 caught twice. The
    // budget divides across the tree's waves, so a child carrying its own value
    // would be a second, unreachable source of truth.
    if let Some(budget) = p.tree_task_budget {
        if let Some(budget) = budget
            && !(0..=MAX_TREE_TASK_BUDGET).contains(&budget)
        {
            return Err(CalmError::BadRequest(format!(
                "tree_task_budget must be between 0 and {MAX_TREE_TASK_BUDGET} (got {budget})"
            )));
        }
        let parent: Option<(String,)> = sqlx::query_as(
            "SELECT parent_wave_id FROM waves WHERE id = ?1 AND parent_wave_id IS NOT NULL",
        )
        .bind(w.id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
        if let Some((parent_wave_id,)) = parent {
            return Err(CalmError::Conflict(format!(
                "tree_task_budget is tree-root-only; wave {} is a child of {parent_wave_id} — \
                 set the budget on its root wave instead",
                w.id.as_str()
            )));
        }
        sqlx::query("UPDATE waves SET tree_task_budget = ?1 WHERE id = ?2")
            .bind(budget)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    Ok(w)
}

pub async fn wave_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_cove_cache: &WaveCoveCache,
) -> Result<()> {
    wave_require_leaf_tx(tx, id).await?;
    wave_delete_leaf_tx(tx, id, wave_cove_cache).await
}

/// Refuse deletion while a direct child exists. This is the authoritative
/// guard for every deletion entry point, including direct repository calls
/// that bypass the HTTP route's best-effort preflight.
pub async fn wave_require_leaf_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    if let Some((child_id,)) =
        sqlx::query_as::<_, (String,)>("SELECT id FROM waves WHERE parent_wave_id = ?1 LIMIT 1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
    {
        return Err(CalmError::Conflict(format!(
            "wave {id} has child wave {child_id}; cancel it if needed, then delete that child wave first"
        )));
    }
    Ok(())
}

async fn wave_delete_leaf_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_cove_cache: &WaveCoveCache,
) -> Result<()> {
    sqlx::query("DELETE FROM wave_vcs_refs WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM wave_vcs_commits WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    // #644 — `tasks.wave_id` has no FK to `waves` (events-outlive-rows
    // convention, design §2), so plan rows must be deleted explicitly
    // alongside the other no-FK wave-owned tables above.
    sqlx::query(
        "DELETE FROM task_ref_index WHERE task_id IN (SELECT id FROM tasks WHERE wave_id = ?1)",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM tasks WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    clear_wave_root_session_refs_for_worker_session_delete_tx(
        tx,
        WorkerSessionDeleteScope::Wave { wave_id: id },
    )
    .await?;
    // `worker_sessions.wave_id` is a required FK. Card/runtime rows may
    // cascade below, but sessions must leave before the wave row itself.
    sqlx::query("DELETE FROM worker_sessions WHERE wave_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let res = sqlx::query("DELETE FROM waves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("wave {id}")));
    }
    // #234 — keep the wave→cove cache in lockstep with the table. Mirror
    // of the card-delete-side write-through in `card_delete_tx`.
    wave_cove_cache.remove(&WaveId::from(id));
    Ok(())
}
