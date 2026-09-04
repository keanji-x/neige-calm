//! #1253 D4 — what happened in the workspace during one half-open time window.
//!
//! A server-side projection over the event log, and **deliberately nothing
//! else**: there is no MCP tool here and there must not be one. The design cut
//! that layer (§0b.4) after establishing that a built-in tool with
//! `visible_to_roles: &[]` is invisible to every model — `registry.rs`'s
//! `descriptors_for_role` is a plain `contains(&role)` filter and the only
//! per-identity augmentation seam handles *plugin* tools. Activity is computed
//! here and injected into the prompt; the agent never queries for it.
//!
//! # Two callers, one computation
//!
//! Since #1343 this projection is read from two places:
//!
//! * `routes::today_summary` — `POST /api/today/summary`, which turns it into
//!   an instruction to rewrite the day's report;
//! * `routes::track_conversations` — a conversation started **on the launchpad
//!   track**, which opens with it as material rather than as an instruction.
//!
//! Neither computes it for itself. [`todays_workspace_activity`] is the one
//! entry point both go through, and [`activity_counts_block`] is the one
//! rendering of the counts, so the two surfaces cannot report different numbers
//! for the same day. What they do *not* share is the sentence around those
//! counts — one asks for a report, the other hands over material — and that
//! difference is the whole reason there are two callers.
//!
//! # `at` is a wall clock, `id` is the cursor — never mix them
//!
//! Copied from `0004_events.sql`, which is where this rule is written:
//!
//! > `at` is the wall-clock timestamp; `id` is the ordering / cursor key.
//! > Never mix the two.
//!
//! This is the repository's first *read* path keyed on `at` (`events_prune`
//! already DELETEs by it). The window below is therefore expressed purely in
//! `at`, and no id cursor appears anywhere in this module. A future caller that
//! wants "everything since the last summary" must not paginate this by id: two
//! rows written in the same millisecond order by id, and `at` can go backwards
//! across a clock adjustment.
//!
//! # A window spanning an upgrade silently under-counts
//!
//! Also copied, from `0007_events_scope.sql`:
//!
//! > Old rows: every existing row backfills to `scope_kind = 'system'` with
//! > NULL ancestor cols via the column default. **We do NOT best-effort
//! > backfill from payload joins.**
//!
//! The join below is on `events.scope_track`, so every row written before
//! migration 0007 is invisible to it, whatever its kind. A window that spans
//! the upgrade point reports less activity than actually happened, with no
//! error and no warning.
//!
//! The same hazard points forward: an emitter that logs one of the allowlisted
//! kinds at `EventScope::System` writes a row this projection cannot see, and
//! nothing — no type, no migration, no test — forbids that.
//!
//! **There is deliberately no enumeration of emitters here any more.** Two were
//! written and both were wrong. The first named three sites; review found four
//! more (`reaper`, `scheduler`, `bin/replay`). The second named those; review
//! then found two further `task.*` producers, and grepping the two variant
//! constructors across `crates/calm-server/src` returns on the order of forty
//! matches spread over `dispatcher`, `scheduler`, `reaper`, `decision_sink` and
//! three MCP tools. A list that has been incomplete twice, in a file nothing
//! recompiles when an emitter is added, is not evidence — it is a comment that
//! reads like evidence, which is worse than saying nothing.
//!
//! So what is actually known is stated instead, with its carrier:
//!
//! * **Verified by execution, per kind**: `track.report_edited` and
//!   `track.lifecycle_changed` are driven through their production REST routes
//!   and counted, by
//!   `today_summary::a_real_report_edit_and_a_real_lifecycle_change_are_both_counted_as_activity`.
//! * **Assumed, and unpinned**: that every `task.completed` / `task.failed`
//!   emitter uses a track or card scope. Spot reads support it and no
//!   counter-example is known, but no test drives one and no enumeration of
//!   them should be trusted.
//!
//! What would settle it is a guard rather than a list: a kernel-level rule that
//! these kinds may not be logged at `System` scope, or one end-to-end case per
//! kind. Neither is in this slice.
//!
//! # Counts only — this is what bounds the prompt
//!
//! Every field of [`WorkspaceActivityWindow`] is an integer. No track titles, no
//! area names, no detail lists, and nothing else of variable length. That is
//! not tidiness: the prompt this feeds goes through `POST /api/cards/{id}/planner/input`,
//! which rejects anything over `MAX_PLANNER_INPUT_CHARS` (32,768) — so the
//! rendered prompt's length has to have a bound that can be computed without
//! running it. Template text plus five integers has one. A "just for context"
//! list of track titles does not, and re-introducing one means also
//! re-introducing a deterministic character budget and its 32,768 / 32,769 /
//! CJK boundary cases (design D4).

