//! Share delegation persistence. Decoding rejects facts outside the closed policy;
//! authorization belongs to organization/share access in the application layer.
use crate::error::{CalmError, Result};
use calm_types::share_links::{
    ShareDelegation, ShareLink, SharePurpose, ShareResourcePolicy, ShareScope,
};
use sqlx::{FromRow, Sqlite, Transaction};

#[derive(FromRow)]
struct LinkRow {
    id: String,
    track_id: Option<String>,
    area_id: Option<String>,
    approver_user_id: String,
    purpose: String,
    resource_policy: String,
    created_at: i64,
    revoked_at: Option<i64>,
}

impl LinkRow {
    fn decode(self) -> Result<ShareLink> {
        let scope = match (self.track_id, self.area_id) {
            (Some(id), None) => ShareScope::Track(id),
            (None, Some(id)) => ShareScope::Area(id),
            _ => return Err(CalmError::Forbidden("invalid share scope".into())),
        };
        if self.purpose != "conversation" || self.resource_policy != "current_deployment" {
            return Err(CalmError::Forbidden("unsupported share delegation".into()));
        }
        Ok(ShareLink {
            delegation: ShareDelegation {
                id: self.id,
                scope,
                approver_user_id: self.approver_user_id,
                purpose: SharePurpose::Conversation,
                resource_policy: ShareResourcePolicy::CurrentDeployment,
                created_at: self.created_at,
            },
            revoked_at: self.revoked_at,
        })
    }
}

const COLUMNS: &str =
    "id,track_id,area_id,approver_user_id,purpose,resource_policy,created_at,revoked_at";

fn target(scope: &ShareScope) -> (Option<&str>, Option<&str>) {
    match scope {
        ShareScope::Track(id) => (Some(id), None),
        ShareScope::Area(id) => (None, Some(id)),
    }
}

pub async fn get_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<ShareLink> {
    sqlx::query_as::<_, LinkRow>(&format!("SELECT {COLUMNS} FROM share_links WHERE id=?1"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("share link {id}")))?
        .decode()
}

pub async fn list_tx(
    tx: &mut Transaction<'_, Sqlite>,
    scope: &ShareScope,
) -> Result<Vec<ShareLink>> {
    let (track, area) = target(scope);
    sqlx::query_as::<_, LinkRow>(&format!(
        "SELECT {COLUMNS} FROM share_links WHERE track_id IS ?1 AND area_id IS ?2 ORDER BY created_at,id"
    ))
    .bind(track)
    .bind(area)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(LinkRow::decode)
    .collect()
}

pub async fn create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    delegation: &ShareDelegation,
    token_hash: &str,
) -> Result<ShareLink> {
    let (track, area) = target(&delegation.scope);
    let SharePurpose::Conversation = delegation.purpose;
    let ShareResourcePolicy::CurrentDeployment = delegation.resource_policy;
    sqlx::query(concat!(
        "INSERT INTO share_links(id,track_id,area_id,approver_user_id,",
        "purpose,resource_policy,created_at,token_hash) ",
        "VALUES(?1,?2,?3,?4,'conversation','current_deployment',?5,?6)",
    ))
    .bind(&delegation.id)
    .bind(track)
    .bind(area)
    .bind(&delegation.approver_user_id)
    .bind(delegation.created_at)
    .bind(token_hash)
    .execute(&mut **tx)
    .await?;
    get_tx(tx, &delegation.id).await
}

pub async fn revoke_tx(tx: &mut Transaction<'_, Sqlite>, id: &str, now: i64) -> Result<ShareLink> {
    sqlx::query(
        "UPDATE share_links SET revoked_at=max(created_at,?2) WHERE id=?1 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    get_tx(tx, id).await
}
