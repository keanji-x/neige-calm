use sqlx::Sqlite;
use sqlx::Transaction;

use super::infra::next_sort_scoped_in_tx;
use super::session_row::{
    WorkerSessionDeleteScope, clear_track_root_session_refs_for_worker_session_delete_tx,
};
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::model::*;
use crate::track_area_cache::TrackAreaCache;

use super::track_tree::MAX_TREE_TASK_BUDGET;
use super::track_workspace::track_workspace_write_tx;
use crate::db::rows::TRACK_SELECT_COLUMNS;

/// Issue #1147 S2 — how a freshly minted track gets its workspace.
///
/// The `NewTrack.cwd` field can only ever describe an *attached* workspace: it
/// is a path the caller already knows, i.e. a directory somebody else created.
/// A managed workspace's path is derived from the track id, which does not
/// exist until this function mints it, so the caller hands in the root and
/// the derivation happens here — there is no point at which both the id and
/// the caller are in scope outside this function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackWorkspacePlan {
    /// Use `NewTrack.cwd` verbatim, `kind = Attached`, frozen at creation.
    ///
    /// Frozen because `attached → *` is not a legal transition (design D6), so
    /// an unfrozen attached row has no legal use — and it is exactly the row a
    /// future PATCH branch that forgot to check `kind` would relocate, i.e.
    /// would move a real user repository (D9).
    AttachedFromCwd,
    /// Derive `<root>/<area_id>/<track_id>`, `kind = Managed`, **not** frozen.
    ///
    /// Unfrozen is the point: design §2.3 makes the workspace a *default* —
    /// re-assignable until work actually happens (S3's PATCH). `NewTrack.cwd`
    /// is ignored on this branch.
    ManagedUnder(std::path::PathBuf),
    /// Derive `<root>/<area_id>/<track_id>`, `kind = Managed`, **frozen at
    /// creation**. The child-track path (design D7).
    ///
    /// Same derivation as [`Self::ManagedUnder`], opposite freeze decision, and
    /// the difference is the whole point of S4: a child track is machine-created
    /// inside a running planner, so the first thing that happens to it is a
    /// harness bootstrap at this exact path. Design §"更换与冻结" requires the
    /// freeze *before* any non-re-anchorable cwd consumer exists, and child
    /// creation is named there explicitly.
    ///
    /// This variant REPLACED an `InheritFrozen(TrackWorkspace)` that copied the
    /// parent's kind AND path, whichever they were. That one had to go, not
    /// just stop being called: while it existed, "two track rows, one *managed*
    /// directory" stayed a constructible state, and S5 recycles by
    /// `kind = managed` + path — so any future caller of it would re-arm
    /// "deleting the child deletes the parent's repository" (issue #1147 N11).
    ManagedFrozenUnder(std::path::PathBuf),
    /// Point at an existing **attached** path, `kind = Attached`, frozen at
    /// creation. The child of an attached parent (design D7, S4 amendment).
    ///
    /// Deliberately NOT the same variant as the managed sibling above, and
    /// deliberately not `AttachedFromCwd` reading `NewTrack.cwd`: this is the
    /// one place in the codebase where inheriting another track's path is
    /// correct, so it says so in its own name and carries the path itself. A
    /// caller cannot reach it by accident, and a reader looking for "who can
    /// still share a directory" finds exactly this variant.
    ///
    /// Sharing is safe here for one reason only, and it is a property of S5:
    /// recycling touches `kind = managed` directories exclusively, so an
    /// attached path is never created, moved or deleted by the server no
    /// matter how many rows point at it. Multiple tracks on one attached
    /// repository is also a pre-existing, legal production state — the same
    /// checkout is routinely opened by several tracks.
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
/// under `<workspace-root>` — say `<root>/<area>/<some-managed-track>` — is
/// removed as collateral when that managed track is deleted, and the attached
/// track silently loses its workspace. Nothing in the tree can produce that
/// today (the only caller feeds an attached parent's own path, and an attached
/// path under the root is itself an invariant violation caught by
/// `every_managed_track_lives_under_the_workspace_root`'s sibling), but this
/// enum is `pub` and constructible from any crate, so the guard lives in the
/// type rather than in a comment about who calls it.
///
/// Constructing this is the only way to reach
/// [`TrackWorkspacePlan::InheritAttachedFrozen`], so the check cannot be
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
                 track would lose its workspace when the managed track that owns that directory is \
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

