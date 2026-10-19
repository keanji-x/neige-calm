//! Response-only workspace projection. Stored tracks/workspaces retain their exact serde shape.
use crate::model::{Track, TrackWorkspace, WorkspaceIsolation};
use serde::{Serialize, Serializer};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Debug, Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackWorkspaceWire {
    #[serde(flatten)]
    pub workspace: TrackWorkspace,
    pub isolation: WorkspaceIsolation,
}

impl From<&TrackWorkspace> for TrackWorkspaceWire {
    fn from(workspace: &TrackWorkspace) -> Self {
        Self {
            workspace: workspace.clone(),
            isolation: workspace.isolation(),
        }
    }
}

/// Single projection point for HTTP track answers and track.updated event frames.
pub fn track_value(track: &Track) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = serde_json::to_value(track)?;
    value["workspace"] = serde_json::to_value(TrackWorkspaceWire::from(&track.workspace))?;
    Ok(value)
}

/// Event transport projection; persistence continues to serialize the domain event.
pub fn event_value(event: &crate::event::Event) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = serde_json::to_value(event)?;
    if let crate::event::Event::TrackUpdated(payload) = event {
        value["data"]["workspace"] = track_value(&payload.track)?["workspace"].clone();
    }
    Ok(value)
}

pub fn serialize_track<S: Serializer>(track: &Track, serializer: S) -> Result<S::Ok, S::Error> {
    track_value(track)
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

pub fn serialize_optional_track<S: Serializer>(
    track: &Option<Track>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    track
        .as_ref()
        .map(track_value)
        .transpose()
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

/// Transport wrapper; never used for database events or operation payloads.
#[derive(Clone, Debug, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[schema(value_type = TrackWireProjection)]
pub struct TrackWire(
    #[schema(inline)]
    #[ts(type = "Track & { workspace: TrackWorkspaceWire }")]
    pub Track,
);

/// Schema description of the transport intersection; domain Track stays persistence-shaped.
#[doc(hidden)]
#[derive(ToSchema)]
pub struct TrackWireProjection {
    #[serde(flatten)]
    pub track: Track,
    pub workspace: TrackWorkspaceWire,
}
impl Serialize for TrackWire {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_track(&self.0, serializer)
    }
}

/// The event transport replaces the Track payload while retaining every other event variant.
#[derive(TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct EventWire<'a>(
    #[ts(
        type = "Exclude<Event, { ev: 'track.updated' }> | (Extract<Event, { ev: 'track.updated' }> & { data: TrackWire })"
    )]
    pub &'a crate::event::Event,
);

impl Serialize for EventWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        event_value(self.0)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrackWorkspaceKind;

    #[test]
    fn isolation_none_wire_projection_does_not_change_stored_workspace() {
        let workspace = TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: "/borrowed".into(),
            frozen_at: Some(1),
            worktree: None,
        };
        let stored = serde_json::to_value(&workspace).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({"kind":"attached","path":"/borrowed","frozen_at":1})
        );
        let mut projected = serde_json::to_value(TrackWorkspaceWire::from(&workspace)).unwrap();
        assert_eq!(
            projected.as_object_mut().unwrap().remove("isolation"),
            Some(serde_json::json!("none"))
        );
        assert_eq!(projected, stored);
        projected["isolation"] = serde_json::json!("managed");
        let decoded: TrackWorkspace = serde_json::from_value(projected).unwrap();
        assert_eq!(decoded.isolation(), WorkspaceIsolation::None);
    }
}
