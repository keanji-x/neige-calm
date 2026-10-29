//! The route half of `neige_track_add` (#2104 K1): the keyed create `POST /api/tracks` runs, for a
//! Planner that adds an ordinary top-level Track from a template or recipe, and the creator's cap on
//! open Tracks it added. This half checks Admin authority atomically with creation.

use std::sync::{Arc, OnceLock};

use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::{
    TrackCreateBinding, TrackCreateRequestFingerprint, track_create_idempotency_get_tx,
};
use crate::error::{CalmError, Result};
use crate::ids::ActorId;
use crate::mcp_server::tools::track_add::{
    OpenAddedTrack, TrackAddRefusal, TrackAddRequest, TrackCreator,
};
use crate::model::{NewTrack, RequestTheme, Track};
use crate::routes::codex_cards::default_cwd;
use crate::state::RouteState;

use super::create::{self, KeyedActor, KeyedCreate, KeyedPlan, SendPath, TRACK_ADD_KEY_PREFIX};
use super::{CreateTrackOptions, preparation};

enum AdmissionRefusal {
    NotAdmin,
    OpenCap(Vec<OpenAddedTrack>),
}

/// What the create transaction checks and stamps for a Track that another Track's Planner adds.
#[derive(Clone)]
pub(super) struct CreatorAdmission {
    creator_track_id: String,
    session: calm_types::worker::WorkerSessionId,
    card: crate::ids::CardId,
    area: crate::ids::AreaId,
    text: String,
    summary: String,
    /// The raw key the creator passed, recorded as `tracks.creator_key`.
    creator_key: String,
    max_open: u32,
    /// Typed authority or cap refusal carried out of the rolled-back transaction.
    refused: Arc<OnceLock<AdmissionRefusal>>,
}

