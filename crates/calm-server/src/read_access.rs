//! Audience and publication fences shared by finite Report and conversation reads.
use crate::{
    auth::{AuthConfig, Principal},
    db::RouteRepo,
    error::Result,
    ids::AreaId,
};

#[derive(Clone, Copy)]
pub(crate) enum Audience<'a> {
    Owner(&'a Principal),
    ShareLink {
        token: &'a str,
        config: &'a AuthConfig,
    },
}

pub(crate) enum ReadFence {
    Owner(AreaId),
    ShareLink(crate::share_access::ShareRead),
}

impl Audience<'_> {
    pub async fn check(self, repo: &dyn RouteRepo, track: &str) -> Result<ReadFence> {
        Ok(match self {
            Self::Owner(principal) => ReadFence::Owner(
                crate::track_access::require_detail_access(repo, track, principal).await?,
            ),
            Self::ShareLink { token, config } => ReadFence::ShareLink(
                crate::share_access::resolve(repo, config, token, Some(track)).await?,
            ),
        })
    }

    pub async fn publish(self, repo: &dyn RouteRepo, track: &str, fence: &ReadFence) -> Result<()> {
        #[cfg(feature = "fixtures")]
        crate::track_access::after_assembly(track).await?;
        match (self, fence) {
            (Self::Owner(principal), ReadFence::Owner(area)) => {
                crate::track_access::require_detail_publication(repo, track, area, principal).await
            }
            (Self::ShareLink { token, config }, ReadFence::ShareLink(expected)) => {
                crate::share_access::require_publication(repo, config, token, Some(track), expected)
                    .await
            }
            _ => unreachable!("read fence belongs to its audience"),
        }
    }
}
