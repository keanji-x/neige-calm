use crate::card_role_cache::CardRoleCache;
use crate::model::CardRole;
use crate::track_area_cache::TrackAreaCache;
use calm_types::ids::{AreaId, CardId, TrackId};

/// Write-surface cache slice used by the truth write entrance.
#[derive(Clone)]
pub struct WriteContext {
    role_cache: CardRoleCache,
    area_cache: TrackAreaCache,
}

impl WriteContext {
    pub fn new(role_cache: CardRoleCache, area_cache: TrackAreaCache) -> Self {
        Self {
            role_cache,
            area_cache,
        }
    }

    pub fn verify_role(&self, card_id: &CardId) -> Option<CardRole> {
        self.role_cache.get(card_id)
    }

    pub fn verify_area(&self, track_id: &TrackId) -> Option<AreaId> {
        self.area_cache.area_of(track_id)
    }

    /// Restore a cache binding after a surrounding database transaction rolled
    /// back a track deletion that had already updated the write-through cache.
    pub fn remember_track(&self, track_id: TrackId, area_id: AreaId) {
        self.area_cache.insert(track_id, area_id);
    }

    /// Remove a committed track deletion from the authorization cache.
    pub fn forget_track(&self, track_id: &TrackId) {
        self.area_cache.remove(track_id);
    }

    #[deprecated(
        since = "0.1.0",
        note = "use WriteContext::verify_role / verify_area; raw getters survive only for legacy db chain glue"
    )]
    pub fn role_cache(&self) -> &CardRoleCache {
        &self.role_cache
    }

    #[deprecated(
        since = "0.1.0",
        note = "use WriteContext::verify_role / verify_area; raw getters survive only for legacy db chain glue"
    )]
    pub fn area_cache(&self) -> &TrackAreaCache {
        &self.area_cache
    }
}