/// #1292 S3 — which user recipe, at which revision, a track is being built
/// from.
///
/// One parameter carrying both halves, rather than two fields on [`NewTrack`],
/// for two reasons.
///
/// It is server-owned. [`NewTrack`] is the caller-supplied shape; `purpose` and
/// `workspace_plan` are already parameters for exactly this reason. Provenance
/// is read out of the `track_recipes` row inside the creating transaction, never
/// taken from a request body — a client that could name its own origin could
/// claim any origin.
///
/// And it makes the pair indivisible for writers that go through
/// [`track_create_tx`]'s parameter: two `Option` fields admit two states the
/// system has no reading for, one `Option<Self>` admits neither.
///
/// That is strictly narrower than what migration 0085's cross-column CHECK
/// does, and the two are not interchangeable. The CHECK binds every writer of
/// the `tracks` row. This type binds only this parameter — [`TrackRow`] and
/// [`Track`] each carry two independent `Option`s and copy them straight
/// through, so a half-pair already in the database would flow out through
/// `GET /api/tracks/{id}` unvalidated. The database is the layer that keeps
/// one from getting there.
///
/// [`TrackRow`]: crate::db::rows::TrackRow
/// [`Track`]: crate::model::Track
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRecipeOrigin {
    pub recipe_id: String,
    /// The recipe's `revision` as read in this transaction. Frozen on the track
    /// from here on: later edits bump the recipe's own revision and leave this
    /// value alone, which is what makes it name a version rather than a row.
    pub revision: i64,
}

