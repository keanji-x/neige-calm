//! `GET /api/track-templates` — the New track picker's read side. An aggregate view,
//! not a table: `id`/`title` from the roster, `input_schema` from the owning plugin's
//! manifest, `tasks` projected off the same compiled recipe `POST /api/tracks`
//! instantiates. This endpoint performs no write of any kind.

use crate::error::{ErrorBody, Result};
use crate::routes::tracks::{compile_template, resolve_template_binding};
use crate::state::{AppState, RouteState};
use crate::templates::{Template, task_payload_key_and_instruction};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/track-templates", get(list_track_templates))
        .route("/api/track-templates/{id}", get(get_track_template))
        .route(
            "/api/track-templates/{id}/plugin-guides",
            get(get_template_plugin_guides),
        )
}

/// One selectable starting point for a new track. "Blank" is not in this list: it is
/// the absence of a template, rendered by the client as its own default option.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrackTemplate {
    /// Template key. Passed back verbatim as `template_id` on `POST /api/tracks`.
    pub id: String,
    pub title: String,
    /// JSON Schema for `template_input`, from the manifest of the running trusted plugin
    /// bound to `id`. Absent means the template takes no input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// The tasks this template pre-sets, in plan order. Always present; not always non-empty.
    pub tasks: Vec<TrackTemplateTask>,
}

/// One pre-set task, projected from a `task` fence in the template file's body.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrackTemplateTask {
    /// The task block's `key` in the recipe this template instantiates to.
    pub key: String,
    /// What that task is for, verbatim from the template.
    pub goal: String,
}

#[utoipa::path(
    get,
    path = "/api/track-templates",
    tag = "tracks",
    responses(
        (status = 200, description = "Selectable track templates", body = Vec<TrackTemplate>),
        // Reachable, not boilerplate: a roster recipe that does not compile answers 500 here
        // for the same reason `POST /api/tracks` does.
        (status = 500, description = "A built-in recipe did not compile", body = ErrorBody),
    ),
)]
pub(crate) async fn list_track_templates(
    State(s): State<RouteState>,
) -> Result<Json<Vec<TrackTemplate>>> {
    let roster = s.templates.entries();
    let mut templates = Vec::with_capacity(roster.len());
    for template in s.templates.user_entries() {
        // Same resolver as create-time binding, so a template can never be advertised with a
        // schema the create path would then refuse to validate against.
        let input_schema = resolve_template_binding(&s, template)
            .await
            .and_then(|manifest| manifest.input_schema.clone());
        let definition = current_definition(template)?;
        templates.push(TrackTemplate {
            id: template.key().to_string(),
            title: definition.title,
            input_schema,
            // Tombstoned blocks are dropped by the projection, not by the read: the picker must
            // not advertise a retired task.
            tasks: definition
                .tasks
                .iter()
                .filter_map(task_payload_key_and_instruction)
                .map(|(key, goal)| TrackTemplateTask { key, goal })
                .collect(),
        });
    }
    Ok(Json(templates))
}

/// A template's title and tasks: the roster entry, and nothing else.
struct Definition {
    title: String,
    /// Whole task-block payloads; the projection to `key` + `goal` happens in the handler.
    tasks: Vec<Value>,
}

/// Fallible, off the same `compile_template` call `POST /api/tracks` uses, so the picker
/// cannot advertise a template create would refuse. No automated test covers the error
/// arm: `TemplateRoster` has no public constructor and every `Template` field is private.
fn current_definition(template: &Template) -> Result<Definition> {
    let compiled = compile_template(template)?;
    Ok(Definition {
        title: template.title().to_string(),
        tasks: compiled
            .task_block_payloads()?
            .into_iter()
            .cloned()
            .collect(),
    })
}

/// Author-owned display metadata and the byte-exact report body. This read grants no authority.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrackTemplateDetail {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub body: String,
}

#[utoipa::path(
    get,
    path = "/api/track-templates/{id}",
    tag = "tracks",
    params(("id" = String, Path, description = "Template key; encode a site/ prefix as part of this segment")),
    responses(
        (status = 200, description = "Selected template content", body = TrackTemplateDetail),
        (status = 404, description = "Unknown template", body = ErrorBody),
    ),
)]
pub(crate) async fn get_track_template(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<TrackTemplateDetail>> {
    let template = s
        .templates
        .get(&id)
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("template {id}")))?;
    Ok(Json(TrackTemplateDetail {
        id: template.key().to_string(),
        title: template.title().to_string(),
        description: template.description().map(str::to_string),
        instructions: template.instructions().map(str::to_string),
        body: template.recipe().body,
    }))
}

/// Documentation included by a template, independently of plugin enablement.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplatePluginGuide {
    pub id: String,
    pub name: String,
}

#[utoipa::path(get, path="/api/track-templates/{id}/plugin-guides", tag="tracks",
    params(("id"=String, Path, description="Template id")),
    responses((status=200, description="Default plugin documentation references", body=Vec<TemplatePluginGuide>),
        (status=404, description="Template not found", body=ErrorBody)))]
pub(crate) async fn get_template_plugin_guides(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TemplatePluginGuide>>> {
    let template = s
        .templates
        .get(&id)
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("template {id}")))?;
    let guide = crate::builtin_plugins::required_owner(template.key())
        .and_then(crate::builtin_plugins::get)
        .map(|component| TemplatePluginGuide {
            id: component.manifest().id.clone(),
            name: component.manifest().display_name.clone(),
        });
    Ok(Json(guide.into_iter().collect()))
}
