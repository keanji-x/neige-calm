//! Share delegation persistence. Decoding rejects facts outside the closed policy;
//! authorization belongs to organization/share access in the application layer.
use crate::db::{ShareReadFacts, ShareTrackFacts};
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
    expires_at: i64,
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
            expires_at: self.expires_at,
        })
    }
}

const COLUMNS: &str =
    "id,track_id,area_id,approver_user_id,purpose,resource_policy,created_at,revoked_at,expires_at";

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
        "purpose,resource_policy,created_at,token_hash,expires_at) ",
        "VALUES(?1,?2,?3,?4,'conversation','current_deployment',?5,?6,?7)",
    ))
    .bind(&delegation.id)
    .bind(track)
    .bind(area)
    .bind(&delegation.approver_user_id)
    .bind(delegation.created_at)
    .bind(token_hash)
    .bind(delegation.created_at + 86_400_000)
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

/// One autocommit statement snapshots access, registry, membership and containment.
/// No deferred transaction or private Workspace/provider field is needed.
#[derive(FromRow)]
struct ReadRow {
    #[sqlx(flatten)]
    link: LinkRow,
    target_area: String,
    #[sqlx(try_from = "String")]
    target_kind: crate::model::AreaKind,
    registered: bool,
    member_role: Option<String>,
    requested_id: Option<String>,
    requested_area: Option<String>,
    requested_purpose: Option<String>,
}

pub(super) async fn read_facts(
    pool: &sqlx::SqlitePool,
    hash: &str,
    owner: &str,
    requested: Option<&str>,
) -> Result<ShareReadFacts> {
    let columns = COLUMNS
        .split(',')
        .map(|column| format!("sl.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let row = sqlx::query_as::<_, ReadRow>(&format!(
        "SELECT {columns}, a.id AS target_area, a.kind AS target_kind,
                EXISTS(SELECT 1 FROM application_users WHERE id=?2) AS registered,
                m.role AS member_role, requested.id AS requested_id,
                requested.area_id AS requested_area, requested.purpose AS requested_purpose
         FROM share_links sl
         LEFT JOIN tracks target ON target.id=sl.track_id
         JOIN areas a ON a.id=COALESCE(sl.area_id,target.area_id)
         LEFT JOIN area_members m ON m.area_id=a.id AND m.user_id=?2
         LEFT JOIN tracks requested ON requested.id=?3
         WHERE sl.token_hash=?1 AND sl.revoked_at IS NULL AND sl.expires_at > ?4"
    ))
    .bind(hash)
    .bind(owner)
    .bind(requested)
    .bind(crate::model::now_ms())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| CalmError::NotFound("share link"))?;
    let link = row.link.decode()?;
    super::area_membership::require_registered(row.registered)?;
    super::area_membership::require_user_area_access(
        row.target_kind,
        super::area_membership::decode_member_role(row.member_role)?,
        calm_types::organization::AreaAccess::Delete,
    )?;
    let requested_track = match (requested, row.requested_id, row.requested_area) {
        (Some(_), Some(_), Some(area)) => Some(ShareTrackFacts {
            area_id: area.into(),
            purpose: row.requested_purpose,
        }),
        (None, None, None) => None,
        (Some(id), None, None) => return Err(CalmError::NotFound(format!("track {id}"))),
        _ => return Err(CalmError::Internal("Track parent Area is missing".into())),
    };
    Ok(ShareReadFacts {
        link,
        area: row.target_area.into(),
        requested_track,
    })
}

/// Use the owning bounded tree query, including its cycle/depth limit.
pub async fn track_tree_contains(
    conn: &mut sqlx::SqliteConnection,
    root: &str,
    track: &str,
) -> Result<bool> {
    let members: Vec<(String, i64)> = sqlx::query_as(super::track_tree::TRACK_TREE_MEMBERS_SQL)
        .bind(root)
        .bind(super::track_tree::MAX_TRACK_TREE_DEPTH)
        .fetch_all(conn)
        .await?;
    Ok(members
        .iter()
        .any(|(id, depth)| id == track && *depth <= super::track_tree::MAX_TRACK_TREE_DEPTH))
}
