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

/// How a freshly minted track gets its workspace. A managed path is derived
/// from the track id, which only exists inside `track_create_tx`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackWorkspacePlan {
    /// Use `NewTrack.cwd` verbatim, `kind = Attached`, frozen at creation
    /// (`attached → *` is not a legal transition).
    AttachedFromCwd,
    /// Derive `<root>/<area_id>/<track_id>`, `kind = Managed`, **not** frozen: a
    /// default, re-assignable until work happens. `NewTrack.cwd` is ignored.
    ManagedUnder(std::path::PathBuf),
    /// Derive `<root>/<area_id>/<track_id>`, `kind = Managed`, **frozen at
    /// creation**: a child track's first event is a harness bootstrap at this path.
    ManagedFrozenUnder(std::path::PathBuf),
    /// Point at an existing **attached** path, `kind = Attached`, frozen. The one
    /// place inheriting another track's path is correct: recycling touches
    /// `kind = managed` directories only, so an attached path is never created,
    /// moved or deleted by the server however many rows point at it.
    InheritAttachedFrozen(AttachedInheritedPath),
}

/// A path that may be inherited as an `attached` workspace, **proven to be
/// outside the managed workspace root**: recycling is by DIRECTORY, so an
/// attached row under `<workspace-root>` would be removed as collateral.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedInheritedPath(String);

impl AttachedInheritedPath {
    /// `Err` if `path` resolves inside `workspace_root`. Both a lexical and a
    /// canonicalized check: the lexical one misses a symlink into the root, the
    /// canonical one is unavailable for a path that does not exist yet.
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

/// Which user recipe, at which revision, a track is being built from.
/// Server-owned: read from the `track_recipes` row inside the creating
/// transaction, never from a request body. One `Option<Self>` keeps the pair
/// indivisible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRecipeOrigin {
    pub recipe_id: String,
    /// The recipe's `revision` as read in this transaction, frozen on the track from here on.
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
    let lifecycle = crate::model::TrackLifecycle::Draft;
    // The workspace is written by `track_workspace_write_tx` below in this same
    // transaction. `tree_task_budget` is stamped NULL by every create path: it is
    // meaningful only on a tree root, and a DB DEFAULT would hand each child a
    // fresh budget. `recipe_id` / `recipe_revision` are a value copy: the recipe
    // can later be edited or deleted.
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
    // Write-through so a follow-up emit inside the same closure sees the fresh binding.
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
        // Every track-create path stamps NULL: the policy is a user PATCH on a tree
        // root, never inherited by a child row.
        claude_permissions_policy: None,
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
    // The transition is validated by `validate_transition` at the call site, not
    // here; production paths that mutate `lifecycle` must call it first.
    // `terminal_at` rides on the lifecycle column: stamped on entering a terminal
    // state, cleared on reopen, untouched by patches that don't name `lifecycle`.
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
                // Reopen / resume: clear the stamp so a reopened track doesn't render with a
                // stale terminal date.
                w.terminal_at = None;
            }
        }
        w.lifecycle = new_lifecycle;
    }
    // Tree-root-only, enforced in this single shared writer. Written ONLY when the
    // patch names it, never re-serialized from the row read above: the row decode
    // is lenient about unknown keys, so a title patch by an older binary would
    // otherwise strip what a newer one stored.
    if let Some(policy) = p.claude_permissions_policy {
        let parent: Option<(String,)> = sqlx::query_as(
            "SELECT parent_track_id FROM tracks WHERE id = ?1 AND parent_track_id IS NOT NULL",
        )
        .bind(w.id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
        if let Some((parent_track_id,)) = parent {
            return Err(CalmError::Conflict(format!(
                "claude_permissions_policy is tree-root-only; track {} is a child of \
                 {parent_track_id} — set the policy on its root track instead",
                w.id.as_str()
            )));
        }
        let stored = policy.as_ref().map(serde_json::to_string).transpose()?;
        sqlx::query("UPDATE tracks SET claude_permissions_policy = ?1 WHERE id = ?2")
            .bind(stored)
            .bind(w.id.as_str())
            .execute(&mut **tx)
            .await?;
        w.claude_permissions_policy = policy;
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

    // Freeze point: the track leaves Draft, the last moment the workspace is
    // provably free of durable consumers. The condition is `w.lifecycle != Draft`,
    // not the transition, so tracks that left Draft earlier freeze too; the
    // freeze is idempotent.
    if w.lifecycle != TrackLifecycle::Draft {
        super::track_workspace::track_workspace_freeze_tx(tx, w.id.as_str(), w.updated_at).await?;
    }

    // These columns deliberately do NOT live on the `Track` struct; targeted
    // single-column writes are the whole PATCH surface.
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
    // Root-only, enforced in this single shared writer: the budget divides across
    // the tree, so a child carrying its own value would be a second source of truth.
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
    track_require_candidate_verification_settled_tx(tx, id).await?;
    track_require_leaf_tx(tx, id).await?;
    track_delete_leaf_tx(tx, id, track_area_cache).await
}

/// Preserve capacity and cleanup ownership until every reserved verification settles.
pub async fn track_require_candidate_verification_settled_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<()> {
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key AND o.kind='candidate-verify' WHERE a.track_id=?1 AND (o.id IS NULL OR o.phase NOT IN ('succeeded','failed')))")
        .bind(id).fetch_one(&mut **tx).await?;
    if active {
        return Err(CalmError::Conflict(format!(
            "track {id} has unresolved candidate verification; wait for verification or owned cleanup to settle, then retry deletion"
        )));
    }
    Ok(())
}

