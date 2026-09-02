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
//! It has exactly one caller: `routes::today_summary`.
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
//! The join below is on `events.scope_wave`, so every row written before
//! migration 0007 is invisible to it, whatever its kind. A window that spans
//! the upgrade point reports less activity than actually happened, with no
//! error and no warning. Same applies forward: an emitter that logs one of the
//! allowlisted kinds at `EventScope::System` writes a row this projection
//! cannot see. Every current emitter of the four kinds uses a wave or card
//! scope (`routes::waves::update_wave` and `wave_report::persist_report` use
//! `EventScope::Wave`; `decision_sink::commit_worker_task_report` uses
//! `EventScope::Card`, which also populates `scope_wave`) — that is a fact
//! about today's code, not a constraint the schema enforces.
//!
//! # Counts only — this is what bounds the prompt
//!
//! Every field of [`WorkspaceActivityWindow`] is an integer. No wave titles, no
//! cove names, no detail lists, and nothing else of variable length. That is
//! not tidiness: the prompt this feeds goes through `POST /api/cards/{id}/spec/input`,
//! which rejects anything over `MAX_SPEC_INPUT_CHARS` (32,768) — so the
//! rendered prompt's length has to have a bound that can be computed without
//! running it. Template text plus five integers has one. A "just for context"
//! list of wave titles does not, and re-introducing one means also
//! re-introducing a deterministic character budget and its 32,768 / 32,769 /
//! CJK boundary cases (design D4).

use sqlx::{Pool, Sqlite};

use crate::error::Result;

/// The event kinds that count as workspace activity, adjudicated in design D4.
///
/// Both permanence requirements are met by construction: all four are absent
/// from `EVENTS_PRUNE_KINDS` (`calm-truth/src/events_prune.rs`), whose
/// allowlist is exact-kind and fails safe, so these rows outlive every
/// retention pass.
///
/// **`turns` is deliberately absent.** Its only fact source,
/// `harness.item.added`, *is* in the 30-day prune allowlist, so a turn count
/// would silently decay to zero past the horizon; there is no permanent
/// substitute (design D4).
pub const ACTIVITY_KINDS: [&str; 4] = [
    "wave.lifecycle_changed",
    "wave.report_edited",
    "task.completed",
    "task.failed",
];

/// The FROM/WHERE both aggregates share, written once.
///
/// Shared rather than repeated because the two queries must select over exactly
/// the same rows: a predicate that drifted between them would report counts and
/// a wave total taken from different populations, and nothing about the result
/// would look wrong.
///
/// * `?1`/`?2` — the half-open window, `[start, end)`. See INV-TODAYDOC-006:
///   an event at the boundary belongs to the later day and to that day only.
/// * `?3` — the wave to exclude, or NULL.
/// * `?4`..`?7` — [`ACTIVITY_KINDS`], bound positionally.
///
/// The join runs through `waves` and `coves` rather than trusting
/// `events.scope_cove`: the scope columns are a snapshot taken at write time,
/// while `waves.cove_id` is current, and a wave whose row is gone should not
/// contribute activity to a page that cannot link to it.
///
/// `coves.kind = 'user'` is the visibility filter, and it is the same predicate
/// `coves_list_user_visible` uses for `GET /api/coves` (#175). It is what keeps
/// the system cove — and therefore the launchpad — out of the count.
const ACTIVITY_FROM_WHERE: &str = r#"
      FROM events e
      JOIN waves w ON w.id = e.scope_wave
      JOIN coves c ON c.id = w.cove_id
     WHERE e.at >= ?1
       AND e.at <  ?2
       AND c.kind = 'user'
       AND (?3 IS NULL OR w.id <> ?3)
       AND e.kind IN (?4, ?5, ?6, ?7)
"#;

/// One window's worth of activity. Integers only — see the module docs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceActivityWindow {
    pub wave_lifecycle_changed: i64,
    pub wave_report_edited: i64,
    pub task_completed: i64,
    pub task_failed: i64,
    /// How many distinct waves contributed any of the counted events.
    ///
    /// Distinct waves, not a sum: it answers "how broad was the day", which the
    /// four counts cannot, and it is still one integer.
    pub waves_touched: i64,
}

impl WorkspaceActivityWindow {
    /// Total counted events. `waves_touched` is not added in — it is a
    /// dimension of the same rows, not more of them.
    pub fn total_events(&self) -> i64 {
        self.wave_lifecycle_changed + self.wave_report_edited + self.task_completed + self.task_failed
    }

    /// Nothing counted. The gate INV-TODAYDOC-007 is written against.
    pub fn is_empty(&self) -> bool {
        self.total_events() == 0
    }
}

