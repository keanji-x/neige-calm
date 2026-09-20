//! What happened in the workspace during one half-open time window: a server-side projection over the event log, injected into the prompt — there is no MCP tool for it.
//! The window is expressed purely in `at` (wall clock), never the `id` cursor. Rows written before migration 0007 have no `scope_track`, so a window spanning that upgrade silently under-counts.
//! Counts only: the rendered prompt must have a length bound computable without running it (`MAX_PLANNER_INPUT_CHARS`).

use sqlx::{Pool, Sqlite};

use crate::db::Repo;
use crate::error::{CalmError, Result};

/// The event kinds that count as workspace activity. None may ever join `EVENTS_PRUNE_KINDS` (pinned by `events_pruner::activity_window_kinds_are_never_prunable`).
/// `turns` is deliberately absent: its only fact source, `harness.item.added`, is pruned after 30 days.
pub const ACTIVITY_KINDS: [&str; 4] = [
    "track.lifecycle_changed",
    "track.report_edited",
    "task.completed",
    "task.failed",
];

/// The one statement this projection runs. One query, not two: the per-kind counts and the distinct-track count come from a single snapshot, so they cannot disagree.
/// `?1`/`?2` the half-open window; `?3` the track to exclude or NULL; `?4`..`?7` [`ACTIVITY_KINDS`], bound twice (restrict and bucket).
/// The join runs through `tracks`/`areas` rather than trusting the write-time `events.scope_area` snapshot; `areas.kind = 'user'` is the same visibility predicate as `GET /api/areas`.
const ACTIVITY_QUERY: &str = r#"
    SELECT COALESCE(SUM(e.kind = ?4), 0) AS lifecycle,
           COALESCE(SUM(e.kind = ?5), 0) AS report,
           COALESCE(SUM(e.kind = ?6), 0) AS completed,
           COALESCE(SUM(e.kind = ?7), 0) AS failed,
           COUNT(DISTINCT w.id)          AS tracks
      FROM events e
      JOIN tracks w ON w.id = e.scope_track
      JOIN areas c ON c.id = w.area_id
     WHERE e.at >= ?1
       AND e.at <  ?2
       AND c.kind = 'user'
       AND (?3 IS NULL OR w.id <> ?3)
       AND e.kind IN (?4, ?5, ?6, ?7)
"#;

/// One window's worth of activity. Integers only.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceActivityWindow {
    pub track_lifecycle_changed: i64,
    pub track_report_edited: i64,
    pub task_completed: i64,
    pub task_failed: i64,
    /// How many distinct tracks contributed any of the counted events.
    pub tracks_touched: i64,
}

impl WorkspaceActivityWindow {
    /// Total counted events; `tracks_touched` is a dimension of the same rows, not more of them.
    /// Saturating so the widest renderable value (`i64::MIN`, used by the prompt-length bounds) does not panic in debug builds.
    pub fn total_events(&self) -> i64 {
        self.track_lifecycle_changed
            .saturating_add(self.track_report_edited)
            .saturating_add(self.task_completed)
            .saturating_add(self.task_failed)
    }

    /// Nothing counted.
    pub fn is_empty(&self) -> bool {
        self.total_events() == 0
    }
}

/// Aggregate workspace activity over the half-open window `[start_ms, end_ms)`.
/// `exclude_track` is the reflexive exclusion (writing the summary is itself a `track.report_edited` on the launchpad); defence in depth, since the visibility join already drops the system area.
pub async fn workspace_activity_window(
    pool: &Pool<Sqlite>,
    start_ms: i64,
    end_ms: i64,
    exclude_track: Option<&str>,
) -> Result<WorkspaceActivityWindow> {
    let [lifecycle, report, completed, failed] = ACTIVITY_KINDS;

    let (track_lifecycle_changed, track_report_edited, task_completed, task_failed, tracks_touched) =
        sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(ACTIVITY_QUERY)
            .bind(start_ms)
            .bind(end_ms)
            .bind(exclude_track)
            .bind(lifecycle)
            .bind(report)
            .bind(completed)
            .bind(failed)
            .fetch_one(pool)
            .await?;

    Ok(WorkspaceActivityWindow {
        track_lifecycle_changed,
        track_report_edited,
        task_completed,
        task_failed,
        tracks_touched,
    })
}

/// Today's window, computed once and the same way for every caller, so the two surfaces cannot disagree on the day or the counts.
pub async fn todays_workspace_activity(
    pool: &Pool<Sqlite>,
    exclude_track: Option<&str>,
) -> Result<WorkspaceActivityWindow> {
    let (start_ms, end_ms) = local_day_window(crate::model::now_ms());
    workspace_activity_window(pool, start_ms, end_ms, exclude_track).await
}