pub async fn track_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewTrack,
    purpose: Option<&str>,
    workspace_plan: &TrackWorkspacePlan,
    recipe_origin: Option<&TrackRecipeOrigin>,
    track_area_cache: &TrackAreaCache,
) -> Result<Track> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM areas WHERE id = ?1")
        .bind(p.area_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("area {}", p.area_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "tracks", "WHERE area_id = ?1", Some(p.area_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let id = new_id();
    // Issue #145 — new tracks seed at `lifecycle = 'draft'`. The DB
    // DEFAULT in migration 0012 also pins this, but stamping it
    // explicitly here matches the "required field, no Option" model:
    // every track-create path declares the seed lifecycle in code so a
    // future change to the seed value can't be reached by skipping
    // the column from the INSERT list.
    let lifecycle = crate::model::TrackLifecycle::Draft;
    // Issue #250 PR 2 — the route layer (`POST /api/tracks`) already validated
    // absolute-path shape + area-folder ownership; this writer stays
    // mechanical.
    //
    // Issue #1147 S1 — the workspace is not part of this INSERT. It is written
    // a few lines down by `track_workspace_write_tx` in this same transaction,
    // so kind/path/frozen_at are always decided together.
    //
    // `terminal_at` is `NULL` on every fresh track (Draft is non-terminal
    // by construction; `TrackLifecycle::is_terminal` returns false for it).
    // Issue #985 slice 6 PR-B — `tree_task_budget` is stamped NULL by every
    // track-create path, the same "declare it in code, never reach it by
    // omitting the column" rule as `lifecycle` above. It matters more here:
    // the budget is single-source, meaningful only on a tree root, and the
    // `child-track` operation creates children through this very function. A
    // child that inherited a budget of its own (which a DB DEFAULT would have
    // given it) would hand each sub-track a fresh tree budget and make the
    // whole-tree bound vacuous.
    //
    // #1292 S3 — `recipe_id` / `recipe_revision` are stamped from
    // [`TrackRecipeOrigin`], which is `None` for every creation source other
    // than "instantiate a user recipe". They are written here, in the same
    // statement as the row they describe, because instantiation is a value
    // copy: after this the recipe can be edited or deleted and nothing else
    // remembers where the track came from.
    sqlx::query(
        r#"INSERT INTO tracks
           (id, area_id, title, sort, archived_at, pinned_at, lifecycle, template_id, plugin_scope, purpose, template_input, terminal_at, tree_task_budget, recipe_id, recipe_revision, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, ?8, ?9, NULL, NULL, ?10, ?11, ?12, ?13)"#,
    )
    .bind(&id)
    .bind(p.area_id.as_str())
    .bind(&p.title)
    .bind(sort)
    .bind(lifecycle.as_db_str())
    .bind(p.template_id.as_deref())
    .bind(p.plugin_scope.as_deref())
    .bind(purpose)
    .bind(p.template_input.as_ref().map(|v| v.to_string()))
    .bind(recipe_origin.map(|o| o.recipe_id.as_str()))
    .bind(recipe_origin.map(|o| o.revision))
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // Issue #1147 S2 — the caller declares the workspace shape; the derivation
    // of a managed path needs the track id, which only exists here. See
    // [`TrackWorkspacePlan`] for why each variant freezes (or does not).
    // The launchpad track does not come through this function at all; it is the
    // documented D9 exception — see `routes/today.rs::launchpad_workspace`.
    let workspace = match workspace_plan {
        TrackWorkspacePlan::AttachedFromCwd => TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: p.cwd.clone(),
            frozen_at: Some(now),
        },
        TrackWorkspacePlan::ManagedUnder(root) => TrackWorkspace {
            kind: TrackWorkspaceKind::Managed,
            path: root
                .join(p.area_id.as_str())
                .join(&id)
                .to_string_lossy()
                .into_owned(),
            frozen_at: None,
        },
        TrackWorkspacePlan::ManagedFrozenUnder(root) => TrackWorkspace {
            kind: TrackWorkspaceKind::Managed,
            path: root
                .join(p.area_id.as_str())
                .join(&id)
                .to_string_lossy()
                .into_owned(),
            frozen_at: Some(now),
        },
        TrackWorkspacePlan::InheritAttachedFrozen(path) => TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: path.as_str().to_string(),
            frozen_at: Some(now),
        },
    };
    track_workspace_write_tx(tx, &id, &workspace).await?;
    // #234 — write-through into the track→area cache. Same semantics as
    // the `card_role_cache` write-through in `card_create_with_id_tx`: a
    // follow-up emit inside the same `write_with_event` closure can
    // see the freshly-minted binding via `enforce_role`'s lookup.
    let track_id: TrackId = id.clone().into();
    track_area_cache.insert(track_id.clone(), p.area_id.clone());
    Ok(Track {
        id: track_id,
        area_id: p.area_id,
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
        recipe_id: recipe_origin.map(|o| o.recipe_id.clone()),
        recipe_revision: recipe_origin.map(|o| o.revision),
        workspace,
        created_at: now,
        updated_at: now,
    })
}