impl CreatorAdmission {
    async fn require_source_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        use calm_truth::decision_gate::{GateDecision, PrincipalDecisionGate};
        let principal = calm_types::worker::Principal::Agent {
            session_id: self.session.clone(),
            track_id: self.creator_track_id.clone().into(),
            area_id: self.area.clone(),
        };
        match PrincipalDecisionGate::new(principal)
            .decide_card_write(
                tx,
                &self.creator_track_id.clone().into(),
                &self.card,
                crate::model::CardRole::Planner,
            )
            .await?
        {
            GateDecision::Allow => {
                let sender =
                    crate::db::sqlite::track_get_tx(tx, &self.creator_track_id.clone().into())
                        .await?;
                if sender.area_id != self.area {
                    return Err(CalmError::Forbidden("creator cannot cross Areas".into()));
                }
                Ok(())
            }
            GateDecision::Deny(_) => {
                Err(CalmError::Forbidden("creator session is inactive".into()))
            }
        }
    }

    /// Canonical replay returns the surviving allocation before mutable mint
    /// checks. Historical receipts are dispatched explicitly by the route.
    pub(super) async fn replay_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        claim: &super::TrackCreateIdempotencyClaim,
    ) -> Result<Option<Track>> {
        self.require_source_tx(tx).await?;
        let Some(binding) =
            track_create_idempotency_get_tx(tx, self.area.as_str(), &claim.key).await?
        else {
            return Ok(None);
        };
        self.replay_binding_tx(tx, &binding, claim).await.map(Some)
    }

    async fn replay_binding_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        binding: &TrackCreateBinding,
        claim: &super::TrackCreateIdempotencyClaim,
    ) -> Result<Track> {
        if !crate::db::sqlite::conversation::card_has_canonical_storage(
            tx,
            &binding.planner_card_id,
        )
        .await?
        {
            return Err(CalmError::IdempotencyKeyReused(
                "historical creator receipt".into(),
            ));
        }
        match &binding.request_fingerprint {
            TrackCreateRequestFingerprint::V1 {
                create_request_sha256,
                first_message_sha256,
            } if create_request_sha256 == &claim.create_request_sha256
                && Some(first_message_sha256) == claim.first_message_sha256.as_ref() => {}
            _ => {
                return Err(crate::operation::idempotency_payload_conflict(Some(
                    &claim.key,
                )));
            }
        }
        super::create::dormant::require_allocation_resources_tx(
            tx,
            self.area.as_str(),
            &binding.track_id,
            &binding.planner_card_id,
            &binding.report_card_id,
        )
        .await?;
        let track = crate::db::sqlite::track_get_tx(tx, &binding.track_id.clone().into()).await?;
        if track.creator_track_id.as_deref() != Some(&self.creator_track_id)
            || track.creator_key.as_deref() != Some(&self.creator_key)
        {
            return Err(CalmError::IdempotencyKeyReused(
                "creator provenance differs".into(),
            ));
        }
        Ok(track)
    }

    pub(super) async fn first_mail_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        recipient: &Track,
    ) -> Result<(crate::event::EventScope, crate::event::Event)> {
        let sender =
            crate::db::sqlite::track_get_tx(tx, &self.creator_track_id.clone().into()).await?;
        let hop = crate::mail::creator_hop_tx(
            tx,
            &self.creator_track_id,
            self.card.as_str(),
            self.session.as_str(),
        )
        .await?;
        let (_, event) = crate::mail::write_mail_tx(
            tx,
            &sender,
            recipient,
            &self.summary,
            &self.text,
            hop,
            None,
            crate::mail::WakeBody::FullText,
        )
        .await?;
        Ok(event)
    }

    /// Refuse when the creator already has `max_open` open Tracks it added. Runs inside the create
    /// transaction (`BEGIN IMMEDIATE`), so concurrent adds cannot both take the last slot.
    pub(super) async fn admit_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        match crate::area_admin::require_admin_tx(tx, &self.creator_track_id).await {
            Ok(()) => {}
            Err(crate::area_admin::RequireAdminError::NotAdmin) => {
                let _ = self.refused.set(AdmissionRefusal::NotAdmin);
                return Err(CalmError::Forbidden("not_admin".into()));
            }
            Err(crate::area_admin::RequireAdminError::Storage(error)) => return Err(error),
        }
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
            let _ = self.refused.set(AdmissionRefusal::OpenCap(open));
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
        crate::routes::conversations_shared::validate_first_message(&args.text)?;
        let (ActorId::AiPlannerSession(session), ActorId::AiPlanner(card)) =
            (&create_actor, &start_actor)
        else {
            return Err(CalmError::Forbidden(
                "creator requires its live Planner session/card".into(),
            )
            .into());
        };
        let refused = Arc::new(OnceLock::new());
        let admission = CreatorAdmission {
            creator_track_id: creator_track_id.clone(),
            creator_key: args.idempotency_key.clone(),
            session: session.clone(),
            card: card.clone(),
            area: area_id.clone().into(),
            text: args.text.clone(),
            summary: args.message.clone(),
            max_open: self.max_open,
            refused: refused.clone(),
        };
        let claim = super::TrackCreateIdempotencyClaim {
            key: format!(
                "{TRACK_ADD_KEY_PREFIX}{creator_track_id}/{}",
                args.idempotency_key
            ),
            create_request_sha256: fingerprint,
            first_message_sha256: Some(crate::conversation_keys::first_message_digest(&args.text)),
        };
        let _area_delete_guard =
            crate::per_card_lock::lock_key(&s.area_delete_locks, &area_id).await;
        let same_key =
            crate::per_card_lock::lock_card(&s.conversation_first_message_locks, &claim.key).await;
        let historical = crate::db::write_in_tx_typed(s.repo.as_ref(), {
            let admission = admission.clone();
            let claim = super::TrackCreateIdempotencyClaim {
                key: claim.key.clone(),
                create_request_sha256: claim.create_request_sha256.clone(),
                first_message_sha256: claim.first_message_sha256.clone(),
            };
            move |tx| {
                Box::pin(async move {
                    admission.require_source_tx(tx).await?;
                    let Some(binding) =
                        track_create_idempotency_get_tx(tx, admission.area.as_str(), &claim.key)
                            .await?
                    else {
                        if create::dormant::prior_start_attempt_tx(
                            tx,
                            admission.area.as_str(),
                            &claim.key,
                        )
                        .await?
                        {
                            return Err(CalmError::Internal(
                                "historical creator start lost its allocation binding".into(),
                            ));
                        }
                        return Ok(None);
                    };
                    if crate::db::sqlite::conversation::card_has_canonical_storage(
                        tx,
                        &binding.planner_card_id,
                    )
                    .await?
                    {
                        Ok(Some(Ok(admission
                            .replay_binding_tx(tx, &binding, &claim)
                            .await?)))
                    } else {
                        Ok(Some(Err(())))
                    }
                })
            }
        })
        .await?;
        match historical {
            Some(Ok(track)) => return Ok(track),
            Some(Err(())) => {
                drop(same_key);
                let actor = KeyedActor {
                    human_source: None,
                    create: create_actor,
                    start_label: start_actor.to_string(),
                    start: start_actor,
                    send_path: SendPath::Mail,
                };
                let plan = create::plan_keyed_create(
                    s,
                    KeyedCreate {
                        area_id,
                        idempotency_key: claim.key,
                        create_request_sha256: claim.create_request_sha256,
                        text: args.text,
                    },
                )
                .await?;
                return match plan {
                    KeyedPlan::Resume(resume) => {
                        Ok(create::resume_prior_attempt(s.clone(), &actor, resume).await?)
                    }
                    KeyedPlan::Mint(_) => Err(CalmError::IdempotencyKeyExhausted(
                        "historical receipt disappeared".into(),
                    )
                    .into()),
                };
            }
            None => {}
        }
        // Only a fresh allocation consults current readiness and resource catalog.
        crate::harness::backend::factory::PlannerFactory::new(
            std::sync::Weak::new(),
            &s.claude_planner,
            &s.acp_planner,
        )
        .catalog_readiness(&planner_provider, &s.provider_availability)
        .await
        .require_added_track()
        .map_err(TrackAddRefusal::ProviderUnavailable)?;
        let cwd_omitted = args.cwd.is_none();
        let cwd = args.cwd.clone().unwrap_or_else(default_cwd);
        let p = NewTrack {
            area_id: area_id.clone().into(),
            title: args.title.trim().to_string(),
            sort: None,
            cwd: cwd.clone(),
            template_id: None,
            plugin_scope: None,
            template_input: args.template_input,
            attach_folder: args.attach_folder.unwrap_or(false),
            theme: RequestTheme::default_dark(),
        };
        let named_source =
            preparation::NamedSource::from_request(args.template_id, args.recipe_id, None)?;
        let preparation::PreparedCreate {
            p,
            folder_claim,
            body_area_id,
            normalized_cwd,
            init,
            workspace_plan,
            ..
        } = preparation::prepare(
            s,
            p,
            named_source,
            if cwd_omitted {
                super::CreateIsolation::Managed
            } else {
                super::CreateIsolation::Attached(super::AttachedIsolation::Worktree)
            },
            None,
        )
        .await?;
        let options = CreateTrackOptions {
            human_source: None,
            admin: false,
            managed_identity: None,
            creator: Some(admission),
            creation_message: Some(args.message),
            planner_provider,
            model: None,
            reasoning_effort: None,
            folder_claim,
            body_area_id,
            normalized_cwd,
            init,
            workspace_plan,
            idempotency_claim: Some(claim),
        };
        if let Some(gate) = s.track_create_mint_rendezvous.clone() {
            gate.hold().await;
        }
        super::create_track_structure_with_context(
            s.clone(),
            create_actor,
            p,
            options,
            super::allocation::CreationContext::CreatorCanonical,
        )
        .await
        .map(|(track, _, _, _)| track)
        .map_err(|error| match refused.get() {
            Some(AdmissionRefusal::NotAdmin) => TrackAddRefusal::NotAdmin,
            Some(AdmissionRefusal::OpenCap(open)) => TrackAddRefusal::OpenCap {
                cap: self.max_open,
                open: open.clone(),
            },
            None => error.into(),
        })
    }
}