/// Refuse deletion while a direct child exists; authoritative for every
/// deletion entry point, including callers that bypass the route's preflight.
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
    // `tasks.track_id` has no FK to `tracks`, so plan rows are deleted explicitly.
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
    sqlx::query("DELETE FROM task_attempt_allocations WHERE track_id = ?1")
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
    track_area_cache.remove(&TrackId::from(id));
    Ok(())
}

/// The request identity stored beside what one `(area_id, Idempotency-Key)`
/// pair minted. `LegacyUnknown` is a named state for pre-fingerprint rows (the
/// route fails them closed). `V2MessageLess` is its own variant because message
/// presence is part of request identity and the digest does not cover it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackCreateRequestFingerprint {
    LegacyUnknown,
    V1 {
        create_request_sha256: String,
        first_message_sha256: String,
    },
    V2MessageLess {
        create_request_sha256: String,
    },
}

/// What one `(area_id, Idempotency-Key)` pair already minted. Three ids, not
/// one: `resume_prior_attempt` needs the card ids and may have no operation
/// payload to read them from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackCreateBinding {
    pub track_id: String,
    pub planner_card_id: String,
    pub report_card_id: String,
    pub request_fingerprint: TrackCreateRequestFingerprint,
}

/// A new binding claim; the create-request digest is required so production
/// code cannot construct a legacy-unknown row. `first_message_sha256: None`
/// means "there was no message" and selects fingerprint version 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackCreateBindingClaim {
    pub track_id: String,
    pub planner_card_id: String,
    pub report_card_id: String,
    pub create_request_sha256: String,
    pub first_message_sha256: Option<String>,
}

/// Claim `(area_id, idempotency_key)` **inside the transaction that minted the
/// track**: written after the track commit, the interval between the two
/// commits is exactly where a retry mints a second track. A duplicate key
/// violates the primary key and rolls the whole create back.
pub async fn track_create_idempotency_claim_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    idempotency_key: &str,
    binding: &TrackCreateBindingClaim,
) -> Result<()> {
    // The version is derived from the claim, not passed in, so a caller cannot
    // write a version 1 row with no message digest.
    let version: i64 = if binding.first_message_sha256.is_some() {
        1
    } else {
        2
    };
    sqlx::query(
        "INSERT INTO track_create_idempotency \
         (area_id, idempotency_key, track_id, planner_card_id, report_card_id, created_at_ms, \
          request_fingerprint_version, create_request_sha256, first_message_sha256) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(area_id)
    .bind(idempotency_key)
    .bind(&binding.track_id)
    .bind(&binding.planner_card_id)
    .bind(&binding.report_card_id)
    .bind(now_ms())
    .bind(version)
    .bind(&binding.create_request_sha256)
    .bind(&binding.first_message_sha256)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

type TrackCreateBindingRow = (String, String, String, i64, Option<String>, Option<String>);

/// The authority for "does a track already exist for this key". The
/// `operations` row cannot be: it is written after `adapter.validate` and is
/// absent for the failures that refuse there.
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
                // The absent digest is the shape: its own variant, never "V1 with something missing".
                (2, Some(create_request_sha256), None) => {
                    TrackCreateRequestFingerprint::V2MessageLess {
                        create_request_sha256,
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
