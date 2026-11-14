//! Owner publication fences shared by finite Report and conversation reads.
use crate::{auth::Principal, db::RouteRepo, error::Result, ids::AreaId};

pub(crate) struct ReadFence {
    pub area: AreaId,
}

impl ReadFence {
    pub async fn check(repo: &dyn RouteRepo, track: &str, principal: &Principal) -> Result<Self> {
        Ok(Self {
            area: crate::track_access::require_detail_access(repo, track, principal).await?,
        })
    }

    pub async fn publish(
        &self,
        repo: &dyn RouteRepo,
        track: &str,
        principal: &Principal,
    ) -> Result<()> {
        #[cfg(feature = "fixtures")]
        crate::track_access::after_assembly(track).await?;
        crate::track_access::require_detail_publication(repo, track, &self.area, principal).await
    }
}