pub async fn track_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: TrackPatch,
) -> Result<Track> {
    let mut w = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
        "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Track::from)
    .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;

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
    // Issue #145 — `TrackPatch.lifecycle` is applied here, but the
    // transition is validated by `validate_transition` at the call
    // site (REST handler / MCP tool), *outside* the DB layer. Routing
    // the validator through the route boundary (rather than this
    // function) keeps `track_update_tx` a pure mechanical row write
    // and avoids threading `ActorId` through every call site that
    // patches the row. Production code paths that mutate
    // `lifecycle` must call `validate_transition` first.
    //
    // Issue #250 PR 2 — `terminal_at` rides on the lifecycle column:
    // when this patch advances the track into a terminal state we
    // stamp the current time; when it reopens or resumes a terminal track
    // (terminal → planning / working) we clear `terminal_at` back to NULL. A
    // patch that doesn't touch
    // `lifecycle` leaves `terminal_at` alone — that matches the
    // archive precedent (changing `title` doesn't bump `archived_at`).
    // The stamp happens inside the same transaction as the track row
    // update and the caller's `TrackLifecycleChanged` event, so a
    // mid-tx crash leaves none of them behind.
    if let Some(new_lifecycle) = p.lifecycle {
        if w.lifecycle.is_terminal() && !new_lifecycle.is_terminal() {
            let parent: Option<(String, String)> =
                sqlx::query_as("SELECT track_id, key FROM tasks WHERE child_track_id = ?1 LIMIT 1")
                    .bind(id)
                    .fetch_optional(&mut **tx)
                    .await?;
            if let Some((parent_track_id, parent_key)) = parent {
                return Err(CalmError::Conflict(format!(
                    "track {id} is child of task {parent_track_id}:{parent_key} and cannot be reopened"
                )));
            }
        }
        if new_lifecycle != w.lifecycle {
            if new_lifecycle.is_terminal() {
                w.terminal_at = Some(now_ms());
            } else if w.lifecycle.is_terminal() {
                // Reopen / resume (terminal → non-terminal). The legal edges
                // here are user-driven terminal → planning / working, gated by
                // `validate_transition`. Clearing
                // the stamp ensures a reopened track doesn't render
                // with a stale terminal date on the calendar.
                w.terminal_at = None;
            }
        }
        w.lifecycle = new_lifecycle;
    }
    w.updated_at = now_ms();

    sqlx::query(
        r#"UPDATE tracks
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

    // #1147 S3 — freeze point 3 of 4 (design §更换与冻结): "the track leaves
    // Draft". Draft is the state in which nothing has been dispatched, so it
    // is the last moment at which the workspace is provably free of durable
    // consumers. The instant the track starts planning/executing, the
    // scheduler, the forge and every worker take the path as a given.
    //
    // The condition is `w.lifecycle != Draft`, not `p.lifecycle == Some(x)`:
    // an already-non-Draft track being patched for any other reason is *also*
    // past the point of no return, and a predicate that only fires on the
    // transition would leave every track whose transition happened before this
    // slice unfrozen forever. The freeze is idempotent and monotonic, so
    // re-asserting it on every non-Draft patch costs one no-op UPDATE.
    //
    // This is the low-level entry: `routes/tracks.rs::update_track`, the MCP
    // tool and `track_lifecycle.rs` all funnel through here.
    if w.lifecycle != TrackLifecycle::Draft {
        super::track_workspace::track_workspace_freeze_tx(tx, w.id.as_str(), w.updated_at).await?;
    }

    // Issue #644 — scheduler budget + gate policy (migration 0041).
    // These columns deliberately do NOT live on the `Track` struct while
    // the plan is inert (PR-A): keeping them off the struct leaves every
    // `SELECT` column list, the `TrackUpdated` wire payload, and the
    // ts-rs export untouched. Targeted single-column writes here are the
    // whole PATCH surface; the PR-B scheduler reads the columns by SQL.
    if let Some(budget) = p.task_budget {
        sqlx::query("UPDATE tracks SET task_budget = ?1 WHERE id = ?2")
            .bind(budget)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(require_gates) = p.require_task_gates {
        sqlx::query("UPDATE tracks SET require_task_gates = ?1 WHERE id = ?2")
            .bind(require_gates)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(ceiling) = p.planner_task_ceiling {
        sqlx::query("UPDATE tracks SET planner_task_ceiling = ?1 WHERE id = ?2")
            .bind(ceiling)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    if let Some(policy) = p.automation_policy {
        sqlx::query("UPDATE tracks SET automation_policy = ?1 WHERE id = ?2")
            .bind(policy)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    // Issue #985 slice 6 PR-B — root-only, enforced HERE rather than at the
    // route: this in-tx helper is the single writer every entry point shares,
    // and a route-only guard is exactly the shape §7 #17/#20 caught twice. The
    // budget divides across the tree's tracks, so a child carrying its own value
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
            "SELECT parent_track_id FROM tracks WHERE id = ?1 AND parent_track_id IS NOT NULL",
        )
        .bind(w.id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
        if let Some((parent_track_id,)) = parent {
            return Err(CalmError::Conflict(format!(
                "tree_task_budget is tree-root-only; track {} is a child of {parent_track_id} — \
                 set the budget on its root track instead",
                w.id.as_str()
            )));
        }
        sqlx::query("UPDATE tracks SET tree_task_budget = ?1 WHERE id = ?2")
            .bind(budget)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
    }
    Ok(w)
}

pub async fn track_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_area_cache: &TrackAreaCache,
) -> Result<()> {
    track_require_leaf_tx(tx, id).await?;
    track_delete_leaf_tx(tx, id, track_area_cache).await
}

/// Refuse deletion while a direct child exists. This is the authoritative
/// guard for every deletion entry point, including direct repository calls
/// that bypass the HTTP route's best-effort preflight.
pub async fn track_require_leaf_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    if let Some((child_id,)) =
        sqlx::query_as::<_, (String,)>("SELECT id FROM tracks WHERE parent_track_id = ?1 LIMIT 1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
    {
        return Err(CalmError::Conflict(format!(
            "track {id} has child track {child_id}; cancel it if needed, then delete that child track first"
        )));
    }
    Ok(())
}

async fn track_delete_leaf_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    track_area_cache: &TrackAreaCache,
) -> Result<()> {
    sqlx::query("DELETE FROM track_vcs_refs WHERE track_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM track_vcs_commits WHERE track_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    // #644 — `tasks.track_id` has no FK to `tracks` (events-outlive-rows
    // convention, design §2), so plan rows must be deleted explicitly
    // alongside the other no-FK track-owned tables above.
    sqlx::query(
        "DELETE FROM task_ref_index WHERE task_id IN (SELECT id FROM tasks WHERE track_id = ?1)",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM tasks WHERE track_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    clear_track_root_session_refs_for_worker_session_delete_tx(
        tx,
        WorkerSessionDeleteScope::Track { track_id: id },
    )
    .await?;
    // `worker_sessions.track_id` is a required FK. Card/runtime rows may
    // cascade below, but sessions must leave before the track row itself.
    sqlx::query("DELETE FROM worker_sessions WHERE track_id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let res = sqlx::query("DELETE FROM tracks WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("track {id}")));
    }
    // #234 — keep the track→area cache in lockstep with the table. Mirror
    // of the card-delete-side write-through in `card_delete_tx`.
    track_area_cache.remove(&TrackId::from(id));
    Ok(())
}

/// #1434 — the request identity stored beside what one
/// `(area_id, Idempotency-Key)` pair minted.
///
/// Version 0 is not represented by missing optional fields. It is a named
/// migration state for rows written before request fingerprints existed; the
/// route fails those rows closed because their original request cannot be
/// reconstructed reliably. Every new claim is [`Self::V1`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackCreateRequestFingerprint {
    LegacyUnknown,
    V1 {
        create_request_sha256: String,
        first_message_sha256: String,
    },
}

/// #1384 / #1434 — what one `(area_id, Idempotency-Key)` pair already minted,
/// together with the request that was allowed to mint it.
///
/// Three ids, not one. `resume_prior_attempt` needs the planner and report
/// card ids to resubmit the harness start, and in the variant-4 shape (the
/// daemon refused before `insert_operation` ran) there is no operation payload
/// to read them from. A role query would be well-defined —
/// `idx_cards_one_planner_per_track` and `idx_cards_one_report_per_track` make
/// both single-valued — but re-deriving a value the mint already knew is a
/// second source of truth for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackCreateBinding {
    pub track_id: String,
    pub planner_card_id: String,
    pub report_card_id: String,
    pub request_fingerprint: TrackCreateRequestFingerprint,
}

