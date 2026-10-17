//! Track detail access resolves the actual parent and current organization
//! facts without decoding private Track fields or acquiring a writer lock.
use crate::auth::Principal;
use crate::db::{RouteRepo, TrackAreaMembership};
use crate::error::{CalmError, Result};
use crate::ids::AreaId;
use calm_types::organization::AreaAccess;

async fn current_membership(
    repo: &dyn RouteRepo,
    track_id: &str,
    principal: &Principal,
) -> Result<TrackAreaMembership> {
    repo.track_area_membership(track_id, &principal.user_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))
}

pub(crate) async fn require_detail_access(
    repo: &dyn RouteRepo,
    track_id: &str,
    principal: &Principal,
) -> Result<AreaId> {
    let facts = current_membership(repo, track_id, principal).await?;
    crate::organization_access::require_area_access_from_membership(
        &facts.area,
        facts.member_role,
        principal,
        AreaAccess::Collaborate,
    )?;
    Ok(facts.area.id)
}

/// Resolve the current parent and authorize protected work in its committing transaction.
pub(crate) async fn require_detail_access_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
    principal: &Principal,
) -> Result<AreaId> {
    let track = crate::db::sqlite::track_get_tx(tx, &track_id.to_string().into()).await?;
    crate::organization_access::require_area_access_tx(
        tx,
        track.area_id.as_str(),
        principal,
        AreaAccess::Collaborate,
    )
    .await?;
    Ok(track.area_id)
}

pub(crate) fn require_same_area(expected: &AreaId, actual: &AreaId) -> Result<()> {
    if expected != actual {
        return Err(CalmError::Forbidden(
            "Track Area changed while reading".into(),
        ));
    }
    Ok(())
}

/// The final current decision for this finite response, not a stream lease or
/// permission to retract bytes already sent. Expected parent is always required.
pub(crate) async fn require_detail_publication(
    repo: &dyn RouteRepo,
    track_id: &str,
    expected_area: &AreaId,
    principal: &Principal,
) -> Result<()> {
    let facts = current_membership(repo, track_id, principal).await?;
    require_same_area(expected_area, &facts.area.id)?;
    crate::organization_access::require_area_access_from_membership(
        &facts.area,
        facts.member_role,
        principal,
        AreaAccess::Collaborate,
    )
}

#[cfg(feature = "fixtures")]
mod fixtures {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::sync::Notify;

    #[derive(Clone)]
    pub struct TrackDetailReadHook {
        pub entered: Arc<Notify>,
        pub release: Arc<Notify>,
    }

    fn hooks() -> &'static Mutex<HashMap<String, TrackDetailReadHook>> {
        static HOOKS: OnceLock<Mutex<HashMap<String, TrackDetailReadHook>>> = OnceLock::new();
        HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub fn install_track_detail_read_hook_for_test(track_id: &str, hook: TrackDetailReadHook) {
        hooks()
            .lock()
            .expect("Track detail hook mutex")
            .insert(track_id.into(), hook);
    }

    pub(crate) async fn after_assembly(track_id: &str) -> Result<()> {
        let hook = hooks()
            .lock()
            .expect("Track detail hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(30), hook.release.notified())
                .await
                .map_err(|_| {
                    CalmError::Internal("Track detail fixture release timed out".into())
                })?;
        }
        Ok(())
    }
}

#[cfg(feature = "fixtures")]
pub(crate) use fixtures::after_assembly;
#[cfg(feature = "fixtures")]
pub use fixtures::{TrackDetailReadHook, install_track_detail_read_hook_for_test};
