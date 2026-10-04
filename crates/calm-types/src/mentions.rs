//! Wire vocabulary for `GET /api/areas/{area_id}/mentions` (#1881): the chat `@` candidates of one
//! area, in three groups. Every item carries `insert`, the exact text a chosen candidate puts in the
//! message; the kernel builds it, so a client never assembles a report path itself.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

use crate::ids::TrackId;

/// The candidates for one query, best first within each group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct MentionCandidates {
    pub tags: Vec<TagMention>,
    pub tracks: Vec<TrackMention>,
    pub blocks: Vec<BlockMention>,
}

/// A report tag of the area; `insert` is ``@`tag:<tag>` ``.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TagMention {
    /// The tag.
    pub label: String,
    /// How many of the area's reports carry the tag.
    pub track_count: u32,
    pub insert: String,
}

/// A report of the area; `insert` is ``@`area/reports/<name>.md` ``, the path `neige track cat` reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackMention {
    /// The track title.
    pub label: String,
    pub track_id: TrackId,
    pub insert: String,
}

/// One block of a report of the area; `insert` is ``@`area/reports/<name>.md#<block_id>` ``.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct BlockMention {
    /// The block's heading as `neige_area_outline` lists it; a block with no heading is not offered.
    pub label: String,
    pub block_id: String,
    /// The title of the track whose report holds the block.
    pub track_title: String,
    pub track_id: TrackId,
    pub insert: String,
}