use sqlx::{Pool, Sqlite};

use crate::db::Repo;
use crate::error::{CalmError, Result};

/// The event kinds that count as workspace activity, adjudicated in design D4.
///
/// All four must outlive every retention pass, and they do: none is in
/// `EVENTS_PRUNE_KINDS` (`calm-truth/src/events_prune.rs`), whose allowlist is
/// exact-kind and fails safe, so a kind absent from it is permanent by
/// construction.
///
/// That is a claim about a `&'static [&str]` in another crate, so it is read by
/// a test rather than asserted here:
/// `events_pruner::activity_window_kinds_are_never_prunable` compares the two
/// lists directly, in the same shape as the existing
/// `first_message_dedup_kind_is_never_prunable`. Without it, adding one of
/// these four to the prune allowlist would silently make every window older
/// than the horizon read as an empty day — and an empty day is refused.
///
/// **`turns` is deliberately absent.** Its only fact source,
/// `harness.item.added`, *is* in the 30-day prune allowlist, so a turn count
/// would silently decay to zero past the horizon; there is no permanent
/// substitute (design D4).
pub const ACTIVITY_KINDS: [&str; 4] = [
    "track.lifecycle_changed",
    "track.report_edited",
    "task.completed",
    "task.failed",
];

/// The one statement this projection runs.
///
/// **One query, not two, and that is a correctness property rather than a
/// tidiness one.** The per-kind counts and the distinct-track count are read in
/// a single pass over a single snapshot, so they cannot disagree. Two
/// statements sharing the same FROM/WHERE *text* are still two snapshots: an
/// event landing between them yields a window reporting one event across two
/// tracks, which is not a state the workspace was ever in, and nothing about the
/// result would look wrong.
///
/// `SUM(e.kind = ?n)` is SQLite's boolean-as-integer, and `COALESCE` covers the
/// empty-window case, where `SUM` over no rows is NULL rather than 0.
///
/// * `?1`/`?2` — the half-open window, `[start, end)`. See INV-TODAYDOC-006:
///   an event at the boundary belongs to the later day and to that day only.
/// * `?3` — the track to exclude, or NULL.
/// * `?4`..`?7` — [`ACTIVITY_KINDS`], bound positionally, and bound twice: once
///   to restrict the rows and once to bucket them, so a kind can never be
///   counted into a column the query did not ask for.
///
/// The join runs through `tracks` and `areas` rather than trusting
/// `events.scope_area`: the scope columns are a snapshot taken at write time,
/// while `tracks.area_id` is current, and a track whose row is gone should not
/// contribute activity to a page that cannot link to it.
///
/// `areas.kind = 'user'` is the visibility filter, and it is the same predicate
/// `areas_list_user_visible` uses for `GET /api/areas` (#175). It is what keeps
/// the system area — and therefore the launchpad — out of the count.
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

/// One window's worth of activity. Integers only — see the module docs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceActivityWindow {
    pub track_lifecycle_changed: i64,
    pub track_report_edited: i64,
    pub task_completed: i64,
    pub task_failed: i64,
    /// How many distinct tracks contributed any of the counted events.
    ///
    /// Distinct tracks, not a sum: it answers "how broad was the day", which the
    /// four counts cannot, and it is still one integer.
    pub tracks_touched: i64,
}

impl WorkspaceActivityWindow {
    /// Total counted events. `tracks_touched` is not added in — it is a
    /// dimension of the same rows, not more of them.
    ///
    /// Saturating, which costs nothing and buys one thing: the fields are
    /// `COUNT`/`SUM` results and cannot be negative in production, so the
    /// widest *renderable* value (`i64::MIN`, which the prompt-length bounds
    /// are computed at) is a value this sum would otherwise panic on in a debug
    /// build. A bound that cannot be measured is not a bound.
    pub fn total_events(&self) -> i64 {
        self.track_lifecycle_changed
            .saturating_add(self.track_report_edited)
            .saturating_add(self.task_completed)
            .saturating_add(self.task_failed)
    }

