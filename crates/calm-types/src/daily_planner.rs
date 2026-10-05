//! Daily Track resolution and report-history read contracts.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReportChangesPage {
    pub date: String,
    pub timezone: String,
    pub through_event_id: i64,
    pub changes: Vec<ReportChange>,
    #[schema(required = true, nullable = true)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReportChange {
    pub track_id: String,
    pub track_title: String,
    pub area_id: String,
    pub area_name: String,
    pub edit_count: i64,
    pub first_event_id: i64,
    pub last_event_id: i64,
    pub summary_before: String,
    pub summary_after: String,
    pub patch: String,
    pub patch_truncated: bool,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReportEdit {
    pub track_id: String,
    pub edit_id: String,
    pub summary_before: String,
    pub summary_after: String,
    pub body_before: String,
    pub body_after: String,
}

#[derive(Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReportEditEntry {
    pub event_id: i64,
    pub at: i64,
    pub edit: ReportEdit,
}

#[derive(Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReportEditsPage {
    pub edits: Vec<ReportEditEntry>,
    // Opaque: the last event id in decimal, passed back as `cursor`.
    #[schema(required = true, nullable = true)]
    pub next_cursor: Option<String>,
}

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct DailyTrackResolved {
    pub date: String,
    pub time_zone: String,
    pub track_id: String,
}