/// The five counts, as five lines — the only place production renders them, and where the length bound lives (fixed template plus five `i64`s).
pub fn activity_counts_block(activity: &WorkspaceActivityWindow) -> String {
    format!(
        "- tracks whose lifecycle changed: {}\n\
         - report edits: {}\n\
         - tasks completed: {}\n\
         - tasks failed: {}\n\
         - distinct tracks touched: {}\n",
        activity.track_lifecycle_changed,
        activity.track_report_edited,
        activity.task_completed,
        activity.task_failed,
        activity.tracks_touched,
    )
}

/// What a conversation started on the launchpad track opens with: material, not an instruction.
/// An empty day is stated, not skipped — otherwise "the server told the agent about today" is silently untrue exactly when the day is empty.
pub fn opening_activity_briefing(activity: &WorkspaceActivityWindow) -> String {
    if activity.is_empty() {
        return "Context from the server before you start: nothing has been \
                recorded in this workspace today — no track lifecycle changes, \
                no report edits, no completed or failed tasks. That is the \
                whole of the day's activity data, and it is empty. If you are \
                asked what happened today, say that nothing was recorded \
                rather than inferring work from the workspace."
            .to_string();
    }
    format!(
        // Deliberately not the summary prompt's opening phrase: `today_summary`'s cases identify a delivered message by its bytes.
        "Context from the server before you start. Here is what this workspace \
         recorded today, counted by the server. These counts are all the \
         activity data available to you — there is no tool to query for more, \
         so do not invent specifics:\n\
         {}",
        activity_counts_block(activity),
    )
}

/// The server-local day containing `now_ms`, as the half-open window `[midnight, next midnight)`.
/// The zone is the server's; adjacency is exact because day N's `end` and day N+1's `start` are the same computation.
pub fn local_day_window(now_ms: i64) -> (i64, i64) {
    use chrono::{Local, TimeZone};

    let today = Local
        .timestamp_millis_opt(now_ms)
        .single()
        // A UTC instant maps to exactly one local time in every zone; the
        // ambiguity `LocalResult` exists for runs the other way.
        .expect("a millisecond instant has one local rendering")
        .date_naive();
    let tomorrow = today
        .succ_opt()
        .expect("the calendar does not end within this program's lifetime");
    (
        local_start_of_day_ms(today),
        local_start_of_day_ms(tomorrow),
    )
}

/// The first instant of `date` in the server's local zone.
fn local_start_of_day_ms(date: chrono::NaiveDate) -> i64 {
    use chrono::{Local, LocalResult, TimeZone};

    // A DST fold: 00:00 happened twice; the day starts at the first one. A DST gap: 00:00 never happened (e.g. America/Santiago);
    // the day starts at the first wall-clock minute that exists — never a silent hole, never `start` past `end`.
    for minutes in 0..(3 * 60) {
        let naive = date
            .and_hms_opt(minutes / 60, minutes % 60, 0)
            .expect("hours below 3 and minutes below 60 are valid times");
        match Local.from_local_datetime(&naive) {
            LocalResult::Single(at) => return at.timestamp_millis(),
            LocalResult::Ambiguous(earliest, _) => return earliest.timestamp_millis(),
            LocalResult::None => continue,
        }
    }
    // Unreachable for any real zone (no offset shift exceeds three hours); fail-safe UTC boundary rather than a panic.
    date.and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp_millis()
}