    /// Nothing counted. The gate INV-TODAYDOC-007 is written against.
    pub fn is_empty(&self) -> bool {
        self.total_events() == 0
    }
}

/// Aggregate workspace activity over the half-open window `[start_ms, end_ms)`.
///
/// `exclude_track` is the **reflexive exclusion**, and it is defence in depth
/// rather than a load-bearing wall — stating that precisely is part of the
/// design's ruling (D4). Writing the summary is itself a `track.report_edited`
/// on the launchpad track, so without an exclusion each run would feed the next
/// one its own footprint. But the launchpad lives in the system area and
/// `c.kind = 'user'` above has already dropped it, so deleting this predicate
/// turns no test red *through the join*. What pins it is
/// `reflexive_exclusion_drops_the_named_track_before_the_visibility_join` in
/// this module, which drives the predicate directly on raw allowlisted rows in
/// a user area — i.e. before the visibility filter can mask it. The predicate
/// becomes load-bearing the day the visibility join widens or the launchpad
/// moves out of the system area.
///
/// The exclusion is by **track**, so a human hand-editing Today's report does
/// not register as activity either. That is intended (design D4).
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

/// Today's window, computed once and the same way for every caller.
///
/// The two surfaces that need the day's activity (#1343) both come through
/// here, so "the server's day" and "the server's counts" have exactly one
/// definition. Splitting `local_day_window(now_ms())` back out to the call
/// sites is what would let one of them drift onto a different clock reading or
/// a different exclusion.
///
/// `exclude_track` is [`workspace_activity_window`]'s reflexive exclusion,
/// passed through unchanged; see that function for what it is and is not.
pub async fn todays_workspace_activity(
    pool: &Pool<Sqlite>,
    exclude_track: Option<&str>,
) -> Result<WorkspaceActivityWindow> {
    let (start_ms, end_ms) = local_day_window(crate::model::now_ms());
    workspace_activity_window(pool, start_ms, end_ms, exclude_track).await
}

/// The five counts, as five lines. The only place production renders them —
/// tests spell the strings back, which is what makes them assertions.
///
/// Both prompts that carry the window embed this, which is what keeps them from
/// disagreeing about what a field is called or which fields exist. It is also
/// where the length bound lives: a fixed template plus five `i64`s has a
/// maximum length that can be computed by reading it, which is what lets
/// `today_summary`'s `the_prompt_is_bounded_far_below_the_planner_input_ceiling`
/// and this module's `the_opening_briefing_is_bounded_...` both be arithmetic
/// rather than sampling.
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

/// What a conversation started on the launchpad track opens with (#1343).
///
/// **Material, not an instruction.** The summary endpoint's prompt tells the
/// agent to rewrite the report; this one tells it nothing to do. A conversation
/// the user starts is theirs to steer, and an opening message that issued an
/// order would take that turn away from them before they had typed anything.
///
/// **An empty day is stated, not skipped, and that is the ruling this function
/// exists to record.** The alternatives were both rejected: sending nothing
/// makes "the server told the agent about today" silently untrue exactly when
/// the day is empty — an agent asked "what happened today?" would then answer
/// from whatever it could find in the workspace instead of from the server's
/// count — and refusing to create the conversation would take a working
/// endpoint away over a condition the user did not ask about. So the empty case
/// gets its own sentence: the day's material is that there is none.
///
/// This is *not* INV-TODAYDOC-007. That invariant is about
/// `POST /api/today/summary` refusing to commission a report from nothing, and
/// it is unchanged and still enforced there; this path commissions nothing.
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
        // The wording deliberately avoids the summary prompt's opening phrase:
        // `today_summary`'s cases identify a delivered message by its bytes,
        // and two prompts sharing a lead sentence would make "the summary
        // arrived" unfalsifiable on any card that also holds this briefing.
        "Context from the server before you start. Here is what this workspace \
         recorded today, counted by the server. These counts are all the \
         activity data available to you — there is no tool to query for more, \
         so do not invent specifics:\n\
         {}",
        activity_counts_block(activity),
    )
}

