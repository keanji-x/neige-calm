use crate::error::{CalmError, Result};
use chrono::{DateTime, NaiveDate, Offset};
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
impl Draft {
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
        let from = date(&self.from)?;
        let until = date(&self.until)?;
        match schedule {
            Schedule::AllDay { date: value } => Ok(date(value)? >= from && date(value)? < until),
            Schedule::Timed {
                start,
                end,
                timezone: zone,
            } => {
                let tz = timezone(&self.timezone)?;
                let start = instant(start, timezone(zone)?)?.with_timezone(&tz);
                // Half-open range: an event ending at midnight is absent from the next day.
                let last = instant(end, timezone(zone)?)? - chrono::Duration::nanoseconds(1);
                Ok(start.date_naive() < until && last.with_timezone(&tz).date_naive() >= from)
            }
        }
    }
}
