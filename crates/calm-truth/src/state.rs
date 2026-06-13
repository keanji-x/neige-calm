use crate::card_role_cache::CardRoleCache;
use crate::model::CardRole;
use crate::wave_cove_cache::WaveCoveCache;
use calm_types::ids::{CardId, CoveId, WaveId};

/// Write-surface cache slice used by the truth write entrance.
#[derive(Clone)]
pub struct WriteContext {
    role_cache: CardRoleCache,
    cove_cache: WaveCoveCache,
}

impl WriteContext {
    pub fn new(role_cache: CardRoleCache, cove_cache: WaveCoveCache) -> Self {
        Self {
            role_cache,
            cove_cache,
        }
    }

    pub fn verify_role(&self, card_id: &CardId) -> Option<CardRole> {
        self.role_cache.get(card_id)
    }

    pub fn verify_cove(&self, wave_id: &WaveId) -> Option<CoveId> {
        self.cove_cache.cove_of(wave_id)
    }

    #[deprecated(
        since = "0.1.0",
        note = "use WriteContext::verify_role / verify_cove; raw getters survive only for legacy db chain glue"
    )]
    pub fn role_cache(&self) -> &CardRoleCache {
        &self.role_cache
    }

    #[deprecated(
        since = "0.1.0",
        note = "use WriteContext::verify_role / verify_cove; raw getters survive only for legacy db chain glue"
    )]
    pub fn cove_cache(&self) -> &WaveCoveCache {
        &self.cove_cache
    }
}