/// The server-local day containing `now_ms`, as the half-open window
/// `[midnight, next midnight)`.
///
/// **The day boundary is a window edge, not a key.** Nothing is stored under
/// it, nothing is looked up by it, and there is no per-day row anywhere — the
/// cross-day history this used to imply was cut in design r4. That is what
/// makes the server's local timezone an acceptable answer.
///
/// **What it costs, and when it stops being harmless.** The zone is the
/// server's, so a user in another zone sees a "today" that starts and ends at
/// the server's midnight. On the deployment this is written for — one box on a
/// LAN, one user, both in one zone — the two never disagree. It stops being
/// harmless as soon as either half of that is false: a user travelling across
/// zones, or two users in different zones, would see a day boundary that is not
/// theirs, with the visible symptom being a summary that reports the wrong
/// slice of work rather than an error. The fix at that point is a workspace
/// timezone setting, which is a product decision in its own right and is
/// deliberately **not** taken here (design D4, §8).
///
/// Adjacency is exact by construction: day N's `end` is computed by the same
/// call that produces day N+1's `start`, so the two are the same integer and no
/// event can fall in both windows or in neither.
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

    // A DST fold: 00:00 happened twice. The day starts at the first one.
    // A DST gap: 00:00 never happened at all (a handful of zones spring
    // forward at midnight, e.g. America/Santiago). The day then starts at the
    // first wall-clock minute that does exist, which the scan finds — never a
    // silent hour-long hole in the window, and never a `start` past `end`.
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
    // Unreachable for any real zone (no offset shift exceeds three hours), and
    // fail-safe rather than panicking: a UTC-based boundary still yields a
    // window of the right length whose day N end equals day N+1's start.
    date.and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp_millis()
}

