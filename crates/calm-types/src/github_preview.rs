//! REST vocabulary for read-only GitHub Issue and PR summaries; no credentials or IO.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum GitHubPreviewKind {
    Issue,
    Pull,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum GitHubPreviewState {
    Open,
    Closed,
    Merged,
    Draft,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct GitHubPullChanges {
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct GitHubPreview {
    pub kind: GitHubPreviewKind,
    pub number: u64,
    pub title: String,
    pub state: GitHubPreviewState,
    pub author: String,
    pub labels: Vec<String>,
    pub excerpt: String,
    #[schema(required = true)]
    pub changes: Option<GitHubPullChanges>,
}
