//! Anonymous read adapters. Every resource consumes the shared audience contract.
use super::{
    conversation_items::ConversationItemsQuery,
    conversation_run::RunReadQuery,
    track_report_series::{ReportSeriesDetail, ReportSeriesQuery},
};
use crate::extract::{Json, Path, Query};
use crate::{
    auth::AuthState,
    error::{CalmError, ErrorBody, Result},
    read_access::Audience,
    state::{AppState, RouteState, WorkerState},
};
use axum::{
    Extension, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
};
use calm_types::share_links::ShareScope;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

pub fn router(auth: AuthState) -> Router<AppState> {
    Router::new()
        .route("/api/share/{token}", get(resolve))
        .route("/api/share/{token}/tracks/{track_id}/report", get(report))
        .route(
            "/api/share/{token}/tracks/{track_id}/report/series",
            get(series),
        )
        .route("/api/share/{token}/tracks/{track_id}/sources", get(sources))
        .route(
            "/api/share/{token}/tracks/{track_id}/sources/{source_id}",
            get(source),
        )
        .route(
            "/api/share/{token}/tracks/{track_id}/conversations",
            get(conversations),
        )
        .route(
            "/api/share/{token}/tracks/{track_id}/conversations/{card_id}/items",
            get(items),
        )
        .route(
            "/api/share/{token}/tracks/{track_id}/conversations/{card_id}/live",
            get(live),
        )
        .route(
            "/api/share/{token}/tracks/{track_id}/conversations/{card_id}/run",
            get(run),
        )
        .merge(crate::ws::share_events::router())
        .layer(Extension(auth))
}

#[derive(Serialize, ToSchema)]
pub struct SharedTrack {
    pub id: String,
    pub title: String,
}
#[derive(Serialize, ToSchema)]
pub struct SharedTarget {
    pub scope: ShareScope,
    pub tracks: Vec<SharedTrack>,
}
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SharedSeriesQuery {
    pub block_id: String,
    pub rev: u64,
    #[serde(default)]
    #[param(required = false)]
    pub detail: ReportSeriesDetail,
}
#[derive(Serialize, ToSchema)]
pub struct SharedRun {
    pub card_id: String,
    #[schema(required = true, nullable = true)]
    pub phase: Option<calm_types::harness::HarnessPhaseTag>,
    #[schema(required = true, nullable = true)]
    pub runtime_state: Option<calm_types::worker::WorkerSessionState>,
}

fn audience<'a>(token: &'a str, auth: &'a AuthState) -> Audience<'a> {
    Audience::ShareLink {
        token,
        config: &auth.config,
    }
}

/// Require both the URL Track and the actual card's current Track. Generic
/// transcript readers never decide which conversation belongs to a share URL.
async fn conversation(
    s: &RouteState,
    auth: &AuthState,
    token: &str,
    track: &str,
    card: &str,
) -> Result<()> {
    audience(token, auth).check(s.repo.as_ref(), track).await?;
    if s.repo.card_track_id(card).await?.as_deref() != Some(track) {
        return Err(CalmError::NotFound("shared conversation".into()));
    }
    let role = s.write.verify_role(&card.into());
    if !matches!(
        role,
        Some(crate::model::CardRole::Planner | crate::model::CardRole::Assistant)
    ) {
        return Err(CalmError::NotFound("shared conversation".into()));
    }
    Ok(())
}

