//! The durable rows the track activity projector reads. Every function here is ONE autocommit
//! SELECT over the pool; a deferred `pool.begin()` would hold read locks across statements and
//! deadlock against IMMEDIATE writers, so there must be no explicit transaction in this module.

use sqlx::{Row, SqlitePool};

use super::notifications::{LastTurn, NotificationRows, NotifyRow, PendingRatify};
use crate::error::Result;
use crate::isolated_codex::lookup::isolated_card_exists_sql;

/// E1 — harness turn end: the newest non-interrupted `turn/completed` transcript row per card,
/// inlined from its exported macro so the two spellings cannot drift; a `const` so the plan test runs THIS text.
pub const E1_HARNESS_TURN_COMPLETED_SQL: &str = concat!(
    "SELECT MAX(",
    calm_truth::last_turn_completed_ms_subquery!(),
    ") FROM cards c WHERE c.track_id = ?1"
);

/// N3 — the Planner card's transcript, one statement with two arms (`?1` = the Planner card):
/// every successful `calm.user.notify` call (the completed MCP tool call row; a row whose
/// `item.error` is set or whose `item.status` is `failed` is not one), and the newest `turn/completed`
/// row that is not `interrupted`. `NOT MATERIALIZED` keeps both arms on
/// `idx_transcript_card_method_created_at`; a `const` so the plan test runs THIS text.
/// E2 is the `MAX` of the notify arm, uncut by any close rule.
pub const N3_PLANNER_TRANSCRIPT_SQL: &str = "WITH h AS NOT MATERIALIZED ( \
       SELECT id, method, item_type, params, created_at_ms FROM harness_items WHERE card_id = ?1) \
     SELECT 'notify' AS arm, id, created_at_ms, \
            json_extract(params, '$.item.arguments.text') AS text, NULL AS status FROM h \
      WHERE method = 'item/completed' AND item_type = 'mcpToolCall' \
        AND json_extract(params, '$.item.tool') = 'calm.user.notify' \
        AND json_extract(params, '$.item.error') IS NULL \
        AND COALESCE(json_extract(params, '$.item.status'), '') <> 'failed' \
     UNION ALL \
     SELECT * FROM ( \
       SELECT 'turn' AS arm, id, created_at_ms, \
              json_extract(params, '$.error.message') AS text, \
              json_extract(params, '$.status') AS status FROM h \
        WHERE method = 'turn/completed' \
          AND COALESCE(json_extract(params, '$.status'), '') <> 'interrupted' \
        ORDER BY created_at_ms DESC, id DESC LIMIT 1)";

/// `tracks` row slice the fold needs.
#[derive(Debug, Clone)]
pub struct TrackRow {
    pub closed_at: Option<i64>,
    pub updated_at: i64,
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
    pub updated_at_ms: i64,
    pub created_at_ms: i64,
    /// `json_extract(handle_state_json, '$.mode')` — `Some("harness")` for the planner / assistant harness rows.
    pub mode: Option<String>,
    /// The card-keyed isolated predicate (`isolated_codex::lookup`).
    pub isolated: bool,
    /// `EXISTS (tasks.worker_card_id = card)` over EVERY attempt — the "never bound to a task" arm negated.
    pub task_bound: bool,
    /// The PTY the session observes through — the renderer registry's key. `NULL` for a harness
    /// row (no PTY) and after the orphan arm deleted the terminal row (FK `ON DELETE SET NULL`).
    pub terminal_run_id: Option<String>,
    /// The exit gate: the terminal row exists and records no exit (`exit_code IS NULL AND
    /// signal_killed = 0`). NO terminal row ⇒ `false`; a NULL of the `LEFT JOIN` never reads open.
    pub pty_open: bool,
}

/// The persisted completion-class evidence, one `MAX` per source (E3 is folded from the W rows by the
/// caller; the interactive PTY card's last output comes from the renderer registry, not a row).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Evidence {
    pub e1_harness_turn_completed: Option<i64>,
    pub e2_user_notify: Option<i64>,
    pub e7_agent_report_edit: Option<i64>,
}

impl Evidence {
    pub fn max(&self) -> Option<i64> {
        [
            self.e1_harness_turn_completed,
            self.e2_user_notify,
            self.e7_agent_report_edit,
        ]
        .into_iter()
        .flatten()
        .max()
    }
}

/// Tick enumeration: every track — no activity predicate, so a short task between two ticks on a quiet track is still found.
pub(crate) async fn track_ids(pool: &SqlitePool) -> Result<Vec<String>> {
    let rows = sqlx::query("SELECT id FROM tracks ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(|r| r.get::<String, _>("id")).collect())
}

pub(crate) async fn track_row(pool: &SqlitePool, track_id: &str) -> Result<Option<TrackRow>> {
    let row = sqlx::query("SELECT closed_at, updated_at FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| TrackRow {
        closed_at: r.get("closed_at"),
        updated_at: r.get("updated_at"),
    }))
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

/// Does ANY attempt row of the track name this card as its worker? Spliced into S0 twice (the
/// eligibility arm and the `task_bound` column) so the two stay one predicate.
fn task_bound_exists_sql(card_expr: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM tasks t WHERE t.track_id = ?1 AND t.worker_card_id = {card_expr})"
    )
}

