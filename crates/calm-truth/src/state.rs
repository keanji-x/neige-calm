use crate::card_role_cache::CardRoleCache;
use crate::model::CardRole;
use crate::wave_area_cache::WaveAreaCache;
use calm_types::ids::{AreaId, CardId, WaveId};

/// Write-surface cache slice used by the truth write entrance.
#[derive(Clone)]
pub struct WriteContext {
    role_cache: CardRoleCache,
    area_cache: WaveAreaCache,
}

impl WriteContext {
    pub fn new(role_cache: CardRoleCache, area_cache: WaveAreaCache) -> Self {
        Self {
            role_cache,
            area_cache,
        }
    }

    pub fn verify_role(&self, card_id: &CardId) -> Option<CardRole> {
        self.role_cache.get(card_id)
    }

    pub fn verify_area(&self, wave_id: &WaveId) -> Option<AreaId> {
        self.area_cache.area_of(wave_id)
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
    pub fn area_cache(&self) -> &WaveAreaCache {
        &self.area_cache
    }
}