/// Today's activity briefing, if this track is the launchpad (#1343).
///
/// `None` for every other track. The predicate is
/// [`routes::today::is_launchpad_track`], which is the one criterion in the
/// codebase — the agent's identity (`planner_harness_start_adapter`) forks on
/// the same call, and two spellings of "is this the launchpad?" would let the
/// briefing and the identity disagree about one track.
///
/// [`routes::today::is_launchpad_track`]: crate::routes::today::is_launchpad_track
///
/// The window itself comes from [`todays_workspace_activity`], the same entry
/// `POST /api/today/summary` uses, so the two surfaces cannot report different
/// numbers for one day. The launchpad excludes itself — that is the reflexive
/// exclusion documented on `workspace_activity_window`, so a report the agent
/// goes on to write does not turn up in the next briefing as activity the
/// workspace did.
///
/// **A workspace with no launchpad yet is `None`, not an empty briefing.**
/// Nothing is ensured from here: `ensure_today_launchpad` materialises a
/// workspace and waits on a `planner-harness-start` (INV-TODAYDOC-001), and a
/// conversation create on some other track has no business doing that.
///
/// It takes a bare `&dyn Repo` rather than the route states it was written
/// against because since #1314 its caller is
/// `PlannerHarnessStartAdapter::prepare_tx`, which holds neither. Both reads
/// below are single autocommit statements off the pool; see the call site for
/// why that, and their placement before the transaction's first write, is what
/// keeps them out of the lock cycle.
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

    /// The four allowlisted strings are the kernel's, not this module's
    /// spelling of them.
    ///
    /// Without this, renaming an event — which `calm-types` does by editing one
    /// `#[serde(rename)]` — leaves this projection silently counting zero of
    /// that kind forever. There is no compiler edge between the two: the
    /// allowlist is `&str`, and the query it feeds matches nothing rather than
    /// failing.
    ///
    /// It is asserted against `kind_tag()` on real `Event` values because that
    /// is the same function the write path stamps `events.kind` with.
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

        /// A raw event row at a chosen `at`. Raw on purpose: these cases are
        /// about the *window arithmetic* and the *predicate*, which need
        /// millisecond control over `at` that no production emitter offers.
        /// That the four kind strings and the scope column match what the
        /// emitters actually write is pinned elsewhere — by
        /// `the_allowlist_spells_the_kernel_s_own_event_kinds` above for the
        /// strings, and by `today_summary::the_projection_counts_a_real_report_edit`
        /// (an end-to-end drive of `POST /api/tracks/{id}/report`) for the row
        /// shape.
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

    /// INV-TODAYDOC-006 — the window is half-open, so a midnight-boundary event
    /// is counted by exactly one of two adjacent days.
    ///
    /// Both halves are asserted, and both are needed: "counted today" alone
    /// stays green if the window became `[start, end]` at both ends, and "not
    /// counted yesterday" alone stays green if it became `(start, end)`.
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

    /// Each allowlisted kind lands in its own field, and a kind outside the
    /// allowlist contributes nothing.
    ///
    /// The negative is the load-bearing half, and it needs a track of its own.
    ///
    /// The per-kind columns are `SUM(e.kind = ?n)`, so they are indifferent to
    /// the `kind IN (...)` conjunct — dropping it changes only which rows reach
    /// `COUNT(DISTINCT w.id)`. A first version of this case put its unlisted
    /// events on tracks that already had listed ones, which made `tracks_touched`
    /// indifferent too: **the mutation that deletes the restriction measured
    /// 8/8 green.** `track-3` exists so that deleting it counts a track whose
    /// entire day consisted of kinds this projection does not count — and
    /// `harness.item.added` is deliberately one of them, since it is
    /// high-frequency AND pruned after 30 days, i.e. exactly the decay the
    /// allowlist exists to prevent.
    #[tokio::test]
    async fn each_kind_lands_in_its_own_field_and_unlisted_kinds_are_ignored() {
        let f = Fixture::new().await;
        f.area("area-user", "user").await;
        f.track("track-1", "area-user").await;
        f.track("track-2", "area-user").await;
        // The discriminator. It carries ONLY unlisted kinds, so it is the one
        // track whose presence in `tracks_touched` can distinguish a query that
        // restricts by kind from one that does not — see the note below.
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

    /// The system area is not activity, which is what INV-TODAYDOC-007's
    /// "empty window" is able to mean at all.
    ///
    /// The launchpad track lives there, and so does everything the kernel does
    /// on its own behalf. If these rows counted, no workspace would ever have
    /// an empty day and the gate would be unreachable.
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

    /// The reflexive exclusion, driven where it can actually be observed.
    ///
    /// The track here sits in a **user** area, so the visibility join lets it
    /// through and the exclusion predicate is the only thing that can drop it.
    /// In production it is not: the launchpad is in the system area and the
    /// case above already removes it, which is why the design calls this
    /// defence in depth and not a wall. Pinning it here rather than through the
    /// endpoint is the design's own instruction — the alternative was writing
    /// an invariant that cannot be falsified.
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

    /// #1343 — an empty day still produces material, and it says so.
    ///
    /// The load-bearing half is that the two states produce *different* text:
    /// a briefing that returned the same sentence either way would satisfy
    /// "something was sent" while telling the agent nothing about which day it
    /// is in. The empty branch is asserted by content rather than by length,
    /// because the failure this rules out — silently feeding an empty count
    /// block — renders as a perfectly plausible message.
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

    /// The briefing fits `planner/input` for every count the type admits.
    ///
    /// Same reasoning as `today_summary`'s bound on the summary prompt, and it
    /// needs its own case because it is a different template: `i64::MIN` five
    /// times is the widest rendering of the counts block, so the bound is
    /// arithmetic rather than a sample. (Counts cannot go negative — they are
    /// `COUNT(*)`/`SUM` — so this is the bound, not a reachable state.)
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

    /// Adjacent days share one integer, so the partition holds whatever the
    /// zone does.
    ///
    /// Asserted as a property of the function rather than against a fixed
    /// timestamp: this suite runs under whatever `TZ` the machine has, and a
    /// golden instant would encode the developer's zone.
    #[test]
    fn day_windows_tile_the_timeline_without_gaps_or_overlap() {
        let day = 86_400_000_i64;
        let noon = 1_700_000_000_000;
        let (start, end) = local_day_window(noon);
        assert!(start <= noon && noon < end, "{start} <= {noon} < {end}");
        // 23h — never enough to skip a day, and enough to land in the next one
        // whatever the offset, so the following day's window really is the
        // following day's.
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
