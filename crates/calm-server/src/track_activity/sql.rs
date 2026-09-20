//! The durable rows the track activity projector reads. Every function here is ONE autocommit
//! SELECT over the pool; a deferred `pool.begin()` would hold read locks across statements and
//! deadlock against IMMEDIATE writers, so there must be no explicit transaction in this module.

use sqlx::{Row, SqlitePool};

use crate::error::Result;
use crate::isolated_codex::lookup::isolated_card_exists_sql;

/// E1 — harness turn end: the newest non-interrupted `turn/completed` transcript row per card,
/// inlined from its exported macro so the two spellings cannot drift; a `const` so the plan test runs THIS text.
pub const E1_HARNESS_TURN_COMPLETED_SQL: &str = concat!(
    "SELECT MAX(",
    calm_truth::last_turn_completed_ms_subquery!(),
    ") FROM cards c WHERE c.track_id = ?1"
);

/// E2 — a successful `calm.user.notify`: the completed MCP tool call row (`item/completed` only).
/// A row whose `item.error` is set or whose `item.status` is `failed` is not evidence.
pub const E2_USER_NOTIFY_SQL: &str = "SELECT MAX(h.created_at_ms) FROM harness_items h \
     WHERE h.card_id IN (SELECT id FROM cards WHERE track_id = ?1) \
       AND h.method = 'item/completed' AND h.item_type = 'mcpToolCall' \
       AND json_extract(h.params, '$.item.tool') = 'calm.user.notify' \
       AND json_extract(h.params, '$.item.error') IS NULL \
       AND COALESCE(json_extract(h.params, '$.item.status'), '') <> 'failed'";

/// `tracks` row slice the fold needs.
#[derive(Debug, Clone)]
pub struct TrackRow {
    pub lifecycle: String,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

/// One `current_tasks` row — the W clause input.
#[derive(Debug, Clone)]
pub struct TaskRow {
    pub key: String,
    pub status: String,
    pub worker_card_id: Option<String>,
    pub child_track_id: Option<String>,
    pub finished_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

/// One eligible session — the S0 result row.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub card_id: String,
    pub provider: String,
    pub state: String,
    pub last_thread_status: Option<String>,
    pub last_activity_ms: Option<i64>,
    pub updated_at_ms: i64,
    pub created_at_ms: i64,
    /// `json_extract(handle_state_json, '$.mode')` — `Some("harness")` for the planner / assistant harness rows.
    pub mode: Option<String>,
    /// The card-keyed isolated predicate (`isolated_codex::lookup`).
    pub isolated: bool,
    /// `EXISTS (tasks.worker_card_id = card)` over EVERY attempt — the "never bound to a task" arm negated.
    pub task_bound: bool,
}

/// One `kernel/card/status` overlay row for a card of the track.
#[derive(Debug, Clone)]
pub struct CardStatusRow {
    pub state: String,
    pub updated_at: i64,
}

/// The persisted completion-class evidence, one `MAX` per source (E3 is folded from the W rows by the caller).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Evidence {
    pub e1_harness_turn_completed: Option<i64>,
    pub e2_user_notify: Option<i64>,
    pub e4_agent_lifecycle_edge: Option<i64>,
    pub e5_interactive_stop_hook: Option<i64>,
    pub e6_interactive_turn_completed: Option<i64>,
    pub e7_agent_report_edit: Option<i64>,
}

impl Evidence {
    pub fn max(&self) -> Option<i64> {
        [
            self.e1_harness_turn_completed,
            self.e2_user_notify,
            self.e4_agent_lifecycle_edge,
            self.e5_interactive_stop_hook,
            self.e6_interactive_turn_completed,
            self.e7_agent_report_edit,
        ]
        .into_iter()
        .flatten()
        .max()
    }
}