/// Aggregate workspace activity over the half-open window `[start_ms, end_ms)`.
///
/// `exclude_wave` is the **reflexive exclusion**, and it is defence in depth
/// rather than a load-bearing wall — stating that precisely is part of the
/// design's ruling (D4). Writing the summary is itself a `wave.report_edited`
/// on the launchpad wave, so without an exclusion each run would feed the next
/// one its own footprint. But the launchpad lives in the system cove and
/// `c.kind = 'user'` above has already dropped it, so deleting this predicate
/// turns no test red *through the join*. What pins it is
/// `reflexive_exclusion_drops_the_named_wave_before_the_visibility_join` in
/// this module, which drives the predicate directly on raw allowlisted rows in
/// a user cove — i.e. before the visibility filter can mask it. The predicate
/// becomes load-bearing the day the visibility join widens or the launchpad
/// moves out of the system cove.
///
/// The exclusion is by **wave**, so a human hand-editing Today's report does
/// not register as activity either. That is intended (design D4).
pub async fn workspace_activity_window(
    pool: &Pool<Sqlite>,
    start_ms: i64,
    end_ms: i64,
    exclude_wave: Option<&str>,
) -> Result<WorkspaceActivityWindow> {
    let [lifecycle, report, completed, failed] = ACTIVITY_KINDS;

    let counted: Vec<(String, i64)> = sqlx::query_as(&format!(
        "SELECT e.kind AS kind, COUNT(*) AS n {ACTIVITY_FROM_WHERE} GROUP BY e.kind"
    ))
    .bind(start_ms)
    .bind(end_ms)
    .bind(exclude_wave)
    .bind(lifecycle)
    .bind(report)
    .bind(completed)
    .bind(failed)
    .fetch_all(pool)
    .await?;

    let waves_touched: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(DISTINCT w.id) {ACTIVITY_FROM_WHERE}"
    ))
    .bind(start_ms)
    .bind(end_ms)
    .bind(exclude_wave)
    .bind(lifecycle)
    .bind(report)
    .bind(completed)
    .bind(failed)
    .fetch_one(pool)
    .await?;

    let mut window = WorkspaceActivityWindow {
        waves_touched,
        ..Default::default()
    };
    for (kind, n) in counted {
        // Matched against the same constants the query bound, so a renamed kind
        // cannot land in a field it does not belong to: it simply cannot come
        // back from a query that never asked for it.
        match kind.as_str() {
            k if k == lifecycle => window.wave_lifecycle_changed = n,
            k if k == report => window.wave_report_edited = n,
            k if k == completed => window.task_completed = n,
            k if k == failed => window.task_failed = n,
            other => {
                return Err(crate::error::CalmError::Internal(format!(
                    "activity window returned unrequested kind `{other}`"
                )));
            }
        }
    }
    Ok(window)
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
    (local_start_of_day_ms(today), local_start_of_day_ms(tomorrow))
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
        use crate::model::WaveLifecycle;

        let lifecycle = Event::WaveLifecycleChanged {
            id: crate::ids::WaveId::from("w".to_string()),
            cove_id: crate::ids::CoveId::from("c".to_string()),
            from: WaveLifecycle::Draft,
            to: WaveLifecycle::Planning,
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
                "wave.report_edited",
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

        async fn cove(&self, id: &str, kind: &str) {
            sqlx::query(
                "INSERT INTO coves(id,name,color,sort,kind,created_at,updated_at) \
                 VALUES(?1,?1,'#abc',1,?2,1,1)",
            )
            .bind(id)
            .bind(kind)
            .execute(self.repo.pool())
            .await
            .unwrap();
        }

        async fn wave(&self, id: &str, cove_id: &str) {
            sqlx::query(
                "INSERT INTO waves(id,cove_id,title,sort,lifecycle,created_at,updated_at) \
                 VALUES(?1,?2,?1,1,'draft',1,1)",
            )
            .bind(id)
            .bind(cove_id)
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
        /// (an end-to-end drive of `POST /api/waves/{id}/report`) for the row
        /// shape.
        async fn event(&self, kind: &str, wave_id: &str, at: i64) {
            sqlx::query(
                "INSERT INTO events(kind,payload,actor,at,scope_kind,scope_wave) \
                 VALUES(?1,'{}','user',?2,'wave',?3)",
            )
            .bind(kind)
            .bind(at)
            .bind(wave_id)
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
        f.cove("cove-user", "user").await;
        f.wave("wave-1", "cove-user").await;

        let midnight = 1_700_000_000_000;
        let day = 86_400_000;
        f.event("wave.report_edited", "wave-1", midnight).await;

        let today = f.window(midnight, midnight + day).await;
        assert_eq!(today.wave_report_edited, 1, "{today:?}");
        assert_eq!(today.waves_touched, 1, "{today:?}");

        let yesterday = f.window(midnight - day, midnight).await;
        assert_eq!(
            yesterday.wave_report_edited, 0,
            "an event at the boundary must not also count for the day that \
             ends there: {yesterday:?}"
        );
        assert_eq!(yesterday.waves_touched, 0, "{yesterday:?}");

        // The instant before the boundary belongs to the earlier day, which is
        // what makes the two windows a partition rather than a gap.
        f.event("wave.report_edited", "wave-1", midnight - 1).await;
        assert_eq!(f.window(midnight - day, midnight).await.wave_report_edited, 1);
        assert_eq!(f.window(midnight, midnight + day).await.wave_report_edited, 1);
    }

    /// Each allowlisted kind lands in its own field, and a kind outside the
    /// allowlist contributes nothing.
    ///
    /// The negative is the load-bearing half. Without it, a query that dropped
    /// the `kind IN (...)` conjunct entirely would still satisfy every positive
    /// assertion here, and the projection would start counting every event in
    /// the log — including the high-frequency `harness.item.added`, whose rows
    /// are pruned after 30 days, which is exactly the decay the allowlist
    /// exists to prevent.
    #[tokio::test]
    async fn each_kind_lands_in_its_own_field_and_unlisted_kinds_are_ignored() {
        let f = Fixture::new().await;
        f.cove("cove-user", "user").await;
        f.wave("wave-1", "cove-user").await;
        f.wave("wave-2", "cove-user").await;

        f.event("wave.lifecycle_changed", "wave-1", 10).await;
        f.event("wave.report_edited", "wave-1", 11).await;
        f.event("wave.report_edited", "wave-2", 12).await;
        f.event("task.completed", "wave-2", 13).await;
        f.event("task.failed", "wave-2", 14).await;
        f.event("harness.item.added", "wave-1", 15).await;
        f.event("card.updated", "wave-2", 16).await;

        let window = f.window(0, 100).await;
        assert_eq!(
            window,
            WorkspaceActivityWindow {
                wave_lifecycle_changed: 1,
                wave_report_edited: 2,
                task_completed: 1,
                task_failed: 1,
                waves_touched: 2,
            }
        );
        assert!(!window.is_empty());
        assert_eq!(window.total_events(), 5);
    }

    /// The system cove is not activity, which is what INV-TODAYDOC-007's
    /// "empty window" is able to mean at all.
    ///
    /// The launchpad wave lives there, and so does everything the kernel does
    /// on its own behalf. If these rows counted, no workspace would ever have
    /// an empty day and the gate would be unreachable.
    #[tokio::test]
    async fn only_user_visible_coves_count_as_activity() {
        let f = Fixture::new().await;
        f.cove("cove-system", "system").await;
        f.wave("wave-launchpad", "cove-system").await;
        f.event("wave.report_edited", "wave-launchpad", 10).await;

        let window = f.window(0, 100).await;
        assert!(
            window.is_empty(),
            "a system-cove wave must not make the day look busy: {window:?}"
        );
    }

    /// The reflexive exclusion, driven where it can actually be observed.
    ///
    /// The wave here sits in a **user** cove, so the visibility join lets it
    /// through and the exclusion predicate is the only thing that can drop it.
    /// In production it is not: the launchpad is in the system cove and the
    /// case above already removes it, which is why the design calls this
    /// defence in depth and not a wall. Pinning it here rather than through the
    /// endpoint is the design's own instruction — the alternative was writing
    /// an invariant that cannot be falsified.
    #[tokio::test]
    async fn reflexive_exclusion_drops_the_named_wave_before_the_visibility_join() {
        let f = Fixture::new().await;
        f.cove("cove-user", "user").await;
        f.wave("wave-self", "cove-user").await;
        f.wave("wave-other", "cove-user").await;
        f.event("wave.report_edited", "wave-self", 10).await;
        f.event("wave.report_edited", "wave-other", 11).await;

        let excluded = workspace_activity_window(f.repo.pool(), 0, 100, Some("wave-self"))
            .await
            .unwrap();
        assert_eq!(excluded.wave_report_edited, 1, "{excluded:?}");
        assert_eq!(excluded.waves_touched, 1, "{excluded:?}");

        let kept = f.window(0, 100).await;
        assert_eq!(
            kept.wave_report_edited, 2,
            "without the exclusion both rows count — otherwise the case above \
             proves nothing: {kept:?}"
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
