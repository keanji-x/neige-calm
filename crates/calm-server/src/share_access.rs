//! Owning share management transactions. HTTP consumes these decisions, never
//! interprets Actor/role headers or treats stored consent as runtime admission.
mod execution;
pub(crate) mod scope;
pub(crate) mod sessions;
use crate::auth::HumanInputSource;
use crate::db::{RouteRepo, sqlite, write_in_tx_typed};
use crate::error::{CalmError, Result};
use calm_types::share_links::{
    ShareDelegation, ShareLink, ShareLinkCreated, SharePurpose, ShareResourcePolicy, ShareScope,
};
pub(crate) use execution::authorize_accepted_tx;
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};

async fn authorize(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    scope: &ShareScope,
) -> Result<String> {
    // Qualify the real source before loading any protected target.
    crate::organization_access::require_deployment_source(source)?;
    let area_id = match scope {
        ShareScope::Track(id) => sqlite::track_get_tx(tx, &id.clone().into())
            .await?
            .area_id
            .to_string(),
        ShareScope::Area(id) => id.clone(),
    };
    let principal =
        crate::organization_access::require_share_owner_tx(tx, &area_id, source).await?;
    Ok(principal.user_id)
}

async fn validate(
    tx: &mut Transaction<'_, Sqlite>,
    source: &HumanInputSource,
    scope: &ShareScope,
    link: &ShareLink,
) -> Result<()> {
    let owner = authorize(tx, source, scope).await?;
    if link.delegation.scope != *scope
        || link.delegation.approver_user_id != owner
        || link.delegation.purpose != SharePurpose::Conversation
        || link.delegation.resource_policy != ShareResourcePolicy::CurrentDeployment
    {
        return Err(CalmError::Forbidden(
            "share delegation does not match current authority".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn create(
    repo: &dyn RouteRepo,
    source: HumanInputSource,
    scope: ShareScope,
) -> Result<ShareLinkCreated> {
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let owner = authorize(tx, &source, &scope).await?;
            let mut bytes = [0u8; 32];
            OsRng.fill_bytes(&mut bytes);
            let token = hex::encode(bytes);
            let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
            let delegation = ShareDelegation {
                id: crate::model::new_id(),
                scope: scope.clone(),
                approver_user_id: owner,
                purpose: SharePurpose::Conversation,
                resource_policy: ShareResourcePolicy::CurrentDeployment,
                created_at: crate::model::now_ms(),
            };
            let link = sqlite::share_links::create_tx(tx, &delegation, &token_hash).await?;
            validate(tx, &source, &scope, &link).await?;
            crate::organization_access::require_deployment_source(&source)?;
            Ok(ShareLinkCreated { link, token })
        })
    })
    .await
}

pub(crate) async fn list(
    repo: &dyn RouteRepo,
    source: HumanInputSource,
    scope: ShareScope,
) -> Result<Vec<ShareLink>> {
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            authorize(tx, &source, &scope).await?;
            let links = sqlite::share_links::list_tx(tx, &scope).await?;
            for link in &links {
                validate(tx, &source, &scope, link).await?;
            }
            crate::organization_access::require_deployment_source(&source)?;
            Ok(links)
        })
    })
    .await
}

pub(crate) async fn revoke(
    repo: &dyn RouteRepo,
    source: HumanInputSource,
    id: String,
) -> Result<ShareLink> {
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            crate::organization_access::require_deployment_source(&source)?;
            let stored = sqlite::share_links::get_tx(tx, &id).await?;
            validate(tx, &source, &stored.delegation.scope, &stored).await?;
            let link = sqlite::share_links::revoke_tx(tx, &id, crate::model::now_ms()).await?;
            validate(tx, &source, &stored.delegation.scope, &link).await?;
            crate::organization_access::require_deployment_source(&source)?;
            Ok(link)
        })
    })
    .await
}

/// A finite read fence. The parent is retained so moving a Track cannot publish
/// bytes assembled under a different Area, even when both Areas have links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShareRead {
    pub link: ShareLink,
    pub area: crate::ids::AreaId,
}

pub(crate) async fn resolve_hash(
    repo: &dyn RouteRepo,
    config: &crate::auth::AuthConfig,
    hash: &str,
    track_id: Option<&str>,
) -> Result<ShareRead> {
    let owner = crate::auth::current_deployment_owner(config)?;
    let facts = repo.share_link_read_facts(hash, &owner, track_id).await?;
    let link = facts.link;
    if link.delegation.approver_user_id != owner
        || link.delegation.purpose != SharePurpose::Conversation
        || link.delegation.resource_policy != ShareResourcePolicy::CurrentDeployment
    {
        return Err(CalmError::Forbidden(
            "share approval is no longer current".into(),
        ));
    }
    if let Some(track_id) = track_id {
        let requested = facts
            .requested_track
            .as_ref()
            .ok_or_else(|| CalmError::NotFound("shared Track".into()))?;
        if !contains_track(
            &link.delegation.scope,
            track_id,
            requested.area_id.as_str(),
            requested.purpose.as_deref(),
        ) {
            return Err(CalmError::NotFound("shared Track".into()));
        }
    }
    let area = facts.area;
    crate::auth::current_deployment_owner(config)?;
    Ok(ShareRead { link, area })
}

fn contains_track(scope: &ShareScope, track: &str, area: &str, purpose: Option<&str>) -> bool {
    match scope {
        ShareScope::Track(id) => id == track,
        ShareScope::Area(id) => id == area && purpose != Some(crate::AREA_CHAT_PURPOSE),
    }
}

/// Resolve the public identifier without ever retrieving a plaintext secret.
pub(crate) async fn resolve_id(
    repo: &dyn RouteRepo,
    config: &crate::auth::AuthConfig,
    id: &str,
) -> Result<(ShareRead, String)> {
    let hash = repo
        .share_link_hash(id)
        .await?
        .ok_or_else(|| CalmError::NotFound("share link".into()))?;
    let access = resolve_hash(repo, config, &hash, None)
        .await
        .map_err(|e| match e {
            CalmError::Forbidden(_) | CalmError::Unauthorized => {
                CalmError::NotFound("share link".into())
            }
            other => other,
        })?;
    Ok((access, hash))
}