#[async_trait::async_trait]
impl crate::mcp_server::tools::track_sources::TrackCatalog for RouteTrackCreator {
    async fn list_sources(
        &self,
        request: crate::routes::track_catalog::ListSources,
    ) -> Result<serde_json::Value> {
        crate::routes::track_catalog::list(&self.route, request).await
    }
    async fn get_source(
        &self,
        request: crate::routes::track_catalog::GetSource,
    ) -> Result<serde_json::Value> {
        crate::routes::track_catalog::get(&self.route, request).await
    }
}

/// A released receipt fixture goes through the actual historical allocation and
/// delivery owner. Fresh production adds never select this path.
#[cfg(any(test, feature = "fixtures"))]
impl RouteTrackCreator {
    pub(crate) async fn historical_receipt_for_test(
        &self,
        request: impl Into<crate::test_seams::HistoricalReceiptForTest>,
    ) -> Result<Track> {
        let request = match request.into() {
            crate::test_seams::HistoricalReceiptForTest::Creator(request) => request,
            crate::test_seams::HistoricalReceiptForTest::Released { request, key } => {
                return self.released_receipt_for_test(request, key).await;
            }
        };
        let s = &self.route;
        let sha = request.fingerprint()?;
        let (ActorId::AiPlannerSession(session), ActorId::AiPlanner(card)) =
            (&request.create_actor, &request.start_actor)
        else {
            unreachable!()
        };
        let admission = CreatorAdmission {
            creator_track_id: request.creator_track_id.clone(),
            creator_key: request.args.idempotency_key.clone(),
            session: session.clone(),
            card: card.clone(),
            area: request.area_id.clone().into(),
            text: request.args.text.clone(),
            summary: request.args.message.clone(),
            max_open: self.max_open,
            refused: Arc::new(OnceLock::new()),
        };
        let plan = create::plan_keyed_create(
            s,
            KeyedCreate {
                area_id: request.area_id.clone(),
                idempotency_key: format!(
                    "{TRACK_ADD_KEY_PREFIX}{}/{}",
                    request.creator_track_id, request.args.idempotency_key
                ),
                create_request_sha256: sha,
                text: request.args.text,
            },
        )
        .await?;
        let actor = KeyedActor {
            human_source: None,
            create: request.create_actor,
            start_label: request.start_actor.to_string(),
            start: request.start_actor,
            send_path: SendPath::Mail,
        };
        let plan = match plan {
            KeyedPlan::Mint(plan) => plan,
            KeyedPlan::Resume(resume) => {
                return create::resume_prior_attempt(s.clone(), &actor, resume).await;
            }
        };
        let cwd_omitted = request.args.cwd.is_none();
        let prepared = preparation::prepare(
            s,
            NewTrack {
                area_id: request.area_id.clone().into(),
                title: request.args.title,
                sort: None,
                cwd: request.args.cwd.unwrap_or_else(default_cwd),
                template_id: None,
                plugin_scope: None,
                template_input: request.args.template_input,
                attach_folder: request.args.attach_folder.unwrap_or(false),
                theme: RequestTheme::default_dark(),
            },
            preparation::NamedSource::from_request(
                request.args.template_id,
                request.args.recipe_id,
                None,
            )?,
            if cwd_omitted {
                super::CreateIsolation::Managed
            } else {
                super::CreateIsolation::Attached(super::AttachedIsolation::Worktree)
            },
            None,
        )
        .await?;
        create::create_track_with_first_message(
            s.clone(),
            &actor,
            prepared.p,
            CreateTrackOptions {
                human_source: None,
                admin: false,
                managed_identity: None,
                creator: Some(admission),
                creation_message: Some(request.args.message),
                planner_provider: request.planner_provider,
                model: None,
                reasoning_effort: None,
                folder_claim: prepared.folder_claim,
                body_area_id: prepared.body_area_id,
                normalized_cwd: prepared.normalized_cwd,
                init: prepared.init,
                workspace_plan: prepared.workspace_plan,
                idempotency_claim: None,
            },
            plan,
        )
        .await
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl RouteTrackCreator {
    /// Reconstruct released REST resources through their allocation and delivery owners.
    /// No fresh HTTP request can select this fixture path.
    async fn released_receipt_for_test(
        &self,
        mut request: super::CreateTrackRequest,
        key: Option<String>,
    ) -> Result<Track> {
        use super::create::CreateShape;
        use axum::http::HeaderMap;
        use http_body_util::BodyExt;
        let s = &self.route;
        let mut headers = HeaderMap::new();
        if let Some(key) = key {
            headers.insert(
                "idempotency-key",
                key.parse()
                    .map_err(|_| CalmError::BadRequest("invalid fixture key".into()))?,
            );
        }
        let plan = create::plan_first_message(
            s,
            &headers,
            request.first_message.take(),
            request.area_id.as_str(),
            create::CreateRequestShape::from_request(&request)?,
        )
        .await?;
        let actor = crate::actor::Actor::server_send();
        let (first, empty) = match plan {
            CreateShape::WithFirstMessage(KeyedPlan::Resume(resume)) => {
                return create::resume_prior_attempt(
                    s.clone(),
                    &KeyedActor::rest(&actor, None),
                    resume,
                )
                .await;
            }
            CreateShape::MessageLess(KeyedPlan::Resume(resume)) => {
                let response = create::resume_message_less(s.clone(), actor, resume, None).await?;
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                return Ok(serde_json::from_slice(&bytes)?);
            }
            CreateShape::WithFirstMessage(KeyedPlan::Mint(plan)) => (Some(plan), None),
            CreateShape::MessageLess(KeyedPlan::Mint(plan)) => (None, plan),
        };
        let provider = request.planner_provider.clone();
        let model = request.model.clone();
        let reasoning_effort = request.reasoning_effort.clone();
        let isolation = super::CreateIsolation::decode(&request.cwd, request.isolation)?;
        let cross = request.allow_cross_area_cwd.clone();
        let admin = request.admin;
        let (p, named, _) = request.into_parts()?;
        let prepared = preparation::prepare(s, p, named, isolation, cross).await?;
        let options = CreateTrackOptions {
            human_source: None,
            admin,
            managed_identity: None,
            creator: None,
            creation_message: None,
            planner_provider: provider,
            model,
            reasoning_effort,
            folder_claim: prepared.folder_claim,
            body_area_id: prepared.body_area_id,
            normalized_cwd: prepared.normalized_cwd,
            init: prepared.init,
            workspace_plan: prepared.workspace_plan,
            idempotency_claim: empty.as_ref().map(create::MessageLessPlan::claim),
        };
        if let Some(plan) = first {
            return create::create_track_with_first_message(
                s.clone(),
                &KeyedActor::rest(&actor, None),
                prepared.p,
                options,
                plan,
            )
            .await;
        }
        let response =
            super::create_track_with_planner_harness(s.clone(), actor, prepared.p, options).await?;
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Ok(serde_json::from_slice(&bytes)?)
    }
}
