//! The route half of `neige_track_add` (#2104 K1): the keyed create `POST /api/tracks` runs, for a
//! Planner that adds an ordinary top-level Track from a stored recipe, and the creator's cap on
//! open Tracks it added. The tool half decides who may call; this half only creates.

use std::sync::{Arc, OnceLock};

use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::TrackWorkspacePlan;
use crate::error::{CalmError, Result};
use crate::mcp_server::tools::track_add::{
    OpenAddedTrack, TrackAddRefusal, TrackAddRequest, TrackCreator,
};
use crate::model::{NewTrack, RequestTheme, Track};
use crate::routes::area_folders::normalize_path;
use crate::routes::codex_cards::default_cwd;
use crate::session_projection_repo::AgentProvider;
use crate::state::RouteState;

use super::create::{self, KeyedActor, KeyedCreate, KeyedPlan, TRACK_ADD_KEY_PREFIX};
use super::{CreateTrackOptions, FolderClaim, TrackInit};

/// What the create transaction checks and stamps for a Track that another Track's Planner adds.
pub(super) struct CreatorAdmission {
    creator_track_id: String,
    /// The raw key the creator passed, recorded as `tracks.creator_key`.
    creator_key: String,
    max_open: u32,
    /// Set when the cap refuses: the open Tracks the refusal lists, read by the same count.
    refused: Arc<OnceLock<Vec<OpenAddedTrack>>>,
}

impl CreatorAdmission {
    /// Refuse when the creator already has `max_open` open Tracks it added. Runs inside the create
    /// transaction (`BEGIN IMMEDIATE`), so concurrent adds cannot both take the last slot.
    pub(super) async fn admit_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        let open: Vec<(String, String)> = sqlx::query_as(concat!(
            "SELECT id, creator_key FROM tracks WHERE creator_track_id=?1",
            " AND closed_at IS NULL",
            " ORDER BY created_at, id",
        ))
        .bind(&self.creator_track_id)
        .fetch_all(&mut **tx)
        .await?;
        if open.len() >= self.max_open as usize {
            let open = open
                .into_iter()
                .map(|(track_id, creator_key)| OpenAddedTrack {
                    track_id,
                    creator_key,
                })
                .collect();
            let _ = self.refused.set(open);
            return Err(CalmError::Conflict(format!(
                "track {} is at its cap of {} open added Tracks",
                self.creator_track_id, self.max_open
            )));
        }
        Ok(())
    }

    /// Record the creator on the row just inserted. `parent_track_id` stays NULL: an added Track is
    /// top-level and charges no tree budget.
    pub(super) async fn stamp_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        track: &mut Track,
    ) -> Result<()> {
        sqlx::query("UPDATE tracks SET creator_track_id=?1, creator_key=?2 WHERE id=?3")
            .bind(&self.creator_track_id)
            .bind(&self.creator_key)
            .bind(track.id.as_str())
            .execute(&mut **tx)
            .await?;
        track.creator_track_id = Some(self.creator_track_id.clone());
        track.creator_key = Some(self.creator_key.clone());
        Ok(())
    }
}

/// `neige_track_add`'s [`TrackCreator`]: the keyed create over this process's routes.
pub(crate) struct RouteTrackCreator {
    route: RouteState,
    /// `--track-add-max-open`.
    max_open: u32,
}

impl RouteTrackCreator {
    pub(crate) fn new(route: RouteState, max_open: u32) -> Self {
        Self { route, max_open }
    }
}

#[async_trait::async_trait]
impl TrackCreator for RouteTrackCreator {
    async fn add(&self, request: TrackAddRequest) -> std::result::Result<Track, TrackAddRefusal> {
        let s = &self.route;
        let fingerprint = request.fingerprint()?;
        let TrackAddRequest {
            creator_track_id,
            area_id,
            create_actor,
            start_actor,
            planner_provider,
            args,
        } = request;
        // The same gate as `POST /api/tracks` (#1817): read before the area lock, applied to a
        // mint only; a replay mints nothing.
        let claude_availability = if planner_provider == AgentProvider::Claude {
            Some(
                s.provider_availability
                    .claude(crate::agent_providers::Freshness::Cached, &s.claude_planner)
                    .await,
            )
        } else {
            None
        };
        let _area_delete_guard =
            crate::per_card_lock::lock_key(&s.area_delete_locks, area_id.as_str()).await;
        let plan = create::plan_keyed_create(
            s,
            KeyedCreate {
                area_id: area_id.clone(),
                idempotency_key: format!(
                    "{TRACK_ADD_KEY_PREFIX}{creator_track_id}/{}",
                    args.idempotency_key
                ),
                create_request_sha256: fingerprint,
                text: args.text.clone(),
            },
        )
        .await?;
        let actor = KeyedActor {
            create: create_actor,
            start_label: start_actor.to_string(),
            start: start_actor,
        };
        let plan = match plan {
            KeyedPlan::Resume(resume) => {
                return Ok(create::resume_prior_attempt(s.clone(), &actor, resume).await?);
            }
            KeyedPlan::Mint(plan) => plan,
        };
        // A dependency unavailable now, not a bad argument: the same call succeeds once the
        // provider is ready.
        if let Some(checked) = &claude_availability
            && let Err(refusal) = checked.catalog()
        {
            return Err(TrackAddRefusal::ProviderUnavailable(format!(
                "the creator's Planner provider {refusal}"
            )));
        }
        let cwd = normalize_path(&default_cwd());
        let refused = Arc::new(OnceLock::new());
        let p = NewTrack {
            area_id: area_id.clone().into(),
            title: args.title.trim().to_string(),
            sort: None,
            cwd: cwd.clone(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        };
        let options = CreateTrackOptions {
            managed_identity: None,
            creator: Some(CreatorAdmission {
                creator_track_id,
                creator_key: args.idempotency_key,
                max_open: self.max_open,
                refused: refused.clone(),
            }),
            creation_message: Some(args.message),
            planner_provider,
            model: None,
            reasoning_effort: None,
            folder_claim: FolderClaim::Skip,
            body_area_id: area_id,
            normalized_cwd: cwd,
            init: TrackInit::Recipe {
                recipe_id: args.recipe_id,
            },
            workspace_plan: TrackWorkspacePlan::ManagedUnder(s.workspace_root.clone()),
            // Set by `create_track_with_first_message` from the plan.
            idempotency_claim: None,
        };
        create::create_track_with_first_message(s.clone(), &actor, p, options, plan)
            .await
            .map_err(|error| match refused.get() {
                Some(open) => TrackAddRefusal::OpenCap {
                    cap: self.max_open,
                    open: open.clone(),
                },
                None => error.into(),
            })
    }
}