/// S0 — the eligible sessions: the card's CURRENT session where the card is a harness card, the
/// current attempt's worker card, or an interactive card never bound to a task. A superseded
/// attempt's worker card matches none, so its leftover `failed` session never reaches the fold.
/// `pty_open` is read through a `LEFT JOIN terminals`: NO terminal row ⇒ closed — `COALESCE(…, 0)`
/// spells that contract out rather than relying on how a bare NULL decodes.
pub async fn eligible_sessions(pool: &SqlitePool, track_id: &str) -> Result<Vec<SessionRow>> {
    let sql = format!(
        "SELECT ws.id, c.id AS card_id, ws.provider, ws.state, \
                ws.updated_at_ms, ws.created_at_ms, \
                json_extract(ws.handle_state_json, '$.mode') AS mode, \
                {isolated} AS isolated, \
                {task_bound} AS task_bound, \
                ws.terminal_run_id, \
                COALESCE(te.exit_code IS NULL AND te.signal_killed = 0, 0) AS pty_open \
           FROM cards c JOIN worker_sessions ws ON ws.id = c.session_id \
           LEFT JOIN terminals te ON te.id = ws.terminal_run_id \
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
            updated_at_ms: r.get("updated_at_ms"),
            created_at_ms: r.get("created_at_ms"),
            mode: r.get("mode"),
            isolated: r.get::<bool, _>("isolated"),
            task_bound: r.get::<bool, _>("task_bound"),
            terminal_run_id: r.get("terminal_run_id"),
            pty_open: r.get::<bool, _>("pty_open"),
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

/// E1, E7 — two autocommit `MAX` statements; E2 is the caller's `MAX` over the N3 notify rows
/// it already read. E3 is computed from the W rows by the caller; the interactive PTY card's last
/// output is read from the renderer registry, not from a row.
pub(crate) async fn evidence(
    pool: &SqlitePool,
    track_id: &str,
    e2_user_notify: Option<i64>,
) -> Result<Evidence> {
    // E7 — a report rewrite by someone other than the user (`EditAuthor`
    // is bare-lowercase on the wire).
    let e7 = "SELECT MAX(at) FROM events \
               WHERE scope_track = ?1 AND kind = 'track.report_edited' \
                 AND json_extract(payload, '$.author') <> 'user'";
    Ok(Evidence {
        e1_harness_turn_completed: max_ms(pool, E1_HARNESS_TURN_COMPLETED_SQL, track_id).await?,
        e2_user_notify,
        e7_agent_report_edit: max_ms(pool, e7, track_id).await?,
    })
}

/// N0–N4 — the rows of the two notification sources and the dismissed keys, five autocommit
/// statements. N0: the track's one Planner card (a unique index); without one there is no notify
/// row, no last turn and no U.
pub(crate) async fn notification_rows(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<NotificationRows> {
    // N1 — the newest `ratify.*` event, kept only when it is a request (`events.id` is monotone,
    // so a later resolution hides it).
    let pending_ratify = sqlx::query(
        "SELECT id, at, kind, json_extract(payload, '$.reason') AS reason FROM events \
          WHERE scope_track = ?1 AND kind IN ('ratify.requested', 'ratify.resolved') \
          ORDER BY id DESC LIMIT 1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?
    .filter(|r| r.get::<&str, _>("kind") == "ratify.requested")
    .map(|r| PendingRatify {
        event_id: r.get("id"),
        at_ms: r.get("at"),
        reason: r.get("reason"),
    });
    // N4 — the keys the user dismissed (primary key prefix).
    let dismissed =
        sqlx::query_scalar("SELECT item_key FROM activity_dismissals WHERE track_id = ?1")
            .bind(track_id)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();

    let planner_card: Option<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(track_id)
            .fetch_optional(pool)
            .await?;
    let Some(planner_card) = planner_card else {
        return Ok(NotificationRows {
            pending_ratify,
            dismissed,
            ..NotificationRows::default()
        });
    };
    // N2 — U: the newest message the USER sent to the Planner card (an AI-header send and a send to
    // an assistant card are not replies).
    let user_sent = "SELECT MAX(at) FROM events \
         WHERE kind = 'harness.user_message.enqueued' AND scope_card = ?1 \
           AND json_extract(actor, '$.kind') = 'User'";
    let user_sent_at = max_ms(pool, user_sent, &planner_card).await?;

    let mut notifies = Vec::new();
    let mut last_turn = None;
    for r in sqlx::query(N3_PLANNER_TRANSCRIPT_SQL)
        .bind(&planner_card)
        .fetch_all(pool)
        .await?
    {
        let (row_id, at_ms, text) = (r.get("id"), r.get("created_at_ms"), r.get("text"));
        if r.get::<&str, _>("arm") == "notify" {
            notifies.push(NotifyRow {
                row_id,
                at_ms,
                text,
            });
        } else {
            last_turn = Some(LastTurn {
                row_id,
                at_ms,
                status: r.get("status"),
                error_message: text,
            });
        }
    }
    Ok(NotificationRows {
        pending_ratify,
        user_sent_at,
        notifies,
        last_turn,
        dismissed,
    })
}