/// Tick enumeration: every unarchived track — no activity predicate, so a short task between two ticks on a quiet track is still found.
pub(crate) async fn unarchived_track_ids(pool: &SqlitePool) -> Result<Vec<String>> {
    let rows = sqlx::query("SELECT id FROM tracks WHERE archived_at IS NULL ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(|r| r.get::<String, _>("id")).collect())
}

pub(crate) async fn track_row(pool: &SqlitePool, track_id: &str) -> Result<Option<TrackRow>> {
    let row = sqlx::query("SELECT lifecycle, updated_at, archived_at FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| TrackRow {
        lifecycle: r.get("lifecycle"),
        updated_at: r.get("updated_at"),
        archived_at: r.get("archived_at"),
    }))
}

/// The planner's last completed turn: max of `last_turn_completed_ms` over every session of a
/// planner card of the track (superseded ones included, so a restart does not reset it); `NULL` if none.
pub const PLANNER_LAST_TURN_SQL: &str = "SELECT MAX(ws.last_turn_completed_ms) \
     FROM worker_sessions ws JOIN cards c ON c.id = ws.card_id \
    WHERE ws.track_id = ?1 AND c.role = 'planner'";

pub(crate) async fn planner_last_turn(pool: &SqlitePool, track_id: &str) -> Result<Option<i64>> {
    max_ms(pool, PLANNER_LAST_TURN_SQL, track_id).await
}

/// W — the current attempt of every task of the track (superseded attempts are not in `current_tasks`).
pub(crate) async fn current_tasks(pool: &SqlitePool, track_id: &str) -> Result<Vec<TaskRow>> {
    let rows = sqlx::query(
        "SELECT key, status, worker_card_id, child_track_id, finished_at_ms, updated_at_ms \
           FROM current_tasks WHERE track_id = ?1 ORDER BY key",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| TaskRow {
            key: r.get("key"),
            status: r.get("status"),
            worker_card_id: r.get("worker_card_id"),
            child_track_id: r.get("child_track_id"),
            finished_at_ms: r.get("finished_at_ms"),
            updated_at_ms: r.get("updated_at_ms"),
        })
        .collect())
}

/// Does ANY attempt row of the track name this card as its worker? Spliced into S0, E5 and E6 so the four stay one predicate.
fn task_bound_exists_sql(card_expr: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM tasks t WHERE t.track_id = ?1 AND t.worker_card_id = {card_expr})"
    )
}

/// S0 — the eligible sessions: the card's CURRENT session where the card is a harness card, the
/// current attempt's worker card, or an interactive card never bound to a task. A superseded
/// attempt's worker card matches none, so its leftover `failed` session never reaches the fold.
pub(crate) async fn eligible_sessions(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Vec<SessionRow>> {
    let sql = format!(
        "SELECT ws.id, c.id AS card_id, ws.provider, ws.state, ws.last_thread_status, \
                ws.last_activity_ms, ws.updated_at_ms, ws.created_at_ms, \
                json_extract(ws.handle_state_json, '$.mode') AS mode, \
                {isolated} AS isolated, \
                {task_bound} AS task_bound \
           FROM cards c JOIN worker_sessions ws ON ws.id = c.session_id \
          WHERE c.track_id = ?1 \
            AND ( json_extract(ws.handle_state_json, '$.mode') = 'harness' \
               OR EXISTS (SELECT 1 FROM current_tasks ct \
                           WHERE ct.track_id = ?1 AND ct.worker_card_id = c.id) \
               OR NOT {task_bound} ) \
          ORDER BY ws.id",
        isolated = isolated_card_exists_sql("c.id"),
        task_bound = task_bound_exists_sql("c.id"),
    );
    let rows = sqlx::query(&sql).bind(track_id).fetch_all(pool).await?;
    Ok(rows
        .iter()
        .map(|r| SessionRow {
            id: r.get("id"),
            card_id: r.get("card_id"),
            provider: r.get("provider"),
            state: r.get("state"),
            last_thread_status: r.get("last_thread_status"),
            last_activity_ms: r.get("last_activity_ms"),
            updated_at_ms: r.get("updated_at_ms"),
            created_at_ms: r.get("created_at_ms"),
            mode: r.get("mode"),
            isolated: r.get::<bool, _>("isolated"),
            task_bound: r.get::<bool, _>("task_bound"),
        })
        .collect())
}

/// The `kernel/card/status` rows of the track's cards, keyed by card id. Which of them count
/// is decided by the fold against S0 (rows without an eligible LIVE session are ignored, never rewritten).
pub(crate) async fn card_status_overlays(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<std::collections::HashMap<String, CardStatusRow>> {
    let rows = sqlx::query(
        "SELECT entity_id, json_extract(payload, '$.state') AS state, updated_at \
           FROM overlays \
          WHERE plugin_id = 'kernel' AND entity_kind = 'card' AND kind = 'status' \
            AND entity_id IN (SELECT id FROM cards WHERE track_id = ?1)",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let state: Option<String> = r.get("state");
            Some((
                r.get::<String, _>("entity_id"),
                CardStatusRow {
                    state: state?,
                    updated_at: r.get("updated_at"),
                },
            ))
        })
        .collect())
}

