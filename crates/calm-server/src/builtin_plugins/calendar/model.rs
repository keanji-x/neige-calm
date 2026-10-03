use crate::error::{CalmError, Result};
use chrono::{
    DateTime, Datelike, Days, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone,
    Utc,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = CalendarSchedule)]
pub enum Schedule {
    AllDay {
        date: String,
    },
    Timed {
        start: String,
        end: String,
        timezone: String,
    },
    /// Local `start..end` ("HH:MM", same day) on each listed weekday from `from` through `until`
    /// (inclusive; open-ended when absent). A wall time that a DST change skips drops that
    /// occurrence; a repeated wall time takes its earlier instant.
    Weekly {
        weekdays: Vec<Weekday>,
        start: String,
        end: String,
        timezone: String,
        from: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
    },
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = CalendarWeekday)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CalendarDraft)]
pub struct Draft {
    pub title: String,
    pub description: String,
    pub schedule: Schedule,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CalendarCreate)]
pub struct Create {
    pub idempotency_key: String,
    pub task: Draft,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CalendarUpdate)]
pub struct Update {
    pub expected_version: i64,
    pub task: Draft,
    pub cancelled: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[schema(as = CalendarEntry)]
pub struct Entry {
    pub id: String,
    pub task: Draft,
    pub version: i64,
    pub cancelled: bool,
    pub source_track_id: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}
/// One timed occurrence as RFC3339 instants.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[schema(as = CalendarOccurrence)]
pub struct Occurrence {
    pub start: String,
    pub end: String,
}
/// A listed entry with its timed occurrences inside the requested window, in start order; an
/// all-day entry has none.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[schema(as = CalendarListedEntry)]
pub struct Listed {
    #[serde(flatten)]
    pub entry: Entry,
    pub occurrences: Vec<Occurrence>,
}
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CalendarWindow)]
pub struct Window {
    pub from: String,
    pub until: String,
    pub timezone: String,
}
pub fn invalid(message: impl Into<String>) -> CalmError {
    CalmError::BadRequest(message.into())
}
pub fn date(value: &str) -> Result<NaiveDate> {
    let parsed =
        NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| invalid("expected YYYY-MM-DD"))?;
    if parsed.format("%Y-%m-%d").to_string() != value {
        return Err(invalid("expected YYYY-MM-DD"));
    }
    Ok(parsed)
}
pub fn timezone(value: &str) -> Result<Tz> {
    value.parse().map_err(|_| invalid("unknown IANA timezone"))
}
fn instant(value: &str, tz: Tz) -> Result<DateTime<chrono::FixedOffset>> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| invalid("expected RFC3339 time with offset"))?;
    if parsed.with_timezone(&tz).offset().fix() != *parsed.offset() {
        return Err(invalid(
            "time offset does not match timezone; resolve ambiguous/nonexistent local times explicitly",
        ));
    }
    Ok(parsed)
}
/// Resolve user-supplied wall time; stored schedules always retain explicit offsets.
fn resolve_time(value: &str, tz: Tz) -> Result<String> {
    if DateTime::parse_from_rfc3339(value).is_ok() {
        instant(value, tz)?;
        return Ok(value.into());
    }
    let local = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .map_err(|_| invalid("expected local YYYY-MM-DDTHH:mm or RFC3339 time with offset"))?;
    if local.format("%Y-%m-%dT%H:%M").to_string() != value {
        return Err(invalid("expected local YYYY-MM-DDTHH:mm"));
    }
    match tz.from_local_datetime(&local) {
        LocalResult::Single(time) => Ok(time.fixed_offset().to_rfc3339()),
        LocalResult::Ambiguous(_, _) => Err(invalid(
            "local time is ambiguous; provide an explicit RFC3339 offset",
        )),
        LocalResult::None => Err(invalid(
            "local time does not exist in this timezone; choose another time",
        )),
    }
}
/// One timed occurrence: its instants and the zone it is written in.
pub struct TimedSpan {
    pub start: DateTime<chrono::FixedOffset>,
    pub end: DateTime<chrono::FixedOffset>,
    pub tz: Tz,
}
impl TimedSpan {
    pub fn occurrence(&self) -> Occurrence {
        Occurrence {
            start: self.start.to_rfc3339(),
            end: self.end.to_rfc3339(),
        }
    }
}
fn wall_clock(value: &str) -> Result<NaiveTime> {
    let parsed =
        NaiveTime::parse_from_str(value, "%H:%M").map_err(|_| invalid("expected HH:MM"))?;
    if parsed.format("%H:%M").to_string() != value {
        return Err(invalid("expected HH:MM"));
    }
    Ok(parsed)
}
/// A validated weekly schedule.
struct Weekly {
    weekdays: Vec<chrono::Weekday>,
    start: NaiveTime,
    end: NaiveTime,
    tz: Tz,
    from: NaiveDate,
    until: Option<NaiveDate>,
}
impl Weekly {
    /// The single weekly occurrence rule: occurrences that start on local dates `first..=last`.
    fn occurrences(&self, first: NaiveDate, last: NaiveDate) -> Vec<TimedSpan> {
        let last = self.until.map_or(last, |until| until.min(last));
        let at = |day: NaiveDate, time: NaiveTime| {
            // `earliest` is None in a DST gap and the earlier instant in an overlap.
            self.tz
                .from_local_datetime(&day.and_time(time))
                .earliest()
                .map(|instant| instant.fixed_offset())
        };
        first
            .max(self.from)
            .iter_days()
            .take_while(|day| *day <= last)
            .filter(|day| self.weekdays.contains(&day.weekday()))
            .filter_map(|day| {
                Some(TimedSpan {
                    start: at(day, self.start)?,
                    end: at(day, self.end)?,
                    tz: self.tz,
                })
            })
            .collect()
    }
}
fn timed(start: &str, end: &str, zone: &str) -> Result<TimedSpan> {
    let tz = timezone(zone)?;
    Ok(TimedSpan {
        start: instant(start, tz)?,
        end: instant(end, tz)?,
        tz,
    })
}
impl Schedule {
    fn weekly(&self) -> Result<Option<Weekly>> {
        let Schedule::Weekly {
            weekdays,
            start,
            end,
            timezone: zone,
            from,
            until,
        } = self
        else {
            return Ok(None);
        };
        let weekly = Weekly {
            weekdays: weekdays
                .iter()
                .map(|day| match day {
                    Weekday::Mon => chrono::Weekday::Mon,
                    Weekday::Tue => chrono::Weekday::Tue,
                    Weekday::Wed => chrono::Weekday::Wed,
                    Weekday::Thu => chrono::Weekday::Thu,
                    Weekday::Fri => chrono::Weekday::Fri,
                    Weekday::Sat => chrono::Weekday::Sat,
                    Weekday::Sun => chrono::Weekday::Sun,
                })
                .collect(),
            start: wall_clock(start)?,
            end: wall_clock(end)?,
            tz: timezone(zone)?,
            from: date(from)?,
            until: until.as_deref().map(date).transpose()?,
        };
        let unique = weekly
            .weekdays
            .iter()
            .enumerate()
            .all(|(index, day)| !weekly.weekdays[..index].contains(day));
        if weekly.weekdays.is_empty() || !unique {
            return Err(invalid("weekdays must be nonempty and unique"));
        }
        if weekly.end <= weekly.start {
            return Err(invalid("end must be later than start on the same day"));
        }
        if weekly.until.is_some_and(|until| until < weekly.from) {
            return Err(invalid("until must not be before from"));
        }
        Ok(Some(weekly))
    }
    /// The latest occurrence that has started by `now`; `None` for an all-day schedule.
    pub fn latest_occurrence(&self, now: DateTime<Utc>) -> Result<Option<TimedSpan>> {
        let span = match self {
            Schedule::AllDay { .. } => None,
            Schedule::Timed {
                start,
                end,
                timezone: zone,
            } => Some(timed(start, end, zone)?),
            Schedule::Weekly { .. } => self.weekly()?.and_then(|weekly| {
                let today = now.with_timezone(&weekly.tz).date_naive();
                // Eight local days hold every listed weekday's latest start.
                weekly
                    .occurrences(today.checked_sub_days(Days::new(7))?, today)
                    .into_iter()
                    .rfind(|span| span.start <= now)
            }),
        };
        Ok(span.filter(|span| span.start <= now))
    }
}
impl Draft {
    pub fn resolve_times(&mut self) -> Result<()> {
        if let Schedule::Timed {
            start,
            end,
            timezone: zone,
        } = &mut self.schedule
        {
            let tz = timezone(zone)?;
            let resolved_start = resolve_time(start, tz)?;
            let resolved_end = resolve_time(end, tz)?;
            *start = resolved_start;
            *end = resolved_end;
        }
        self.validate()
    }
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() || self.title.len() > 500 || self.description.len() > 20000
        {
            return Err(invalid(
                "title must be nonempty and at most 500 bytes; description at most 20000 bytes",
            ));
        }
        match &self.schedule {
            Schedule::AllDay { date: value } => {
                date(value)?;
            }
            Schedule::Timed {
                start,
                end,
                timezone: zone,
            } => {
                let tz = timezone(zone)?;
                if instant(end, tz)? <= instant(start, tz)? {
                    return Err(invalid("end must be later than start"));
                }
            }
            Schedule::Weekly { .. } => {
                self.schedule.weekly()?;
            }
        }
        Ok(())
    }
}
impl Window {
    pub fn validate(&self) -> Result<()> {
        let span = date(&self.until)?
            .signed_duration_since(date(&self.from)?)
            .num_days();
        timezone(&self.timezone)?;
        if !(1..=366).contains(&span) {
            return Err(invalid("window must span 1 to 366 days"));
        }
        Ok(())
    }
    pub fn contains(&self, schedule: &Schedule) -> Result<bool> {
        match schedule {
            Schedule::AllDay { date: value } => {
                Ok(date(value)? >= date(&self.from)? && date(value)? < date(&self.until)?)
            }
            _ => Ok(!self.occurrences(schedule)?.is_empty()),
        }
    }
    /// Timed occurrences that overlap the window's local days, in start order; none when all-day.
    pub fn occurrences(&self, schedule: &Schedule) -> Result<Vec<TimedSpan>> {
        let from = date(&self.from)?;
        let until = date(&self.until)?;
        let tz = timezone(&self.timezone)?;
        let candidates = match schedule {
            Schedule::AllDay { .. } => Vec::new(),
            Schedule::Timed {
                start,
                end,
                timezone: zone,
            } => vec![timed(start, end, zone)?],
            // Zone offsets differ by at most 26 hours, so two extra local days cover each side.
            Schedule::Weekly { .. } => schedule.weekly()?.map_or_else(Vec::new, |weekly| {
                let margin = Days::new(2);
                weekly.occurrences(
                    from.checked_sub_days(margin).unwrap_or(NaiveDate::MIN),
                    until.checked_add_days(margin).unwrap_or(NaiveDate::MAX),
                )
            }),
        };
        Ok(candidates
            .into_iter()
            .filter(|span| {
                // Half-open range: an event ending at midnight is absent from the next day.
                let last = span.end - chrono::Duration::nanoseconds(1);
                span.start.with_timezone(&tz).date_naive() < until
                    && last.with_timezone(&tz).date_naive() >= from
            })
            .collect())
    }
}