#[utoipa::path(get, path="/api/share/{token}", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier")),
    responses(
        (status=200,description="Shared resource",body=SharedTarget),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn resolve(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path(token): Path<String>,
) -> Result<Response> {
    let access = crate::share_access::resolve(s.repo.as_ref(), &auth.config, &token, None).await?;
    let tracks = match &access.link.delegation.scope {
        ShareScope::Track(id) => vec![
            s.repo
                .track_get(id)
                .await?
                .ok_or_else(|| CalmError::NotFound("shared Track".into()))?,
        ],
        ShareScope::Area(id) => s
            .repo
            .tracks_by_area(id)
            .await?
            .into_iter()
            .filter(|t| t.purpose.as_deref() != Some(crate::AREA_CHAT_PURPOSE))
            .collect(),
    };
    for track in &tracks {
        let current = crate::share_access::resolve(
            s.repo.as_ref(),
            &auth.config,
            &token,
            Some(track.id.as_str()),
        )
        .await?;
        if current != access {
            return Err(CalmError::Forbidden("share scope changed".into()));
        }
    }
    crate::share_access::require_publication(s.repo.as_ref(), &auth.config, &token, None, &access)
        .await?;
    Ok(Json(SharedTarget {
        scope: access.link.delegation.scope,
        tracks: tracks
            .into_iter()
            .map(|t| SharedTrack {
                id: t.id.to_string(),
                title: t.title,
            })
            .collect(),
    })
    .into_response())
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/report", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier")),
    responses(
        (status=200,description="Shared resource",body=super::tracks::TrackReportReadResponse),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn report(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id)): Path<(String, String)>,
) -> Result<Response> {
    crate::report_resource::get_track_report(&s, audience(&token, &auth), track_id).await
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/report/series", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier"), SharedSeriesQuery),
    responses(
        (status=200,description="Shared resource",body=super::track_report_series::ReportSeriesResolved),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Block revision changed",body=super::track_report_series::ReportSeriesRevConflict),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn series(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id)): Path<(String, String)>,
    Query(q): Query<SharedSeriesQuery>,
) -> Result<Response> {
    crate::report_resource::get_report_series(
        &s,
        audience(&token, &auth),
        track_id,
        q.block_id,
        ReportSeriesQuery {
            rev: q.rev,
            detail: q.detail,
        },
    )
    .await
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/sources", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier")),
    responses(
        (status=200,description="Shared resource",body=crate::report_sources::TrackSourceList),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn sources(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id)): Path<(String, String)>,
) -> Result<Response> {
    crate::report_resource::list_track_sources(&s, audience(&token, &auth), track_id).await
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/sources/{source_id}", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier"), ("source_id"=String,Path,description="Shared resource identifier")),
    responses(
        (status=200,description="Shared resource",body=crate::report_sources::TrackSourceDetail),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn source(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id, source_id)): Path<(String, String, String)>,
) -> Result<Response> {
    crate::report_resource::get_track_source(&s, audience(&token, &auth), track_id, source_id).await
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/conversations", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier")),
    responses(
        (status=200,description="Shared resource",body=Vec<crate::model::TrackConversationSummary>),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn conversations(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id)): Path<(String, String)>,
) -> Result<Response> {
    let a = audience(&token, &auth);
    let fence = a.check(s.repo.as_ref(), &track_id).await?;
    let rows = super::track_conversations::load_published_summaries(&w, &track_id).await?;
    for row in &rows {
        if s.repo.card_track_id(&row.id).await?.as_deref() != Some(&track_id) {
            return Err(CalmError::Forbidden("conversation moved".into()));
        }
    }
    a.publish(s.repo.as_ref(), &track_id, &fence).await?;
    Ok(Json(rows).into_response())
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/conversations/{card_id}/items", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier"), ("card_id"=String,Path,description="Shared resource identifier"), ConversationItemsQuery),
    responses(
        (status=200,description="Shared resource",body=super::conversation_items::ConversationItemsReadResponse),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn items(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id, card_id)): Path<(String, String, String)>,
    Query(q): Query<ConversationItemsQuery>,
) -> Result<Response> {
    conversation(&s, &auth, &token, &track_id, &card_id).await?;
    let response =
        super::conversation_items::read(&s, audience(&token, &auth), card_id.clone(), q).await?;
    conversation(&s, &auth, &token, &track_id, &card_id).await?;
    Ok(response)
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/conversations/{card_id}/live", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier"), ("card_id"=String,Path,description="Shared resource identifier"), RunReadQuery),
    responses(
        (status=200,description="Shared resource",body=super::harness_live::HarnessLiveReadResponse),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn live(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id, card_id)): Path<(String, String, String)>,
    Query(q): Query<RunReadQuery>,
) -> Result<Response> {
    conversation(&s, &auth, &token, &track_id, &card_id).await?;
    let response =
        super::harness_live::read(&s, audience(&token, &auth), card_id.clone(), q).await?;
    conversation(&s, &auth, &token, &track_id, &card_id).await?;
    Ok(response)
}

#[utoipa::path(get, path="/api/share/{token}/tracks/{track_id}/conversations/{card_id}/run", tag="share_links", security(()),
    params(("token"=String,Path,description="Shared resource identifier"), ("track_id"=String,Path,description="Shared resource identifier"), ("card_id"=String,Path,description="Shared resource identifier")),
    responses(
        (status=200,description="Shared resource",body=SharedRun),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share or resource not found",body=ErrorBody),
        (status=409,description="Resource revision or read contract changed",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn run(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path((token, track_id, card_id)): Path<(String, String, String)>,
) -> Result<Response> {
    conversation(&s, &auth, &token, &track_id, &card_id).await?;
    let a = audience(&token, &auth);
    let scope =
        super::conversation_read::ReadScope::check_audience(s.repo.as_ref(), &card_id, a).await?;
    let frame = super::conversation_read::current_frame(s.repo.as_ref(), &card_id).await?;
    let response = SharedRun {
        card_id: card_id.clone(),
        phase: frame.canonical.as_ref().map(|f| f.header.phase),
        runtime_state: frame.runtime.map(|r| r.state),
    };
    scope.publish_audience(s.repo.as_ref(), a).await?;
    conversation(&s, &auth, &token, &track_id, &card_id).await?;
    Ok(Json(response).into_response())
}