/// Today's activity briefing, if this track is the launchpad; `None` for every other track and for a workspace with no launchpad yet (nothing is ensured from here).
/// Both reads are single autocommit statements off the pool, placed before the transaction's first write to stay out of the lock cycle.
pub(crate) async fn launchpad_opening_briefing(
    repo: &dyn Repo,
    track_id: &str,
) -> Result<Option<String>> {
    if !crate::routes::today::is_launchpad_track(repo, track_id).await? {
        return Ok(None);
    }
    let pool = repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("today's activity window requires a sqlite-backed repo".into())
    })?;
    let activity = todays_workspace_activity(&pool, Some(track_id)).await?;
    Ok(Some(opening_activity_briefing(&activity)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;

    /// Renaming an event in `calm-types` would otherwise leave this projection silently counting zero of that kind: the allowlist is `&str` and the query matches nothing rather than failing.
    #[test]
    fn the_allowlist_spells_the_kernel_s_own_event_kinds() {
        use crate::event::Event;
        use crate::model::TrackLifecycle;

        let lifecycle = Event::TrackLifecycleChanged {
            id: crate::ids::TrackId::from("w".to_string()),
            area_id: crate::ids::AreaId::from("c".to_string()),
            from: TrackLifecycle::Draft,
            to: TrackLifecycle::Planning,
            agent_message: None,
        };
        let completed = Event::TaskCompleted {
            idempotency_key: "k".into(),
            result: serde_json::Value::Null,
            artifacts: Vec::new(),
            agent_message: None,
        };
        let failed = Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "e".into(),
            details: None,
            agent_message: None,
        };
        assert_eq!(
            [
                lifecycle.kind_tag(),
                "track.report_edited",
                completed.kind_tag(),
                failed.kind_tag(),
            ],
            ACTIVITY_KINDS,
            "the allowlist must spell the kinds the kernel writes"
        );
    }

    struct Fixture {
        repo: SqlxRepo,
    }

    impl Fixture {
        async fn new() -> Self {
            let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
            Self { repo }
        }

        async fn area(&self, id: &str, kind: &str) {
            sqlx::query(
                "INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) \
                 VALUES(?1,?1,'#abc',1,?2,1,1)",
            )
            .bind(id)
            .bind(kind)
            .execute(self.repo.pool())
            .await
            .unwrap();
        }

        async fn track(&self, id: &str, area_id: &str) {
            sqlx::query(
                "INSERT INTO tracks(id,area_id,title,sort,lifecycle,created_at,updated_at) \
                 VALUES(?1,?2,?1,1,'draft',1,1)",
            )
            .bind(id)
            .bind(area_id)
            .execute(self.repo.pool())
            .await
            .unwrap();
        }

        /// A raw event row at a chosen `at`; these cases need millisecond control over `at` that no production emitter offers.
        async fn event(&self, kind: &str, track_id: &str, at: i64) {
            sqlx::query(
                "INSERT INTO events(kind,payload,actor,at,scope_kind,scope_track) \
                 VALUES(?1,'{}','user',?2,'track',?3)",
            )
            .bind(kind)
            .bind(at)
            .bind(track_id)
            .execute(self.repo.pool())
            .await
            .unwrap();
        }

        async fn window(&self, start: i64, end: i64) -> WorkspaceActivityWindow {
            workspace_activity_window(self.repo.pool(), start, end, None)
                .await
                .unwrap()
        }
    }

    /// The window is half-open, so a midnight-boundary event is counted by exactly one of two adjacent days.
    /// Both halves are needed: "counted today" alone passes a closed window, "not counted yesterday" alone passes an open one.
    #[tokio::test]
    async fn a_boundary_event_belongs_to_the_later_day_and_only_to_it() {
        let f = Fixture::new().await;
        f.area("area-user", "user").await;
        f.track("track-1", "area-user").await;

        let midnight = 1_700_000_000_000;
        let day = 86_400_000;
        f.event("track.report_edited", "track-1", midnight).await;

        let today = f.window(midnight, midnight + day).await;
        assert_eq!(today.track_report_edited, 1, "{today:?}");
        assert_eq!(today.tracks_touched, 1, "{today:?}");

        let yesterday = f.window(midnight - day, midnight).await;
        assert_eq!(
            yesterday.track_report_edited, 0,
            "an event at the boundary must not also count for the day that \
             ends there: {yesterday:?}"
        );
        assert_eq!(yesterday.tracks_touched, 0, "{yesterday:?}");

        // The instant before the boundary belongs to the earlier day, which is
        // what makes the two windows a partition rather than a gap.
        f.event("track.report_edited", "track-1", midnight - 1)
            .await;
        assert_eq!(
            f.window(midnight - day, midnight).await.track_report_edited,
            1
        );
        assert_eq!(
            f.window(midnight, midnight + day).await.track_report_edited,
            1
        );
    }

    /// The negative is the load-bearing half and needs a track of its own: the per-kind columns are indifferent to the `kind IN (...)` conjunct,
    /// so only a track whose entire day is unlisted kinds makes `tracks_touched` detect its deletion.
    #[tokio::test]
    async fn each_kind_lands_in_its_own_field_and_unlisted_kinds_are_ignored() {
        let f = Fixture::new().await;
        f.area("area-user", "user").await;
        f.track("track-1", "area-user").await;
        f.track("track-2", "area-user").await;
        // The discriminator: it carries ONLY unlisted kinds.
        f.track("track-3", "area-user").await;

        f.event("track.lifecycle_changed", "track-1", 10).await;
        f.event("track.report_edited", "track-1", 11).await;
        f.event("track.report_edited", "track-2", 12).await;
        f.event("task.completed", "track-2", 13).await;
        f.event("task.failed", "track-2", 14).await;
        f.event("harness.item.added", "track-1", 15).await;
        f.event("card.updated", "track-2", 16).await;
        f.event("harness.item.added", "track-3", 17).await;
        f.event("card.updated", "track-3", 18).await;

        let window = f.window(0, 100).await;
        assert_eq!(
            window,
            WorkspaceActivityWindow {
                track_lifecycle_changed: 1,
                track_report_edited: 2,
                task_completed: 1,
                task_failed: 1,
                // Two, not three: `track-3` had a busy day of kinds this
                // projection does not count, and a day of those is not a day.
                tracks_touched: 2,
            }
        );
        assert!(!window.is_empty());
        assert_eq!(window.total_events(), 5);
    }

    /// The system area is not activity; if these rows counted, no workspace would ever have an empty day.
    #[tokio::test]
    async fn only_user_visible_areas_count_as_activity() {
        let f = Fixture::new().await;
        f.area("area-system", "system").await;
        f.track("track-launchpad", "area-system").await;
        f.event("track.report_edited", "track-launchpad", 10).await;

        let window = f.window(0, 100).await;
        assert!(
            window.is_empty(),
            "a system-area track must not make the day look busy: {window:?}"
        );
    }

    /// The track sits in a user area, so the visibility join lets it through and the exclusion predicate is the only thing that can drop it.
    #[tokio::test]
    async fn reflexive_exclusion_drops_the_named_track_before_the_visibility_join() {
        let f = Fixture::new().await;
        f.area("area-user", "user").await;
        f.track("track-self", "area-user").await;
        f.track("track-other", "area-user").await;
        f.event("track.report_edited", "track-self", 10).await;
        f.event("track.report_edited", "track-other", 11).await;

        let excluded = workspace_activity_window(f.repo.pool(), 0, 100, Some("track-self"))
            .await
            .unwrap();
        assert_eq!(excluded.track_report_edited, 1, "{excluded:?}");
        assert_eq!(excluded.tracks_touched, 1, "{excluded:?}");

        let kept = f.window(0, 100).await;
        assert_eq!(
            kept.track_report_edited, 2,
            "without the exclusion both rows count — otherwise the case above \
             proves nothing: {kept:?}"
        );
    }

    /// The load-bearing half is that the two states produce different text; asserted by content, since the failure renders as a plausible message.
    #[test]
    fn an_empty_day_is_briefed_as_an_empty_day_rather_than_as_no_briefing() {
        let empty = opening_activity_briefing(&WorkspaceActivityWindow::default());
        assert!(
            !empty.trim().is_empty(),
            "an empty day must still be stated: {empty}"
        );
        assert!(
            empty.contains("nothing has been recorded"),
            "the empty branch has to name the emptiness, not just omit the \
             counts: {empty}"
        );
        assert!(
            !empty.contains("- report edits:"),
            "…and it must not carry a block of zeroes: {empty}"
        );

        let busy = opening_activity_briefing(&WorkspaceActivityWindow {
            track_report_edited: 2,
            tracks_touched: 1,
            ..WorkspaceActivityWindow::default()
        });
        assert!(busy.contains("- report edits: 2"), "{busy}");
        assert_ne!(
            empty, busy,
            "the two days must not read identically to the agent"
        );
    }

    /// `i64::MIN` five times is the widest rendering of the counts block, so the bound is arithmetic rather than a sample.
    #[test]
    fn the_opening_briefing_is_bounded_far_below_the_planner_input_ceiling() {
        let widest = opening_activity_briefing(&WorkspaceActivityWindow {
            track_lifecycle_changed: i64::MIN,
            track_report_edited: i64::MIN,
            task_completed: i64::MIN,
            task_failed: i64::MIN,
            tracks_touched: i64::MIN,
        });
        assert!(
            widest.chars().count() < crate::routes::cards::MAX_PLANNER_INPUT_CHARS,
            "the briefing must fit `planner/input` for every possible count; it \
             is {} chars",
            widest.chars().count()
        );
    }

    /// Asserted as a property rather than against a fixed timestamp: the suite runs under whatever `TZ` the machine has.
    #[test]
    fn day_windows_tile_the_timeline_without_gaps_or_overlap() {
        let day = 86_400_000_i64;
        let noon = 1_700_000_000_000;
        let (start, end) = local_day_window(noon);
        assert!(start <= noon && noon < end, "{start} <= {noon} < {end}");
        // 23h — never enough to skip a day, and enough to land in the next one whatever the offset.
        let (next_start, next_end) = local_day_window(end + 23 * 3_600_000);
        assert_eq!(
            end, next_start,
            "day N must end at the same instant day N+1 starts"
        );
        assert!(next_end > next_start);
        // A day is a day, give or take a DST shift.
        assert!((end - start - day).abs() <= 2 * 3_600_000, "{start}..{end}");
    }
}