/// A new binding claim. Unlike the versioned read model, both request digests
/// are required fields: production code cannot construct a legacy-unknown row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackCreateBindingClaim {
    pub track_id: String,
    pub planner_card_id: String,
    pub report_card_id: String,
    pub create_request_sha256: String,
    pub first_message_sha256: String,
}

/// Claim `(area_id, idempotency_key)` for a track, **inside the transaction
/// that minted it**.
///
/// Takes `&mut Transaction` rather than `&self` for the one reason this whole
/// mechanism exists: written on a pooled connection it would commit at some
/// point after the track row, and the interval between the two commits is
/// exactly the window in which a retry sees a track it cannot find the binding
/// for and mints a second one. Composed into `create_track_structure`'s closure
/// there is no such interval — the id and the fact of who owns it are one
/// commit.
///
/// A duplicate `(area_id, idempotency_key)` violates the primary key and
/// surfaces as an error that rolls the whole create back. That is the intended
/// answer: the route maps it fail-closed rather than letting a second track
/// commit behind a 409.
pub async fn track_create_idempotency_claim_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    idempotency_key: &str,
    binding: &TrackCreateBindingClaim,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO track_create_idempotency \
         (area_id, idempotency_key, track_id, planner_card_id, report_card_id, created_at_ms, \
          request_fingerprint_version, create_request_sha256, first_message_sha256) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
    )
    .bind(area_id)
    .bind(idempotency_key)
    .bind(&binding.track_id)
    .bind(&binding.planner_card_id)
    .bind(&binding.report_card_id)
    .bind(now_ms())
    .bind(&binding.create_request_sha256)
    .bind(&binding.first_message_sha256)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