/// The existing `kernel/track/activity` payload, if any.
pub(crate) async fn existing_activity_payload(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<serde_json::Value>> {
    let row = sqlx::query(
        "SELECT payload FROM overlays \
          WHERE plugin_id = 'kernel' AND entity_kind = 'track' AND kind = 'activity' \
            AND entity_id = ?1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let text: String = row.get("payload");
    Ok(Some(serde_json::from_str(&text)?))
}

async fn max_ms(pool: &SqlitePool, sql: &str, track_id: &str) -> Result<Option<i64>> {
    let row = sqlx::query(sql).bind(track_id).fetch_one(pool).await?;
    Ok(row.try_get::<Option<i64>, _>(0)?)
}

/// E1, E2, E4–E7 — six autocommit `MAX` statements. E3 is computed from the W rows by the caller.
pub(crate) async fn evidence(pool: &SqlitePool, track_id: &str) -> Result<Evidence> {
    // E4 — a lifecycle edge NOT driven by the user (`track.*` is never pruned;
    // `events.actor` is the `ActorId` JSON).
    let e4 = "SELECT MAX(at) FROM events \
               WHERE scope_track = ?1 AND kind = 'track.lifecycle_changed' \
                 AND json_extract(actor, '$.kind') <> 'User'";
    // E5 — the stop hook of an interactive claude/codex card never bound to a task. A
    // task-bound worker lights once, through E3.
    let e5 = format!(
        "SELECT MAX(e.at) FROM events e \
          WHERE e.scope_track = ?1 AND e.kind IN ('claude.hook', 'codex.hook') \
            AND json_extract(e.payload, '$.kind') IN ('hook.claude.stop', 'hook.codex.stop') \
            AND NOT {}",
        task_bound_exists_sql("json_extract(e.payload, '$.card_id')")
    );
    // E6 — the feeder's monotone turn-completion column on a shared-daemon interactive card
    // never bound to a task (any session state — an exited session keeps it).
    let e6 = format!(
        "SELECT MAX(ws.last_turn_completed_ms) FROM worker_sessions ws \
          WHERE ws.track_id = ?1 AND ws.provider = 'codex' \
            AND COALESCE(json_extract(ws.handle_state_json, '$.mode'), '') <> 'harness' \
            AND NOT {}",
        task_bound_exists_sql("ws.card_id")
    );
    // E7 — a report rewrite by someone other than the user (`EditAuthor`
    // is bare-lowercase on the wire).
    let e7 = "SELECT MAX(at) FROM events \
               WHERE scope_track = ?1 AND kind = 'track.report_edited' \
                 AND json_extract(payload, '$.author') <> 'user'";
    Ok(Evidence {
        e1_harness_turn_completed: max_ms(pool, E1_HARNESS_TURN_COMPLETED_SQL, track_id).await?,
        e2_user_notify: max_ms(pool, E2_USER_NOTIFY_SQL, track_id).await?,
        e4_agent_lifecycle_edge: max_ms(pool, e4, track_id).await?,
        e5_interactive_stop_hook: max_ms(pool, &e5, track_id).await?,
        e6_interactive_turn_completed: max_ms(pool, &e6, track_id).await?,
        e7_agent_report_edit: max_ms(pool, e7, track_id).await?,
    })
}
