//! Server-owned additions to the bound TrackFsView; no global navigation policy.
use super::{op, track_file};
use crate::mcp_server::{ToolCallIdentity, framing::RpcError, registry::AppContext};
use crate::model::Track;
use crate::track_fs_view::{TrackFsNodeKind, normalize_path};
use std::sync::Arc;

pub(super) enum ServerPath<'a> {
    GuideDirectory,
    Guide(&'a str),
    Operations,
    Operation { id: &'a str, result: bool },
    Truth,
}

impl<'a> ServerPath<'a> {
    pub(super) fn classify(path: &'a str) -> Self {
        if path == track_file::GUIDE_DIR {
            Self::GuideDirectory
        } else if let Some(name) = path.strip_prefix("guide/") {
            Self::Guide(name)
        } else if path == "ops" {
            Self::Operations
        } else if let Some(rest) = path.strip_prefix("ops/") {
            match rest.strip_suffix("/result") {
                Some(id) => Self::Operation { id, result: true },
                None => Self::Operation {
                    id: rest,
                    result: false,
                },
            }
        } else {
            Self::Truth
        }
    }
}

/// Locate an own-view node after the caller has established the live bound track.
/// This does not authorize foreign tracks, read content, or add directory listings.
/// cat must still execute its final content checks, even after successful location.
pub async fn own_node_kind(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    track: &Track,
    path: &str,
) -> Result<TrackFsNodeKind, RpcError> {
    use TrackFsNodeKind::{Directory, File};
    if identity.track_id.as_deref() != Some(track.id.as_str()) {
        return Err(RpcError::forbidden(
            "node is outside the caller's bound track",
        ));
    }
    let path = normalize_path(path);
    match ServerPath::classify(&path) {
        ServerPath::GuideDirectory | ServerPath::Operations => Ok(Directory),
        ServerPath::Guide(name) => {
            track_file::guide_text(name)?;
            Ok(File)
        }
        ServerPath::Operation { id, result } => {
            op::lookup(ctx, identity, id).await?;
            Ok(if result { File } else { Directory })
        }
        ServerPath::Truth => track_file::own_view(ctx, identity)
            .node_kind(track, &path)
            .await
            .map_err(track_file::track_fs_error_to_rpc),
    }
}