type TrackCreateBindingRow = (String, String, String, i64, Option<String>, Option<String>);

/// The read side: one primary-key hit, on a pooled connection.
///
/// This is the new authority for "does a track already exist for this key".
/// The `operations` row is not, and cannot be: it is written after
/// `adapter.validate` and so is absent for the whole class of failures that
/// refuse there.
pub async fn track_create_idempotency_get_pool(
    pool: &sqlx::SqlitePool,
    area_id: &str,
    idempotency_key: &str,
) -> Result<Option<TrackCreateBinding>> {
    let row: Option<TrackCreateBindingRow> = sqlx::query_as(
        "SELECT track_id, planner_card_id, report_card_id, request_fingerprint_version, \
                create_request_sha256, first_message_sha256 \
         FROM track_create_idempotency \
         WHERE area_id = ?1 AND idempotency_key = ?2",
    )
    .bind(area_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await?;
    row.map(
        |(
            track_id,
            planner_card_id,
            report_card_id,
            version,
            create_request_sha256,
            first_message_sha256,
        )| {
            let request_fingerprint = match (version, create_request_sha256, first_message_sha256) {
                (0, None, None) => TrackCreateRequestFingerprint::LegacyUnknown,
                (1, Some(create_request_sha256), Some(first_message_sha256)) => {
                    TrackCreateRequestFingerprint::V1 {
                        create_request_sha256,
                        first_message_sha256,
                    }
                }
                (version, create_hash, message_hash) => {
                    return Err(CalmError::Internal(format!(
                        "track-create idempotency binding has invalid fingerprint state: \
                         version={version}, create_hash_present={}, message_hash_present={}",
                        create_hash.is_some(),
                        message_hash.is_some()
                    )));
                }
            };
            Ok(TrackCreateBinding {
                track_id,
                planner_card_id,
                report_card_id,
                request_fingerprint,
            })
        },
    )
    .transpose()
}
